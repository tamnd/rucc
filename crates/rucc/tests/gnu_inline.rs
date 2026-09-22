//! A program that defines for itself a name one of its headers offered an `extern inline` body
//! for, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! This is what every program that includes `stdio.h` at `-O1` or above and then writes its own
//! `vprintf` or `putchar` is doing, micropython's `shared/libc/printf.c` among them. glibc's
//! `bits/stdio.h` defines those names with `extern __inline __attribute__((__gnu_inline__))`, a
//! body offered for inlining and never emitted, so the program supplying one is giving the name
//! its only definition rather than writing a second one. The unit tests in `rucc-sema` cover which
//! orderings are taken; what is left is the fact only the listing shows, which is that the body in
//! the object file is the program's and not the header's.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the listing this reads is
/// the same listing on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-gnu-inline-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler makes of that source, as the listing and what it said about it.
fn compile(what: &str, source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), text, said)
}

#[test]
fn the_programs_own_body_is_the_one_that_reaches_the_object_file() {
    let source = "\
extern __inline __attribute__((__gnu_inline__)) int f(void) {
    return 1;
}

int f(void) {
    return 2;
}
";
    let (ok, text, said) = compile("replaces", source);
    assert!(ok, "the compiler refused a pair gcc takes:\n{said}");

    // gcc-16 writes `movl $2, %eax` here at every optimisation level. The header's body is offered
    // and dropped, the program's is emitted, and there is one definition of the name rather than
    // two or none.
    assert!(text.contains("$2"), "the header's body was emitted instead:\n{text}");
    assert!(!text.contains("$1"), "both bodies reached the listing:\n{text}");
    assert!(text.contains("\t.globl\tf\n"), "the definition was held back:\n{text}");
}

#[test]
fn a_body_offered_underneath_a_definition_is_still_a_redefinition() {
    let source = "\
int f(void) {
    return 2;
}

extern __inline __attribute__((__gnu_inline__)) int f(void) {
    return 1;
}
";
    let (ok, _, said) = compile("reversed", source);

    // The other order, which gcc refuses. What is taken is a definition replacing a body that was
    // only ever offered, and an offer underneath a definition is not that.
    assert!(!ok, "a second body was accepted");
    assert!(said.contains("redefinition of 'f'"), "got {said}");
}
