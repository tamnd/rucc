//! What `__attribute__((constructor))` and `__attribute__((destructor))` reach the assembler as,
//! end to end: one pointer wide entry per function, in the section the format's startup code calls
//! what it finds in.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! The unit tests underneath cover one step each: `rucc-sema` reads the attribute and its priority,
//! `rucc-asm` writes the section directive with the type a startup list carries, and `rucc-object`
//! writes the section header. What is left is the trip itself, which is only visible from the
//! outside, so this runs the compiler over C and reads the listing it writes.
//!
//! Three targets rather than one, because the section name is the whole of what changes between
//! them and a name that is wrong is a program whose constructors quietly do not run.

use std::path::PathBuf;
use std::process::Command;

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-ctor-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler wrote for that source on that target, and whether it agreed to write anything.
fn compile(what: &str, target: &str, source: &str) -> (bool, String, String) {
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
fn asm(what: &str, target: &str, source: &str) -> String {
    let (ok, wrote, said) = compile(what, target, source);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    wrote
}

/// One of each, with a body, which is the whole of what the attributes ask for.
const BOTH: &str = "\
static int state;

__attribute__((constructor)) static void setup(void) {
    state = 1;
}

__attribute__((destructor)) static void teardown(void) {
    state = 0;
}

int reads(void) { return state; }
";

#[test]
fn an_elf_file_gets_an_entry_in_each_of_the_two_arrays() {
    let text = asm("elf", "x86_64-unknown-linux-gnu", BOTH);
    assert!(text.contains("\t.section\t.init_array,\"aw\",@init_array\n"), "{text}");
    assert!(text.contains("\t.section\t.fini_array,\"aw\",@fini_array\n"), "{text}");
    // The address of the function rather than a copy of anything, which is what the startup code
    // calls. It is a relocation because where the function lands is the linker's answer.
    assert!(text.contains("\t.quad\tsetup\n"), "no entry for the constructor:\n{text}");
    assert!(text.contains("\t.quad\tteardown\n"), "no entry for the destructor:\n{text}");
    // The body is still written, and the attribute is the only thing that keeps it: nothing in the
    // file calls either function and both are `static`.
    assert!(text.contains("\nsetup:\n"), "{text}");
    assert!(text.contains("\nteardown:\n"), "{text}");
}

#[test]
fn a_priority_goes_in_the_section_name_and_sorts_ahead_of_one_without() {
    let text = asm(
        "priority",
        "x86_64-unknown-linux-gnu",
        "\
__attribute__((constructor)) void last(void) {}
__attribute__((constructor(101))) void first(void) {}
",
    );
    // The number is in the name because an ELF linker sorts the numbered sections by it and puts
    // the whole run of them in front of the unnumbered one, whichever order the files were linked
    // in. Five digits, which is what gcc writes and what makes the sort the linker does textual.
    assert!(text.contains("\t.section\t.init_array.00101,\"aw\",@init_array\n"), "{text}");
    assert!(text.contains("\t.section\t.init_array,\"aw\",@init_array\n"), "{text}");
    // And in this file as well, which is what the other two formats rely on: they have no sorting
    // and run the entries in the order the section holds them.
    let numbered = text.find(".init_array.00101").expect("the numbered entry");
    let plain = text.find(".section\t.init_array,").expect("the unnumbered entry");
    assert!(numbered < plain, "the numbered entry was written second:\n{text}");
}

#[test]
fn a_declaration_with_the_attribute_and_no_definition_under_it_emits_nothing() {
    // Which is what lets a header write the attribute: the file that includes it and never defines
    // the function has nothing to put in the array, since an entry is an address.
    let text = asm(
        "declared",
        "x86_64-unknown-linux-gnu",
        "\
__attribute__((constructor)) void elsewhere(void);

int calls(void) { return 0; }
",
    );
    assert!(
        !text.contains(".init_array"),
        "an entry for something this file defines nothing of:\n{text}"
    );
}

#[test]
fn a_windows_file_gets_an_entry_the_c_runtime_walks() {
    let text = asm(
        "coff",
        "x86_64-pc-windows-msvc",
        "\
__attribute__((constructor)) void setup(void) {}
__attribute__((constructor(101))) void sooner(void) {}
",
    );
    // COFF has no section types and no sorting of its own. What it has is the `$`: the linker
    // gathers the sections whose names match up to it and orders them by what follows, so the CRT
    // walking from `.CRT$XCA` to `.CRT$XCZ` finds them in name order.
    assert!(text.contains("\t.section\t.CRT$XCU,\"dw\"\n"), "{text}");
    assert!(text.contains("\t.section\t.CRT$XCA00101,\"dw\"\n"), "{text}");
    assert!(text.contains("\t.quad\tsetup\n"), "{text}");
}

#[test]
fn a_darwin_file_gets_an_entry_dyld_calls() {
    let text = asm(
        "macho",
        "x86_64-apple-darwin",
        "\
__attribute__((constructor)) void setup(void) {}
",
    );
    // The section attribute is what makes dyld call what is in it, and it is part of the name here
    // because a Mach-O section directive carries the segment, the section and the attributes.
    assert!(text.contains("\t.section\t__DATA,__mod_init_func,mod_init_funcs\n"), "{text}");
    assert!(text.contains("\t.quad\t_setup\n"), "{text}");
}

#[test]
fn a_destructor_is_refused_on_the_two_formats_that_have_nowhere_to_put_one() {
    // Refused rather than dropped. The whole point of the attribute is that something else calls
    // the function, and a program that quietly does not get its call has no way of noticing until
    // whatever the function was to undo is left undone.
    for target in ["x86_64-pc-windows-msvc", "x86_64-apple-darwin"] {
        let (ok, _, said) = compile(
            "refused",
            target,
            "\
__attribute__((destructor)) void teardown(void) {}
",
        );
        assert!(!ok, "{target} wrote a destructor entry");
        assert!(said.contains("E0519"), "{said}");
        assert!(said.contains("'destructor'"), "{said}");
    }
}

#[test]
fn a_priority_outside_the_range_is_an_error_and_a_reserved_one_is_a_warning() {
    let (ok, _, said) = compile(
        "range",
        "x86_64-unknown-linux-gnu",
        "\
__attribute__((constructor(70000))) void late(void) {}
",
    );
    assert!(!ok, "a priority wider than the field was taken");
    assert!(said.contains("0 to 65535"), "{said}");
    // The low hundred are the C library's, so a program may ask for one and is told that it did.
    // gcc warns about it without being asked and so does this.
    let (ok, text, said) = compile(
        "reserved",
        "x86_64-unknown-linux-gnu",
        "\
__attribute__((constructor(50))) void early(void) {}
",
    );
    assert!(ok, "a reserved priority was refused:\n{said}");
    assert!(said.contains("reserved for the implementation"), "{said}");
    assert!(text.contains("\t.section\t.init_array.00050,\"aw\",@init_array\n"), "{text}");
}

#[test]
fn the_attribute_on_something_that_is_not_a_function_is_ignored() {
    let (ok, text, said) = compile(
        "object",
        "x86_64-unknown-linux-gnu",
        "\
__attribute__((constructor)) int not_a_function;
",
    );
    assert!(ok, "{said}");
    assert!(said.contains("E0703"), "{said}");
    assert!(!text.contains(".init_array"), "an object was put in the array:\n{text}");
}
