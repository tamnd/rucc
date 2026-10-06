//! A comparison on AArch64 whose answer is wanted as a number, written as the `cmp` and the
//! `cset` and nothing after them, as gcc writes it.
//!
//! `cset` writes a one or a zero to a `w` register, and that clears every bit above it. Before
//! this the selector widened the bit with `and w0, w0, #1` all the same, which is one instruction
//! on every `return a < b;` and every flag the kernel works out of a comparison.

use std::process::Command;

const SOURCE: &str = "\
int less(int a, int b) { return a < b; }
long same(long a, long b) { return a == b; }
unsigned above(unsigned a) { return a > 7; }
long wide(int a, int b) { return a != b; }
int before(double x, double y) { return x < y; }
int apart(double x, double y) { return __builtin_islessgreater(x, y); }
";

fn listing() -> String {
    let dir = std::env::temp_dir().join(format!("rucc-a64-compare-bits-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
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
fn the_answer_of_a_comparison_is_not_widened_again() {
    let text = listing();
    for (name, want) in [
        ("less", &["cmp w0, w1", "cset w0, lt", "ret"][..]),
        ("same", &["cmp x0, x1", "cset w0, eq", "ret"]),
        ("above", &["cmp w0, #7", "cset w0, hi", "ret"]),
        ("wide", &["cmp w0, w1", "cset w0, ne", "ret"]),
        ("before", &["fcmp d0, d1", "cset w0, mi", "ret"]),
    ] {
        assert_eq!(body(&text, name), want, "{name}:\n{text}");
    }
    let apart = body(&text, "apart");
    assert!(apart.iter().any(|line| line.starts_with("csinc w0")), "{text}");
    assert!(!apart.iter().any(|line| line.starts_with("and ")), "{text}");
}
