//! Assembly that goes through the preprocessor first, read the way the kernel's `.S` files need.
//!
//! Design: `spec/05-preprocessor.md`, the section on assembly.
//!
//! A kernel `.S` includes headers that are also C headers and leave their C out under
//! `#ifndef __ASSEMBLER__`, writes x86 immediates as `$SYMBOL` with the symbol a macro, writes
//! AArch64 ones as `#0` inside function-like macros, and has comments that start with `#` and
//! contain apostrophes. gcc reads all of that without a word, and so has this.
//!
//! The objects go to `/dev/null`, as the kernel's probes send theirs, so this is compiled on Unix
//! only.

#![cfg(unix)]

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

/// The compiler given these flags and this file on standard input as `assembler-with-cpp`.
fn run(flags: &[&str], input: &str) -> Output {
    run_as("assembler-with-cpp", flags, input)
}

/// The same, with the language of standard input named.
fn run_as(language: &str, flags: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(flags)
        .args(["-x", language, "-"])
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

/// What `-E -P` makes of `input`, which has to be free of complaints.
fn preprocessed(input: &str) -> String {
    let out = run(&["--target=x86_64-unknown-linux-gnu", "-E", "-P"], input);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && stderr.is_empty(), "{stderr}");
    String::from_utf8(out.stdout).expect("the output is text")
}

const X86: &str = "\
#ifndef __ASSEMBLER__
int c_only(void);
#endif
# The kernel's own comments look like this, and it's fine for them to say anything.
#define TI_flags 8
#define GET_FLAGS(reg) movq $TI_flags, reg
\t.text
\t.globl f
f:
\tGET_FLAGS(%rax)
\tret
";

#[test]
fn a_kernel_shaped_x86_file_preprocesses_the_way_gcc_does() {
    let text = preprocessed(X86);
    assert!(!text.contains("c_only"), "{text}");
    assert!(text.contains("# The kernel's own comments"), "{text}");
    assert!(text.contains("movq $8, %rax"), "{text}");
}

#[test]
fn a_kernel_shaped_x86_file_assembles() {
    let out = run(&["--target=x86_64-unknown-linux-gnu", "-c", "-o", "/dev/null"], X86);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn an_aarch64_immediate_inside_a_function_like_macro_is_kept() {
    let src = "#define ZERO(r) mov r, #0\n\t.text\ng:\n\tZERO(x1)\n\tret\n";
    let out = run(&["--target=aarch64-unknown-linux-gnu", "-c", "-o", "/dev/null"], src);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(preprocessed(src).contains("mov x1, #0"));
}

#[test]
fn assembly_is_told_it_is_assembly_and_c_is_not() {
    let probe = "#ifdef __ASSEMBLER__\nasm\n#endif\n#ifdef __STDC_VERSION__\nversion\n#endif\n";
    assert_eq!(preprocessed(probe).trim(), "asm");
    let out = run_as("c", &["--target=x86_64-unknown-linux-gnu", "-E", "-P"], probe);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "version");
}
