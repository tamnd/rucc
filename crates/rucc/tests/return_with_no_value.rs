//! `return;` in a function that gives a value, which C89 allowed. The value is zero of the
//! return type, and for a pointer that is a null pointer.
//!
//! The `compile` of the regexp code that yacc puts in APE's expr does `return;` in a function
//! that gives a `char *`, and the compiler stopped with a panic there.

use std::process::Command;

fn compile(what: &str, source: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("rucc-no-value-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-std=c89", "-O2", "-w", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn a_pointer_function_with_an_empty_return_compiles() {
    let source = "char *f(int x) { if (x) return; return \"a\"; }\n";
    let (ok, err) = compile("pointer", source);
    assert!(ok, "{err}");
}

#[test]
fn an_integer_and_a_floating_function_with_an_empty_return_compile() {
    let source = "int f(int x) { if (x) return; return 2; }\n\
                  double g(int x) { if (x) return; return 2.5; }\n";
    let (ok, err) = compile("scalar", source);
    assert!(ok, "{err}");
}
