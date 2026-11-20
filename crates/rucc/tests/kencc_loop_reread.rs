//! A load in a loop that writes nothing reads memory again on each pass under `-fkencc`, end to
//! end.
//!
//! 9front's `ilock` waits with `while(l->key);` and `key` is not `volatile`. kencc reads `l->key`
//! on each pass. A compiler that reads it one time before the loop makes a kernel that hangs at the
//! first busy lock.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "typedef struct L { unsigned long key; int x; } L;\n\
                      extern int flag;\n\
                      static int peek(L *l) { return l->x; }\n\
                      void spin(L *l) { while(l->key) ; }\n\
                      void global(void) { while(flag) ; }\n\
                      void pure(L *l) { while(l->key) peek(l); }\n\
                      void count(L *l, int *n) { int i = 0; while(l->key) i++; *n = i; }\n";

/// A directory of this test's own, empty.
fn dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-kencc-reread-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The assembly of [`SOURCE`] for `target` with `flags`.
fn assembly(target: &str, flags: &[&str]) -> String {
    let dir = dir();
    std::fs::write(dir.join("a.c"), SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(flags)
        .args(["-O2", "-fno-asynchronous-unwind-tables", "-S", "-o", "-", "a.c"])
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether each loop of `func` reads memory between its label and the branch back to it. A loop
/// with no read of memory in it is a loop that read the value before it.
fn loops_read_memory(asm: &str, func: &str) -> bool {
    let lines: Vec<&str> = asm.lines().collect();
    let start = lines.iter().position(|l| *l == format!("{func}:")).expect("the function is there");
    let body: Vec<&str> =
        lines[start..].iter().take_while(|l| !l.contains(".size")).copied().collect();
    let mut found = false;
    for (at, line) in body.iter().enumerate() {
        let Some(label) = line.strip_suffix(':').filter(|l| l.starts_with(".L")) else { continue };
        let Some(back) = body[at..].iter().position(|l| l.trim_start().ends_with(label)) else {
            continue;
        };
        found = true;
        if !body[at..=at + back].iter().any(|l| l.contains('(') || l.contains('[')) {
            return false;
        }
    }
    found
}

#[test]
fn a_spin_loop_reads_memory_on_each_pass() {
    for target in ["x86_64-unknown-plan9", "aarch64-unknown-plan9"] {
        let asm = assembly(target, &["-fkencc"]);
        for func in ["spin", "global", "pure", "count"] {
            assert!(loops_read_memory(&asm, func), "{target} {func}:\n{asm}");
        }
    }
}

#[test]
fn without_kencc_the_load_still_leaves_the_loop() {
    let asm = assembly("x86_64-unknown-linux-gnu", &[]);
    assert!(!loops_read_memory(&asm, "spin"), "{asm}");
}
