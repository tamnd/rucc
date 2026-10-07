//! A left shift of eight or sixteen bits by a count in a register.
//!
//! C promotes both operands of a shift to `int`, so no source writes one, but the optimizer makes
//! it. A shift whose result is stored to a `u8` is narrowed to eight bits, and when both arms of an
//! `if` shift the same value by different constants they become one shift by a `select` of the two.
//! The kernel's adt7316 and both Broadcom `phy_n.c` drivers do that, and every target refused them
//! with "no rule lowers `shl.i8`".

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The shapes from the kernel, cut down.
const SOURCE: &str = "\
typedef unsigned char u8;
typedef unsigned short u16;
int write_reg(int, u8);
int write_word(int, u16);
int adt7316(int bits, u16 data) {
    u8 offset = bits - 8, lsb, reg;
    if (bits > 8) {
        lsb = data & ((1 << offset) - 1);
        if (bits == 12)
            reg = lsb << 4;
        else
            reg = lsb << 6;
        return write_reg(1, reg);
    }
    return 0;
}
int phy_n(u16 value, int core) {
    u16 mask;
    if (core == 0)
        mask = value << 3;
    else
        mask = value << 11;
    return write_word(2, mask);
}
";

fn fixture(name: &str, source: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-narrow-shift-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join(format!("{name}.c"));
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

fn rucc(args: &[&str], path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .arg(path)
        .output()
        .expect("the compiler is built before its own tests run")
}

#[test]
fn every_target_lowers_a_narrow_shift_by_a_register() {
    let path = fixture("kernel", SOURCE);
    for target in
        ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu", "i686-unknown-linux-gnu"]
    {
        for level in ["-O1", "-O2", "-Os", "-O3"] {
            let target = format!("--target={target}");
            let out = rucc(&[&target, level, "-S", "-o", "-"], &path);
            assert!(
                out.status.success(),
                "{target} {level}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
}

/// Every value shifted by every pair of counts gives the bits the unnarrowed shift does.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn the_narrow_shift_keeps_the_low_bits() {
    let source = "\
typedef unsigned char u8;
typedef unsigned short u16;
__attribute__((noinline)) u8 byte(u8 x, int c) {
    u8 r;
    if (c) r = x << 3; else r = x << 7;
    return r;
}
__attribute__((noinline)) u16 half(u16 x, int c) {
    u16 r;
    if (c) r = x << 5; else r = x << 15;
    return r;
}
int main(void) {
    int bad = 0;
    for (unsigned x = 0; x < 65536; x++) {
        for (int c = 0; c < 2; c++) {
            if (x < 256 && byte(x, c) != (u8)(x << (c ? 3 : 7))) bad++;
            if (half(x, c) != (u16)(x << (c ? 5 : 15))) bad++;
        }
    }
    return bad != 0;
}
";
    let path = fixture("run", source);
    let prog = path.with_extension("");
    for level in ["-O1", "-O2"] {
        let built = rucc(&[level, "-o", prog.to_str().expect("a temporary path is text")], &path);
        assert!(built.status.success(), "{level}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&prog).output().expect("what was linked can be run");
        assert_eq!(ran.status.code(), Some(0), "{level}");
    }
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
}
