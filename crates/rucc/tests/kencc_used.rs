//! kencc's `USED` and `SET` statements under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4148, and 06.3 of the rucc-plan9 plan.
//!
//! In kencc, `USED(a, b);` and `SET(a);` are statements with keywords of their own. They give
//! each expression its type and emit no code. The 9front tree uses `USED` 971 times and `SET` 138
//! times, and `va_end` in `u.h` expands to `USED`. In C they are identifiers, so without
//! `-fkencc` they stay calls of an undeclared function.

use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "int g;\n\
                      int f(int a, int b, char *fmt, ...)\n\
                      {\n\
                      \tint x, y;\n\
                      \tUSED(a, b);\n\
                      \tUSED();\n\
                      \tSET(x);\n\
                      \tif(a) USED(fmt); else SET(y);\n\
                      #ifdef USED\n\
                      \treturn -1;\n\
                      #endif\n\
                      \tUSED(g++);\n\
                      \treturn g;\n\
                      }\n";

fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-used-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, what it wrote and what it said.
fn run(what: &str, flags: &[&str]) -> (bool, String, String) {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    (out.status.success(), text(out.stdout), text(out.stderr))
}

#[test]
fn used_and_set_are_statements_that_evaluate_nothing() {
    let (ok, out, err) = run("kencc", &["-fkencc", "-O1", "-Wall", "-Wextra"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    // The names in the statements count as used, so there is no warning about them.
    assert!(err.is_empty(), "{err}");
    let start = out.find("\nf:").expect("f is in the listing");
    let body = &out[start..start + out[start..].find("\tret").expect("f returns")];
    // `#ifdef USED` is false, so f returns g, and `USED(g++)` does not change g.
    assert!(!body.contains("$-1"), "{body}");
    assert!(body.contains("movl\tg(%rip), %eax"), "{body}");
    assert!(!body.contains("incl") && !body.contains("addl"), "{body}");
}

#[test]
fn without_kencc_they_are_identifiers() {
    let (ok, _, err) = run("c", &[]);
    assert!(!ok);
    assert!(err.contains("implicit declaration of function 'USED'"), "{err}");
    assert!(err.contains("implicit declaration of function 'SET'"), "{err}");
}
