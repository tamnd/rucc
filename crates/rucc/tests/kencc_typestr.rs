//! kencc's `typestr` storage class under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4148, and 06.2 of the rucc-plan9 plan.
//!
//! In kencc, `typestr` is the storage class of a typedef whose type is a `struct` or a `union`.
//! `funct.c` lets such a type have operator functions, but the 9front tree has none. rucc reads
//! the class as `typedef` and refuses a type that is not a `struct` or a `union`, as `dodecl`
//! does. In C, `typestr` is an identifier.

use std::path::PathBuf;
use std::process::Command;

const RECORDS: &str = "typestr struct Vec Vec;\n\
                       struct Vec { int x, y; };\n\
                       typestr union U U;\n\
                       union U { int i; char c; };\n\
                       int f(Vec *v)\n\
                       {\n\
                       \ttypestr struct In In;\n\
                       \tstruct In { int a; } in;\n\
                       \tVec w;\n\
                       \tw = *v;\n\
                       \tin.a = w.x;\n\
                       \treturn in.a + w.y + sizeof(U);\n\
                       }\n";

const SCALAR: &str = "typestr int T;\n";

/// Whether the compiler took `source`, and what it said.
fn check(what: &str, source: &str, flags: &[&str]) -> (bool, String) {
    let dir =
        std::env::temp_dir().join(format!("rucc-kencc-typestr-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fsyntax-only"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn typestr_names_a_struct_or_a_union() {
    let (ok, err) = check("records", RECORDS, &["-fkencc"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(err.is_empty(), "{err}");
}

#[test]
fn typestr_of_a_scalar_is_an_error() {
    let (ok, err) = check("scalar", SCALAR, &["-fkencc"]);
    assert!(!ok);
    assert!(err.contains("typestr must be struct/union"), "{err}");
}

#[test]
fn without_kencc_typestr_is_an_identifier() {
    let (ok, err) = check("c", RECORDS, &[]);
    assert!(!ok);
    assert!(err.contains("expected `;`, found `struct`"), "{err}");
}
