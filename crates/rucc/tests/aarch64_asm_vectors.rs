//! An `asm` operand that is a vector, `"w" (p)` for a `uint64x2_t`, which the kernel's
//! lib/raid/xor/arm64/xor-eor3.c hands to an `eor3`.
//!
//! The lanes are checked against what gcc 16's build of the same file printed on an arm64 Mac,
//! and only run on one. That the operands land in vector registers for Linux is checked anywhere.

use std::path::PathBuf;
use std::process::Command;

/// A sixteen byte vector in and out of an `eor`, and an eight byte one read and written by an
/// `add` through `+w`.
const SOURCE: &str = r#"#include <arm_neon.h>
int printf(const char *, ...);
static inline uint64x2_t eor(uint64x2_t p, uint64x2_t q) {
  uint64x2_t res;
  asm("eor %0.16b, %1.16b, %2.16b" : "=w"(res) : "w"(p), "w"(q));
  return res;
}
static inline uint8x8_t add8(uint8x8_t a, uint8x8_t b) {
  asm("add %0.8b, %0.8b, %1.8b" : "+w"(a) : "w"(b));
  return a;
}
static volatile uint64_t a[2] = { 0x1122334455667788ull, 0xf0f0f0f0f0f0f0f0ull };
static volatile uint64_t b[2] = { 0xffull, 0x0f0f0f0f0f0f0f0full };
static volatile uint8_t x[8] = { 1, 2, 3, 4, 5, 6, 7, 250 };
static volatile uint8_t y[8] = { 10, 20, 30, 40, 50, 60, 70, 10 };
int main(void) {
  uint64_t c[2], p[2] = { a[0], a[1] }, q[2] = { b[0], b[1] };
  vst1q_u64(c, eor(vld1q_u64(p), vld1q_u64(q)));
  uint8_t z[8], s[8], t[8];
  for (int i = 0; i < 8; i++) { s[i] = x[i]; t[i] = y[i]; }
  vst1_u8(z, add8(vld1_u8(s), vld1_u8(t)));
  printf("%016llx %016llx", (unsigned long long)c[0], (unsigned long long)c[1]);
  for (int i = 0; i < 8; i++) printf(" %d", z[i]);
  printf("\n");
  return 0;
}
"#;

/// What gcc's build printed. Only a machine that runs the program reads it.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXPECTED: &str = "1122334455667777 ffffffffffffffff 11 22 33 44 55 66 77 4\n";

/// A directory of this test's own with the source in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("rucc-aarch64-asm-vectors-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("a.c"), SOURCE).expect("the fixture can be written");
    dir
}

#[test]
fn a_vector_operand_is_in_a_vector_register_for_linux() {
    let dir = fixture("linux");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-S", "-o", "-", "a.c"])
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.is_empty(), "{err}");
    let text = String::from_utf8_lossy(&out.stdout);
    let picked = |op: &str| {
        text.lines().any(|line| {
            let mut words = line.split_whitespace();
            words.next() == Some(op) && words.all(|word| word.starts_with('v'))
        })
    };
    assert!(picked("eor") && picked("add"), "{text}");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn a_vector_operand_keeps_its_lanes() {
    for level in ["-O0", "-O2"] {
        let dir = fixture(&level[1..]);
        let built = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([level, "-o", "a", "a.c"])
            .current_dir(&dir)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(dir.join("a")).output().expect("the program runs");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(String::from_utf8_lossy(&ran.stdout), EXPECTED, "{level}");
    }
}
