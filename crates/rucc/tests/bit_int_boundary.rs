//! A `_BitInt` narrower than its register at a function boundary on AArch64 and i386. Both leave
//! the bits above the value unspecified, the way the x86-64 psABI does, so the side that receives
//! one reads only its own bits. aarch64-linux-gnu-gcc 14 and gcc 15 `-m32` keep that from both
//! sides, and every pairing of the two compilers as callee and caller gave gcc's answers. Each
//! of these was refused with E0653 before (tamnd/rucc#425). RISC-V extends the value to the whole
//! register, has not been checked against a compiler, and is still refused.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// A callee that divides an unsigned `_BitInt(37)`, and a caller that tests the sign of a
/// `_BitInt(7)` it gets back.
const SOURCE: &str = "\
unsigned long long quot(unsigned _BitInt(37) x) { return x / 3; }
_BitInt(7) seven(void);
int negative(void) { return seven() < 0; }
";

/// Whether rucc compiled `SOURCE` for `target` at `-O2`, the listing, and what it said.
fn assembly(target: &str) -> (bool, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .arg(format!("--target={target}"))
        .args(["-O2", "-fno-asynchronous-unwind-tables", "-x", "c", "-", "-S", "-o", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(SOURCE.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("the compiler finished");
    let listing = String::from_utf8(out.stdout).expect("a listing is text");
    (out.status.success(), listing, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The body of `name` in `listing`, up to its `ret`.
fn body<'a>(listing: &'a str, name: &str) -> &'a str {
    let start = listing.find(&format!("\n{name}:\n")).expect("the function is in the listing");
    let rest = &listing[start..];
    &rest[..rest.find("ret").expect("the function returns")]
}

#[test]
fn aarch64_masks_an_argument_and_extends_a_value_it_gets_back() {
    let (ok, listing, errors) = assembly("aarch64-unknown-linux-gnu");
    assert!(ok, "{errors}");
    let quot = body(&listing, "quot");
    assert!(quot.contains("and x0, x0, #137438953471\n"), "the 37 bits are kept: {quot}");
    let negative = body(&listing, "negative");
    let after = &negative[negative.find("bl seven").expect("the call is made")..];
    assert!(after.contains("lsl w0, w0, #1\n"), "the seven bits are moved up: {negative}");
    assert!(after.contains("asr w0, w0, #1\n"), "and their sign brought down: {negative}");
}

#[test]
fn i386_masks_an_argument_and_extends_a_value_it_gets_back() {
    let (ok, listing, errors) = assembly("i686-unknown-linux-gnu");
    assert!(ok, "{errors}");
    let quot = body(&listing, "quot");
    assert!(quot.contains("andl\t$31, %ecx\n"), "five bits of the high word are kept: {quot}");
    let negative = body(&listing, "negative");
    let after = &negative[negative.find("call\tseven").expect("the call is made")..];
    assert!(after.contains("shlb\t$1, %al\n"), "the seven bits are moved up: {negative}");
    assert!(after.contains("sarb\t$1, %al\n"), "and their sign brought down: {negative}");
}

#[test]
fn riscv64_still_refuses_one() {
    let (ok, _, errors) = assembly("riscv64-unknown-linux-gnu");
    assert!(!ok);
    assert!(errors.contains("parameter 0 is a width no argument register holds"), "{errors}");
}
