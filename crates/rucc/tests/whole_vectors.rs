//! A vector of four `int` or two `long` under an add, a subtract or a bitwise operator is one
//! instruction on x86-64 rather than one per lane.
//!
//! tamnd/rucc#2320. The front end used to walk every operator over a vector a lane at a time
//! through memory, which is what salsa20 and every other `emmintrin.h` program pays for. These
//! run a program that mixes the whole vector operators with reads and writes of single lanes, at
//! every level, so that the two kinds of access are seen to be the same bytes, and read the
//! assembly to see the instructions are the vector ones.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-whole-{}-{what}", std::process::id()));
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

/// Every operator at both shapes, a compound assignment, a lane written between two whole
/// operations, and a vector reached through a pointer that is not aligned. `main` gives back
/// which check failed, or zero.
const PROGRAM: &str = r"
typedef int v4si __attribute__((vector_size(16)));
typedef unsigned v4su __attribute__((vector_size(16)));
typedef long long v2di __attribute__((vector_size(16)));
typedef int v4si_u __attribute__((vector_size(16), aligned(1)));

__attribute__((noinline)) v4si mix(v4si a, v4si b) {
  v4si c = a + b;
  c[2] = 100;
  c = c - a;
  c ^= b;
  return (c & a) | b;
}

__attribute__((noinline)) v2di wide(v2di a, v2di b) {
  v2di c = a - b;
  c += a;
  c[0] += 1;
  return c ^ (a | b);
}

__attribute__((noinline)) void through(char *bytes) {
  v4si_u *p = (v4si_u *)(bytes + 1);
  *p = *p + *p;
}

int main(void) {
  v4si a = { 1, -2, 0x7fffffff, 40 };
  v4si b = { 5, 6, 7, -8 };
  v4si m = mix(a, b);
  for (int i = 0; i < 4; i++) {
    int c = a[i] + b[i];
    if (i == 2)
      c = 100;
    c = c - a[i];
    c ^= b[i];
    if (m[i] != ((c & a[i]) | b[i]))
      return 1 + i;
  }
  v4su u = { 0xffffffffu, 1, 2, 3 };
  v4su one = { 1, 1, 1, 1 };
  u = u + one;
  if (u[0] != 0 || u[3] != 4)
    return 5;
  v2di x = { 1LL << 40, -3 };
  v2di y = { 7, 1LL << 62 };
  v2di w = wide(x, y);
  for (int i = 0; i < 2; i++) {
    long long c = x[i] - y[i];
    c += x[i];
    if (i == 0)
      c += 1;
    if (w[i] != (c ^ (x[i] | y[i])))
      return 6 + i;
  }
  char bytes[20];
  for (int i = 0; i < 20; i++)
    bytes[i] = (char)i;
  through(bytes);
  for (int i = 0; i < 20; i++)
    if (bytes[i] != ((i >= 1 && i < 17) ? 2 * i : i))
      return 8;
  return 0;
}
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_whole_vector_operator_gives_the_answer_its_lanes_would() {
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

/// The operators are the vector instructions, at both shapes.
#[test]
fn a_whole_vector_operator_is_one_instruction() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    for inst in ["paddd", "psubd", "pxor", "pand", "por", "paddq", "psubq", "movdqu"] {
        assert!(asm.contains(inst), "no {inst} in\n{asm}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
