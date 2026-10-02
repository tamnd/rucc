//! Tests for how far an object with static storage is aligned on x86-64, which is past what its
//! type asks for once it is long enough, as gcc does it under `-malign-data=compat`.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the listing is the same everywhere.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, under a directory of its own so two of these running at once do not collide.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-align-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source at the level given.
fn asm(what: &str, level: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args([level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The `.p2align` written for the object with that name.
fn p2align(listing: &str, name: &str) -> u32 {
    let lines: Vec<&str> = listing.lines().collect();
    let at = lines
        .iter()
        .position(|line| *line == format!("{name}:"))
        .unwrap_or_else(|| panic!("{name} is not defined:\n{listing}"));
    lines[..at]
        .iter()
        .rev()
        .find_map(|line| line.trim().strip_prefix(".p2align"))
        .and_then(|rest| rest.trim().split(',').next()?.trim().parse().ok())
        .unwrap_or_else(|| panic!("{name} has no alignment:\n{listing}"))
}

/// The kernel's `struct tracepoint` is 72 bytes, which gcc aligns to 32, and the names beside it
/// are short strings it aligns to 8. The `__tracepoints` section is laid out by those.
#[test]
fn a_long_aggregate_is_aligned_to_thirty_two_and_a_word_long_one_to_a_word() {
    let source = "struct tp { const char *name; long a[8]; };\n\
        struct tp t1 __attribute__((section(\"__tp\"))) = { \"x\" };\n\
        static const char s1[] __attribute__((used, section(\"__tps\"))) = \"write_msr\";\n\
        static const char s2[] __attribute__((used, section(\"__tps\"))) = \"rd\";\n\
        int table[4] = { 1 };\n\
        struct tp zeroed;\n";
    for level in ["-O0", "-O2"] {
        let listing = asm("long", level, source);
        assert_eq!(p2align(&listing, "t1"), 5, "{listing}");
        assert_eq!(p2align(&listing, "s1"), 3, "{listing}");
        assert_eq!(p2align(&listing, "s2"), 0, "{listing}");
        assert_eq!(p2align(&listing, "table"), 4, "{listing}");
        assert_eq!(p2align(&listing, "zeroed"), 5, "{listing}");
    }
}

/// An alignment the declaration asked for is kept even when it is less than the raise, and a
/// thread-local is only raised as far as a word.
#[test]
fn an_asked_for_alignment_and_a_thread_local_are_not_raised_past_a_word() {
    let source = "struct tp { const char *name; long a[8]; };\n\
        struct tp asked __attribute__((aligned(8))) = { \"q\" };\n\
        __thread struct tp each = { \"z\" };\n";
    let listing = asm("asked", "-O2", source);
    assert_eq!(p2align(&listing, "asked"), 3, "{listing}");
    assert_eq!(p2align(&listing, "each"), 3, "{listing}");
}
