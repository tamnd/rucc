//! Microsoft's `__int8`, `__int16`, `__int32` and `__int64` on a Windows target.
//!
//! mingw-w64's `_mingw.h` defines all four as macros, so they only matter to a file that uses
//! one before its first include, which code written for MSVC first often does. On any other
//! target they are ordinary identifiers, as they are in gcc and clang.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-int64-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler agreed, and what it said.
fn check(what: &str, target: &str, source: &str) -> (bool, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([target, "-std=c11", "-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

const TYPES: &str = "\
__int64 a;
unsigned __int64 b;
__int64 int c;
__int8 d;
unsigned __int8 e;
signed __int16 f;
unsigned __int32 g;
_Static_assert(_Generic(a, long long: 1, default: 0), \"__int64 is long long\");
_Static_assert(_Generic(b, unsigned long long: 1, default: 0), \"unsigned __int64\");
_Static_assert(_Generic(c, long long: 1, default: 0), \"__int64 int\");
_Static_assert(_Generic(d, char: 1, default: 0), \"__int8 is char\");
_Static_assert(_Generic(e, unsigned char: 1, default: 0), \"unsigned __int8\");
_Static_assert(_Generic(f, short: 1, default: 0), \"__int16 is short\");
_Static_assert(_Generic(g, unsigned int: 1, default: 0), \"__int32 is int\");
";

#[test]
fn the_microsoft_integer_names_are_types_on_windows() {
    for target in ["--target=x86_64-windows-gnu", "--target=aarch64-windows-gnu"] {
        let (ok, said) = check("types", target, TYPES);
        assert!(ok, "{target}: {said}");
    }
}

#[test]
fn a_third_long_is_still_too_many() {
    let (ok, said) = check("long", "--target=x86_64-windows-gnu", "long __int64 x;\n");
    assert!(!ok);
    assert!(said.contains("too long"), "{said}");
}

#[test]
fn elsewhere_they_are_names_a_program_may_use() {
    let source = "int __int64 = 1, __int8 = 2;\n";
    let (ok, said) = check("names", "--target=x86_64-linux-gnu", source);
    assert!(ok, "{said}");
}
