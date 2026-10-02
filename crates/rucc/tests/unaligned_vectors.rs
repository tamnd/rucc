//! A sixteen byte vector read or written through a type that is less aligned than sixteen bytes
//! is an unaligned access, at every level.
//!
//! Since #2635 a vector of sixteen bytes is a value in one vector register, and the rules for
//! reading and writing such a value used `movaps`, which faults on an address that is not a
//! multiple of sixteen. That is the right instruction for a `_Float128` and the wrong one for
//! `_mm_loadu_si128`, which reads through `__m128i_u`, a type aligned to one byte, and is how
//! every SSE2 program reads bytes it did not lay out itself. Postgres segfaulted in initdb this
//! way: the radix tree in `tidstore.c` searches a node's sixteen chunks with that load, and the
//! chunks start two bytes into the node.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-unaligned-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A load and a store through the under-aligned type, the way the SSE2 header writes them, at
/// every offset into a buffer that is itself aligned, so that fifteen of the sixteen are not.
/// `main` gives back which check failed, or zero.
const PROGRAM: &str = r"
typedef long long m128i __attribute__((vector_size(16)));
typedef long long m128i_u __attribute__((vector_size(16), aligned(1)));

static inline m128i load(const void *p) { return *(const m128i_u *)p; }
static inline void store(void *p, m128i v) { *(m128i_u *)p = v; }

__attribute__((noinline)) m128i read_at(const unsigned char *p) { return load(p); }
__attribute__((noinline)) void write_at(unsigned char *p, m128i v) { store(p, v); }

struct node { unsigned char kind, count, chunks[16]; void *children[16]; };

__attribute__((noinline)) int first(struct node *n) {
  m128i v = load(n->chunks);
  return (int)(v[0] & 0xff);
}

int main(void) {
  _Alignas(16) unsigned char in[48], out[48];
  for (int i = 0; i < 48; i++)
    in[i] = (unsigned char)(i * 7 + 3);
  for (int at = 0; at < 16; at++) {
    for (int i = 0; i < 48; i++)
      out[i] = 0;
    write_at(out + at, read_at(in + at));
    for (int i = 0; i < 48; i++)
      if (out[i] != (i >= at && i < at + 16 ? in[i] : 0))
        return 1 + at;
  }
  struct node n = { 0 };
  n.chunks[0] = 42;
  if (first(&n) != 42)
    return 20;
  return 0;
}
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn an_under_aligned_vector_is_read_and_written_at_any_address() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert_eq!(out.status.code(), Some(0), "{level}: a check failed");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// And on every host, the only `movaps` that touches memory is one the frame wrote, into a slot it
/// aligned itself, on both targets that share the rules.
#[test]
fn the_aligned_move_reaches_only_the_frame() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for target in ["x86_64-unknown-linux-gnu", "i686-unknown-linux-gnu"] {
        for level in ["-O0", "-O2"] {
            let (ok, said) = run(
                &dir,
                &[&format!("--target={target}"), "-msse2", level, "-S", "a.c", "-o", "a.s"],
            );
            assert!(ok, "{target} {level}: {said}");
            let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
            for line in asm.lines().map(str::trim).filter(|line| line.starts_with("movaps")) {
                let frame = ["(%rsp)", "(%rbp)", "(%esp)", "(%ebp)"];
                assert!(
                    !line.contains('(') || frame.iter().any(|base| line.contains(base)),
                    "{target} {level}: {line} in\n{asm}"
                );
            }
            if target.starts_with("x86_64") {
                assert!(asm.contains("movups"), "{target} {level}: no movups in\n{asm}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
