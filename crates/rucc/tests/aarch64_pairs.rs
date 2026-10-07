//! Two loads or two stores of words next to each other are one `ldp` or `stp` on AArch64, as gcc
//! writes them.
//!
//! A structure of two longs read for a sum, or written from two arguments, is two accesses at one
//! base with offsets a word apart, and the machine does both in one instruction. A copy that loads
//! and stores in turn is left alone, since there is no alias information after allocation and the
//! second load cannot move above the first store. Floats, doubles and `long double` pair the
//! same way, into `ldp` and `stp` of `s`, `d` and `q` registers.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
struct p { long a, b; };
void st(struct p *q, long x, long y) { q->a = x; q->b = y; }
long ld(struct p *q) { return q->a + q->b; }
void st4(int *q, int x, int y) { q[2] = x; q[3] = y; }
void cp(struct p *d, const struct p *s) { *d = *s; }
long arr(long *a) { return a[0] * a[1] + a[2] * a[3]; }
long far(long *a) { return a[0] + a[100]; }
void dst(double *d, double x, double y) { d[0] = x; d[1] = y; }
float fs(float *f) { return f[2] * f[3]; }
void qst(long double *d, long double x, long double y) { d[2] = x; d[3] = y; }
void mix(double *d, double a, long b) { d[0] = a; d[1] = (double)b; }
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-pairs-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-fno-pic", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out
}

fn listing() -> String {
    String::from_utf8(compile(&["-S"]).stdout).expect("a listing is text")
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
fn neighbouring_words_are_one_instruction() {
    let text = listing();
    for (name, lines) in [
        ("st", &["stp x1, x2, [x0]", "ret"][..]),
        ("ld", &["ldp x1, x0, [x0]", "add x0, x1, x0", "ret"][..]),
        ("st4", &["stp w1, w2, [x0, #8]", "ret"][..]),
        ("dst", &["stp d0, d1, [x0]", "ret"][..]),
        ("fs", &["ldp s0, s1, [x0, #8]", "fmul s0, s0, s1", "ret"][..]),
        ("qst", &["stp q0, q1, [x0, #32]", "ret"][..]),
    ] {
        assert_eq!(body(&text, name), lines, "{name}:\n{text}");
    }
    let arr = body(&text, "arr");
    assert!(arr.contains(&"ldp x1, x2, [x0]".to_string()), "{text}");
    assert!(arr.contains(&"ldp x2, x0, [x0, #16]".to_string()), "{text}");
}

/// A load that would have to move above a store stays where it is, and two words that are not
/// next to each other stay two loads.
#[test]
fn what_cannot_be_paired_is_left_alone() {
    let text = listing();
    // In `mix` the register the first store reads is written again before the second.
    for name in ["cp", "far", "mix"] {
        let body = body(&text, name);
        assert!(
            !body.iter().any(|line| line.starts_with("ldp") || line.starts_with("stp")),
            "{name}:\n{text}"
        );
    }
}

/// The integrated assembler takes the pairs too.
#[test]
fn the_object_assembles() {
    let out = compile(&["-c"]);
    assert!(!out.stdout.is_empty());
}
