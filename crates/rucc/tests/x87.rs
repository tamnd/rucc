//! What the x87 arithmetic encodes to, which is the only place the answer is visible.
//!
//! Design: `spec/10-backend.md` section 10.2, and tamnd/rucc#585 for why this file exists.
//!
//! A subtraction and a division on the x87 stack come in two directions, and which mnemonic names
//! which is a fact about the assembly dialect rather than about the machine. Intel's `FSUBP
//! ST(i), ST(0)` computes `ST(i) - ST(0)` and is `DE E8+i`, and the AT&T `fsubp` is `DE E0+i`,
//! which is the other subtraction. This compiler writes AT&T and encodes what gas encodes, so it
//! asks for `fsubrp` to get the one it wants, and it asked for `fsubp` for a while and every
//! `long double` subtraction in every program came out backwards.
//!
//! Reading the mnemonic back is what the unit test in `rucc-codegen` does, and it is not enough on
//! its own: a name is what got this wrong, so a test that repeats the name agrees with the bug.
//! This reads the bytes of the object instead, which is what the processor is going to read. The
//! host cannot run an x86-64 program, so the bytes are as close to the answer as a test here gets.

use std::process::Command;

/// The target is written down rather than taken from the host, so that the bytes this compares are
/// the same bytes on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The object the compiler makes of that source.
fn object(what: &str, source: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("rucc-x87-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    let out = dir.join("one.o");
    std::fs::write(&path, source).expect("the fixture can be written");
    let ran = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .arg("-c")
        .arg("-o")
        .arg(&out)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(
        ran.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let bytes = std::fs::read(&out).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

/// Whether that pair of bytes is anywhere in the object.
///
/// Crude on purpose. The fixture is one arithmetic instruction in one function, so the object is
/// small enough that a two byte sequence in it is the instruction rather than a coincidence, and
/// each test below asserts the presence of one pair and the absence of the other, which no
/// coincidence satisfies twice.
fn holds(bytes: &[u8], pair: [u8; 2]) -> bool {
    bytes.windows(2).any(|window| window == pair)
}

#[test]
fn a_long_double_subtraction_is_the_one_below_minus_the_top() {
    // `DE E9` is `FSUBP ST(1), ST(0)` in Intel's spelling, which is `ST(1) - ST(0)`, and the left
    // operand is the one pushed first so it is `ST(1)`. `DE E1` is the other direction and is what
    // gas calls `fsubp`.
    let bytes = object("sub", "long double f(long double a, long double b) { return a - b; }\n");
    assert!(holds(&bytes, [0xDE, 0xE9]), "the subtraction is not the one that was wanted");
    assert!(!holds(&bytes, [0xDE, 0xE1]), "the subtraction came out the other way round");
}

#[test]
fn a_long_double_division_is_the_one_below_over_the_top() {
    let bytes = object("div", "long double f(long double a, long double b) { return a / b; }\n");
    assert!(holds(&bytes, [0xDE, 0xF9]), "the division is not the one that was wanted");
    assert!(!holds(&bytes, [0xDE, 0xF1]), "the division came out the other way round");
}

#[test]
fn an_addition_and_a_multiplication_have_one_direction_each() {
    // Here for the contrast rather than for the coverage. These two are the reason the bug lived:
    // they do not care which operand is which, so half the arithmetic looked right.
    let sum = object("add", "long double f(long double a, long double b) { return a + b; }\n");
    assert!(holds(&sum, [0xDE, 0xC1]), "the addition is not `faddp`");
    let product = object("mul", "long double f(long double a, long double b) { return a * b; }\n");
    assert!(holds(&product, [0xDE, 0xC9]), "the multiplication is not `fmulp`");
}
