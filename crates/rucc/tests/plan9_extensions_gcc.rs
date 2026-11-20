//! `-fplan9-extensions` against what GCC 16 says, end to end.
//!
//! Design: tamnd/rucc#4151, and 06.10 of the rucc-plan9 plan.
//!
//! Each case was compiled by GCC 16.1 with `-fsyntax-only`. The test gives the messages that GCC
//! wrote, without the place and the option name, and rucc must write the same messages. GCC
//! spells a pointer to a typedef with the typedef name and rucc with the tag, so the messages
//! that name such a type are compared up to the type.

use std::path::PathBuf;
use std::process::Command;

/// The members of a `Lock` are reached through `Big`, and a `Big *` converts to a `Lock *`.
const REACHED: &str = "typedef struct Lock Lock;\n\
                       typedef struct Ref Ref;\n\
                       typedef struct Big Big;\n\
                       struct Lock { int key; };\n\
                       struct Ref { Lock; long ref; };\n\
                       struct Big { long pad; Ref; };\n\
                       void lock(Lock *l);\n\
                       Lock *ret(Big *b) { return b; }\n\
                       void assign(Big *b) { Lock *l; l = b; Lock *m = b; lock(l); lock(m); }\n\
                       int name(Big *b) { return b->Ref.Lock.key + b->Lock.key + b->key; }\n";

/// Two anonymous members that both hold a `Lock`.
const TWICE: &str = "typedef struct Lock Lock;\n\
                     struct Lock { int key; };\n\
                     typedef struct A { Lock; } A;\n\
                     typedef struct B { Lock; } B;\n\
                     struct Both { A; B; };\n";

/// A comparison does not convert, and a cast is always allowed.
const COMPARED: &str = "typedef struct Lock Lock;\n\
                        struct Lock { int key; };\n\
                        struct Ref { Lock; long ref; };\n\
                        void *v(struct Ref *r) { return (Lock *)r; }\n\
                        int cmp(struct Ref *r, Lock *l) { return r == l; }\n";

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-plan9-gcc-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The messages of rucc for `source` with `flags`, without the place and the code.
fn messages(what: &str, flags: &[&str], source: &str) -> Vec<String> {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fsyntax-only"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter_map(|line| line.split_once(": ").map(|(_, rest)| rest))
        .map(|rest| rest.rsplit_once(" [E").map_or(rest, |(text, _)| text).to_owned())
        .collect()
}

#[test]
fn the_members_and_the_conversions_are_silent() {
    assert_eq!(messages("reached", &["-fplan9-extensions"], REACHED), Vec::<String>::new());
}

#[test]
fn without_the_option_gcc_refuses_them() {
    let want = [
        "warning: declaration does not declare anything",
        "warning: declaration does not declare anything",
        "error: returning 'struct Big *' from a function with incompatible return type 'struct Lock *'",
        "error: assignment to 'struct Lock *' from incompatible pointer type 'struct Big *'",
        "error: initialization of 'struct Lock *' from incompatible pointer type 'struct Big *'",
        "error: 'struct Big' has no member named 'Ref'",
        "error: 'struct Big' has no member named 'Lock'",
        "error: 'struct Big' has no member named 'key'",
    ];
    assert_eq!(messages("refused", &[], REACHED), want);
}

#[test]
fn a_typedef_name_reached_twice_is_a_duplicate_member() {
    let want = ["error: duplicate member 'key'", "error: duplicate member 'Lock'"];
    assert_eq!(messages("twice", &["-fplan9-extensions"], TWICE), want);
    // Under `-fms-extensions` the typedef name is not a member name.
    assert_eq!(messages("twice-ms", &["-fms-extensions"], TWICE), want[..1]);
}

#[test]
fn a_comparison_does_not_convert() {
    let want = ["warning: comparison of distinct pointer types lacks a cast"];
    assert_eq!(messages("compared", &["-fplan9-extensions"], COMPARED), want);
}
