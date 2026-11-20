//! A function declared with a parameter whose type is not finished yet, end to end.
//!
//! gcc takes the declaration and says nothing until something calls the function, and the kernel's
//! `irq.h` relies on that: it declares `irq_chip_set_parent_state` with an `enum irqchip_irq_state`
//! parameter in files that never see the enumeration finished. So a declaration is let go, and a
//! call is still refused, since the argument has no size to travel at.

use std::path::PathBuf;
use std::process::Command;

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-unfinished-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler wrote a listing for that source, and what it said.
fn compile(what: &str, source: &str) -> (bool, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn a_declaration_nobody_calls_is_taken() {
    let (ok, said) =
        compile("declared", "extern int g(enum st which, int v);\nint f(void) { return 0; }\n");
    assert!(ok, "{said}");
}

#[test]
fn a_call_to_it_is_still_refused() {
    let (ok, said) = compile(
        "called",
        "extern int g(enum st which, int v);\nenum st { A };\nint f(void) { return g(A, 1); }\n",
    );
    assert!(!ok, "a call with an argument of no size was compiled");
    assert!(said.contains("E0519"), "{said}");
}
