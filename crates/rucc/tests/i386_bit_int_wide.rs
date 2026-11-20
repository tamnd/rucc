//! A `_BitInt(128)` inside a function on i386, which is four registers wide there. gcc 14 has the
//! type on i386, and libgcc's `floatbitinttf.c`, `fixtfbitint.c`, `floatbitintxf.c` and
//! `fixxfbitint.c` use it as the integer of `TImode`, which i386 does not have. Each of them
//! stopped with "no rule lowers a `zext` producing a `i128`", in the NetBSD build of libgcc for
//! i386 (tamnd/rucc-bsd#3).

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The operations those four files do on the type: widenings from a word and from a `long long`,
/// shifts by a constant and by a count, an add, a subtract, an and, an or, a compare and a
/// narrowing, all on values that live and die inside the function.
const SOURCE: &str = "\
typedef unsigned _BitInt(128) u128;
typedef _BitInt(128) s128;
unsigned top(unsigned long long a, unsigned long long b, unsigned n) {
  u128 x = ((u128)a << 64) | b;
  x = (x << (n & 127)) + (u128)b;
  return (unsigned)(x >> 96);
}
int sign(long long a, unsigned n) {
  s128 x = (s128)a << 40;
  x >>= n & 63;
  return (int)(x >> 64);
}
int less(unsigned long long a, unsigned long long b) {
  u128 x = (u128)a << 64, y = (u128)b << 63;
  return x < y;
}
unsigned borrow(unsigned long long a, unsigned b) {
  u128 x = ((u128)a << 64) - b;
  return (unsigned)(x >> 32) & (unsigned)(x >> 100);
}
unsigned loop(const unsigned *w, int n) {
  u128 acc = 0;
  for (int i = 0; i < n; i++)
    acc = (acc << 32) | w[i];
  return (unsigned)(acc >> 64) ^ (unsigned)acc;
}
";

fn compile(what: &str, source: &str, level: &str) -> (bool, String, String) {
    // The tests in this file run on threads of one process, so each takes a directory of its own.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-bit-int-{}-{what}-{level}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-asynchronous-unwind-tables", "-S"])
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let listing = String::from_utf8(out.stdout).expect("a listing is text");
    (out.status.success(), listing, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Every function lowers at every level, in words, with nothing called.
#[test]
fn a_bit_int_of_four_words_inside_a_function_is_split_into_words() {
    for level in ["-O0", "-O1", "-O2", "-Os"] {
        let (ok, listing, said) = compile("inside", SOURCE, level);
        assert!(ok, "{level}: {said}");
        assert!(!listing.contains("call"), "{level}: {listing}");
        for name in ["top", "sign", "less", "borrow", "loop"] {
            assert!(listing.contains(&format!("\n{name}:\n")), "{level}: {name}: {listing}");
        }
    }
}

/// A `_BitInt(72)` is twelve bytes on i386, three words, and is held in four here. A load or a
/// store of the four would reach past the object, so it is still refused rather than written.
#[test]
fn a_bit_int_of_three_words_in_memory_is_still_refused() {
    let source = "void put(_BitInt(72) *p, long long v) { *p = v; }\n";
    for level in ["-O0", "-O2"] {
        let (ok, _, said) = compile("three", source, level);
        assert!(!ok, "{level}: a store of sixteen bytes into twelve: {said}");
        assert!(said.contains("cannot generate code for 'put'"), "{level}: {said}");
    }
}

/// Division at this width is a call to a `ti` routine on x86-64, and libgcc for i386 has none, so
/// it is still refused by name rather than turned into a call to nothing.
#[test]
fn a_divide_at_four_words_is_still_refused() {
    let source = "unsigned f(unsigned long long a, unsigned b) {\n\
                  unsigned _BitInt(128) x = (unsigned _BitInt(128))a << 64;\n\
                  return (unsigned)(x / b);\n}\n";
    let (ok, listing, said) = compile("divide", source, "-O0");
    assert!(!ok, "{said}");
    assert!(!listing.contains("__udivti3"), "{listing}");
}
