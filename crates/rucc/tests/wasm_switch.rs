//! The shape of a `switch` on wasm32 with optimization. The driver splits each `switch` into
//! clusters with the rules of the native targets and the table bounds that were measured on
//! Wasmtime: a table at `-O2` from 24 cases, and a table at `-Os` from 4 cases. A table that is
//! the last test before the default has no range check, because `br_table` sends a value outside
//! the table to its default label.
//!
//! Design: #2866.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// A `switch` of `cases` dense cases, each of which calls a function.
fn switch(cases: u32) -> String {
    let mut text = String::from("void g(int);\nvoid f(int x) {\n  switch (x) {\n");
    for case in 0..cases {
        text.push_str(&format!("  case {case}: g({}); break;\n", case * 7 + 3));
    }
    text.push_str("  default: g(-1);\n  }\n}\n");
    text
}

/// The assembly of rucc for `source` on wasm32-wasip1 at the optimization level `level`.
fn assembly(source: &str, level: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=wasm32-wasip1", level, "-x", "c", "-", "-S", "-o", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(source.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("the compiler finished");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_dense_switch_of_30_cases_at_o2_is_one_table_with_no_range_check() {
    let text = assembly(&switch(30), "-O2");
    assert_eq!(text.matches("br_table").count(), 1, "{text}");
    assert!(!text.contains("i32.lt_u") && !text.contains("i32.gt_u"), "{text}");
}

#[test]
fn a_dense_switch_of_16_cases_at_o2_is_a_tree_of_tests() {
    let text = assembly(&switch(16), "-O2");
    assert!(!text.contains("br_table"), "{text}");
}

#[test]
fn a_dense_switch_of_4_cases_at_os_is_a_table() {
    let text = assembly(&switch(4), "-Os");
    assert_eq!(text.matches("br_table").count(), 1, "{text}");
}
