//! `__attribute__((warn_unused))`, end to end: taken without a word on a structure, a union or an
//! enumeration, and on a pointer type, there to ask about, and what gcc says about it anywhere
//! else, in its words. Measured against gcc 16.2.0.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-warn-unused-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, what it wrote on its standard output and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Whether one source compiled for x86-64 Linux, and what was said.
fn compile(what: &str, source: &str) -> (bool, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let args = ["--target=x86_64-unknown-linux-gnu", "-Wall", "-Wextra", "-S", "-o", "-", "a.c"];
    let (ok, _, err) = run(&dir, &args);
    let _ = std::fs::remove_dir_all(&dir);
    (ok, err)
}

#[test]
fn a_type_marked_warn_unused_is_taken_without_a_word() {
    let source = "struct __attribute__((warn_unused)) a { int a; };\n\
        struct b { int b; } __attribute__((warn_unused));\n\
        union __attribute__((warn_unused)) u { int u; };\n\
        enum __attribute__((warn_unused)) e { E0 };\n\
        struct __attribute__((__warn_unused__)) f { int f; };\n\
        struct [[gnu::warn_unused]] g { int g; };\n\
        struct c { int c; } __attribute__((warn_unused)) c;\n\
        int *__attribute__((warn_unused)) p;\n\
        _Static_assert(__has_attribute(warn_unused) == 1, \"known\");\n\
        _Static_assert(__builtin_has_attribute(struct a, warn_unused), \"kept\");\n\
        _Static_assert(__builtin_has_attribute(c, warn_unused), \"kept on the type\");\n\
        _Static_assert(!__builtin_has_attribute(struct f, unused), \"nothing else\");\n\
        int use(void) { struct a a = {1}; return a.a + (int)sizeof(struct b); }\n";
    let (ok, err) = compile("types", source);
    assert!(ok, "{err}");
    assert_eq!(err, "");
}

#[test]
fn warn_unused_anywhere_but_a_type_is_ignored_in_gcc_s_words() {
    let ignored = "warning: 'warn_unused' attribute ignored";
    for source in [
        "typedef int __attribute__((warn_unused)) t;\n",
        "typedef int t __attribute__((warn_unused));\n",
        "__attribute__((warn_unused)) typedef struct { int a; } t;\n",
        "int __attribute__((warn_unused)) v;\n",
        "int v __attribute__((warn_unused));\n",
        "[[gnu::warn_unused]] int v;\n",
        "void __attribute__((warn_unused)) f(void);\n",
        "__attribute__((warn_unused)) void f(void) {}\n",
        "void f(int p __attribute__((warn_unused))) { (void)p; }\n",
        "struct s { int __attribute__((warn_unused)) m; };\n",
        "int f(void) { int __attribute__((warn_unused)) l = 0; return l; }\n",
    ] {
        let (ok, err) = compile("ignored", source);
        assert!(ok, "{source}\n{err}");
        assert!(err.contains(ignored), "{source}\nwanted {ignored:?}, got:\n{err}");
        assert_eq!(err.matches("warning:").count(), 1, "{source}\n{err}");
    }
}

#[test]
fn warn_unused_given_an_argument_is_refused_wherever_it_is() {
    let wrong = "error: wrong number of arguments specified for 'warn_unused' attribute";
    for source in [
        "struct __attribute__((warn_unused(1))) d { int d; };\n",
        "enum __attribute__((warn_unused(1))) e { E0 };\n",
        "int __attribute__((warn_unused(1))) v;\n",
    ] {
        let (ok, err) = compile("arity", source);
        assert!(!ok, "{source}");
        assert!(err.contains(wrong), "{source}\nwanted {wrong:?}, got:\n{err}");
        assert!(err.contains("expected 0, found 1"), "{source}\n{err}");
        assert!(!err.contains("warning:"), "{source}\n{err}");
    }
}
