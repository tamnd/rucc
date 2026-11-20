//! A block reached only by a branch on a comparison does not make that comparison again on
//! x86-64, as gcc does not.
//!
//! `if (x == 50) ... else if (x > 50)` wrote `cmpl $50, %edi` in the entry block for the `je` and
//! again in the block the `je` falls to for the `jle`. The second block is reached from nowhere
//! else and the `je` writes none of the flags, so the answer is still there.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
void a(void); void b(void); void c(void);
void f(int x) { if (x == 50) a(); else if (x > 50) b(); else c(); }
void h(int x, int *p) { if (x == 50) a(); *p = 0; if (x > 50) b(); }
";

fn listing() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-x64-across-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-fno-pic", "-S", "-o", "-"])
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

fn compares(text: &str, name: &str) -> usize {
    body(text, name).iter().filter(|line| line.starts_with("cmp")).count()
}

#[test]
fn an_equality_and_an_order_on_the_same_values_are_one_comparison() {
    let text = listing();
    assert_eq!(compares(&text, "f"), 1, "{text}");
}

/// The block after the call to `a` is also reached from the call, which wrote the flags, so the
/// second comparison stays.
#[test]
fn a_block_with_a_second_way_in_makes_the_comparison_again() {
    let text = listing();
    assert_eq!(compares(&text, "h"), 2, "{text}");
}
