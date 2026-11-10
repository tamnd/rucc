//! The integer conversions of kencc under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4148, and 06.8 and 03.9 of the rucc-plan9 plan.
//!
//! kencc takes the type of a binary operation from the table `tab` in `sub.c`. The rank is the
//! larger of the two, and the type is unsigned when either operand is unsigned. The arithmetic
//! operators then promote the type to at least `int` and keep the sign, so `unsigned char` with
//! `int` is `unsigned int`. A comparison, a conditional and a compound assignment do not
//! promote, so `char` with `unsigned char` is `unsigned char` there.

use std::path::PathBuf;
use std::process::Command;

/// Each line holds in kencc and not in C.
const SOURCE: &str = "typedef unsigned char uchar;\n\
                      typedef unsigned short ushort;\n\
                      #define IS(e, t) _Generic((e), t: 1, default: 0)\n\
                      uchar c; char s; ushort us; long long v; unsigned int ui; long l;\n\
                      _Static_assert(IS(c + 1, unsigned int), \"uchar with int\");\n\
                      _Static_assert(IS(c + c, unsigned int), \"uchar with uchar\");\n\
                      _Static_assert(IS(us * 2, unsigned int), \"ushort with int\");\n\
                      _Static_assert(IS(v + ui, unsigned long long), \"vlong with uint\");\n\
                      _Static_assert(IS(l - ui, unsigned long), \"long with uint\");\n\
                      _Static_assert(IS(-c, unsigned int), \"negation\");\n\
                      _Static_assert(IS(c << 1, unsigned int), \"shift\");\n\
                      _Static_assert(IS(1 ? s : c, uchar), \"conditional\");\n\
                      _Static_assert((uchar)0 - 1 > 0, \"the difference is unsigned\");\n\
                      _Static_assert((char)-1 == (uchar)255, \"the comparison is in uchar\");\n";

fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-arith-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source and what it said.
fn run(what: &str, flags: &[&str]) -> (bool, String) {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fsyntax-only", "-w"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn kencc_keeps_the_sign() {
    let (ok, err) = run("kencc", &["-fkencc"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(err.is_empty(), "{err}");
}

#[test]
fn c_promotes_to_int() {
    let (ok, err) = run("c", &[]);
    assert!(!ok);
    for what in [
        "uchar with int",
        "uchar with uchar",
        "ushort with int",
        "vlong with uint",
        "long with uint",
        "negation",
        "shift",
        "conditional",
        "the difference is unsigned",
        "the comparison is in uchar",
    ] {
        assert!(err.contains(what), "{what}: {err}");
    }
}
