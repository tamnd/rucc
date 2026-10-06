//! A vector with one value in every lane, `_mm_set1_epi32` or `(__v2di){a, a}`, is the value moved
//! into a vector register and copied to the other lanes with one `pshufd`, on x86-64.
//!
//! tamnd/rucc#1994. Postgres' `pg_lfind32` starts with `vector32_broadcast(key)`, which is
//! `_mm_set1_epi32`, and `XidInMVCCSnapshot` calls it twice for every tuple it checks. gcc writes
//! a `movd` and a `pshufd` for it, and rucc wrote the four lanes one at a time over a zero, which
//! was sixteen instructions.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-broadcast-{}-{what}", std::process::id()));
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

/// Vectors of one value in every lane, one with a different value in one lane, a search written
/// the way `pg_lfind32` is, and a `main` that prints what each gives.
const PROGRAM: &str = r#"
int printf(const char *, ...);

typedef int v4si __attribute__((vector_size(16)));
typedef long long v2di __attribute__((vector_size(16)));

__attribute__((noinline)) v4si four(int a) { return (v4si){a, a, a, a}; }

__attribute__((noinline)) v2di two(long long a) { return (v2di){a, a}; }

__attribute__((noinline)) v4si three(int a, int b) { return (v4si){a, a, b, a}; }

__attribute__((noinline)) int find(int key, const v4si *base, int n) {
  v4si keys = (v4si){key, key, key, key};
  for (int i = 0; i < n; i++) {
    v4si eq = keys == base[i];
    if (eq[0] | eq[1] | eq[2] | eq[3])
      return 1;
  }
  return 0;
}

int main(void) {
  int ints[] = {0, 1, -1, 0x7fffffff, (int) 0x80000000, 12345};
  for (int i = 0; i < 6; i++) {
    v4si v = four(ints[i]), w = three(ints[i], ~ints[i]);
    printf("%d %d %d %d | %d %d %d %d\n", v[0], v[1], v[2], v[3], w[0], w[1], w[2], w[3]);
  }
  long long longs[] = {0, -1, 0x123456789abcdefLL, (long long) 0x8000000000000000ULL};
  for (int i = 0; i < 4; i++) {
    v2di v = two(longs[i]);
    printf("%lld %lld\n", v[0], v[1]);
  }
  v4si xids[9];
  for (int i = 0; i < 9; i++)
    xids[i] = (v4si){i * 7919, i * 7919 + 1, i * 7919 + 2, i * 7919 + 3};
  for (int key = 0; key < 9 * 7919 + 8; key += 997)
    printf("%d", find(key, xids, 9));
  printf("\n");
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn one_value_in_every_lane_gives_the_same_answers() {
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

/// The lines of one function in an assembly listing, trimmed, up to its `.size`.
fn body<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    let start = asm.find(&format!("\n{name}:")).expect("the function is there");
    let end = asm[start..].find(&format!(".size\t{name}")).map_or(asm.len(), |at| start + at);
    asm[start..end].lines().map(str::trim).filter(|line| !line.is_empty()).collect()
}

/// At `-O2` four `int` of one value is a `movd` and a `pshufd $0`, two `long` is a `movq` and a
/// `pshufd`, and neither writes a lane on its own. One lane of another value is still written
/// lane by lane.
#[test]
fn one_value_in_every_lane_is_one_pshufd() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    let lane_by_lane =
        |line: &&str| ["movss", "movsd", "punpcklqdq"].iter().any(|it| line.starts_with(it));
    for name in ["four", "two"] {
        let lines = body(&asm, name);
        assert_eq!(
            lines.iter().filter(|line| line.starts_with("pshufd")).count(),
            1,
            "{name}: {lines:#?}"
        );
        assert!(!lines.iter().any(lane_by_lane), "{name}: {lines:#?}");
    }
    assert!(body(&asm, "four").iter().any(|line| line.starts_with("pshufd\t$0,")));
    assert!(body(&asm, "three").iter().any(lane_by_lane));
    let _ = std::fs::remove_dir_all(&dir);
}
