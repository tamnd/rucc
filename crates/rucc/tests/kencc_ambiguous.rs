//! kencc's ambiguous structure element under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4148, and 06.2 of the rucc-plan9 plan.
//!
//! kencc's `dotsearch` in `cc/sub.c` finds a member name first in the record itself, then by the
//! type of an anonymous member, and then in each anonymous member. When the name is found in two
//! places at the level where it is found first, kencc reports "ambiguous structure element". A
//! record with such a name is valid, and only the access to that name is an error.

use std::path::PathBuf;
use std::process::Command;

const TYPES: &str = "typedef struct A { int x; int a; } A;\n\
                     typedef struct B { int x; int b; } B;\n\
                     struct C { A; B; int c; };\n\
                     struct D { A; B; int x; };\n\
                     struct E { struct C; int e; };\n";

/// Whether the compiler took `body` after the types, and what it said.
fn check(what: &str, body: &str, flags: &[&str]) -> (bool, String) {
    let dir =
        std::env::temp_dir().join(format!("rucc-kencc-ambiguous-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, format!("{TYPES}{body}")).expect("the fixture can be written");
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
fn a_name_in_two_anonymous_members_is_ambiguous() {
    let (ok, err) = check("two", "int f(struct C *c) { return c->x; }\n", &["-fkencc"]);
    assert!(!ok);
    assert!(err.contains("ambiguous structure element: x"), "{err}");
}

#[test]
fn a_name_one_level_down_is_ambiguous_too() {
    let (ok, err) = check("deep", "int f(struct E *e) { return e->x; }\n", &["-fkencc"]);
    assert!(!ok);
    assert!(err.contains("ambiguous structure element: x"), "{err}");
}

#[test]
fn a_name_in_one_place_is_not_ambiguous() {
    let body = "int f(struct C *c, struct D *d, struct E *e)\n\
                {\n\
                \treturn c->a + c->b + c->c + c->A.x + c->B.x + d->x + e->a + e->e;\n\
                }\n";
    let (ok, err) = check("one", body, &["-fkencc"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(err.is_empty(), "{err}");
}

/// GCC refuses the record itself under `-fplan9-extensions`, as it does for any duplicate member.
#[test]
fn plan9_extensions_refuse_the_record() {
    let (ok, err) = check("gnu", "", &["-fplan9-extensions"]);
    assert!(!ok);
    assert!(err.contains("duplicate member 'x'"), "{err}");
}
