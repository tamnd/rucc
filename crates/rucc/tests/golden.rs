//! The golden suite: a `.c` file, and what the compiler is expected to make of it.
//!
//! Design: `spec/15-testing.md` section 15.1, and the `M2` exit criterion in
//! `spec/17-milestones.md`.
//!
//! The cases are in `tests/golden` at the top of the repository rather than next to this file,
//! because one `.c` input has a `.tast` expectation and an `.ir` expectation beside it, and a
//! `.mir` one as that emitter arrives, and none of them belongs to one crate. Regenerate them
//! with `cargo xtask bless`, which runs exactly the commands below and writes what came out.
//!
//! What makes a golden file worth the maintenance is that its diff is readable. A conversion
//! that stops being inserted, a linkage that changes, a constant that stops being folded: each
//! of those is one line in a diff and none of them is a line anybody would write a unit test
//! for in advance. What makes it affordable is that blessing is one command, and what keeps
//! blessing honest is that the diff has to be read.
//!
//! The target is written on the command line rather than taken from the host, so that the
//! expectation is a fact about the compiler and not about the machine CI happened to run on.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The triple every case is compiled for, whatever the host is.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The dialect a case is compiled under when it does not name one, which is the default one.
const STD: &str = "gnu23";

/// The comment a case names its dialect with, on a line of its own and nothing else on it.
const STD_DIRECTIVE: &str = "// std: ";

/// The top of the repository, which is where the compiler is run from.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate is two levels under the repository root")
        .to_path_buf()
}

/// Where the cases live.
fn golden_dir() -> PathBuf {
    repo_root().join("tests").join("golden")
}

/// Every case in the directory, named by its file name, in an order every host agrees on so
/// that a failure names the same case everywhere and the output of two runs can be compared.
fn cases() -> Vec<String> {
    let dir = golden_dir();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .map(|path| path.file_name().expect("a file has a name").to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(!names.is_empty(), "{}: no cases", dir.display());
    names
}

/// Runs the compiler over one case, and gives back what it wrote to standard output or what it
/// said when it refused.
///
/// The case is named relative to the repository root and the compiler is run from there,
/// because the name of the input is printed in the IR module header, and an absolute path would
/// bless the layout of one person's disk into a file everybody else has to match. The name is
/// spelled with forward slashes rather than built with [`Path::join`] for the same reason:
/// Windows opens it either way, and only one of the two spellings goes into the expectation.
fn emit(case: &str, kind: &str) -> Result<String, String> {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(repo_root())
        .args([
            format!("--target={TARGET}"),
            format!("-std={}", dialect(case)),
            format!("--emit={kind}"),
        ])
        .arg(format!("tests/golden/{case}"))
        .args(["-o", "-"])
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(said);
    }
    assert!(
        said.is_empty(),
        "{case} compiled but said something, which a golden case must not:\n{said}"
    );
    Ok(String::from_utf8(out.stdout).expect("what the compiler writes is text"))
}

/// The dialect one case asks to be compiled under.
///
/// Almost every case wants the default, and the ones that do not are the ones about a rule that
/// changed: `int f();` means a function taking anything before C23 and a function taking nothing
/// from C23 on, so a case about the first of those cannot be written in the default dialect at
/// all.
fn dialect(case: &str) -> String {
    let text = std::fs::read_to_string(golden_dir().join(case)).unwrap_or_default();
    for line in text.lines() {
        if let Some(named) = line.strip_prefix(STD_DIRECTIVE) {
            return named.trim().to_owned();
        }
    }
    STD.to_owned()
}

/// What was blessed for one case, and [`None`] when there is no expectation of that kind.
fn blessed(case: &str, kind: &str) -> Option<String> {
    std::fs::read_to_string(golden_dir().join(case).with_extension(kind)).ok()
}

#[test]
fn every_case_produces_the_typed_tree_that_was_blessed() {
    let mut stale = Vec::new();
    for case in cases() {
        let expected = blessed(&case, "tast")
            .unwrap_or_else(|| panic!("{case}: no typed tree beside it; run `cargo xtask bless`"));
        let actual =
            emit(&case, "tast").unwrap_or_else(|said| panic!("{case} did not compile:\n{said}"));
        if actual != expected {
            stale.push(format!("{case}:\n--- blessed\n{expected}--- produced\n{actual}"));
        }
    }
    report(&stale);
}

#[test]
fn every_case_the_walk_can_lower_produces_the_ir_that_was_blessed() {
    let mut stale = Vec::new();
    for case in cases() {
        match (emit(&case, "ir"), blessed(&case, "ir")) {
            (Ok(actual), Some(expected)) if actual == expected => {}
            (Ok(actual), Some(expected)) => {
                stale.push(format!("{case}:\n--- blessed\n{expected}--- produced\n{actual}"));
            }
            // The walk is not finished, and a case it refuses has no `.ir` beside it rather
            // than an expectation nobody can produce. Which cases those are is itself checked,
            // so that the day one of them starts lowering is the day the suite asks for it.
            (Ok(_), None) => stale.push(format!("{case}: lowers now and has no `.ir` beside it")),
            (Err(said), Some(_)) => {
                stale.push(format!("{case}: has a blessed `.ir` and no longer lowers:\n{said}"));
            }
            (Err(said), None) => assert!(
                said.contains("[E0519]"),
                "{case} has no `.ir` beside it because the walk refuses it, but what it said is \
                 not that something is unsupported:\n{said}"
            ),
        }
    }
    report(&stale);
}

/// Fails the test when anything was found, with every case that was and the diff for each.
fn report(stale: &[String]) {
    assert!(
        stale.is_empty(),
        "{} case(s) no longer produce what was blessed. Read the diff, and if every change is \
         one you meant, run `cargo xtask bless`.\n\n{}",
        stale.len(),
        stale.join("\n")
    );
}

#[test]
fn no_expectation_is_left_behind_by_a_case_that_was_removed() {
    // An expectation with no `.c` next to it is a test that stopped running without anybody
    // deciding that it should, which `spec/15-testing.md` section 15.7 is explicit about.
    let dir = golden_dir();
    for entry in std::fs::read_dir(&dir).expect("a readable directory") {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().is_some_and(|ext| ext == "tast" || ext == "ir") {
            assert!(
                path.with_extension("c").exists(),
                "{}: an expectation with no case to produce it",
                path.display()
            );
        }
    }
}
