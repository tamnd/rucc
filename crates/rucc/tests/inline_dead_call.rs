//! A `static` function called once where it can run and once more behind a branch that never
//! runs is inlined into the one call that can, the way gcc inlines it. In the kernel's
//! mm/memory.c the second call to `do_pte_missing` is behind `unlikely(!userfaultfd_wp(vma))`,
//! which is false without `CONFIG_USERFAULTFD`, and counting it kept `do_anonymous_page` and
//! `do_fault` out of line.

use std::process::Command;

const SOURCE: &str = "\
struct vma { long flags; long a, b; };
extern void work(long);
static inline _Bool wp(struct vma *v) { return 0; }
static long anon(struct vma *v, long x) {
    for (int i = 0; i < 4; i++) work(x + i * v->a);
    work(v->b); work(v->flags); work(x * 3); work(x ^ v->a);
    if (v->flags & 4) work(1); else work(2);
    return x + v->b;
}
static long missing(struct vma *v, long x) { if (v->a) return anon(v, x); return x; }
static long marker(struct vma *v, long x) {
    if (__builtin_expect(!!(!wp(v)), 0)) return 7;
    return missing(v, x);
}
long fault(struct vma *v, long x) { if (x & 1) return marker(v, x); return missing(v, x); }
";

#[test]
fn a_call_behind_a_branch_that_never_runs_is_not_counted() {
    let dir = std::env::temp_dir().join(format!("rucc-inline-dead-call-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("assembly is text");
    assert!(text.lines().any(|line| line == "fault:"), "{text}");
    assert!(!text.lines().any(|line| line == "anon:"), "{text}");
}
