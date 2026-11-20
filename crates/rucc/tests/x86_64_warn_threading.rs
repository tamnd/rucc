//! The test in a kernel `WARN_ON` of an `&&` is threaded, so no truth value is worked out first.
//!
//! `WARN_ON` hands back the value it tested, through a second `__builtin_expect` nobody reads.
//! That read was still there when jump threading ran, since dead code elimination came much later,
//! so threading saw a block below reading the `&&` and left it alone. The `&&` was worked out into
//! a register with `sete` and tested again. `fpregs_assert_state_consistent` in
//! arch/x86/kernel/fpu/core.c does this on every return to user space, and gcc branches straight
//! from the two compares.

use std::process::Command;

const SOURCE: &str = "\
struct fpu { unsigned int last_cpu; };
extern struct fpu *owner;
extern int cpu;
#define W(c) ({ int __r = !!(c); if (__builtin_expect(!!(__r), 0)) asm volatile(\"ud2\"); \\
    __builtin_expect(!!(__r), 0); })
static inline int valid(struct fpu *fpu, unsigned int c) {
    return fpu == *(volatile __typeof__(owner) *)&owner && c == fpu->last_cpu;
}
void check(struct fpu *fpu) { W(!valid(fpu, cpu)); }
";

fn listing() -> String {
    let dir = std::env::temp_dir().join(format!("rucc-x64-warn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("warn.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn the_and_is_branched_on_not_worked_out() {
    let text = listing();
    let body: Vec<&str> = text
        .lines()
        .skip_while(|line| *line != "check:")
        .take_while(|line| !line.starts_with("\t.size"))
        .map(str::trim)
        .collect();
    assert!(!body.is_empty(), "{text}");
    assert!(!body.iter().any(|line| line.starts_with("set") || line.starts_with("xor")), "{text}");
    let jumps = body.iter().filter(|line| line.starts_with('j')).count();
    assert_eq!(jumps, 2, "{text}");
}
