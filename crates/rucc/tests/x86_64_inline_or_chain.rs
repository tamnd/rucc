//! A helper that is an `||` of tests returning `true` early goes into a caller for the speedup, as
//! gcc copies `recalc_sigpending_tsk` into `recalc_sigpending` in kernel/signal.c.
//!
//! The inliner weighs bodies as the front end wrote them. Three things there kept the helper a
//! call: the joins of the `||` hid the early return from the predictor, a `true` in one bit read as
//! a negative return, and the `unlikely` on the static branch in `freezing` was lost, so the call
//! to `freezing_slow_path` behind it counted as made one time in two.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = r#"
struct sig { unsigned long shared; };
struct task { unsigned long flags, jobctl, pending, blocked; struct sig *signal; };
struct key { int enabled; };
extern struct key freezer_active;
extern _Bool freezing_slow_path(struct task *);
extern _Bool cgroup_frozen(struct task *);
static inline __attribute__((always_inline)) _Bool arch_static_branch(struct key *key) {
    asm goto("1: jmp %l[l_yes] # %c0" : : "i"(key) : : l_yes);
    return 0;
l_yes:
    return 1;
}
static inline _Bool freezing(struct task *p) {
    if (__builtin_expect(!!arch_static_branch(&freezer_active), 0))
        return freezing_slow_path(p);
    return 0;
}
_Bool recalc(struct task *t) {
    if ((t->jobctl & 0x3f) || (t->pending & ~t->blocked) ||
        (t->signal->shared & ~t->blocked) || cgroup_frozen(t)) {
        asm volatile("lock orb %1, %0" : "+m"(t->flags) : "iq"((char)4));
        return 1;
    }
    return 0;
}
void recalc_sigpending(struct task *t) {
    t->pending ^= t->blocked >> 1;
    if (!recalc(t) && !freezing(t))
        if (__builtin_expect(t->flags & 4, 0))
            t->flags &= ~4ul;
}
"#;

fn listing() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-x64-or-chain-{}-{n}", std::process::id()));
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
fn the_helper_goes_into_its_caller_for_the_speedup() {
    let text = listing();
    // A tail call is a `jmp`, and the local labels have the name in them too.
    let calls: Vec<String> = body(&text, "recalc_sigpending")
        .into_iter()
        .filter(|line| line.starts_with("call") || line.starts_with("jmp"))
        .filter(|line| line.split_whitespace().last() == Some("recalc"))
        .collect();
    assert!(calls.is_empty(), "{calls:?}\n{text}");
    // The helper is not static, so it is still there for anyone else.
    assert!(text.contains("\nrecalc:"), "{text}");
}
