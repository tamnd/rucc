//! A `long long` divided by a power of two on i386. The rewrite to shifts used to stop at the
//! width of a register, so every one of these was a call to `__divdi3` or one of its kin, and at
//! `-O0` the divisor was not even seen as a constant. The kernel links no `libgcc`, and gcc writes
//! shifts for all of them at every level, so `do_div` callers and `x / PAGE_SIZE` on a `u64` in a
//! 32 bit build each turned into an undefined symbol at the final link.
//!
//! The other divisors are held to what gcc 14 does at `-O2` and `-Os`: which ones are calls, and
//! where a quotient and a remainder of the same operands are one call to `__udivmoddi4` or
//! `__divmoddi4`. tamnd/rucc#4562.

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
    compiled(SOURCE, level)
}

/// The i386 assembly for that source at that level.
fn compiled(source: &str, level: &str) -> String {
    // The tests in this file run on threads of one process and ask for the same levels, so the
    // process id and the level alone would give two of them the same directory, and one could
    // remove it while the other was still compiling the file in it.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-divide-{}-{level}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
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

/// The routines each function of the listing calls, in the order it calls them.
fn calls<'a>(listing: &'a str, name: &str) -> Vec<&'a str> {
    let start =
        listing.find(&format!("\n{name}:\n")).unwrap_or_else(|| panic!("{name}: {listing}"));
    let body = &listing[start + name.len() + 3..];
    let end = body.find(".size").unwrap_or(body.len());
    body[..end]
        .lines()
        .filter_map(|line| line.trim().strip_prefix("call"))
        .map(|callee| callee.trim().trim_end_matches("@PLT"))
        .filter(|callee| !callee.starts_with("__x86.get_pc_thunk"))
        .collect()
}

/// A divisor that gcc 14 writes without a call at `-O2` on i386. The remainder of the dividend by
/// the odd part of the divisor is the remainder of a short sum of its pieces, and the quotient is
/// a multiply by an inverse. An unsigned divisor with the top bit set is a compare.
const BY_CONSTANTS: &str = "\
typedef long long s64;
typedef unsigned long long u64;
u64 udiv10(u64 a) { return a / 10; }
u64 urem10(u64 a) { return a % 10; }
u64 udiv7(u64 a) { return a / 7; }
u64 udiv3(u64 a) { return a / 3; }
u64 urem_all(u64 a) { return a % 0xffffffffULL; }
s64 sdiv10(s64 a) { return a / 10; }
s64 srem7(s64 a) { return a % 7; }
s64 srem_minus3(s64 a) { return a % -3; }
s64 sdiv12(s64 a) { return a / 12; }
u64 udiv_huge(u64 a) { return a / 0x8000000000000001ULL; }
u64 urem_huge(u64 a) { return a % 0xfffffffffffffff0ULL; }
";

/// Each of them calls nothing at `-O2`, as with gcc.
#[test]
fn a_long_long_divided_by_ten_calls_nothing() {
    let listing = compiled(BY_CONSTANTS, "-O2");
    for name in [
        "udiv10",
        "urem10",
        "udiv7",
        "udiv3",
        "urem_all",
        "sdiv10",
        "srem7",
        "srem_minus3",
        "sdiv12",
        "udiv_huge",
        "urem_huge",
    ] {
        assert_eq!(calls(&listing, name), Vec::<&str>::new(), "{name}: {listing}");
    }
}

/// At `-Os` gcc calls the runtime for a divisor that is not a power of two, as the call is the
/// shorter code. A huge unsigned divisor is still a compare.
#[test]
fn at_os_a_long_long_divided_by_ten_is_a_call() {
    let listing = compiled(BY_CONSTANTS, "-Os");
    assert_eq!(calls(&listing, "udiv10"), ["__udivdi3"], "{listing}");
    assert_eq!(calls(&listing, "srem7"), ["__moddi3"], "{listing}");
    assert_eq!(calls(&listing, "udiv_huge"), Vec::<&str>::new(), "{listing}");
}

/// A divisor that has no such sum, which is 1000 among others, is still a call to the runtime, as
/// it is in gcc. So is a signed quotient by a negative divisor.
#[test]
fn any_other_divisor_is_still_a_call() {
    let source = "\
long long f(long long a) { return a / 1000; }
unsigned long long g(unsigned long long a) { return a % 1000000000; }
long long h(long long a) { return a / -10; }
";
    let listing = compiled(source, "-O2");
    assert_eq!(calls(&listing, "f"), ["__divdi3"], "{listing}");
    assert_eq!(calls(&listing, "g"), ["__umoddi3"], "{listing}");
    assert_eq!(calls(&listing, "h"), ["__divdi3"], "{listing}");
}

/// A quotient and a remainder of the same operands are one call to `__udivmoddi4` or
/// `__divmoddi4`, where gcc makes one. The two on separate sides of a branch are separate calls,
/// and so are the two of a constant dividend, as with gcc.
#[test]
fn a_quotient_and_a_remainder_are_one_call() {
    let source = "\
typedef long long s64;
typedef unsigned long long u64;
u64 same(u64 a, u64 b, u64 *r) { *r = a % b; return a / b; }
s64 ssame(s64 a, s64 b, s64 *r) { *r = a % b; return a / b; }
u64 k1000(u64 a, u64 *r) { *r = a % 1000; return a / 1000; }
u64 sib(u64 a, u64 b, int c) { if (c) return a / b; return a % b; }
u64 kdiv(u64 b, u64 *r) { *r = 1000000000000ULL % b; return 1000000000000ULL / b; }
u64 k10(u64 a, u64 *r) { *r = a % 10; return a / 10; }
";
    for level in ["-O2", "-Os"] {
        let listing = compiled(source, level);
        assert_eq!(calls(&listing, "same"), ["__udivmoddi4"], "{level}: {listing}");
        assert_eq!(calls(&listing, "ssame"), ["__divmoddi4"], "{level}: {listing}");
        assert_eq!(calls(&listing, "k1000"), ["__udivmoddi4"], "{level}: {listing}");
        let mut sib = calls(&listing, "sib");
        sib.sort_unstable();
        assert_eq!(sib, ["__udivdi3", "__umoddi3"], "{level}: {listing}");
        let mut kdiv = calls(&listing, "kdiv");
        kdiv.sort_unstable();
        assert_eq!(kdiv, ["__udivdi3", "__umoddi3"], "{level}: {listing}");
    }
    assert_eq!(calls(&compiled(source, "-O2"), "k10"), Vec::<&str>::new());
    assert_eq!(calls(&compiled(source, "-Os"), "k10"), ["__udivmoddi4"]);
}
