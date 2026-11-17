//! The flags NetBSD builds its i386 boot code with, `-Os -fomit-frame-pointer` under the default
//! stack alignment of sixteen bytes. `bootxx_ffsv1` has to fit in 7680 bytes. gcc links it in 6628
//! and rucc linked it in 8312, and each test here is one of the causes.

use std::process::Command;

const BOOT: [&str; 6] = [
    "-m32",
    "-Os",
    "-ffreestanding",
    "-fomit-frame-pointer",
    "-fno-asynchronous-unwind-tables",
    "-fno-pie",
];

fn listing(name: &str, source: &str, extra: &[&str]) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-small-boot-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(BOOT)
        .args(["-S", "-o", "-"])
        .args(extra)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// The lines of one function, from its label to its `.size`.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let label = format!("{name}:");
    text.lines()
        .skip_while(|line| *line != label)
        .take_while(|line| !line.contains(".size"))
        .map(str::trim)
        .collect()
}

/// A local initialized with a UUID, as the GPT code of libsa does. gcc puts the sixteen bytes in
/// `.rodata` and copies them, which is shorter than one store for each field.
const UUID: &str = "\
struct uuid { unsigned int a; unsigned short b, c; unsigned char d[8]; };
int match(const struct uuid *);
int f(void) {
  struct uuid u = { 0x49f48d5a, 0xb10e, 0x11dc, { 0xb9, 0x9b, 0x00, 0x19, 0xd1, 0x87, 0x9d, 0x48 } };
  return match(&u);
}
";

#[test]
fn a_constant_local_is_copied_from_an_image_at_os() {
    let text = listing("uuid", UUID, &[]);
    let f = body(&text, "f");
    assert!(f.iter().any(|line| line.contains(".Linit")), "{text}");
    assert!(!f.iter().any(|line| line.contains("1240763738")), "{text}");
    let text = listing("uuid-o2", UUID, &["-O2"]);
    assert!(!text.contains(".Linit"), "{text}");
}

/// Twelve words of which one is not zero. gcc clears the object first when more entries are zero
/// than its `CLEAR_RATIO`, which is two at -Os, and fewer than a quarter are not.
const SPARSE: &str = "\
struct s { int a[12]; };
int g(struct s *);
int f(int x) {
  struct s v = { { 0, 0, 0, 0, 0, 0, 0, 0, x, 0, 0, 0 } };
  return g(&v);
}
";

#[test]
fn a_mostly_zero_local_is_cleared_first() {
    let text = listing("sparse", SPARSE, &[]);
    let f = body(&text, "f");
    let zeros = f.iter().filter(|line| line.starts_with("movl\t$0,")).count();
    assert!(zeros < 4, "{text}");
}

/// Three words of arguments under a sixteen byte alignment. gcc pushes them at -Os behind a
/// `subl $4, %esp` and takes off sixteen bytes after the call. rucc pushes the register of the top
/// argument one more time as the pad, because a `pushl` is one byte and the `subl` is three.
const PADDED: &str = "\
int h(int, int, int);
int f(int a) { return h(a, a + 1, a + 2) + 1; }
";

#[test]
fn arguments_are_pushed_behind_a_pad_of_one_push() {
    let text = listing("padded", PADDED, &[]);
    let f = body(&text, "f");
    assert!(!f.contains(&"subl\t$4, %esp"), "{text}");
    let pushes: Vec<_> = f.iter().filter(|line| line.starts_with("push")).collect();
    assert_eq!(pushes.len(), 4, "{text}");
    assert_eq!(pushes[0], pushes[1], "{text}");
    assert!(f.contains(&"addl\t$16, %esp"), "{text}");
}

/// A word next to a sector buffer. The word goes nearest the stack pointer, where its address is a
/// byte of displacement and not four.
const SECTOR: &str = "\
int readsect(char *, int *);
int f(void) {
  char buf[512];
  int n;
  if (readsect(buf, &n))
    return -1;
  return n + buf[0];
}
";

#[test]
fn a_small_slot_goes_below_a_large_one() {
    let text = listing("sector", SECTOR, &[]);
    let f = body(&text, "f");
    let far = f.iter().filter(|line| {
        line.split_once("(%esp)")
            .and_then(|(head, _)| head.rsplit([' ', '\t', ',']).next())
            .and_then(|disp| disp.parse::<i64>().ok())
            .is_some_and(|disp| disp > 127 && disp < 512)
    });
    assert_eq!(far.count(), 0, "{text}");
}

/// What the compiler says of the inlining of `source` with `-fopt-info-all`.
fn remarks(name: &str, source: &str, extra: &[&str]) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-small-boot-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(BOOT)
        .args(["-S", "-o", "-", "-fopt-info-all"])
        .args(extra)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|line| line.ends_with("[inline]"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `-finline-small-functions` is on at -Os, as in gcc. No call is hot at -Os, so a call to a
/// `static` function that grows the program is weighed and left alone, and says so.
const SMALL: &str = "\
extern int base[];
int put(int);
static int at(int i, int j) { if (base[i] > j) return put(base[i] - j); return base[j] + base[i + 1]; }
int f(int i) { return at(i, 3); }
int g(int i) { return at(i + 2, 5); }
";

#[test]
fn small_functions_are_weighed_at_os() {
    let said = remarks("small", SMALL, &[]);
    assert!(
        said.contains(
            "f: missed: inline call not inlined: call is unlikely and code size would grow"
        ),
        "{said}"
    );
    let said = remarks("small-off", SMALL, &["-fno-inline-small-functions"]);
    assert!(!said.contains("call is unlikely"), "{said}");
}
