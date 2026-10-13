//! The kernel's `scoped_seqlock_read`, whose step calls `__scoped_seqlock_bug` in a state the loop
//! test has already ruled out. Nothing defines that function, so a call to it that is still there
//! is a link failure, and gcc removes it whatever the body of the loop holds.

use std::fmt::Write;
use std::process::Command;

const HEADER: &str = "\
typedef struct { int lock; unsigned seq; } seqlock_t;
enum ss_state { ss_done = 0, ss_lock, ss_lock_irqsave, ss_lockless };
struct ss_tmp { enum ss_state state; unsigned long data; int *lock; int *lock_irqsave; };
extern void spin_lock(int *);
extern void spin_unlock(int *);
extern unsigned long spin_lock_irqsave(int *);
extern void spin_unlock_irqrestore(int *, unsigned long);
static inline __attribute__((always_inline)) unsigned read_seqbegin(seqlock_t *l)
{ return *(volatile unsigned *)&l->seq & ~1u; }
static inline __attribute__((always_inline)) int read_seqretry(seqlock_t *l, unsigned s)
{ return *(volatile unsigned *)&l->seq != s; }
static inline __attribute__((always_inline)) void cleanup(struct ss_tmp *sst)
{
	if (sst->lock) spin_unlock(sst->lock);
	if (sst->lock_irqsave) spin_unlock_irqrestore(sst->lock_irqsave, sst->data);
}
extern void __scoped_seqlock_invalid_target(void);
extern void __scoped_seqlock_bug(void);
static inline __attribute__((always_inline)) void
next(struct ss_tmp *sst, seqlock_t *lock, enum ss_state target)
{
	switch (sst->state) {
	case ss_done: __scoped_seqlock_bug(); return;
	case ss_lock: case ss_lock_irqsave: sst->state = ss_done; return;
	case ss_lockless:
		if (!read_seqretry(lock, sst->data)) { sst->state = ss_done; return; }
		break;
	}
	switch (target) {
	case ss_done: __scoped_seqlock_invalid_target(); return;
	case ss_lock: sst->lock = &lock->lock; spin_lock(sst->lock); sst->state = ss_lock; return;
	case ss_lock_irqsave:
		sst->lock_irqsave = &lock->lock;
		sst->data = spin_lock_irqsave(sst->lock_irqsave);
		sst->state = ss_lock_irqsave;
		return;
	case ss_lockless: sst->data = read_seqbegin(lock); return;
	}
}
#define scoped_seqlock_read(l, target) \\
	for (struct ss_tmp s __attribute__((cleanup(cleanup))) = \\
	     { .state = ss_lockless, .data = read_seqbegin(l) }; \\
	     s.state != ss_done; next(&s, l, target))
extern void use(long);
struct sig { seqlock_t stats_lock; long x[100]; long total; };
";

/// The header and a reader whose critical section makes that many tests, each with a call behind
/// it so that none of them becomes a select.
fn source(tests: usize) -> String {
    let mut text = String::from(HEADER);
    text.push_str("long f(struct sig *sig)\n{\n\tlong total = 0;\n");
    text.push_str("\tscoped_seqlock_read(&sig->stats_lock, ss_lock_irqsave) {\n");
    text.push_str("\t\ttotal = sig->total;\n");
    for at in 0..tests {
        writeln!(text, "\t\tif (sig->x[{at}] > {at}) use({at});").expect("a string takes text");
    }
    text.push_str("\t}\n\treturn total;\n}\n");
    text
}

fn listing(target: &str, text: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-scoped-seqlock-{}-{target}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("array.c");
    std::fs::write(&path, text).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-fno-strict-aliasing", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn the_impossible_state_goes_whatever_the_critical_section_tests() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for tests in [0, 80] {
            let text = listing(target, &source(tests));
            assert!(!text.contains("__scoped_seqlock_bug"), "{target} with {tests} tests\n{text}");
        }
    }
}
