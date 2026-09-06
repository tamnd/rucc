//! The bisection interface, end to end: `-fdisable-<pass>` and `-fenable-<pass>` over a list of
//! functions.
//!
//! Design: `spec/optimizer/41-correctness.md` section 41.6, and the M4.0 milestone in
//! `spec/17-milestones.md`.
//!
//! The unit tests in `rucc-opt` cover what the flags mean and the ones in `rucc-driver` cover how
//! they are spelled. What is left is the thing somebody debugging a miscompilation actually does,
//! which is run the compiler twice over one file and read the difference, so these run the
//! compiler.
//!
//! The two functions in the fixture are the same function twice on purpose. Anything the
//! optimizer does to one of them it would do to the other, so a difference between them in the
//! output is the gate and cannot be anything else.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the IR this compares is
/// the same IR on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Two identical functions, each with a multiplication of two constants in it for folding to
/// find, and each with the constants left over afterwards for dead code elimination to clear up.
const SOURCE: &str = "\
int f(int x) {
    int a = 3;
    int b = 4;
    return x + (a * b);
}

int g(int x) {
    int a = 3;
    int b = 4;
    return x + (a * b);
}
";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-gates-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("two.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The IR the compiler produces for the fixture under these flags.
fn ir(what: &str, flags: &[&str]) -> String {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["--emit=ir", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The body of one function of the module, which is what a gate makes different.
fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    let head = format!("func @{name}(");
    let start = ir.find(&head).unwrap_or_else(|| panic!("there is no function called {name}"));
    let rest = &ir[start..];
    let end = rest.find("\n}").expect("a function that starts is a function that ends");
    &rest[..end]
}

#[test]
fn without_a_gate_the_two_functions_come_out_the_same() {
    let ir = ir("plain", &["-O2"]);
    assert_eq!(body(&ir, "f").replacen("@f", "@g", 1), body(&ir, "g"));
    assert!(body(&ir, "f").contains("iconst.i32 12"), "{ir}");
    assert!(!body(&ir, "f").contains("mul"), "{ir}");
}

#[test]
fn disabling_a_pass_for_one_function_leaves_the_other_optimized() {
    let ir = ir("disable", &["-O2", "-fdisable-fold=g"]);
    assert!(body(&ir, "f").contains("iconst.i32 12"), "f was not gated:\n{ir}");
    assert!(body(&ir, "g").contains("mul"), "g kept the multiply it was told to keep:\n{ir}");
}

#[test]
fn a_function_is_named_by_its_number_as_well_as_by_its_name() {
    // Which is what a script bisecting a file it has never read gives, since it can count
    // functions without knowing what any of them are called.
    let by_number = ir("number", &["-O2", "-fdisable-fold=1"]);
    let by_name = ir("name", &["-O2", "-fdisable-fold=g"]);
    // The bodies rather than the whole module, because the header carries the path of the
    // fixture and each of these two wrote its own.
    for name in ["f", "g"] {
        assert_eq!(body(&by_number, name), body(&by_name, name));
    }
    assert!(body(&by_number, "g").contains("mul"), "{by_number}");
}

#[test]
fn enabling_a_pass_reaches_one_function_at_a_level_that_runs_nothing() {
    let ir = ir("enable", &["-O0", "-fenable-fold=1"]);
    assert!(body(&ir, "f").contains("mul"), "nothing asked for f:\n{ir}");
    assert!(body(&ir, "g").contains("iconst.i32 12"), "g was asked for:\n{ir}");
}

#[test]
fn a_pass_that_did_not_run_on_a_function_says_nothing_about_it() {
    let path = fixture("remarks");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["--emit=ir", "-o", "-", "-O2", "-fopt-info-all", "-fdisable-fold=g"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success());
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(said.contains(": f: optimized:"), "fold said nothing about f:\n{said}");
    assert!(
        !said.contains(": g: optimized: integer instruction folded"),
        "a pass that did not run reported that it found nothing:\n{said}"
    );
}

#[test]
fn a_gate_that_names_a_pass_this_compiler_does_not_have_is_refused() {
    let path = fixture("refused");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["--emit=ir", "-o", "-", "-O2", "-fdisable-nosuchpass"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(!out.status.success(), "a misspelled pass name was accepted");
    assert!(said.contains("not a pass this compiler has"), "{said}");
}
