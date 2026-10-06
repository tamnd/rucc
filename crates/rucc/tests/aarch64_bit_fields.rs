//! A field or a bit read out of a register on AArch64 written as `ubfx` or `tst`, as gcc writes it.
//!
//! Before this `(x >> 5) & 7` was a `lsr` and then an `and`, and `(f & 4) != 0` was a mask, a
//! compare against zero and a `cset`. The machine takes a field of any width at any place in one
//! `ubfx`, and one bit is a field one bit wide, which is already the zero or one C wants. A test of
//! several bits is `tst` and `cset`, or `tst` and a branch on the flags when an `if` reads it.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
extern void f(void);
int has(unsigned flags) { return (flags & 4) != 0; }
int hasl(unsigned long flags) { return (flags & 0x100) != 0; }
int field(unsigned x) { return (x >> 5) & 7; }
long fieldl(unsigned long x) { return (x >> 12) & 0xff; }
long top(unsigned long x) { return (x >> 60) & 0xf; }
int iseq(unsigned f) { return (f & 0x30) == 0; }
int isne(unsigned long f) { return (f & 0xff00) != 0; }
void actn(unsigned flags) { if (!(flags & 0x30)) f(); }
unsigned orr(unsigned x) { return (x >> 3) | 0xff; }
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-fields-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-fno-pic", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out
}

fn listing() -> String {
    String::from_utf8(compile(&["-S"]).stdout).expect("a listing is text")
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn a_field_is_one_instruction() {
    let text = listing();
    for (name, line) in [
        ("has", "ubfx w0, w0, #2, #1"),
        ("hasl", "ubfx x0, x0, #8, #1"),
        ("field", "ubfx w0, w0, #5, #3"),
        ("fieldl", "ubfx x0, x0, #12, #8"),
        ("top", "ubfx x0, x0, #60, #4"),
    ] {
        assert_eq!(body(&text, name), [line, "ret"], "{name}:\n{text}");
    }
}

#[test]
fn several_bits_are_a_test() {
    let text = listing();
    assert_eq!(body(&text, "iseq"), ["tst w0, #48", "cset w0, eq", "ret"], "{text}");
    assert_eq!(body(&text, "isne"), ["tst x0, #65280", "cset w0, ne", "ret"], "{text}");
    let actn = body(&text, "actn");
    assert!(actn.contains(&"tst w0, #48".to_string()), "{text}");
    assert!(actn.iter().any(|line| line.starts_with("b.eq ")), "{text}");
    assert!(!actn.iter().any(|line| line.starts_with("and ")), "{text}");
}

/// A constant beside a shifted register is the instruction's immediate rather than a register.
#[test]
fn the_constant_stays_an_immediate() {
    let text = listing();
    assert_eq!(body(&text, "orr"), ["lsr w0, w0, #3", "orr w0, w0, #255", "ret"], "{text}");
}

/// The integrated assembler takes the new instructions too.
#[test]
fn the_object_assembles() {
    let out = compile(&["-c"]);
    assert!(!out.stdout.is_empty());
}
