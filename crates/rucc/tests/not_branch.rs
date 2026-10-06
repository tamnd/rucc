//! `if (!flag)` on a `_Bool` is a test of the flag and a jump the other way, and not an `xor` of
//! it with one and a test of that.
//!
//! tamnd/rucc#1994. Postgres' `XidInMVCCSnapshot` tests `!snapshot->suboverflowed` and
//! `!snapshot->takenDuringRecovery` for every tuple it looks at, and each was a load, an
//! `xorb $1`, a `testb` and a jump, where gcc writes a test and a jump.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-not-branch-{}-{what}", std::process::id()));
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

/// Branches on a negated flag, one of them with a hint on it and one where the negation is used
/// again after the branch, and a `main` that prints what each gives for every flag there is.
const PROGRAM: &str = r#"
int printf(const char *, ...);

typedef struct { int n; _Bool done, lost; } S;

__attribute__((noinline)) int hit(int x) { return x * 3; }

__attribute__((noinline)) int open_one(S *s) {
  if (!s->done)
    return hit(s->n);
  return 7;
}

__attribute__((noinline)) int both(S *s, int x) {
  if (!s->done) {
    if (!__builtin_expect(s->lost, 0))
      return hit(x);
    return -1;
  }
  return 9;
}

__attribute__((noinline)) int kept(_Bool b, int x) {
  _Bool not = !b;
  if (not)
    x += hit(x);
  return x + not;
}

int main(void) {
  for (int i = 0; i < 4; i++) {
    S s = {i + 5, i & 1, (i >> 1) & 1};
    printf("%d %d %d\n", open_one(&s), both(&s, i), kept(i & 1, i));
  }
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_branch_on_a_negated_flag_gives_the_same_answers() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(got, "15 0 1\n7 9 1\n21 -1 9\n7 9 3\n", "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lines of one function in an assembly listing, trimmed, up to its `.size`.
fn body<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    let start = asm.find(&format!("\n{name}:")).expect("the function is there");
    let end = asm[start..].find(&format!(".size\t{name}")).map_or(asm.len(), |at| start + at);
    asm[start..end].lines().map(str::trim).filter(|line| !line.is_empty()).collect()
}

/// No `xor` is left in front of the branches at `-O0` or at `-O2`.
#[test]
fn a_branch_on_a_negated_flag_tests_the_flag() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O2"] {
        let (ok, said) =
            run(&dir, &["--target=x86_64-unknown-linux-gnu", level, "-S", "a.c", "-o", "a.s"]);
        assert!(ok, "{level}: {said}");
        let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
        for name in ["open_one", "both"] {
            let lines = body(&asm, name);
            assert!(
                !lines.iter().any(|line| line.starts_with("xor")),
                "{level} {name}: {lines:#?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
