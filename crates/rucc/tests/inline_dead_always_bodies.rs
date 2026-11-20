//! An `always_inline` body that the first inliner has copied into every caller is gone before the
//! second inliner measures the unit, as gcc removes what its early inliner left unreachable before
//! `inline_small_functions`. So the dead body neither counts toward the size the unit may grow from
//! nor has calls copied into it.
//!
//! Linux 7.3's mm/mprotect.c is this shape. Its four `__always_inline` helpers stayed in the unit
//! after they were copied, the second inliner copied `pte_val` and `__pte` into them, and the
//! unit reached its growth limit before the live calls in `change_pte_range` were weighed. 189
//! calls stayed out of line, and `pte_needs_flush` kept a call to `pte_val`. gcc inlines all of
//! them.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned long u64;
#define L \"nop\\n\\t\"
#define L10 L L L L L L L L L L
static inline u64 val(u64 x)
{
	u64 r;
	asm volatile(L10 L10 L10 L10 L10 \"mov %1, %0\" : \"=r\"(r) : \"r\"(x));
	return r;
}
#define V1(i) s += val(p[i]);
#define V10(i) V1(i) V1(i+1) V1(i+2) V1(i+3) V1(i+4) V1(i+5) V1(i+6) V1(i+7) V1(i+8) V1(i+9)
#define V40(i) V10(i) V10(i+10) V10(i+20) V10(i+30)
#define HELPER(n) \\
static __attribute__((always_inline)) inline u64 helper##n(u64 *p) \\
{ u64 s = 0; V40(0) return s; } \\
u64 entry##n(u64 *p) { return helper##n(p); }
HELPER(0) HELPER(1) HELPER(2) HELPER(3) HELPER(4)
static inline int needs(u64 a, u64 b) { return val(a) != val(b); }
int last(u64 a, u64 b) { return needs(a, b); }
";

/// The IR the compiler produced for the source at `-O2`.
fn ir() -> String {
    let dir = std::env::temp_dir().join(format!("rucc-inline-dead-bodies-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-linux-gnu", "-O2", "--emit=ir", "-w", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_dead_always_inline_body_does_not_use_up_the_unit_growth() {
    let text = ir();
    let calls =
        text.lines().filter(|line| line.contains("call") && line.contains(" @val(")).count();
    assert_eq!(calls, 0, "every call to val is inlined, as gcc does\n{text}");
    let body = text.lines().any(|line| line.starts_with("func @helper0(") && line.ends_with('{'));
    assert!(!body, "the copied helper has no body of its own\n{text}");
}
