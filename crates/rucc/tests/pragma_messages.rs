//! `#pragma message`, `#pragma GCC warning` and `#pragma GCC error`, end to end, measured
//! against gcc 13: what each says, at which line, and what the warning flags do to it.
//!
//! Design: `message_pragma` in `crates/rucc-pp/src/directive.rs` for the two `GCC` ones, which
//! the preprocessor says, and `message_line` in `crates/rucc-parse/src/pack.rs` for `message`.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

fn run(flags: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=x86_64-unknown-linux-gnu", "-std=gnu17", "-S", "-o", "/dev/null"])
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

/// The line of each diagnostic of `severity` the compiler gave about the input, in order.
fn lines(said: &str, severity: &str) -> Vec<u32> {
    let marker = format!(": {severity}: ");
    said.lines()
        .filter(|line| line.starts_with("<stdin>:") && line.contains(&marker))
        .filter_map(|line| line.split(':').nth(1)?.parse().ok())
        .collect()
}

/// The messages of the diagnostics of `severity`, with the position taken off, in the order of
/// the lines they are about. The preprocessor's come out before the parser's, where gcc, which
/// does both in one pass, has them by line.
fn said(out: &str, severity: &str) -> Vec<String> {
    let marker = format!(": {severity}: ");
    let mut said: Vec<(u32, String)> = out
        .lines()
        .filter(|line| line.starts_with("<stdin>:"))
        .filter_map(|line| {
            let (at, rest) = line.split_once(&marker)?;
            Some((at.split(':').nth(1)?.parse().ok()?, rest.to_owned()))
        })
        .collect();
    said.sort_by_key(|&(line, _)| line);
    said.into_iter().map(|(_, message)| message).collect()
}

const LINES: &str = "\
#pragma message \"plain\"
#pragma message (\"paren\")
#pragma message (\"a\" \"b\")
#pragma GCC warning \"careful\"
#pragma GCC warning (\"paren warning\")
#pragma message
#pragma message (
#pragma message 42
#pragma GCC warning
#pragma GCC warning 42
#pragma message \"x\" junk
#pragma GCC warning \"y\" junk
#define W _Pragma(\"GCC warning \\\"from macro\\\"\")
W
#pragma GCC warning \"a\\\"b\\\\c\\x41\\101\"
int x;
";

/// Every line, said where gcc 13 says it and in its words.
#[test]
fn each_line_is_said_the_way_gcc_says_it() {
    let out = run(&[], LINES);
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{text}");
    assert_eq!(lines(&text, "note"), [1, 2, 3, 11], "{text}");
    assert_eq!(
        said(&text, "note"),
        [
            "`#pragma message: plain`",
            "`#pragma message: paren`",
            "`#pragma message: ab`",
            "`#pragma message: x`",
        ],
        "{text}"
    );
    let mut warned = lines(&text, "warning");
    warned.sort_unstable();
    assert_eq!(warned, [4, 6, 7, 8, 11, 12, 14, 15], "{text}");
    let expected = "expected a string after `#pragma message` [E0798]";
    assert_eq!(
        said(&text, "warning"),
        [
            "careful [W0335]",
            expected,
            expected,
            expected,
            "junk at end of `#pragma message` [E0798]",
            "y [W0335]",
            "from macro [W0335]",
            "a\"b\\cAA [W0335]",
        ],
        "{text}"
    );
    assert_eq!(lines(&text, "error"), [5, 9, 10], "{text}");
    assert!(
        said(&text, "error").iter().all(|m| m == "invalid `#pragma GCC warning` directive [E0672]")
    );
}

/// `#pragma GCC error` stops the build with its text.
#[test]
fn gcc_error_stops_the_build() {
    let out = run(&[], "#pragma GCC error \"stop\"\nint x;\n");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{text}");
    assert_eq!(said(&text, "error"), ["stop [E0810]"], "{text}");
}

/// The warning answers to no option: `-Wno-cpp` leaves it and `-pedantic-errors` leaves it a
/// warning, while `-w` drops it and `-Werror` makes it an error. The note is said whatever.
#[test]
fn the_warning_answers_to_the_flags_gcc_reads_for_it() {
    let input = "#pragma message \"note\"\n#pragma GCC warning \"careful\"\nint x;\n";
    for (flags, warned, failed) in [
        (&["-Wno-cpp"][..], 1, false),
        (&["-pedantic-errors"][..], 1, false),
        (&["-w"][..], 0, false),
        (&["-Werror"][..], 0, true),
    ] {
        let out = run(flags, input);
        let text = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.success(), !failed, "{flags:?}: {text}");
        assert_eq!(lines(&text, "warning").len(), warned, "{flags:?}: {text}");
        assert_eq!(lines(&text, "note"), [1], "{flags:?}: {text}");
    }
}

/// `#pragma once` in the main file is the other warning with no option, and `-pedantic-errors`
/// leaves it a warning too.
#[test]
fn pedantic_errors_leaves_a_pragma_once_in_the_main_file_a_warning() {
    let out = run(&["-pedantic-errors"], "#pragma once\nint x;\n");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert_eq!(lines(&text, "warning"), [1], "{text}");
}
