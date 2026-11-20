//! Brace elision over an array whose elements are pointers, end to end.
//!
//! Design: tamnd/rucc#4152, and 03.3 of the rucc-plan9 plan.
//!
//! In `char *d[2][2] = { "a", "b", "c", "d" }` each literal is the value of one pointer, as
//! C11 6.7.9p20 requires and as gcc reads it. A string literal fills an array as a whole only
//! where the elements of the array are integers.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-elision-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, what it wrote and what it said.
fn run(what: &str, source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    (out.status.success(), text(out.stdout), text(out.stderr))
}

/// The data of `name` in a listing, from its label to its `.size`.
fn data<'a>(listing: &'a str, name: &str) -> &'a str {
    let start = listing.find(&format!("\n{name}:\n")).expect("the object is in the listing");
    let rest = &listing[start..];
    &rest[..rest.find("\t.size").expect("the object has a size")]
}

#[test]
fn each_literal_is_one_pointer() {
    let source = "char *d[2][2] = { \"a\", \"b\", \"c\", \"d\" };\n\
                  struct S { char *p[2]; int n; } s = { \"x\", \"y\", 3 };\n";
    let (ok, out, err) = run("pointers", source);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(err.is_empty(), "{err}");
    assert_eq!(data(&out, "d").matches("\t.quad\t.Lstr.").count(), 4, "{out}");
    let s = data(&out, "s");
    assert_eq!(s.matches("\t.quad\t.Lstr.").count(), 2, "{s}");
    assert!(s.contains("\t.long\t3\n"), "{s}");
}

#[test]
fn a_literal_still_fills_an_array_of_characters() {
    let (ok, out, err) = run("chars", "char b[2][3] = { \"ab\", \"cd\" };\n");
    assert!(ok, "{err}");
    let b = data(&out, "b");
    assert!(b.contains("\t.ascii\t\"ab\\000\"\n\t.ascii\t\"cd\\000\"\n"), "{b}");
    let (ok, _, err) = run("ints", "int a[2][3] = { \"ab\" };\n");
    assert!(!ok);
    assert!(
        err.contains(
            "cannot initialize array of 'int' from a string literal with type array of 'char'"
        ),
        "{err}"
    );
}
