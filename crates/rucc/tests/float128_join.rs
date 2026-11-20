//! A conversion of a conditional of two constants to `__float128` on x86-64. gcc folds the
//! conversion into the two arms, so each arm is a `__float128` constant and nothing is converted
//! at run time. Before, rucc kept the two `long double` constants, joined them, and called
//! `__extendxftf2` on the result. libgfortran's `sind_r16` does this, and it was the one symbol
//! the libgfortran built by rucc needed that the one built by gcc did not (tamnd/rucc-bsd#5).

use std::io::Write as _;
use std::process::{Command, Stdio};

/// What libgfortran's `trigd.inc` does when it sets the answer to a half: `mpfr_set_ld` is an
/// assignment, and the answer is a `__float128`.
const SOURCE: &str = "\
__float128 half(int n) { __float128 x; x = (n - 180 < 0 ? 0.5L : -0.5L); return x; }
__float128 scaled(int c, __float128 y) { return y * (c ? 0.5L : -0.5L); }
";

/// The listing rucc makes of `SOURCE` at this level for x86-64, and what it said.
fn assembly(level: &str) -> (bool, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .arg("--target=x86_64-unknown-linux-gnu")
        .args([level, "-fno-asynchronous-unwind-tables", "-x", "c", "-", "-S", "-o", "-"])
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

#[test]
fn a_conditional_of_two_constants_is_converted_when_it_is_compiled() {
    let (ok, listing, errors) = assembly("-O2");
    assert!(ok, "{errors}");
    assert!(!listing.contains("__extendxftf2"), "a conversion is left: {listing}");
    assert!(listing.contains("__multf3"), "the multiply is still made: {listing}");
}
