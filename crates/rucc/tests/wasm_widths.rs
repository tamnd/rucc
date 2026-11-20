//! The arithmetic of a bit-field wider than 32 bits on wasm32, which is at a width that wasm has
//! no type for. The driver runs the widths pass of `rucc-codegen` before the wasm backend, as the
//! native targets do, and the pass puts each such value into an `i64`. A `_BitInt` of such a width
//! at a function boundary is still refused, because the wasm ABI of a `_BitInt` has not been
//! taught.
//!
//! Design: #2865.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

/// The output of rucc for `source` on stdin, with `-S` to stdout at `-O2` on wasm32-wasip1.
fn assembly(source: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=wasm32-wasip1", "-O2", "-x", "c", "-", "-S", "-o", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(source.as_bytes()).unwrap();
    child.wait_with_output().expect("the compiler finished")
}

#[test]
fn a_bit_field_of_40_bits_is_arithmetic_in_an_i64() {
    // The rotate of `gcc.c-torture/execute/pr34971.c`.
    let out = assembly(
        "struct s { unsigned long long b : 40; } x;\n\
         unsigned long long rot(void) { return (x.b << 8) + (x.b >> 32); }\n",
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    // The spare bits of the 40-bit value are cleared before the shift right reads them.
    assert!(text.contains("i64.const\t1099511627775\n"), "{text}");
    assert!(text.contains("i64.shr_u"), "{text}");
}

#[test]
fn a_bit_int_of_40_bits_at_a_boundary_is_still_refused() {
    let out = assembly("_BitInt(40) f(_BitInt(40) x) { return x + 1; }\n");
    assert!(!out.status.success());
    let error = String::from_utf8(out.stderr).unwrap();
    assert!(error.contains("a value of type i40 is not translated for wasm yet"), "{error}");
}
