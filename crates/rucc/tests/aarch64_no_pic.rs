//! What `-fno-pic` reaches the AArch64 assembler as, which is a variable another object defines
//! reached with `adrp` and no slot, as gcc writes it.
//!
//! The linker copies such a variable into an executable that is not position independent, so the
//! distance to it is one the link knows. Before this the flag stopped short of the back end on
//! AArch64, and every `extern` variable the arm64 kernel reads cost a load from `.got` first.
//! A weak name nothing here defines is the exception, since it may be null and `adrp` cannot reach
//! null from a kernel's addresses, and gcc reads it out of a slot there too.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
extern int away;
static int quiet;
extern long many[];
extern int maybe __attribute__((weak));
extern int inside __attribute__((weak, visibility(\"hidden\")));
int read(void) { return away + quiet; }
long at(int i) { return many[i]; }
void write(int v) { away = v; quiet = v; }
int *weak(void) { return &maybe; }
int *hidden(void) { return &inside; }
";

fn listing(flags: &[&str]) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-no-pic-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .args(flags)
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
fn a_variable_another_object_defines_is_reached_directly() {
    for flags in [&["-fno-pic"][..], &["-fno-PIE"], &["-fPIE", "-fno-pie"]] {
        let text = listing(flags);
        for name in ["read", "at", "write", "hidden"] {
            let body = body(&text, name);
            assert!(!body.is_empty(), "{flags:?} {name}:\n{text}");
            assert!(!body.iter().any(|line| line.contains(":got")), "{flags:?} {name}:\n{text}");
        }
        let read = body(&text, "read");
        assert!(read.iter().any(|line| line.ends_with(", away")), "{flags:?}:\n{text}");
    }
}

#[test]
fn a_weak_name_nothing_here_defines_is_still_read_from_a_slot() {
    let text = listing(&["-fno-pic"]);
    assert!(body(&text, "weak").iter().any(|line| line.ends_with(":got:maybe")), "{text}");
}

/// What the flag changes, so the test above is known to be asking about something.
#[test]
fn a_position_independent_executable_reads_them_from_the_table() {
    let text = listing(&[]);
    assert!(body(&text, "read").iter().any(|line| line.contains(":got:away")), "{text}");
}
