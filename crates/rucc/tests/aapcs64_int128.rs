//! Where an `__int128` argument goes on AArch64, checked in the assembly rucc writes.
//!
//! AAPCS64 rule C.9 starts a sixteen byte aligned integer at an even numbered x register, so one
//! after an `int` is in x2 and x3 and x1 is left empty. Rule C.11 puts one that does not fit in the
//! argument area and spends the x registers that are left, so an argument after it is in memory
//! too. Darwin keeps the second rule and not the first. Every expectation here is what clang 18
//! writes for aarch64-linux-gnu, aarch64-w64-mingw32 and arm64-apple-darwin from the same source
//! at -O2. tamnd/rucc#2222.

use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "\
long long low(int a, __int128 b) { return (long long)b; }
long long after(long long a, long long b, long long c, long long d, long long e, long long f,
                long long g, __int128 x, long long y) { return y; }
void take(int, __int128);
void call(void) { take(1, 5); }
";

/// The fixture, under a directory of its own so that two of these running at once do not write
/// the same file.
fn fixture(target: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-int128-{}-{target}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The instructions of each function in the fixture, by the function's name with Darwin's
/// underscore taken off, with every directive and local label left out.
fn functions(target: &str) -> Vec<(String, Vec<String>)> {
    let path = fixture(target);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused the fixture for {target}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('.') {
            continue;
        }
        if let Some(label) = line.strip_suffix(':') {
            if !label.starts_with('.') && !label.starts_with('L') {
                found.push((label.trim_start_matches('_').to_owned(), Vec::new()));
            }
            continue;
        }
        if let Some((_, insts)) = found.last_mut() {
            insts.push(line.to_owned());
        }
    }
    found
}

fn function<'a>(found: &'a [(String, Vec<String>)], name: &str) -> &'a [String] {
    found
        .iter()
        .find(|(label, _)| label == name)
        .map(|(_, insts)| insts.as_slice())
        .unwrap_or_else(|| panic!("no function {name} in {found:#?}"))
}

/// The checks for one target, with the register the low half arrives in after an `int`.
fn check(target: &str, low: &str) {
    let found = functions(target);
    let got = function(&found, "low");
    assert_eq!(got, [format!("mov x0, {low}"), "ret".to_owned()], "{target}");

    // The seventh `long long` is in x6, so the `__int128` does not fit in x7 alone, goes to the
    // bottom of the argument area and leaves x7 unused, and the last `long long` is above it.
    let got = function(&found, "after");
    assert!(!got.iter().any(|inst| inst.contains("x7")), "{target}: {got:#?}");
    assert_eq!(got[got.len() - 2..], ["ldr x0, [sp, #16]", "ret"], "{target}: {got:#?}");

    let high = format!("x{}", low[1..].parse::<u32>().expect("a register number") + 1);
    let got = function(&found, "call");
    assert!(got.contains(&format!("mov {low}, #5")), "{target}: {got:#?}");
    assert!(got.contains(&format!("mov {high}, #0")), "{target}: {got:#?}");
}

#[test]
fn an_int128_after_an_int_starts_at_an_even_register_on_linux() {
    check("aarch64-linux-gnu", "x2");
}

#[test]
fn an_int128_after_an_int_starts_at_an_even_register_on_windows() {
    check("aarch64-windows-gnu", "x2");
}

#[test]
fn an_int128_after_an_int_takes_the_next_two_registers_on_darwin() {
    check("aarch64-apple-darwin", "x1");
}
