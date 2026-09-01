//! The golden suite: a `.c` file, and the typed tree it is expected to produce.
//!
//! Design: `spec/15-testing.md` section 15.1, and the `M2` exit criterion in
//! `spec/17-milestones.md`.
//!
//! The cases are in `tests/golden` at the top of the repository rather than next to this file,
//! because the same `.c` inputs get an `.ir` and a `.mir` expectation as those emitters arrive
//! and none of the three belongs to one crate. Regenerate them with `cargo xtask bless`, which
//! runs exactly the command below and writes what came out.
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

/// The dialect every case is compiled under, which is the default one.
const STD: &str = "gnu23";

/// Where the cases live.
fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate is two levels under the repository root")
        .join("tests")
        .join("golden")
}

/// The typed tree of one case, as the compiler writes it to standard output.
fn tast_of(source: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([format!("--target={TARGET}"), format!("-std={STD}"), "--emit=tast".to_owned()])
        .arg(source)
        .args(["-o", "-"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(
        out.status.success(),
        "{} did not compile:\n{}",
        source.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{} compiled but said something, which a golden case must not:\n{}",
        source.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("the typed tree is text")
}

#[test]
fn every_case_produces_the_typed_tree_that_was_blessed() {
    let dir = golden_dir();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    // Sorted, so that a failure names the same case on every host and the output of a run is
    // something two people can compare.
    entries.sort();
    assert!(!entries.is_empty(), "{}: no cases", dir.display());

    let mut stale = Vec::new();
    for source in &entries {
        let expected_path = source.with_extension("tast");
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|e| {
            panic!("{}: {e}; run `cargo xtask bless`", expected_path.display())
        });
        let actual = tast_of(source);
        if actual != expected {
            stale.push(format!(
                "{}:\n--- blessed\n{expected}--- produced\n{actual}",
                expected_path.display()
            ));
        }
    }
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
    // A `.tast` with no `.c` next to it is a test that stopped running without anybody deciding
    // that it should, which `spec/15-testing.md` section 15.7 is explicit about.
    let dir = golden_dir();
    for entry in std::fs::read_dir(&dir).expect("a readable directory") {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().is_some_and(|ext| ext == "tast") {
            assert!(
                path.with_extension("c").exists(),
                "{}: an expectation with no case to produce it",
                path.display()
            );
        }
    }
}
