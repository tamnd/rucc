//! The SHA-1, SHA-256 and SHA-512 instructions of `<arm_neon.h>`, which FreeBSD's
//! lib/libmd/sha256c_arm64.c and sha512c_arm64.c are written with.
//!
//! What they give is checked against what gcc 13's build of the same file printed under
//! qemu-aarch64, and only run on an arm64 Mac, where the instructions are there. That each is one
//! instruction is checked anywhere.

use std::path::PathBuf;
use std::process::Command;

/// Every intrinsic once, on numbers read from volatile arrays so nothing is worked out at build
/// time.
const SOURCE: &str = r#"#include <arm_neon.h>
#include <stdint.h>
int printf(const char *, ...);
static volatile uint32_t ws[] = { 0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19, 1, 2, 3, 0x80000000 };
static volatile uint64_t ds[] = { 0x6a09e667f3bcc908ull, 0xbb67ae8584caa73bull, 0x3c6ef372fe94f82bull, 0xa54ff53a5f1d36f1ull, 5, 0xdeadbeefcafef00dull };
static void four(uint32x4_t v) { printf("%08x %08x %08x %08x\n", vgetq_lane_u32(v, 0), vgetq_lane_u32(v, 1), vgetq_lane_u32(v, 2), vgetq_lane_u32(v, 3)); }
static void two(uint64x2_t v) { printf("%016llx %016llx\n", (unsigned long long)vgetq_lane_u64(v, 0), (unsigned long long)vgetq_lane_u64(v, 1)); }
int main(void) {
  uint32_t w[12];
  for (int i = 0; i < 12; i++) w[i] = ws[i];
  uint32x4_t a = vld1q_u32(w), b = vld1q_u32(w + 4), c = vld1q_u32(w + 8);
  uint64x2_t x = { ds[0], ds[1] }, y = { ds[2], ds[3] }, z = { ds[4], ds[5] };
  four(vsha256hq_u32(a, b, c));
  four(vsha256h2q_u32(b, a, c));
  four(vsha256su0q_u32(a, b));
  four(vsha256su1q_u32(a, b, c));
  four(vsha1cq_u32(a, ws[0] ^ 0x7c3d99df, c));
  four(vsha1mq_u32(a, ws[1], c));
  four(vsha1pq_u32(b, ws[2], a));
  four(vsha1su0q_u32(a, b, c));
  four(vsha1su1q_u32(a, b));
  printf("%08x\n", vsha1h_u32(ws[3]));
  two(vsha512hq_u64(x, y, z));
  two(vsha512h2q_u64(x, y, z));
  two(vsha512su0q_u64(x, y));
  two(vsha512su1q_u64(x, y, z));
  return 0;
}
"#;

/// What gcc's build printed. Only a machine that runs the program reads it.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXPECTED: &str = "ca69877c af8229e5 99745eee b97e58b6
4e92b427 311b1308 87eadc7f 563db30b
61c53abb 1a913b18 d91cbb90 056bf7e2
05112ef3 db0bd830 4af666ec 3c78a94d
b449a6bf 34057ce9 e7f1f91f 24f80ff8
cdfd6559 fc0519e0 958a0b55 6e851ca9
e173213c 4a5fef08 5d328372 69ea6980
56671514 1e285bbd 6d60a10e be4a9db6
e2191dd7 49c8ee5d cf1c7cd6 8eadd1da
a953fd4e
39d793747cfcae20 04a8145df74c2dcf
c12a350359277c2a d7946139995f9748
5188661940a4d57c 70db79326d146858
e68d2e3fd4f09678 284a10253efc1ffd
";

/// A directory of this test's own with the source in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-arm-neon-sha-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("a.c"), SOURCE).expect("the fixture can be written");
    dir
}

#[test]
fn each_sha_intrinsic_is_one_instruction() {
    let dir = fixture("listing");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-freebsd15.1", "-O2", "-S", "-o", "-", "a.c"])
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.is_empty(), "{err}");
    let text = String::from_utf8_lossy(&out.stdout);
    let rounds = [
        "sha256h q",
        "sha256h2 q",
        "sha256su0 v",
        "sha256su1 v",
        "sha1c q",
        "sha1m q",
        "sha1p q",
        "sha1su0 v",
        "sha1su1 v",
        "sha1h s",
        "sha512h q",
        "sha512h2 q",
        "sha512su0 v",
        "sha512su1 v",
    ];
    for round in rounds {
        let count = text.lines().filter(|line| line.trim_start().starts_with(round)).count();
        assert_eq!(count, 1, "{round}:\n{text}");
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn the_sha_intrinsics_give_what_gcc_s_do() {
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
