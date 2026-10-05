//! `__attribute__((warn_if_not_aligned(n)))` and `-Wif-not-aligned`, end to end, against what
//! gcc 13 says about the same programs on x86-64 and on i386.

use std::path::PathBuf;
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-wina-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler took the source under those flags, and what it said.
fn check(what: &str, target: &str, flags: &[&str], source: &str) -> (bool, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={target}");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(&target)
        .args(flags)
        .args(["-fsyntax-only", "a.c"])
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// What the warning says, counted in what the compiler said.
fn said(err: &str) -> usize {
    err.lines()
        .filter(|line| {
            (line.contains("is less than") || line.contains("isn't aligned to"))
                && !line.trim_start().starts_with('|')
        })
        .count()
}

/// Every place the number can come from, the typedef, a typedef of it, a bare one, the member's
/// own, a tagged record's, an enumeration's and an array's, and every layout that can satisfy it
/// or not. A variable of the type is nothing to warn about.
const MEMBERS: &str = "typedef unsigned long long u64 __attribute__((aligned(4), warn_if_not_aligned(8)));\n\
    typedef u64 u64b;\n\
    typedef int big __attribute__((warn_if_not_aligned));\n\
    struct a { int i; u64 x; };\n\
    struct b { int i; u64 x; } __attribute__((aligned(8)));\n\
    struct c { u64 x; int i; };\n\
    struct d { char c; u64 x; } __attribute__((packed));\n\
    struct e { int i; unsigned long long y __attribute__((warn_if_not_aligned(8))); };\n\
    struct f { int i; int y __attribute__((warn_if_not_aligned(8))); };\n\
    struct g { long long l; int i; u64 x; };\n\
    struct h { u64 x; };\n\
    struct h2 { int i; struct h h; };\n\
    struct i { int i; big x; };\n\
    struct j { int i; u64 arr[2]; };\n\
    union k { int i; u64 x; };\n\
    struct __attribute__((warn_if_not_aligned(16))) l { long long a; };\n\
    struct m { int i; struct l l; };\n\
    struct n { int i; struct { u64 x; } inner; };\n\
    struct q { int i; _Alignas(8) u64 x; };\n\
    struct s { char c; u64 x; } __attribute__((packed, aligned(8)));\n\
    struct t1 { int i; u64 x; int j; u64 y; };\n\
    struct t2 { int i; const u64 x; };\n\
    struct t3 { int i; u64 *p; };\n\
    struct t4 { int i; u64b x; };\n\
    struct t6 { int i; char x __attribute__((warn_if_not_aligned(2))); char y __attribute__((warn_if_not_aligned(4))); };\n\
    struct t7 { int i; u64 x[]; };\n\
    struct b2 { int i; __attribute__((warn_if_not_aligned(8))) int x; };\n\
    enum __attribute__((warn_if_not_aligned(8))) en { A };\n\
    struct b3 { int i; enum en e; };\n\
    typedef int arr[2] __attribute__((warn_if_not_aligned(8)));\n\
    struct b4 { int i; arr a; };\n\
    u64 global;\n\
    void fn(void) { u64 local; (void)local; }\n";

/// What gcc 13 says on both targets, in its order.
const BOTH: &[&str] = &[
    "alignment 4 of 'struct a' is less than 8",
    "'x' offset 4 in 'struct a' isn't aligned to 8",
    "'x' offset 4 in 'struct b' isn't aligned to 8",
    "alignment 4 of 'struct c' is less than 8",
    "alignment 1 of 'struct d' is less than 8",
    "'x' offset 1 in 'struct d' isn't aligned to 8",
    "alignment 4 of 'struct f' is less than 8",
    "'y' offset 4 in 'struct f' isn't aligned to 8",
    "'x' offset 12 in 'struct g' isn't aligned to 8",
    "alignment 4 of 'struct h' is less than 8",
    "alignment 4 of 'struct i' is less than 16",
    "'x' offset 4 in 'struct i' isn't aligned to 16",
    "alignment 4 of 'struct j' is less than 8",
    "'arr' offset 4 in 'struct j' isn't aligned to 8",
    "alignment 4 of 'union k' is less than 8",
    "alignment 4 of 'struct <anonymous>' is less than 8",
    "'x' offset 1 in 'struct s' isn't aligned to 8",
    "alignment 4 of 'struct t1' is less than 8",
    "'x' offset 4 in 'struct t1' isn't aligned to 8",
    "alignment 4 of 'struct t1' is less than 8",
    "alignment 4 of 'struct t2' is less than 8",
    "'x' offset 4 in 'struct t2' isn't aligned to 8",
    "alignment 4 of 'struct t4' is less than 8",
    "'x' offset 4 in 'struct t4' isn't aligned to 8",
    "'y' offset 5 in 'struct t6' isn't aligned to 4",
    "alignment 4 of 'struct t7' is less than 8",
    "'x' offset 4 in 'struct t7' isn't aligned to 8",
    "alignment 4 of 'struct b2' is less than 8",
    "'x' offset 4 in 'struct b2' isn't aligned to 8",
    "alignment 4 of 'struct b3' is less than 8",
    "'e' offset 4 in 'struct b3' isn't aligned to 8",
    "alignment 4 of 'struct b4' is less than 8",
    "'a' offset 4 in 'struct b4' isn't aligned to 8",
];

/// What gcc 13 says on x86-64 alone, where a `long long` member is at a multiple of eight.
const X86_64: &[&str] = &[
    "alignment 8 of 'struct m' is less than 16",
    "'l' offset 8 in 'struct m' isn't aligned to 16",
];

/// What gcc 13 says on i386 alone, where it is at a multiple of four.
const I386: &[&str] = &[
    "alignment 4 of 'struct e' is less than 8",
    "'y' offset 4 in 'struct e' isn't aligned to 8",
    "alignment 4 of 'struct g' is less than 8",
    "alignment 4 of 'struct m' is less than 16",
    "'l' offset 4 in 'struct m' isn't aligned to 16",
];

#[test]
fn a_member_that_asks_for_more_than_it_gets_is_warned_about_in_gcc_s_words() {
    for (target, own) in [("x86_64-unknown-linux-gnu", X86_64), ("i686-unknown-linux-gnu", I386)] {
        let (ok, err) = check("members", target, &[], MEMBERS);
        assert!(ok, "{target}: {err}");
        let wanted: Vec<&str> = BOTH.iter().chain(own).copied().collect();
        for message in &wanted {
            let times = wanted.iter().filter(|other| *other == message).count();
            assert_eq!(err.matches(message).count(), times, "{target}: {message}\n{err}");
        }
        assert_eq!(said(&err), wanted.len(), "{target}:\n{err}");
        assert!(err.contains("if-not-aligned") || err.contains("E0824"), "{err}");
    }
}

#[test]
fn the_warning_is_turned_off_and_made_an_error_by_its_own_name() {
    let target = "x86_64-unknown-linux-gnu";
    let (ok, err) = check("off", target, &["-Wno-if-not-aligned"], MEMBERS);
    assert!(ok, "{err}");
    assert_eq!(said(&err), 0, "{err}");
    let (ok, err) = check("error", target, &["-Werror=if-not-aligned"], MEMBERS);
    assert!(!ok, "{err}");
    assert!(err.contains("error"), "{err}");
}

/// What gcc refuses, in its words: the attribute on an object or a function, on a bit-field or a
/// bit-field of a type that carries it, a number that is not a power of two, and a second
/// argument. Zero is warned about and dropped.
#[test]
fn the_attribute_is_checked_in_gcc_s_words() {
    let target = "x86_64-unknown-linux-gnu";
    for (source, wanted, error) in [
        (
            "int v __attribute__((warn_if_not_aligned(8)));\n",
            "'warn_if_not_aligned' may not be specified for 'v'",
            true,
        ),
        (
            "void f(void) { int b __attribute__((warn_if_not_aligned(8))); (void)b; }\n",
            "'warn_if_not_aligned' may not be specified for 'b'",
            true,
        ),
        (
            "void f(void) __attribute__((warn_if_not_aligned(8)));\n",
            "'warn_if_not_aligned' may not be specified for 'f'",
            true,
        ),
        (
            "struct b1 { int i; int x:3 __attribute__((warn_if_not_aligned(8))); };\n",
            "'warn_if_not_aligned' may not be specified for 'x'",
            true,
        ),
        (
            "typedef int w __attribute__((warn_if_not_aligned(8)));\n\
             struct p { int i; w x:3; };\n",
            "cannot declare bit-field 'x' with 'warn_if_not_aligned' type",
            true,
        ),
        (
            "typedef int t __attribute__((warn_if_not_aligned(3)));\n",
            "requested alignment '3' is not a positive power of 2",
            true,
        ),
        (
            "typedef int t __attribute__((warn_if_not_aligned(-8)));\n",
            "requested alignment '-8' is not a positive power of 2",
            true,
        ),
        (
            "typedef int t __attribute__((warn_if_not_aligned(8, 2)));\n",
            "wrong number of arguments specified for 'warn_if_not_aligned' attribute",
            true,
        ),
        ("typedef int t __attribute__((warn_if_not_aligned(8, 2)));\n", "expected between 0 and 1, found 2", true),
        (
            "typedef int t __attribute__((warn_if_not_aligned(0)));\n\
             struct z { char c; t x; };\n",
            "requested alignment '0' is not a positive power of 2",
            false,
        ),
        (
            "typedef int t __attribute__((aligned(0)));\n",
            "requested alignment '0' is not a positive power of 2",
            false,
        ),
    ] {
        let (ok, err) = check("words", target, &[], source);
        assert_eq!(ok, !error, "{source}\n{err}");
        assert!(err.contains(wanted), "{source}\nwanted {wanted:?}, got:\n{err}");
    }
    // Zero leaves nothing to warn about, and the name is one `__has_attribute` knows.
    let source = "typedef int t __attribute__((warn_if_not_aligned(0)));\n\
        struct z { char c; t x; };\n\
        _Static_assert(__has_attribute(warn_if_not_aligned), \"warn_if_not_aligned\");\n";
    let (ok, err) = check("zero", target, &[], source);
    assert!(ok, "{err}");
    assert_eq!(said(&err), 0, "{err}");
}
