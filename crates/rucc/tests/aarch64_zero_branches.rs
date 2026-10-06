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
    ] {
        let body = body(&text, name);
        assert!(body.iter().any(|line| line.starts_with(branch)), "{name} {branch}:\n{text}");
        assert!(!body.iter().any(|line| line.starts_with("cmp ")), "{name}:\n{text}");
        assert!(!body.iter().any(|line| line.starts_with("b.")), "{name}:\n{text}");
    }
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

/// The integrated assembler takes the new instructions too.
#[test]
fn the_object_assembles() {
    let out = compile(&["-c"]);
    assert!(!out.stdout.is_empty());
}
