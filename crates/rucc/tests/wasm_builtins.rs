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

/// The declarations of `<math.h>` that the tests of the maths functions use, so that they need no
/// sysroot.
const MATH: &str = "\
    double sqrt(double); float sqrtf(float);\n\
    double ceil(double); float ceilf(float);\n\
    double floor(double); float floorf(float);\n\
    double trunc(double); float truncf(float);\n\
    double rint(double); float rintf(float);\n\
    double nearbyint(double); float nearbyintf(float);\n\
    double fabs(double); float fabsf(float);\n\
    double copysign(double, double); float copysignf(float, float);\n\
    double round(double);\n";

/// The maths functions that wasm has an instruction for are that instruction, as clang 23 writes
/// them, and `round`, which has no instruction, stays a call. #3198.
#[test]
fn the_libm_functions_with_an_instruction_are_that_instruction() {
    let source = format!(
        "{MATH}\
        double s(double x) {{ return sqrt(x); }}\n\
        float sf(float x) {{ return sqrtf(x); }}\n\
        double c(double x) {{ return ceil(x) * floor(x) * trunc(x) * rint(x) * nearbyint(x); }}\n\
        float cf(float x) {{ return ceilf(x) * floorf(x) * truncf(x) * rintf(x) * nearbyintf(x); }}\n\
        double r(double x) {{ return round(x); }}\n"
    );
    let code = instructions(&[], &source);
    for want in [
        "f64.sqrt",
        "f32.sqrt",
        "f64.ceil",
        "f64.floor",
        "f64.trunc",
        "f32.ceil",
        "f32.floor",
        "f32.trunc",
    ] {
        assert!(code.iter().any(|line| line == want), "no `{want}` in {code:#?}");
    }
    // `rint` and `nearbyint` are both `nearest`.
    assert_eq!(code.iter().filter(|line| *line == "f64.nearest").count(), 2, "{code:#?}");
    assert_eq!(code.iter().filter(|line| *line == "f32.nearest").count(), 2, "{code:#?}");
    let calls: Vec<_> = code.iter().filter(|line| line.starts_with("call")).collect();
    assert_eq!(calls, ["call round"], "{code:#?}");
}

/// `fabs` and `copysign` reach the backend as masks of the sign bit, and the selector writes them
/// as `abs` and `copysign`, with the shapes that a constant operand folds them to.
#[test]
fn the_sign_masks_are_abs_and_copysign() {
    let source = format!(
        "{MATH}\
        double a(double x) {{ return fabs(x); }}\n\
        float af(float x) {{ return fabsf(x); }}\n\
        double c(double x, double y) {{ return copysign(x, y); }}\n\
        float cf(float x, float y) {{ return copysignf(x, y); }}\n\
        double k(double x) {{ return copysign(3.0, x); }}\n\
        double n(double x) {{ return -fabs(x); }}\n"
    );
    let code = instructions(&[], &source);
    for want in
        ["f64.abs", "f32.abs", "f64.copysign", "f32.copysign", "f64.const 0x1.8p1", "f64.neg"]
    {
        assert!(code.iter().any(|line| line == want), "no `{want}` in {code:#?}");
    }
    assert!(!code.iter().any(|line| line.contains("reinterpret")), "{code:#?}");
}

/// A call stays a call where the name may not be the library's function, and the `__builtin_`
/// spelling is the instruction in every mode, as in clang. Under `-fmath-errno`,
/// `sqrt` is the instruction and a call when the answer is a NaN, because only the function sets
/// `errno`.
#[test]
fn a_libm_call_stays_a_call_where_the_instruction_is_not_the_function() {
    let source = format!(
        "{MATH}\
        double s(double x) {{ return sqrt(x); }}\n\
        double c(double x) {{ return ceil(x); }}\n"
    );
    let code = instructions(&["-fno-builtin"], &source);
    assert!(code.iter().any(|line| line == "call sqrt"), "{code:#?}");
    assert!(code.iter().any(|line| line == "call ceil"), "{code:#?}");

    // Only the function that the flag names.
    let code = instructions(&["-fno-builtin-ceil"], &source);
    assert!(code.iter().any(|line| line == "call ceil"), "{code:#?}");
    assert!(code.iter().any(|line| line == "f64.sqrt"), "{code:#?}");

    // The `__builtin_` spelling is the library's function in every mode.
    let spelled = "double c(double x) { return __builtin_ceil(x); }\n\
        float t(float x) { return __builtin_truncf(x); }\n";
    for flag in ["-fno-builtin", "-ffreestanding"] {
        let code = instructions(&[flag], spelled);
        assert!(code.iter().any(|line| line == "f64.ceil"), "{flag}: {code:#?}");
        assert!(code.iter().any(|line| line == "f32.trunc"), "{flag}: {code:#?}");
        assert!(!code.iter().any(|line| line.starts_with("call")), "{flag}: {code:#?}");
    }

    let code = instructions(&["-fmath-errno"], &source);
    let sqrt = code.iter().position(|line| line == "f64.sqrt").expect("f64.sqrt");
    let call = code.iter().position(|line| line == "call sqrt").expect("call sqrt");
    assert!(sqrt < call && code[sqrt..call].iter().any(|line| line == "f64.eq"), "{code:#?}");
    assert!(code.iter().any(|line| line == "f64.ceil"), "{code:#?}");

    // A unit that defines the function calls its own.
    let own = format!("{source}double ceil(double x) {{ return x; }}\n");
    let code = instructions(&[], &own);
    assert!(code.iter().any(|line| line == "f64.sqrt"), "{code:#?}");
    assert!(!code.iter().any(|line| line == "f64.ceil"), "{code:#?}");
}
