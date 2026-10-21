//! What `__attribute__((alloc_size))` tells `__builtin_object_size`, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! Every answer below is what gcc 16 gives at `-O1` and up for the same source. Each one is checked
//! by a call to a function nothing defines under a test the answer makes false, so a right answer
//! is a call the optimizer took out and a wrong one is a name left in the object. That is the
//! shape the kernel's fortified string functions have, and it is what they need from this.

use std::process::Command;

const SOURCE: &str = r#"
typedef unsigned long size_t;
void *my(size_t) __attribute__((alloc_size(1)));
void *my2(size_t, size_t) __attribute__((alloc_size(1, 2)));
void *plain(size_t);
void * __attribute__((alloc_size(2))) __attribute__((malloc)) beside(void *, size_t);
void *cached(void *, size_t) __attribute__((alloc_size(2)));
void ok(void *);
void wrong_a(void);
void wrong_b(void);
void wrong_c(void);
void wrong_d(void);
void wrong_e(void);
void wrong_f(void);
void wrong_g(void);
void wrong_h(void);
void wrong_i(void);
void wrong_j(void);
void wrong_k(void);
void wrong_l(void);
/* Which allocator is settled only after the questions are answered, as in `kmalloc`. */
static inline __attribute__((always_inline)) void *pick(size_t n) {
    if (__builtin_constant_p(n) && n) return cached(0, n);
    return my(n);
}
static const char *ptr_of_11 = "this is 11!";
/* `check_mul_overflow` and `kvmalloc_array`, which give back NULL on the way that overflows. */
static inline __attribute__((always_inline)) _Bool checked(_Bool overflow) {
    return __builtin_expect(!!overflow, 0);
}
static inline __attribute__((always_inline)) void *array(size_t n, size_t size) {
    size_t bytes;
    if (__builtin_expect(!!checked(__builtin_mul_overflow(n, size, &bytes)), 0)) return 0;
    return my(bytes);
}
/* `kmemdup`, whose other way is only taken when the source is known to be too small. */
static inline __attribute__((always_inline)) void *dup(const void *p, size_t size) {
    size_t p_size = __builtin_dynamic_object_size(p, 0);
    if (p_size < size) return cached(0, 0);
    return cached(0, size);
}
void f(size_t n, const char *src) {
    char *p = my(10);
    if (__builtin_object_size(p, 0) != 10) wrong_a();
    if (__builtin_object_size(p + 4, 2) != 6) wrong_b();
    char *q = my2(3, 5);
    if (__builtin_dynamic_object_size(q, 0) != 15) wrong_c();
    char *r = plain(10);
    if (__builtin_object_size(r, 0) != (size_t)-1) wrong_d();
    /* The operand is not evaluated, so there is no call for the size to come from. */
    if (__builtin_object_size(my(12), 0) != (size_t)-1) wrong_e();
    /* Not a constant, so only the dynamic spelling has an answer, and it is the argument. */
    char *s = my(n);
    if (__builtin_object_size(s, 0) != (size_t)-1) wrong_f();
    if (__builtin_dynamic_object_size(s, 0) != n) wrong_g();
    /* Beside the star, where the kernel writes it on `devm_kmalloc`. */
    char *t = beside(0, 7);
    if (__builtin_object_size(t, 0) != 7) wrong_h();
    char *u = pick(n);
    if (__builtin_dynamic_object_size(u, 0) != n) wrong_i();
    /* Read out of a pointer nothing writes, as fortify_kunit's known sizes test does. */
    if (__builtin_object_size(ptr_of_11, 1) != 12) wrong_j();
    char *v = array(1, 1 * (1UL << 12));
    if (__builtin_object_size(v, 0) != 4096) wrong_k();
    char *w = dup(src, n);
    if (__builtin_dynamic_object_size(w, 1) != n) wrong_l();
    ok(p);
    ok(q);
    ok(r);
    ok(s);
    ok(t);
    ok(u);
    ok(v);
    ok(w);
}
"#;

#[test]
fn an_alloc_size_answers_the_object_size_of_what_the_call_returned() {
    let dir = std::env::temp_dir().join(format!("rucc-alloc-size-{}", std::process::id()));
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
        assert!(!bytes.windows(6).any(|at| at == b"wrong_"), "{level} answered one wrong");
        assert!(bytes.windows(3).any(|at| at == b"ok\0"), "{level} took out the calls to keep");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
