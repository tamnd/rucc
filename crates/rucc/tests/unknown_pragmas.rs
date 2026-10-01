//! `-Wunknown-pragmas`, end to end, measured against gcc 13: which pragmas it calls unknown under
//! `-Wall`, by which words, and that it says nothing without being asked.
//!
//! Design: `pack_line` in `crates/rucc-parse/src/pack.rs`.

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

/// The messages of the warnings, with the position taken off.
fn warnings(said: &str) -> Vec<String> {
    said.lines()
        .filter_map(|line| line.split_once(": warning: ").map(|(_, rest)| rest.to_owned()))
        .collect()
}

const LINES: &str = "\
#pragma foo bar
#pragma GCC poison zzz
#pragma GCC push_options
#pragma GCC optimize (\"O2\")
#pragma GCC pop_options
#pragma GCC target (\"avx2\")
#pragma GCC novector
#pragma GCC bogus
#pragma pack(1)
#pragma pack()
#pragma weak w1
#pragma redefine_extname a1 b1
#pragma scalar_storage_order default
#pragma omp parallel
#pragma STDC FP_CONTRACT ON
#pragma STDC FLOAT_CONST_DECIMAL64 OFF
#pragma ms_struct on
#pragma region
#pragma endregion
#pragma GCC diagnostic push
#pragma
#pragma intrinsic(memset)
int w1;
";

/// The lines gcc 13 calls unknown under `-Wall`, named by the same words.
#[test]
fn an_unknown_pragma_is_named_under_wall() {
    let out = run(&["-Wall"], LINES);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert_eq!(lines(&said, "warning"), [1, 7, 8, 14, 15, 17, 21, 22], "{said}");
    assert_eq!(
        warnings(&said),
        [
            "ignoring `#pragma foo bar` [E0745]",
            "ignoring `#pragma GCC novector` [E0745]",
            "ignoring `#pragma GCC bogus` [E0745]",
            "ignoring `#pragma omp parallel` [E0745]",
            "ignoring `#pragma STDC FP_CONTRACT` [E0745]",
            "ignoring `#pragma ms_struct on` [E0745]",
            "ignoring `#pragma` [E0745]",
            "ignoring `#pragma intrinsic` [E0745]",
        ],
        "{said}"
    );
    let out = run(&["-Wunknown-pragmas"], LINES);
    assert_eq!(lines(&String::from_utf8_lossy(&out.stderr), "warning").len(), 8);
}

/// Without being asked it says nothing, and `-Wno-unknown-pragmas` after `-Wall` takes it back,
/// while a malformed line gcc knows is still `-Wpragmas`, which is on by default.
#[test]
fn an_unknown_pragma_is_quiet_unless_asked() {
    for flags in [&[][..], &["-Wall", "-Wno-unknown-pragmas"][..]] {
        let out = run(flags, LINES);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{said}");
        assert!(warnings(&said).is_empty(), "{flags:?}: {said}");
    }
    let out = run(&[], "#pragma GCC visibility\nint x;\n");
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(lines(&said, "warning"), [1], "{said}");
    assert!(warnings(&said)[0].ends_with("[E0798]"), "{said}");
}

/// `#pragma GCC diagnostic` can ask for it from a line on, the way the command line can.
#[test]
fn a_diagnostic_pragma_can_ask_for_it() {
    let input = "#pragma foo\n#pragma GCC diagnostic warning \"-Wunknown-pragmas\"\n#pragma bar\n";
    let out = run(&[], input);
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(warnings(&said), ["ignoring `#pragma bar` [E0745]"], "{said}");
}
