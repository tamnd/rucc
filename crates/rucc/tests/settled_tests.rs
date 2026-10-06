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
    asm_with(what, source, &[])
}

/// [`asm`] with more flags, for a fixture whose shape needs one the kernel builds with.
fn asm_with(what: &str, source: &str, flags: &[&str]) -> String {
    compiled(what, source, flags).0
}

/// The assembly at `-O2` with those flags, and what the compiler said.
fn compiled(what: &str, source: &str, flags: &[&str]) -> (String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (String::from_utf8(out.stdout).expect("what the compiler writes is text"), said)
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

/// `tso_build_data` calls `skb_frag_address`, which calls `skb_frag_page` twice. Once the first
/// call's warning is gone the second read of the `netmem` repeats the first, and only when the two
/// are one value does the second test go too.
#[test]
fn a_field_read_again_after_a_warning_goes_is_read_once() {
    let source = "struct frag { unsigned long netmem; unsigned int len, off; };\n\
        struct tso { int idx; int size; char *data; };\n\
        void warn(void);\n\
        static inline int is_iov(unsigned long n) { return n & 1UL; }\n\
        static inline void *to_page(unsigned long n) {\n\
            if (__builtin_expect(!!is_iov(n), 0)) { warn(); return 0; }\n\
            return (void *)n;\n\
        }\n\
        static inline void *frag_page(const struct frag *f) {\n\
            if (is_iov(f->netmem)) return 0;\n\
            return to_page(f->netmem);\n\
        }\n\
        static inline void *frag_address(const struct frag *f) {\n\
            if (!frag_page(f)) return 0;\n\
            return (char *)frag_page(f) + f->off;\n\
        }\n\
        void build(struct frag *frags, struct tso *t, int size) {\n\
            t->size -= size;\n\
            if (t->size == 0) {\n\
                struct frag *f = &frags[t->idx];\n\
                t->size = f->len;\n\
                t->data = frag_address(f);\n\
                t->idx++;\n\
            }\n\
        }\n";
    let listing = asm("again", source);
    assert!(!listing.contains("call\twarn"), "the warning is still there:\n{listing}");
}

/// `misc_open` returns early when it found no `fops`, and `replace_fops` then makes
/// `BUG_ON(!(file->f_op = new_fops))` about the pointer it just found was not null. The two nulls
/// are two constants, so the test below only matches the one above when they are read as one.
#[test]
fn a_pointer_found_not_null_is_not_tested_again() {
    let source = "struct file { const void *f_op; };\n\
        void put(void);\n\
        int f(struct file *file, const void *nf) {\n\
            if (!nf) return -19;\n\
            put();\n\
            if (!(file->f_op = nf)) __builtin_trap();\n\
            return 0;\n\
        }\n";
    let listing = asm("null", source);
    assert!(!listing.contains("ud2"), "the trap is still there:\n{listing}");
}

/// `check_copy_size` asks `__builtin_object_size` about its `addr`, which in
/// `io_handle_query_entry` is a parameter. Once that function is inlined into `io_query` the
/// address is a 48 byte local, a length past it has already returned, and the
/// `WARN_ON_ONCE(bytes > INT_MAX)` behind that cannot fire. The question has to wait for the
/// inlining to get that answer.
#[test]
fn an_object_size_through_a_parameter_is_answered_after_inlining() {
    let source = "void over(void);\n\
        unsigned long cp(void *d, unsigned long n);\n\
        static inline __attribute__((always_inline)) int check(const void *addr, unsigned long n) {\n\
            int sz = __builtin_object_size(addr, 0);\n\
            if (__builtin_expect(sz >= 0 && sz < n, 0)) { over(); return 0; }\n\
            if (__builtin_expect(n > 0x7fffffff, 0)) __builtin_trap();\n\
            return 1;\n\
        }\n\
        static int entry(char *data, unsigned int n) {\n\
            if (check(data, n)) return cp(data, n);\n\
            return -14;\n\
        }\n\
        int f(unsigned int n) {\n\
            char buf[48];\n\
            return entry(buf, n);\n\
        }\n";
    let listing = asm("objsize", source);
    assert!(!listing.contains("ud2"), "the trap is still there:\n{listing}");
}

/// evdev's `str_to_user`, called with `_IOC_SIZE (cmd)`, which is at most 16383, and testing what
/// it was given against `INT_MAX` the way `check_copy_size` does.
const EVDEV: &str = "unsigned long cp(void *d, const char *s, unsigned long n);\n\
    static __attribute__((noinline)) int to_user(const char *s, unsigned n, void *d) {\n\
        if (__builtin_expect(n > 0x7fffffff, 0)) __builtin_trap();\n\
        return cp(d, s, n);\n\
    }\n\
    int f(unsigned cmd, void *d, const char *a, const char *b) {\n\
        if (cmd & 1) return to_user(a, (cmd >> 16) & 0x3fff, d);\n\
        if (cmd & 2) return to_user(b, (cmd >> 16) & 0x3fff, d);\n\
        return 0;\n\
    }\n";

#[test]
fn a_length_every_caller_keeps_small_is_not_tested_against_int_max() {
    let listing = asm("ipvrp", EVDEV);
    assert!(!listing.contains("ud2"), "the test is still there:\n{listing}");
}

#[test]
fn the_range_every_caller_agrees_on_is_said() {
    let (_, said) = compiled("ipvrp-said", EVDEV, &["-fopt-info-all"]);
    let written = "optimized: range every call agrees on written on a parameter";
    assert!(said.contains(&format!("to_user: {written}")), "{said}");
    // `f` is exported, so a call this unit cannot see may pass it anything.
    assert!(!said.contains(&format!(" f: {written}")), "{said}");
    // A length read from memory can be anything. Passing `cmd` itself would not do, since below
    // `cmd & 1` it is known not to be the largest value and the union of the two is a range.
    let open =
        EVDEV.replace("to_user(b, (cmd >> 16) & 0x3fff, d)", "to_user(b, *(unsigned *)d, d)");
    assert_ne!(open, EVDEV);
    let (_, said) = compiled("ipvrp-unbounded", &open, &["-fopt-info-all"]);
    assert!(!said.contains(written), "{said}");
    assert!(said.contains("to_user: missed: parameter left unbounded"), "{said}");
}

#[test]
fn a_length_one_caller_does_not_bound_is_still_tested() {
    let source = "unsigned long cp(void *d, const char *s, unsigned long n);\n\
        static __attribute__((noinline)) int to_user(const char *s, unsigned n, void *d) {\n\
            if (__builtin_expect(n > 0x7fffffff, 0)) __builtin_trap();\n\
            return cp(d, s, n);\n\
        }\n\
        int f(unsigned cmd, void *d, const char *a, const char *b) {\n\
            if (cmd & 1) return to_user(a, (cmd >> 16) & 0x3fff, d);\n\
            if (cmd & 2) return to_user(b, cmd, d);\n\
            return 0;\n\
        }\n";
    let listing = asm("ipvrp-open", source);
    assert!(listing.contains("ud2"), "the test went:\n{listing}");
}

#[test]
fn an_object_size_through_a_cleanup_variable_is_answered() {
    // keyboard.c's vt_do_kdgkbdiacr: the buffer is a `__free (kfree)` variable, so the cleanup
    // takes its address and it stays a slot, and the size the allocator was asked for is what
    // keeps `check_copy_size` from testing the length against `INT_MAX`.
    let source = "typedef unsigned long size_t;\n\
        void *km(size_t n, size_t s) __attribute__((alloc_size(1, 2)));\n\
        void kfree(const void *);\n\
        void over(int, unsigned long);\n\
        unsigned long raw(void *to, const void *from, unsigned long n);\n\
        static inline void free_it(void *p) { void *q = *(void **)p; if (q) kfree(q); }\n\
        static inline __attribute__((always_inline)) int check(const void *addr, size_t n) {\n\
            int sz = __builtin_object_size(addr, 0);\n\
            if (__builtin_expect(sz >= 0 && sz < n, 0)) { over(sz, n); return 0; }\n\
            if (__builtin_expect(n > 0x7fffffff, 0)) __builtin_trap();\n\
            return 1;\n\
        }\n\
        extern unsigned int size;\n\
        int f(void *u) {\n\
            int i, n;\n\
            char __attribute__((cleanup(free_it))) *buf = km(256, 3);\n\
            if (!buf) return -12;\n\
            n = size;\n\
            for (i = 0; i < n; i++) buf[i] = i;\n\
            if (check(buf, n * 3UL) && raw(u, buf, n * 3UL)) return -14;\n\
            return 0;\n\
        }\n";
    let listing = asm("cleanup-objsize", source);
    assert!(!listing.contains("ud2"), "the test is still there:\n{listing}");
}

#[test]
fn a_size_worked_out_by_an_overflow_check_on_two_constants_is_a_constant() {
    let source = "void a(unsigned long); void b(unsigned long);\n\
        static inline __attribute__((always_inline)) void km(unsigned long size) {\n\
            if (__builtin_constant_p(size)) a(size); else b(size);\n\
        }\n\
        void f(void) { unsigned long bytes; if (__builtin_mul_overflow(256ul, 3ul, &bytes)) return; km(bytes); }\n";
    let listing = asm("mul-overflow-constant", source);
    assert!(listing.contains("jmp\ta") && !listing.contains("\tb\n"), "the test went:\n{listing}");
}

#[test]
fn an_object_size_through_kmalloc_array_and_a_cleanup_variable_is_answered() {
    let source = "typedef unsigned long size_t;\n\
        void *kbig(size_t n) __attribute__((alloc_size(1)));\n\
        void kfree(void *p);\n\
        void copy_overflow(int, size_t);\n\
        unsigned long raw_copy(void *to, const void *from, unsigned long n);\n\
        extern int table_size;\n\
        static inline void free_it(void *p) { void *q = *(void **)p; if (q) kfree(q); }\n\
        static inline __attribute__((alloc_size(1, 2))) void *kmalloc_array(size_t n, size_t size) {\n\
            size_t bytes;\n\
            if (__builtin_expect(__builtin_mul_overflow(n, size, &bytes), 0)) return 0;\n\
            return kbig(bytes);\n\
        }\n\
        static inline __attribute__((always_inline)) int check(const void *addr, size_t bytes) {\n\
            int sz = __builtin_object_size(addr, 0);\n\
            if (sz >= 0 && sz < bytes) { copy_overflow(sz, bytes); return 0; }\n\
            if (__builtin_expect(bytes > 0x7fffffff, 0)) __builtin_trap();\n\
            return 1;\n\
        }\n\
        int f(void *to) {\n\
            int n = table_size;\n\
            char __attribute__((cleanup(free_it))) *dia = kmalloc_array(256, 3);\n\
            if (!dia) return -12;\n\
            if (check(dia, n * 3ul)) return raw_copy(to, dia, n * 3ul);\n\
            return 0;\n\
        }\n";
    let listing = asm_with("kmalloc-array-cleanup", source, &["-ftrivial-auto-var-init=zero"]);
    assert!(!listing.contains("ud2"), "the test went:\n{listing}");
}

#[test]
fn a_size_bounded_through_a_division_does_not_wrap_when_doubled() {
    let source = "void warn(void);\n\
        void *kz(unsigned long n);\n\
        int f(unsigned target, unsigned long *out) {\n\
            unsigned long sz = 128, len;\n\
            for (;;) {\n\
                len = (sz - 20) / 4;\n\
                if (len > target) break;\n\
                sz *= 2;\n\
                if (__builtin_expect(sz < 128, 0)) { warn(); return -28; }\n\
            }\n\
            *out = len;\n\
            return kz(sz) != 0;\n\
        }\n";
    let listing = asm("udiv-back", source);
    assert!(!listing.contains("warn"), "the test went:\n{listing}");
}

#[test]
fn a_size_a_division_does_not_bound_is_still_tested() {
    let source = "void warn(void);\n\
        int f(unsigned long sz, unsigned long target) {\n\
            if ((sz - 20) / 4 > target) return 0;\n\
            if (sz >= 128) if (sz + sz < 128) warn();\n\
            return 1;\n\
        }\n";
    let listing = asm("udiv-open", source);
    assert!(listing.contains("warn"), "the test went:\n{listing}");
}

#[test]
fn a_static_that_is_only_ever_written_is_dropped_with_its_stores() {
    let source = "void *make(void);\n\
        void fix(unsigned long);\n\
        static void *cache __attribute__((section(\".data..ro_after_init\")));\n\
        static struct { int a, b; } pair;\n\
        void init(void) { cache = make(); fix((unsigned long)cache); pair.b = 3; }\n";
    let listing = asm("write-only", source);
    assert!(!listing.contains("ro_after_init"), "the object stayed:\n{listing}");
    assert!(!listing.contains("pair"), "the object stayed:\n{listing}");
    assert!(listing.contains("fix"), "the call went:\n{listing}");
}

#[test]
fn a_static_that_is_read_or_written_through_volatile_is_kept() {
    let source = "static int kept;\n\
        static int loud;\n\
        void set(int v) { kept = v; *(volatile int *)&loud = v; }\n\
        int get(void) { return kept; }\n";
    let listing = asm("write-read", source);
    assert!(listing.contains("kept:"), "the read object went:\n{listing}");
    assert!(listing.contains("loud"), "the volatile store went:\n{listing}");
}

#[test]
fn a_copy_of_a_const_structure_is_stores_of_what_it_holds() {
    let source = "struct t;\n\
        struct hw { unsigned flags; unsigned long res, ticks; int (*open)(struct t *); char name[6]; };\n\
        int o(struct t *);\n\
        static const struct hw hw0 __attribute__((section(\".init.rodata\"))) = { .flags = 3, .open = o, .name = \"timer\" };\n\
        void init(struct hw *to) { *to = hw0; }\n";
    let listing = asm_with("const-copy", source, &["-fno-pic"]);
    assert!(!listing.contains("hw0"), "the object stayed:\n{listing}");
    assert!(listing.contains("o(%rip)") || listing.contains("$o"), "the address went:\n{listing}");
}

#[test]
fn a_copy_of_a_const_structure_holding_an_address_past_a_name_is_still_a_copy() {
    let source = "extern char buf[16];\n\
        struct s { char *at; long n; };\n\
        static const struct s s0 = { buf + 4, 7 };\n\
        void init(struct s *to) { *to = s0; }\n";
    let listing = asm_with("const-copy-away", source, &["-fno-pic"]);
    assert!(listing.contains("s0"), "the object went:\n{listing}");
}

#[test]
fn a_per_cpu_static_only_written_through_its_segment_is_dropped() {
    let source = "struct kp;\n\
        static struct kp *inst __attribute__((section(\".data..percpu\")));\n\
        static inline void set(struct kp *p) { *(struct kp * __seg_gs *)(unsigned long)&inst = p; }\n\
        void use(struct kp *p) { set(p); set(0); }\n";
    let listing = asm_with("percpu-write", source, &["-fno-pic"]);
    assert!(!listing.contains("inst"), "the object stayed:\n{listing}");
    assert!(!listing.contains("%gs:"), "the stores stayed:\n{listing}");
}

#[test]
fn a_per_cpu_static_read_or_reached_through_an_offset_is_kept() {
    let source = "static int seen __attribute__((section(\".data..percpu\")));\n\
        static int other __attribute__((section(\".data..percpu\")));\n\
        extern unsigned long off[4];\n\
        void put(int v) { *(int __seg_gs *)(unsigned long)&seen = v; }\n\
        int get(void) { return *(int __seg_gs *)(unsigned long)&seen; }\n\
        void far(int cpu, int v) { *(int *)((unsigned long)&other + off[cpu]) = v; }\n";
    let listing = asm_with("percpu-read", source, &["-fno-pic"]);
    assert!(listing.contains("seen:"), "the read object went:\n{listing}");
    assert!(listing.contains("other:"), "the object written through an offset went:\n{listing}");
}

#[test]
fn a_function_called_once_is_inlined_after_the_small_functions_around_it() {
    let checks: String = (0..30)
        .map(|i| {
            format!("if (s->at[{}] == state + {i}) s->count = wait(state * {});\n", i % 2, i + 1)
        })
        .collect();
    let source = format!(
        "extern int key;\n\
        void begin(void *), end(void *, int);\n\
        int wait(long);\n\
        int pending(void);\n\
        struct sem {{ int count; long at[2]; }};\n\
        static inline int down_slow(struct sem *s, long state, long timeout) {{\n\
            for (;;) {{\n\
                if (pending()) return -4;\n\
                if (timeout <= 0) return -62;\n\
                timeout = wait(timeout);\n\
                {checks}\
                if (s->at[0] == state) return 0;\n\
            }}\n\
        }}\n\
        static inline int down_common(struct sem *s, long state, long timeout) {{\n\
            if (__builtin_expect(key, 0)) begin(s);\n\
            int ret = down_slow(s, state, timeout);\n\
            if (__builtin_expect(key, 0)) end(s, ret);\n\
            return ret;\n\
        }}\n\
        __attribute__((noinline)) void down(struct sem *s) {{ down_common(s, 2, 1000); }}\n\
        __attribute__((noinline)) int down_one(struct sem *s) {{ return down_common(s, 1, 1000); }}\n\
        __attribute__((noinline)) int down_for(struct sem *s, long t) {{ return down_common(s, 2, t); }}\n"
    );
    let listing = asm("once-last", &source);
    assert!(listing.contains("down_slow:"), "the large function went in:\n{listing}");
    assert!(!listing.contains("down_common:"), "the small function stayed a call:\n{listing}");
    assert_eq!(
        listing.matches("call\tbegin").count(),
        3,
        "the tracepoint is not in each caller:\n{listing}"
    );
}

#[test]
fn a_function_no_larger_than_a_call_is_inlined_before_its_own_calls_are() {
    let source = "extern int key;\n\
        struct probe { void (*fn)(void *, void *, int); void *data; };\n\
        extern struct probe *probes;\n\
        extern int online(int);\n\
        extern int cpu;\n\
        static inline void trace_lock(void *mm, int write) {\n\
            if (__builtin_expect(key, 0) && online(cpu)) {\n\
                struct probe *it = probes;\n\
                if (it) it->fn(it->data, mm, write);\n\
                if (it && it[1].fn) it[1].fn(it[1].data, mm, write);\n\
                if (it && it[2].fn) it[2].fn(it[2].data, mm, write);\n\
            }\n\
        }\n\
        void do_trace_lock(void *mm, int write) { trace_lock(mm, write); }\n\
        static inline void lock_trace(void *mm, int write) {\n\
            if (__builtin_expect(key, 0)) do_trace_lock(mm, write);\n\
        }\n\
        void lock_read(void *mm) { lock_trace(mm, 0); }\n\
        void lock_write(void *mm) { lock_trace(mm, 1); }\n";
    let listing = asm("early-small", source);
    let body = |name: &str| {
        let start = listing.find(&format!("\n{name}:")).expect("the function is in the listing");
        let end = listing[start..].find("\t.size").map_or(listing.len(), |end| start + end);
        listing[start..end].to_owned()
    };
    for caller in ["lock_read", "lock_write"] {
        let body = body(caller);
        assert!(
            !body.contains("call\tdo_trace_lock"),
            "the one line function stayed a call:\n{body}"
        );
        assert!(body.contains("call\tonline"), "the tracepoint is not in the caller:\n{body}");
    }
}
