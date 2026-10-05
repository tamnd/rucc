//! The scalar `__builtin_wasm_*` functions of clang on the wasm rows, and what gcc says about them
//! on the other rows. The facts come from clang 23 from wasi-sdk 34: each call is one instruction
//! in place of the call, a saturating conversion needs the feature `nontrapping-fptoint`, and the
//! memory index is the constant 0.
//!
//! Design: #2865, and section 11.5 of the WebAssembly notes.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

/// The output of rucc for `source` on stdin, with the arguments after the input.
fn rucc(target: &str, args: &[&str], source: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .arg(format!("--target={target}"))
        .args(["-x", "c", "-"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(source.as_bytes()).unwrap();
    child.wait_with_output().expect("the compiler finished")
}

/// The instruction lines of the assembly that rucc writes for `source`, without the directives.
fn instructions(args: &[&str], source: &str) -> Vec<String> {
    let mut all = vec!["-O2", "-S", "-o", "-"];
    all.extend_from_slice(args);
    let out = rucc("wasm32-wasip1", &all, source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().replace('\t', " "))
        .collect()
}

/// The text of the error that rucc gives for `source` on `target`.
fn refusal(target: &str, args: &[&str], source: &str) -> String {
    let mut all = vec!["-S", "-o", "-"];
    all.extend_from_slice(args);
    let out = rucc(target, &all, source);
    assert!(!out.status.success(), "rucc took the program on {target}");
    String::from_utf8(out.stderr).unwrap()
}

#[test]
fn the_scalar_wasm_builtins_are_one_instruction_each() {
    let source = "\
        float f(float a, float b) { return __builtin_wasm_max_f32(a, b); }\n\
        double d(double a, double b) { return __builtin_wasm_min_f64(a, b); }\n\
        int s(double x) { return __builtin_wasm_trunc_saturate_s_i32_f64(x); }\n\
        long long u(float x) { return __builtin_wasm_trunc_u_i64_f32(x); }\n\
        unsigned long m(void) { return __builtin_wasm_memory_grow(0, 1) + __builtin_wasm_memory_size(0); }\n\
        void *t(void) { return __builtin_wasm_tls_base(); }\n\
        unsigned long z(void) { return __builtin_wasm_tls_size() + __builtin_wasm_tls_align(); }\n";
    let code = instructions(&[], source);
    for want in [
        "f32.max",
        "f64.min",
        "i32.trunc_sat_f64_s",
        "i64.trunc_f32_u",
        "memory.grow 0",
        "memory.size 0",
        "global.get __tls_base",
        "global.get __tls_size",
        "global.get __tls_align",
    ] {
        assert!(code.iter().any(|line| line == want), "no `{want}` in {code:#?}");
    }
    // The call is gone, so no object has to define the name.
    assert!(!code.iter().any(|line| line.starts_with("call")), "{code:#?}");

    // `__has_builtin` answers 1 for each name on the wasm rows.
    let asked = "int k = __has_builtin(__builtin_wasm_memory_size) + __has_builtin(__builtin_wasm_trunc_s_i32_f32);\n";
    let out = rucc("wasm32-wasip1", &["-E", "-o", "-"], asked);
    assert!(String::from_utf8_lossy(&out.stdout).contains("int k = 1 + 1;"));

    // clang refuses a saturating conversion without the feature, and a memory index other than 0.
    let mvp = refusal(
        "wasm32-wasip1",
        &["-mcpu=mvp"],
        "int f(float x) { return __builtin_wasm_trunc_saturate_s_i32_f32(x); }\n",
    );
    assert!(mvp.contains("needs target feature nontrapping-fptoint"), "{mvp}");
    let index = refusal(
        "wasm32-wasip1",
        &[],
        "unsigned long f(void) { return __builtin_wasm_memory_size(1); }\n",
    );
    assert!(index.contains("the memory index of `__builtin_wasm_memory_size`"), "{index}");
}

#[test]
fn the_wasm_builtins_are_unknown_on_the_other_rows() {
    let asked = "int k = __has_builtin(__builtin_wasm_memory_size);\n";
    let out = rucc("x86_64-linux-gnu", &["-E", "-o", "-"], asked);
    assert!(String::from_utf8_lossy(&out.stdout).contains("int k = 0;"));

    // gcc 16 has no wasm target, so the call is to a function that nothing declared.
    let call = refusal(
        "x86_64-linux-gnu",
        &[],
        "unsigned long f(void) { return __builtin_wasm_memory_size(0); }\n",
    );
    assert!(
        call.contains("implicit declaration of function '__builtin_wasm_memory_size'"),
        "{call}"
    );
}
