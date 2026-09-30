//! What `__attribute__((may_alias))` does to the aliasing node an access carries, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4 and `spec/08-ir.md` section 8.9.
//!
//! An access through a type that says `may_alias` may touch an object of any type, the way an
//! access through a character type may, so what it carries is the character type's node, which is
//! the root of the tree and conflicts with everything. `<emmintrin.h>`'s `__m128i_u`, the kernel's
//! `get_unaligned` and every hand written `memcpy` that reads a word at a time are written against
//! that. An access that kept the node for the type underneath is one the optimizer is entitled to
//! move past a store through any other type, which is wrong code rather than slow code.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const TARGET: &str = "x86_64-linux-gnu";

fn fixture(what: &str, source: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-may-alias-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The IR for that source at `-O0`, which is where each access gets its node and nothing has moved.
fn ir(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O0", "-fstrict-aliasing", "--emit=ir", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused it:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The name of the aliasing node the one store in function `name` carries.
fn stored_through(text: &str, name: &str) -> String {
    let open = format!("func @{name}(");
    let store = text
        .lines()
        .map(str::trim)
        .skip_while(|line| !line.starts_with(&open))
        .take_while(|line| *line != "}")
        .find(|line| line.starts_with("store") && line.contains("tbaa !"))
        .unwrap_or_else(|| panic!("no store with a node in {name}:\n{text}"));
    let index = store.split("tbaa !").nth(1).expect("a node").split([',', ' ']).next().unwrap();
    let head = format!("!{index} = tbaa \"");
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(&head))
        .unwrap_or_else(|| panic!("no node !{index}:\n{text}"));
    line[head.len()..].split('"').next().expect("a name").to_owned()
}

#[test]
fn a_store_through_a_typedef_that_may_alias_carries_the_character_node() {
    let text = ir(
        "typedef",
        "\
typedef unsigned int __attribute__((may_alias)) u32_a;
typedef unsigned int plain;
void through(u32_a *p) { *p = 1; }
void around(plain *p) { *p = 1; }
",
    );
    assert_eq!(stored_through(&text, "through"), "char", "{text}");
    // The type underneath is left alone: only the spelling that said it may alias.
    assert_eq!(stored_through(&text, "around"), "int", "{text}");
}

#[test]
fn the_attribute_after_the_typedef_name_says_the_same() {
    let text = ir(
        "after",
        "\
typedef long word __attribute__((__may_alias__));
void through(word *p) { *p = 1; }
",
    );
    assert_eq!(stored_through(&text, "through"), "char", "{text}");
}

#[test]
fn a_member_of_a_record_that_may_alias_carries_the_character_node() {
    let text = ir(
        "record",
        "\
struct __attribute__((may_alias)) unaligned { unsigned int x; };
typedef struct { unsigned int x; } __attribute__((may_alias)) after_t;
struct plain { unsigned int x; };
typedef struct plain __attribute__((may_alias)) plain_a;
void tagged(struct unaligned *p) { p->x = 1; }
void named(after_t *p) { p->x = 1; }
void renamed(plain_a *p) { p->x = 1; }
void ordinary(struct plain *p) { p->x = 1; }
",
    );
    assert_eq!(stored_through(&text, "tagged"), "char", "{text}");
    assert_eq!(stored_through(&text, "named"), "char", "{text}");
    assert_eq!(stored_through(&text, "renamed"), "char", "{text}");
    assert_eq!(stored_through(&text, "ordinary"), "int", "{text}");
}
