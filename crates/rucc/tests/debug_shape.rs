//! What the two flags about the shape of the debug output do, which is nothing yet, and the
//! difference between nothing and silence.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.8.
//!
//! `-gz` says how the debug sections are compressed and `-gsplit-dwarf` says they go in a file of
//! their own. This compiler writes no debug sections at all, so the first of them has nothing to
//! act on and the second has nothing to put in the file it would write. Those two facts lead to
//! opposite answers, and the point of this file is that the difference is deliberate: a flag that
//! changes nothing about what is produced is taken, and a flag whose whole observable effect is
//! that a file appears is refused, because the file would not appear.
//!
//! The compression case is asserted on bytes rather than on options, because the claim being made
//! to a build that passes the flag is about the object, not about a field somewhere.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A program with enough in it that an object built from it is not empty.
const SOURCE: &str = "\
int twice(int n) { return n + n; }
int total(const int *of, int many) {
    int sum = 0;
    for (int i = 0; i < many; i++) sum += twice(of[i]);
    return sum;
}
";

/// The target the object is built for, written down rather than taken from the host, because an
/// object is only produced for the one this compiler has a back end for.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// A directory of this test's own, so that two of these running at once do not write the same
/// file, with the source already in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-gz-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    dir
}

/// What the compiler did with those flags: whether it succeeded, and what it said.
fn run(dir: &Path, flags: &[&str], object: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-c"])
        .args(flags)
        .arg("-o")
        .arg(dir.join(object))
        .arg(dir.join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn asking_for_compressed_debug_sections_produces_the_same_object_as_not_asking() {
    // The honest reading of taking a flag that has nothing to act on. Every value produces the
    // bytes no value produces, so a build that passes `-gz` gets what it would have got anyway
    // rather than a quietly different file, and the flag is a description rather than a promise.
    // The day there is a debug section to compress, this test is the one that has to change, and
    // it is written so that it fails rather than passes on that day.
    let dir = fixture("same");
    let (ok, said) = run(&dir, &[], "plain.o");
    assert!(ok, "{said}");
    let plain = std::fs::read(dir.join("plain.o")).expect("the object was written");

    for spelling in ["-gz", "-gz=none", "-gz=zlib", "-gz=zlib-gnu", "-gz=zstd"] {
        let (ok, said) = run(&dir, &[spelling], "asked.o");
        assert!(ok, "{spelling}: {said}");
        assert!(said.is_empty(), "{spelling} was taken without comment: {said}");
        let asked = std::fs::read(dir.join("asked.o")).expect("the object was written");
        assert_eq!(asked, plain, "{spelling} changed the object");
    }

    // And a value nothing has heard of stops the compilation, so that a typo in a distribution's
    // flags is found here rather than by whoever later wonders why nothing got smaller.
    let (ok, said) = run(&dir, &["-gz=gzip"], "never.o");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "a value outside the list is refused");
    assert!(said.contains("is not a way to compress"), "{said}");
    assert!(said.contains("zstd"), "the refusal lists what would have worked: {said}");
}

#[test]
fn splitting_the_debug_information_off_is_refused_and_not_splitting_it_is_not() {
    // gcc writes the `.dwo` whether or not it found anything to put in it, so a make rule that
    // depends on the file fires there and would not fire here. Refusing says so at the point the
    // flag is read, which is the only point where the answer is any use to the person reading it.
    let dir = fixture("split");
    let (ok, said) = run(&dir, &["-gsplit-dwarf", "-g"], "one.o");
    assert!(!ok, "the flag is refused: {said}");
    assert!(said.contains(".dwo"), "the refusal names the file it would have written: {said}");
    assert!(!dir.join("one.dwo").exists(), "and no such file was written");

    // The other direction describes what happens, so it is taken and says nothing, and it leaves
    // the question of how much debug information there is to the flag that asks that.
    let (ok, said) = run(&dir, &["-gno-split-dwarf", "-g"], "whole.o");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{said}");
    assert!(said.is_empty(), "{said}");
}
