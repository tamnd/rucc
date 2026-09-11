//! What `-fsanitize=` does here, which is stop the compilation and say why.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.7.
//!
//! This is the one family in that section refused for a reason that is not about the bytes. A flag
//! is taken when ignoring it costs speed and refused when ignoring it changes what is produced, and
//! a sanitizer is neither of those. It is a promise that the program is watched while it runs, so a
//! build that asked for one and was quietly handed a program with no checks in it does not get a
//! slower program or a file of the wrong shape. It gets a test suite that passes for the wrong
//! reason, and there is no point afterwards where that announces itself.
//!
//! So the answer is a refusal that names the sanitizer and names `-fsafety=detect`, which is the
//! checking this compiler does have. What this file holds the compiler to is that the refusal is
//! precise: it fires on the command lines that are still asking for a check by the end, and not on
//! the ones that asked and took it back, and not on the flags that only describe what a check would
//! have done.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Something with a shift, a division and a dereference in it, so that every sanitizer named below
/// has something in the function it would have had to instrument.
const SOURCE: &str = "\
int scale(int *p, int n, int by) { return p[n] / by + (n << by); }
int main(void) { int a[4] = {1, 2, 3, 4}; return scale(a, 3, 1) == 6 ? 0 : 1; }
";

/// The target the object is built for, written down rather than taken from the host, because an
/// object is only produced for the one this compiler has a back end for.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// Spellings that are taken, because each of them describes what a check does when it fires and
/// every check is refused, so there is nothing left for them to be an answer about.
const TAKEN: [&str; 9] = [
    "-fno-sanitize=all",
    "-fno-sanitize=address",
    "-fno-sanitize=undefined,thread",
    "-fsanitize-recover=undefined",
    "-fno-sanitize-recover=all",
    "-fsanitize-trap=undefined",
    "-fsanitize-undefined-trap-on-error",
    "-fsanitize-address-use-after-scope",
    "-fsanitize-sections=.data",
];

/// A directory of this test's own, with the source already in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-san-{}-{what}", std::process::id()));
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
fn asking_for_a_sanitizer_is_refused_by_name_and_writes_no_object() {
    let dir = fixture("refused");
    for asked in [
        "address",
        "kernel-address",
        "hwaddress",
        "thread",
        "leak",
        "undefined",
        "signed-integer-overflow",
        "shift",
        "bounds",
        "null",
        "memory",
        "alias",
        "restrict",
    ] {
        let (ok, said) = run(&dir, &[&format!("-fsanitize={asked}")], "never.o");
        assert!(!ok, "-fsanitize={asked} is refused");
        assert!(said.contains(asked), "the refusal names it: {said}");
        assert!(said.contains("-fsafety=detect"), "and names the nearest thing: {said}");
        assert!(!dir.join("never.o").exists(), "-fsanitize={asked} wrote an object anyway");
    }

    // Coverage instrumentation, which is the same answer for the same reason: a fuzzer whose calls
    // into the coverage runtime were never generated runs blind and reports nothing.
    let (ok, said) = run(&dir, &["-fsanitize-coverage=trace-pc"], "never.o");
    assert!(!ok, "coverage instrumentation is refused");
    assert!(said.contains("feedback"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn taking_a_check_back_leaves_a_command_line_that_asked_for_nothing() {
    let dir = fixture("taken");
    let (ok, said) = run(&dir, &[], "plain.o");
    assert!(ok, "{said}");
    let plain = std::fs::read(dir.join("plain.o")).expect("the object was written");
    assert!(plain.len() > 200, "the object holds a compiled function: {} bytes", plain.len());

    // A build whose shared flags ask for a check and whose rule for one file takes it back is a
    // build that compiles that file here, which is why the answer waits for the end of the line.
    for pair in [
        ["-fsanitize=address", "-fno-sanitize=address"],
        ["-fsanitize=address,undefined", "-fno-sanitize=all"],
        ["-fsanitize=undefined", "-fno-sanitize=undefined"],
    ] {
        let (ok, said) = run(&dir, &pair, "asked.o");
        assert!(ok, "{pair:?}: {said}");
        let asked = std::fs::read(dir.join("asked.o")).expect("the object was written");
        assert_eq!(asked, plain, "{pair:?} changed the object");
    }

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
fn a_name_that_is_not_a_sanitizer_gets_a_different_message_from_one_that_is() {
    // Two different mistakes deserve two different answers. Somebody who wrote `-fsanitize=adress`
    // has a typo, and somebody who wrote `-fsanitize=address` has a compiler that cannot do it yet.
    let dir = fixture("names");
    for bad in ["-fsanitize=adress", "-fsanitize=undefined,bogus", "-fno-sanitize=bogus"] {
        let (ok, said) = run(&dir, &[bad], "never.o");
        assert!(!ok, "{bad} is refused");
        assert!(said.contains("is not a sanitizer"), "{bad}: {said}");
    }

    // gcc takes `all` only in the negative, because turning on every check at once includes checks
    // that contradict each other, and this says the same.
    let (ok, said) = run(&dir, &["-fsanitize=all"], "never.o");
    assert!(!ok, "-fsanitize=all is refused");
    assert!(said.contains("-fno-sanitize=all"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}
