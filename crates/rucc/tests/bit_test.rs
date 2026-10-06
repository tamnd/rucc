//! A bit asked about with `if (flags & MASK)` is a `test` of the register on x86-64, and the
//! register is not copied for an `and` to overwrite.
//!
//! tamnd/rucc#1994. Postgres asks bits of a tuple's `t_infomask` everywhere, and where the mask
//! was still wanted after the question rucc wrote `movq %rax, %rcx` and `andl $16, %ecx` in front
//! of the jump, where gcc writes `testb $16, %al`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-bit-test-{}-{what}", std::process::id()));
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

/// Bits asked about at every width, kept as a value and branched on, with the mask still wanted
/// afterwards, and a `main` that asks each of them about a spread of numbers.
const PROGRAM: &str = r#"
int printf(const char *, ...);

__attribute__((noinline)) int kind(unsigned short info, int a, int b) {
  if (info & 0x10) return a;
  if (info & 0x800) return b;
  return info;
}

__attribute__((noinline)) long wide(long flags, long y) {
  long r = 0;
  if (flags & 0x40000000L) r += y;
  if (!(flags & -8L)) r += 3;
  return r + flags;
}

__attribute__((noinline)) int byte(signed char c, unsigned u) {
  return ((c & 0x80) != 0) + ((u & 4) == 0) * 2 + ((u & 0x80000000u) != 0) * 4;
}

int main(void) {
  for (int i = 0; i < 40; i++) {
    unsigned v = (unsigned) i * 2654435761u;
    printf("%d %ld %ld %d\n", kind((unsigned short) v, i, -i), wide((long) v << (i % 33), i),
           wide(i - 20, 7), byte((signed char) v, v));
  }
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_bit_asked_with_a_test_gives_the_same_answers() {
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

/// At `-O2` the two questions `kind` asks are tests of the one register and there is no `and`.
#[test]
fn a_bit_asked_of_a_register_still_wanted_is_a_test() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    let start = asm.find("\nkind:").expect("the function is there");
    let end = asm[start..].find("\nwide:").map_or(asm.len(), |at| start + at);
    let body = &asm[start..end];
    let lines: Vec<&str> = body.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    assert!(!lines.iter().any(|line| line.starts_with("and")), "{body}");
    assert!(lines.iter().any(|line| line.starts_with("test") && line.contains("$16,")), "{body}");
    assert!(lines.iter().any(|line| line.starts_with("test") && line.contains("$2048,")), "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}
