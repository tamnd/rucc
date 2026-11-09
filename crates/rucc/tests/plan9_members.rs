//! Anonymous members in the Plan 9 dialect, end to end.
//!
//! Design: tamnd/rucc#4148 and tamnd/rucc#4151, and 06.2 of the rucc-plan9 plan.
//!
//! The 9front tree declares a member with no name and a typedef type, as in `struct Ref { Lock;
//! long ref; }`. Under `-fplan9-extensions` and `-fkencc`, the members of `Lock` are reached
//! through `Ref`, the typedef name names the member, and a `Ref *` converts to a `Lock *` with
//! the offset of the member added. On a Plan 9 target without these options, that conversion is
//! an error and not a warning, because it passes the wrong address.

use std::path::PathBuf;
use std::process::Command;

/// The source of the tests. `Ref` holds a `Lock` first, and `Big` holds a `Ref` after a `long`,
/// so that a conversion from `Big *` must add 8.
const SOURCE: &str = "typedef struct Lock Lock;\n\
                      typedef struct Ref Ref;\n\
                      typedef struct Big Big;\n\
                      struct Lock { int key; };\n\
                      struct Ref { Lock; long ref; };\n\
                      struct Big { long pad; Ref; };\n\
                      void lock(Lock *l);\n\
                      long incref(Ref *r) { lock(r); r->key = 1; r->Lock.key = 2; return ++r->ref; }\n\
                      long big(Big *b) { lock(b); b->Ref.Lock.key = 3; return b->key + b->ref; }\n";

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-plan9-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, what it wrote and what it said.
fn run(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    (out.status.success(), text(out.stdout), text(out.stderr))
}

/// The body of `function` in a listing, from its label to its `ret`.
fn body<'a>(listing: &'a str, function: &str) -> &'a str {
    let start = listing.find(&format!("\n{function}:")).expect("the function is in the listing");
    let rest = &listing[start..];
    &rest[..rest.find("\tret").expect("the function returns")]
}

#[test]
fn the_members_are_reached_and_the_pointer_points_at_the_member() {
    for option in ["-fplan9-extensions", "-fkencc"] {
        let (ok, out, err) =
            run(option, &["--target=x86_64-unknown-linux-gnu", "-O1", option], SOURCE);
        assert!(ok, "{option}: the compiler refused the source:\n{err}");
        assert!(err.is_empty(), "{option}: {err}");
        // `Lock` is first in `Ref`, so the pointer is passed as it is.
        let incref = body(&out, "incref");
        assert!(!incref.contains("leaq"), "{option}: {incref}");
        // `Ref` is at 8 in `Big`, so `lock(b)` gets `b + 8`, and `b->key` is at 8 too.
        let big = body(&out, "big");
        assert!(big.contains("leaq\t8(%rdi)"), "{option}: {big}");
        assert!(big.contains("movl\t$3, (%rbx)"), "{option}: {big}");
    }
}

#[test]
fn only_kencc_names_the_member_by_its_tag() {
    let source = "struct Lock { int key; };\n\
                  struct Ref { struct Lock; long ref; };\n\
                  int f(struct Ref *r) { return r->Lock.key; }\n";
    let (ok, _, err) = run("tag", &["--target=x86_64-unknown-linux-gnu", "-fkencc"], source);
    assert!(ok, "{err}");
    let (ok, _, err) =
        run("no-tag", &["--target=x86_64-unknown-linux-gnu", "-fplan9-extensions"], source);
    assert!(!ok);
    assert!(err.contains("'struct Ref' has no member named 'Lock'"), "{err}");
}

#[test]
fn a_plan_9_target_refuses_the_conversion_without_the_rule() {
    let (ok, _, err) =
        run("refused", &["--target=x86_64-plan9", "-fms-extensions", "-std=gnu89"], SOURCE);
    assert!(!ok);
    assert!(
        err.contains(
            "'struct Big *' converts to 'struct Lock *' only under -fplan9-extensions or -fkencc"
        ),
        "{err}"
    );
    // On another target it stays what gcc says under the same options, a warning.
    let (_, _, err) = run(
        "warned",
        &["--target=x86_64-unknown-linux-gnu", "-fms-extensions", "-std=gnu89"],
        SOURCE,
    );
    assert!(err.contains("warning: passing argument 1 of 'lock' from incompatible pointer type"));
}
