//! `-flax-vector-conversions`, which NetBSD gives its NEON AES and ChaCha code.
//!
//! gcc lets a vector be assigned to a vector of the same size whose lanes differ when the flag is
//! given. Two vectors of integers convert with any count of lanes, and two vectors of floating
//! lanes convert when the counts are the same. A vector of integers and a vector of floating lanes
//! do not convert, with the flag or without it.

use std::path::PathBuf;
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-lax-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// Whether `source` compiles with `flags`, and what the compiler said.
fn compile(what: &str, source: &str, flags: &[&str]) -> (bool, String) {
    let dir = dir(what);
    let path = dir.join("a.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-netbsd", "-c", "-o"])
        .arg(dir.join("a.o"))
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// An assignment, an initializer, an argument and a return, each between vectors of sixteen bytes
/// whose lanes differ.
const LAX: &str = r"
typedef unsigned char u8x16 __attribute__((vector_size(16)));
typedef unsigned int u32x4 __attribute__((vector_size(16)));
typedef long long s64x2 __attribute__((vector_size(16)));
typedef float f32x4 __attribute__((vector_size(16)));
typedef int s32x4 __attribute__((vector_size(16)));
static u32x4 take(u32x4 x) { return x; }
u8x16 lax(u8x16 a, s64x2 b) {
    u32x4 w = a;
    w = b;
    s64x2 d = take(a);
    return w + (u32x4)d;
}
f32x4 same(f32x4 f) { s32x4 i = (s32x4)f; return (f32x4)i; }
";

#[test]
fn the_flag_is_taken_and_vectors_of_one_size_convert() {
    for flags in [&["-flax-vector-conversions"][..], &["-fno-lax-vector-conversions"], &[]] {
        let (ok, said) = compile("lax", LAX, flags);
        assert!(ok, "{flags:?}: {said}");
    }
}

#[test]
fn a_vector_of_integers_and_one_of_floats_still_do_not_convert() {
    let source = "typedef unsigned int u32x4 __attribute__((vector_size(16)));\n\
        typedef float f32x4 __attribute__((vector_size(16)));\n\
        u32x4 f(f32x4 a) { return a; }\n";
    let (ok, said) = compile("mixed", source, &["-flax-vector-conversions"]);
    assert!(!ok, "a vector of floats became a vector of integers: {said}");
}
