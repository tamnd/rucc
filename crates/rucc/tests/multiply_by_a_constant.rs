//! A multiply by a constant on x86-64 reads its source from where it is and writes a register of
//! its own, with no copy of the source in front of it.
//!
//! `imul $k, %src, %dst` names the two registers apart. When the description tied them, the
//! allocator copied the source into the destination first and the instruction then read the source
//! anyway, so every multiply by a constant whose source was still wanted after it had a `mov` in
//! front that nothing read. The checksum loop in Postgres is one of those per multiply.

use std::path::PathBuf;
use std::process::Command;

/// Two functions where the source of the multiply is read again after it.
const SOURCE: &str = "
unsigned f(unsigned x) { return (x * 16777619u) ^ x; }
long g(long x, long y) { return x * 1000 + y * 3000 + x; }
";

/// What the compiler writes for [`SOURCE`] at `-O2`.
fn assembly() -> String {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rucc-multiply-by-a-constant-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("a.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The instruction in front of each multiply, which is not a copy of a register.
#[test]
fn nothing_copies_the_source_of_a_multiply_by_a_constant() {
    let asm = assembly();
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let mut seen = 0;
    for (at, line) in lines.iter().enumerate() {
        if !line.starts_with("imul") || !line.contains('$') {
            continue;
        }
        seen += 1;
        let before = lines[at - 1];
        let copy = before.starts_with("mov")
            && before.split_once(char::is_whitespace).is_some_and(|(_, operands)| {
                operands.split(',').all(|operand| operand.trim().starts_with('%'))
            });
        assert!(!copy, "a copy in front of `{line}`:\n{asm}");
    }
    assert_eq!(seen, 3, "three multiplies by a constant in\n{asm}");
}
