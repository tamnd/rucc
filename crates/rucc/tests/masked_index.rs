//! An array indexed by `(int) (x & 15)` is indexed by `x & 15`, with no sign extension in between.
//!
//! tamnd/rucc#1994. Postgres' `pfree` finds the methods of a chunk's context with
//! `mcxt_methods[header & MEMORY_CONTEXT_METHODID_MASK]`, where the index goes through an enum, and
//! x86-64 wrote a `movslq` between the `andq $15` and the multiply on every call.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-masked-{}-{what}", std::process::id()));
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

/// A table of functions picked by the low four bits of a header, the way `pfree` picks one, an
/// index whose sign bit is not known, and a `main` that prints what each gives.
const PROGRAM: &str = r#"
int printf(const char *, ...);

typedef unsigned long long u64;
typedef struct { long (*one)(long); long (*two)(long); } Methods;

static long add(long x) { return x + 1; }
static long sub(long x) { return x - 1; }
static long neg(long x) { return -x; }

static const Methods table[16] = {
  [0] = {add, sub}, [3] = {sub, neg}, [15] = {neg, add},
};

__attribute__((noinline)) long pick(const u64 *header, long x) {
  return table[(int) (*header & 15)].two(x);
}

__attribute__((noinline)) int wide(const u64 *header) {
  return (int) (*header & 0xffffffffu) < 0;
}

int main(void) {
  u64 headers[] = {0x10, 0x23, 0xffffffffffffff0fULL, 0x80000000ULL, 0x7fffffffULL};
  for (int i = 0; i < 3; i++)
    printf("%ld ", pick(&headers[i], 40 + i));
  for (int i = 0; i < 5; i++)
    printf("%d", wide(&headers[i]));
  printf("\n");
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn an_index_masked_to_four_bits_gives_the_same_answers() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(got, "39 -41 43 00110\n", "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lines of one function in an assembly listing, trimmed, up to its `.size`.
fn body<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    let start = asm.find(&format!("\n{name}:")).expect("the function is there");
    let end = asm[start..].find(&format!(".size\t{name}")).map_or(asm.len(), |at| start + at);
    asm[start..end].lines().map(str::trim).filter(|line| !line.is_empty()).collect()
}

/// At `-O2` the masked index has no `movslq`.
#[test]
fn an_index_masked_to_four_bits_is_not_sign_extended() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    let lines = body(&asm, "pick");
    assert!(lines.iter().any(|line| line.starts_with("and")), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.starts_with("movs")), "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}
