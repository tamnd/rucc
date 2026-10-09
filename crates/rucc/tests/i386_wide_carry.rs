//! A `long long` add or subtract on i386, and a `__int128` one on x86-64, as an `add` and an `adc`
//! or a `sub` and an `sbb`. The halves used to pass the carry as a `cmp`, a `setb`, a `movzbl` and
//! one more add, and the 32 bit kernel does that on every `u64`. A `__builtin_bswap64` on i386 is
//! two `bswapl` now as well, where it was forty eight shifts and masks.

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

const SWAP: &str = "\
unsigned long long swap(unsigned long long x) { return __builtin_bswap64(x); }
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

#[test]
fn a_long_long_byte_swap_is_two_word_swaps() {
    for level in ["-O0", "-O2"] {
        let text = assembly("swap", "i686-unknown-linux-gnu", SWAP, level);
        assert_eq!(text.matches("\tbswapl\t").count(), 2, "{level}\n{text}");
        assert!(!text.contains("16711935"), "{level}: the masks are still there\n{text}");
    }
}
