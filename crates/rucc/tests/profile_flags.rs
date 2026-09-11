//! What the `-fprofile` family does here, which is nothing on one half and a refusal on the other,
//! and why one family gets both answers.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.7.
//!
//! A profile is a count per edge, gathered by running a build of the program that was instrumented
//! to count and read back on a second compilation. There is none of it here, so the question for
//! every flag in the family is what ignoring it does, and the family is the only one in the
//! specification where the two halves get different answers.
//!
//! Ignoring a request to read the counts gives the program that would have been given anyway,
//! which is section 4.1's hint about speed, and gcc agrees on the strongest possible terms: its own
//! `-fprofile-use` object is byte for byte the no flag object when there are no counts to read.
//! Ignoring a request to write them means a file the build declared as an output never appears, the
//! second pass then optimizes against counts that were never gathered, and nothing anywhere says
//! so. That is the same reading `-gsplit-dwarf` gets, and this file holds the compiler to both
//! halves of it on the bytes rather than on the options.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A loop with a branch in it, because a count per edge is only worth gathering where there is an
/// edge whose frequency is not obvious from the source.
const SOURCE: &str = "\
int classify(int n) { return n % 7 == 0 ? n / 7 : n + 1; }
int main(void) {
    int total = 0;
    for (int i = 0; i < 100; i++) total += classify(i);
    return total == 0;
}
";

/// The target the object is built for, written down rather than taken from the host, because an
/// object is only produced for the one this compiler has a back end for.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// Every spelling that is taken. The first group asks to read the counts and is what the argument
/// above is about, and the rest describe instrumentation that is refused, so there is nothing left
/// for them to be an answer about and dropping them promises nothing.
const TAKEN: [&str; 20] = [
    "-fprofile-use",
    "-fprofile-use=counts",
    "-fno-profile-use",
    "-fprofile-dir=counts",
    "-fprofile-abs-path",
    "-fno-profile-abs-path",
    "-fprofile-correction",
    "-fno-profile-correction",
    "-fprofile-partial-training",
    "-fno-profile-partial-training",
    "-fprofile-update=single",
    "-fprofile-update=atomic",
    "-fprofile-update=prefer-atomic",
    "-fprofile-reproducible=serial",
    "-fprofile-reproducible=multithreaded",
    "-fprofile-values",
    "-fprofile-info-section",
    "-fprofile-filter-files=a.c",
    "-fprofile-exclude-files=b.c",
    "-fprofile-note=a.gcno",
];

/// Every spelling that is refused, with the word the refusal has to contain. The first five ask
/// for an instrumented program and the last writes a file beside the object.
const REFUSED: [(&str, &str); 7] = [
    ("-fprofile-generate", "instrument"),
    ("-fprofile-generate=counts", "instrument"),
    ("-fprofile-arcs", "instrument"),
    ("--coverage", "instrument"),
    ("-fcondition-coverage", "instrument"),
    ("-fpath-coverage", "instrument"),
    ("-ftest-coverage", ".gcno"),
];

/// A directory of this test's own, with the source already in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-prof-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("a.c"), SOURCE).expect("the fixture can be written");
    dir
}

/// What the compiler did with those flags: whether it succeeded, and what it said.
fn run(dir: &Path, flags: &[&str], object: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-O2", "-c"])
        .args(flags)
        .arg("-o")
        .arg(dir.join(object))
        .arg(dir.join("a.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn asking_to_read_a_profile_produces_the_object_asking_for_nothing_produces() {
    let dir = fixture("read");
    let (ok, said) = run(&dir, &[], "plain.o");
    assert!(ok, "{said}");
    let plain = std::fs::read(dir.join("plain.o")).expect("the object was written");

    // And it holds machine code, so the equality below is a statement about a real compilation
    // rather than about two empty files.
    assert!(plain.len() > 200, "the object holds a compiled function: {} bytes", plain.len());

    for spelling in TAKEN {
        let (ok, said) = run(&dir, &[spelling], "asked.o");
        assert!(ok, "{spelling}: {said}");
        assert!(said.is_empty(), "{spelling} was taken without comment: {said}");
        let asked = std::fs::read(dir.join("asked.o")).expect("the object was written");
        assert_eq!(asked, plain, "{spelling} changed the object");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn asking_to_write_one_is_refused_and_writes_no_object_and_no_file() {
    let dir = fixture("write");
    for (spelling, wanted) in REFUSED {
        let (ok, said) = run(&dir, &[spelling], "never.o");
        assert!(!ok, "{spelling} is refused");
        assert!(said.contains(wanted), "{spelling}: {said}");
        // The refusal is the whole point: nothing is written, so there is no half built object for
        // a make rule to find and call finished.
        assert!(!dir.join("never.o").exists(), "{spelling} wrote an object anyway");
    }

    // Nothing in the family writes the coverage note this compiler was asked for and refused,
    // which is the file the refusal is about.
    let left: Vec<_> = std::fs::read_dir(&dir)
        .expect("the fixture is there")
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .filter(|name| {
            let name = name.to_string_lossy();
            name.ends_with(".gcno") || name.ends_with(".gcda")
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(left.is_empty(), "a refused flag left a file behind: {left:?}");
}

#[test]
fn a_value_gcc_does_not_take_is_not_taken_here_either() {
    let dir = fixture("values");
    let refuse = |flag: &str, wanted: &str| {
        let (ok, said) = run(&dir, &[flag], "never.o");
        assert!(!ok, "{flag} is refused");
        assert!(said.contains(wanted), "{flag}: {said}");
    };

    refuse("-fprofile-update=none", "update method");
    refuse("-fprofile-update=", "update method");
    refuse("-fprofile-reproducible=any", "reproducibility method");
    let _ = std::fs::remove_dir_all(&dir);
}
