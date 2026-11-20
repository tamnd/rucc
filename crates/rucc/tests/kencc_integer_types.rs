//! The type of an integer constant under `-fkencc`, end to end.
//!
//! Design: 06.1 of the rucc-plan9 plan. The values were measured with 7c on 9front arm64, by
//! the `plan9/const` cases of rucc-compat.
//!
//! `lex.c` of kencc gives a constant a type by its own rules and not by the list of C99. A
//! decimal constant above `vlong` is `uvlong`, and one `l` gives `long` whatever the value, as
//! kencc cuts the value to the width of `long`.

use std::process::Command;

/// Whether the compiler took `body` for the target, under `-fkencc`, and what it said.
fn check(what: &str, target: &str, body: &str) -> (bool, String) {
    let dir = std::env::temp_dir()
        .join(format!("rucc-kencc-integer-types-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, body).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-fsyntax-only", "-fkencc"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A declaration that is valid only when `cond` holds, as kencc has no `_Static_assert`.
fn holds(n: usize, cond: &str) -> String {
    format!("char check{n}[({cond}) ? 1 : -1];\n")
}

const KENCC: &[&str] = &[
    "sizeof(9223372036854775808) == 8 && -(9223372036854775808) > 0",
    "sizeof(2147483648L) == 4 && -(2147483648L) > 0",
    "sizeof(4294967296L) == 4 && 4294967296L == 0",
    "sizeof(2147483648) == 8 && -(2147483648) < 0",
    "sizeof(0x80000000) == 4 && -(0x80000000) > 0",
    "sizeof(4294967296u) == 8 && -(4294967296u) > 0",
    "sizeof(1L) == 4 && -(1L) < 0",
];

#[test]
fn a_constant_has_the_type_that_kencc_gives_it_on_each_row() {
    let body: String = KENCC.iter().enumerate().map(|(n, cond)| holds(n, cond)).collect();
    for target in ["aarch64-unknown-plan9", "x86_64-unknown-plan9"] {
        let (ok, err) = check(target, target, &body);
        assert!(ok, "{target} refused a constant:\n{err}");
    }
}

#[test]
fn without_kencc_the_constants_have_the_types_of_c() {
    let dir =
        std::env::temp_dir().join(format!("rucc-kencc-integer-types-{}-c", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, holds(0, "sizeof(2147483648L) == 8"))
        .expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-plan9", "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
