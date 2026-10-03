//! The extension macros under `#pragma GCC target`, end to end, measured against gcc 13: defined
//! from the line on, taken away by the `pop_options` or `reset_options` that ends its reach, and
//! left alone by a line gcc does not read.
//!
//! Design: `crates/rucc-pp/src/options.rs`.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The lines `-E` keeps of `input`, without the pragma lines it passes on and the blank ones.
fn kept(flags: &[&str], input: &str) -> Vec<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(flags)
        .args(["-E", "-P", "-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the input into");
    stdin.write_all(input.as_bytes()).expect("the input can be written");
    drop(stdin);
    let out = child.wait_with_output().expect("the compiler finished");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

const LINES: &str = "\
#ifdef __SSE4_2__
unit
#endif
#pragma GCC push_options
#pragma GCC target(\"sse4.2\")
#if defined __SSE4_2__ && defined __SSE4_1__ && defined __POPCNT__ && defined __CRC32__
pushed __SSE4_2__
#endif
#pragma GCC pop_options
#if defined __SSE4_2__ || defined __SSE4_1__ || defined __POPCNT__
popped
#endif
#pragma GCC target(\"sse4.2\") junk
#pragma GCC target(\"sse4.2\"
#pragma GCC target(\"nosuch\")
#pragma GCC target(\"sse4.2\", \"nosuch\")
#pragma GCC push_options junk
#ifdef __SSE4_2__
refused
#endif
_Pragma(\"GCC target(\\\"popcnt\\\")\")
#if defined __POPCNT__ && !defined __SSE4_2__
operator
#endif
#pragma GCC target(\"no-popcnt\")
#ifndef __POPCNT__
off
#endif
#pragma GCC target \"ssse3\"
#pragma GCC reset_options
#if !defined __SSSE3__ && defined __SSE2__
reset
#endif
";

/// What gcc 13 keeps of the lines on x86-64.
#[test]
fn the_extension_macros_follow_the_lines() {
    let kept = kept(&["--target=x86_64-unknown-linux-gnu"], LINES);
    assert_eq!(kept, ["pushed 1", "operator", "off", "reset"]);
}

/// A unit built for more keeps it under the lines, and `reset_options` goes back to it rather
/// than to the baseline.
#[test]
fn the_unit_is_what_the_lines_go_back_to() {
    let input = "\
#pragma GCC target(\"no-sse4.2\")
#ifndef __SSE4_2__
off
#endif
#pragma GCC reset_options
#ifdef __SSE4_2__
back
#endif
";
    assert_eq!(kept(&["--target=x86_64-unknown-linux-gnu", "-msse4.2"], input), ["off", "back"]);
}

/// AArch64's strings move no macro, as they change nothing in the function either.
#[test]
fn the_lines_move_nothing_on_aarch64() {
    let input = "\
#pragma GCC target(\"+crc\")
#ifdef __ARM_FEATURE_CRC32
crc
#endif
#ifdef __SSE4_2__
sse
#endif
";
    assert!(kept(&["--target=aarch64-unknown-linux-gnu"], input).is_empty());
}
