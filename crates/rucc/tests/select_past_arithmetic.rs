//! A select whose value is worked out after the comparison it chooses by is the comparison and a
//! conditional move, on x86-64, with the comparison moved down past the arithmetic.
//!
//! tamnd/rucc#1994. `if (!(x & MASK)) r = -r` is a select on a comparison, and the `neg` comes
//! after the comparison and writes the condition state, so rucc kept the comparison's answer in a
//! byte with `sete` and tested the byte again in front of the `cmov`. gcc asks the question after
//! the `neg` instead. Postgres' `_bt_compare` ends with one of these for every key it compares.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-select-past-{}-{what}", std::process::id()));
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

/// Three selects whose values are arithmetic after the comparison, and a `main` that prints what
/// each gives.
const PROGRAM: &str = r#"
int printf(const char *, ...);

__attribute__((noinline)) int flip(int x, int r) {
  if (!(x & 2))
    r = -r;
  return r;
}

__attribute__((noinline)) int same(int x, int y, int r) {
  if (x == y)
    r ^= 5;
  return r;
}

__attribute__((noinline)) long lower(long a, long b, long c) { return a < b ? c ^ 7 : c & 9; }

int main(void) {
  int xs[] = {0, 2, 3, -1, 4, 0x7ffffffe};
  for (int i = 0; i < 6; i++)
    printf("%d %d %d %ld %ld\n", flip(xs[i], 10 + i), same(xs[i], 2, i), same(xs[i], xs[i], -i),
           lower(xs[i], 1, 100 + i), lower(xs[i], -5, -100 - i));
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_select_past_arithmetic_gives_the_same_answers() {
    const WANT: &str = "\
-10 0 5 99 8\n\
11 4 -6 1 9\n\
12 2 -5 0 8\n\
13 3 -8 96 9\n\
-14 4 -7 8 8\n\
15 5 -2 9 1\n";
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        assert_eq!(String::from_utf8_lossy(&out.stdout), WANT, "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lines of one function in an assembly listing, trimmed, up to its `.size`.
fn body<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    let start = asm.find(&format!("\n{name}:")).expect("the function is there");
    let end = asm[start..].find(&format!(".size\t{name}")).map_or(asm.len(), |at| start + at);
    asm[start..end].lines().map(str::trim).filter(|line| !line.is_empty()).collect()
}

/// At `-O2` none of the three writes the answer to a byte or tests one, and each has one `cmov`.
#[test]
fn a_select_past_arithmetic_keeps_no_byte() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    for name in ["flip", "same", "lower"] {
        let lines = body(&asm, name);
        let byte = |line: &&str| line.starts_with("set") || line.starts_with("testb");
        assert!(!lines.iter().any(byte), "{name}: {lines:#?}");
        let moves = lines.iter().filter(|line| line.starts_with("cmov")).count();
        assert_eq!(moves, 1, "{name}: {lines:#?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
