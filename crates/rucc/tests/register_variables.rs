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
fn an_asm_operand_kept_in_a_named_register_is_refused() {
    // gcc puts such an operand in the register the declaration named whatever the constraint would
    // otherwise have allowed, and a program that writes one is counting on that: tcc's
    // `tests/tcctest.c` declares one in `%eax` and hands it to a template that reads `%eax` by
    // name. Here the operand would go wherever the allocator put the variable, so it is turned
    // down rather than assembled into a program that reads something else.
    let source = "long f(void) { register long x asm (\"rbx\") = 1; asm (\"\" : : \"r\" (x)); \
                  return x; }\n";
    let (ok, _, said) = run("operand", source);
    assert!(!ok, "an operand in a named register was accepted");
    assert!(said.contains("named register"), "{said}");
}
