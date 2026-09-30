//! What a call to a function carrying `__attribute__((error("...")))` or `warning("...")` gets said
//! about it, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! The attribute is a promise about the optimizer as much as about the call: gcc reports a call
//! that survives to code generation and nothing about one it took out. The kernel's
//! `BUILD_BUG_ON` is written against that, a call under a condition that is only a constant once
//! the inline function it is in has been inlined, so the only place the behaviour can be checked
//! is from the outside, over a source that needs the optimizer to settle it. What is compared is
//! what gcc 16 says for the same sources.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that what this reads is the same
/// on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-notices-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took that source with those flags, the listing and what it said.
fn compile(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(flags)
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), text, said)
}

/// The kernel's `BUILD_BUG_ON`, as `include/linux/compiler_types.h` and `build_bug.h` spell it,
/// inside an inline function whose argument is what settles it.
const KERNEL: &str = "\
#define __compiletime_error(msg) __attribute__((__error__(msg)))
#define ___compiletime_assert(condition, msg, prefix, suffix) \\
\tdo { \\
\t\t__attribute__((__noreturn__)) extern void prefix ## suffix(void) __compiletime_error(msg); \\
\t\tif (!(condition)) prefix ## suffix(); \\
\t} while (0)
#define _compiletime_assert(c, m, p, s) ___compiletime_assert(c, m, p, s)
#define compiletime_assert(c, m) _compiletime_assert(c, m, __compiletime_assert_, __COUNTER__)
#define BUILD_BUG_ON(c) compiletime_assert(!(c), \"BUILD_BUG_ON failed: \" #c)

static inline __attribute__((always_inline)) int shift(int by) {
\tBUILD_BUG_ON(by >= 32);
\treturn 1 << by;
}
int small(void) { return shift(3); }
";

#[test]
fn a_build_bug_on_the_inliner_settles_false_says_nothing() {
    for level in ["-O1", "-O2", "-Os"] {
        let (ok, text, said) = compile(&format!("kernel-fine{level}"), &[level], KERNEL);
        assert!(ok, "{level}: {said}");
        assert!(said.is_empty(), "{level}: {said}");
        assert!(!text.contains("__compiletime_assert_0"), "{level}: {text}");
    }
}

#[test]
fn a_build_bug_on_the_inliner_settles_true_is_an_error_quoting_the_message_at_the_call() {
    let source = format!("{KERNEL}int large(void) {{ return shift(40); }}\n");
    for level in ["-O1", "-O2"] {
        let (ok, _, said) = compile(&format!("kernel-bad{level}"), &[level], &source);
        assert!(!ok, "{level}: the call survived and was not refused");
        // The line of the `BUILD_BUG_ON`, which is where the call was written. gcc names the same
        // line, through a note about the macro the call is spelled in.
        let wanted = "one.c:12:2: error: call to '__compiletime_assert_0' declared with attribute \
                      error: BUILD_BUG_ON failed: by >= 32";
        assert!(said.contains(wanted), "{level}: {said}");
        assert_eq!(said.matches(": error: ").count(), 1, "{level}: {said}");
    }
}

/// A call in a branch the optimizer took out is not in the program, at every level: gcc folds an
/// `if (0)` at `-O0` as well, and so does this compiler.
#[test]
fn a_call_in_code_that_was_taken_out_says_nothing() {
    let source = "\
extern void bad(void) __attribute__((error(\"never\")));
void f(void) { if (0) bad(); }
void g(void) { if (sizeof(int) == 3) bad(); }
static inline void check(int n) { if (n > 4) bad(); }
void h(void) { check(2); }
";
    for level in ["-O0", "-O1", "-O2"] {
        let flags: &[&str] = &[level];
        let (ok, text, said) = compile(&format!("gone{level}"), flags, source);
        if level == "-O0" {
            // Nothing is inlined at `-O0`, so the copy of `check` is emitted and its call is one
            // the program makes, which gcc reports too.
            assert!(!ok, "{said}");
            assert!(said.contains("one.c:4:46: error: call to 'bad'"), "{said}");
            continue;
        }
        assert!(ok, "{level}: {said}");
        assert!(said.is_empty(), "{level}: {said}");
        // Every call to `check` went in, so gcc emits no copy of it and neither does this.
        assert!(!text.contains("check"), "{level}: {text}");
    }
}

#[test]
fn a_warning_attribute_is_a_warning_and_the_listing_is_still_written() {
    let source = "\
extern void meh(void) __attribute__((warning(\"meh is slow\")));
void f(int x) { if (x) meh(); }
";
    let (ok, text, said) = compile("warning", &["-O2"], source);
    assert!(ok, "{said}");
    let wanted = "one.c:2:24: warning: call to 'meh' declared with attribute warning: meh is slow";
    assert!(said.contains(wanted), "{said}");
    assert!(text.contains("meh"), "{text}");
    let (ok, _, said) = compile("werror", &["-O2", "-Werror"], source);
    assert!(!ok, "-Werror let a warning through: {said}");
}

/// A function may carry both, and a later declaration's message is the one a call is reported
/// with, which is what gcc does.
#[test]
fn both_attributes_are_said_and_the_latest_message_stands() {
    let source = "\
void c(void) __attribute__((error(\"one\"), warning(\"two\")));
void d(void) __attribute__((error(\"first\")));
void d(void) __attribute__((error(\"second\")));
void (*p)(void) = d;
void u(void) { c(); d(); p(); }
";
    let (ok, _, said) = compile("both", &["-O2"], source);
    assert!(!ok, "{said}");
    for wanted in [
        "one.c:5:16: error: call to 'c' declared with attribute error: one",
        "one.c:5:16: warning: call to 'c' declared with attribute warning: two",
        "one.c:5:21: error: call to 'd' declared with attribute error: second",
    ] {
        assert!(said.contains(wanted), "{wanted} in\n{said}");
    }
    // A call through a pointer is not a call to `d` as far as anyone can tell, and gcc says nothing
    // about it either.
    assert_eq!(said.matches("call to").count(), 3, "{said}");
}
