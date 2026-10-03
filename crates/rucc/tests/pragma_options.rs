//! `#pragma GCC target`, `optimize`, `push_options`, `pop_options` and `reset_options`, end to end,
//! measured against gcc 13: which functions they reach, what the stack of saved options puts
//! back, and what is said about a line that is not one.
//!
//! Design: `options_line` in `crates/rucc-parse/src/options.rs`, read by `targeted` and
//! `pragma_optimize` in `crates/rucc-sema/src/check/attr.rs`.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

fn run(flags: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=x86_64-unknown-linux-gnu", "-std=gnu17"])
        .args(flags)
        .args(["-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the input into");
    stdin.write_all(input.as_bytes()).expect("the input can be written");
    drop(stdin);
    child.wait_with_output().expect("the compiler finished")
}

/// Each diagnostic, as `line:column: severity: message`, in line order.
fn said(out: &Output) -> Vec<String> {
    let err = String::from_utf8_lossy(&out.stderr);
    let mut said: Vec<(u32, u32, String)> = err
        .lines()
        .filter_map(|line| line.strip_prefix("<stdin>:"))
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            let row = parts.next()?.parse().ok()?;
            let col = parts.next()?.parse().ok()?;
            let rest = parts.next()?.trim();
            // The code goes, and a message may have a bracket of its own.
            let rest = rest.rsplit_once(" [").map_or(rest, |(message, _)| message).to_owned();
            Some((row, col, rest))
        })
        .filter(|(_, _, rest)| !rest.starts_with("note:") && !rest.starts_with("help:"))
        .collect();
    said.sort_by_key(|&(row, col, _)| (row, col));
    said.into_iter().map(|(row, col, rest)| format!("{row}:{col}: {rest}")).collect()
}

/// The CRC32 intrinsic is built for `crc32`, which the unit is not, so a call to it is accepted
/// only from a function a `target` line reaches: one defined under it, one declared under it and
/// defined after the `pop_options`, and one under a second line that adds to the first. The
/// `pop_options` and the `reset_options` put back what the unit has.
#[test]
fn a_target_line_reaches_the_functions_after_it_until_it_is_popped() {
    let out = run(
        &["-fsyntax-only"],
        "\
#include <smmintrin.h>
#pragma GCC push_options
#pragma GCC target(\"crc32\")
unsigned under(unsigned c, unsigned v) { return _mm_crc32_u32(c, v); }
unsigned declared(unsigned c, unsigned v);
#pragma GCC pop_options
unsigned declared(unsigned c, unsigned v) { return _mm_crc32_u32(c, v); }
unsigned popped(unsigned c, unsigned v) { return _mm_crc32_u32(c, v); }
#pragma GCC target(\"popcnt\")
#pragma GCC target(\"crc32\")
unsigned added(unsigned c, unsigned v) { return _mm_crc32_u32(c, v) + _mm_popcnt_u32(v); }
#pragma GCC reset_options
unsigned reset(unsigned c, unsigned v) { return _mm_crc32_u32(c, v); }
__attribute__((target(\"crc32\"))) unsigned own(unsigned c, unsigned v) { return _mm_crc32_u32(c, v); }
",
    );
    let mismatch = "error: inlining failed in call to 'always_inline' '_mm_crc32_u32': target \
                    specific option mismatch";
    assert_eq!(said(&out), [format!("8:50: {mismatch}"), format!("13:49: {mismatch}")]);
}

/// A name gcc does not know is refused once, however many functions the line reaches, and the
/// functions are then built as if the line had not been written.
#[test]
fn a_target_gcc_does_not_know_is_refused_once() {
    let out = run(
        &["-fsyntax-only"],
        "\
#pragma GCC target(\"nosuch\")
int one(void) { return 1; }
int two(void) { return 2; }
",
    );
    assert!(!out.status.success());
    assert_eq!(said(&out), ["2:5: error: attribute 'target' argument 'nosuch' is unknown"]);
}

/// `optimize("no-stack-protector")` over a function is the attribute on it, in either spelling
/// and as an option among others, and `reset_options` ends it.
#[test]
fn an_optimize_line_reaches_the_functions_after_it() {
    let out = run(
        &["-S", "-o", "-", "-fstack-protector-all"],
        "\
void use(char *);
#pragma GCC optimize(\"no-stack-protector\")
int one(void) { char b[64]; use(b); return 0; }
#pragma GCC reset_options
#pragma GCC optimize 2, \"-fno-stack-protector\"
int two(void) { char b[64]; use(b); return 0; }
#pragma GCC reset_options
int three(void) { char b[64]; use(b); return 0; }
",
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let mut protected = Vec::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(name) = line.strip_suffix(':').filter(|name| !name.starts_with('.')) {
            current = Some(name);
        }
        if line.contains("%fs:40") {
            protected.extend(current.take());
        }
    }
    assert_eq!(protected, ["three"], "{text}");
}

/// A line that is not one is warned about, or refused when something follows the strings, in
/// gcc's words and at gcc's columns, and lines it reads cleanly are taken without a word.
#[test]
fn a_line_that_is_not_one_is_warned_about() {
    let out = run(
        &["-fsyntax-only"],
        "\
#pragma GCC pop_options
#pragma GCC optimize
#pragma GCC optimize(O2)
#pragma GCC target
#pragma GCC target(avx2)
#pragma GCC target(\"avx2\"
#pragma GCC optimize(\"O2\"
#pragma GCC push_options junk
#pragma GCC reset_options junk
#pragma GCC pop_options junk
#pragma GCC target(\"avx2\", \"bmi\")
#pragma GCC target(\"\")
#pragma GCC target \"sse4.2\"
#pragma GCC optimize 3
#pragma GCC optimize(2, \"O3\")
#pragma GCC optimize(\"-fno-strict-aliasing\",\"O1\")
#pragma GCC target(\"avx2\") junk
#pragma GCC optimize(\"O2\") junk
int x;
",
    );
    assert_eq!(
        said(&out),
        [
            "1:9: warning: `#pragma GCC pop_options` without a corresponding \
             `#pragma GCC push_options`",
            "2:9: warning: `#pragma GCC optimize` is not a string or number",
            "3:9: warning: `#pragma GCC optimize` is not a string or number",
            "4:19: warning: `#pragma GCC option` is not a string",
            "5:20: warning: `#pragma GCC option` is not a string",
            "6:9: warning: `#pragma GCC target (string [,string]...)` does not have a final `)`",
            "7:9: warning: `#pragma GCC optimize (string [,string]...)` does not have a final `)`",
            "8:9: warning: junk at end of `#pragma push_options`",
            "9:9: warning: junk at end of `#pragma reset_options`",
            "10:9: warning: junk at end of `#pragma pop_options`",
            "17:9: error: `#pragma GCC target` string is badly formed",
            "18:9: error: `#pragma GCC optimize` string is badly formed",
        ]
    );
}
