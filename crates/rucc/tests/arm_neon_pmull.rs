//! The 64 bit carry-less product of `<arm_neon.h>`, `vmull_p64` and `vmull_high_p64`, and the
//! `poly128_t` it gives back, which the kernel's lib/crc/arm64/crc64-neon.c is written with.
//!
//! The products are checked against what clang's build of the same file printed on an arm64 Mac,
//! where the instruction is PMULL, and only run on one. That the header compiles for Linux is
//! checked anywhere.

use std::path::PathBuf;
use std::process::Command;

/// Every product and reinterpretation the kernel's CRC code uses, on numbers read from a volatile
/// array so nothing is worked out at build time.
const SOURCE: &str = r#"#include <arm_neon.h>
#include <stdint.h>
int printf(const char *, ...);
static volatile uint64_t xs[] = { 1, 0x8000000000000000ull, 0xeadc41fd2ba3d420ull, 0xffffffffffffffffull };
static void show(poly128_t r) {
  uint64x2_t v = vreinterpretq_u64_p128(r);
  printf("%016llx%016llx\n", (unsigned long long)vgetq_lane_u64(v, 1), (unsigned long long)vgetq_lane_u64(v, 0));
}
int main(void) {
  for (int i = 0; i < 4; i++) show(vmull_p64(xs[i], xs[3 - i]));
  uint64x2_t a = { xs[0], xs[2] }, b = { xs[1], xs[3] };
  show(vmull_high_p64(vreinterpretq_p64_u64(a), vreinterpretq_p64_u64(b)));
  show(vreinterpretq_p128_u64(a));
  return 0;
}
"#;

/// What clang's build printed. Only a machine that runs the program reads it.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXPECTED: &str = "0000000000000000ffffffffffffffff
756e20fe95d1ea100000000000000000
756e20fe95d1ea100000000000000000
0000000000000000ffffffffffffffff
59b43f54e69eb3e059b43f54e69eb3e0
eadc41fd2ba3d4200000000000000001
";

/// A directory of this test's own with the source in it.
fn fixture(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-arm-neon-pmull-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("a.c"), SOURCE).expect("the fixture can be written");
    dir
}

#[test]
fn the_carry_less_product_compiles_for_linux() {
    let dir = fixture("linux");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-c", "-o", "a.o", "a.c"])
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.is_empty(), "{err}");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn the_carry_less_product_is_what_pmull_gives() {
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
