//! `__attribute__((null_terminated_string_arg(n)))`, end to end: the places gcc 14 takes it
//! without a word, and what it says about a number that does not name a pointer parameter, in its
//! words.

use std::path::{Path, PathBuf};
use std::process::Command;

const X86_64: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-string-arg-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished and what it said, for one source under those flags.
fn compile(what: &str, target: &str, flags: &[&str], source: &str) -> (bool, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={target}");
    let mut args = vec![target.as_str(), "-S", "-o", "-"];
    args.extend_from_slice(flags);
    args.push("a.c");
    let out = run(&dir, &args);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Everywhere gcc 14 takes it: a declaration and a definition, before the declarator and after
/// it, a parameter past the first and one before the `...`, an enumerator for the number, a
/// function nobody can count the parameters of, and a function type reached through a typedef, a
/// pointer, a member and a parameter.
#[test]
fn a_string_argument_is_taken_where_gcc_takes_it() {
    let source = "__attribute__((null_terminated_string_arg(1))) int a(const char *s);\n\
        int b(int n, const char *s) __attribute__((null_terminated_string_arg(2)));\n\
        [[gnu::null_terminated_string_arg(1)]] int c(char *s, ...);\n\
        enum { FIRST = 1 };\n\
        int d(const char *s) __attribute__((null_terminated_string_arg(FIRST)));\n\
        typedef int f_t(const char *) __attribute__((null_terminated_string_arg(1)));\n\
        int (*fp)(const char *) __attribute__((null_terminated_string_arg(1)));\n\
        int g(const char *s, const char *t) __attribute__((null_terminated_string_arg(1), \
            null_terminated_string_arg(2)));\n\
        int g(const char *s, const char *t) { return *s + *t; }\n\
        __attribute__((__null_terminated_string_arg__(1u))) int h(const char *s) { return *s; }\n\
        struct s { int (*m)(char *) __attribute__((null_terminated_string_arg(1))); };\n\
        void k(int (*p)(char *) __attribute__((null_terminated_string_arg(1))));\n\
        int l(char s[]) __attribute__((null_terminated_string_arg(sizeof(char))));\n\
        _Static_assert(__has_attribute(null_terminated_string_arg), \"gcc 14\");\n";
    let (ok, err) = compile("quiet", X86_64, &["-Wall", "-Wextra"], source);
    assert!(ok, "{err}");
    assert_eq!(err, "");
    // Before C23 an empty list says nothing about the parameters, and any number is taken.
    let unprototyped = "int e() __attribute__((null_terminated_string_arg(7)));\n";
    let (ok, err) = compile("unprototyped", X86_64, &["-std=gnu17", "-Wall"], unprototyped);
    assert!(ok, "{err}");
    assert_eq!(err, "");
}

/// What gcc 14 says about a number that names no pointer parameter, and that it only warns.
#[test]
fn a_string_argument_is_checked_in_gcc_s_words() {
    let name = "'null_terminated_string_arg' attribute argument";
    for (source, said, errors, warnings) in [
        (
            "int f(const char *s) __attribute__((null_terminated_string_arg(0)));\n",
            format!("{name} value '0' does not refer to a function parameter"),
            0,
            1,
        ),
        (
            "int f(const char *s) __attribute__((null_terminated_string_arg(2)));\n",
            format!("{name} value '2' exceeds the number of function parameters 1"),
            0,
            1,
        ),
        (
            "int f(const char *s, ...) __attribute__((null_terminated_string_arg(2)));\n",
            format!("{name} value '2' exceeds the number of function parameters 1"),
            0,
            1,
        ),
        (
            "int f(void) __attribute__((null_terminated_string_arg(1)));\n",
            format!("{name} value '1' exceeds the number of function parameters 0"),
            0,
            1,
        ),
        (
            "int f(const char *s) __attribute__((null_terminated_string_arg(-1)));\n",
            format!("{name} value '-1' exceeds the number of function parameters 1"),
            0,
            1,
        ),
        (
            "int f(int n) __attribute__((null_terminated_string_arg(1)));\n",
            format!("{name} value '1' refers to parameter type 'int'"),
            0,
            1,
        ),
        (
            "typedef unsigned long size_t;\n\
             int f(size_t n) __attribute__((null_terminated_string_arg(1u)));\n",
            format!("{name} value '1u' refers to parameter type 'size_t' {{aka 'long unsigned int'}}"),
            0,
            1,
        ),
        (
            "int f(int n) __attribute__((null_terminated_string_arg(sizeof(int) - 3)));\n",
            format!("{name} value '1ul' refers to parameter type 'int'"),
            0,
            1,
        ),
        (
            "int f(const char *s) __attribute__((null_terminated_string_arg(1.0)));\n",
            format!("{name} has type 'double'"),
            0,
            1,
        ),
        (
            "int f(const char *s) __attribute__((null_terminated_string_arg(\"1\")));\n",
            format!("{name} has type 'char *'"),
            0,
            1,
        ),
        (
            "int n;\nint f(const char *s) __attribute__((null_terminated_string_arg(n)));\n",
            format!("{name} value 'n' is not an integer constant"),
            0,
            1,
        ),
        (
            "int f(const char *s) __attribute__((null_terminated_string_arg(nope)));\n",
            "error: 'nope' undeclared here (not in a function)".to_owned(),
            1,
            1,
        ),
        (
            "int f(const char *s) __attribute__((null_terminated_string_arg(nope)));\n",
            format!("{name} is invalid"),
            1,
            1,
        ),
        (
            "int v __attribute__((null_terminated_string_arg(1)));\n",
            "'null_terminated_string_arg' attribute only applies to function types".to_owned(),
            0,
            1,
        ),
        (
            "struct s { char *m __attribute__((null_terminated_string_arg(1))); };\n",
            "'null_terminated_string_arg' attribute only applies to function types".to_owned(),
            0,
            1,
        ),
        (
            "int f(const char *s) __attribute__((null_terminated_string_arg));\n",
            "error: wrong number of arguments specified for 'null_terminated_string_arg' attribute"
                .to_owned(),
            1,
            0,
        ),
        (
            "int f(const char *s) __attribute__((null_terminated_string_arg(1, 2)));\n",
            "expected 1, found 2".to_owned(),
            1,
            0,
        ),
    ] {
        let (ok, err) = compile("words", X86_64, &["-O2"], source);
        assert_eq!(ok, errors == 0, "{source}\n{err}");
        assert!(err.contains(&said), "{source}\nwanted {said:?}, got:\n{err}");
        assert_eq!(err.matches("error:").count(), errors, "{source}\n{err}");
        assert_eq!(err.matches("warning:").count(), warnings, "{source}\n{err}");
    }
    // Under C23 an empty list is a prototype that takes nothing, so the number is counted.
    let empty = "int f() __attribute__((null_terminated_string_arg(1)));\n";
    let (ok, err) = compile("c23", X86_64, &["-std=c23"], empty);
    assert!(ok, "{err}");
    assert!(err.contains(&format!("{name} value '1' exceeds the number of function parameters 0")));
    // gcc 13 has never heard of it.
    let old = "int f(const char *s) __attribute__((null_terminated_string_arg(1)));\n";
    let (ok, err) = compile("old", X86_64, &["-fgnuc-version=13.2.0"], old);
    assert!(ok, "{err}");
    assert!(err.contains("warning: 'null_terminated_string_arg' attribute directive ignored"));
    assert!(!err.contains("refers to"), "{err}");
}
