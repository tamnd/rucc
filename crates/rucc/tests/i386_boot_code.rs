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
    listing_of(SOURCE, name, extra)
}

fn listing_of(source: &str, name: &str, extra: &[&str]) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-boot-code-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
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

/// A count and an offset kept in names and changed in place, as `fsread` changes `fs_off`.
const COUNTED: &str = "\
static unsigned char meta;
unsigned off;
int read(int);
int step(int n) { if (read(n)) return -1; meta++; off += n; return 0; }
";

#[test]
fn a_name_changed_in_place_is_written_into_the_instruction_at_os() {
    let text = listing_of(COUNTED, "counted", &[]);
    let step = body(&text, "step");
    assert!(step.contains(&"addb\t$1, meta"), "{text}");
    assert!(
        step.iter().any(|line| line.starts_with("addl\t%") && line.ends_with(", off")),
        "{text}"
    );
    assert!(!step.iter().any(|line| line.contains("$meta") || line.contains("$off")), "{text}");
}

/// A `long long` shift by a register, as `fsread` has four of, and the test of a UFS1 or UFS2
/// magic number it makes with `||` and `&&`.
const BRANCHES: &str = "\
long long shift(long long x, int n) { return x << n; }
struct fs { int magic; long long loc; int bsize; } fs;
int try[4];
int find(int n) {
  if ((fs.magic == 0x11954 || (fs.magic == 0x19540119 && fs.loc == try[n]))
      && fs.bsize <= 65536 && fs.bsize >= 1376)
    return 1;
  return 0;
}
";

#[test]
fn a_shift_of_a_long_long_by_a_register_is_one_branch() {
    let text = listing_of(BRANCHES, "shift", &[]);
    let shift = body(&text, "shift");
    let branches =
        shift.iter().filter(|line| line.starts_with('j') && !line.starts_with("jmp")).count();
    assert_eq!(branches, 1, "one branch for both halves\n{text}");
    assert!(!shift.iter().any(|line| line.starts_with("set")), "no truth value is kept\n{text}");
    // The count is tested in its low byte, which is three bytes shorter.
    assert!(shift.iter().any(|line| line.starts_with("testb\t$32, %")), "{text}");
    assert!(!shift.iter().any(|line| line.starts_with("testl\t$32")), "{text}");
}

#[test]
fn a_condition_known_on_an_edge_is_not_kept_as_a_byte() {
    let text = listing_of(BRANCHES, "find", &[]);
    let find = body(&text, "find");
    assert!(!find.iter().any(|line| line.starts_with("movb\t$0")), "{text}");
    assert!(!find.iter().any(|line| line.starts_with("testb\t%")), "{text}");
}

/// A call that is not expected to happen, which the traces above `-O1` put after the return.
const UNLIKELY: &str = "\
void a(void);
void b(void);
void h(int x) {
  if (__builtin_expect(x, 0))
    a();
  b();
}
";

#[test]
fn a_block_not_expected_to_run_stays_in_place_at_os() {
    let text = listing_of(UNLIKELY, "unlikely", &[]);
    let h = body(&text, "h");
    let a = h.iter().position(|line| line.starts_with("call\ta")).expect("a is called");
    let b = h.iter().position(|line| line.ends_with("\tb")).expect("b is called");
    assert!(a < b, "the call of a is jumped over and not jumped to\n{text}");
    let jumps = h.iter().filter(|line| line.starts_with('j') && !line.ends_with("\tb")).count();
    assert_eq!(jumps, 1, "one branch over the call\n{text}");
}

#[test]
fn a_block_not_expected_to_run_goes_after_the_return_at_o2() {
    let text = listing_of(UNLIKELY, "unlikely-o2", &["-O2"]);
    let h = body(&text, "h");
    let a = h.iter().position(|line| line.starts_with("call\ta")).expect("a is called");
    let b = h.iter().position(|line| line.ends_with("\tb")).expect("b is called");
    assert!(b < a, "the traces put the call of a at the end\n{text}");
}

/// A `printf` of two numbers, and a call of a function that takes its own five arguments off the
/// stack under `-mrtd`, the last two of which go there under `-mregparm=3`.
const PASSED: &str = "\
int printf(const char *, ...);
void say(int a, int b) { printf(\"%d %d\\n\", a, b); }
int h(int, int, int, int, int);
int five(int a) { return h(a, 1, 2, 3, a) + 1; }
";

#[test]
fn the_arguments_a_call_passes_on_the_stack_are_pushed_at_os() {
    let text = listing_of(PASSED, "pushed", &[]);
    let say = body(&text, "say");
    let pushes = say.iter().filter(|line| line.starts_with("pushl")).count();
    assert_eq!(pushes, 3, "the format and both numbers are pushed\n{text}");
    assert!(say.contains(&"addl\t$12, %esp"), "the caller takes them off again\n{text}");
    assert!(!say.iter().any(|line| line.contains("(%esp)")), "nothing is stored\n{text}");
    let five = body(&text, "five");
    assert!(!five.iter().any(|line| line.contains("(%esp)")), "both words are pushed\n{text}");
    let call = five.iter().position(|line| line.ends_with("\th")).expect("h is called");
    assert!(
        !five[call..].iter().any(|line| line.starts_with("subl")),
        "a callee that takes its arguments off leaves the stack pointer where it was\n{text}"
    );
}

#[test]
fn a_frame_that_was_only_the_area_of_the_pushed_arguments_goes_at_os() {
    let text = listing_of(PASSED, "unframed", &[]);
    let say = body(&text, "say");
    assert!(!say.iter().any(|line| line.starts_with("subl")), "{text}");
    let adds = say.iter().filter(|line| line.starts_with("addl")).count();
    assert_eq!(adds, 1, "only the caller's add behind the call is left\n{text}");
    let five = body(&text, "five");
    assert!(
        !five.iter().any(|line| line.ends_with(", %esp")),
        "the callee takes the pushes off and there is no frame to give back\n{text}"
    );
}

#[test]
fn the_arguments_a_call_passes_on_the_stack_are_stored_at_o2() {
    let text = listing_of(PASSED, "stored", &["-O2"]);
    let say = body(&text, "say");
    assert!(!say.iter().any(|line| line.starts_with("pushl\t$")), "{text}");
    assert!(say.iter().any(|line| line.starts_with("subl")), "the frame holds the area\n{text}");
}

/// A function that answers -1 once a call is over, which `boot2` does on each of its errors.
const MINUS: &str = "\
int g(void);
int fail(void) { g(); return -1; }
";

#[test]
fn minus_one_is_an_or_into_the_register_at_os() {
    let text = listing_of(MINUS, "minus-os", &[]);
    let fail = body(&text, "fail");
    assert!(fail.contains(&"orl\t$-1, %eax"), "three bytes where the move is five\n{text}");
    let text = listing_of(MINUS, "minus-o2", &["-O2"]);
    let fail = body(&text, "fail");
    assert!(fail.contains(&"movl\t$-1, %eax"), "the move reads nothing at -O2\n{text}");
}
