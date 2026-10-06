//! A value the allocator spilled is read out of the frame by the instruction that wants it, rather
//! than into a scratch register on the line in front of it, on x86-64 above `-O0`.
//!
//! tamnd/rucc#1994. The hot loop of Postgres' tuple deforming kept its bound on the stack and read
//! it back into `%r10` on every turn to compare against it. gcc compares against the stack.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-reload-fold-{}-{what}", std::process::id()));
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

/// A loop with more running sums than x86-64 has registers for, so the bound and the pointer are
/// spilled and read back on every turn. `main` prints what it comes to over a few lengths.
const PROGRAM: &str = r#"
#include <stdio.h>

__attribute__((noinline)) long mix(const long *a, int n, long k) {
  long s0 = 0, s1 = 0, s2 = 0, s3 = 0, s4 = 0, s5 = 0, s6 = 0, s7 = 0, s8 = 0, s9 = 0;
  for (int i = 0; i < n; i++) {
    long v = a[i];
    s0 += v; s1 ^= v + s0; s2 += v * s1; s3 |= v - s2; s4 += s3 ^ v;
    s5 ^= s4 + v; s6 += s5 & v; s7 -= s6 | v; s8 += s7 * k; s9 ^= s8 + k;
  }
  return s0 + s1 + s2 + s3 + s4 + s5 + s6 + s7 + s8 + s9;
}

int main(void) {
  long a[40];
  for (int i = 0; i < 40; i++)
    a[i] = (long) i * 2654435761L ^ 977;
  for (int n = -1; n <= 40; n += 3)
    printf("%d %ld\n", n, mix(a, n, n * 7 + 1));
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_reload_read_by_the_instruction_that_wanted_it_gives_the_same_answers() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let mut want = None;
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        let want = want.get_or_insert_with(|| got.clone());
        assert_eq!(&got, want, "{level} printed something -O0 did not");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// At `-O2` the loop compares its counter against the bound where the bound is in the frame, and
/// nothing is compared against a scratch register.
#[test]
fn the_loop_compares_against_the_frame() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    let start = asm.find("mix:").expect("the function is there");
    let end = asm[start..].find("main:").map_or(asm.len(), |at| start + at);
    let body = &asm[start..end];
    let cmp: Vec<&str> =
        body.lines().map(str::trim).filter(|line| line.starts_with("cmpl")).collect();
    assert!(
        cmp.iter().any(|line| line.contains("(%rsp)")),
        "no compare against the frame in\n{body}"
    );
    assert!(!cmp.iter().any(|line| line.contains("%r10") || line.contains("%r11")), "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}
