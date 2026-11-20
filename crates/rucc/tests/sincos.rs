//! A call to the library's `sin` and one to its `cos` of the same operand as one call to
//! `sincos`, as gcc writes them.
//!
//! gcc does this from `-O1` up where the C library has `sincos`, which is glibc, musl and the
//! FreeBSD libm. At `-O0` both calls stay, and so do two calls of different operands and the calls
//! to a `sin` that the program defines itself.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the listing is the same
/// wherever this runs.
const TARGET: &str = "x86_64-unknown-freebsd15.1";

const SOURCE: &str = "\
double sin(double); double cos(double);
float sinf(float); float cosf(float);
double both(double x) { return sin(x) + 2.0 * cos(x); }
float bothf(float x) { return sinf(x) - cosf(x); }
double apart(double x, double y) { return sin(x) + cos(y); }
";

const OWN: &str = "\
double sin(double x) { return x; }
double cos(double);
double both(double x) { return sin(x) + cos(x); }
";

/// The assembly for `source` under those flags, for `target`.
fn assembly(what: &str, source: &str, target: &str, flags: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-sincos-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("rucc runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("the assembly is text")
}

/// The lines of the function `name`, from its label to the next label of a function.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let label = format!("{name}:");
    text.lines()
        .skip_while(|line| line.trim() != label)
        .skip(1)
        .take_while(|line| !line.contains(".size"))
        .collect()
}

/// Whether the lines call `name`, as a call or as a tail call.
fn calls(lines: &[&str], name: &str) -> bool {
    lines.iter().any(|line| {
        let line = line.trim();
        (line.starts_with("call") || line.starts_with("jmp") || line.starts_with("bl"))
            && line.split([' ', '\t', '@']).any(|part| part == name)
    })
}

#[test]
fn a_sine_and_a_cosine_of_one_operand_are_one_call() {
    let text = assembly("o2", SOURCE, TARGET, &["-O2"]);
    for (name, pair, sin, cos) in
        [("both", "sincos", "sin", "cos"), ("bothf", "sincosf", "sinf", "cosf")]
    {
        let lines = body(&text, name);
        assert!(calls(&lines, pair), "{name}: {text}");
        assert!(!calls(&lines, sin) && !calls(&lines, cos), "{name}: {text}");
    }
    let lines = body(&text, "apart");
    assert!(!calls(&lines, "sincos"), "two operands are two calls: {text}");
    assert!(calls(&lines, "sin") && calls(&lines, "cos"), "{text}");
}

#[test]
fn the_pair_is_one_call_on_linux_and_on_aarch64() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let text = assembly(target, SOURCE, target, &["-O2"]);
        assert!(calls(&body(&text, "both"), "sincos"), "{target}: {text}");
    }
}

#[test]
fn both_calls_stay_at_o0_without_builtins_and_for_a_function_of_the_program() {
    let text = assembly("o0", SOURCE, TARGET, &["-O0"]);
    let lines = body(&text, "both");
    assert!(calls(&lines, "sin") && calls(&lines, "cos") && !calls(&lines, "sincos"), "{text}");
    let text = assembly("nobuiltin", SOURCE, TARGET, &["-O2", "-fno-builtin"]);
    assert!(!calls(&body(&text, "both"), "sincos"), "{text}");
    let text = assembly("own", OWN, TARGET, &["-O2", "-fno-inline"]);
    assert!(!calls(&body(&text, "both"), "sincos"), "{text}");
}

/// The answers of `sincos` are the answers of the two calls, over a spread of operands.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn the_pair_answers_what_the_two_calls_did() {
    // The reference answers are calls through pointers, which are not the library's calls and
    // stay as they are.
    const RUN: &str = "\
#include <math.h>
double both(double x, double *c) { *c = cos(x); return sin(x); }
float bothf(float x, float *c) { *c = cosf(x); return sinf(x); }
double (*volatile s)(double) = sin, (*volatile k)(double) = cos;
float (*volatile sf)(float) = sinf, (*volatile kf)(float) = cosf;
int main(void) {
  for (int i = -2000; i < 2000; i++) {
    double x = i * 0.37, c;
    float cf;
    if (both(x, &c) != s(x) || c != k(x)) return 1;
    if (bothf((float)x, &cf) != sf((float)x) || cf != kf((float)x)) return 2;
  }
  return 0;
}
";
    let dir = std::env::temp_dir().join(format!("rucc-sincos-run-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("run.c"), RUN).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .args(["-O2", "run.c", "-o", "run", "-lm"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ran = Command::new(dir.join("run")).status().expect("what was linked can be run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(ran.code(), Some(0), "sincos disagreed with sin and cos");
}
