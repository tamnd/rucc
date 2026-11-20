//! An integer narrower than a word that a call passes on the stack is written as the full word, as
//! gcc writes it.
//!
//! Before, an `int` was written with a four byte store and the four bytes above it held what was
//! on the stack before. `syscall` reads each argument as a `long`, and LTP's `io_uring01` passes
//! the `_NSIG / 8` of `io_uring_enter` as an `int` in the seventh one. The kernel read a `size_t`
//! with garbage in its upper half, the wait failed with `EINVAL`, and the test found no completion.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
long syscall(long, ...);
long enter(int fd, unsigned a, unsigned b, unsigned c, void *sig)
{
	return syscall(426, fd, a, b, c, sig, 64 / 8);
}
void p(long, long, long, long, long, long, char, short, int);
void named(char c, short s, int i) { p(1, 2, 3, 4, 5, 6, c, s, i); }
";

fn listing(target: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-narrow-stack-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-fno-pic", "-S", "-o", "-"])
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

/// The stores into the outgoing area of a function, which are the ones through the stack pointer.
fn stack_stores(text: &str, name: &str) -> Vec<String> {
    body(text, name)
        .into_iter()
        .filter(|line| line.ends_with("(%rsp)") || line.ends_with("(%esp)"))
        .collect()
}

#[test]
fn a_narrow_integer_on_the_stack_is_written_as_a_full_word() {
    let text = listing("x86_64-unknown-linux-gnu");
    for name in ["enter", "named"] {
        let stores = stack_stores(&text, name);
        assert!(!stores.is_empty(), "{name}: {text}");
        for store in &stores {
            assert!(store.starts_with("movq"), "{name}: {store}\n{text}");
        }
    }
}

#[test]
fn a_narrow_integer_on_the_i386_stack_is_written_as_a_full_word() {
    let text = listing("i386-unknown-linux-gnu");
    for name in ["enter", "named"] {
        let stores = stack_stores(&text, name);
        assert!(!stores.is_empty(), "{name}: {text}");
        for store in &stores {
            assert!(store.starts_with("movl") || store.starts_with("pushl"), "{name}: {store}");
        }
    }
}
