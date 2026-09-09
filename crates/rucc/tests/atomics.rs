//! What an access to an `_Atomic` object reaches the assembler as, end to end.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.9.
//!
//! The golden case in `tests/golden/atomics.c` holds the IR the walk produces, which is where the
//! decision between one instruction and a loop is made. What is left is the other end: whether the
//! machine the IR is selected for has the instruction the walk assumed, and whether the ordering
//! the IR asked for is the ordering the assembly asks the processor for. Neither is visible from
//! the IR, so this runs the compiler over C and reads the assembly it writes.
//!
//! The refusals are here as well, because the point of one is what a user is told, and what a user
//! is told is only visible from outside the crate that decides it.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the instructions this
/// compares are the same instructions on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-atomics-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler wrote for that source, and whether it agreed to write anything.
fn compile(what: &str, source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
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
fn asm(what: &str, source: &str) -> String {
    let (ok, wrote, said) = compile(what, source);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    wrote
}

/// What the compiler said about source it refuses.
fn refusal(what: &str, source: &str) -> String {
    let (ok, _, said) = compile(what, source);
    assert!(!ok, "the compiler accepted a fixture it has no instruction for:\n{said}");
    said
}

/// A store of a whole word is already atomic on this machine and the ordering is what costs an
/// instruction: nothing after the store may be seen before it, and only a fence says so. A load
/// needs nothing, because this machine does not reorder a load ahead of the loads before it.
#[test]
fn a_plain_write_is_a_store_and_a_fence_and_a_plain_read_is_a_load() {
    let text = asm(
        "plain",
        "\
_Atomic int g;
void put(int v) { g = v; }
int get(void) { return g; }
",
    );
    let put = text.split("put:").nth(1).expect("the writer is in the listing");
    let put = put.split("get:").next().expect("the two functions are apart");
    assert!(put.contains("movl\t%edi"), "{put}");
    assert!(put.contains("mfence"), "{put}");
    let get = text.split("\nget:").nth(1).expect("the reader is in the listing");
    assert!(!get.contains("mfence"), "{get}");
    assert!(!get.contains("lock"), "{get}");
}

/// The five operators the machine has an instruction for come out as that instruction, at the
/// width the object has rather than the width the arithmetic was written at.
#[test]
fn an_operator_the_machine_has_is_one_locked_instruction() {
    let text = asm(
        "rmw",
        "\
_Atomic int g;
_Atomic char c;
void add(int v) { g += v; }
void mask(int v) { g &= v; }
void narrow(int v) { c += v; }
",
    );
    assert!(text.contains("xaddl"), "{text}");
    assert!(text.contains("xaddb"), "{text}");
    assert!(text.contains("lock"), "{text}");
}

/// One it has no instruction for is the loop, which is the only other thing it can be: read the
/// object, work out the answer, and put it back if nothing else got there first.
#[test]
fn an_operator_the_machine_does_not_have_is_a_compare_and_exchange_loop() {
    let text = asm(
        "loop",
        "\
_Atomic int g;
void multiply(int v) { g *= v; }
",
    );
    assert!(text.contains("cmpxchgl"), "{text}");
    assert!(text.contains("lock"), "{text}");
}

/// A float has no locked arithmetic anywhere, so it is the loop as well, with the object
/// exchanged as the integer of its width and the addition done on the value that integer holds.
#[test]
fn a_float_is_exchanged_as_the_integer_of_its_width() {
    let text = asm(
        "float",
        "\
_Atomic float f;
void grow(float v) { f += v; }
",
    );
    assert!(text.contains("addss"), "{text}");
    assert!(text.contains("cmpxchgl"), "{text}");
}

/// A step of an atomic pointer is a number of elements, and the instruction adds a number of
/// bytes, so the scaling happens before the machine ever sees it.
#[test]
fn a_step_of_an_atomic_pointer_is_scaled_before_the_instruction() {
    let text = asm(
        "pointer",
        "\
int room[8];
int * _Atomic p;
void walk(void) { p = room; p += 3; }
",
    );
    assert!(text.contains("xaddq"), "{text}");
    assert!(text.contains(",4)"), "{text}");
}

/// An object that is not one value the machine reaches in one instruction is refused where it is
/// written. gcc calls libatomic for one of these, which takes a lock out of a table keyed by the
/// address, and a program half of whose accesses take that lock is not atomic at all.
#[test]
fn a_type_the_machine_cannot_reach_in_one_instruction_is_refused() {
    let said = refusal(
        "wide",
        "\
struct S { int a, b, c; };
_Atomic struct S big;
",
    );
    assert!(said.contains("no instruction for"), "{said}");
    assert!(said.contains("E0519"), "{said}");

    let said = refusal("long-double", "_Atomic long double ld;\n");
    assert!(said.contains("E0519"), "{said}");
}

/// A bit-field cannot carry the qualifier, because 6.7.2.1p5 says what a bit-field's type may be
/// and an atomic type is not one of them. gcc refuses the same declaration.
#[test]
fn a_bit_field_cannot_be_atomic() {
    let said = refusal("bit-field", "struct B { _Atomic int m : 3; };\nstruct B b;\n");
    assert!(said.contains("has atomic type"), "{said}");
    assert!(said.contains("E0700"), "{said}");
}

/// The shipped header, which is the other half of this: the names C11 gives these operations are
/// macros over the `__atomic_` builtins, and a program that includes it gets the same instruction
/// a program that wrote the operator gets.
#[test]
fn the_shipped_header_reaches_the_same_instructions() {
    let text = asm(
        "header",
        "\
#include <stdatomic.h>
int add(atomic_int *p) { return atomic_fetch_add(p, 2); }
void fence(void) { atomic_thread_fence(memory_order_seq_cst); }
int flag(atomic_flag *f) { return atomic_flag_test_and_set(f); }
",
    );
    assert!(text.contains("xaddl"), "{text}");
    assert!(text.contains("mfence"), "{text}");
    assert!(text.contains("lock"), "{text}");
}
