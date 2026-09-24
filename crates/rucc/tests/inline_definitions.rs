//! What happens to a body C 6.7.4p7 says this unit emits nothing for, when this unit calls it.
//!
//! Design: `spec/08-ir.md` section 8.1.
//!
//! The bargain an inline definition offers is that the call is replaced by the body, so nobody
//! ever has to resolve the name and no object file has to hold one. A compiler that inlines keeps
//! its end of it. This one inlines only a function marked `always_inline`, so a call left standing
//! is a call to a name nothing defines and the program fails at the link on a function it can see
//! the body of. micropython is
//! a program that does exactly that: `py/misc.h` writes `MP_COMPRESSED_ROM_TEXT` as `inline
//! __attribute__((always_inline))`, nothing defines it out of line, and every file that reports an
//! error calls it.
//!
//! So a copy goes out of line, weak, in the units that call one. This reads the listing, because
//! what matters is which symbols the object file offers and under what binding, and that is only
//! visible from the outside.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the listing this reads is
/// the same listing on every machine that runs the suite, and so that nothing here depends on the
/// C library the host happens to have.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-inline-defs-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source, at the level asked for.
fn asm(what: &str, level: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .arg(level)
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

#[test]
fn an_inline_definition_this_file_calls_is_emitted_weak() {
    let source = "\
inline __attribute__((always_inline)) int pick(int x) {
    return x + 1;
}

int main(void) {
    return pick(1);
}
";
    for level in ["-O0", "-O1", "-O2"] {
        let text = asm("called", level, source);
        // Defined, so the call has something to reach, and weak, so the several files that each
        // include the header this came out of do not hand the linker several definitions of one
        // name. A file that does hold the real external definition beats every one of these,
        // because a strong definition beats a weak one.
        assert!(text.contains("\t.weak\tpick\n"), "{level}:\n{text}");
        assert!(text.contains("\npick:\n"), "{level}:\n{text}");
        assert!(!text.contains("\t.globl\tpick\n"), "{level}:\n{text}");
    }
}

#[test]
fn an_inline_definition_nothing_in_the_file_calls_is_still_emitted_as_nothing() {
    let source = "\
inline int unused(int x) {
    return x + 1;
}

int main(void) {
    return 0;
}
";
    let text = asm("unused", "-O2", source);
    // Which is what keeps a file that includes a header full of these from carrying a copy of
    // every one of them. glibc writes `vprintf`, `putchar`, `getchar` and a dozen more that way,
    // and a program that calls one of them should pay for one of them.
    assert!(!text.contains("\nunused:\n"), "{text}");
    assert!(!text.contains("\t.weak\tunused\n"), "{text}");
    assert!(!text.contains("\t.globl\tunused\n"), "{text}");
}

#[test]
fn a_definition_this_file_does_emit_is_offered_to_the_linker_as_it_was() {
    let source = "\
inline int pick(int x) {
    return x + 1;
}

int pick(int x);

int main(void) {
    return pick(1);
}
";
    let text = asm("external", "-O2", source);
    // The plain declaration underneath makes this an external definition under 6.7.4p7, so there
    // is nothing to copy and nothing to weaken: it is the definition of the name and it is
    // offered as one.
    assert!(text.contains("\t.globl\tpick\n"), "{text}");
    assert!(!text.contains("\t.weak\tpick\n"), "{text}");
}

#[test]
fn a_static_inline_is_not_one_of_these() {
    let source = "\
static inline int pick(int x) {
    return x + 1;
}

int main(void) {
    return pick(1);
}
";
    let text = asm("static", "-O2", source);
    // 6.7.4p7 is about a name with external linkage, so a `static inline` was never an inline
    // definition and is emitted the way any other `static` function this file calls is. Weakening
    // one would be offering the linker a name the program said nothing outside may reach.
    assert!(text.contains("\npick:\n"), "{text}");
    assert!(!text.contains("\t.weak\tpick\n"), "{text}");
    assert!(!text.contains("\t.globl\tpick\n"), "{text}");
}

#[test]
fn what_an_inline_definition_reaches_is_emitted_with_it() {
    let source = "\
static int helper(int x) {
    return x + 1;
}

inline int pick(int x) {
    return helper(x);
}

int main(void) {
    return pick(1);
}
";
    let text = asm("reaches", "-O2", source);
    // The copy is a body like any other and the names in it are references like any other, so the
    // `static` function it calls has to be emitted too. Walking the file without reaching through
    // one of these would leave the copy calling a name this object does not define, which is the
    // link error this is here to stop, one step further in.
    assert!(text.contains("\npick:\n"), "{text}");
    assert!(text.contains("\nhelper:\n"), "{text}");
}

#[test]
fn a_static_function_only_an_uncalled_inline_definition_reaches_is_left_out() {
    let source = "\
static int helper(int x) {
    return x + 1;
}

inline int unused(int x) {
    return helper(x);
}

int main(void) {
    return 0;
}
";
    let text = asm("unreached", "-O2", source);
    // The other side of the same walk. Nothing calls the inline definition, so no copy of it goes
    // out of line, so nothing calls the `static` function either and neither of them is emitted.
    assert!(!text.contains("\nunused:\n"), "{text}");
    assert!(!text.contains("\nhelper:\n"), "{text}");
}
