//! A `long long` division on i386 of values that fit in 32 bits, done at 32 bits as gcc does it.
//!
//! C converts both sides of `(u64)a / b` to 64 bits before it divides, and a division at 64 bits
//! on i386 is a call to `__udivdi3` or one of its kin, which the kernel does not link. gcc sees the
//! operands are widened from 32 bits and divides at 32, so the call never happens. A constant
//! divisor then gets its magic number at 32 bits, and so does a dividend that a mask or a shift
//! has already brought below 2 to the 32.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
typedef unsigned long long u64;
typedef long long s64;
u64 ten(unsigned a) { return (u64)a / 10; }
u64 left(unsigned a) { return (u64)a % 1000; }
s64 third(int a) { return (s64)a / 3; }
s64 signed_left(int a) { return (s64)a % -7; }
u64 masked(u64 a) { return (a & 0xffffffff) / 1000; }
u64 shifted(u64 a) { return (a >> 32) / 7; }
u64 both(unsigned a, unsigned b) { return (u64)a / b; }
u64 both_left(unsigned a, unsigned b) { return (u64)a % b; }
u64 whole(u64 a) { return a / 10; }
s64 any(int a, int b) { return (s64)a / b; }
";

fn listing(level: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-i386-narrow-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-fno-pic", level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn a_division_of_values_that_fit_in_a_word_calls_nothing() {
    for level in ["-O0", "-O2"] {
        let text = listing(level);
        for name in
            ["ten", "left", "third", "signed_left", "masked", "shifted", "both", "both_left"]
        {
            let body = body(&text, name);
            assert!(!body.is_empty(), "{level} {name}:\n{text}");
            assert!(!body.iter().any(|line| line.starts_with("call")), "{level} {name}:\n{text}");
        }
    }
}

#[test]
fn a_constant_divisor_is_a_multiply_at_the_word() {
    let text = listing("-O2");
    for (name, multiply) in [
        ("ten", "mull"),
        ("left", "mull"),
        ("third", "imull"),
        ("signed_left", "imull"),
        ("masked", "mull"),
        ("shifted", "mull"),
    ] {
        let body = body(&text, name);
        assert!(body.iter().any(|line| line.starts_with(multiply)), "{name}:\n{text}");
        assert!(!body.iter().any(|line| line.contains("div")), "{name}:\n{text}");
    }
    for name in ["both", "both_left"] {
        assert!(body(&text, name).iter().any(|line| line.starts_with("divl")), "{name}:\n{text}");
    }
}

/// A value that is 64 bits wide to begin with does not fit in a word, and a signed division by a
/// variable may be the most negative `int` over minus one, whose quotient is 2 to the 31 and does
/// not fit either. Both are still the runtime's, as they are with gcc.
#[test]
fn what_may_not_fit_is_still_a_call() {
    let text = listing("-O2");
    for (name, routine) in [("whole", "__udivdi3"), ("any", "__divdi3")] {
        let called = format!("call\t{routine}");
        assert!(body(&text, name).contains(&called), "{name}:\n{text}");
    }
}
