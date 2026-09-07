//! How the address of a name is come by, end to end: the address of a function this file only
//! declares has to be read out of the global offset table rather than worked out from where the
//! instruction is.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3.
//!
//! Everything this compiler emits is position independent, and in a position independent link the
//! distance from an instruction to a name is a number the linker only has when it is putting both
//! ends in the same program. A function another object may be the one that defines is not such a
//! name, so asking for the distance is a link that fails rather than a program that runs.
//!
//! The unit tests in `rucc-codegen`, `rucc-mir`, `rucc-asm` and `rucc-object` each cover one step:
//! which names need the table, that the machine instruction carries the fact, that the listing
//! spells it, and that the encoder asks for the other relocation. What is left is the trip itself,
//! which is only visible from the outside, so this runs the compiler over C and reads the listing.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the directives this
/// compares are the same directives on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// One address of each kind: a function no file here defines, a function this file does, and an
/// object no file here defines.
///
/// All three are taken rather than called or read, because taking the address is the case the
/// relocation is about. A call goes to the name through a stub the linker is free to make, which
/// is a different relocation and was already right.
const SOURCE: &str = "\
extern void away(int);
extern int object;

void here(int x) {
    (void)x;
}

void (*taken)(int);
void (*mine)(int);
int *pointed;

void take(void) {
    taken = away;
    mine = here;
    pointed = &object;
}
";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-offset-table-{}-{what}", std::process::id()));
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
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

#[test]
fn the_address_of_a_function_this_file_does_not_define_is_read_out_of_the_table() {
    let text = asm("away", SOURCE);
    assert!(text.contains("away@GOTPCREL(%rip)"), "{text}");
    // A load and not an address computation, which is the whole of the difference: the slot holds
    // the address, so what reads it gets an address rather than a place.
    assert!(
        !text.contains("leaq\taway(%rip)"),
        "the address was worked out rather than read:\n{text}"
    );
}

#[test]
fn the_address_of_a_function_this_file_defines_is_worked_out_from_where_the_code_is() {
    let text = asm("here", SOURCE);
    assert!(text.contains("leaq\there(%rip)"), "{text}");
    assert!(!text.contains("here@GOTPCREL"), "a name in this file went through the table:\n{text}");
}

#[test]
fn the_address_of_an_object_another_file_defines_is_worked_out_the_same_way() {
    // Not an oversight and not a case waiting to be fixed. An object a shared library defines is
    // one the linker answers by making room for it in this program and copying it there, so the
    // name really does end up somewhere this file can measure to. A function cannot be copied,
    // because it has one address every object in the program has to agree on.
    let text = asm("object", SOURCE);
    assert!(text.contains("leaq\tobject(%rip)"), "{text}");
    assert!(!text.contains("object@GOTPCREL"), "an object went through the table:\n{text}");
}

#[test]
fn a_call_to_a_function_this_file_does_not_define_still_goes_straight_to_the_name() {
    // The other half of the same fact. A call may go through a stub the linker writes, and the
    // relocation for a call already says so, so nothing about a call changes.
    let text = asm("call", "extern void away(int);\nvoid go(void) { away(1); }\n");
    assert!(text.contains("\tcall\taway\n"), "{text}");
    assert!(!text.contains("away@GOTPCREL"), "a call went through the table:\n{text}");
}
