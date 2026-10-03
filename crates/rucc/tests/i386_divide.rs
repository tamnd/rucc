//! A `long long` divided by a power of two on i386. The rewrite to shifts used to stop at the
//! width of a register, so every one of these was a call to `__divdi3` or one of its kin, and at
//! `-O0` the divisor was not even seen as a constant. The kernel links no `libgcc`, and gcc writes
//! shifts for all of them at every level, so `do_div` callers and `x / PAGE_SIZE` on a `u64` in a
//! 32 bit build each turned into an undefined symbol at the final link.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
typedef long long s64;
typedef unsigned long long u64;
s64 half(s64 a) { return a / 2; }
s64 left(s64 a) { return a % 4; }
s64 down(s64 a) { return a / -8; }
s64 word(s64 a) { return a / (1LL << 32); }
s64 high(s64 a) { return a % -(1LL << 40); }
u64 page(u64 a) { return a / 4096; }
u64 rest(u64 a) { return a % (1ULL << 35); }
";

fn listing(level: &str) -> String {
    // The tests in this file run on threads of one process and ask for the same levels, so the
    // process id and the level alone would give two of them the same directory, and one could
    // remove it while the other was still compiling the file in it.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-divide-{}-{level}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-asynchronous-unwind-tables", "-S"])
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// None of them calls anything, at either level.
#[test]
fn a_long_long_divided_by_a_power_of_two_calls_nothing() {
    for level in ["-O0", "-O2"] {
        let listing = listing(level);
        assert!(!listing.contains("call"), "{level}: {listing}");
        for name in ["half", "left", "down", "word", "high", "page", "rest"] {
            assert!(listing.contains(&format!("\n{name}:\n")), "{level}: {name}");
        }
    }
}

/// A divisor that is not a power of two still goes to the runtime, as it does in gcc: the multiply
/// that would replace it needs the high half of a 128 bit product.
#[test]
fn any_other_divisor_is_still_a_call() {
    let dir = std::env::temp_dir().join(format!("rucc-i386-divide-{}-other", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, "long long f(long long a) { return a / 10; }\n").expect("written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(listing.contains("__divdi3"), "{listing}");
}
