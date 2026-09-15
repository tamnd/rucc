//! A file of assembly on the command line, end to end.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1.
//!
//! A `.s` or a `.S` next to the C is ordinary in a project with a hot loop in it, and until there
//! was an assembler what this file checked was that the compiler said so rather than exiting zero
//! having written nothing. Now there is one for the directives and the labels, so what is checked
//! is the other side of the same question: a file of directives produces a real object, a file with
//! an instruction in it is still refused by name and by line, and neither of them exits zero with
//! nothing beside it.
//!
//! GMP is the project that showed it, and it is the case in `gmp_thirty_two_bit_word_probe` below.
//! Its configure assembles a file with no instruction in it to find out how the local assembler
//! spells a thirty two bit word, and reads the answer out of the value of a symbol in the object, so
//! nothing but an object will do. Before there was one it found no object beside any of its three
//! probes and gave up with `cannot determine how to define a 32-bit word`.
//!
//! What is not checked here is the shape of the object down to the byte. `rucc-object` and
//! `rucc-asm` have their own tests for that, and the comparison that matters is against what gas
//! writes for the same input, which is a corpus run and not a unit test. What is here is the
//! driver's half: the right file appears, with the right name, in the right place.

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

/// Whether a file is a relocatable ELF object.
///
/// The header and nothing past it. Sixteen bytes of identification, then two bytes of type, and
/// type one is the kind a compiler writes. Checking this much is what says the file is an object
/// rather than a copy of its own input, which is the mistake a driver that forwarded the wrong
/// temporary would make and which a test that only asked whether the name exists would miss.
fn is_an_object(path: &Path) -> bool {
    let bytes = std::fs::read(path).expect("the object can be read back");
    bytes.len() > 18 && &bytes[..4] == b"\x7fELF" && bytes[16] == 1 && bytes[17] == 0
}

#[test]
fn gmp_thirty_two_bit_word_probe() {
    let dir = dir("alone");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["-c", "probe.s"]);
    assert!(ok, "the probe was refused:\n{said}");
    let object = dir.join("probe.o");
    assert!(object.exists(), "no object appeared beside the input");
    assert!(is_an_object(&object), "what appeared is not a relocatable object");
    // The name the probe defines has to be in the file, because the value beside it is the whole
    // answer configure is after. It is in the string table, so it is in the bytes.
    let bytes = std::fs::read(&object).expect("the object can be read back");
    assert!(bytes.windows(5).any(|w| w == b"probe"), "the object does not name what it defines");
}

#[test]
fn naming_the_output_puts_it_there() {
    // Because the rule that derives a name from an input and an explicit `-o` are two paths, and a
    // check written against one of them would pass while the other wrote somewhere else.
    let dir = dir("named");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["-c", "probe.s", "-o", "elsewhere.o"]);
    assert!(ok, "the probe was refused:\n{said}");
    assert!(is_an_object(&dir.join("elsewhere.o")), "the object is not where it was asked for");
    assert!(
        !dir.join("probe.o").exists(),
        "it was also written to the name that was not asked for"
    );
}

#[test]
fn assembly_that_wants_the_preprocessor_first_goes_through_it() {
    // A `.S` runs through the preprocessor and then needs assembling, so it has one more phase in
    // front of it. Separate because the two kinds are separate in the plan, and the macro here is
    // the point: a driver that skipped the preprocessor would hand `.long ZERO` to a reader that
    // has never heard of `ZERO` and the file would be refused rather than quietly wrong.
    let dir = dir("cpp");
    let text = "#define ZERO 0\n\t.text\n\t.globl probe\nprobe:\n\t.long ZERO\n";
    write(&dir, "probe.S", text);
    let (ok, said) = run(&dir, &["-c", "probe.S"]);
    assert!(ok, "the probe was refused:\n{said}");
    assert!(is_an_object(&dir.join("probe.o")), "no object appeared beside the input");
}

#[test]
fn an_instruction_is_refused_by_name_and_by_line() {
    // The half that is not written. Refusing it is the whole design of the reader: an assembler
    // that skipped what it did not recognise would write an object that links, and what would be
    // wrong with it is a run of missing bytes in the middle of a function, which nothing finds
    // until the program runs.
    let dir = dir("instruction");
    write(&dir, "hot.s", "\t.text\n\t.globl go\ngo:\n\tmovq %rdi, %rax\n\tret\n");
    let (ok, said) = run(&dir, &["-c", "hot.s"]);
    assert!(!ok, "an instruction was accepted:\n{said}");
    assert!(said.contains("movq"), "the message does not name the instruction:\n{said}");
    assert!(said.contains("hot.s:4"), "the message does not carry the line:\n{said}");
    assert!(!dir.join("hot.o").exists(), "a half-written object was left behind");
}

#[test]
fn a_link_takes_the_object_the_assembly_produced() {
    // The driver schedules an object for the assembly in a temporary directory and hands the path
    // to the linker, and until there was an assembler the link was where it came apart, with `ld`
    // saying it cannot find a file in a directory nobody named. The variable rather than a function
    // because a label and four bytes are not a function body, and what is being checked is that the
    // linker was given something it could resolve a name out of.
    let dir = dir("link");
    write(&dir, "main.c", "extern int probe_value;\nint main(void) { return probe_value; }\n");
    let value = "\t.data\n\t.globl probe_value\n\t.type probe_value, @object\n\t.size probe_value, \
                 4\nprobe_value:\n\t.long 7\n";
    write(&dir, "probe.s", value);
    let (ok, said) = run(&dir, &["main.c", "probe.s", "-o", "both"]);
    assert!(ok, "the link failed:\n{said}");
    assert!(dir.join("both").exists(), "nothing was linked");
    assert!(!said.contains("cannot find"), "the linker was handed a path to nothing:\n{said}");
}

#[test]
fn an_object_file_is_not_this_and_is_still_passed_through() {
    // The check is on the phases rather than on the kind, and an object has no compile phase
    // either. It also has no assemble phase, which is the difference, and a compiler that ran the
    // assembly reader over `a.o` would refuse a file that is already what it was asked to make.
    let dir = dir("object");
    write(&dir, "main.c", "int main(void) { return 0; }\n");
    let (ok, said) = run(&dir, &["-c", "main.c"]);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    let (ok, said) = run(&dir, &["main.o", "-o", "prog"]);
    assert!(ok, "an object file was refused:\n{said}");
    assert!(dir.join("prog").exists(), "nothing was linked");
}

#[test]
fn stopping_before_the_assembler_writes_nothing_and_says_nothing() {
    // A `.s` has no preprocessing phase, so `rucc -E probe.s` asks for a phase the file does not
    // have and there is nothing left to do. gcc 16 exits zero and writes no file at all for the
    // same command, and this is here because the obvious reading of `-E` is that it copies the text
    // through, which would leave a file beside a build that never asked for one.
    let dir = dir("stopped");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["-E", "probe.s", "-o", "out.i"]);
    assert!(ok, "a mode that never reaches the assembler was refused:\n{said}");
    assert!(!dir.join("out.i").exists(), "a file was written for a phase the input does not have");
    assert!(!dir.join("probe.o").exists(), "an object was written by a mode that stops before one");
}
