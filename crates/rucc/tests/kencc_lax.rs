//! The declarations that kencc takes and C does not, under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4148, and 06.7 of the rucc-plan9 plan.
//!
//! - Implicit `int`, at file scope and in an old-style definition, as in `flambe.c` and `mk`.
//! - `enum E` and `int` are one type, as with `Drawop` in `draw.h`.
//! - Two declarations that differ only in qualifiers declare one name, as `perror` in APE's
//!   `stdio.h` and in `libc.h`.

use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "enum E { A, B };\n\
                      int takes(enum E);\n\
                      int takes(int x) { return x; }\n\
                      void perror(char*);\n\
                      void perror(const char*);\n\
                      extern int arr[];\n\
                      int arr[4];\n\
                      static count;\n\
                      main(argc, argv) char **argv;\n\
                      {\n\
                      \treturn takes(argc) + arr[0] + count + _Generic(A, int: 0, default: 1);\n\
                      }\n";

fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-lax-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source and what it said.
fn run(what: &str, flags: &[&str]) -> (bool, String) {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fsyntax-only"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn kencc_takes_the_declarations() {
    let (ok, err) = run("kencc", &["-fkencc", "-w"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(err.is_empty(), "{err}");
    // Implicit `int` is still said without `-w`, as a warning.
    let (ok, err) = run("warned", &["-fkencc"]);
    assert!(ok, "{err}");
    assert!(err.contains("warning: type defaults to 'int' in declaration of 'count'"), "{err}");
}

#[test]
fn c_refuses_them() {
    let (ok, err) = run("c", &[]);
    assert!(!ok);
    assert!(err.contains("error: conflicting types for 'takes'; have 'int(int)'"), "{err}");
    assert!(
        err.contains("error: conflicting types for 'perror'; have 'void(const char *)'"),
        "{err}"
    );
    assert!(err.contains("error: type defaults to 'int' in declaration of 'count'"), "{err}");
}
