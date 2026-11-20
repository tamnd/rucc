//! `#pragma GCC diagnostic`, end to end, measured against gcc 13: which uses of a deprecated
//! function are warned about, which are errors and which say nothing, line by line.
//!
//! Design: `crates/rucc-parse/src/diagnostic.rs` reads the lines, `rucc_diag::Scoped` says what
//! holds where.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

fn run(flags: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=x86_64-unknown-linux-gnu", "-std=gnu17", "-S", "-o", "-"])
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

/// The messages of the warnings, with the position taken off.
fn warnings(said: &str) -> Vec<String> {
    said.lines()
        .filter_map(|line| line.split_once(": warning: ").map(|(_, rest)| rest.to_owned()))
        .collect()
}

const SCOPES: &str = "\
__attribute__((deprecated)) int f(void);
int a(void) { return f(); }
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored \"-Wdeprecated-declarations\"
int b(void) { return f(); }
#pragma GCC diagnostic pop
int c(void) { return f(); }
#pragma GCC diagnostic error \"-Wdeprecated-declarations\"
int d(void) { return f(); }
#pragma GCC diagnostic pop
int e(void) { return f(); }
#define QUIET _Pragma(\"GCC diagnostic ignored \\\"-Wdeprecated-declarations\\\"\")
QUIET
int g(void) { return f(); }
";

/// `ignored` holds until the `pop`, `error` makes an error of what follows it, a `pop` with
/// nothing pushed goes back to the command line, and `_Pragma` out of a macro is the same line.
#[test]
fn a_diagnostic_pragma_holds_from_its_line_to_the_pop() {
    let out = run(&[], SCOPES);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}");
    assert_eq!(lines(&said, "warning"), [2, 7, 11], "{said}");
    assert_eq!(lines(&said, "error"), [9], "{said}");
}

/// `warning` keeps a warning a warning under `-Werror`, and only from its line on.
#[test]
fn warning_takes_back_werror_for_what_follows() {
    let input = "\
__attribute__((deprecated)) int f(void);
int a(void) { return f(); }
#pragma GCC diagnostic warning \"-Wdeprecated-declarations\"
int b(void) { return f(); }
";
    let out = run(&["-Werror"], input);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}");
    assert_eq!(lines(&said, "error"), [2], "{said}");
    assert_eq!(lines(&said, "warning"), [4], "{said}");

    let out = run(&["-Werror"], &input.replace("int a(void) { return f(); }\n", ""));
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert_eq!(lines(&said, "warning"), [3], "{said}");
}

/// A line in a header holds for what comes after its `#include`, though the header's bytes are
/// placed after the whole of the file.
#[test]
fn a_line_in_a_header_holds_after_the_include() {
    let dir = std::env::temp_dir().join(format!("rucc-pragma-diagnostic-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("quiet.h"),
        "#pragma GCC diagnostic ignored \"-Wdeprecated-declarations\"\n",
    )
    .unwrap();
    let input = "\
__attribute__((deprecated)) int f(void);
int a(void) { return f(); }
#include \"quiet.h\"
int b(void) { return f(); }
";
    let include = format!("-I{}", dir.display());
    let out = run(&[&include], input);
    let said = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{said}");
    assert_eq!(lines(&said, "warning"), [2], "{said}");
}

/// What gcc says about a line it cannot read, under `-Wpragmas`, which `-Wno-pragmas` quiets.
#[test]
fn a_malformed_line_is_warned_about_under_wpragmas() {
    let input = "\
#pragma GCC diagnostic
#pragma GCC diagnostic bogus \"-Wall\"
#pragma GCC diagnostic ignored
#pragma GCC diagnostic ignored \"deprecated-declarations\"
#pragma GCC diagnostic ignored_attributes \"vendor::attr\"
#pragma GCC diagnostic ignored \"-Wdeprecated-declarations\" and more
__attribute__((deprecated)) int f(void);
int a(void) { return f(); }
";
    let out = run(&[], input);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    let kinds = "`error`, `warning`, `ignored`, `push`, `pop` or `ignored_attributes` after \
                 `#pragma GCC diagnostic` [E0798]";
    assert_eq!(
        warnings(&said),
        [
            format!("missing {kinds}"),
            format!("expected {kinds}"),
            "missing option after `#pragma GCC diagnostic` kind [E0798]".to_owned(),
            "`deprecated-declarations` is not an option that controls warnings [E0798]".to_owned(),
        ],
        "{said}"
    );
    assert_eq!(lines(&said, "warning"), [1, 2, 3, 4], "{said}");

    let out = run(&["-Wno-pragmas"], input);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert!(warnings(&said).is_empty(), "{said}");
}
