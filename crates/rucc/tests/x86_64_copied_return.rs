//! A return copied over every jump to it leaves nothing behind that no path reaches.
//!
//! `set_worker_dying` in kernel/workqueue.c of Linux 7.3 returns early from three `WARN_ON` tests,
//! and the block that clears `last_woken_worker` is laid out below the return and jumps back up.
//! The layout copied the `ret` into each block that jumped to it and kept the block it came from,
//! which nothing fell into, so objtool said it was an unreachable instruction. The fixture is the
//! function with the kernel's types made small.

use std::process::Command;

const SOURCE: &str = "\
struct pool { int workers, idle; void *last; };
struct worker { long current; struct pool *pool; int flags; int *count; struct worker *next; };
void saturate(int *count, int how);
#define unlikely(x) __builtin_expect(!!(x), 0)
#define WARN(x, n) ({ int c = !!(x); if (unlikely(c)) __asm__ volatile(\"ud2\\n\\t.byte \" #n); \\
    unlikely(c); })
void dying(struct worker *worker) {
    struct pool *pool = worker->pool;
    if (WARN(worker->current, 1) || WARN(worker->next != worker, 2) ||
        WARN(!(worker->flags & 4), 3))
        return;
    pool->workers--;
    pool->idle--;
    if (pool->last == worker)
        pool->last = 0;
    worker->flags |= 2;
    int old = __atomic_fetch_add(worker->count, 1, __ATOMIC_RELAXED);
    if (unlikely(old == 0))
        saturate(worker->count, 2);
    else if (unlikely((unsigned)old >= 0x7fffffff))
        saturate(worker->count, 1);
}
";

fn listing(extra: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-x64-copied-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join(format!("copied{}.c", extra.len()));
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .args(extra)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// The labels in the body that come straight after a jump or a return and have an instruction
/// under them, but that no instruction in the body names.
fn unreached(body: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    let mut ended = false;
    let mut label: Option<&str> = None;
    for line in body {
        if let Some(name) = line.strip_suffix(':').filter(|name| name.starts_with(".L")) {
            label = if ended { Some(name) } else { None };
            continue;
        }
        if line.starts_with('.') {
            continue;
        }
        if let Some(name) = label.take() {
            let named = body.iter().any(|other| {
                !other.ends_with(':') && other.split([' ', '\t', ',']).any(|word| word == name)
            });
            if !named {
                found.push(name.to_string());
            }
        }
        ended = line.starts_with("jmp") || line.starts_with("ret");
    }
    found
}

fn check(extra: &[&str]) {
    let text = listing(extra);
    let body: Vec<&str> = text
        .lines()
        .skip_while(|line| *line != "dying:")
        .take_while(|line| !line.starts_with("\t.size"))
        .map(str::trim)
        .collect();
    assert!(body.iter().any(|line| line.starts_with("ud2")), "{text}");
    assert_eq!(unreached(&body), Vec::<String>::new(), "{text}");
}

#[test]
fn the_return_left_behind_goes() {
    check(&[]);
}

#[test]
fn the_return_left_behind_goes_through_the_thunk() {
    check(&["-mfunction-return=thunk-extern"]);
}
