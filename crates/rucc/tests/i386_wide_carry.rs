//! A `long long` add or subtract on i386, and a `__int128` one on x86-64, as an `add` and an `adc`
//! or a `sub` and an `sbb`. The halves used to pass the carry as a `cmp`, a `setb`, a `movzbl` and
//! one more add, and the 32 bit kernel does that on every `u64`. A `__builtin_bswap64` on i386 is
//! two `bswapl` now as well, where it was forty eight shifts and masks, and a negation is `negl`,
//! `adcl $0` and `negl`, and `s64 < 0` asks the high word alone. A shift by a constant fills the
//! word it moves bits into with `shldl` or `shrdl`, where it was two shifts and an or.

use std::process::Command;

const WIDE: &str = "\
typedef unsigned long long u64;
u64 add(u64 a, u64 b) { return a + b; }
u64 sub(u64 a, u64 b) { return a - b; }
u64 addk(u64 a) { return a + 0x100000005ULL; }
u64 addw(u64 a, unsigned b) { return a + b; }
void acc(u64 *p, u64 v) { *p += v; }
";

const QUAD: &str = "\
typedef unsigned __int128 u128;
u128 add(u128 a, u128 b) { return a + b; }
u128 sub(u128 a, u128 b) { return a - b; }
";

const NEG: &str = "\
typedef unsigned long long u64;
u64 neg(u64 a) { return -a; }
u64 zero(u64 a) { u64 z = 0; return z - a; }
";

const QUAD_NEG: &str = "\
unsigned __int128 neg(unsigned __int128 a) { return -a; }
";

const COMPARE: &str = "\
typedef unsigned long long u64; typedef long long s64;
int lt0(s64 x) { return x < 0; }
int ge0(s64 x) { return x >= 0; }
int gtm1(s64 x) { return x > -1; }
int fits(u64 x) { return x <= 0xffffffffULL; }
int big(u64 x) { return x >= 0x100000000ULL; }
";

const SWAP: &str = "\
unsigned long long swap(unsigned long long x) { return __builtin_bswap64(x); }
";

const SHIFT: &str = "\
typedef unsigned long long u64;
u64 shl5(u64 x) { return x << 5; }
u64 shr5(u64 x) { return x >> 5; }
long long sar5(long long x) { return x >> 5; }
u64 rol13(u64 x) { return (x << 13) | (x >> 51); }
";

const QUAD_SHIFT: &str = "\
typedef unsigned __int128 u128;
u128 shl5(u128 x) { return x << 5; }
u128 shr5(u128 x) { return x >> 5; }
";

fn assembly(name: &str, target: &str, source: &str, level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-wide-carry-{}-{name}-{target}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-fno-pic", "-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name} {level}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

#[test]
fn a_long_long_sum_carries_in_the_flag() {
    for level in ["-O1", "-O2", "-Os"] {
        let text = assembly("wide", "i686-unknown-linux-gnu", WIDE, level);
        assert!(!text.contains("setb"), "{level}: the carry is still a byte\n{text}");
        assert_eq!(text.matches("\tadcl\t").count(), 4, "{level}\n{text}");
        assert_eq!(text.matches("\tsbbl\t").count(), 1, "{level}\n{text}");
        assert!(text.contains("adcl\t$1,"), "{level}: the high word of the constant\n{text}");
        assert!(text.contains("adcl\t$0,"), "{level}: a word added to a pair\n{text}");
    }
}

#[test]
fn an_int128_sum_carries_in_the_flag() {
    for level in ["-O1", "-O2"] {
        let text = assembly("quad", "x86_64-unknown-linux-gnu", QUAD, level);
        assert!(!text.contains("setb"), "{level}: the carry is still a byte\n{text}");
        assert_eq!(text.matches("\tadcq\t").count(), 1, "{level}\n{text}");
        assert_eq!(text.matches("\tsbbq\t").count(), 1, "{level}\n{text}");
    }
}

/// gcc's `negl`, `adcl $0` and `negl`, since the `neg` of the low word leaves the borrow in the
/// carry flag.
#[test]
fn a_long_long_negation_carries_in_the_flag() {
    for level in ["-O1", "-O2", "-Os"] {
        let text = assembly("neg", "i686-unknown-linux-gnu", NEG, level);
        assert!(!text.contains("setb"), "{level}: the borrow is still a byte\n{text}");
        assert_eq!(text.matches("\tnegl\t").count(), 4, "{level}\n{text}");
        assert_eq!(text.matches("\tadcl\t$0,").count(), 2, "{level}\n{text}");
        assert!(!text.contains("movl\t$0,"), "{level}: the zero it was asked against\n{text}");
    }
    let text = assembly("quad-neg", "x86_64-unknown-linux-gnu", QUAD_NEG, "-O2");
    assert!(!text.contains("setb"), "{text}");
    assert_eq!(text.matches("\tnegq\t").count(), 2, "{text}");
    assert_eq!(text.matches("\tadcq\t$0,").count(), 1, "{text}");
}

/// A comparison with a constant whose low word is zero or all ones asks the low words a question
/// with one answer, so it is one comparison of the high words.
#[test]
fn a_long_long_compared_at_a_word_boundary_is_one_compare() {
    for level in ["-O1", "-O2"] {
        let text = assembly("compare", "i686-unknown-linux-gnu", COMPARE, level);
        assert_eq!(text.matches("\tcmpl\t").count(), 5, "{level}\n{text}");
        assert!(!text.contains("sete"), "{level}: the high words are still asked if equal\n{text}");
    }
}

#[test]
fn a_long_long_byte_swap_is_two_word_swaps() {
    for level in ["-O0", "-O2"] {
        let text = assembly("swap", "i686-unknown-linux-gnu", SWAP, level);
        assert_eq!(text.matches("\tbswapl\t").count(), 2, "{level}\n{text}");
        assert!(!text.contains("16711935"), "{level}: the masks are still there\n{text}");
    }
}

/// gcc's `shldl $5` and `shll $5` for `x << 5`, and `shrdl` the other way. A rotate is two of
/// them, one for each word.
#[test]
fn a_long_long_shift_by_a_constant_fills_a_word_from_the_other() {
    for level in ["-O1", "-O2", "-Os"] {
        let text = assembly("shift", "i686-unknown-linux-gnu", SHIFT, level);
        assert_eq!(text.matches("\tshldl\t$5,").count(), 1, "{level}\n{text}");
        assert_eq!(text.matches("\tshrdl\t$5,").count(), 2, "{level}\n{text}");
        assert_eq!(text.matches("\tshldl\t$13,").count(), 2, "{level}\n{text}");
        assert!(
            !text.contains("\torl\t"),
            "{level}: the words are still put together by hand\n{text}"
        );
    }
    let text = assembly("quad-shift", "x86_64-unknown-linux-gnu", QUAD_SHIFT, "-O2");
    assert_eq!(text.matches("\tshldq\t$5,").count(), 1, "{text}");
    assert_eq!(text.matches("\tshrdq\t$5,").count(), 1, "{text}");
    assert!(!text.contains("\torq\t"), "{text}");
}
