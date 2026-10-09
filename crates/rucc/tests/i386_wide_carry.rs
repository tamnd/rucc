//! A `long long` add or subtract on i386, and a `__int128` one on x86-64, as an `add` and an `adc`
//! or a `sub` and an `sbb`. The halves used to pass the carry as a `cmp`, a `setb`, a `movzbl` and
//! one more add, and the 32 bit kernel does that on every `u64`. A `__builtin_bswap64` on i386 is
//! two `bswapl` now as well, where it was forty eight shifts and masks, and a negation is `negl`,
//! `adcl $0` and `negl`, and `s64 < 0` asks the high word alone. A shift by a constant fills the
//! word it moves bits into with `shldl` or `shrdl`, where it was two shifts and an or, and a shift
//! by a count in a register does the same with the count in `cl` and picks the words with two
//! `cmov`s on one test.

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

const VAR_SHIFT: &str = "\
typedef unsigned long long u64;
u64 shl(u64 x, unsigned n) { return x << n; }
u64 shr(u64 x, unsigned n) { return x >> n; }
long long sar(long long x, unsigned n) { return x >> n; }
unsigned word(unsigned x, unsigned n) { return x << (n & 31); }
";

const QUAD_VAR_SHIFT: &str = "\
typedef unsigned __int128 u128;
u128 shl(u128 x, unsigned n) { return x << n; }
u128 shr(u128 x, unsigned n) { return x >> n; }
__int128 sar(__int128 x, unsigned n) { return x >> n; }
";

const BITS: &str = "\
typedef unsigned long long u64;
u64 xda(u64 b)
{
	b &= 0xff;
	return (b & 0x80 ? 0xe100 : 0) ^ (b & 0x40 ? 0x7080 : 0) ^ (b & 0x20 ? 0x3840 : 0) ^
	       (b & 0x10 ? 0x1c20 : 0) ^ (b & 0x08 ? 0x0e10 : 0) ^ (b & 0x04 ? 0x0708 : 0) ^
	       (b & 0x02 ? 0x0384 : 0) ^ (b & 0x01 ? 0x01c2 : 0);
}
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

/// gcc's `shldl %cl` for the word the bits cross into, and the count used as it is, since the
/// machine masks a count to the word before it shifts. Whether the count reached a whole word is
/// asked after the shifts, so the moves read the answer from the flags rather than from a byte.
#[test]
fn a_long_long_shift_by_a_register_fills_a_word_from_the_other() {
    for level in ["-O1", "-O2", "-Os"] {
        let text = assembly("var-shift", "i686-unknown-linux-gnu", VAR_SHIFT, level);
        assert_eq!(text.matches("\tshldl\t%cl,").count(), 1, "{level}\n{text}");
        assert_eq!(text.matches("\tshrdl\t%cl,").count(), 2, "{level}\n{text}");
        assert!(!text.contains("\tandl\t$31,"), "{level}: the count is still masked\n{text}");
        assert!(!text.contains("\tset"), "{level}: the word test is still a byte\n{text}");
        assert!(!text.contains("\torl\t"), "{level}\n{text}");
        let text = assembly("quad-var-shift", "x86_64-unknown-linux-gnu", QUAD_VAR_SHIFT, level);
        assert_eq!(text.matches("\tshldq\t%cl,").count(), 1, "{level}\n{text}");
        assert_eq!(text.matches("\tshrdq\t%cl,").count(), 2, "{level}\n{text}");
        assert!(!text.contains("\tandq\t$63,"), "{level}: the count is still masked\n{text}");
        assert!(!text.contains("\torq\t"), "{level}\n{text}");
    }
}

/// The kernel's `xda_le` from `gf128mul.c`, a row of `?:` on the bits of a `u64` that is known to
/// fit in a byte. Each bit is a `test` and a `cmov` on its own zero, where it was an `and` of a
/// copy, the high words asked about with `xorl $0` and an `or`, and a zero shared by all eight that
/// went to the stack and was loaded back for each of them. Once that zero stayed in a register the
/// byte itself still went to the frame at the top, written for reads the cleanup had all served out
/// of `esi`, and nothing is written there now.
#[test]
fn a_long_long_bit_row_keeps_its_zeros_out_of_the_stack() {
    for level in ["-O1", "-O2", "-Os"] {
        let text = assembly("bits", "i686-unknown-linux-gnu", BITS, level);
        assert_eq!(text.matches("\ttestl\t$").count(), 8, "{level}\n{text}");
        assert_eq!(text.matches("\tcmovnel\t").count(), 8, "{level}\n{text}");
        assert!(
            !text.contains("xorl\t$0,"),
            "{level}: a half known to be zero is asked about\n{text}"
        );
        assert!(!text.contains("\torl\t"), "{level}\n{text}");
        // The low word of the argument and nothing else read from the frame. The high word is
        // never wanted, so it is not read either.
        assert_eq!(text.matches("(%esp), %").count(), 1, "{level}\n{text}");
        let stored =
            text.lines().filter(|line| line.starts_with("\tmovl\t%") && line.ends_with("(%esp)"));
        assert_eq!(stored.count(), 0, "{level}: a store nothing reads back\n{text}");
    }
}
