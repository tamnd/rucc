//! What `__attribute__((section("name")))` reaches the assembler as, end to end, and one program
//! that finds what it put there the way a linker set does.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! The attribute is on the refused list until it works, because ignoring it is wrong code rather
//! than slow code: the kernel's `__init`, every linker set and every table a linker script gathers
//! are built out of it, and a name that stays in `.data` is a table that comes out empty. The IR,
//! the listing and the object writer already carried a section for a variable, and the constructor
//! tables go through that path, so what is tested here is the part the attribute added: sema reading
//! it, a function carrying it down to the listing, and the function after it going back to `.text`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A directory of its own for each fixture, so that two of these running at once do not write the
/// same file.
fn dir(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-section-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// What the compiler said and wrote for one source, run in `dir` with `args` after it.
fn rucc(dir: &Path, source: &str, args: &[&str]) -> (bool, String, String) {
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(dir)
        .args(args)
        .arg("one.c")
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let wrote = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), wrote, said)
}

/// The listing for source the compiler accepts on `target`.
fn asm(what: &str, target: &str, source: &str) -> String {
    let dir = dir(what);
    let (ok, wrote, said) = rucc(&dir, source, &[&format!("--target={target}"), "-S", "-o", "-"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    wrote
}

/// What the compiler said about source it refuses on `target`.
fn refused(what: &str, target: &str, source: &str) -> String {
    let dir = dir(what);
    let (ok, _, said) = rucc(&dir, source, &[&format!("--target={target}"), "-S", "-o", "-"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "the compiler accepted the fixture");
    said
}

const LINUX: &str = "x86_64-unknown-linux-gnu";

/// A function and an object in sections of their own, with a function after them that is not.
const PLACED: &str = "\
__attribute__((section(\".init.text\"))) int early(void) { return 1; }
int table[2] __attribute__((section(\"my_table\"))) = { 1, 2 };
int later(void) { return 2; }
";

#[test]
fn a_function_and_an_object_go_in_the_section_they_name() {
    let text = asm("elf", LINUX, PLACED);
    assert!(text.contains("\t.section\t.init.text,\"ax\",@progbits\n"), "{text}");
    assert!(text.contains("\t.section\tmy_table,\"aw\",@progbits\n"), "{text}");
    // The function after the placed one is back in the text section, and before its label, which
    // is the whole of what keeps it out of `.init.text` and out of the memory the kernel frees.
    let placed = text.find(".init.text").expect("the placed function");
    let back = text[placed..].find("\t.text\n").map(|at| at + placed).expect("a return to .text");
    let later = text.find("\nlater:\n").expect("the later function");
    assert!(back < later, "the function after the placed one followed it in:\n{text}");
}

#[test]
fn an_arm64_listing_names_the_section_too() {
    let text = asm("arm64", "aarch64-unknown-linux-gnu", PLACED);
    assert!(text.contains("\t.section\t.init.text,\"ax\",@progbits\n"), "{text}");
    assert!(text.contains("\t.section\tmy_table,\"aw\",@progbits\n"), "{text}");
}

#[test]
fn a_windows_listing_names_the_section_with_its_own_flags() {
    let text = asm("coff", "x86_64-windows-gnu", PLACED);
    assert!(text.contains("\t.section\t.init.text,\"xr\"\n"), "{text}");
    assert!(text.contains("\t.section\tmy_table,\"dw\"\n"), "{text}");
}

#[test]
fn a_darwin_section_names_its_segment_and_code_is_marked_as_code() {
    let source = "\
__attribute__((section(\"__TEXT,__early\"))) int early(void) { return 1; }
int table[2] __attribute__((section(\"__DATA,__table\"))) = { 1, 2 };
";
    let text = asm("macho", "aarch64-apple-darwin", source);
    assert!(text.contains("\t.section\t__TEXT,__early,regular,pure_instructions\n"), "{text}");
    assert!(text.contains("\t.section\t__DATA,__table\n"), "{text}");
}

#[test]
fn a_darwin_section_without_a_segment_is_refused() {
    let said = refused(
        "segment",
        "aarch64-apple-darwin",
        "int x __attribute__((section(\"table\"))) = 1;\n",
    );
    assert!(said.contains("requires a segment and section separated by a comma"), "{said}");
}

#[test]
fn a_tentative_definition_in_a_section_is_not_offered_to_the_linker_to_merge() {
    // `-fcommon` makes `int x;` a `.comm`, which has no section, so the section would be lost. gcc
    // keeps the section and so does this.
    let dir = dir("common");
    let args = [&format!("--target={LINUX}") as &str, "-fcommon", "-S", "-o", "-"];
    let (ok, text, said) = rucc(&dir, "int x __attribute__((section(\"kept\")));\n", &args);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    assert!(text.contains("\t.section\tkept,\"aw\",@progbits\n"), "{text}");
    assert!(!text.contains(".comm"), "{text}");
}

#[test]
fn an_object_on_the_stack_is_in_no_section_and_saying_otherwise_is_refused() {
    let said = refused(
        "local",
        LINUX,
        "int f(void) { int x __attribute__((section(\"s\"))) = 1; return x; }\n",
    );
    assert!(said.contains("section attribute cannot be specified for local variables"), "{said}");
    // A `static` one inside a function is an object like one at file scope, and is placed.
    let text = asm(
        "static",
        LINUX,
        "int f(void) { static int x __attribute__((section(\"s\"))) = 1; return x; }\n",
    );
    assert!(text.contains("\t.section\ts,\"aw\",@progbits\n"), "{text}");
}

#[test]
fn a_section_named_by_anything_but_a_string_is_refused() {
    let said = refused("number", LINUX, "int x __attribute__((section(1))) = 1;\n");
    assert!(said.contains("'section' attribute argument not a string constant"), "{said}");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_linker_set_finds_what_was_put_in_it() {
    // The use the attribute exists for, run rather than inspected. Each entry is put in a section
    // whose name is a C identifier, so the linker defines `__start_` and `__stop_` around it, and
    // the program walks from one to the other. The functions are placed as well and called through
    // the table, which is the kernel's initcall shape reduced to three entries. An entry that stayed
    // in `.data` is not between the two symbols, and a function whose section was dropped still
    // runs, so the answer is only right when both halves are.
    let dir = dir("set");
    let source = "\
typedef int (*entry)(void);
__attribute__((section(\"rucc_init\"), used)) static int one(void) { return 1; }
__attribute__((section(\"rucc_init\"), used)) static int two(void) { return 2; }
__attribute__((section(\"rucc_init\"), used)) static int four(void) { return 4; }
__attribute__((section(\"rucc_set\"), used)) static entry a = one;
__attribute__((section(\"rucc_set\"), used)) static entry b = two;
__attribute__((section(\"rucc_set\"), used)) static entry c = four;
extern entry __start_rucc_set[], __stop_rucc_set[];
extern char __start_rucc_init[], __stop_rucc_init[];
int main(void) {
    int sum = 0;
    for (entry *e = __start_rucc_set; e < __stop_rucc_set; e++)
        sum += (*e)();
    char *f = (char *)one;
    if (f < __start_rucc_init || f >= __stop_rucc_init)
        return 1;
    return sum == 7 ? 42 : 2;
}
";
    let (ok, _, said) = rucc(&dir, source, &["-o", "prog"]);
    assert!(ok, "the link failed:\n{said}");
    let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(42), "the set was not what was put in it");
}
