//! What `-fsemantic-interposition` and `-fno-semantic-interposition` change, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6 and `spec/11-asm-objects-debug.md` section 11.3.
//!
//! The flag is about one thing: whether the optimizer may believe a body it can see. Under `-fPIC`
//! a name the file exports is one the dynamic linker may find another definition of first, so the
//! body in front of the compiler is not necessarily the one that runs, and everything read off it is
//! read off the wrong function. Under `-fno-semantic-interposition` the build promises that the
//! definition here is the one that runs, which is a promise rather than a deduction and is the one
//! every distribution makes when it builds a shared library.
//!
//! The IR rather than the assembly, because what changes is a fact written onto a call site and the
//! assembly is where facts have already been spent. `call.nofree` is the one summary there is today,
//! from `spec/safe-memory/07-check-elimination.md` section 7.5, and it stands in for the rest: the
//! dereferenced ranges and the escaping parameters are the same question asked of the same body.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because interposition is a fact
/// about ELF and the other two formats answer it differently.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// One function calling another, both of them exported, which is the whole of the question.
///
/// `helper` frees nothing, so a compilation that may believe its body writes `nofree` onto the call
/// in `caller` and one that may not does not.
const PAIR: &str = "\
int helper(int x) { return x + 1; }
int caller(int x) { return helper(x) + 1; }
";

/// The IR the compiler produces for that source under those flags.
fn ir(what: &str, flags: &[&str], source: &str) -> String {
    let dir = fixture(what);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O1", "--emit=ir", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Where the file is going, for the tests that only need a path and not a compilation.
fn fixture(what: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rucc-interp-{}-{what}", std::process::id()))
}

/// An executable puts every name in one program, so the body in front of the compiler is the one
/// that will run and what it says about itself is worth writing down.
#[test]
fn an_executable_believes_the_body_it_can_see() {
    let text = ir("exe", &[], PAIR);
    assert!(text.contains("call.nofree @helper"), "{text}");
}

/// A library may not, and this is what `-fPIC` costs beyond the load it costs in the code. The
/// definition the process ends up using is whichever one the dynamic linker finds first, and a
/// replacement for `helper` is free to call `free`.
#[test]
fn a_library_cannot_believe_a_body_something_else_may_replace() {
    let text = ir("lib", &["-fPIC"], PAIR);
    assert!(text.contains("call @helper"), "{text}");
    assert!(!text.contains("nofree"), "{text}");
}

/// And the promise puts it back, which is the whole of what the flag is for.
#[test]
fn the_promise_puts_the_belief_back() {
    let text = ir("promised", &["-fPIC", "-fno-semantic-interposition"], PAIR);
    assert!(text.contains("call.nofree @helper"), "{text}");
}

/// The other direction is the default and says so, so a build that adds it to a line that already
/// had the negative one gets the honest answer back.
#[test]
fn the_last_one_on_the_line_is_the_one_that_counts() {
    let promised =
        ir("last-no", &["-fPIC", "-fsemantic-interposition", "-fno-semantic-interposition"], PAIR);
    assert!(promised.contains("call.nofree @helper"), "{promised}");

    let honest =
        ir("last-yes", &["-fPIC", "-fno-semantic-interposition", "-fsemantic-interposition"], PAIR);
    assert!(!honest.contains("nofree"), "{honest}");
}

/// A name nothing outside the library can reach is not one anything can replace, so the promise is
/// not needed for it. This is the same rule from both ends as the addresses in `pic.rs`, and it is
/// the reason `-fPIC -fvisibility=hidden` is what a library that cares is built with.
#[test]
fn a_library_believes_a_body_nothing_outside_it_can_name() {
    let hidden = ir("hidden", &["-fPIC", "-fvisibility=hidden"], PAIR);
    assert!(hidden.contains("call.nofree @helper"), "{hidden}");

    let quiet = ir(
        "quiet",
        &["-fPIC"],
        "\
static int helper(int x) { return x + 1; }
int caller(int x) { return helper(x) + 1; }
",
    );
    assert!(quiet.contains("call.nofree @helper"), "{quiet}");
}

/// The flag is about the link and not about the machine, and nothing outside ELF is given the ELF
/// answer. Mach-O has a two level namespace, so a name a library defines is bound to that library
/// rather than looked up in load order, and there is nothing for a promise to be about.
#[test]
fn a_format_that_cannot_interpose_needs_no_promise() {
    let dir = fixture("darwin");
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, PAIR).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-apple-darwin", "-O1", "--emit=ir", "-o", "-", "-fPIC"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("call.nofree @helper"), "{text}");
}
