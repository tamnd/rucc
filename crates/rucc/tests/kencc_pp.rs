//! The preprocessor of kencc under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4149, and 03.10 and 06.4 of the rucc-plan9 plan.
//!
//! kencc has its own preprocessor in `cc/macbody`. A parameter is replaced in a string literal
//! or a character constant of the body, so `assert` in `libc.h` gives the condition as text.
//! `#` and `##` are not operators. `#include` takes no macro. No macro is predefined.

use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "#define assert(x) if(x){}else _assert(\"x\")\n\
                      #define CH(c) 'c'\n\
                      #define ESC(n) \"\\nn and n\"\n\
                      #define S(x) #x\n\
                      #define P(a,b) a##b\n\
                      #ifdef __GNUC__\n\
                      gnuc\n\
                      #endif\n\
                      f(int i, int n){ assert(i < n); CH(q); ESC(z); S(y); P(p,q); }\n";

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-pp-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the preprocessor took the source, the lines it wrote that are not line markers, and
/// what it said.
fn run(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-plan9", "-E", "-P"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    let lines = text(out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    (out.status.success(), lines, text(out.stderr))
}

#[test]
fn kencc_replaces_in_literals_and_has_no_operators() {
    let (ok, out, err) = run("kencc", &["-fkencc"], SOURCE);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "f(int i, int n){ if(i < n){}else _assert(\"i < n\"); 'q'; \"\\nz and z\"; #y; p##q; }"
    );
}

#[test]
fn c_keeps_the_standard_rules() {
    let (ok, out, err) = run("c", &[], SOURCE);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "gnuc\nf(int i, int n){ if(i < n){}else _assert(\"x\"); 'c'; \"\\nn and n\"; \"y\"; pq; }"
    );
}

#[test]
fn kencc_defines_no_macro_and_takes_no_macro_in_an_include() {
    let (ok, out, _) = run("none", &["-fkencc", "-dM"], "");
    assert!(ok);
    assert_eq!(out, "");
    let (ok, _, err) = run("include", &["-fkencc"], "#define H \"u.h\"\n#include H\n");
    assert!(!ok);
    assert!(err.contains("error: expected a file name in `<>` or `\"\"`"), "{err}");
    // A command line macro is still defined.
    let (ok, out, _) = run("cmdline", &["-fkencc", "-DX=1"], "X\n");
    assert!(ok);
    assert_eq!(out, "1");
}
