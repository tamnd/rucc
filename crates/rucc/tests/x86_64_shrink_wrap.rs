//! A function whose likely path needs none of the registers its prologue saves does not save them
//! on that path, as gcc does not at `-O1` and up.
//!
//! `mas_ascend` in lib/maple_tree.c pushed two registers on every call for a loop it leaves at
//! once most of the time. The prologue now goes at the top of the block every block that wants it
//! is behind, and the way around it returns with a bare `ret`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
extern long slow(long *);
long f(long *p, long x) {
    if (__builtin_expect(x == 0, 1))
        return p[0];
    long a = slow(p);
    long b = slow(p + 1);
    return a + b + x;
}
long h(long *p, long x) {
    if (__builtin_expect(x != 0, 1)) {
        long a = slow(p);
        return a + slow(p + 1) + x;
    }
    return p[0];
}
long walk(long *p, long n) {
    if (n < 0)
        return 0;
    long t = 0;
    for (long i = 0; i < n; i++)
        t += slow(p + i);
    if (n == 7)
        return 1;
    return t;
}
long g(long *p, long x) {
    long a = slow(p);
    if (x)
        a += slow(p + 1);
    return a + x;
}
";

fn listing() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-x64-wrap-{}-{n}", std::process::id()));
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

/// The lines of one function, labels and directives included.
fn lines(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .map(|line| line.trim().to_string())
        .collect()
}

fn pushes_before_branch(text: &str, name: &str) -> usize {
    lines(text, name)
        .iter()
        .take_while(|line| !line.starts_with('j'))
        .filter(|line| line.starts_with("push"))
        .count()
}

#[test]
fn the_likely_path_saves_nothing() {
    let text = listing();
    assert_eq!(pushes_before_branch(&text, "f"), 0, "{text}");
    assert!(lines(&text, "f").iter().any(|line| line.starts_with("push")), "{text}");
}

/// The rows say where the return address is from every instruction, which is the frame's size
/// on the way through the prologue and nothing once it is given back. Read in the order of the
/// listing, as the unwinder reads them, the distance is 8 at each `ret` and a multiple of 16 at each
/// `call`, which it is not when a block after a return was left without the rows it runs under.
fn check_rows(text: &str, name: &str) {
    let mut cfa = 8;
    let mut kept = Vec::new();
    for line in lines(text, name) {
        if let Some(at) = line.strip_prefix(".cfi_def_cfa_offset ") {
            cfa = at.parse().expect("a distance");
        } else if line == ".cfi_remember_state" {
            kept.push(cfa);
        } else if line == ".cfi_restore_state" {
            cfa = kept.pop().expect("a remembered state");
        } else if line == "ret" {
            assert_eq!(cfa, 8, "{name}\n{text}");
        } else if line.starts_with("call") {
            assert_eq!(cfa % 16, 0, "{name}\n{text}");
        }
    }
}

#[test]
fn the_unwind_rows_follow_the_frame() {
    let text = listing();
    for name in ["f", "h", "walk", "g"] {
        check_rows(&text, name);
    }
    assert_eq!(pushes_before_branch(&text, "walk"), 0, "{text}");
}

/// A function that calls on every path has its prologue where it was.
#[test]
fn a_call_on_every_path_keeps_the_prologue_at_the_top() {
    let text = listing();
    assert!(pushes_before_branch(&text, "g") > 0, "{text}");
}
