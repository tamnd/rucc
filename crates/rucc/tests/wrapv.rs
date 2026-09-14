//! What a build that says its arithmetic wraps looks like, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6 and `spec/08-ir.md` section 8.4.
//!
//! These flags are not a request to generate something else. The instructions are the same
//! instructions and the machine gives the same bits either way, and what changes is what the
//! optimizer is allowed to conclude from them. So the assertions are of two kinds: what the
//! licence looks like where it is written down, which is a flag on an instruction, and what a pass
//! that reads it does differently. The first alone would pass on a compiler that wrote the flag and
//! never looked at it again, and the second alone would not say which flag did it.
//!
//! The two questions are separate on purpose. A program can mean its signed arithmetic to wrap and
//! still never walk a pointer off the end of an object, which is why `-fwrapv` and
//! `-fwrapv-pointer` are two flags, and a kernel turns both off with the one older flag that means
//! the pair.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the width an address has is
/// the same wherever this runs.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// One of each: signed arithmetic, unsigned arithmetic that never claimed anything, and an index
/// that has to be scaled before it is added to a pointer.
const EACH: &str = "\
int add(int a, int b) { return a + b; }
int sub(int a, int b) { return a - b; }
int mul(int a, int b) { return a * b; }
int neg(int a) { return -a; }
int inc(int a) { return ++a; }
unsigned plain(unsigned a, unsigned b) { return a + b; }
int idx(int *p, int i) { return p[i]; }
";

/// A loop the optimizer can only work the trip count of out by assuming the counter does not turn
/// round, which is what makes it the thing to measure a withdrawn licence with.
const LOOP: &str = "\
int sum(int *a, int n) { int s = 0; for (int i = 0; i < n; i++) s += a[i]; return s; }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-wrapv-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler wrote for that source under those flags, in whichever form was asked for.
fn run(what: &str, emit: &[&str], flags: &[&str], source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(emit)
        .args(["-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The IR for that source under those flags, which is where the licence is written down.
fn ir(what: &str, flags: &[&str]) -> String {
    run(what, &["-O0", "--emit=ir"], flags, EACH)
}

/// The instructions of one function, without the name of it or the braces around it.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("func @{name}(");
    text.lines()
        .map(str::trim)
        .skip_while(|line| !line.starts_with(&open))
        .skip(1)
        .take_while(|line| *line != "}")
        .filter(|line| !line.is_empty())
        .collect()
}

/// Whether any instruction in that function says it does not wrap.
fn claims(text: &str, name: &str) -> bool {
    body(text, name).iter().any(|line| line.contains(".nsw"))
}

#[test]
fn signed_arithmetic_says_it_does_not_overflow_unless_the_build_said_it_does() {
    let plain = ir("signed", &[]);
    for name in ["add", "sub", "mul", "neg", "inc"] {
        assert!(claims(&plain, name), "{name} in {:?}", body(&plain, name));
    }
    for flag in ["-fwrapv", "-fno-strict-overflow"] {
        let text = ir("signed", &[flag]);
        for name in ["add", "sub", "mul", "neg", "inc"] {
            assert!(!claims(&text, name), "{flag} left {name} claiming it does not wrap");
        }
    }
}

/// And unsigned arithmetic is the same either way, because it never claimed anything.
///
/// C says what an unsigned addition that overflows produces, so there is nothing to assume and
/// nothing for a flag to withdraw. A compiler that took `-fwrapv` as a mode rather than as a
/// licence being withheld would have nothing to change here either, which is why this is asserted
/// beside the case above rather than instead of it.
#[test]
fn unsigned_arithmetic_is_the_same_either_way() {
    for flags in [&[][..], &["-fwrapv"][..], &["-fno-strict-overflow"][..]] {
        let text = ir("unsigned", flags);
        assert!(!claims(&text, "plain"), "{flags:?}");
    }
}

/// The index a subscript is scaled by is the pointer question and answers to the other flag.
#[test]
fn scaling_an_index_says_it_does_not_overflow_unless_the_build_said_it_does() {
    for (flags, want) in [
        (&[][..], true),
        (&["-fwrapv"][..], true),
        (&["-fwrapv-pointer"][..], false),
        (&["-fno-strict-overflow"][..], false),
    ] {
        let text = ir("scale", flags);
        assert_eq!(claims(&text, "idx"), want, "{flags:?} on idx");
    }
}

/// And the older flag is the pair of the newer two rather than a third answer.
///
/// gcc says so itself: its help text for `-fstrict-overflow` reads "negated as `-fwrapv`
/// `-fwrapv-pointer`". So the two halves are asserted apart, since a compiler that read the older
/// flag as the signed one alone would pass every test above.
#[test]
fn the_older_flag_is_both_of_the_newer_ones() {
    let both = ir("both", &["-fno-strict-overflow"]);
    assert!(!claims(&both, "add"), "the signed half");
    assert!(!claims(&both, "idx"), "the pointer half");

    let signed = ir("half", &["-fwrapv"]);
    assert!(!claims(&signed, "add"));
    assert!(claims(&signed, "idx"), "-fwrapv is not the pointer flag");

    // And the last one wins, which is what a build that turns one of these on globally and off for
    // one directory is relying on.
    let back = ir("back", &["-fno-strict-overflow", "-fstrict-overflow"]);
    assert!(claims(&back, "add"));
    assert!(claims(&back, "idx"));
}

/// And a pass that reads the licence does less work with it withdrawn, which is the point.
///
/// The count of a loop whose counter goes up one at a time rests on the counter not turning round,
/// so a build where it may turn round cannot have the count and cannot do the thing the count is
/// for. What the splitter does with it is ask the runtime once in front of the loop how many bytes
/// the walk has room for, and send the iterations that fit down a copy of the body with no checks
/// in it. That question is the measurement, since it is the whole of what the licence bought: it is
/// there in the plain build and it is not there with the licence withdrawn, and the loop is then
/// the one loop it always was with its check still inside it.
///
/// Counting what the safety summary says is left over is what this used to do, and it stopped
/// saying anything once the splitter started reaching this loop, because splitting copies the body
/// and the copy that keeps the checks counts the same as a loop that was never split at all. The
/// number went the right way for a while through how the two builds happened to duplicate blocks
/// rather than because one of them had checked less.
#[test]
fn withdrawing_the_licence_stops_the_optimizer_sizing_the_walk_in_front_of_the_loop() {
    let windows = |flags: &[&str]| {
        let text = run("hoist", &["-O2", "-fsafety=detect", "--emit=ir"], flags, LOOP);
        text.lines().filter(|line| line.contains("cap_extent")).count()
    };
    assert!(windows(&[]) > 0, "nothing sized the walk in the plain build");
    assert_eq!(windows(&["-fwrapv"]), 0, "the flag left a window in front of the loop");
}
