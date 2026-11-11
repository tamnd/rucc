//! The i386 flags of FreeBSD's `boot2`, which has to fit in 7680 bytes: `-Os -fomit-frame-pointer
//! -mrtd -mregparm=3 -mpreferred-stack-boundary=2`. gcc writes the client in 5216 bytes and rucc
//! wrote 6696.
//!
//! Two of the causes. `ebp` was never handed to the allocator on i386, so a function with more
//! live values than five registers spilled them, and gcc's `fsread` is 585 bytes shorter for that
//! more than for anything. And the address of a name was a `leal sym, %reg`, which is a byte
//! longer than the `movl $sym, %reg` gcc writes for the same number.

use std::process::Command;

const BOOT: [&str; 6] = [
    "-Os",
    "-fomit-frame-pointer",
    "-mrtd",
    "-mregparm=3",
    "-mpreferred-stack-boundary=2",
    "-march=i386",
];

/// Eight arguments and seven sums of them live across a call, which is more than the five
/// registers there were. `mark` puts a byte of each in a byte register, which `ebp` has none of.
const SOURCE: &str = "\
int g(int, int, int, int, int, int, int, int);
int f(int a, int b, int c, int d, int e, int h, int i, int j) {
  int x = a * b, y = c * d, z = e * h, w = i * j, v = a + j, u = b + i, s = c + h;
  g(x, y, z, w, v, u, s, a);
  return x + y + z + w + v + u + s + a + b + c + d + e + h + i + j;
}
void mark(char *p, int a, int b, int c, int d, int e, int h) {
  char x = a * b, y = c * d, z = e * h, w = a + h, v = b + e, u = c + d;
  p[0] = x; p[1] = y; p[2] = z; p[3] = w; p[4] = v; p[5] = u;
  p[6] = x ^ y; p[7] = z ^ w; p[8] = v ^ u; p[9] = a; p[10] = b;
}
static char buf[64];
extern int table[];
char *at(void) { return buf + 4; }
int put(int *, char *);
int both(void) { return put(table, buf); }
";

fn listing(name: &str, extra: &[&str]) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-boot-code-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i386-unknown-freebsd15.1", "-ffreestanding"])
        .args(BOOT)
        .args(extra)
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
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

#[test]
fn ebp_holds_a_value_in_a_function_that_keeps_no_frame_pointer() {
    let text = listing("spare", &[]);
    let f = body(&text, "f");
    assert!(f.contains(&"pushl\t%ebp"), "ebp is saved where it is used\n{text}");
    assert!(!f.contains(&"movl\t%esp, %ebp"), "no frame pointer is kept\n{text}");
    let used = f.iter().filter(|line| line.contains("%ebp") && !line.starts_with("p")).count();
    assert!(used > 0, "ebp holds a value\n{text}");
    // A byte goes in a register that has one. The listing would have been refused otherwise.
    assert!(!body(&text, "mark").is_empty(), "{text}");
}

#[test]
fn ebp_stays_the_frame_pointer_where_one_is_kept() {
    let text = listing("kept", &["-fno-omit-frame-pointer"]);
    let f = body(&text, "f");
    assert!(f.contains(&"movl\t%esp, %ebp"), "{text}");
    let written =
        f.iter().filter(|line| line.ends_with(", %ebp") && !line.starts_with("movl\t%esp")).count();
    assert_eq!(written, 0, "nothing else is written into ebp\n{text}");
}

#[test]
fn the_address_of_a_name_is_a_move_of_the_number() {
    let text = listing("named", &[]);
    assert!(body(&text, "at").contains(&"movl\t$buf+4, %eax"), "{text}");
    let both = body(&text, "both");
    assert!(both.contains(&"movl\t$table, %eax"), "{text}");
    assert!(both.contains(&"movl\t$buf, %edx"), "{text}");
    assert!(!text.contains("leal\tbuf") && !text.contains("leal\ttable"), "{text}");
}

#[test]
fn position_independent_code_keeps_the_address_from_the_table() {
    let text = listing("pic", &["-fPIC"]);
    assert!(text.contains("@GOTOFF"), "{text}");
    assert!(!text.contains("$buf") && !text.contains("$table"), "{text}");
}
