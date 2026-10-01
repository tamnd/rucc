//! What `__attribute__((weakref("target")))` reaches the assembler as, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! A weakref makes a `static` name another spelling of a symbol some other object may or may not
//! define. A reference through it is weak, so the link leaves the target undefined when nothing
//! defines it and the name reads as a null pointer, which is how a library asks whether an
//! optional piece is linked in. gcc writes `.weakref local, target` and lets the assembler work
//! out the rest: the target is a weak undefined symbol unless the object also refers to it by its
//! own name, which makes it an ordinary one, or defines it, which makes it that definition. This
//! compiler writes the answer rather than the directive, which is the same symbol table.
//!
//! The listing rather than the object, for the reason `weak.rs` beside this reads the listing.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the directives this
/// compares are the same directives on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-weakref-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler wrote for that source for a target, and whether it agreed to write it.
fn compile_for(target: &str, what: &str, source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
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
    let (ok, wrote, said) = compile_for(TARGET, what, source);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    wrote
}

/// What the compiler said about source it refuses.
fn refused(target: &str, what: &str, source: &str) -> String {
    let (ok, _, said) = compile_for(target, what, source);
    assert!(!ok, "the compiler accepted the fixture");
    said
}

/// A weakref to something nothing defines is a weak undefined reference to it, under its own
/// name, and the test of the local name is a test of that symbol's address.
#[test]
fn a_weakref_to_a_missing_symbol_is_a_weak_reference_to_it() {
    let text = asm(
        "missing",
        "\
static int maybe(void) __attribute__((weakref(\"real_maybe\")));
int ask(void) { return maybe ? maybe() : 0; }
",
    );

    assert!(text.contains("\t.weak\treal_maybe\n"), "{text}");
    assert!(text.contains("real_maybe"), "{text}");
    // The local spelling is not a symbol of its own, since there is nothing here for it to be.
    assert!(!text.contains("maybe:"), "{text}");
    assert!(!text.contains("\t.globl\treal_maybe"), "{text}");
}

/// The older spelling, `weakref` with the target given to `alias`, is the same reference, and an
/// object is referred to the same way as a function.
#[test]
fn a_weakref_named_through_alias_is_the_same_reference() {
    let text = asm(
        "through-alias",
        "\
static int maybe(void) __attribute__((weakref, alias(\"real_maybe\")));
static int value __attribute__((weakref(\"real_value\")));
int ask(void) { return (maybe ? maybe() : 0) + (&value ? value : 0); }
",
    );

    assert!(text.contains("\t.weak\treal_maybe\n"), "{text}");
    assert!(text.contains("\t.weak\treal_value\n"), "{text}");
}

/// A use of the target by its own name is an ordinary reference, and the object has one symbol
/// for the target, so the weakref beside it does not make that one weak.
#[test]
fn a_weakref_beside_an_ordinary_use_is_an_ordinary_reference() {
    let text = asm(
        "strong",
        "\
extern int needed(void);
static int maybe(void) __attribute__((weakref(\"needed\")));
int ask(void) { return (maybe ? maybe() : 0) + needed(); }
",
    );

    assert!(!text.contains("\t.weak\tneeded"), "{text}");
    assert!(text.contains("needed"), "{text}");
}

/// A declaration of the target that nothing uses is not a reference, so the weakref stays weak.
/// This is the conformance case's shape: `nowhere` is declared and only reached through the
/// weakref.
#[test]
fn a_declaration_nothing_uses_leaves_the_weakref_weak() {
    let text = asm(
        "declared",
        "\
extern int nowhere(void);
static int missing(void) __attribute__((weakref(\"nowhere\")));
int ask(void) { return missing == 0; }
",
    );

    assert!(text.contains("\t.weak\tnowhere\n"), "{text}");
}

/// A target the file defines is what the weakref reaches, even a `static` one nothing else calls,
/// which is kept for it rather than dropped.
#[test]
fn a_weakref_to_something_the_file_defines_reaches_the_definition() {
    let text = asm(
        "defined",
        "\
static int here(void) { return 3; }
static int by_weakref(void) __attribute__((weakref(\"here\")));
int ask(void) { return by_weakref(); }
",
    );

    assert!(text.contains("here:"), "{text}");
    assert!(!text.contains("\t.weak\there"), "{text}");
}

/// gcc's rules about where one may be written, in gcc's words.
#[test]
fn a_weakref_is_refused_where_gcc_refuses_it() {
    let said = refused(TARGET, "public", "int e(void) __attribute__((weakref(\"x\")));\n");
    assert!(said.contains("'weakref' symbol 'e' must have static linkage"), "{said}");

    let said =
        refused(TARGET, "order", "static int e(void) __attribute__((alias(\"x\"), weakref));\n");
    assert!(said.contains("'weakref' attribute must appear before 'alias' attribute"), "{said}");

    let said = refused(TARGET, "valued", "static int e __attribute__((weakref(\"x\"))) = 3;\n");
    assert!(said.contains("'e' defined both normally and as 'alias' attribute"), "{said}");

    let said = refused(
        TARGET,
        "redefined",
        "static int e(void) __attribute__((weakref(\"x\")));\nstatic int e(void) { return 0; }\n",
    );
    assert!(said.contains("redefinition of 'e'"), "{said}");
}

/// A bare `weakref` with no target anywhere is a warning, and the declaration is then an ordinary
/// one, which is what gcc makes of it.
#[test]
fn a_weakref_with_no_target_is_warned_about_and_left_ordinary() {
    let (ok, text, said) = compile_for(
        TARGET,
        "bare",
        "\
int hook(void) __attribute__((weakref));
int ask(void) { return hook(); }
",
    );
    assert!(ok, "{said}");
    assert!(
        said.contains("'weakref' attribute should be accompanied with an 'alias' attribute"),
        "{said}"
    );
    assert!(!text.contains("\t.weak\thook"), "{text}");
}

/// A target with no ELF has no way to write the reference, so it is refused there rather than
/// dropped.
#[test]
fn a_weakref_is_refused_where_the_object_format_has_no_way_to_write_it() {
    let source = "static int maybe(void) __attribute__((weakref(\"real\")));\n";
    for target in ["x86_64-w64-mingw32", "arm64-apple-darwin"] {
        let said = refused(target, target, source);
        assert!(said.contains("'weakref' attribute is not supported"), "{target}: {said}");
    }
}
