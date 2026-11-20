//! A shifted register as the second operand of an add, subtract or bitwise instruction on AArch64,
//! as gcc writes it.
//!
//! The machine shifts the second register on the way in for nothing, so `a + (b << 3)` is one
//! `add` with `lsl #3` in it rather than a `lsl` and then an `add`. A multiply by three, five, nine
//! or seventeen is the value added to itself shifted, which is one instruction where the multiply
//! wants the constant in a register first. A sum that is only ever an address still goes into the
//! load as a scaled index, which is better again.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
long scale(long x) { return x * 3; }
int five(int x) { return x * 5; }
long nine(long x) { return x * 9; }
long sub(long a, long b) { return a - (b >> 4); }
unsigned long subu(unsigned long a, unsigned long b) { return a - (b >> 4); }
unsigned mix(unsigned a, unsigned b) { return (b >> 7) ^ a; }
int sar(int a, int b) { return a & (b >> 2); }
long orr(long a, long b) { return (b << 12) | a; }
long idx(long *a, long i, long j) { return a[i] + (j << 3); }
void put(long *a, long i, long v) { a[i] = v; }
long both(long *a, long i) { long *p = a + i; return *p + (long)p; }
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-shifted-{}-{n}", std::process::id()));
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
fn the_shift_is_part_of_the_instruction() {
    let text = listing();
    for (name, line) in [
        ("scale", "add x0, x0, x0, lsl #1"),
        ("five", "add w0, w0, w0, lsl #2"),
        ("nine", "add x0, x0, x0, lsl #3"),
        ("sub", "sub x0, x0, x1, asr #4"),
        ("subu", "sub x0, x0, x1, lsr #4"),
        ("mix", "eor w0, w0, w1, lsr #7"),
        ("sar", "and w0, w0, w1, asr #2"),
        ("orr", "orr x0, x0, x1, lsl #12"),
    ] {
        let body = body(&text, name);
        assert_eq!(body, [line, "ret"], "{name}:\n{text}");
    }
}

/// An address read by nothing but a load or a store of the size the shift scales by goes into the
/// access, and the shifted add is left for a sum that is a value too.
#[test]
fn an_address_keeps_its_scaled_index() {
    let text = listing();
    assert_eq!(
        body(&text, "idx"),
        ["ldr x0, [x0, x1, lsl #3]", "add x0, x0, x2, lsl #3", "ret"],
        "{text}"
    );
    assert_eq!(body(&text, "put"), ["str x2, [x0, x1, lsl #3]", "ret"], "{text}");
    let both = body(&text, "both");
    assert!(both.contains(&"add x0, x0, x1, lsl #3".to_string()), "{text}");
    assert!(both.contains(&"ldr x1, [x0]".to_string()), "{text}");
}

/// The integrated assembler takes the new instructions too.
#[test]
fn the_object_assembles() {
    let out = compile(&["-c"]);
    assert!(!out.stdout.is_empty());
}
