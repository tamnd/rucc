//! What `__attribute__((weak))` reaches the assembler as, end to end.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1 and `spec/13-gnu-compat.md` section 13.4.
//!
//! The attribute asks for two different things and the difference is whether this file defines the
//! name. On a definition it says another object's definition of the same name beats this one,
//! which is how a library ships a default somebody may replace. On a declaration of something this
//! file does not define it says the link may leave the name undefined and hand every reference a
//! zero address, which is how a library offers a hook a profiler may fill in: the calls are written
//! under `if (hook)` and the test is false when nobody filled it in.
//!
//! The second is the half that was missing and is why this file exists. zstd declares four tracing
//! hooks that way and defines none of them, so thirty of its files would not link at all, which is
//! what tamnd/rucc#1414 was. The first half was already reachable through `alias`, since
//! `weak, alias` is how glibc writes half of its second names, and nothing carried the attribute to
//! it.
//!
//! The listing rather than the object, for the reason `visibility.rs` beside this reads the
//! listing: it is what a person debugging this reads, and reading a symbol table would mean a
//! dependency the top crate does not otherwise have. What the object says is held to the same list
//! in `rucc-object`, which is where both paths read it from.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the directives this
/// compares are the same directives on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-weak-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler wrote for that source, and whether it agreed to write it.
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

/// A definition carrying the attribute is one another object may beat.
#[test]
fn a_definition_that_asked_to_lose_is_written_weak_rather_than_global() {
    let text = asm(
        "definition",
        "\
__attribute__((weak)) int shared(void) { return 1; }
__attribute__((weak)) int held = 2;
int ordinary(void) { return 3; }
",
    );

    assert!(text.contains("\t.weak\tshared\n"), "{text}");
    assert!(text.contains("\t.weak\theld\n"), "a variable takes a second path: {text}");
    // And nothing says it twice, since the two directives are two answers to one question and a
    // file that gives both is a file whose assembler picks one.
    assert!(!text.contains("\t.globl\tshared\n"), "{text}");
    assert!(!text.contains("\t.globl\theld\n"), "{text}");
    // A name nobody marked is left alone, which is the half that is easy to break.
    assert!(text.contains("\t.globl\tordinary\n"), "{text}");
}

/// A hook a file declares and nobody defines is a name the link may leave undefined.
///
/// The whole of tamnd/rucc#1414. Without the directive the name arrives at the linker as an
/// ordinary undefined symbol, the link fails, and the program that fails is one that was written to
/// work whether or not anybody filled the hook in.
#[test]
fn a_hook_nobody_defines_is_declared_weak_rather_than_wanted() {
    let text = asm(
        "hook",
        "\
__attribute__((weak)) extern int somebody_elses_hook(int x);
int call_it(int x) { return somebody_elses_hook ? somebody_elses_hook(x) : 0; }
",
    );

    assert!(text.contains("\t.weak\tsomebody_elses_hook\n"), "{text}");
    // Nothing else is said about it, because there is nothing else this file knows: it has no
    // bytes, no size and no section, and what kind of thing it is is the linker's to find out.
    assert!(!text.contains("\t.type\tsomebody_elses_hook"), "{text}");
    assert!(!text.contains("\t.globl\tsomebody_elses_hook"), "{text}");
}

/// An ordinary undefined name is left alone, which is what makes the link fail when it should.
#[test]
fn a_name_nobody_marked_is_still_a_name_the_link_has_to_find() {
    let text = asm(
        "plain",
        "\
extern int elsewhere(int x);
int call_it(int x) { return elsewhere(x); }
",
    );

    assert!(!text.contains(".weak"), "{text}");
}

/// The attribute is a fact about the name, so a header may say it and the definition below need
/// not say it again.
///
/// This is how every library that ships a replaceable default is written: the declaration in the
/// header carries the attribute and the file that defines the name is an ordinary definition.
#[test]
fn a_declaration_that_asked_is_enough_for_the_definition_under_it() {
    let text = asm(
        "carried",
        "\
__attribute__((weak)) int later(void);
int later(void) { return 1; }
",
    );

    assert!(text.contains("\t.weak\tlater\n"), "{text}");
    assert!(!text.contains("\t.globl\tlater\n"), "{text}");
    // And it is the definition that was written weak rather than a second undefined entry for a
    // name this file has right here.
    assert_eq!(text.matches("\t.weak\tlater\n").count(), 1, "{text}");
}

/// A name the linker never sees has nobody to lose to, so the attribute is refused on one.
///
/// gcc refuses it too, in the same words. Accepting it would write `.weak` for a symbol that is
/// local to the file, which an assembler either rejects or turns into a global name, and either of
/// those is a `static` that stopped being one.
#[test]
fn a_static_name_has_nothing_to_give_way_to_and_is_told_so() {
    let (ok, _, said) = compile(
        "static",
        "static int quiet(void) __attribute__((weak));\nstatic int quiet(void) { return 1; }\n",
    );

    assert!(!ok, "expected this to be refused: {said}");
    assert!(said.contains("E0711"), "{said}");
    assert!(said.contains("must be public"), "{said}");
}
