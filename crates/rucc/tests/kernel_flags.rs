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
