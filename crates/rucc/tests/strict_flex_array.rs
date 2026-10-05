//! `__attribute__((strict_flex_array(level)))`, end to end: one trailing array member measured by
//! `__builtin_object_size` through a pointer at its own `-fstrict-flex-arrays` level rather than
//! the command line's, and what gcc says about the attribute on the way, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, since the sizes are its.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-strict-flex-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Checks one source under those flags, and what was said.
fn check(what: &str, flags: &[&str], source: &str) -> (bool, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={TARGET}");
    let mut args = vec![target.as_str(), "-O2", "-fsyntax-only"];
    args.extend_from_slice(flags);
    args.push("a.c");
    let said = run(&dir, &args);
    let _ = std::fs::remove_dir_all(&dir);
    said
}

/// The members, each the last of a record of its own: the array's length, the level the
/// attribute asks for if it asks, and the sizes gcc 13 answers through a pointer under
/// `-fstrict-flex-arrays=0` and `=3`, where -1 is the size of something that may be longer.
const MEMBERS: &[(&str, Option<u8>, [i64; 2])] = &[
    ("4", Some(3), [16, 16]),
    ("4", Some(0), [-1, -1]),
    ("1", Some(1), [-1, -1]),
    ("1", Some(2), [4, 4]),
    ("0", Some(2), [-1, -1]),
    ("0", Some(3), [0, 0]),
    ("", Some(3), [-1, -1]),
    ("4", None, [-1, 16]),
];

/// The members as static assertions over parameters, at one of the two command line levels.
fn sizes(at: usize) -> String {
    let mut text = String::new();
    for (index, (len, level, answers)) in MEMBERS.iter().enumerate() {
        let attr = level.map_or_else(String::new, |level| {
            format!(" __attribute__((strict_flex_array({level})))")
        });
        text.push_str(&format!("struct s{index} {{ int n; int x[{len}]{attr}; }};\n"));
        text.push_str(&format!(
            "void f{index}(struct s{index} *p) {{ _Static_assert(__builtin_object_size(p->x, 1) \
             == (__SIZE_TYPE__) {}, \"s{index}\"); }}\n",
            answers[at]
        ));
    }
    text
}

#[test]
fn a_member_s_own_level_takes_the_place_of_the_command_line_s() {
    for (at, flag) in ["-fstrict-flex-arrays=0", "-fstrict-flex-arrays=3"].into_iter().enumerate() {
        let (ok, err) = check("sizes", &[flag], &sizes(at));
        assert!(ok, "{flag}:\n{err}");
        assert_eq!(err, "", "{flag}");
    }
    // The other spellings ask the same, and an object reached by name rather than through a
    // pointer has the size its type says whatever the member asks.
    let spelled = "struct a { int n; int x[4] __attribute__((__strict_flex_array__(0))); };\n\
        struct b { int n; [[gnu::strict_flex_array(0)]] int x[4]; };\n\
        struct a va;\n\
        void g(struct a *p, struct b *q) {\n\
            _Static_assert(__builtin_object_size(p->x, 1) == (__SIZE_TYPE__)-1, \"armoured\");\n\
            _Static_assert(__builtin_object_size(q->x, 1) == (__SIZE_TYPE__)-1, \"standard\");\n\
            _Static_assert(__builtin_object_size(va.x, 1) == 16, \"named\");\n\
        }\n";
    let (ok, err) = check("spelled", &["-fstrict-flex-arrays=3"], spelled);
    assert!(ok, "{err}");
    assert_eq!(err, "");
}

/// What gcc 13 says about the attribute, in its words. Each is an error.
#[test]
fn strict_flex_array_is_checked_in_gcc_s_words() {
    let named = |name: &str| {
        format!("error: 'strict_flex_array' attribute may not be specified for '{name}'")
    };
    let between = |value: &str| {
        format!(
            "error: 'strict_flex_array' attribute argument '{value}' is not an integer constant \
             between 0 and 3"
        )
    };
    let not_integer = "error: 'strict_flex_array' attribute argument not an integer".to_owned();
    let arity = "error: wrong number of arguments specified for 'strict_flex_array' attribute";
    for (source, said) in [
        ("int v __attribute__((strict_flex_array(1)));\n", named("v")),
        ("typedef int t[1] __attribute__((strict_flex_array(1)));\n", named("t")),
        ("void g(int p[1] __attribute__((strict_flex_array(1))));\n", named("p")),
        ("__attribute__((strict_flex_array(1))) int f(void) { return 0; }\n", named("f")),
        ("void g(int [1] __attribute__((strict_flex_array(1))));\n", named("({anonymous})")),
        ("void h(void) { int v[2] __attribute__((strict_flex_array(1))); }\n", named("v")),
        (
            "struct s { int n; int : 3 __attribute__((strict_flex_array(1))); };\n",
            "error: 'strict_flex_array' attribute may not be specified for a non-array field"
                .to_owned(),
        ),
        (
            "struct s { int n; int x __attribute__((strict_flex_array(1))); };\n",
            "error: 'strict_flex_array' attribute may not be specified for a non-array field"
                .to_owned(),
        ),
        ("struct s { int n; int x[1] __attribute__((strict_flex_array(4))); };\n", between("4")),
        ("struct s { int n; int x[1] __attribute__((strict_flex_array(-1))); };\n", between("-1")),
        (
            "struct s { int n; int x[1] __attribute__((strict_flex_array(\"1\"))); };\n",
            not_integer.clone(),
        ),
        (
            "struct s { int n; int x[1] __attribute__((strict_flex_array(1.0))); };\n",
            not_integer.clone(),
        ),
        (
            "int k; struct s { int n; int x[1] __attribute__((strict_flex_array(k))); };\n",
            not_integer,
        ),
        ("struct s { int n; int x[1] __attribute__((strict_flex_array)); };\n", arity.to_owned()),
        (
            "struct s { int n; int x[1] __attribute__((strict_flex_array)); };\n",
            "expected 1, found 0".to_owned(),
        ),
        (
            "struct s { int n; int x[1] __attribute__((strict_flex_array(1, 2))); };\n",
            "expected 1, found 2".to_owned(),
        ),
    ] {
        let (ok, err) = check("words", &[], source);
        assert!(!ok, "{source}");
        assert!(err.contains(&said), "{source}\nwanted {said:?}, got:\n{err}");
        assert_eq!(err.matches("error:").count(), 1, "{source}\n{err}");
    }
    // Nothing to say about an array that is not the last member, a member of a union, an array
    // of arrays or a level an enumeration constant spells, and the attribute is there to ask
    // about.
    let quiet = "enum { L = 2 };\n\
        struct s { int x[2] __attribute__((strict_flex_array(1))); int n; };\n\
        union u { int n; int x[2] __attribute__((strict_flex_array(3))); };\n\
        struct t { int n; int x[2][2] __attribute__((strict_flex_array(3))); };\n\
        struct w { int n; int x[1] __attribute__((strict_flex_array(L))); };\n\
        void gw(struct w *p) { _Static_assert(__builtin_object_size(p->x, 1) == 4, \"L\"); }\n\
        _Static_assert(__has_attribute(strict_flex_array), \"gcc 13\");\n";
    let (ok, err) = check("quiet", &["-Wall", "-Wextra"], quiet);
    assert!(ok, "{err}");
    assert_eq!(err, "");
    // gcc 12 has never heard of it.
    let old = "struct s { int n; int x[1] __attribute__((strict_flex_array(9))); };\n";
    let (ok, err) = check("old", &["-fgnuc-version=12.2.0"], old);
    assert!(ok, "{err}");
    assert!(err.contains("warning: 'strict_flex_array' attribute directive ignored"), "{err}");
}
