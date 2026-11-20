//! A load whose only reader widens it is one instruction that reads memory and widens it.
//!
//! Issue 1894. Reading a `char`, a `short` or an `int` into a wider type used to be a narrow load
//! and then a widening between registers, where the machine has one instruction for both. The unit
//! tests in `rucc-codegen` cover the fold. These run the compiler and read what it wrote.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the assertions are about the compiler and
/// not about the machine the suite ran on.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// One function per widening, each reading one element and giving it back wider, with the
/// instruction GCC 16 writes for it at `-O2`.
const CASES: &[(&str, &str, &str)] = &[
    ("s32", "long s32(const int *p, long i) { return p[i]; }", "movslq"),
    ("u32", "unsigned long u32(const unsigned *p, long i) { return p[i]; }", "movl"),
    ("s16", "long s16(const short *p, long i) { return p[i]; }", "movswq"),
    ("u16", "unsigned long u16(const unsigned short *p, long i) { return p[i]; }", "movzwl"),
    ("s8", "long s8(const signed char *p, long i) { return p[i]; }", "movsbq"),
    ("u8", "unsigned long u8(const unsigned char *p, long i) { return p[i]; }", "movzbl"),
    ("s16i", "int s16i(const short *p, long i) { return p[i]; }", "movswl"),
    ("u16i", "unsigned u16i(const unsigned short *p, long i) { return p[i]; }", "movzwl"),
    ("s8i", "int s8i(const signed char *p, long i) { return p[i]; }", "movsbl"),
    ("u8i", "unsigned u8i(const unsigned char *p, long i) { return p[i]; }", "movzbl"),
    ("s8w", "short s8w(const signed char *p, long i) { return p[i]; }", "movsbw"),
];

/// The assembly the compiler writes for every case at this level.
fn assembly(level: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rucc-widening-loads-{}-{}",
        std::process::id(),
        level.trim_start_matches('-')
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("widening.c");
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

/// Each case is the one instruction that reads memory and widens, and nothing else.
fn check(level: &str) {
    let asm = assembly(level);
    for (name, _, want) in CASES {
        let body = body(&asm, name);
        let mnemonic = |line: &String| line.split_whitespace().next().unwrap_or("").to_owned();
        assert_eq!(body.len(), 1, "{name} at {level} is more than one instruction: {body:?}");
        assert_eq!(mnemonic(&body[0]), *want, "{name} at {level}: {body:?}");
        assert!(body[0].contains('('), "{name} at {level} does not read memory: {body:?}");
    }
}

#[test]
fn a_widened_load_is_one_instruction_at_o2() {
    check("-O2");
}

#[test]
fn a_widened_load_is_one_instruction_at_os() {
    check("-Os");
}
