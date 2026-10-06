//! A number written into a register that holds it already is left out on AArch64, as gcc writes it.
//!
//! A constant is written in each block that wants it, so a loop whose sum starts at zero wrote
//! `mov x2, #0` in the entry block for the path that skips the loop and again in the block in
//! front of the loop. The allocator gives both the same register and nothing in between writes
//! it, so the second one is taken out.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
long sum(int *a, unsigned n) { long s = 0; for (unsigned i = 0; i < n; i++) s += a[i]; return s; }
long count(long *a, long n) { long s = 0; for (long i = 0; i < n; i++) s += a[i] > 0; return s; }
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-held-{}-{n}", std::process::id()));
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
fn a_number_is_written_once() {
    let text = listing();
    for name in ["sum", "count"] {
        let body = body(&text, name);
        let zeros: Vec<&String> =
            body.iter().filter(|line| line.starts_with("mov ") && line.ends_with(", #0")).collect();
        let regs: Vec<&str> = zeros.iter().map(|line| &line[4..6]).collect();
        let mut unique = regs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(regs.len(), unique.len(), "{name} writes a zero twice:\n{text}");
    }
}

/// The integrated assembler takes the function as well.
#[test]
fn the_object_assembles() {
    let out = compile(&["-c"]);
    assert!(!out.stdout.is_empty());
}
