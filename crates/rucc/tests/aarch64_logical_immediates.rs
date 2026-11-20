//! An `and`, `or` or `xor` with a constant on AArch64 written with the constant in the
//! instruction, as gcc writes it.
//!
//! The logical instructions hold a pattern rather than a number: a run of ones, turned some way
//! round and repeated to fill the register. Masks, alignments and flag bits are nearly all of that
//! shape. Before this every one of them was built into a register with `mov` first, and one that
//! needed more than sixteen bits took a `movk` as well.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
unsigned mask(unsigned x) { return x & 0xff0; }
unsigned long lanes(unsigned long x) { return x & 0x00ff00ff00ff00ffUL; }
unsigned top(unsigned x) { return x | 0x80000000u; }
long flip(long x) { return x ^ 0x5555555555555555L; }
int align(int x) { return x & -16; }
long page(long x) { return x & ~4095L; }
unsigned odd(unsigned x) { return x & 0x12345; }
";

fn listing() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-logical-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
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
fn a_pattern_is_carried_by_the_instruction() {
    let text = listing();
    for (name, op) in [
        ("mask", "and w0, w0, #4080"),
        ("lanes", "and x0, x0, #71777214294589695"),
        ("top", "orr w0, w0, #-2147483648"),
        ("flip", "eor x0, x0, #6148914691236517205"),
        ("align", "and w0, w0, #-16"),
        ("page", "and x0, x0, #-4096"),
    ] {
        assert_eq!(body(&text, name), [op, "ret"], "{name}:\n{text}");
    }
}

/// A constant that is not one run of ones is still built in a register.
#[test]
fn anything_else_is_built_in_a_register() {
    let text = listing();
    let odd = body(&text, "odd");
    assert!(odd.iter().any(|line| line.starts_with("mov w1")), "{text}");
    assert!(odd.contains(&"and w0, w0, w1".to_string()), "{text}");
}

/// The listing assembles, so the constants are ones the encoder can carry.
#[test]
fn the_object_is_written() {
    let dir = std::env::temp_dir().join(format!("rucc-a64-logical-obj-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-c", "-o"])
        .arg(dir.join("one.o"))
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
