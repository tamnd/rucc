//! A loop over `int` arrays that runs a multiple of four times is done four at a time on x86-64
//! at `-O2`, and the ones where that would change the answer are left alone.
//!
//! tamnd/rucc#1994. The loop in `pg_checksum_block` is at the top of the Postgres profile against
//! `gcc -O2`, which does it in the vector registers. These run it next to a copy written so that
//! it cannot be vectorized, along with two loops whose accesses overlap from one iteration to the
//! next, and read the assembly to see the checksum loop is the vector one.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-vectorize-{}-{what}", std::process::id()));
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

/// The checksum of `src/include/storage/checksum_impl.h`, the same loop with the counter going
/// down so that it stays a lane at a time, and two loops that read what the iteration before
/// wrote. `main` gives back which check failed, or zero.
const PROGRAM: &str = r"
typedef unsigned int uint32;
#define N_SUMS 32
#define FNV_PRIME 16777619
#define ROWS 64

static const uint32 base[N_SUMS] = {
  0x5B1F36E9, 0xB8525960, 0x02AB50AA, 0x1DE66D2A, 0x79FF467A, 0x9BB9F8A3, 0x217E7CD2, 0x83E13D2C,
  0xF8D4474F, 0xE39EB970, 0x42C6AE16, 0x993216FA, 0x7B093B5D, 0x98DAFF3C, 0xF718902A, 0x0B1C9CDB,
  0xE58F764B, 0x187636BC, 0x5D7B3BB1, 0xE73DE7DE, 0x92BEC979, 0xCCA6C0B2, 0x304A0979, 0x85AA43D4,
  0x783125BB, 0x6CA8EAA2, 0xE407EAC6, 0x4B5CFC3E, 0x9FBF8C76, 0x15CA20BE, 0xF2CA9FD3, 0x959BD756 };

#define CHECKSUM_COMP(checksum, value) \
  do { uint32 t = (checksum) ^ (value); (checksum) = t * FNV_PRIME ^ (t >> 17); } while (0)

__attribute__((noinline)) uint32 pg_checksum_block(const uint32 (*data)[N_SUMS]) {
  uint32 sums[N_SUMS];
  uint32 result = 0;
  uint32 i, j;
  for (j = 0; j < N_SUMS; j++)
    sums[j] = base[j];
  for (i = 0; i < ROWS; i++)
    for (j = 0; j < N_SUMS; j++)
      CHECKSUM_COMP(sums[j], data[i][j]);
  for (i = 0; i < 2; i++)
    for (j = 0; j < N_SUMS; j++)
      CHECKSUM_COMP(sums[j], 0);
  for (i = 0; i < N_SUMS; i++)
    result ^= sums[i];
  return result;
}

__attribute__((noinline)) uint32 by_hand(const uint32 (*data)[N_SUMS]) {
  uint32 sums[N_SUMS];
  uint32 result = 0;
  int i, j;
  for (j = N_SUMS - 1; j >= 0; j--)
    sums[j] = base[j];
  for (i = 0; i < ROWS; i++)
    for (j = N_SUMS - 1; j >= 0; j--)
      CHECKSUM_COMP(sums[j], data[i][j]);
  for (i = 0; i < 2; i++)
    for (j = N_SUMS - 1; j >= 0; j--)
      CHECKSUM_COMP(sums[j], 0);
  for (j = N_SUMS - 1; j >= 0; j--)
    result ^= sums[j];
  return result;
}

__attribute__((noinline)) void carried(uint32 *a) {
  for (int i = 0; i < 32; i++)
    a[i + 1] = a[i] * 3 ^ 1;
}

__attribute__((noinline)) void two(uint32 *to, const uint32 *from) {
  for (int i = 0; i < 32; i++)
    to[i] = from[i] * 5 + 7;
}

uint32 data[ROWS][N_SUMS];

int main(void) {
  uint32 seed = 12345;
  for (int i = 0; i < ROWS; i++)
    for (int j = 0; j < N_SUMS; j++) {
      seed = seed * 1103515245u + 12345u;
      data[i][j] = seed;
    }
  if (pg_checksum_block(data) != by_hand(data))
    return 1;

  uint32 a[33], b[33];
  a[0] = b[0] = 9;
  carried(a);
  for (int i = 0; i < 32; i++)
    b[i + 1] = b[i] * 3 ^ 1;
  for (int i = 0; i < 33; i++)
    if (a[i] != b[i])
      return 2;

  for (int i = 0; i < 33; i++)
    a[i] = b[i] = (uint32) i * 77u;
  two(a + 1, a);
  for (int i = 0; i < 32; i++)
    b[i + 1] = b[i] * 5 + 7;
  for (int i = 0; i < 33; i++)
    if (a[i] != b[i])
      return 3;
  return 0;
}
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_loop_done_four_at_a_time_gives_the_answer_one_at_a_time_would() {
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

/// The checksum loop is a sixteen byte load of each array, `pmuludq` for the multiply and
/// `psrld` for the shift, which is what gcc writes for it.
#[test]
fn the_checksum_loop_is_in_the_vector_registers() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    let start = asm.find("pg_checksum_block:").expect("the function is there");
    let end = asm[start..].find("by_hand:").map_or(asm.len(), |at| start + at);
    let body = &asm[start..end];
    for inst in ["movdqu", "pmuludq", "psrld", "pxor"] {
        assert!(body.contains(inst), "no {inst} in\n{body}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// At `-O1` the loop stays a lane at a time, the way it does under gcc.
#[test]
fn the_checksum_loop_is_a_lane_at_a_time_below_o2() {
    let dir = dir("o1");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O1", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    assert!(!asm.contains("pmuludq"), "a vector multiply at -O1 in\n{asm}");
    let _ = std::fs::remove_dir_all(&dir);
}
