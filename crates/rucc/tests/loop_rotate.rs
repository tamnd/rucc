//! A loop whose test is at the top and whose step is a block of its own is laid out with the test
//! last, so that each time round is one jump rather than two.
//!
//! Issue 1994. The inner loops of `mul_var` and `accum_sum_add` in Postgres' `numeric.c` came out
//! as the body, a `cmpl` and a `jge` out of the loop, then the step of the walked pointer and a
//! `jmp` back to the top, so every turn ran a branch that was not taken and a jump that was.
//! `loop_rotate.c` is those two loops. The unit tests in `rucc-codegen` cover the layout. This runs
//! the compiler and reads the jumps it wrote.

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

/// Every `jmp` in the lines that goes to a label above it.
fn jumps_back<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut found = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let Some(to) = line.strip_prefix("jmp\t") else { continue };
        let label = format!("{to}:");
        if lines[..at].contains(&label.as_str()) {
            found.push(*line);
        }
    }
    found
}

#[test]
fn the_numeric_loops_jump_back_only_on_their_test() {
    let asm = assembly();
    for name in ["mul_inner", "accum_inner"] {
        let lines = body(&asm, name);
        assert!(lines.iter().any(|line| line.starts_with('j')), "{name} has no loop:\n{asm}");
        let back = jumps_back(&lines);
        assert!(back.is_empty(), "{name} jumps back with {back:?}:\n{asm}");
    }
}
