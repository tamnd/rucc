//! A function with no prototype on wasm32, which is not variadic there as it is everywhere else.
//!
//! A wasm function has one type, and a call must give exactly that type, so clang makes the type
//! of `int f()` the type of its definition and passes each promoted argument of a call as a fixed
//! parameter. The facts come from clang 23 from wasi-sdk 34. `int main()` is the case that
//! matters most: wasi-libc calls `__main_void` as `() -> i32`, and a `main` that takes the buffer
//! of a variadic function does not link to it.
//!
//! Design: #2864.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The IR that rucc prints for `source` under gnu11, where `()` is not a prototype.
fn ir(target: &str, source: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .arg(format!("--target={target}"))
        .args(["-std=gnu11", "-O0", "--emit=ir", "-o", "-", "-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(source.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("the compiler finished");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

const SOURCE: &str = "\
int ext();
int g(a, d) char a; double d; { return a + (int)d; }
int main() { float f = 2; return ext('c', f, 5LL) + ext() + g(1, 2.0); }
";

#[test]
fn a_function_with_no_prototype_has_the_type_of_its_definition_on_wasm() {
    let ir = ir("wasm32-wasip1", SOURCE);
    for line in [
        "func @ext() -> i32, linkage(external);",
        "func @g(i32, f64) -> i32, linkage(external) {",
        "func @main() -> i32, linkage(external) {",
        ": (i32, f64, i64) -> i32",
        "call @ext() : () -> i32",
        "call @g(",
    ] {
        assert!(ir.contains(line), "no {line:?} in\n{ir}");
    }
    assert!(!ir.contains("..."), "{ir}");
}

#[test]
fn a_function_with_no_prototype_stays_variadic_on_the_other_rows() {
    let ir = ir("x86_64-linux-gnu", SOURCE);
    assert!(ir.contains("func @main(...) -> i32"), "{ir}");
    assert!(ir.contains("func @ext(...) -> i32"), "{ir}");
}
