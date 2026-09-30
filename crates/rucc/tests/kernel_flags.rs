//! The gcc flags the Linux kernel's build passes, asked about the way kbuild asks.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12.
//!
//! `cc-option` compiles an empty file with the flag and keeps the flag when the exit status is
//! zero, so the exit status is the whole answer, and a flag taken without being honored is the
//! answer that goes wrong. The objects go to `/dev/null`, as kbuild's do, so this is compiled on
//! Unix only.

#![cfg(unix)]

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

/// The compiler given these flags and this C on standard input.
fn run(flags: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(flags)
        .args(["-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the input into");
    stdin.write_all(input.as_bytes()).expect("the input can be written");
    drop(stdin);
    child.wait_with_output().expect("the compiler finished")
}

/// What `cc-option` asks: whether an empty file compiles with `flag` for `target`.
fn cc_option(target: &str, flag: &str) -> bool {
    let target = format!("--target={target}");
    run(&[&target, flag, "-c", "-o", "/dev/null"], "").status.success()
}

const X86: &str = "x86_64-unknown-linux-gnu";
const ARM64: &str = "aarch64-unknown-linux-gnu";

#[test]
fn a_flag_that_asks_for_what_happens_passes_the_probe() {
    for flag in [
        "-fno-allow-store-data-races",
        "-fzero-init-padding-bits=all",
        "-fshort-wchar",
        "-fno-PIE",
        "-mindirect-branch=thunk-extern",
        "-mfunction-return=thunk-extern",
        "-mindirect-branch=thunk-inline",
        "-mindirect-branch=thunk",
        "-mfunction-return=thunk-inline",
        "-mfunction-return=thunk",
        "-mindirect-branch-register",
        "-mindirect-branch-cs-prefix",
        "-mharden-sls=all",
        "-fno-jump-tables",
        "-fconserve-stack",
        "-gz=zstd",
        "-ftrivial-auto-var-init=zero",
        "-ftrivial-auto-var-init=pattern",
        "-fzero-call-used-regs=used-gpr",
    ] {
        assert!(cc_option(X86, flag), "{flag}");
    }
    assert!(cc_option(ARM64, "-mno-outline-atomics"));
    assert!(cc_option(ARM64, "-fzero-call-used-regs=used-gpr"));
}

#[test]
fn a_flag_that_is_not_honored_fails_the_probe() {
    for flag in ["-fzero-call-used-regs=all", "-fzero-call-used-regs=used"] {
        assert!(!cc_option(X86, flag), "{flag}");
    }
    assert!(!cc_option(ARM64, "-mbranch-protection=pac-ret+bti"));
    // And what gcc does not know either, which is what `cc-disable-warning` depends on.
    assert!(!cc_option(X86, "-Wthread-safety"));
    assert!(cc_option(X86, "-Wno-frame-address"));
}

/// A helper with a 200 byte buffer, called once from a function with none, is inlined by default,
/// since gcc lets any frame grow to 256 bytes. Under `-fconserve-stack` that is 100 bytes, so it
/// stays a call and its buffer stays in its own frame, which is what gcc 13 does with both.
#[test]
fn conserve_stack_keeps_a_large_buffer_out_of_its_caller() {
    let src = "\
void use(char *);
static int helper(int x) { char buf[200]; buf[0] = x; use(buf); return buf[1]; }
int caller(int x) { return helper(x) + 1; }
";
    let target = "--target=x86_64-unknown-linux-gnu";
    for (flags, kept) in [(&[][..], false), (&["-fconserve-stack"][..], true)] {
        let mut args = vec![target, "-O2", "-S", "-o", "-"];
        args.extend_from_slice(flags);
        let out = run(&args, src);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text.contains("call\thelper"), kept, "{flags:?}:\n{text}");
    }
}

#[test]
fn short_wchar_changes_what_a_wide_string_is() {
    let src = "int size = sizeof(L\"ab\");\nint unsigned_ = (__WCHAR_TYPE__)-1 > 0;\n";
    let out = run(&["--target=x86_64-unknown-linux-gnu", "-fshort-wchar", "-S", "-o", "-"], src);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    // Three 16 bit units, the terminator included, and unsigned.
    assert!(text.contains(".long\t6"), "{text}");
    assert!(text.contains(".long\t1"), "{text}");
}

/// `__is_constexpr` from `include/linux/compiler.h`, which measures a `void` through a `void *` it
/// dereferences when its argument is not a constant. gcc says nothing about either, and a word
/// here is an error in every build with `CONFIG_WERROR`, which `allmodconfig` turns on.
#[test]
fn the_kernel_s_constant_test_compiles_quietly_under_werror() {
    let source = "#define __is_constexpr(x) \\\n\
        (sizeof(int) == sizeof(*(8 ? ((void *)((long)(x) * 0l)) : (int *)8)))\n\
        unsigned long f(unsigned long n) { return __is_constexpr(n - 1); }\n\
        int g(void) { return __is_constexpr(3); }\n\
        typeof(*(void *)0) *h(void) { return 0; }\n";
    let out =
        run(&["--target=x86_64-unknown-linux-gnu", "-Werror", "-c", "-o", "/dev/null"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    let pedantic =
        run(&["--target=x86_64-unknown-linux-gnu", "-pedantic", "-c", "-o", "/dev/null"], source);
    assert!(String::from_utf8_lossy(&pedantic.stderr).contains("to a void type"));
}

/// Two more things in the headers every kernel unit includes that gcc takes without a word:
/// `struct mm_struct`, whose only members before its flexible array are inside an anonymous
/// `struct`, and `check_copy_size`, which keeps what `__builtin_object_size` answers in an `int`.
#[test]
fn the_kernel_s_headers_compile_quietly_under_werror() {
    let source = "struct mm { struct { int users; long flags; }; unsigned long cpu_bitmap[]; };\n\
        unsigned long first(struct mm *mm) { return mm->cpu_bitmap[0] + mm->users; }\n\
        int check(const void *addr, unsigned long bytes) {\n\
            int sz = __builtin_object_size(addr, 0);\n\
            return sz >= 0 && sz < bytes;\n\
        }\n";
    let out = run(
        &["--target=x86_64-unknown-linux-gnu", "-O2", "-Werror", "-c", "-o", "/dev/null"],
        source,
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// `ilog2` of a 64 bit constant, which `mmzone.h` sizes an array with under `defconfig`. The
/// constant goes into `__ilog2_u32` in an arm the `sizeof` test never takes, and gcc says nothing
/// about a conversion that never happens. The same one in a taken arm is still warned about.
#[test]
fn a_conversion_in_an_arm_that_is_never_taken_is_quiet() {
    let source = "int __ilog2_u32(unsigned int n);\n\
        int __ilog2_u64(unsigned long long n);\n\
        #define ilog2(n) (sizeof(n) <= 4 ? __ilog2_u32(n) : __ilog2_u64(n))\n\
        int f(void) { return ilog2(0x400000000ULL); }\n";
    let flags = ["--target=x86_64-unknown-linux-gnu", "-O2", "-Werror", "-c", "-o", "/dev/null"];
    let out = run(&flags, source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    let taken = "int __ilog2_u32(unsigned int n);\n\
        int f(void) { return sizeof(long) > 4 ? __ilog2_u32(0x400000000ULL) : 0; }\n";
    let out = run(&flags, taken);
    assert!(String::from_utf8_lossy(&out.stderr).contains("changes value"));
}

/// `packed` on an enumeration asks for the smallest type that holds it, the same as
/// `-fshort-enums` does for all of them, and the kernel asserts that `enum rw_hint` is one byte.
/// The sizes were measured against gcc 13, before and after the tag and past the width of an
/// `int`.
#[test]
fn a_packed_enumeration_is_as_small_as_its_values_allow() {
    let source = "enum a { A0 = 0, A5 = 5 } __attribute__((__packed__));\n\
        enum __attribute__((packed)) b { B = -200 };\n\
        enum c { C = 70000 } __attribute__((packed));\n\
        enum d { D = 0x100000000 } __attribute__((packed));\n\
        enum e { E = 1 };\n\
        _Static_assert(sizeof(enum a) == 1 && sizeof(enum b) == 2, \"small\");\n\
        _Static_assert(sizeof(enum c) == 4 && sizeof(enum d) == 8, \"wide\");\n\
        _Static_assert(sizeof(enum e) == 4 && (enum b)B < 0, \"the rest\");\n";
    let out =
        run(&["--target=x86_64-unknown-linux-gnu", "-Werror", "-c", "-o", "/dev/null"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// `DEFINE_RATELIMIT_STATE` fills the lock of a `static` inside a function with a compound
/// literal, and gcc takes that as a constant when everything in the literal is one. Its address
/// is still not a constant, and a literal that reads a parameter is still refused.
#[test]
fn a_literal_in_a_block_is_a_constant_value_for_a_static_object() {
    let target = "--target=x86_64-unknown-linux-gnu";
    let source = "typedef struct { int v; void *owner; const char *name; } lock_t;\n\
        struct rs { lock_t lock; int interval; };\n\
        int f(void) {\n\
            static struct rs l = { .lock = (lock_t) { .owner = ((void *)-1L), .name = \"l\" } };\n\
            return l.interval;\n\
        }\n";
    let out = run(&[target, "-Werror", "-c", "-o", "/dev/null"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
    for refused in [
        "int *g(void) { static int *p = &(int){ 1 }; return p; }\n",
        "struct s { int v; };\n\
         struct t { struct s s; };\n\
         int h(int x) { static struct t t = { (struct s){ x } }; return t.s.v; }\n",
    ] {
        let out = run(&[target, "-c", "-o", "/dev/null"], refused);
        assert!(!out.status.success(), "{refused}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("initializer element is not constant")
        );
    }
}

/// `-Wno-pointer-sign` quiets the warning gcc files under that name, which the kernel passes
/// with `-Werror` because it mixes `char *` and `unsigned char *` all over. `-Werror=` and
/// `-Wno-error=` of the name decide whether it is an error, whatever `-Werror` says.
#[test]
fn a_warning_turned_off_by_name_is_not_heard() {
    let target = "--target=x86_64-unknown-linux-gnu";
    let source = "int f(unsigned *u) { int *p = u; return *p; }\n";
    let compile = |flags: &[&str]| {
        let mut all = vec![target, "-c", "-o", "/dev/null"];
        all.extend_from_slice(flags);
        run(&all, source)
    };
    let quiet = compile(&["-Werror", "-Wno-pointer-sign"]);
    assert!(quiet.status.success(), "{}", String::from_utf8_lossy(&quiet.stderr));
    assert!(quiet.stderr.is_empty(), "{}", String::from_utf8_lossy(&quiet.stderr));

    let back = compile(&["-Wno-pointer-sign", "-Wpointer-sign"]);
    assert!(back.status.success());
    assert!(String::from_utf8_lossy(&back.stderr).contains("warning: pointer targets"));

    let error = compile(&["-Werror=pointer-sign"]);
    assert!(!error.status.success());
    assert!(String::from_utf8_lossy(&error.stderr).contains("error: pointer targets"));

    let kept = compile(&["-Werror", "-Wno-error=pointer-sign"]);
    assert!(kept.status.success(), "{}", String::from_utf8_lossy(&kept.stderr));
    assert!(String::from_utf8_lossy(&kept.stderr).contains("warning: pointer targets"));
}

/// `-Wno-attributes` quiets an attribute that does nothing where it was written, and the
/// reserved constructor priority answers to its own name. `-Wno-error` takes back `-Werror`.
#[test]
fn an_ignored_attribute_and_a_reserved_priority_answer_to_gcc_s_names() {
    let target = "--target=x86_64-unknown-linux-gnu";
    let compile = |source: &str, flags: &[&str]| {
        let mut all = vec![target, "-c", "-o", "/dev/null"];
        all.extend_from_slice(flags);
        run(&all, source)
    };
    let object = "__attribute__((constructor)) int not_a_function;\n";
    let quiet = compile(object, &["-Werror", "-Wno-attributes"]);
    assert!(quiet.status.success(), "{}", String::from_utf8_lossy(&quiet.stderr));
    assert!(quiet.stderr.is_empty(), "{}", String::from_utf8_lossy(&quiet.stderr));

    let early = "__attribute__((constructor(50))) void early(void) {}\n";
    let heard = compile(early, &["-Werror", "-Wno-attributes"]);
    assert!(!heard.status.success(), "{}", String::from_utf8_lossy(&heard.stderr));
    let quiet = compile(early, &["-Werror", "-Wno-prio-ctor-dtor"]);
    assert!(quiet.status.success(), "{}", String::from_utf8_lossy(&quiet.stderr));
    assert!(quiet.stderr.is_empty(), "{}", String::from_utf8_lossy(&quiet.stderr));

    let back = compile(early, &["-Werror", "-Wno-error"]);
    assert!(back.status.success(), "{}", String::from_utf8_lossy(&back.stderr));
    assert!(String::from_utf8_lossy(&back.stderr).contains("warning: "));
}

/// The two comparisons of addresses the kernel puts inside `BUILD_BUG_ON`, which only builds when
/// the call to the function declared `error` is gone: a function defined here against `NULL`, as
/// i915 checks each callback it registers, and two of `ERR_PTR`'s sentinels against each other. A
/// function only declared is left alone, because a weak one may never be defined.
#[test]
fn an_address_compared_in_a_build_bug_on_is_decided() {
    let source = "extern void bad(void) __attribute__((error(\"BUILD_BUG_ON failed\")));\n\
        struct x { int a; };\n\
        static int notify(struct x *p) { return p->a; }\n\
        typedef int (*fn)(struct x *);\n\
        static inline void *ERR_PTR(long e) { return (void *)e; }\n\
        void t(fn f) {\n\
            if (notify == 0) bad();\n\
            if (ERR_PTR(-2) == ERR_PTR(-19)) bad();\n\
            f(0);\n\
        }\n\
        void u(void) { t(notify); }\n";
    let out = run(&["--target=x86_64-unknown-linux-gnu", "-O2", "-c", "-o", "/dev/null"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let weak = "extern void bad(void) __attribute__((error(\"BUILD_BUG_ON failed\")));\n\
        extern void hook(void) __attribute__((weak));\n\
        void t(void) { if (hook == 0) bad(); }\n";
    let out = run(&["--target=x86_64-unknown-linux-gnu", "-O2", "-c", "-o", "/dev/null"], weak);
    assert!(!out.status.success(), "a weak declaration may be null");
}

/// `swap_table.h` checks `(unsigned long)xa_mk_value(0)` in a `BUILD_BUG_ON`, which is an integer
/// cast to a pointer inside an inline function and cast back outside it. gcc gives the number back.
#[test]
fn a_small_number_cast_to_a_pointer_and_back_is_that_number() {
    let source = "extern void bad(void) __attribute__((error(\"BUILD_BUG_ON failed\")));\n\
        static inline void *xa_mk_value(unsigned long v) { return (void *)((v << 1) | 1); }\n\
        void t(void) { if ((unsigned long)xa_mk_value(0) != 0b1UL) bad(); }\n";
    let out = run(&["--target=x86_64-unknown-linux-gnu", "-O2", "-c", "-o", "/dev/null"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// ext4 writes `failed_mount8: __maybe_unused` on a label only some configurations jump to. gcc
/// gives the attribute to the label, so there is nothing to warn about under `-Werror`.
#[test]
fn an_attribute_after_a_label_is_the_label_s() {
    let source = "int g(int);\n\
        int f(int x) {\n\
            if (x) goto out;\n\
            x = g(x);\n\
        out: __attribute__((__unused__))\n\
            x = g(x);\n\
            return x;\n\
        }\n";
    let out =
        run(&["--target=x86_64-unknown-linux-gnu", "-Werror", "-c", "-o", "/dev/null"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// i915 keeps each DMI table behind a pointer to an array of unknown size and passes `*list` to a
/// function that takes a pointer. The array decays, so nothing reads the incomplete type, and gcc
/// takes it. `sizeof *list` still needs the size and is still refused.
#[test]
fn a_pointer_to_an_array_of_unknown_size_can_be_dereferenced() {
    let source = "struct d { int a; };\n\
        int check(const struct d *);\n\
        struct q { const struct d (*list)[]; };\n\
        static const struct q qs[] = { { .list = &(const struct d[]) { { 1 }, { 0 } } } };\n\
        int f(int i) { return check(*qs[i].list) + (*qs[i].list)[1].a; }\n";
    let out =
        run(&["--target=x86_64-unknown-linux-gnu", "-Werror", "-c", "-o", "/dev/null"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let size = "struct d { int a; };\n\
        unsigned long h(const struct d (*p)[]) { return sizeof *p; }\n";
    let out = run(&["--target=x86_64-unknown-linux-gnu", "-c", "-o", "/dev/null"], size);
    assert!(!out.status.success(), "the size of an array of unknown size is not known");
}

/// i915's `hwm_field_read_and_scale` is a `static` function, not declared `inline`, that hands its
/// mask parameter to `REG_FIELD_GET`, and each of its two callers passes a `REG_GENMASK`. The
/// `BUILD_BUG_ON` inside only goes away once the call is inlined and the mask is a constant, which
/// is what gcc's inliner does for a call that answers a `__builtin_constant_p` in the callee.
#[test]
fn a_constant_passed_to_a_parameter_the_callee_asks_about_is_inlined() {
    let source = "extern void bad(void) __attribute__((error(\"FIELD_GET: mask is not constant\")));\n\
        #define GENMASK(h, l) (((~0U) << (l)) & (~0U >> (31 - (h))))\n\
        #define FIELD_GET(m, v) ({ if (!__builtin_constant_p(m)) bad(); \\\n\
            if ((m) == 0) bad(); if ((m) & ((m) + (1U << __builtin_ctz(m)))) bad(); \\\n\
            ((v) & (m)) >> __builtin_ctz(m); })\n\
        unsigned rd(int);\n\
        static unsigned long scale(int r, unsigned msk, int shift, unsigned sf) {\n\
            unsigned v = rd(r);\n\
            v = FIELD_GET(msk, v);\n\
            return ((unsigned long)v * sf) >> shift;\n\
        }\n\
        unsigned long a(int s) { return scale(1, GENMASK(15, 8), s, 3); }\n\
        unsigned long b(int s) { return scale(2, GENMASK(14, 0), s, 3); }\n";
    let out = run(&["--target=x86_64-unknown-linux-gnu", "-O2", "-c", "-o", "/dev/null"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
