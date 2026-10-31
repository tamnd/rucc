//! A number written into a register that holds it already is left out on x86-64, as gcc writes it.
//!
//! A walk over a list that counts from zero wrote `xorl %ecx, %ecx` in the entry block for the path
//! that skips the loop and again in the block in front of the loop. The allocator gives both the
//! same register and nothing in between writes it, so the second one is taken out. A number read
//! on both sides of a call is still written again after the call.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
struct node { struct node *next; int x; };
int count(struct node *h, int k) {
    int n = 0;
    for (struct node *p = h->next; p != h; p = p->next) n += p->x == k;
    return n;
}
extern void g(long);
void two(void) { g(0); g(0); }
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-x64-held-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-fno-pic", "-o", "-"])
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
fn a_zero_is_written_once() {
    let text = listing();
    let body = body(&text, "count");
    let zeros: Vec<&String> = body.iter().filter(|line| line.starts_with("xorl\t")).collect();
    let mut unique = zeros.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(zeros.len(), unique.len(), "count writes a zero twice:\n{text}");
}

/// A call may write any register it does not owe back, so the argument is zeroed again for the
/// second call.
#[test]
fn a_number_is_written_again_after_a_call() {
    let text = listing();
    let two = body(&text, "two");
    assert_eq!(two.iter().filter(|line| *line == "xorl\t%edi, %edi").count(), 2, "{text}");
}

/// The integrated assembler takes the function as well.
#[test]
fn the_object_assembles() {
    let out = compile(&["-c"]);
    assert!(!out.stdout.is_empty());
}
