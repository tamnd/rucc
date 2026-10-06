//! A branch on whether a register is zero on AArch64 written as `cbz` or `cbnz`, as gcc writes it.
//!
//! Before this every `if (p)`, `while (n)` and `if (x == 0)` was `cmp` against zero and then
//! `b.eq` or `b.ne`, which is two instructions where the machine has one. `cbz` reaches as far as
//! `b.eq` does, so the layout of a function has nothing new to worry about. A comparison against
//! any other constant still sets the flags and branches on them.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
struct node { struct node *next; long v; };
extern void f(void);
long walk(struct node *n) { long s = 0; while (n) { s += n->v; n = n->next; } return s; }
int truth(int x, int y) { if (x) return y + 1; return y - 1; }
long wide(long x, long y) { if (x == 0) return y; return y * 5; }
int narrow(unsigned x, int y) { if (x != 0) return y * 7; return y; }
int boolean(_Bool b, int y) { if (b) return y * 3; return y; }
void guard(int *p) { if (!p) return; f(); }
int seven(int x, int y) { if (x == 7) return y * 7; return y; }
long sum(int *a, unsigned n) { long s = 0; for (unsigned i = 0; i < n; i++) s += a[i]; return s; }
int one(unsigned x, int y) { if (x >= 1) return y * 3; return y; }
int below(unsigned long x, int y) { if (x < 1) return y * 3; return y; }
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-zero-{}-{n}", std::process::id()));
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
fn a_branch_on_zero_is_one_instruction() {
    let text = listing();
    for (name, branch) in [
        ("walk", "cbnz x1, "),
        ("truth", "cbz w0, "),
        ("wide", "cbnz x0, "),
        ("narrow", "cbz w0, "),
        ("boolean", "cbz w0, "),
        ("guard", "cbz x0, "),
        ("one", "cbz w0, "),
        ("below", "cbnz x0, "),
    ] {
        let body = body(&text, name);
        assert!(body.iter().any(|line| line.starts_with(branch)), "{name} {branch}:\n{text}");
        assert!(!body.iter().any(|line| line.starts_with("cmp ")), "{name}:\n{text}");
        assert!(!body.iter().any(|line| line.starts_with("b.")), "{name}:\n{text}");
    }
}

/// An unsigned counter that starts at zero is compared as `0 < n` on the way into the loop, which
/// is `n != 0`, so the guard is a `cbz` and only the test at the bottom of the loop is an ordering.
#[test]
fn an_unsigned_loop_guard_is_a_test_against_zero() {
    let text = listing();
    let sum = body(&text, "sum");
    assert!(sum.iter().any(|line| line.starts_with("cbz w1, ")), "{text}");
    assert!(!sum.contains(&"cmp w1, #0".to_string()), "{text}");
    assert_eq!(sum.iter().filter(|line| line.starts_with("cmp ")).count(), 1, "{text}");
}

/// Seven is not zero, so the comparison and the jump on the flags stay.
#[test]
fn a_comparison_with_anything_else_keeps_its_flags() {
    let text = listing();
    let seven = body(&text, "seven");
    assert!(seven.contains(&"cmp w0, #7".to_string()), "{text}");
    assert!(seven.iter().any(|line| line.starts_with("b.ne ")), "{text}");
    assert!(!seven.iter().any(|line| line.starts_with("cb")), "{text}");
}

/// The null pointer `walk` compares against is the `#0` the `cbnz` reads, not a register, so the
/// loop writes no zero on each turn for nothing to read.
#[test]
fn the_null_pointer_is_not_written_into_the_loop() {
    let text = listing();
    let lines: Vec<&str> = text
        .lines()
        .skip_while(|line| *line != "walk:")
        .take_while(|line| !line.starts_with("\t.size"))
        .collect();
    let back = lines
        .iter()
        .find_map(|line| line.trim().strip_prefix("cbnz x1, "))
        .unwrap_or_else(|| panic!("{text}"));
    let top = lines
        .iter()
        .position(|line| *line == format!("{back}:"))
        .unwrap_or_else(|| panic!("{text}"));
    let turn = &lines[top..];
    assert!(
        !turn.iter().any(|line| line.starts_with("\tmov ") && line.ends_with(", #0")),
        "{text}"
    );
}

/// The integrated assembler takes the new instructions too.
#[test]
fn the_object_assembles() {
    let out = compile(&["-c"]);
    assert!(!out.stdout.is_empty());
}
