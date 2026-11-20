//! An addition whose left operand is still wanted writes its answer over the right one.
//!
//! Issue 1895. A two address instruction on x86-64 writes its answer over its first source, so
//! when that source is read again later the allocator used to copy it into a register of its own
//! first. Addition, the bitwise operations and the multiply read their sources either way round,
//! so the answer can go over the second source instead when that one is finished with. The unit
//! tests in `rucc-regalloc` cover the decision. These run the compiler and read the loop it wrote.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the assertions are about the compiler and
/// not about the machine the suite ran on.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A loop adding a value it does not change to one it works out each turn. `k` is read again on
/// the next turn and `t` is finished with at the addition, so the addition has to go over `t`.
const SOURCE: &str = "\
long run(const long *v, int n, long k) {
    long total = 0;
    for (int i = 0; i < n; i++) {
        long t = v[i] * 7;
        total ^= k + t;
    }
    return total;
}
";

/// The assembly the compiler writes for the fixture at this level.
fn assembly(level: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rucc-commutes-{}-{}",
        std::process::id(),
        level.trim_start_matches('-')
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("commutes.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture at {level}:\n{said}");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The instructions of the block holding the multiply, from the label in front of it to the first
/// branch after it, which is the body of the loop.
fn body(asm: &str) -> Vec<String> {
    let lines: Vec<&str> = asm.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let at = lines.iter().position(|line| line.starts_with("imul")).expect("the loop multiplies");
    let from = lines[..at].iter().rposition(|line| line.ends_with(':')).map_or(0, |from| from + 1);
    let to =
        lines[at..].iter().position(|line| line.starts_with('j')).map_or(lines.len(), |to| at + to);
    lines[from..to].iter().map(|line| (*line).to_string()).collect()
}

/// Whether an instruction copies one register into another. A sign or zero extension such as
/// `movslq` is not a copy, since it changes the value, and the loop needs the one it has.
fn copy(inst: &str) -> bool {
    let Some((opcode, operands)) = inst.split_once(char::is_whitespace) else { return false };
    let plain = ["mov", "movq", "movl", "movw", "movb"].contains(&opcode);
    plain && operands.split(',').all(|operand| operand.trim().starts_with('%'))
}

#[test]
fn the_addition_goes_over_the_value_it_is_finished_with() {
    for level in ["-O2", "-Os"] {
        let asm = assembly(level);
        let body = body(&asm);
        assert!(
            body.iter().any(|line| line.starts_with("add")),
            "the loop at {level} has no addition in it:\n{}",
            body.join("\n")
        );
        let copies: Vec<&String> = body.iter().filter(|line| copy(line)).collect();
        assert!(copies.is_empty(), "the loop at {level} copies a register: {copies:?}\n{asm}");
    }
}
