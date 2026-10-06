//! Which calls the inliner weighs by what is left of the callee once the constants they pass have
//! folded it, which is the summary of section 33.3 of `spec/optimizer/33-inlining.md`
//! (tamnd/rucc#3058).
//!
//! Each shape is a `static` function with cheap arms a mode picks and a long tail, called with a
//! mode for a cheap arm, with one for the tail and with one that is not known. gcc 16 at `-O2`
//! copies the cheap arm into each caller that passes the mode for it, and so does rucc. Where the
//! tail runs, gcc 16 calls a copy of the tail it split off, `pick.part.0`, and rucc calls the whole
//! function, since it does not split functions yet.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// What the inliner says when the constants a call passes made the callee smaller.
const CUT: &str = "note: call weighed without the code its constant arguments remove (1) [inline]";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-summary-{}-{what}", std::process::id()));
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

/// How many times that function calls or jumps to `pick`, a call in tail position being a jump.
fn picks(listing: &str, function: &str) -> usize {
    let label = format!("{function}:");
    listing
        .lines()
        .skip_while(|line| *line != label)
        .skip(1)
        .take_while(|line| {
            !(line.ends_with(':') && !line.starts_with(['.', '\t', ' ']) && !line.is_empty())
        })
        .filter(|line| matches!(line.trim(), "call\tpick" | "jmp\tpick"))
        .count()
}

/// Forty lines of arithmetic on `x`, which is well over what a call to a function nobody declared
/// `inline` may grow its caller by.
fn tail() -> String {
    let mut tail = String::new();
    for times in 3..=42 {
        let shift = (times - 3) % 7 + 1;
        let mix = 5 + 17 * (times - 3);
        let _ = writeln!(tail, "  x = x * {times} + (x >> {shift}) ^ {mix};");
    }
    tail
}

/// Checks which callers keep a call to `pick` and which say the constants they pass cut it down,
/// at `-O2` and at `-O3`.
fn check(what: &str, source: &str, copied: &[&str], called: &[&str]) {
    for level in ["-O2", "-O3"] {
        let (listing, said) = compiled(what, level, source);
        for function in copied {
            assert_eq!(picks(&listing, function), 0, "{function} at {level}:\n{listing}\n{said}");
            assert!(said.contains(&format!("one.c: {function}: {CUT}")), "{function}:\n{said}");
        }
        for function in called {
            assert_eq!(picks(&listing, function), 1, "{function} at {level}:\n{listing}\n{said}");
        }
    }
}

/// An `if` on the mode. `h` takes the cheap arm for its first call and the tail for its second.
#[test]
fn a_call_passing_the_mode_of_the_cheap_arm_is_copied() {
    let source = format!(
        "static int pick(int mode, int x)\n{{\n  if (mode == 0)\n    return x + 1;\n{}  return x;\n}}\n\
         int f(int x) {{ return pick(0, x); }}\n\
         int g(int x) {{ return pick(1, x); }}\n\
         int h(int x) {{ return pick(0, x) + pick(1, x + 1); }}\n",
        tail()
    );
    check("if", &source, &["f"], &["g", "h"]);
}

/// A `switch` on the mode with two cheap arms. `k` passes a mode for the tail and `h` passes one
/// that is not known, so neither is cut down and neither says so.
#[test]
fn a_switch_on_the_mode_is_cut_to_the_arm_the_call_picks() {
    let source = format!(
        "static int pick(int mode, int x)\n{{\n  switch (mode) {{\n  case 0: return x + 1;\n  \
         case 1: return x - 1;\n  default: break;\n  }}\n{}  return x;\n}}\n\
         int f(int x) {{ return pick(0, x); }}\n\
         int g(int x) {{ return pick(1, x); }}\n\
         int k(int x) {{ return pick(2, x); }}\n\
         int h(int x, int m) {{ return pick(m, x); }}\n",
        tail()
    );
    check("switch", &source, &["f", "g"], &["k", "h"]);
    let (_, said) = compiled("switch", "-O2", &source);
    assert!(!said.contains(&format!("one.c: k: {CUT}")), "{said}");
    assert!(!said.contains(&format!("one.c: h: {CUT}")), "{said}");
}

/// A test inside a test, where the tail runs only when the first holds and the second does not.
/// `f` and `g` take a cheap way out, each by a test of its own. `k` runs the tail, and at `-O2` it
/// is copied anyway, as the one call left to a function that would go away with it, as gcc 16 does.
#[test]
fn a_test_inside_a_test_is_decided_by_both_constants() {
    let source = format!(
        "static int pick(int mode, int lim, int x)\n{{\n  if (mode > 3) {{\n    if (lim < 10)\n      \
         return x;\n{}  }}\n  return x * 2;\n}}\n\
         int f(int x) {{ return pick(5, 4, x); }}\n\
         int g(int x) {{ return pick(1, 40, x); }}\n\
         int k(int x) {{ return pick(5, 40, x); }}\n",
        tail()
    );
    for level in ["-O2", "-O3"] {
        let (listing, said) = compiled("nest", level, &source);
        for function in ["f", "g", "k"] {
            assert_eq!(picks(&listing, function), 0, "{function} at {level}:\n{listing}\n{said}");
        }
        for function in ["f", "g"] {
            assert!(said.contains(&format!("one.c: {function}: {CUT}")), "{function}:\n{said}");
        }
    }
}
