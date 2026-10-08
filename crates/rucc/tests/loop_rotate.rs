//! A loop whose test is at the top and whose last block only steps it on is laid out with the test
//! last, so that each time round is one jump rather than two.
//!
//! Issue 1994. The carry loop of Postgres' `accum_sum_carry` came out as the test, the body, a
//! `cmpq` and a `je` out of the loop, then a block that copies the carry for the next turn and a
//! `jmp` back to the top, so every turn ran a branch that was not taken and a jump that was.
//! `loop_rotate.c` is that loop and the inner loops of `mul_var` and `accum_sum_add`, which are
//! one block each and are here so that they stay closed by their test. The unit tests in
//! `rucc-codegen` cover the layout. This runs the compiler and reads the jumps it wrote.

use std::process::Command;

/// The assembly the compiler writes for the fixture for x86-64 at `-O2`.
fn assembly() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/loop_rotate.c");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(["-S", "-o", "-", "-O2"])
        .arg(path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The lines of one function, from its label to the next function's.
fn body<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    let start = format!("{name}:");
    asm.lines()
        .map(str::trim)
        .skip_while(|line| *line != start)
        .skip(1)
        .take_while(|line| !line.ends_with(':') || line.starts_with(".L"))
        .collect()
}

/// Every conditional jump in the lines that goes to a label above it, which is a loop closed by
/// its own test.
fn tested_back<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut found = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let Some((op, to)) = line.split_once('\t') else { continue };
        if !op.starts_with('j') || op == "jmp" {
            continue;
        }
        let label = format!("{to}:");
        if lines[..at].contains(&label.as_str()) {
            found.push(*line);
        }
    }
    found
}

#[test]
fn the_numeric_loops_are_closed_by_their_test() {
    let asm = assembly();
    for name in ["carry_inner", "mul_inner", "accum_inner"] {
        let lines = body(&asm, name);
        assert!(lines.iter().any(|line| line.starts_with('j')), "{name} has no loop:\n{asm}");
        assert!(!tested_back(&lines).is_empty(), "{name} jumps back without a test:\n{asm}");
    }
}
