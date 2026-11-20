//! A test a branch above already decided is threaded through a copy before if-conversion, as gcc
//! threads `___update_load_sum` in kernel/sched/pelt.c.
//!
//! `if (!load) runnable = running = 0;` and then tests of `load`, `runnable` and `running` used to
//! leave `runnable` and `running` as conditional moves on `load` that were then tested, so the way
//! in from `load == 0` worked out two zeros and tested them. gcc goes straight to the return.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
struct sa { unsigned long a, b, c; };
int f(struct sa *s, unsigned long load, unsigned long runnable, int running, unsigned long d)
{
    if (!load)
        runnable = running = 0;
    if (load)
        s->a += load * d;
    if (runnable)
        s->b += runnable * d;
    if (running)
        s->c += d << 10;
    return 1;
}
";

fn listing() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-x64-implied-{}-{n}", std::process::id()));
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

#[test]
fn each_value_is_tested_once_and_none_is_a_conditional_move() {
    let text = listing();
    let body = body(&text, "f");
    assert!(!body.iter().any(|line| line.starts_with("cmov")), "{text}");
    assert_eq!(body.iter().filter(|line| line.starts_with("test")).count(), 3, "{text}");
}
