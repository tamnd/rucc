//! The salsa20/8 core libsodium's scrypt writes with `emmintrin.h`, checked against the same core
//! written with plain `uint32_t` arithmetic.
//!
//! tamnd/rucc#2320. The four `__m128i` locals are kept in vector registers at `-O1` and above,
//! which means every lane the header reads or writes is a lane instruction on a register rather
//! than a load or a store, and a function this size is where the allocator has to choose between
//! registers for them. A merge of one lane that lost the other three only showed up here.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-salsa-{}-{what}", std::process::id()));
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

/// The vector core in libsodium's layout, where each register holds a diagonal of the matrix, and
/// the scalar one it has to agree with. `main` gives back one more than the first lane that
/// disagrees, or zero.
const PROGRAM: &str = r"
#include <emmintrin.h>
#include <stdint.h>
#include <string.h>

#define ARX(out, in1, in2, s) { __m128i T = _mm_add_epi32(in1, in2); \
  out = _mm_xor_si128(out, _mm_slli_epi32(T, s)); \
  out = _mm_xor_si128(out, _mm_srli_epi32(T, 32 - s)); }
#define TWO \
  ARX(X1, X0, X3, 7) ARX(X2, X1, X0, 9) ARX(X3, X2, X1, 13) ARX(X0, X3, X2, 18) \
  X1 = _mm_shuffle_epi32(X1, 0x93); X2 = _mm_shuffle_epi32(X2, 0x4E); \
  X3 = _mm_shuffle_epi32(X3, 0x39); \
  ARX(X3, X0, X1, 7) ARX(X2, X3, X0, 9) ARX(X1, X2, X3, 13) ARX(X0, X1, X2, 18) \
  X1 = _mm_shuffle_epi32(X1, 0x39); X2 = _mm_shuffle_epi32(X2, 0x4E); \
  X3 = _mm_shuffle_epi32(X3, 0x93);

__attribute__((noinline)) void salsa8(__m128i *B) {
  __m128i X0 = B[0], X1 = B[1], X2 = B[2], X3 = B[3];
  TWO TWO TWO TWO
  B[0] = _mm_add_epi32(B[0], X0); B[1] = _mm_add_epi32(B[1], X1);
  B[2] = _mm_add_epi32(B[2], X2); B[3] = _mm_add_epi32(B[3], X3);
}

#define R(a, b) (((a) << (b)) | ((a) >> (32 - (b))))
static void plain(uint32_t B[16]) {
  uint32_t x[16];
  memcpy(x, B, 64);
  for (int i = 0; i < 8; i += 2) {
    x[4] ^= R(x[0] + x[12], 7); x[8] ^= R(x[4] + x[0], 9);
    x[12] ^= R(x[8] + x[4], 13); x[0] ^= R(x[12] + x[8], 18);
    x[9] ^= R(x[5] + x[1], 7); x[13] ^= R(x[9] + x[5], 9);
    x[1] ^= R(x[13] + x[9], 13); x[5] ^= R(x[1] + x[13], 18);
    x[14] ^= R(x[10] + x[6], 7); x[2] ^= R(x[14] + x[10], 9);
    x[6] ^= R(x[2] + x[14], 13); x[10] ^= R(x[6] + x[2], 18);
    x[3] ^= R(x[15] + x[11], 7); x[7] ^= R(x[3] + x[15], 9);
    x[11] ^= R(x[7] + x[3], 13); x[15] ^= R(x[11] + x[7], 18);
    x[1] ^= R(x[0] + x[3], 7); x[2] ^= R(x[1] + x[0], 9);
    x[3] ^= R(x[2] + x[1], 13); x[0] ^= R(x[3] + x[2], 18);
    x[6] ^= R(x[5] + x[4], 7); x[7] ^= R(x[6] + x[5], 9);
    x[4] ^= R(x[7] + x[6], 13); x[5] ^= R(x[4] + x[7], 18);
    x[11] ^= R(x[10] + x[9], 7); x[8] ^= R(x[11] + x[10], 9);
    x[9] ^= R(x[8] + x[11], 13); x[10] ^= R(x[9] + x[8], 18);
    x[12] ^= R(x[15] + x[14], 7); x[13] ^= R(x[12] + x[15], 9);
    x[14] ^= R(x[13] + x[12], 13); x[15] ^= R(x[14] + x[13], 18);
  }
  for (int i = 0; i < 16; i++)
    B[i] += x[i];
}

int main(void) {
  static const int map[16] = {0, 5, 10, 15, 4, 9, 14, 3, 8, 13, 2, 7, 12, 1, 6, 11};
  uint32_t s[16], d[16];
  for (int i = 0; i < 16; i++)
    s[i] = 0x9e3779b9u * (i + 1);
  for (int i = 0; i < 16; i++)
    d[i] = s[map[i]];
  __m128i B[4];
  memcpy(B, d, 64);
  for (int round = 0; round < 3; round++) {
    salsa8(B);
    plain(s);
  }
  memcpy(d, B, 64);
  for (int i = 0; i < 16; i++)
    if (d[i] != s[map[i]])
      return 1 + i;
  return 0;
}
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn the_vector_salsa_core_agrees_with_the_scalar_one() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert_eq!(out.status.code(), Some(0), "{level}: a lane disagrees");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
