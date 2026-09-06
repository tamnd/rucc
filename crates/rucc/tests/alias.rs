//! What `__attribute__((alias("target")))` reaches the assembler as, end to end: a second name for
//! something the same file defines, and no second copy of what it names.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! The unit tests in `rucc-sema`, `rucc-asm` and `rucc-object` each cover one step of the trip: the
//! attribute is read, the listing writer turns one into a binding and a `.set`, and the object
//! writer turns one into a symbol table entry. What is left is the trip itself, which is only
//! visible from the outside, so this runs the compiler over C and reads the listing it writes.
//!
//! The listing rather than the object, for the reason `linkage.rs` beside this reads the listing:
//! it is what a person debugging this reads, and reading a symbol table would mean a dependency the
//! top crate does not otherwise have.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the directives this
/// compares are the same directives on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-alias-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler wrote for that source, and whether it agreed to write anything.
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
    let wrote = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), wrote, said)
}

/// The assembly the compiler writes for source it accepts.
fn asm(what: &str, source: &str) -> String {
    let (ok, wrote, said) = compile(what, source);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    wrote
}

#[test]
fn a_second_name_reaches_the_same_object_the_first_one_does() {
    let text = asm(
        "object",
        "\
int a = 7;
extern int b __attribute__((alias(\"a\")));
",
    );
    assert!(text.contains("\t.set\tb,a\n"), "no second name:\n{text}");
    assert!(text.contains("\t.globl\tb\n"), "the second name was not offered:\n{text}");
    // One image and not two, which is the point of an alias: the four bytes belong to `a` and `b`
    // is an entry in a table pointing at them.
    assert_eq!(text.matches("\t.long\t7\n").count(), 1, "the object was written twice:\n{text}");
    // No type and no size on the second name, because an assembler takes both from the first one
    // and writing them again would only be a second chance to disagree.
    assert!(!text.contains("\t.type\tb,"), "{text}");
    assert!(!text.contains("\t.size\tb,"), "{text}");
}

#[test]
fn a_second_name_may_be_global_where_the_first_one_is_not() {
    // What makes the attribute worth having: `helper` is a name no other file may reach and `shim`
    // is a name every file may, and they are one function.
    let text = asm(
        "function",
        "\
static int helper(int x) {
    return x + 1;
}

extern int shim(int) __attribute__((alias(\"helper\")));
",
    );
    assert!(text.contains("\t.set\tshim,helper\n"), "{text}");
    assert!(text.contains("\t.globl\tshim\n"), "{text}");
    assert!(!text.contains("\t.globl\thelper\n"), "the target was offered too:\n{text}");
    // The body is written once, under the name that has one.
    assert!(text.contains("\nhelper:\n"), "{text}");
    assert!(!text.contains("\nshim:\n"), "the second name got a body:\n{text}");
}

#[test]
fn the_target_of_a_second_name_is_kept_even_when_only_the_alias_reaches_it() {
    // A `static` function nothing in the file calls is not emitted, and an alias of one is a caller
    // the compiler cannot see the call from, so the target has to survive that pass.
    let text = asm(
        "reached",
        "\
static int quiet(void) {
    return 3;
}

extern int loud(void) __attribute__((alias(\"quiet\")));
",
    );
    assert!(text.contains("\nquiet:\n"), "the target was dropped as unreferenced:\n{text}");
    assert!(text.contains("\t.set\tloud,quiet\n"), "{text}");
}

#[test]
fn a_second_name_for_something_no_file_defines_is_refused() {
    // Not an undefined reference: an alias is a second name for an address rather than a use of
    // one, so there is nothing for the linker to go looking for.
    let (ok, _, said) = compile(
        "undefined",
        "\
extern int b __attribute__((alias(\"nowhere\")));
",
    );
    assert!(!ok, "the compiler wrote a second name for nothing");
    assert!(said.contains("E0697"), "{said}");
    assert!(said.contains("'nowhere'"), "{said}");
}

#[test]
fn what_is_aliased_has_to_be_written_as_a_string() {
    // `alias(a)` reads as an identifier rather than an expression, because `format(printf, 1, 2)`
    // does, so the two arrive here differently and both have to be turned away.
    let (ok, _, said) = compile(
        "identifier",
        "\
int a = 7;
extern int b __attribute__((alias(a)));
",
    );
    assert!(!ok, "the compiler took an identifier as a symbol name");
    assert!(said.contains("E0695"), "{said}");
}
