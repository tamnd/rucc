//! What the `-flto` family does here, which is nothing, and why that is a different answer from
//! the one `-gsplit-dwarf` gets two flags away in the same specification.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.7, and `spec/09-optimizer.md` for the work it is
//! waiting on.
//!
//! Link time optimization is the optimizer run once over the whole program rather than once per
//! file. There is none of it here yet, so the family is read, checked and recorded rather than
//! acted on, and the case for taking it rather than refusing it rests on two facts that this file
//! holds the compiler to.
//!
//! The first is that ignoring it costs speed and not correctness: a build that asks for it gets
//! the program it would have got anyway, which is what section 4.1 means by a hint about speed.
//! The second is about the object. gcc's `-flto` object holds the bytecode and no machine code at
//! all, which is why it is only useful to a link that knows about it, and every object here holds
//! the code, which is what `-ffat-lto-objects` asks gcc for. So a build that passes `-flto` to
//! this compiler gets an object that is more usable than the one it asked for, not a different
//! one, and the way to say that out loud is to assert it on the bytes.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Two files, so that the thing being asked for is an optimization across a boundary rather than
/// one inside a function. `helper` is exactly what an inliner would reach for across a file.
const CALLER: &str = "\
extern int helper(int n);
int twice_over(int n) { return helper(n) + helper(n); }
";

/// The other side of it.
const CALLEE: &str = "int helper(int n) { return n * 3; }\n";

/// The target the object is built for, written down rather than taken from the host, because an
/// object is only produced for the one this compiler has a back end for.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// Every spelling of the family that is taken, which is what the assertions below are quantified
/// over. `-flto=thin` is not here because it is clang's and is refused, which is its own test.
const TAKEN: [&str; 12] = [
    "-flto",
    "-flto=auto",
    "-flto=jobserver",
    "-flto=1",
    "-flto=8",
    "-fno-lto",
    "-flto-partition=balanced",
    "-flto-partition=one",
    "-flto-partition=none",
    "-flto-compression-level=9",
    "-ffat-lto-objects",
    "-fuse-linker-plugin",
];

/// A directory of this test's own, with both files already in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-lto-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("caller.c"), CALLER).expect("the fixture can be written");
    std::fs::write(dir.join("callee.c"), CALLEE).expect("the fixture can be written");
    dir
}

/// What the compiler did with those flags on one of the two files: whether it succeeded, and what
/// it said.
fn run(dir: &Path, flags: &[&str], source: &str, object: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-O2", "-c"])
        .args(flags)
        .arg("-o")
        .arg(dir.join(object))
        .arg(dir.join(source))
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn every_spelling_that_is_taken_produces_the_object_no_spelling_produces() {
    // The honest reading of taking a family that has nothing to act on, asserted on the bytes
    // rather than on the options. The day the optimizer grows a link time half, this is the test
    // that has to change, and it is written so that it fails rather than passes on that day.
    let dir = fixture("same");
    let (ok, said) = run(&dir, &[], "callee.c", "plain.o");
    assert!(ok, "{said}");
    let plain = std::fs::read(dir.join("plain.o")).expect("the object was written");

    // And it holds machine code, which is the whole difference from what gcc writes here. The
    // shortest honest check is that the file is not nearly empty, since a slim object is headers
    // and bytecode with an empty `.text`, and this one is a function with a multiply in it.
    assert!(plain.len() > 200, "the object holds a compiled function: {} bytes", plain.len());

    for spelling in TAKEN {
        let (ok, said) = run(&dir, &[spelling], "callee.c", "asked.o");
        assert!(ok, "{spelling}: {said}");
        assert!(said.is_empty(), "{spelling} was taken without comment: {said}");
        let asked = std::fs::read(dir.join("asked.o")).expect("the object was written");
        assert_eq!(asked, plain, "{spelling} changed the object");
    }

    // Both files, because the point of the flag is what happens between two of them and the
    // answer has to be the same for the one that calls as for the one that is called.
    let (ok, said) = run(&dir, &[], "caller.c", "callerplain.o");
    assert!(ok, "{said}");
    let plain = std::fs::read(dir.join("callerplain.o")).expect("the object was written");
    let (ok, said) = run(&dir, &["-flto=auto"], "caller.c", "callerlto.o");
    assert!(ok, "{said}");
    let asked = std::fs::read(dir.join("callerlto.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(asked, plain, "the calling side is the same too");
}

#[test]
fn a_value_gcc_does_not_take_is_not_taken_here_either() {
    // The family is taken, and that is not the same as the values being waved through. Somebody
    // who wrote `-flto=thin` meant clang, where it names a real and different arrangement, and
    // the useful thing to do with that command line is say so rather than compile it serially and
    // let them find out from a profile.
    let dir = fixture("refused");
    let refuse = |flag: &str, wanted: &str| {
        let (ok, said) = run(&dir, &[flag], "callee.c", "never.o");
        assert!(!ok, "{flag} is refused");
        assert!(said.contains(wanted), "{flag}: {said}");
    };

    refuse("-flto=thin", "link time jobs");
    refuse("-flto=full", "link time jobs");
    // gcc refuses a zero rather than reading it as a request for none, which is worth copying:
    // a build that computed the number from `nproc` and got zero has a bug either way.
    refuse("-flto=0", "link time jobs");
    refuse("-flto-partition=big", "partitioning model");
    // Nineteen is the top of zstd's range and the top of the one gcc checks against.
    refuse("-flto-compression-level=20", "compression level");
    let _ = std::fs::remove_dir_all(&dir);
}
