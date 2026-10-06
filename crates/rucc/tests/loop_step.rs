//! A loop that walks a pointer and tests it at the bottom goes round on one branch on x86-64.
//!
//! tamnd/rucc#1994. When the exit test was rewritten to ask the pointer, the pointer was stepped
//! behind the test, so every turn was a branch out, an add and a jump back. Stepped in front of the
//! test it is an add, a compare and a branch back, which is what gcc writes.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-loop-step-{}-{what}", std::process::id()));
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

/// Three loops over an array whose length is handed in, and a `main` that checks them at lengths
/// from nothing to a few. It gives back which check failed, or zero. The third starts where it is
/// told to, so its exit test stays on the counter and only the pointer's step can move.
const PROGRAM: &str = r"
__attribute__((noinline)) void scale(int *a, int n, int k) {
  for (int i = 0; i < n; i++)
    a[i] *= k;
}

__attribute__((noinline)) long total(const long *a, int n) {
  long s = 0;
  for (int i = 0; i < n; i++)
    s += a[i] * 3;
  return s;
}

__attribute__((noinline)) void clear(long *a, int from, int n) {
  for (int i = from; i < n; i++)
    a[i] = 0;
}

int main(void) {
  for (int n = -2; n < 9; n++) {
    int a[10];
    long b[10];
    for (int i = 0; i < 10; i++) {
      a[i] = i + 1;
      b[i] = i + 1;
    }
    scale(a, n, 5);
    for (int i = 0; i < 10; i++)
      if (a[i] != (i < n ? 5 * (i + 1) : i + 1))
        return 1;
    long want = 0;
    for (int i = 0; i < n; i++)
      want += (i + 1) * 3;
    if (total(b, n) != want)
      return 2;
    clear(b, 1, n);
    for (int i = 0; i < 10; i++)
      if (b[i] != (i >= 1 && i < n ? 0 : i + 1))
        return 3;
  }
  return 0;
}
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_loop_stepped_in_front_of_its_test_writes_what_it_wrote_before() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert_eq!(out.status.code(), Some(0), "{level}: a check failed");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `scale` and `clear` at `-O2` have no jump that is not a branch on a condition: the guard leaves
/// early and the loop goes back round on the branch that tests it.
#[test]
fn the_loop_goes_round_on_the_branch_that_tests_it() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    for (name, next) in [("scale:", "total:"), ("clear:", "main:")] {
        let start = asm.find(name).expect("the function is there");
        let end = asm[start..].find(next).map_or(asm.len(), |at| start + at);
        let body = &asm[start..end];
        let jumps = body.lines().filter(|line| line.trim_start().starts_with("jmp")).count();
        assert_eq!(jumps, 0, "a jump back round the loop in\n{body}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
