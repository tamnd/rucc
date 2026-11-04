//! A product two words wide as the one `mul` gcc writes, which leaves the low word in `rax` and the
//! high word in `rdx`. The two halves used to be selected one at a time, an `imulq` for the low
//! word and a `mulq` for the high one, which multiplied the same two registers twice. The kernel's
//! `mul_u64_u32_shr` is this, and the scheduler and the clocks call it on every tick.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned long long u64; typedef unsigned __int128 u128;
u64 shr32(u64 a, unsigned m) { return (u64)(((u128)a * m) >> 32); }
u128 full(u64 a, u64 b) { return (u128)a * b; }
__int128 signed_full(long a, long b) { return (__int128)a * b; }
u64 high(u64 a, u64 b) { return (u64)(((u128)a * b) >> 64); }
";

const I386: &str = "\
typedef unsigned long long u64;
u64 full(unsigned a, unsigned b) { return (u64)a * b; }
";

fn assembly(name: &str, target: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-wide-product-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-fno-pic", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// The body of one function in the assembly.
fn body<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text.find(&format!("\n{name}:")).expect("the function is in the assembly");
    let rest = &text[start + 1..];
    &rest[..rest.find("\n\t.size").unwrap_or(rest.len())]
}

#[test]
fn both_halves_of_a_product_come_from_one_multiply() {
    let text = assembly("quad", "x86_64-unknown-linux-gnu", SOURCE);
    for (name, mul) in [("shr32", "mulq"), ("full", "mulq"), ("signed_full", "imulq")] {
        let body = body(&text, name);
        assert_eq!(body.matches("mulq\t").count(), 1, "{name}\n{body}");
        assert!(body.contains(&format!("\t{mul}\t")), "{name}\n{body}");
    }
    let body = body(&text, "high");
    assert_eq!(body.matches("mulq\t").count(), 1, "high\n{body}");
}

#[test]
fn a_long_long_product_of_two_words_is_one_mull() {
    let text = assembly("wide", "i686-unknown-linux-gnu", I386);
    let body = body(&text, "full");
    assert_eq!(body.matches("mull\t").count(), 1, "{body}");
}
