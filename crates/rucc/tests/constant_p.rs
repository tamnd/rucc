//! The branch a `__builtin_constant_p` answer rules out is gone before code generation.
//!
//! The kernel calls a function that does not exist on that branch: `__bad_size_call_parameter` in
//! `percpu.h`, `__compiletime_assert_N` behind `BUILD_BUG_ON`, and the FORTIFY `__write_overflow`.
//! gcc takes the branch out every time, so a call that survives here is a link error where gcc gave
//! none. Each shape below is one of those, reduced, and the object has to name none of them at
//! any level that optimizes. `g` is the kernel's `min`, whose signedness check asks about
//! `ret >= 0` after `ret < 0` has returned, which only the ranges answer. See tamnd/rucc#2265.

use std::process::Command;

const SOURCE: &str = r#"
extern void __bad_size_call_parameter(void);
extern void __compiletime_assert_1(void);
extern void __write_overflow(void);
extern void ok(int);
#define __always_inline inline __attribute__((always_inline))
static __always_inline void check(int n) {
    if (!__builtin_constant_p(n) || n < 8) ok(n); else __compiletime_assert_1();
}
static __always_inline unsigned long pcpu_read(void *p, int size) {
    unsigned long v;
    switch (size) {
    case 1: v = *(unsigned char *)p; break;
    case 2: v = *(unsigned short *)p; break;
    case 4: v = *(unsigned int *)p; break;
    case 8: v = *(unsigned long *)p; break;
    default: __bad_size_call_parameter(); v = 0; break;
    }
    return v;
}
static __always_inline void *fort_memset(void *p, int c, unsigned long n) {
    unsigned long sz = __builtin_object_size(p, 0);
    if (__builtin_constant_p(n) && sz != (unsigned long)-1 && n > sz) __write_overflow();
    return __builtin_memset(p, c, n);
}
#define BUILD_BUG_ON(c) do { if (c) __compiletime_assert_1(); } while (0)
static __always_inline int bit(unsigned nr) {
    BUILD_BUG_ON(!__builtin_constant_p(nr));
    return 1 << nr;
}
#define statically_true(x) (__builtin_constant_p(x) && (x))
int a(int x) { check(3); check(x); return x; }
unsigned long b(long *p, int *q) { return pcpu_read(p, sizeof *p) + pcpu_read(q, sizeof *q); }
void c(void) { char buf[16]; fort_memset(buf, 0, 8); ok(buf[1]); }
int d(void) { return bit(3) | bit(5); }
int e(int ret) { if (ret < 0) return ret; return statically_true(ret >= 0) ? 1 : 2; }
static __always_inline long rd(void *p, int size) {
    long v = 0;
    switch (__builtin_constant_p(size) ? size : 0) {
    case 4: v = *(int *)p; break;
    case 8: v = *(long *)p; break;
    default: __bad_size_call_parameter(); break;
    }
    return v;
}
long f(long *p) { return rd(p, 8); }
#define __is_nonneg(x) (__builtin_constant_p((x) >= 0) && ((x) >= 0))
#define min(a, b) ({ \
    if (!(__is_nonneg(a) || __builtin_types_compatible_p(typeof(a), typeof(b)))) \
        __compiletime_assert_1(); \
    (a) < (long)(b) ? (a) : (long)(b); })
long g(int ret) { char buf[16]; if (ret < 0) return ret; return min(ret, sizeof(buf)); }
"#;

#[test]
fn no_call_a_constant_p_answer_rules_out_reaches_the_object() {
    let dir = std::env::temp_dir().join(format!("rucc-constant-p-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=x86_64-unknown-linux-gnu", level, "-c", "-o"])
            .arg(dir.join("one.o"))
            .arg(dir.join("one.c"))
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(done.status.success(), "{level}: {}", String::from_utf8_lossy(&done.stderr));
        let bytes = std::fs::read(dir.join("one.o")).expect("the object was written");
        for name in ["__bad_size_call_parameter", "__compiletime_assert_1", "__write_overflow"] {
            let named = bytes.windows(name.len()).any(|at| at == name.as_bytes());
            assert!(!named, "{level} left a call to {name}");
        }
        assert!(
            bytes.windows(3).any(|at| at == b"ok\0"),
            "{level} took out the calls it should keep"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
