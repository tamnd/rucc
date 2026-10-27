//! A word compared on RISC-V after it was cut out of a wider register.
//!
//! RV64 keeps a thirty two bit value sign extended in its register, and `slt` and `sltu` compare
//! the whole register, which gives the answer for the word only because of that. The `sext.w` a
//! narrowing writes is what makes the promise true for a word that came from the high half of a
//! `long` or from a small struct passed in one register, and the kernel's `printf_spec` is the
//! second of those: `field_width` is the top half of the register, and with the `sext.w` gone a
//! width of -1 read as 0xffffffff and `vsnprintf` padded its way off the end of the buffer.

use std::path::PathBuf;
use std::process::Command;

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("rucc-riscv64-word-compares-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the temporary directory takes a directory");
    let path = dir.join("t.c");
    std::fs::write(&path, source).expect("the temporary directory takes a file");
    path
}

/// The listing the compiler writes for that source at `-O2`.
fn asm(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=riscv64-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_high_half_of_a_long_is_sign_extended_before_it_is_compared() {
    let text = asm("high", "int f(int n, unsigned long x) { return n >= (int)(x >> 32); }\n");
    assert!(text.contains("sext.w"), "the word was compared without its sign:\n{text}");
}

#[test]
fn a_word_of_a_struct_in_a_register_is_sign_extended_before_it_is_compared() {
    let source = "struct spec { unsigned char flags; unsigned char base; short precision; \
                  int field_width; } __attribute__((packed));\n\
                  int f(int n, struct spec s) { return n >= s.field_width; }\n";
    let text = asm("spec", source);
    assert!(text.contains("sext.w"), "the field was compared without its sign:\n{text}");
}

#[test]
fn the_low_half_of_a_long_is_sign_extended_before_it_is_compared() {
    let text = asm("low", "int f(int n, unsigned long x) { return n < (int)x; }\n");
    assert!(text.contains("sext.w"), "the word was compared without its sign:\n{text}");
}
