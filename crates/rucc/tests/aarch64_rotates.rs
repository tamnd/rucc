//! A rotate by a constant, and a shift whose count was masked or widened, on AArch64, as gcc writes
//! them.
//!
//! C has no rotate, so `(x << 7) | (x >> 25)` is two shifts and an or until something puts them
//! back together, and the machine has had `ror` all along. A shift count the program masked to the
//! width, as in `x << (n & 31)`, needs no `and`, because the machine reads the count modulo the
//! width anyway. A sixty-four bit shift by an `int` needs no widening for the same reason.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
unsigned rolk(unsigned w) { return (w << 7) | (w >> 25); }
unsigned rork(unsigned w) { return (w >> 9) | (w << 23); }
unsigned long rork64(unsigned long w) { return (w >> 13) | (w << 51); }
unsigned long rolk64(unsigned long w) { return (w << 1) | (w >> 63); }
unsigned vsl(unsigned a, unsigned b) { return a << (b & 31); }
int vsa(int a, int b) { return a >> (b & 31); }
unsigned long lsrm(unsigned long a, unsigned long b) { return a >> (b & 63); }
unsigned long shv(unsigned long a, int b) { return a << b; }
long sar(long a, unsigned b) { return a >> b; }
unsigned long msk(unsigned long a, unsigned long b) { return a << (b & 15); }
unsigned half(unsigned w) { return (w << 7) | (w >> 24); }
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-rotates-{}-{n}", std::process::id()));
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
fn a_rotate_by_a_constant_is_one_instruction() {
    let text = listing();
    for (name, line) in [
        ("rolk", "ror w0, w0, #25"),
        ("rork", "ror w0, w0, #9"),
        ("rork64", "ror x0, x0, #13"),
        ("rolk64", "ror x0, x0, #63"),
    ] {
        assert_eq!(body(&text, name), [line, "ret"], "{name}:\n{text}");
    }
}

#[test]
fn a_masked_or_widened_count_is_read_as_it_is() {
    let text = listing();
    for (name, line) in [
        ("vsl", "lslv w0, w0, w1"),
        ("vsa", "asrv w0, w0, w1"),
        ("lsrm", "lsrv x0, x0, x1"),
        ("shv", "lslv x0, x0, x1"),
        ("sar", "asrv x0, x0, x1"),
    ] {
        assert_eq!(body(&text, name), [line, "ret"], "{name}:\n{text}");
    }
}

/// A mask narrower than the width changes the count, and shifts whose counts do not add up to the
/// width are not a rotate, so both keep every instruction they had.
#[test]
fn anything_else_is_left_alone() {
    let text = listing();
    assert!(body(&text, "msk").iter().any(|line| line.starts_with("and ")), "{text}");
    assert!(!body(&text, "half").iter().any(|line| line.starts_with("ror ")), "{text}");
}

/// The integrated assembler takes the new instructions too.
#[test]
fn the_object_assembles() {
    let out = compile(&["-c"]);
    assert!(!out.stdout.is_empty());
}
