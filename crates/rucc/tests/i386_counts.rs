//! The bit counts and the byte reversal of a `long long` on i386. Each one used to be refused with
//! "no rule lowers `lshr.i64`", because the arithmetic they become was written after the step that
//! splits a 64 bit value into two registers had already run. The kernel's `fls64`, `__ffs64` and
//! `swab64` all reach these on a 32 bit build that has no assembly of its own for them.

use std::process::Command;

const SEARCH: &str = "\
typedef unsigned long long u64;
int clz(unsigned a) { return __builtin_clz(a); }
int ctz(unsigned a) { return __builtin_ctz(a); }
int clzll(u64 a) { return __builtin_clzll(a); }
int ctzll(u64 a) { return __builtin_ctzll(a); }
";

const SOURCE: &str = "\
typedef unsigned long long u64;
int pc(u64 a) { return __builtin_popcountll(a); }
int clz(u64 a) { return __builtin_clzll(a); }
int ctz(u64 a) { return __builtin_ctzll(a); }
int ffs(u64 a) { return __builtin_ffsll(a); }
u64 bs(u64 a) { return __builtin_bswap64(a); }
int par(u64 a) { return __builtin_parityll(a); }
int clrsb(long long a) { return __builtin_clrsbll(a); }
";

fn listing(level: &str) -> String {
    compiled("counts", SOURCE, level)
}

fn compiled(name: &str, source: &str, level: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-i386-{name}-{}-{level}", std::process::id()));
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

/// Every one of them compiles, at both levels, to code that calls nothing: the kernel links no
/// `libgcc`, so a `__popcountdi2` or a `__ctzdi2` would be an undefined symbol at the final link.
#[test]
fn a_count_or_a_reversal_of_a_long_long_is_inline_code() {
    for level in ["-O0", "-O2"] {
        let listing = listing(level);
        assert!(!listing.contains("call"), "{level}: {listing}");
        for name in ["pc", "clz", "ctz", "ffs", "bs", "par", "clrsb"] {
            assert!(listing.contains(&format!("\n{name}:\n")), "{level}: {name}");
        }
    }
}

/// gcc's `bsrl` and `bsfl` for a zero count, with the conditional move x86-64 has after them for
/// the zero, where it was a dozen shifts and masks and a multiply. A `long long` is a search of
/// each word and a move that picks one, where the pair went through the same arithmetic at sixty
/// four bits and came to some seventy instructions. zstd's `ZSTD_highbit32` is one of these in
/// every Huffman table it reads.
#[test]
fn a_zero_count_on_i386_is_a_search() {
    for level in ["-O1", "-O2"] {
        let text = compiled("search", SEARCH, level);
        assert_eq!(text.matches("\tbsrl\t").count(), 3, "{level}\n{text}");
        assert_eq!(text.matches("\tbsfl\t").count(), 3, "{level}\n{text}");
        assert!(!text.contains("16843009"), "{level}: the count is still written out\n{text}");
        assert!(!text.contains("call"), "{level}\n{text}");
    }
}

/// NetBSD's libsa writes `htole32` and `htobe32` as four byte stores to a union and a read of the
/// word. Scalar replacement leaves a dead `trunc` of each byte beside the shifts, and `bswap` only
/// rewrites a tree whose nodes nothing else reads, so the bytes stayed shifts and masks. With `dce`
/// in front of `bswap`, the first is the word as it came and the second is one `bswapl`.
#[test]
fn a_word_put_together_from_its_bytes_through_a_union_is_one_move_or_one_bswap() {
    const UNION: &str = "\
typedef unsigned int u32; typedef unsigned char u8;
typedef union { u32 val; u8 bytes[4]; } un32;
u32 htole32(u32 v) { un32 u; u.bytes[0] = v & 0xff; u.bytes[1] = (v >> 8) & 0xff;
    u.bytes[2] = (v >> 16) & 0xff; u.bytes[3] = (v >> 24) & 0xff; return u.val; }
u32 htobe32(u32 v) { un32 u; u.bytes[3] = v & 0xff; u.bytes[2] = (v >> 8) & 0xff;
    u.bytes[1] = (v >> 16) & 0xff; u.bytes[0] = (v >> 24) & 0xff; return u.val; }
";
    for level in ["-O2", "-Os"] {
        let text = compiled("union", UNION, level);
        let body = |name: &str| {
            let start = text.find(&format!("\n{name}:\n")).expect("the function is in the listing");
            let end = text[start..].find(".size").map_or(text.len(), |at| start + at);
            text[start..end].to_owned()
        };
        let le = body("htole32");
        assert!(!le.contains("shr") && !le.contains("shl") && !le.contains("or"), "{level}\n{le}");
        let be = body("htobe32");
        assert!(be.contains("bswapl"), "{level}\n{be}");
        assert!(!be.contains("shr") && !be.contains("shl"), "{level}\n{be}");
    }
}
