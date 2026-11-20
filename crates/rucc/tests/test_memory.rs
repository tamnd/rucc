//! A bit asked of a field with `if (p->flags & MASK)` is a `test` of the byte in memory on x86-64,
//! with no load in front of it.
//!
//! tamnd/rucc#1994. Postgres asks bits of a tuple's sixteen bit `t_infomask` all through the
//! executor, and rucc read the field with `movzwl 4(%rdi), %eax` and tested the register, where gcc
//! writes `testb $8, 5(%rdi)` for the one byte the bit is in.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-test-memory-{}-{what}", std::process::id()));
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

/// Bits asked of fields of every width, in the low byte, in a higher one and across two, and a
/// `main` that asks them of a spread of records.
const PROGRAM: &str = r#"
int printf(const char *, ...);

struct tup {
  int id;
  unsigned short info;
  unsigned char kind;
  unsigned word;
  long long big;
};

__attribute__((noinline)) int hint(const struct tup *t, int a, int b) {
  if (t->info & 0x0800) return a;
  return b;
}

__attribute__((noinline)) int low(const int *p, int a, int b) {
  if (*p & 4) return a;
  return b;
}

__attribute__((noinline)) int mixed(const struct tup *t) {
  int r = 0;
  if (t->info & 0x0801) r += 1;
  if (!(t->kind & 0x80)) r += 2;
  if (t->word & 0x80000000u) r += 4;
  if (t->word & 0x00ff0000u) r += 8;
  if (t->big & 0x100) r += 16;
  if (t->big & -8LL) r += 32;
  if (!(t->info & 0x8000)) r += 64;
  return r;
}

int main(void) {
  struct tup t;
  for (int i = 0; i < 64; i++) {
    unsigned v = (unsigned) i * 2654435761u;
    t.id = i;
    t.info = (unsigned short) (v >> 7);
    t.kind = (unsigned char) (v >> 3);
    t.word = v;
    t.big = (long long) v << (i % 29);
    printf("%d %d %d\n", hint(&t, i, -i), low(&t.id, i, 3 * i), mixed(&t));
  }
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_bit_asked_of_memory_gives_the_same_answers() {
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

/// The lines of one function in what the compiler wrote, up to the label of the next.
fn body<'a>(asm: &'a str, name: &str, next: &str) -> Vec<&'a str> {
    let start = asm.find(&format!("\n{name}:")).expect("the function is there");
    let end = asm[start..].find(&format!("\n{next}:")).map_or(asm.len(), |at| start + at);
    asm[start..end].lines().map(str::trim).filter(|line| !line.is_empty()).collect()
}

/// At `-O2` the bit in the high byte of the sixteen bit field is a test of that byte, and the bit
/// of the `int` is a test of its first byte, with no load of either into a register.
#[test]
fn a_bit_asked_of_a_field_is_a_test_of_the_byte_it_is_in() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    // The test is the one line in each that reads the field.
    let hint = body(&asm, "hint", "low");
    let read: Vec<&&str> = hint.iter().filter(|line| line.contains("(%rdi)")).collect();
    assert_eq!(read, [&"testb\t$8, 5(%rdi)"], "{hint:#?}");
    let low = body(&asm, "low", "mixed");
    let read: Vec<&&str> = low.iter().filter(|line| line.contains("(%rdi)")).collect();
    assert_eq!(read, [&"testb\t$4, (%rdi)"], "{low:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}
