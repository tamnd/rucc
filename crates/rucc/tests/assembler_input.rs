//! A file of assembly on the command line, end to end.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1.
//!
//! This compiler does not have an assembler yet, and a `.s` or a `.S` next to the C is ordinary in
//! a project with a hot loop in it. What is checked here is the one thing that must be true while
//! that is still the case, which is that the compiler says so. Exiting zero having written nothing
//! is the worse of the two ways to be wrong: every caller that checks the status believes it
//! worked, and what it does next is read a file that is not there.
//!
//! GMP is the project that showed it. Its configure assembles three small files to find out how the
//! local assembler spells a thirty two bit word, reads all three exit statuses as success, finds no
//! object beside any of them, and gives up with `cannot determine how to define a 32-bit word`. The
//! probe never sees a message, so the message is not what fixes GMP, and it is what turns a wrong
//! answer into one somebody can act on.
//!
//! These will change when there is an assembler. That is the point of writing them: the day a `.s`
//! produces an object, every assertion here is about the wrong outcome and fails, which is a better
//! way to find out than a test that passes either way.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, so this is the same question on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// What GMP's probe writes, which has no instruction in it at all: a section, a name, a label and
/// four bytes.
const ASSEMBLY: &str = "\t.text\n\t.globl probe\nprobe:\n\t.long 0\n";

/// A directory of its own, so that two of these running at once do not write the same file.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-asm-input-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// One file written under it.
fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("the fixture can be written");
    path
}

/// Whether the compiler finished, and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn an_assembly_file_that_cannot_be_assembled_is_refused_rather_than_passed_over() {
    let dir = dir("alone");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["-c", "probe.s"]);
    assert!(!ok, "the compiler said it succeeded:\n{said}");
    assert!(said.contains("no assembler"), "{said}");
    assert!(said.contains("probe.s"), "the message does not name the file:\n{said}");
    assert!(!dir.join("probe.o").exists(), "an object appeared after all");
}

#[test]
fn naming_the_output_makes_no_difference_to_that() {
    // Because it is not the rule that derives a name from an input that is missing. Worth its own
    // case: an explicit `-o` is the first thing anybody tries, and it produced the same silence.
    let dir = dir("named");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["-c", "probe.s", "-o", "probe.o"]);
    assert!(!ok, "the compiler said it succeeded:\n{said}");
    assert!(said.contains("no assembler"), "{said}");
    assert!(!dir.join("probe.o").exists(), "an object appeared after all");
}

#[test]
fn assembly_that_wants_the_preprocessor_first_is_the_same_answer() {
    // A `.S` runs through the preprocessor and then needs assembling, so it has one more phase in
    // front of it and the same one missing behind it. Separate because the two kinds are separate
    // in the plan, and a check written against one of them would pass while the other stayed quiet.
    let dir = dir("cpp");
    let text = "#define ZERO 0\n\t.text\n\t.globl probe\nprobe:\n\t.long ZERO\n";
    write(&dir, "probe.S", text);
    let (ok, said) = run(&dir, &["-c", "probe.S"]);
    assert!(!ok, "the compiler said it succeeded:\n{said}");
    assert!(said.contains("no assembler"), "{said}");
    assert!(!dir.join("probe.o").exists(), "an object appeared after all");
}

#[test]
fn a_link_says_it_here_rather_than_letting_the_linker_miss_a_temporary() {
    // The driver does schedule an object for the assembly, in a temporary directory, and hands the
    // path to the linker. Before this the link was the only place that noticed, and what it said
    // was that `ld` cannot find a file in a directory nobody named, which is a message about this
    // and does not read like one.
    let dir = dir("link");
    write(&dir, "main.c", "int probe(void); int main(void) { return probe(); }\n");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["main.c", "probe.s", "-o", "both"]);
    assert!(!ok, "the compiler said it succeeded:\n{said}");
    assert!(said.contains("no assembler"), "{said}");
    assert!(said.contains("probe.s"), "the message does not name the file:\n{said}");
    assert!(!said.contains("cannot find"), "the linker answered for it:\n{said}");
}

#[test]
fn an_object_file_is_not_this_and_is_still_passed_through() {
    // The check is on the phases rather than on the kind, and an object has no compile phase
    // either. It also has no assemble phase, which is the difference, and a compiler that refused
    // `rucc a.o -o a` would have traded one wrong answer for a louder one.
    let dir = dir("object");
    write(&dir, "main.c", "int main(void) { return 0; }\n");
    let (ok, said) = run(&dir, &["-c", "main.c"]);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    let (ok, said) = run(&dir, &["main.o", "-o", "prog"]);
    assert!(ok, "an object file was refused:\n{said}");
    assert!(dir.join("prog").exists(), "nothing was linked");
}

#[test]
fn stopping_before_the_assembler_is_not_this_either() {
    // `rucc -E probe.s` asks for the preprocessor and assembly enters after it, so there is no
    // phase left to run and nothing missing. The plan already notes that and it is not an error.
    let dir = dir("stopped");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["-E", "probe.s", "-o", "out.i"]);
    assert!(ok, "a mode that never reaches the assembler was refused:\n{said}");
    assert!(!said.contains("no assembler"), "{said}");
}
