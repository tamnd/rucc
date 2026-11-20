//! The record layout of kencc on the Plan 9 targets, end to end.
//!
//! Design: tamnd/rucc#4154, and 08.1 of the rucc-plan9 plan.
//!
//! An object from rucc must link with objects from `6c` and `7c`, so each record must have the
//! size and the offsets that kencc gives it. Every record rounds to 8 bytes, a member that is a
//! record goes at a multiple of 8, and `#pragma pack on` packs to 1. The unit tests in
//! `rucc-types` cover the bit-fields.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-layout-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, and what it said.
fn run(what: &str, flags: &[&str], source: &str) -> (bool, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

const LAYOUT: &str = "#define offsetof(t, m) __builtin_offsetof(t, m)\n\
                      struct One { char c; };\n\
                      struct Two { char c; struct One t; };\n\
                      struct Mixed { char c; short s; int i; char d; };\n\
                      struct Bits { int a:3; int b:5; int c:30; char d; int e:4; };\n\
                      union U { char c; short s; };\n\
                      struct Ptr { char c; void *p; long l; long long v; };\n\
                      #pragma pack on\n\
                      struct Packed { char c; int i; char d; };\n\
                      #pragma pack off\n\
                      _Static_assert(sizeof(struct One) == 8, \"One\");\n\
                      _Static_assert(_Alignof(struct One) == 8, \"One is aligned\");\n\
                      _Static_assert(offsetof(struct Two, t) == 8, \"t\");\n\
                      _Static_assert(sizeof(struct Two) == 16, \"Two\");\n\
                      _Static_assert(offsetof(struct Mixed, d) == 8, \"d\");\n\
                      _Static_assert(sizeof(struct Mixed) == 16, \"Mixed\");\n\
                      _Static_assert(offsetof(struct Bits, d) == 8, \"d after the units\");\n\
                      _Static_assert(sizeof(struct Bits) == 16, \"Bits\");\n\
                      _Static_assert(sizeof(union U) == 8, \"U\");\n\
                      _Static_assert(offsetof(struct Ptr, l) == 16, \"l\");\n\
                      _Static_assert(sizeof(struct Ptr) == 32, \"Ptr\");\n\
                      _Static_assert(offsetof(struct Packed, d) == 5, \"packed d\");\n\
                      _Static_assert(sizeof(struct Packed) == 6, \"Packed\");\n";

#[test]
fn records_have_the_layout_of_kencc_on_amd64_and_arm64() {
    for target in ["--target=x86_64-plan9", "--target=aarch64-plan9"] {
        let (ok, err) = run("plan9", &[target, "-fkencc", "-fsyntax-only"], LAYOUT);
        assert!(ok, "{target}: the compiler refused the source:\n{err}");
    }
}

#[test]
fn other_targets_keep_their_own_layout() {
    let source = "struct One { char c; };\n_Static_assert(sizeof(struct One) == 1, \"One\");\n";
    let (ok, err) =
        run("linux", &["--target=x86_64-unknown-linux-gnu", "-fkencc", "-fsyntax-only"], source);
    assert!(ok, "the compiler refused the source:\n{err}");
}
