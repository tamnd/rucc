//! What `__builtin_dynamic_object_size` says about a flexible array member with `counted_by`.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! gcc 16 answers every kind of the dynamic spelling with the count times the size of an element,
//! read where the program takes the address of the array, less how far into the array the address
//! is and never below zero. A signed count below zero is no elements. The kernel's
//! `overflow.DEFINE_FLEX_test` puts a structure with a count of zero in a buffer on the stack that
//! has room for two elements and wants the answer to be zero, which is the count and not the room.
//!
//! Each answer below is what gcc 16 gives at `-O2` for the same source. Each one is checked by a
//! call that only a wrong answer leaves in the object, in a function of its own, so that a call
//! left by one wrong answer does not keep the next from being folded.

use std::process::Command;

const SOURCE: &str = r#"
typedef unsigned long size_t;
struct s { int n; short x[] __attribute__((counted_by(n))); };
struct u { unsigned char n; int x[] __attribute__((counted_by(n))); };
struct l { long n; char x[] __attribute__((counted_by(n))); };
void ok(void *);
#define WANT(name, got, want) \
    do { \
        extern void name(void); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
#define FIVE(name, got, want) \
    void ask_##name(struct s *p) { p->n = 5; WANT(name, got, want); ok(p); }
FIVE(wrong_kind_0, __builtin_dynamic_object_size(p->x, 0), 10)
FIVE(wrong_kind_1, __builtin_dynamic_object_size(p->x, 1), 10)
FIVE(wrong_kind_2, __builtin_dynamic_object_size(p->x, 2), 10)
FIVE(wrong_kind_3, __builtin_dynamic_object_size(p->x, 3), 10)
FIVE(wrong_index, __builtin_dynamic_object_size(&p->x[2], 1), 6)
FIVE(wrong_past, __builtin_dynamic_object_size(p->x + 7, 1), 0)
FIVE(wrong_static, __builtin_object_size(p->x, 1), -1)
/* The count is read where the address of the array is taken. */
void taken(struct s *p) {
    p->n = 5;
    short *at = p->x;
    p->n = 7;
    WANT(wrong_taken, __builtin_dynamic_object_size(at, 1), 10);
    ok(p);
}
void negative(struct s *p) {
    p->n = -3;
    WANT(wrong_negative, __builtin_dynamic_object_size(p->x, 1), 0);
    ok(p);
}
void narrow(struct u *q) {
    q->n = 200;
    WANT(wrong_unsigned, __builtin_dynamic_object_size(q->x, 1), 800);
    ok(q);
}
/* gcc 16 cuts a signed count of eight bytes to an `int`. */
void wide(struct l *r) {
    r->n = 0x100000002L;
    WANT(wrong_long, __builtin_dynamic_object_size(r->x, 1), 2);
    ok(r);
}
/* gcc has no answer for a join between two of them. */
void join(struct s *p, int k) {
    p->n = 5;
    WANT(wrong_join, __builtin_dynamic_object_size(k ? p->x : p->x + 2, 1), -1);
    ok(p);
}
/* The shape of the kernel's DEFINE_FLEX: room for two elements and a count of zero. */
void flex(void) {
    union { char bytes[sizeof(struct s) + 2 * sizeof(short)]; struct s obj; } held;
    struct s *p = &held.obj;
    p->n = 0;
    WANT(wrong_flex_0, __builtin_dynamic_object_size(p->x, 0), 0);
    ok(p);
}
void flex_member(void) {
    union { char bytes[sizeof(struct s) + 2 * sizeof(short)]; struct s obj; } held;
    struct s *p = &held.obj;
    p->n = 0;
    WANT(wrong_flex_1, __builtin_dynamic_object_size(p->x, 1), 0);
    ok(p);
}
void flex_whole(void) {
    union { char bytes[sizeof(struct s) + 2 * sizeof(short)]; struct s obj; } held;
    struct s *p = &held.obj;
    p->n = 0;
    WANT(wrong_flex_whole, __builtin_dynamic_object_size(p, 0), sizeof(struct s) + 4);
    ok(p);
}
"#;

#[test]
fn a_counted_flexible_array_is_as_long_as_its_count() {
    let dir = std::env::temp_dir().join(format!("rucc-counted-size-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    for level in ["-O2", "-O3"] {
        let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=x86_64-unknown-linux-gnu", "-fgnuc-version=16.2.0", level, "-c"])
            .arg("-o")
            .arg(dir.join("one.o"))
            .arg(dir.join("one.c"))
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(done.status.success(), "{level}: {}", String::from_utf8_lossy(&done.stderr));
        let bytes = std::fs::read(dir.join("one.o")).expect("the object was written");
        let wrong: Vec<String> = bytes
            .split(|&byte| byte == 0)
            .filter(|name| name.starts_with(b"wrong_"))
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect();
        assert!(wrong.is_empty(), "{level} answered {wrong:?} wrong");
        assert!(bytes.windows(3).any(|at| at == b"ok\0"), "{level} took out the calls to keep");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
