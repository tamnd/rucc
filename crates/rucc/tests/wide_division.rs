//! A sixty four bit division by a constant is a high multiply rather than a `div`.
//!
//! Issue 1938. The unit tests in `rucc-codegen` run the rewrite on numbers, and
//! `cargo xtask divide` runs what comes out against gcc. These read what the compiler writes for
//! the four divisions the issue names, at `-O2`, where the `div` goes, and at `-Os`, where it
//! stays because it is shorter.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the assertions are about the compiler and
/// not about the machine the suite ran on.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// One function per division, with the one operand multiply gcc 16 writes for it at `-O2`.
const CASES: &[(&str, &str, &str)] = &[
    ("s10", "long long s10(long long x) { return x / 10; }", "imulq"),
    ("s7", "long long s7(long long x) { return x / 7; }", "imulq"),
    ("u7", "unsigned long long u7(unsigned long long x) { return x / 7; }", "mulq"),
    ("u10", "unsigned long long u10(unsigned long long x) { return x % 10; }", "mulq"),
];

/// The assembly the compiler writes for every case at this level.
fn assembly(level: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rucc-wide-division-{}-{}",
        std::process::id(),
        level.trim_start_matches('-')
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("division.c");
    let source: String = CASES.iter().map(|(_, code, _)| format!("{code}\n")).collect();
    std::fs::write(&path, source).expect("the fixture can be written");
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

/// The instructions of one function, from its label to its return.
fn body(asm: &str, name: &str) -> Vec<String> {
    let lines: Vec<&str> = asm.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let label = format!("{name}:");
    let from = lines.iter().position(|line| *line == label).expect("every case is emitted") + 1;
    let to = lines[from..].iter().position(|line| line.starts_with("ret")).expect("it returns");
    lines[from..from + to]
        .iter()
        .filter(|line| !line.starts_with('.'))
        .map(|line| (*line).to_string())
        .collect()
}

fn mnemonic(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

fn divides(body: &[String]) -> bool {
    body.iter().any(|line| matches!(mnemonic(line), "divq" | "idivq"))
}

#[test]
fn a_sixty_four_bit_division_by_a_constant_is_a_high_multiply_at_o2() {
    let asm = assembly("-O2");
    for (name, _, want) in CASES {
        let body = body(&asm, name);
        assert!(!divides(&body), "{name} still divides: {body:?}");
        let high = |line: &String| mnemonic(line) == *want && !line.contains(',');
        assert!(body.iter().any(high), "{name} has no one operand {want}: {body:?}");
    }
}

#[test]
fn a_sixty_four_bit_division_by_a_constant_keeps_the_div_at_os() {
    let asm = assembly("-Os");
    for (name, _, _) in CASES {
        let body = body(&asm, name);
        assert!(divides(&body), "{name} at -Os does not divide: {body:?}");
    }
}
