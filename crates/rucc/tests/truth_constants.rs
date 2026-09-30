//! A truth value combined with a constant takes the constant as an immediate.
//!
//! `!b` on a `_Bool` the front end kept as a value is `xor` with a one. The byte rules took an
//! immediate and the one bit rules did not, so the one went into a register of its own first:
//! `movb $1, %al; xorb %al, %dil`, where gcc writes a single `xorb $1`.

use std::process::Command;

/// The assembly the compiler writes for `source` on x86-64 at `-O2`.
fn assembly(what: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-truth-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_truth_value_flipped_by_a_constant_is_one_instruction() {
    let text = assembly("flip", "_Bool flip(_Bool a) { return a ^ 1; }\n");
    assert!(text.contains("xorb\t$1, %"), "{text}");
    assert!(!text.contains("movb\t$1"), "{text}");
}
