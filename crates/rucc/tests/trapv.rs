//! What a build that says its signed arithmetic must stop rather than wrap looks like, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6.
//!
//! This is the one flag in that family that asks for something to be generated. `-fwrapv` and its
//! relatives withdraw a licence and the instructions come out the same, and this replaces the
//! instruction with a call to a routine that does the arithmetic, looks at what it got and stops
//! the program where the answer is not the right one. So the assertions are about which operations
//! became calls, which did not, and what the calls are named, because the names are libgcc's and an
//! object rucc compiled has to stop the same way as an object gcc compiled beside it.
//!
//! What is left alone matters as much as what is not. C already says what an unsigned addition that
//! overflows produces and what a shift that moves a bit past the top does, so neither is checked
//! here and neither is checked by gcc, and the multiply that turns an index into a number of bytes
//! is the compiler's arithmetic rather than the program's and is left alone as well.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the width of a `long` and
/// the name of the routine that goes with it are the same wherever this runs.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// One of each operation that can overflow in a signed type, and one of each that cannot.
const EACH: &str = "\
int add(int a, int b) { return a + b; }
int sub(int a, int b) { return a - b; }
int mul(int a, int b) { return a * b; }
int neg(int a) { return -a; }
int inc(int a) { return ++a; }
int dec(int a) { return --a; }
int div(int a, int b) { return a / b; }
int shift(int a, int b) { return a << b; }
unsigned plain(unsigned a, unsigned b) { return a + b; }
double real(double a, double b) { return a + b; }
int idx(int *p, int i) { return p[i]; }
";

/// The same operation in each width, which is what says the routine is named after the operands
/// rather than after the operation alone.
const WIDTHS: &str = "\
int word(int a, int b) { return a + b; }
long long twice(long long a, long long b) { return a + b; }
short half(short a) { return ++a; }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-trapv-{}-{what}", std::process::id()));
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

/// The IR for that source under those flags, which is where the call first appears.
fn ir(what: &str, flags: &[&str], source: &str) -> String {
    run(what, &["-O0", "--emit=ir"], flags, source)
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

/// The routine that function calls, where it calls one.
fn calls(text: &str, name: &str) -> Option<String> {
    let at = body(text, name).into_iter().find(|line| line.contains("= call @"))?;
    let after = at.split("= call @").nth(1)?;
    Some(after.split('(').next()?.to_owned())
}

#[test]
fn signed_arithmetic_that_can_overflow_becomes_the_routine_that_checks_it() {
    let text = ir("each", &["-ftrapv"], EACH);
    for (name, routine) in [
        ("add", "__addvsi3"),
        ("sub", "__subvsi3"),
        ("mul", "__mulvsi3"),
        ("neg", "__negvsi2"),
        ("inc", "__addvsi3"),
        ("dec", "__subvsi3"),
    ] {
        assert_eq!(
            calls(&text, name).as_deref(),
            Some(routine),
            "{name} in {:?}",
            body(&text, name)
        );
    }
}

/// And nothing else does, because nothing else can overflow in a way C leaves open.
///
/// A divide that overflows is the one case the hardware stops on already, a shift past the top is
/// undefined for a reason of its own that gcc does not check either, unsigned arithmetic and
/// floating point are both defined, and the multiply behind a subscript is the compiler's own
/// arithmetic rather than something the program wrote.
#[test]
fn what_is_defined_already_or_stops_already_is_left_alone() {
    let text = ir("alone", &["-ftrapv"], EACH);
    for name in ["div", "shift", "plain", "real", "idx"] {
        assert_eq!(calls(&text, name), None, "{name} in {:?}", body(&text, name));
    }
}

/// The routine is named after the width of what it works on, and there is none below a word.
///
/// C promotes a `char` and a `short` to an `int` before any operator sees them, so the addition
/// that could overflow is at `int` by the time it is lowered and a step of a `short` object cannot
/// overflow at all: one more than the largest `short` is a number an `int` holds.
#[test]
fn the_routine_is_named_after_the_width_of_what_it_works_on() {
    let text = ir("widths", &["-ftrapv"], WIDTHS);
    assert_eq!(calls(&text, "word").as_deref(), Some("__addvsi3"));
    assert_eq!(calls(&text, "twice").as_deref(), Some("__addvdi3"));
    assert_eq!(calls(&text, "half"), None, "{:?}", body(&text, "half"));
}

/// And a build that says nothing gets none of it, which is what makes this a flag and not a mode.
#[test]
fn a_build_that_did_not_ask_gets_the_instruction() {
    let text = ir("silent", &[], EACH);
    for name in ["add", "sub", "mul", "neg", "inc", "dec"] {
        assert_eq!(calls(&text, name), None, "{name} in {:?}", body(&text, name));
    }
}

/// Wrapping and stopping are two answers to one question, so the last one written is the answer.
///
/// gcc resolves the contradiction that way and says nothing about it in the manual, so this was
/// measured against gcc 16 rather than read: `-ftrapv -fwrapv` emits no checked calls and `-fwrapv
/// -ftrapv` emits them. Both halves are asserted, since a compiler that let one of them win
/// whichever order they came in would pass a test that only wrote them one way round.
#[test]
fn the_last_answer_to_the_signed_question_is_the_one_that_counts() {
    let wraps = ir("wraps", &["-ftrapv", "-fwrapv"], EACH);
    assert_eq!(calls(&wraps, "add"), None, "{:?}", body(&wraps, "add"));
    assert!(
        !body(&wraps, "add").iter().any(|line| line.contains(".nsw")),
        "the flag that won was asked to let the arithmetic wrap"
    );

    let stops = ir("stops", &["-fwrapv", "-ftrapv"], EACH);
    assert_eq!(calls(&stops, "add").as_deref(), Some("__addvsi3"));

    // And the flag that turns it off is the flag that turns it off, rather than the other answer.
    let off = ir("off", &["-ftrapv", "-fno-trapv"], EACH);
    assert_eq!(calls(&off, "add"), None);
    assert!(body(&off, "add").iter().any(|line| line.contains(".nsw")), "nothing wraps either");
}

/// And the call reaches the assembler under the name the runtime gave it.
///
/// The IR above says which routine was chosen and this says the choice survived to the text the
/// assembler reads, which is the half that matters to the linker: the name has to be the one
/// libgcc defines, and rucc links libgcc already.
#[test]
fn the_name_the_runtime_uses_is_the_name_that_is_written_out() {
    let text = run("asm", &["-O2", "-S"], &["-ftrapv"], EACH);
    for routine in ["__addvsi3", "__subvsi3", "__mulvsi3", "__negvsi2"] {
        assert!(
            text.lines()
                .any(|line| line.trim_start().starts_with("call") && line.contains(routine)),
            "{routine} is not called in:\n{text}"
        );
    }
}
