//! What `-fstrict-flex-arrays` does to `__builtin_object_size` through a pointer, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The kernel builds with `-fstrict-flex-arrays=3`, and its fortified `memcpy` asks for the size of
//! the member it copies into. Through a pointer the answer is the member's own size unless the
//! member is an array at the end of the record that the level counts as flexible. Every number
//! below is what gcc 15 printed for the same expression at `-O2`, with the pointer a parameter so
//! that nothing but the type is known.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const TARGET: &str = "x86_64-linux-gnu";

const TYPES: &str = "
struct A { char name[16]; int x; };
struct B { int n; char tail[8]; };
struct C { int n; char one[1]; };
struct D { int n; char zero[0]; };
struct E { int n; char fam[]; };
struct F { struct B inner; };
union U { int n; char tail[8]; };
struct N { int n; struct { int k; char t[4]; }; };
";

/// Each question and gcc's answer at levels zero to three, with -1 standing for `(size_t) -1`.
const TABLE: &[(&str, [i64; 4])] = &[
    ("a->name, 1", [16, 16, 16, 16]),
    ("&a->name[4], 1", [12, 12, 12, 12]),
    ("a->name, 0", [-1, -1, -1, -1]),
    ("a->name, 3", [16, 16, 16, 16]),
    ("&a[2].name[3], 1", [13, 13, 13, 13]),
    ("b->tail, 1", [-1, 8, 8, 8]),
    ("b->tail, 3", [0, 8, 8, 8]),
    ("(*b).tail, 1", [-1, 8, 8, 8]),
    ("&b[1].tail[2], 1", [-1, 6, 6, 6]),
    ("&b->n, 1", [4, 4, 4, 4]),
    ("c->one, 1", [-1, -1, 1, 1]),
    ("d->zero, 1", [-1, -1, -1, 0]),
    ("e->fam, 1", [-1, -1, -1, -1]),
    ("f->inner.tail, 1", [8, 8, 8, 8]),
    ("u->tail, 1", [-1, -1, -1, -1]),
    ("q->t, 1", [4, 4, 4, 4]),
];

fn fixture(what: &str, source: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-flex-arrays-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The compiler's exit status and its errors for that source under those flags.
fn check(what: &str, flags: &[&str], source: &str) -> (bool, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .arg("-fsyntax-only")
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The table at one level, as static assertions over parameters.
fn source(level: usize) -> String {
    let mut body = String::new();
    for (question, answers) in TABLE {
        body.push_str(&format!(
            "    _Static_assert(__builtin_object_size({question}) == (__SIZE_TYPE__) {}, \
             \"{question}\");\n",
            answers[level]
        ));
    }
    format!(
        "{TYPES}\nvoid f(struct A *a, struct B *b, struct C *c, struct D *d, struct E *e, \
         struct F *f, union U *u, struct N *q) {{\n{body}}}\n"
    )
}

#[test]
fn every_level_gives_the_answers_gcc_gives() {
    for level in 0..4 {
        let flag = format!("-fstrict-flex-arrays={level}");
        let (ok, errors) = check("level", &[&flag], &source(level));
        assert!(ok, "{flag}:\n{errors}");
    }
}

#[test]
fn the_bare_flag_is_level_three_and_the_default_is_level_zero() {
    let (ok, errors) = check("bare", &["-fstrict-flex-arrays"], &source(3));
    assert!(ok, "{errors}");
    let (ok, errors) = check("default", &[], &source(0));
    assert!(ok, "{errors}");
    let (ok, errors) =
        check("off", &["-fstrict-flex-arrays=3", "-fno-strict-flex-arrays"], &source(0));
    assert!(ok, "{errors}");
}

#[test]
fn a_level_that_is_not_one_of_the_four_is_refused() {
    for flag in ["-fstrict-flex-arrays=4", "-fstrict-flex-arrays=x", "-fstrict-flex-arrays="] {
        let (ok, errors) = check("refused", &[flag], "int x;\n");
        assert!(!ok, "{flag} was taken");
        assert!(errors.contains("a number from 0 to 3"), "{flag}: {errors}");
    }
}
