//! Calls through a pointer that can only be one function, which gcc makes calls to that function.
//!
//! The kernel reaches most of its implementations through `static const` tables of operations, and
//! where the table is the only one a call can read, gcc calls the function by name and then decides
//! whether to inline it like any other call. drivers/virtio/virtio_ring.c is built that way, and
//! rucc kept a call through a retpoline thunk in each place gcc had inlined `virtqueue_add_split`.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the listing is the same everywhere.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, under a directory of its own so two of these running at once do not collide.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-direct-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source at `-O2`.
fn asm(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// A member of a `static const` table read at a known place is the function it was initialized
/// to, and a function that small is then copied in.
#[test]
fn a_call_through_a_table_of_operations_is_a_call_to_the_function() {
    let source = "struct ops { int (*add)(int); int (*del)(int); };\n\
        static const struct ops split_ops;\n\
        static int add_split(int x) { return x * 7 + 3; }\n\
        static int del_split(int x) { return x - 1; }\n\
        static const struct ops split_ops = { .add = add_split, .del = del_split };\n\
        int use(int x) { return split_ops.add(x) + split_ops.del(x); }\n";
    let listing = asm("table", source);
    assert!(!listing.contains("call\t*"), "the call is still through a pointer:\n{listing}");
    assert!(!listing.contains("call\tadd_split"), "the call was not inlined:\n{listing}");
}

/// A pointer that is the address of a function declared elsewhere is a call by name.
#[test]
fn a_call_through_the_address_of_a_function_is_a_call_by_name() {
    let source = "int ext(int);\n\
        int use(int x) { int (*f)(int) = ext; return f(x) + 1; }\n";
    let listing = asm("named", source);
    assert!(listing.contains("call\text"), "the call is not by name:\n{listing}");
}
