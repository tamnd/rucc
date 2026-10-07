//! `__attribute__((nonnull_if_nonzero(p, n)))`, end to end: the places gcc 15 takes it without a
//! word, glibc's `<string.h>` among them, and what it says about numbers that do not name a pointer
//! parameter and integer ones, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;

const X86_64: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-nonnull-if-{}-{what}", std::process::id()));
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

/// Everywhere gcc 15 takes it: glibc's declarations, two and three numbers, an enumeration and a
/// `char` for the count, a definition, a function nobody can count the parameters of, and a
/// function type reached through a typedef, a pointer, a member and a parameter. A null pointer
/// with a count that is not zero is not warned about, as `nonnull`'s is not.
#[test]
fn a_conditional_nonnull_is_taken_where_gcc_takes_it() {
    let source = "typedef __SIZE_TYPE__ size_t;\n\
        extern void *memcpy(void *__restrict __dest, const void *__restrict __src, size_t __n) \
            __attribute__((__nothrow__, __leaf__)) __attribute__((__nonnull_if_nonzero__(1, 3))) \
            __attribute__((__nonnull_if_nonzero__(2, 3)));\n\
        extern int memcmp(const void *a, const void *b, size_t n) \
            __attribute__((__pure__, __nonnull_if_nonzero__(1, 3), \
            __nonnull_if_nonzero__(2, 3)));\n\
        void *fill(void *d, size_t n, int m) __attribute__((nonnull_if_nonzero(1, 2, 3)));\n\
        enum e { A, B };\n\
        void *by_enum(void *d, enum e n) __attribute__((nonnull_if_nonzero(1, 2)));\n\
        [[gnu::nonnull_if_nonzero(1, 2)]] void *by_char(void *d, char n);\n\
        void *by_bits(void *d, unsigned _BitInt(7) n) \
            __attribute__((nonnull_if_nonzero(1, A + 2)));\n\
        __attribute__((nonnull_if_nonzero(1, 2))) int def(const char *s, long n) \
            { return n ? *s : 0; }\n\
        typedef void *f_t(void *, size_t) __attribute__((nonnull_if_nonzero(1, 2)));\n\
        void *(*fp)(void *, size_t) __attribute__((nonnull_if_nonzero(1, 2)));\n\
        struct s { void *(*m)(void *, size_t) __attribute__((nonnull_if_nonzero(1, 2))); };\n\
        void k(void *(*p)(void *, size_t) __attribute__((nonnull_if_nonzero(1, 2))));\n\
        int use(char *d, const char *s) \
            { memcpy(d, s, 0); memcpy(0, s, 4); return memcmp(d, 0, 0); }\n\
        _Static_assert(__has_attribute(nonnull_if_nonzero), \"gcc 15\");\n";
    let (ok, err) = compile("quiet", X86_64, &["-Wall", "-Wextra", "-std=gnu23"], source);
    assert!(ok, "{err}");
    assert_eq!(err, "");
    // Before C23 an empty list says nothing about the parameters, and any number is taken.
    let unprototyped = "void *f() __attribute__((nonnull_if_nonzero(1, 2)));\n";
    let (ok, err) = compile("unprototyped", X86_64, &["-std=gnu17", "-Wall"], unprototyped);
    assert!(ok, "{err}");
    assert_eq!(err, "");
}

/// What gcc 15 says about numbers that name the wrong parameters, each one counted from one in
/// its words, and that it only warns.
#[test]
fn a_conditional_nonnull_is_checked_in_gcc_s_words() {
    let name = "'nonnull_if_nonzero' attribute argument";
    for (source, said, errors, warnings) in [
        (
            "void *f(int d, unsigned long n) __attribute__((nonnull_if_nonzero(1, 2)));\n",
            format!("{name} 1 value '1' refers to parameter type 'int'"),
            0,
            1,
        ),
        (
            "void *f(void *d, void *n) __attribute__((nonnull_if_nonzero(1, 2)));\n",
            format!("{name} 2 value '2' refers to parameter type 'void *'"),
            0,
            1,
        ),
        (
            "void *f(void *d, _Bool n) __attribute__((nonnull_if_nonzero(1, 2)));\n",
            format!("{name} 2 value '2' refers to parameter type 'bool'"),
            0,
            1,
        ),
        (
            "void *f(void *d, unsigned long n, double m) \
                __attribute__((nonnull_if_nonzero(1, 2, 3)));\n",
            format!("{name} 3 value '3' refers to parameter type 'double'"),
            0,
            1,
        ),
        (
            "void *f(void *d, unsigned long n) __attribute__((nonnull_if_nonzero(0, 2)));\n",
            format!("{name} 1 value '0' does not refer to a function parameter"),
            0,
            1,
        ),
        (
            "void *f(void *d, unsigned long n) __attribute__((nonnull_if_nonzero(1, 3)));\n",
            format!("{name} 2 value '3' exceeds the number of function parameters 2"),
            0,
            1,
        ),
        (
            "void *f(void *d, unsigned long n) __attribute__((nonnull_if_nonzero(3, 3)));\n",
            format!("{name} 1 value '3' exceeds the number of function parameters 2"),
            0,
            2,
        ),
        (
            "void *f(void *d, unsigned long n) __attribute__((nonnull_if_nonzero(1.0, 2)));\n",
            format!("{name} 1 has type 'double'"),
            0,
            1,
        ),
        (
            "int n;\nvoid *f(void *d, unsigned long n) \
                __attribute__((nonnull_if_nonzero(1, n)));\n",
            format!("{name} 2 value 'n' is not an integer constant"),
            0,
            1,
        ),
        (
            "int v __attribute__((nonnull_if_nonzero(1, 2)));\n",
            "'nonnull_if_nonzero' attribute only applies to function types".to_owned(),
            0,
            1,
        ),
        (
            "void *f(void *d, unsigned long n) __attribute__((nonnull_if_nonzero(1)));\n",
            "error: wrong number of arguments specified for 'nonnull_if_nonzero' attribute"
                .to_owned(),
            1,
            0,
        ),
        (
            "void *f(void *d, unsigned long n) __attribute__((nonnull_if_nonzero(1, 2, 2, 2)));\n",
            "expected between 2 and 3, found 4".to_owned(),
            1,
            0,
        ),
        (
            "void *f(void *d, unsigned long n) __attribute__((nonnull_if_nonzero));\n",
            "expected between 2 and 3, found 0".to_owned(),
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
    // Under C23 an empty list is a prototype that takes nothing, so both numbers are counted.
    let empty = "void *f() __attribute__((nonnull_if_nonzero(1, 2)));\n";
    let (ok, err) = compile("c23", X86_64, &["-std=c23"], empty);
    assert!(ok, "{err}");
    for argno in [1, 2] {
        let said =
            format!("{name} {argno} value '{argno}' exceeds the number of function parameters 0");
        assert!(err.contains(&said), "wanted {said:?}, got:\n{err}");
    }
    // gcc 14 has never heard of it.
    let old = "void *f(int d, long n) __attribute__((nonnull_if_nonzero(1, 2)));\n";
    let (ok, err) = compile("old", X86_64, &["-fgnuc-version=14.2.0"], old);
    assert!(ok, "{err}");
    assert!(err.contains("warning: 'nonnull_if_nonzero' attribute directive ignored"), "{err}");
    assert!(!err.contains("refers to"), "{err}");
}
