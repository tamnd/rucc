//! What `__attribute__((visibility(...)))` and `-fvisibility=` reach the assembler as, end to end.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3 and `spec/13-gnu-compat.md` section 13.4.
//!
//! The unit tests each cover one step of the trip: the attribute is read in `rucc-sema`, the flag
//! is parsed in `rucc-driver`, the IR's answer reaches a machine function in `rucc-codegen`, and
//! the listing and the object writer each say it in their own way. What is left is the trip
//! itself, which is only visible from the outside, and it is worth a test of its own because
//! tamnd/rucc#733 was a field that every step handled and that nothing carried between them.
//!
//! The listing rather than the object, for the reason `alias.rs` beside this reads the listing: it
//! is what a person debugging this reads, and reading a symbol table would mean a dependency the
//! top crate does not otherwise have.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the directives this
/// compares are the same directives on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-visibility-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler wrote for that source under those flags, and whether it agreed to write it.
fn compile(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let wrote = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), wrote, said)
}

/// The assembly the compiler writes for source it accepts.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let (ok, wrote, said) = compile(what, flags, source);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    wrote
}

/// The attribute on a function and on a variable, which are two different paths to the listing.
#[test]
fn the_attribute_a_declaration_wrote_reaches_the_listing() {
    let text = asm(
        "attribute",
        &[],
        "\
int shown(void) { return 1; }
int hidden(void) __attribute__((visibility(\"hidden\")));
int hidden(void) { return 2; }
int kept __attribute__((visibility(\"protected\"))) = 3;
",
    );

    assert!(text.contains("\t.hidden\thidden\n"), "{text}");
    assert!(text.contains("\t.protected\tkept\n"), "a variable takes a second path: {text}");
    // Still global to the static linker, which is the half of this that is easy to break: the
    // binding and the visibility are two answers and a writer with one word for both says the
    // wrong one of them.
    assert!(text.contains("\t.globl\thidden\n"), "{text}");
    // And a name nobody marked is left alone rather than given a directive that says default,
    // which is what gcc writes and what an assembler assumes.
    assert!(!text.contains("shown\n\t.hidden"), "{text}");
}

/// The flag says what a name gets when no declaration of it said anything.
///
/// This is the whole point of `-fvisibility=hidden`: a library is compiled with it and every name
/// in it stops being part of the interface, which is smaller to load and faster to call into.
#[test]
fn the_flag_decides_for_every_name_no_declaration_spoke_for() {
    let source = "\
int one(void) { return 1; }
int two = 2;
";
    let hidden = asm("flag", &["-fvisibility=hidden"], source);
    assert!(hidden.contains("\t.hidden\tone\n"), "{hidden}");
    assert!(hidden.contains("\t.hidden\ttwo\n"), "{hidden}");

    // `internal` is hidden plus a promise nothing here reads, so it comes out as the weaker of
    // the two rather than as a refusal over a distinction this compiler does not make.
    let internal = asm("internal", &["-fvisibility=internal"], source);
    assert!(internal.contains("\t.hidden\tone\n"), "{internal}");

    // And without the flag there is nothing to say, since exported is what a name gets anyway.
    let plain = asm("plain", &[], source);
    assert!(!plain.contains(".hidden"), "{plain}");
    assert!(!plain.contains(".protected"), "{plain}");
}

/// A declaration that said something beats the flag, which is what makes the flag usable.
///
/// gcc writes `-fvisibility=hidden` as a default rather than as an override for exactly this
/// reason: a library puts it on the whole tree and marks the dozen names it means to export one
/// at a time. A compiler that let the flag win would export nothing at all, which is the same
/// empty dynamic symbol table tamnd/rucc#733 was about, arrived at from the other end.
#[test]
fn a_declaration_that_asked_beats_the_flag_that_did_not_know_about_it() {
    let text = asm(
        "override",
        &["-fvisibility=hidden"],
        "\
__attribute__((visibility(\"default\"))) int exported(void) { return 1; }
int internal_helper(void) { return 2; }
",
    );

    assert!(!text.contains("\t.hidden\texported\n"), "the attribute wins: {text}");
    assert!(text.contains("\t.hidden\tinternal_helper\n"), "{text}");
}

/// A `static` name is told nothing, whatever was written on it or asked for on the command line.
///
/// It is already invisible to everything outside the file, so there is no dynamic symbol table
/// for it to be in or out of, and gcc writes no directive for one either.
#[test]
fn a_static_name_gets_no_directive_it_would_have_no_use_for() {
    let text = asm(
        "static",
        &["-fvisibility=hidden"],
        "\
static int quiet(void) { return 1; }
int loud(void) { return quiet(); }
",
    );

    assert!(!text.contains("\t.hidden\tquiet\n"), "{text}");
    assert!(text.contains("\t.hidden\tloud\n"), "and the one that has a use for it gets it");
}

/// A second name for something has its own answer, since the attribute is written on the alias.
///
/// `weak, alias, visibility("hidden")` is a name a library keeps to itself while the thing it
/// points at stays exported, which is how glibc writes half of the aliases in it.
#[test]
fn an_alias_answers_for_itself_and_not_for_what_it_points_at() {
    let text = asm(
        "alias",
        &[],
        "\
int real = 7;
extern int second __attribute__((alias(\"real\"), visibility(\"hidden\")));
",
    );

    assert!(text.contains("\t.hidden\tsecond\n"), "{text}");
    assert!(!text.contains("\t.hidden\treal\n"), "what it points at is untouched: {text}");
}

/// Where two declarations of a name disagree, the first one stands.
///
/// The same rule the assembler name and the alias are under, and it is there for the same reason:
/// what a second one would change is an answer everything above it has already been compiled
/// against. gcc keeps the first too and warns about the second, and the warning is not here yet.
#[test]
fn the_first_declaration_to_say_something_is_the_one_that_stands() {
    let text = asm(
        "disagree",
        &[],
        "\
int f(void) __attribute__((visibility(\"hidden\")));
int f(void) __attribute__((visibility(\"protected\")));
int f(void) { return 1; }
",
    );

    assert!(text.contains("\t.hidden\tf\n"), "{text}");
    assert!(!text.contains("\t.protected\tf\n"), "{text}");
}

/// A string the attribute does not take is refused rather than read as one that is close to it.
#[test]
fn a_visibility_nobody_defined_is_refused() {
    let (ok, _, said) =
        compile("unknown", &[], "int f(void) __attribute__((visibility(\"invisible\")));\n");

    assert!(!ok, "expected this to be refused: {said}");
    assert!(said.contains("E0701"), "{said}");
}
