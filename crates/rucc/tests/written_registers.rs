//! A value that lives across a call to a function this file has already compiled stays in a
//! register the callee does not write, as gcc's `-fipa-ra` keeps it, rather than in one the caller
//! saves in its prologue. Linux's `vfs_read` and `ksys_read` each pushed two more registers than
//! gcc's for that reason, on the path every `read` takes.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Each compilation gets a file of its own, since the tests run at once.
static NEXT: AtomicUsize = AtomicUsize::new(0);

const SOURCE: &str = "\
__attribute__((noinline)) TAG int scale(int x) { return x * 3; }
int caller(int a, int b) { int t = scale(a); return t + b; }
";

fn compile(tag: &str, flags: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-written-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join(format!("one{}.c", NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&path, SOURCE.replace("TAG", tag)).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// The lines of `caller`, from its label to the end of the text.
fn caller(text: &str) -> Vec<&str> {
    text.lines().skip_while(|line| *line != "caller:").collect()
}

#[test]
fn a_value_across_a_call_to_a_known_function_is_not_saved() {
    for level in ["-O2", "-Os"] {
        let text = compile("", &[level]);
        let body = caller(&text);
        assert!(body.iter().any(|line| line.contains("call")), "{level}\n{text}");
        assert!(!body.iter().any(|line| line.contains("push")), "{level}\n{text}");
    }
}

/// A weak callee may be another object's code by the time the program runs, a function `-pg`
/// hooks may be patched, and `-O1` is not a level gcc does this at.
#[test]
fn a_callee_that_may_be_other_code_is_taken_to_write_everything() {
    for (tag, flags) in [
        ("__attribute__((weak))", &["-O2"][..]),
        ("", &["-O2", "-pg"][..]),
        ("", &["-O1"][..]),
        ("", &["-O2", "-fPIC"][..]),
    ] {
        let text = compile(tag, flags);
        let body = caller(&text);
        assert!(body.iter().any(|line| line.contains("push")), "{tag} {flags:?}\n{text}");
    }
}
