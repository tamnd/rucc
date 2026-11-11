//! The negation of a floating value under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4148, and 06.8 of the rucc-plan9 plan.
//!
//! kencc has no instruction for the negation of a floating value. `tcom` in `cc/com.c` makes
//! `-x` into `0 - x` when `machcap` refuses the operator, and `6c` and `7c` both refuse it. So
//! the negation of a zero is a positive zero, and `hoc` and `pic` of 9front print `0` where a C
//! compiler gives `-0`.

use std::path::PathBuf;
use std::process::Command;

/// Each assertion holds in kencc. The sign of a zero shows in its reciprocal.
const SOURCE: &str = "_Static_assert(1 / -0.0 > 0, \"a negated constant zero is positive\");\n\
                      _Static_assert(1 / -0.0f > 0, \"the same for float\");\n\
                      _Static_assert(-1.5 == -1.5, \"a value that is not zero still changes sign\");\n\
                      _Static_assert(1 / -(0.0 * -1.0) > 0, \"an expression is negated as 0 - x\");\n\
                      #define IS(e, t) _Generic((e), t: 1, default: 0)\n\
                      float f;\n\
                      _Static_assert(IS(-f, float), \"the type stays float\");\n";

fn fixture(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-kencc-negation-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source and what it said.
fn run(what: &str, flags: &[&str]) -> (bool, String) {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fsyntax-only", "-w"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn kencc_negates_as_zero_minus_the_value() {
    let (ok, err) = run("kencc", &["-fkencc"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(err.is_empty(), "{err}");
}

#[test]
fn c_gives_a_negative_zero() {
    let (ok, err) = run("c", &[]);
    assert!(!ok, "C has a negative zero, so the first assertion fails");
    assert!(err.contains("a negated constant zero is positive"), "{err}");
}
