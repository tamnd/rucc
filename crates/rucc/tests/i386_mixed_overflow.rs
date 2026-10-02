//! `__builtin_mul_overflow` and its kin on i386 with a signed and an unsigned `long long`. The type
//! that holds every value of both is 65 bits wide, and on x86-64 the check is done in `__int128`,
//! but i386 has no such type, so the check is done at 64 bits with the exact overflow rules. The
//! kernel's `check_mul_overflow` in `grow_buffers` mixes a `sector_t` with a signed size, and
//! fs/buffer.c used to be refused with a 128 bit `zext` nothing lowered.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned long long u64; typedef long long s64;
int f(u64 a, u64 b, s64 *r) { return __builtin_mul_overflow(a, b, r); }
int g(s64 a, u64 b, s64 *r) { return __builtin_mul_overflow(a, b, r); }
int h(s64 a, u64 b, u64 *r) { return __builtin_add_overflow(a, b, r); }
int k(u64 a, s64 b, s64 *r) { return __builtin_sub_overflow(a, b, r); }
";

fn build(level: &str) -> std::process::Output {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-mixed-overflow-{}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-fno-pic", "-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_signed_and_an_unsigned_long_long_are_checked_without_a_wider_type() {
    for level in ["-O0", "-O2"] {
        let out = build(level);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{level}: {stderr}");
        let text = String::from_utf8_lossy(&out.stdout);
        for name in ["f:", "g:", "h:", "k:"] {
            assert!(text.contains(name), "{level}: {name} is not written\n{text}");
        }
    }
}
