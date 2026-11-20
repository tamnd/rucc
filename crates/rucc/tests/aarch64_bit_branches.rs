//! A branch on one bit on AArch64 written as `tbz` or `tbnz`, as gcc writes it.
//!
//! Before this `if (flags & 8)` was a `ubfx` of the bit and a `cbnz` on it, and `if (!(x & (1ul <<
//! 40)))` was a `tst` and a `b.eq`. The machine tests one bit and jumps in one instruction. That
//! instruction only reaches 32 KiB either side, and nothing here moves a jump that falls short, so
//! a function long enough for that to matter keeps the `tst` and the jump on the flags, and so does
//! one with an `asm` template that can be longer than its lines.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
extern void f(void);
volatile int g;
void act(unsigned flags) { if (flags & 8) f(); }
void top(unsigned x) { if (x & 0x80000000u) f(); }
void sign(unsigned long x) { if (x & 0x8000000000000000ul) f(); }
void clear(unsigned long x) { if (!(x & (1ul << 40))) f(); }
int scan(unsigned long *p) { int n = 0; while (!(*p & 1)) { n++; p++; } return n; }
void two(unsigned x) { if (x & 12) f(); }
void rept(unsigned x) { if (x & 8) f(); __asm__ volatile(\".rept 3\\n nop\\n .endr\"); }
void lines(unsigned x) { if (x & 8) f(); __asm__ volatile(\"nop\\n nop; nop\"); }
";

fn compile(source: &str, flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-bits-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
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

fn listing(source: &str) -> String {
    String::from_utf8(compile(source, &["-S"]).stdout).expect("a listing is text")
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
fn a_branch_on_one_bit_is_one_instruction() {
    let text = listing(SOURCE);
    for (name, branch) in [
        ("act", "tbnz w0, #3, "),
        ("top", "tbnz w0, #31, "),
        ("sign", "tbnz x0, #63, "),
        ("clear", "tbz x0, #40, "),
        // The loop is rotated, so its test is at the bottom and goes back round while the bit is clear.
        ("scan", "tbz x2, #0, "),
        ("lines", "tbnz w0, #3, "),
    ] {
        let body = body(&text, name);
        assert!(body.iter().any(|line| line.starts_with(branch)), "{name} {branch}:\n{text}");
        for test in ["tst ", "ubfx ", "cbnz ", "b.ne ", "b.eq "] {
            assert!(!body.iter().any(|line| line.starts_with(test)), "{name} {test}:\n{text}");
        }
    }
}

/// Two bits are not one, so the test stays and the jump reads the flags.
#[test]
fn two_bits_keep_the_test() {
    let text = listing(SOURCE);
    let two = body(&text, "two");
    assert!(two.contains(&"tst w0, #12".to_string()), "{text}");
    assert!(two.iter().any(|line| line.starts_with("b.ne ")), "{text}");
}

/// A template that repeats what is in it may be any length, so the jump stays one that reaches.
#[test]
fn a_template_that_repeats_keeps_the_test() {
    let text = listing(SOURCE);
    let rept = body(&text, "rept");
    assert!(rept.contains(&"tst w0, #8".to_string()), "{text}");
    assert!(!rept.iter().any(|line| line.starts_with("tbnz ")), "{text}");
}

/// A function past what `tbz` reaches across keeps the `tst` and the jump on the flags.
#[test]
fn a_long_function_keeps_the_test() {
    let stores: String = (0..1500).map(|n| format!("g = {n};\n")).collect();
    let source = format!(
        "extern void f(void);\nvolatile int g;\nvoid big(unsigned x) {{ if (x & 8) f();\n{stores}}}\n"
    );
    let text = listing(&source);
    let big = body(&text, "big");
    assert!(big.contains(&"tst w0, #8".to_string()), "{}", big[..8].join("\n"));
    assert!(!big.iter().any(|line| line.starts_with("tbnz ")));
}

/// The integrated assembler takes the new instructions too.
#[test]
fn the_object_assembles() {
    let out = compile(SOURCE, &["-c"]);
    assert!(!out.stdout.is_empty());
}
