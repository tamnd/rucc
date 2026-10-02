//! Tests an inlined helper makes again that the code around it already settled.
//!
//! The kernel's helpers check their arguments with `BUG_ON` and `WARN_ON_ONCE`, and once a helper is
//! inlined most of those checks repeat a test the caller made a line earlier. gcc's ranges see that
//! and drop the check with its `__bug_table` entry. These are three shapes from the x86-64
//! defconfig build where rucc kept the check and gcc did not.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the listing is the same everywhere.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, under a directory of its own so two of these running at once do not collide.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-settled-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source at `-O2`.
fn asm(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The `BUG_ON(index >= t->num_targets)` of `dm_table_get_target`, inside a loop that only runs
/// while `i < t->num_targets`. The field is read twice, so the test is only settled once the two
/// reads are one value.
#[test]
fn a_bound_the_loop_already_tested_is_not_tested_again() {
    let source = "struct tgt { int (*it)(struct tgt *, int *); };\n\
        struct table { unsigned int num; struct tgt *targets; };\n\
        static inline struct tgt *get(struct table *t, unsigned int index) {\n\
            if (__builtin_expect(index >= t->num, 0)) __builtin_trap();\n\
            return t->targets + index;\n\
        }\n\
        int none(struct table *t) {\n\
            for (unsigned int i = 0; i < t->num; i++) {\n\
                struct tgt *ti = get(t, i);\n\
                int n = 0;\n\
                if (!ti->it) return 0;\n\
                ti->it(ti, &n);\n\
                if (n) return 0;\n\
            }\n\
            return 1;\n\
        }\n";
    let listing = asm("bound", source);
    assert!(!listing.contains("ud2"), "the trap is still there:\n{listing}");
}

/// `count = min(count, sizeof(buf) - 1)` and then `WARN_ON_ONCE(bytes > INT_MAX)`, which is
/// `copy_from_user` in lib/kstrtox.c.
#[test]
fn a_clamped_length_is_never_too_long() {
    let source = "void warn(void);\n\
        unsigned long cp(char *d, unsigned long n);\n\
        static inline __attribute__((always_inline)) int check(unsigned long bytes) {\n\
            if (__builtin_expect(bytes > 0x7fffffff, 0)) { warn(); return 0; }\n\
            return 1;\n\
        }\n\
        int f(unsigned long count) {\n\
            char buf[68];\n\
            count = count < sizeof(buf) - 1 ? count : sizeof(buf) - 1;\n\
            if (check(count)) return cp(buf, count);\n\
            return -1;\n\
        }\n";
    let listing = asm("clamped", source);
    assert!(!listing.contains("call\twarn"), "the warning is still there:\n{listing}");
}

/// `skb_frag_page` tests the low bit of the `netmem` and then calls `netmem_to_page`, which tests
/// it again under `WARN_ON_ONCE`.
#[test]
fn a_bit_tested_once_is_not_tested_again() {
    let source = "void warn(void);\n\
        struct frag { unsigned long netmem; };\n\
        static inline int is_iov(unsigned long n) { return n & 1UL; }\n\
        static inline void *to_page(unsigned long n) {\n\
            if (__builtin_expect(!!(is_iov(n)), 0)) { warn(); return 0; }\n\
            return (void *)n;\n\
        }\n\
        static inline void *frag_page(const struct frag *f) {\n\
            unsigned long n = f->netmem;\n\
            if (is_iov(n)) return 0;\n\
            return to_page(n);\n\
        }\n\
        void *g(struct frag *f) { return frag_page(f); }\n";
    let listing = asm("bit", source);
    assert!(!listing.contains("call\twarn"), "the warning is still there:\n{listing}");
}

/// A field read above a branch and again in one arm of it is read once. The two addresses are two
/// values, one per block, so the loads are matched on where they point rather than on the address.
#[test]
fn a_field_read_again_below_a_branch_is_read_once() {
    let source = "struct s { int a, b; };\n\
        int f(struct s *p, int c) { int x = p->b; if (c) return x + p->b * 3; return x; }\n";
    let listing = asm("field", source);
    assert_eq!(listing.matches("4(%rdi)").count(), 1, "the field is read twice:\n{listing}");
}

/// `__schedstats_from_dl_se` returns `NULL` for a server and `&dl_task_of(dl_se)->stats`
/// otherwise, and its caller tests the pointer and calls `dl_task_of` again. Both `BUG_ON`s test
/// a bit the code above already found clear, once straight after the test and once past the join
/// on the pointer.
#[test]
fn a_bit_tested_before_a_join_is_still_known_after_it() {
    let source = "struct se { unsigned long long rt; unsigned int thr : 1, srv : 1, def : 1; };\n\
        struct stats { long w; };\n\
        struct task { long pad[8]; struct stats st; long more[4]; struct se dl; };\n\
        extern int stats_on;\n\
        void wait_start(struct task *p, struct stats *s);\n\
        static inline int dl_server(struct se *se) { return se->srv; }\n\
        static inline struct task *task_of(struct se *se) {\n\
            if (__builtin_expect(dl_server(se), 0)) __builtin_trap();\n\
            return (struct task *)((char *)se - __builtin_offsetof(struct task, dl));\n\
        }\n\
        static inline __attribute__((always_inline)) struct stats *from(struct se *se) {\n\
            if (!stats_on) return 0;\n\
            if (dl_server(se)) return 0;\n\
            return &task_of(se)->st;\n\
        }\n\
        void f(struct se *se) {\n\
            struct stats *s = from(se);\n\
            if (s) wait_start(task_of(se), s);\n\
        }\n";
    let listing = asm("join", source);
    assert!(!listing.contains("ud2"), "a trap is still there:\n{listing}");
}
