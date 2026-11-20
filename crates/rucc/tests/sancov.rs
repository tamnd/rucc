//! `-fsanitize-coverage=trace-pc` and `-fsanitize-coverage=trace-cmp`, which the kernel's kcov
//! puts on every unit that does not opt out. The calls carry gcc's names and arguments, a function
//! marked `no_sanitize_coverage` makes none of them, and the program does what it did without them.

use std::path::Path;
use std::process::Command;

/// A comparison of each width the calls are for, a `switch`, and a function that asks for none.
const SOURCE: &str = r#"
int g(int);
__attribute__((noinline)) int f(int x, unsigned char c, long long y, double d) {
    int r = 0;
    if (x > 5) r += g(1);
    if (c == 3) r += g(2);
    if (y < x) r += g(3);
    if (d < 1.5) r += g(4);
    switch (x) {
    case 1: r += g(5); break;
    case 7: r += g(6); break;
    case 100: r += g(9); break;
    default: r += g(7);
    }
    return r;
}
__attribute__((no_sanitize_coverage)) int quiet(int x) { return x > 2 ? g(x) : 0; }
"#;

/// [`SOURCE`] with a runtime that counts the calls, itself marked so that it makes none, and a
/// `main` whose exit status says whether the counts and the result came out right.
#[cfg(target_arch = "aarch64")]
const RUNS: &str = r#"
typedef unsigned long long u64;
static long pcs, cmps, consts, switches;
static u64 table[5];
#define QUIET __attribute__((no_sanitize_coverage))
QUIET void __sanitizer_cov_trace_pc(void) { pcs++; }
QUIET void __sanitizer_cov_trace_cmp1(unsigned char a, unsigned char b) { cmps++; }
QUIET void __sanitizer_cov_trace_cmp2(unsigned short a, unsigned short b) { cmps++; }
QUIET void __sanitizer_cov_trace_cmp4(unsigned a, unsigned b) { cmps++; }
QUIET void __sanitizer_cov_trace_cmp8(u64 a, u64 b) { cmps++; }
QUIET void __sanitizer_cov_trace_const_cmp1(unsigned char a, unsigned char b) { consts++; }
QUIET void __sanitizer_cov_trace_const_cmp2(unsigned short a, unsigned short b) { consts++; }
QUIET void __sanitizer_cov_trace_const_cmp4(unsigned a, unsigned b) { consts++; }
QUIET void __sanitizer_cov_trace_const_cmp8(u64 a, u64 b) { consts++; }
QUIET void __sanitizer_cov_trace_cmpf(float a, float b) { cmps++; }
QUIET void __sanitizer_cov_trace_cmpd(double a, double b) { cmps += a + b == 2.5; }
QUIET void __sanitizer_cov_trace_switch(u64 value, u64 *cases) {
    switches += value == 7;
    for (int i = 0; i < 5; i++)
        table[i] = cases[i];
}
QUIET int g(int x) { return x; }
QUIET int main(void) {
    if (f(7, 3, -4, 1.0) != 16)
        return 1;
    if (pcs == 0 || cmps < 2 || consts < 1 || switches != 1)
        return 2;
    if (table[0] != 3 || table[1] != 32 || table[2] != 1 || table[3] != 7 || table[4] != 100)
        return 3;
    long before = pcs;
    if (quiet(5) != 5 || pcs != before)
        return 4;
    return 0;
}
"#;

fn compile(dir: &Path, source: &str, args: &[&str]) -> std::process::Output {
    std::fs::create_dir_all(dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-fsanitize-coverage=trace-pc", "-fsanitize-coverage=trace-cmp"])
        .args(args)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run")
}

/// The listing for that target, with the lines of `quiet` apart from the rest.
fn listing(target: &str, level: &str) -> (String, String) {
    let dir =
        std::env::temp_dir().join(format!("rucc-sancov-{}-{target}{level}", std::process::id()));
    let out = compile(&dir, SOURCE, &[&format!("--target={target}"), level, "-S", "-o", "-"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{target} {level}: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    let start = text.find("\nquiet:").expect("quiet is defined");
    let end = text[start..].find(".size\tquiet").map_or(text.len(), |at| start + at);
    let quiet = text[start..end].to_owned();
    (text, quiet)
}

#[test]
fn every_block_and_comparison_calls_the_runtime_gcc_names() {
    let targets =
        ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "i686-unknown-linux-gnu"];
    for target in targets {
        for level in ["-O0", "-O2"] {
            let (text, quiet) = listing(target, level);
            for call in [
                "__sanitizer_cov_trace_pc",
                "__sanitizer_cov_trace_cmp8",
                "__sanitizer_cov_trace_cmpd",
                "__sanitizer_cov_trace_const_cmp4",
                "__sanitizer_cov_trace_switch",
                "__sancov_gen_cov_switch_values.0",
            ] {
                assert!(text.contains(call), "{target} {level} has no {call}:\n{text}");
            }
            assert!(!quiet.contains("__sanitizer_cov"), "{target} {level}:\n{quiet}");
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[test]
fn the_calls_see_the_values_and_the_program_is_unchanged() {
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let dir =
            std::env::temp_dir().join(format!("rucc-sancov-run-{}{level}", std::process::id()));
        let program = dir.join("one");
        let program_text = program.to_str().expect("a temporary path is text");
        let source = format!("{SOURCE}{RUNS}");
        let out = compile(&dir, &source, &[level, "-o", program_text]);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let status = Command::new(&program).status().expect("the program runs");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(status.success(), "{level}: {status}");
    }
}

#[test]
fn the_kinds_gcc_does_not_have_are_refused() {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-fsanitize-coverage=trace-pc-guard", "-c", "-o", "-", "-x", "c", "/dev/null"])
        .current_dir(std::env::temp_dir())
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("trace-pc or trace-cmp"), "{said}");
}
