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

/// kencc reads the expansion again as text, so an argument and a punctuator of the body with no
/// space between them make one token. games/life.c has `op= 2` with `+` for `op`. The text of
/// `-E` is the same in C, so the test compiles the source.
const JOIN: &str = "#define N(op) x op= 2\n\
                    #define M(a) a+b\n\
                    void f(int x, int b){ N(+); N(<<); x = M(+); }\n";

/// Whether the compiler took `source` with `flags`, and what it said.
fn compiles(what: &str, flags: &[&str], source: &str) -> (bool, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-plan9", "-fsyntax-only", "-w"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn kencc_joins_an_argument_with_the_body() {
    let (ok, err) = compiles("join", &["-fkencc"], JOIN);
    assert!(ok, "{err}");
    let (ok, err) = compiles("nojoin", &[], JOIN);
    assert!(!ok);
    assert!(err.contains("expected an expression, found `=`"), "{err}");
}

/// `-fno-kencc-preprocessor` is kencc `-p`: the ANSI C preprocessor. Of the predefined macros, it
/// has the `__DATE__` and `__TIME__` of `/bin/cpp`, which mp3enc's version.c uses, and no other.
#[test]
fn kencc_p_has_the_operators_of_ansi_c() {
    let (ok, out, err) = run("ansi", &["-fkencc", "-fno-kencc-preprocessor"], SOURCE);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "f(int i, int n){ if(i < n){}else _assert(\"x\"); 'c'; \"\\nn and n\"; \"y\"; pq; }"
    );
    let stamps = "__DATE__ __TIME__ __STDC__ __GNUC__\n";
    let (ok, out, err) = run("stamps", &["-fkencc", "-fno-kencc-preprocessor"], stamps);
    assert!(ok, "{err}");
    assert!(out.starts_with('"') && out.ends_with("\" __STDC__ __GNUC__"), "{out}");
    let (_, out, _) = run("nostamps", &["-fkencc"], stamps);
    assert_eq!(out, stamps.trim_end());
}
