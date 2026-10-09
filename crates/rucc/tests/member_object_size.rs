//! What `__builtin_object_size` says about a member once the function asking has been inlined.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The kernel's fortified `strcpy` asks `__builtin_object_size (p, 1)` about its own parameter, so
//! the question is only answered after the call is inlined, and by then the address is an add to
//! the start of the structure. Lowering marks that add with the size of the member it steps to,
//! and the answer for the closest member is that size, as it is from gcc. Before the mark the
//! answer was what was left of the whole structure, and `lib/test_fortify` builds that the kernel
//! expects to be refused went through.
//!
//! A member that may run past its end, an array at the end of a structure or a record that ends in
//! a flexible array, is answered as the whole object even where the address it is in was worked
//! out from another member. bcachefs's `bkey_xattr_init` takes `container_of` on `&_k->k` and
//! clears the `v` after it, and when the answer was what was left of `k`, which is nothing, the
//! fortified `memset` refused the build. pm8001 casts the address of a pointer member to a structure
//! and copies into the flexible array at its end, which is the same question.
//!
//! Every answer below is what gcc 16 gives at `-O2` for the same source. Each one is checked by a
//! call to a function declared with `error`, under a test the right answer makes false, so a
//! wrong answer is a refused build that names it.

use std::path::PathBuf;
use std::process::{Command, Output};

const SOURCE: &str = r#"
typedef unsigned long size_t;
struct in { int x; char y[8]; };
struct o { int a; char buf[16]; struct in inner; int c; char tail[4]; } inst;
struct o *gp;
struct fl { int n; char d[]; };
struct mid { long a; struct fl in; };
struct holder { int x; struct mid m; } *hp;
struct key { long a, b, c; char t; };
struct ki { struct key k; long v[]; } *kp;
struct fx { char t, n; short l; char name[]; };
struct kx { union { struct key k; struct ki k_i; }; struct fx v; };
struct payload { int sig; short id; long *specific; };
struct control { int ret, len; char buffer[]; };
extern void *grab(size_t) __attribute__((alloc_size(1)));
#define container_of(p, T, m) ((T *)((char *)(p) - __builtin_offsetof(T, m)))
#define I static inline __attribute__((always_inline))
I size_t q1(const void *p) { return __builtin_object_size(p, 1); }
I size_t q3(const void *p) { return __builtin_object_size(p, 3); }
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
void f(int i) {
    WANT(wrong_buf, q1(inst.buf), 16);
    WANT(wrong_index, q1(&inst.buf[2]), 14);
    WANT(wrong_cast, q1((char *)&inst.inner + 6), 6);
    WANT(wrong_first, q1(&inst.a), 4);
    WANT(wrong_arrow, q1(gp->buf), 16);
    WANT(wrong_tail, q1(gp->tail), -1);
    WANT(wrong_inner, q1(&gp->inner), 12);
    WANT(wrong_nested, q1(gp->inner.y), 8);
    WANT(wrong_past, q1((char *)gp->buf + 20), 0);
    WANT(wrong_least, q3(inst.buf), 16);
    WANT(wrong_least_arrow, q3(gp->buf), 16);
    WANT(wrong_unknown, q1(&inst.buf[i]), -1);
    WANT(wrong_whole, q1(&inst), 40);
    char *p = inst.buf;
    WANT(wrong_local, q1(p + 4), 12);
    WANT(wrong_flex_holder, q1(&hp->m), -1);
    WANT(wrong_flex_inner, q1(&hp->m.in), -1);
    int k = 1;
    WANT(wrong_counted, q1(gp->buf + k), 16);
    WANT(wrong_counted_object, q1(inst.buf + k), 16);
    WANT(wrong_counted_whole, q1((char *)&inst + 30 + k), 9);
    struct kx *open = container_of(&kp->k, struct kx, k);
    WANT(wrong_open_unknown, q1(&open->v), -1);
    struct ki *heap = grab(64);
    struct kx *held = container_of(&heap->k, struct kx, k);
    WANT(wrong_open_heap, q1(&held->v), 32);
    WANT(wrong_open_name, q1(held->v.name), 28);
    struct payload *pay = grab(128);
    struct control *ctl = (struct control *)&pay->specific;
    WANT(wrong_cast_flex, q1(ctl->buffer), 112);
}
"#;

/// The kernel's `write_overflow-strncpy.c`, cut down to the fortified `strncpy` it calls.
const OVERFLOW: &str = r#"
typedef unsigned long size_t;
extern void __write_overflow(void)
    __attribute__((__error__("detected write beyond size of object (1st parameter)")));
extern void fortify_panic(void) __attribute__((noreturn));
extern inline __attribute__((always_inline)) __attribute__((gnu_inline))
char *strncpy(char *const p, const char *q, size_t size) {
    const size_t p_size = __builtin_object_size(p, 1);
    if (__builtin_constant_p(p_size < size) && p_size < size)
        __write_overflow();
    if (p_size < size)
        fortify_panic();
    return __builtin_strncpy(p, q, size);
}
struct fortify_object { int a; char buf[16]; int c; } instance;
const char large_src[32] = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
void f(void) { strncpy(instance.buf, large_src, sizeof(instance.buf) + 1); }
"#;

fn compile(name: &str, source: &str, level: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("rucc-member-size-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", level, "-c", "-o"])
        .arg(dir.join("one.o"))
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn the_closest_member_is_answered_after_inlining() {
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let out = compile(&format!("ok{level}"), SOURCE, level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn a_fortified_copy_past_the_member_is_refused() {
    let out = compile("overflow", OVERFLOW, "-O2");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the overflow went through");
    assert!(said.contains("detected write beyond size of object"), "{said}");
}
