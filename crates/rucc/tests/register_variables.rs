//! Local register variables end to end.
//!
//! `register long x asm ("rbx");` on an object of automatic storage is GNU C's other reading of
//! the string after `asm`. On something with a symbol the string renames the symbol, and a local
//! has no symbol at all, so the string names a machine register and the object starts out holding
//! whatever that register holds where the declaration stands.
//!
//! That is a thing garbage collectors written in C need and nothing else does. A root that lives
//! only in a callee saved register is a root no walk of the stack finds, so micropython's
//! `gc_helper_get_regs` in `shared/runtime/gchelper_generic.c` declares six of these and copies
//! them into a buffer it can walk. A compiler that ignores the specifier stores six uninitialised
//! slots and the collector misses six roots, which is why this is honoured rather than warned
//! about.
//!
//! The unit tests in `rucc-sema` cover reading the declaration and the ones in `rucc-codegen`
//! cover what the instruction lowers to. What is left is the trip itself, which is only visible
//! from the outside, so this runs the compiler over C and reads the listing.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the listing this compares
/// is the same listing on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-register-vars-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler says about that source, and whether it finished.
fn run(what: &str, source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), text, said)
}

/// The assembly the compiler writes for that source.
fn asm(what: &str, source: &str) -> String {
    let (ok, text, said) = run(what, source);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    text
}

#[test]
fn a_local_in_a_named_register_reads_that_register() {
    let source = "long f(void) { register long x asm (\"rbx\"); return x; }\n";
    let text = asm("one", source);
    assert!(text.contains("movq\t%rbx,"), "the register was never read:\n{text}");
}

#[test]
fn the_six_micropython_reads_are_the_six_registers() {
    // `gc_helper_get_regs` itself, with the buffer it writes into passed in. Every one of the six
    // has to be the register the declaration named, because the whole point of the buffer is that
    // those exact registers are in it.
    let source = "\
void f(unsigned long *a) {
    register long rbx asm (\"rbx\");
    register long rbp asm (\"rbp\");
    register long r12 asm (\"r12\");
    register long r13 asm (\"r13\");
    register long r14 asm (\"r14\");
    register long r15 asm (\"r15\");
    a[0] = rbx; a[1] = rbp; a[2] = r12; a[3] = r13; a[4] = r14; a[5] = r15;
}
";
    let text = asm("six", source);
    for register in ["%rbx", "%rbp", "%r12", "%r13", "%r14", "%r15"] {
        assert!(text.contains(&format!("movq\t{register},")), "{register} is not read:\n{text}");
    }
}

#[test]
fn an_initializer_is_what_the_object_starts_with_rather_than_the_register() {
    // The declaration says both things at once and the initializer is the later of the two, so it
    // is the one that stands. What that leaves is a function returning the constant, and the read
    // of the register is dead and goes.
    let source = "long f(void) { register long x asm (\"r12\") = 7; return x; }\n";
    let text = asm("seeded", source);
    assert!(text.contains("$7,"), "the initializer is not what came back:\n{text}");
}

#[test]
fn a_name_this_machine_has_not_got_is_refused() {
    let source = "long f(void) { register long x asm (\"nowhere\"); return x; }\n";
    let (ok, _, said) = run("nameless", source);
    assert!(!ok, "a register that does not exist was accepted");
    assert!(said.contains("not a register this machine has"), "{said}");
}

#[test]
fn an_asm_operand_kept_in_a_named_register_goes_in_that_register() {
    // The one use of these the GNU manual calls reliable, and the only way to name a register the
    // constraint letters have no letter for. The template reads `%r12` by name and says nothing
    // about where the operand is, so the declaration is the only thing that can put the two in the
    // same place.
    let source = "long f(void) { register long x asm (\"r12\"); \
                  asm volatile (\"mov $0x4542, %%r12\" : \"=r\" (x)); return x; }\n";
    let text = asm("operand", source);
    assert!(text.contains("%r12"), "the operand did not land in the register named:\n{text}");
    assert!(
        !text.contains("movq\t%rax, %rax"),
        "the operand went somewhere else and was copied back:\n{text}"
    );
}

#[test]
fn an_input_in_a_named_register_is_the_register_the_template_reads() {
    // The other half of the same thing. tcc's `tests/tcctest.c` hands one of these to a template
    // that names the register outright, so the value has to be in `%r12` before the template runs.
    let source = "long f(long a) { register long x asm (\"r12\") = a; long y; \
                  asm volatile (\"mov %%r12, %0\" : \"=r\" (y) : \"r\" (x)); return y; }\n";
    let text = asm("input", source);
    assert!(text.contains("%r12"), "the input never reached the register named:\n{text}");
}

#[test]
fn an_operand_wanted_in_memory_and_in_a_named_register_is_refused() {
    // A register is not an address, so there is nowhere for this one to go. Refused rather than
    // quietly given one of the two, because either choice is a program that does not do what it
    // says.
    let source = "long f(void) { register long x asm (\"r12\") = 1; asm (\"\" : : \"m\" (x)); \
                  return x; }\n";
    let (ok, _, said) = run("both", source);
    assert!(!ok, "an operand in memory and in a register at once was accepted");
    assert!(said.contains("in memory and in a named register"), "{said}");
}

#[test]
fn a_register_named_at_file_scope_is_refused_by_name() {
    // The other extension under the same syntax, which takes the register away from every function
    // in the file. Read as an ordinary global it would compile and then disagree with whatever
    // assembly expected the value in the register, so it is refused and the message says what it
    // is rather than that no register was named.
    let source = "register long gr asm (\"r12\");\nlong f(void) { return gr; }\n";
    let (ok, _, said) = run("global", source);
    assert!(!ok, "a global register variable was compiled");
    assert!(said.contains("'gr' is a global register variable"), "{said}");
}
