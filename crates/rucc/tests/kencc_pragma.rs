//! The pragmas of kencc under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4150, and 06.5 of the rucc-plan9 plan.
//!
//! kencc acts on `pack` and `lib` and ignores every other pragma with no message. `pack on`
//! packs the records after it, and `pack off` stops it. `lib "name"` asks the linker for a
//! library.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-pragma-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, what it wrote and what it said.
fn run(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    (out.status.success(), text(out.stdout), text(out.stderr))
}

/// The packed layout of `bootexec.h`, and the pragmas of the tree that kencc ignores.
const PACKED: &str = "#pragma pack on\n\
                      struct A { char c; int i; };\n\
                      #pragma pack off\n\
                      struct B { char c; int i; };\n\
                      #pragma pack yes\n\
                      struct C { short s; long long v; };\n\
                      #pragma pack\n\
                      struct D { short s; long long v; };\n\
                      #pragma varargck type \"V\" void*\n\
                      #pragma varargck argpos print 1\n\
                      #pragma varargck flag ','\n\
                      #pragma incomplete Intmap\n\
                      #pragma src \"/sys/src/libc\"\n\
                      #pragma profile off\n\
                      #pragma fpround on\n\
                      #pragma interface\n\
                      #pragma warning (disable: 4334)\n\
                      _Static_assert(sizeof(struct A) == 5, \"A is packed\");\n\
                      _Static_assert(sizeof(struct B) == 8, \"B is not packed\");\n\
                      _Static_assert(sizeof(struct C) == 10, \"C is packed\");\n\
                      _Static_assert(sizeof(struct D) == 16, \"D is not packed\");\n";

#[test]
fn pack_acts_and_the_other_pragmas_are_ignored() {
    let flags = ["--target=x86_64-unknown-linux-gnu", "-fsyntax-only", "-Wall", "-fkencc"];
    let (ok, _, err) = run("kencc", &flags, PACKED);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(err.is_empty(), "{err}");
    // In C, `pack on` is malformed and the line is ignored, so `A` is not packed.
    let (ok, _, err) = run("c", &flags[..3], PACKED);
    assert!(!ok);
    assert!(err.contains("missing `(` after `#pragma pack` - ignored"), "{err}");
    assert!(err.contains("A is packed"), "{err}");
}

#[test]
fn lib_asks_the_linker_for_the_library() {
    let source = "#pragma lib \"lib9p.a\"\nint x;\n";
    let flags = ["--target=x86_64-w64-windows-gnu", "-S", "-o", "-", "-fkencc"];
    let (ok, out, err) = run("lib", &flags, source);
    assert!(ok, "{err}");
    assert!(out.contains(".drectve"), "{out}");
    assert!(out.contains("/DEFAULTLIB:lib9p.a"), "{out}");
    let (ok, _, err) = run("bad-lib", &flags, "#pragma lib lib9p\nint x;\n");
    assert!(!ok);
    assert!(err.contains("syntax in `#pragma lib`"), "{err}");
}
