//! A `static` function copied into all its callers when that leaves the program no larger, which is
//! gcc's `want_inline_function_to_all_callers_p` (tamnd/rucc#3150).
//!
//! The shape is the one from the issue: `pick` has a cheap arm two of its three callers pick with
//! constants and forty lines of tail the third runs. gcc 16 at `-O1` copies all three calls and
//! drops `pick`, saying it inlined three calls and eliminated one function, and so does rucc. At
//! `-O2` the two cheap calls go in first, as small calls, and the third is then the only one.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// What the inliner says for each call it takes this way.
const ALL: &str = "call to a static function inlined into all its callers";

/// The shape from the issue.
const PICK: &str = r"
static int pick(int mode, int lim, int x)
{
  if (mode > 3) {
    if (lim < 10)
      return x;
    x = x * 3 + lim;
    x ^= x >> 3;
    x = x * 5 + mode;
    x ^= x << 7;
    x = x * 11 + lim;
    x ^= x >> 5;
    x = x * 13 + mode;
    x ^= x << 3;
    x = x * 17 + lim;
    x ^= x >> 11;
    x = x * 19 + mode;
    x ^= x << 5;
    x = x * 23 + lim;
    x ^= x >> 7;
    x = x * 29 + mode;
    x ^= x << 9;
    x = x * 31 + lim;
    x ^= x >> 13;
    x = x * 37 + mode;
    x ^= x << 2;
  }
  return x * 2;
}
int f(int x) { return pick(5, 4, x); }
int g(int x) { return pick(1, 40, x); }
int k(int x) { return pick(5, 40, x); }
";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-all-callers-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source at that level, and what it said.
fn compiled(what: &str, level: &str, source: &str) -> (String, String) {
    let path = fixture(&format!("{what}{level}"), source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args([level, "-fopt-info-all", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (String::from_utf8(out.stdout).expect("what the compiler writes is text"), said)
}

/// Whether anything in the listing calls or jumps to `pick`, or defines it.
fn mentions_pick(listing: &str) -> bool {
    listing.lines().any(|line| {
        let line = line.trim();
        line == "pick:" || matches!(line, "call\tpick" | "jmp\tpick")
    })
}

#[test]
fn every_call_goes_in_and_the_function_goes_at_o1() {
    let (listing, said) = compiled("o1", "-O1", PICK);
    assert!(!mentions_pick(&listing), "{listing}");
    assert_eq!(said.matches(ALL).count(), 3, "{said}");
}

#[test]
fn every_call_goes_in_and_the_function_goes_at_o2() {
    let (listing, _) = compiled("o2", "-O2", PICK);
    assert!(!mentions_pick(&listing), "{listing}");
}

/// With the third call passing what nobody knows, two copies of the tail are more than the body,
/// so all three stay calls at `-O1`, as they do with gcc 16.
#[test]
fn copies_that_grow_the_program_leave_every_call() {
    let source = PICK.replace(
        "int g(int x) { return pick(1, 40, x); }",
        "int g(int x, int m) { return pick(m, 40, x); }",
    );
    assert_ne!(source, PICK);
    let (listing, said) = compiled("grow", "-O1", &source);
    assert!(mentions_pick(&listing), "{listing}");
    assert!(!said.contains(ALL), "{said}");
}

/// A body that passes its one argument on with five more costs gcc more than a call of it, so the
/// three copies would grow the program and gcc 16 keeps the three calls at `-O1`.
#[test]
fn a_function_that_passes_more_than_it_is_given_stays_a_call_at_o1() {
    let src = "\
extern int sink(int, int, int, int, int, int);
static int wrap(int x) { return sink(x, 1, 2, 3, 4, 5); }
int f(int x) { return wrap(x); }
int g(int x) { return wrap(x + 1); }
int k(int x) { return wrap(x * 2); }
";
    let (asm, said) = compiled("wrap", "-O1", src);
    let calls = asm.lines().filter(|line| {
        let line = line.trim();
        line.starts_with("call") && line.ends_with("wrap")
    });
    assert_eq!(calls.count(), 3, "{asm}");
    assert!(!said.contains(ALL), "{said}");
}
