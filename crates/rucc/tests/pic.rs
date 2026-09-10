//! What `-fPIC` and `-fPIE` reach the assembler as, end to end.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3 and `spec/04-driver-and-cli.md` section 4.6.
//!
//! The two flags meant the same thing until tamnd/rucc#756, which is that an object this compiler
//! wrote could not go into a shared library at all once the code touched a global. The linker
//! stopped on it rather than getting it wrong, and its advice was to recompile with the flag that
//! was already on the command line and being dropped.
//!
//! The listing rather than the object, for the reason `visibility.rs` beside this reads the
//! listing: it is what a person debugging this reads, and reading a relocation table would mean a
//! dependency the top crate does not otherwise have. What the listing cannot show is that the
//! result links and runs, and that is checked by hand against a real linker rather than here,
//! because the suite has no linker for a target that is not the one it is running on.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because this is a question about a
/// format and the answer for the other two formats is a different one.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-pic-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source under those flags.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-"])
        .args(flags)
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

/// One variable defined here, one defined elsewhere, one `static`, and a function that reads all
/// three, which is every case the question has.
const THREE: &str = "\
extern int away;
int here = 1;
static int quiet = 3;
int read_all(void) { return away + here + quiet; }
";

/// An executable reaches every one of them from the instruction pointer.
///
/// Including the one it does not define, which is the part that is easy to disbelieve: the linker
/// answers a reference to a variable some library defines by making room for it in the executable
/// and copying it there, so the name really does end up at a distance this file could have
/// measured. That is a copy relocation and it is why `-fPIE` is cheaper than `-fPIC`.
#[test]
fn an_executable_works_every_address_out_for_itself() {
    for flags in [&[][..], &["-fPIE"], &["-fpie"]] {
        let text = asm("exe", flags, THREE);
        assert!(text.contains("\tmovl\taway(%rip)"), "{flags:?}: {text}");
        assert!(text.contains("\tmovl\there(%rip)"), "{flags:?}: {text}");
        assert!(!text.contains("GOTPCREL"), "{flags:?}: {text}");
    }
}

/// A library reads the exported ones out of the global offset table, and the `static` one not.
///
/// `here` is the surprising one. A name this file plainly defines still cannot be reached from the
/// instruction pointer inside a shared library, because it is exported and something loaded
/// earlier may define it too, and then the address the whole process uses is not the one here.
#[test]
fn a_library_reads_the_exported_ones_out_of_the_table() {
    for flags in [&["-fPIC"][..], &["-fpic"]] {
        let text = asm("lib", flags, THREE);
        assert!(text.contains("\tmovq\taway@GOTPCREL(%rip)"), "{flags:?}: {text}");
        assert!(text.contains("\tmovq\there@GOTPCREL(%rip)"), "{flags:?}: {text}");
        assert!(text.contains("\tmovl\tquiet(%rip)"), "a static is nobody else's: {text}");
    }
}

/// A name nothing outside the library can reach costs the library nothing.
///
/// This is the reason `-fPIC -fvisibility=hidden` is what a library that cares about its own speed
/// is built with, and it is the same rule from both ends: hidden is not in the dynamic symbol
/// table to be looked up, and protected says a reference from inside binds to the definition
/// inside, so neither is a name another object may answer for.
#[test]
fn a_library_pays_nothing_for_a_name_nothing_outside_it_can_see() {
    let text = asm("hidden", &["-fPIC", "-fvisibility=hidden"], THREE);
    assert!(!text.contains("GOTPCREL"), "{text}");

    let marked = asm(
        "marked",
        &["-fPIC"],
        "\
__attribute__((visibility(\"protected\"))) int kept = 1;
int read_kept(void) { return kept; }
",
    );
    assert!(marked.contains("\tmovl\tkept(%rip)"), "{marked}");
}

/// The last one written is the one that counts, which is how every other flag with two directions
/// behaves and is what a build gets when a wrapper script adds one to a line that already had the
/// other.
#[test]
fn the_last_one_on_the_line_is_the_one_that_counts() {
    let library = asm("last-pic", &["-fPIE", "-fPIC"], THREE);
    assert!(library.contains("GOTPCREL"), "{library}");

    let executable = asm("last-pie", &["-fPIC", "-fPIE"], THREE);
    assert!(!executable.contains("GOTPCREL"), "{executable}");
}

/// `__PIE__` says which of the two it is, and `__PIC__` is defined either way.
///
/// Either way because it says there are no absolute addresses in the text, and that has been true
/// here since the predefines were written. A program reads `__PIE__` to find out whether a name it
/// exports is one something else may replace, which is a different question and until #756 had no
/// answer at all: `__PIC__` was 2 and `__PIE__` was defined nowhere, which said the opposite of
/// what the code generator did.
#[test]
fn the_macros_say_which_of_the_two_links_is_coming() {
    let source = "\
#ifndef __PIC__
#error there are no absolute addresses either way
#endif
#ifdef __PIE__
int for_an_executable(void) { return 1; }
#else
int for_a_library(void) { return 1; }
#endif
";
    let executable = asm("macro-exe", &[], source);
    assert!(executable.contains("for_an_executable:"), "{executable}");

    let library = asm("macro-lib", &["-fPIC"], source);
    assert!(library.contains("for_a_library:"), "{library}");
}
