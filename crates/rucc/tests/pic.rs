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
///
/// Which instruction names the address is left out of what is asked here, because that is not what
/// this is about and it does change: `rucc_codegen::combine` puts the load of a name into the
/// arithmetic that reads it, so two of the three below are named by an `addl` rather than a `movl`.
/// What the question is about is the `(%rip)` and the absence of a table.
#[test]
fn an_executable_works_every_address_out_for_itself() {
    for flags in [&[][..], &["-fPIE"], &["-fpie"]] {
        let text = asm("exe", flags, THREE);
        assert!(text.contains("away(%rip)"), "{flags:?}: {text}");
        assert!(text.contains("here(%rip)"), "{flags:?}: {text}");
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
        assert!(text.contains("quiet(%rip)"), "a static is nobody else's: {text}");
        assert!(!text.contains("quiet@GOTPCREL"), "a static is nobody else's: {text}");
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
    assert!(text.contains("here(%rip)"), "a name this file defines is here: {text}");
    assert!(text.contains("quiet(%rip)"), "a static is nobody else's: {text}");
    assert!(!text.contains("here@GOTPCREL"), "{text}");
    assert!(!text.contains("quiet@GOTPCREL"), "{text}");

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

/// And it still pays for a name it only mentions, which is the whole of tamnd/rucc#1234.
///
/// `-fvisibility=hidden` is a claim about the names this file puts into the library. A name it
/// declares and does not define is one it knows nothing about, and calling that hidden tells the
/// linker to resolve it inside this object, which it cannot do: libexpat reads `stderr`, `stderr`
/// is in libc, and the link stopped with `R_X86_64_PC32 against symbol stderr@@GLIBC_2.2.5 can not
/// be used when making a shared object`, advising a `-fPIC` that was already on the line.
///
/// Measured against gcc 16.2.0, which writes the same two: the plain declaration goes through the
/// table and the marked one is reached from the instruction pointer.
#[test]
fn a_library_still_pays_for_a_name_it_only_declares() {
    let source = "\
extern int plain;
__attribute__((visibility(\"hidden\"))) extern int marked;
int read_both(void) { return plain + marked; }
";
    let text = asm("declared", &["-fPIC", "-fvisibility=hidden"], source);
    assert!(text.contains("\tmovq\tplain@GOTPCREL(%rip)"), "{text}");
    assert!(text.contains("marked(%rip)"), "{text}");
    assert!(!text.contains("marked@GOTPCREL"), "an attribute on a declaration counts: {text}");
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

/// A function named behind a comma is called by name, and its address never comes out of the
/// table.
///
/// tcc writes `(tcc_enter_state(s1), _tcc_warning)(fmt, ...)` and gcc calls `_tcc_warning`
/// there, even at O0. Loading the address through the GOT is not wrong, but tcc's own linker
/// fills that slot wrongly for a function the shared library defines, and its dlltest crashed.
/// At O2 the call is in tail position and becomes a jump, which goes by name the same way.
#[test]
fn a_function_named_behind_a_comma_is_called_by_name() {
    let source = "\
void enter(void);
void warn(const char *fmt, ...);
void tell(const char *what) { (enter(), warn)(\"%s\", what); }
";
    for level in ["-O0", "-O2"] {
        let text = asm("comma", &["-fPIC", level], source);
        assert!(text.contains("call\tenter"), "{level}: {text}");
        let reached = if level == "-O2" { "\tjmp\twarn" } else { "\tcall\twarn" };
        assert!(text.contains(reached), "{level}: {text}");
        assert!(!text.contains("warn@GOTPCREL"), "{level}: {text}");
    }
}

/// Every name there is a way to reach, for the position dependent tests below: a variable defined
/// here, one defined elsewhere, a weak one nothing may define, a function this file only declares,
/// a weak one, and the address of each.
const EVERY: &str = "\
extern int away;
int here = 1;
extern int maybe __attribute__((weak));
extern void act(void);
extern void perhaps(void) __attribute__((weak));
int read_all(void) { return away + here; }
int *address_of_maybe(void) { return &maybe; }
int read_maybe(void) { return maybe; }
void (*address_of_act(void))(void) { return act; }
void call_perhaps(void) { if (perhaps) perhaps(); act(); }
";

/// `-fno-pic` and `-fno-pie` in every spelling gcc takes reach every name directly, and nothing at
/// all goes through the global offset table, the weak ones included.
///
/// The weak ones are the point. An executable reads a weak variable out of the table, because
/// in a link that is position independent nothing else can say zero for one nobody defined. A
/// link that is not can, `ld` resolves a direct reference to an undefined weak name to zero, and
/// a kernel's link script asserts there is no `.got` at all, so one slot for a `__start_` symbol
/// is a kernel that does not link. tamnd/rucc#2276.
#[test]
fn position_dependent_code_reaches_every_name_directly() {
    for flags in
        [&["-fno-pic"][..], &["-fno-PIC"], &["-fno-pie"], &["-fno-PIE"], &["-fPIE", "-fno-pie"]]
    {
        let text = asm("absolute", flags, EVERY);
        assert!(!text.contains("GOTPCREL"), "{flags:?}: {text}");
        assert!(!text.contains("@PLT"), "{flags:?}: {text}");
        assert!(text.contains("maybe(%rip)"), "{flags:?}: {text}");
        assert!(text.contains("act(%rip)"), "{flags:?}: {text}");
        assert!(text.contains("perhaps(%rip)"), "{flags:?}: {text}");
        assert!(text.contains("call\tperhaps"), "{flags:?}: {text}");
    }
    // What gets the table without the flag, so that the test above is known to be asking about
    // something the flag changes.
    let executable = asm("absolute-exe", &[], EVERY);
    assert!(executable.contains("maybe@GOTPCREL(%rip)"), "{executable}");
}

/// A constant holding an address goes in `.rodata` under `-fno-pic`, as gcc puts it, and in
/// `.data.rel.ro` otherwise. Without the loader moving the file there is no write for the relro
/// segment to protect, and the kernel's section checks compare against gcc's names.
#[test]
fn a_constant_holding_an_address_is_read_only_data_without_pic() {
    let source = "extern int x;\nconst int *const p[] = { &x };\n";
    let absolute = asm("rodata-absolute", &["-fno-pic"], source);
    assert!(!absolute.contains(".data.rel.ro"), "{absolute}");
    assert!(absolute.contains("\t.section\t.rodata"), "{absolute}");
    let executable = asm("rodata-pie", &["-fPIE"], source);
    assert!(executable.contains(".data.rel.ro"), "{executable}");
}

/// A no speaks only for its own family, the way gcc reads the two, so a library asked for is still
/// a library after `-fno-pie`, and an executable asked for is still one after `-fno-pic`.
#[test]
fn a_no_for_one_family_leaves_the_other_alone() {
    let library = asm("absolute-lib", &["-fPIC", "-fno-pie"], EVERY);
    assert!(library.contains("away@GOTPCREL(%rip)"), "{library}");
    let executable = asm("absolute-pie", &["-fPIE", "-fno-pic"], EVERY);
    assert!(executable.contains("maybe@GOTPCREL(%rip)"), "{executable}");
}

/// Neither `__PIC__` nor `__PIE__` under `-fno-pic`, which is what gcc does, since there the claim
/// both of them make stops being true.
#[test]
fn position_dependent_code_defines_neither_macro() {
    let source = "\
#if defined __PIC__ || defined __pic__ || defined __PIE__ || defined __pie__
int position_independent(void) { return 1; }
#else
int position_dependent(void) { return 1; }
#endif
";
    for flag in ["-fno-pic", "-fno-pie", "-fno-PIE"] {
        let text = asm("absolute-macro", &[flag], source);
        assert!(text.contains("position_dependent:"), "{flag}: {text}");
    }
}
