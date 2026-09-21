//! What an assembler name on a declaration reaches the assembler as, end to end, including the
//! calls the program did not write by hand.
//!
//! Design: `spec/13-gnu-compat.md` section 13.3.
//!
//! A declaration is allowed to say what the linker should call it, and a program that declares
//! `memcpy` with a name of its own has said that every copy this file asks the library for goes
//! to that name. It reaches the compiler two ways: written out as `memcpy`, which resolves to the
//! declaration that was renamed, and written out as `__builtin_memcpy`, which resolves to the
//! implicit declaration the checker made for the prefixed spelling and is a different declaration
//! with no label on it. Both are the same function, so both go to the same symbol.
//!
//! The listing rather than the object, for the reason `alias.rs` beside this reads the listing: it
//! is what a person debugging this reads.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the directives this
/// compares are the same directives on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// What every fixture here declares, which is the library renamed and a length the compiler
/// cannot see the value of.
///
/// The length matters: a copy of a size the compiler knows is laid out as moves and reaches no
/// symbol at all, and what this is about is the symbol.
const PRELUDE: &str = "\
typedef __SIZE_TYPE__ size_t;
extern void *memcpy(void *, const void *, size_t) __asm(\"my_memcpy\");
extern void *memset(void *, int, size_t) __asm(\"my_memset\");
extern size_t n;
char to[64], from[64];
";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-rename-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source.
fn asm(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-"])
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

#[test]
fn a_call_written_by_hand_goes_to_the_name_the_declaration_gave_it() {
    let text = asm("hand", &format!("{PRELUDE}void f(void) {{ memcpy(to, from, n); }}\n"));
    assert!(text.contains("\tcall\tmy_memcpy\n"), "the rename was lost:\n{text}");
    assert!(!text.contains("\tcall\tmemcpy\n"), "the plain name was called as well:\n{text}");
}

#[test]
fn a_call_written_with_the_builtin_prefix_goes_to_the_same_name() {
    let text =
        asm("builtin", &format!("{PRELUDE}void f(void) {{ __builtin_memcpy(to, from, n); }}\n"));
    assert!(text.contains("\tcall\tmy_memcpy\n"), "the rename was lost:\n{text}");
    assert!(
        !text.contains("\tcall\tmemcpy\n"),
        "the library was called behind the rename:\n{text}"
    );
}

#[test]
fn the_same_holds_for_every_library_name_a_builtin_falls_back_to() {
    let text = asm("memset", &format!("{PRELUDE}void f(void) {{ __builtin_memset(to, 7, n); }}\n"));
    assert!(text.contains("\tcall\tmy_memset\n"), "the rename was lost:\n{text}");
    assert!(
        !text.contains("\tcall\tmemset\n"),
        "the library was called behind the rename:\n{text}"
    );
}

#[test]
fn a_file_that_renames_nothing_calls_the_library_by_the_name_the_library_uses() {
    let text = asm(
        "plain",
        "\
typedef __SIZE_TYPE__ size_t;
extern size_t n;
char to[64], from[64];
void f(void) { __builtin_memcpy(to, from, n); }
",
    );
    assert!(text.contains("\tcall\tmemcpy\n"), "the builtin reached no library:\n{text}");
}

#[test]
fn a_rename_of_one_library_name_leaves_the_others_alone() {
    let text =
        asm("one", &format!("{PRELUDE}void f(void) {{ __builtin_memmove(to, from, n); }}\n"));
    assert!(text.contains("\tcall\tmemmove\n"), "a name nothing renamed moved:\n{text}");
}
