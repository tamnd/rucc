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
        "-mindirect-branch-register",
        "-mindirect-branch-cs-prefix",
        "-mharden-sls=all",
        "-fno-jump-tables",
        "-fconserve-stack",
        "-gz=zstd",
        "-ftrivial-auto-var-init=zero",
        "-ftrivial-auto-var-init=pattern",
    ] {
        assert!(cc_option(X86, flag), "{flag}");
    }
    assert!(cc_option(ARM64, "-mno-outline-atomics"));
}

#[test]
fn a_flag_that_is_not_honored_fails_the_probe() {
    for flag in ["-mindirect-branch=thunk"] {
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
