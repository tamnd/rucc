//! The scalar and vector `__builtin_wasm_*` functions of clang on the wasm rows, and what gcc says
//! about them on the other rows. The facts come from clang 23 from wasi-sdk 34: each call is one
//! instruction in place of the call, a saturating conversion needs the feature
//! `nontrapping-fptoint`, a vector builtin needs `simd128` or `relaxed-simd`, and the memory index
//! is the constant 0.
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

/// The vector types of the signatures of [`VECTOR`], in the words of the table.
const LANES: &str = "\
    typedef signed char sc16 __attribute__((vector_size(16)));\n\
    typedef unsigned char uc16 __attribute__((vector_size(16)));\n\
    typedef short s8 __attribute__((vector_size(16)));\n\
    typedef unsigned short us8 __attribute__((vector_size(16)));\n\
    typedef int i4 __attribute__((vector_size(16)));\n\
    typedef unsigned int ui4 __attribute__((vector_size(16)));\n\
    typedef long long ll2 __attribute__((vector_size(16)));\n\
    typedef float f4 __attribute__((vector_size(16)));\n\
    typedef double d2 __attribute__((vector_size(16)));\n";

/// Each vector builtin of clang 23 with a type of its own, the type that clang gives it, and the
/// instruction that clang writes for it. The types come from `-Xclang -ast-dump` of clang 23.
const VECTOR: &[(&str, &str, &str)] = &[
    ("abs_f32x4", "f4(f4)", "f32x4.abs"),
    ("abs_f64x2", "d2(d2)", "f64x2.abs"),
    ("abs_i16x8", "s8(s8)", "i16x8.abs"),
    ("abs_i32x4", "i4(i4)", "i32x4.abs"),
    ("abs_i64x2", "ll2(ll2)", "i64x2.abs"),
    ("abs_i8x16", "sc16(sc16)", "i8x16.abs"),
    ("all_true_i16x8", "int(s8)", "i16x8.all_true"),
    ("all_true_i32x4", "int(i4)", "i32x4.all_true"),
    ("all_true_i64x2", "int(ll2)", "i64x2.all_true"),
    ("all_true_i8x16", "int(sc16)", "i8x16.all_true"),
    ("any_true_v128", "int(sc16)", "v128.any_true"),
    ("avgr_u_i16x8", "us8(us8, us8)", "i16x8.avgr_u"),
    ("avgr_u_i8x16", "uc16(uc16, uc16)", "i8x16.avgr_u"),
    ("bitmask_i16x8", "unsigned(s8)", "i16x8.bitmask"),
    ("bitmask_i32x4", "unsigned(i4)", "i32x4.bitmask"),
    ("bitmask_i64x2", "unsigned(ll2)", "i64x2.bitmask"),
    ("bitmask_i8x16", "unsigned(sc16)", "i8x16.bitmask"),
    ("bitselect", "i4(i4, i4, i4)", "v128.bitselect"),
    ("ceil_f32x4", "f4(f4)", "f32x4.ceil"),
    ("ceil_f64x2", "d2(d2)", "f64x2.ceil"),
    ("dot_s_i32x4_i16x8", "i4(s8, s8)", "i32x4.dot_i16x8_s"),
    ("extadd_pairwise_i16x8_s_i32x4", "i4(s8)", "i32x4.extadd_pairwise_i16x8_s"),
    ("extadd_pairwise_i16x8_u_i32x4", "ui4(us8)", "i32x4.extadd_pairwise_i16x8_u"),
    ("extadd_pairwise_i8x16_s_i16x8", "s8(sc16)", "i16x8.extadd_pairwise_i8x16_s"),
    ("extadd_pairwise_i8x16_u_i16x8", "us8(uc16)", "i16x8.extadd_pairwise_i8x16_u"),
    ("floor_f32x4", "f4(f4)", "f32x4.floor"),
    ("floor_f64x2", "d2(d2)", "f64x2.floor"),
    ("max_f32x4", "f4(f4, f4)", "f32x4.max"),
    ("max_f64x2", "d2(d2, d2)", "f64x2.max"),
    ("min_f32x4", "f4(f4, f4)", "f32x4.min"),
    ("min_f64x2", "d2(d2, d2)", "f64x2.min"),
    ("narrow_s_i16x8_i32x4", "s8(i4, i4)", "i16x8.narrow_i32x4_s"),
    ("narrow_s_i8x16_i16x8", "sc16(s8, s8)", "i8x16.narrow_i16x8_s"),
    ("narrow_u_i16x8_i32x4", "us8(i4, i4)", "i16x8.narrow_i32x4_u"),
    ("narrow_u_i8x16_i16x8", "uc16(s8, s8)", "i8x16.narrow_i16x8_u"),
    ("nearest_f32x4", "f4(f4)", "f32x4.nearest"),
    ("nearest_f64x2", "d2(d2)", "f64x2.nearest"),
    ("pmax_f32x4", "f4(f4, f4)", "f32x4.pmax"),
    ("pmax_f64x2", "d2(d2, d2)", "f64x2.pmax"),
    ("pmin_f32x4", "f4(f4, f4)", "f32x4.pmin"),
    ("pmin_f64x2", "d2(d2, d2)", "f64x2.pmin"),
    ("q15mulr_sat_s_i16x8", "s8(s8, s8)", "i16x8.q15mulr_sat_s"),
    (
        "relaxed_dot_i8x16_i7x16_add_s_i32x4",
        "i4(sc16, sc16, i4)",
        "i32x4.relaxed_dot_i8x16_i7x16_add_s",
    ),
    ("relaxed_dot_i8x16_i7x16_s_i16x8", "s8(sc16, sc16)", "i16x8.relaxed_dot_i8x16_i7x16_s"),
    ("relaxed_laneselect_i16x8", "s8(s8, s8, s8)", "i16x8.relaxed_laneselect"),
    ("relaxed_laneselect_i32x4", "i4(i4, i4, i4)", "i32x4.relaxed_laneselect"),
    ("relaxed_laneselect_i64x2", "ll2(ll2, ll2, ll2)", "i64x2.relaxed_laneselect"),
    ("relaxed_laneselect_i8x16", "sc16(sc16, sc16, sc16)", "i8x16.relaxed_laneselect"),
    ("relaxed_madd_f32x4", "f4(f4, f4, f4)", "f32x4.relaxed_madd"),
    ("relaxed_madd_f64x2", "d2(d2, d2, d2)", "f64x2.relaxed_madd"),
    ("relaxed_max_f32x4", "f4(f4, f4)", "f32x4.relaxed_max"),
    ("relaxed_max_f64x2", "d2(d2, d2)", "f64x2.relaxed_max"),
    ("relaxed_min_f32x4", "f4(f4, f4)", "f32x4.relaxed_min"),
    ("relaxed_min_f64x2", "d2(d2, d2)", "f64x2.relaxed_min"),
    ("relaxed_nmadd_f32x4", "f4(f4, f4, f4)", "f32x4.relaxed_nmadd"),
    ("relaxed_nmadd_f64x2", "d2(d2, d2, d2)", "f64x2.relaxed_nmadd"),
    ("relaxed_q15mulr_s_i16x8", "s8(s8, s8)", "i16x8.relaxed_q15mulr_s"),
    ("relaxed_swizzle_i8x16", "sc16(sc16, sc16)", "i8x16.relaxed_swizzle"),
    ("relaxed_trunc_s_i32x4_f32x4", "i4(f4)", "i32x4.relaxed_trunc_f32x4_s"),
    ("relaxed_trunc_s_zero_i32x4_f64x2", "i4(d2)", "i32x4.relaxed_trunc_f64x2_s_zero"),
    ("relaxed_trunc_u_i32x4_f32x4", "ui4(f4)", "i32x4.relaxed_trunc_f32x4_u"),
    ("relaxed_trunc_u_zero_i32x4_f64x2", "ui4(d2)", "i32x4.relaxed_trunc_f64x2_u_zero"),
    ("sqrt_f32x4", "f4(f4)", "f32x4.sqrt"),
    ("sqrt_f64x2", "d2(d2)", "f64x2.sqrt"),
    ("swizzle_i8x16", "sc16(sc16, sc16)", "i8x16.swizzle"),
    ("trunc_f32x4", "f4(f4)", "f32x4.trunc"),
    ("trunc_f64x2", "d2(d2)", "f64x2.trunc"),
    ("trunc_sat_s_zero_f64x2_i32x4", "i4(d2)", "i32x4.trunc_sat_f64x2_s_zero"),
    ("trunc_sat_u_zero_f64x2_i32x4", "ui4(d2)", "i32x4.trunc_sat_f64x2_u_zero"),
    ("trunc_saturate_s_i32x4_f32x4", "i4(f4)", "i32x4.trunc_sat_f32x4_s"),
    ("trunc_saturate_u_i32x4_f32x4", "i4(f4)", "i32x4.trunc_sat_f32x4_u"),
];

/// A function for each builtin of [`VECTOR`] that gives its operands to the builtin.
fn vector_calls() -> String {
    let mut source = String::from(LANES);
    for (name, signature, _) in VECTOR {
        let (ret, params) = signature.split_once('(').unwrap();
        let params: Vec<&str> = params.trim_end_matches(')').split(", ").collect();
        let list: Vec<String> =
            params.iter().enumerate().map(|(i, p)| format!("{p} a{i}")).collect();
        let args: Vec<String> = (0..params.len()).map(|i| format!("a{i}")).collect();
        source.push_str(&format!(
            "{ret} f_{name}({}) {{ return __builtin_wasm_{name}({}); }}\n",
            list.join(", "),
            args.join(", ")
        ));
    }
    source
}

/// Each vector builtin is its operands and then one SIMD instruction, which is the whole body that
/// clang 23 writes for the same function at `-O2`.
#[test]
fn the_vector_wasm_builtins_are_one_simd_instruction_each() {
    let out = rucc(
        "wasm32-wasip1",
        &["-O2", "-S", "-o", "-", "-msimd128", "-mrelaxed-simd"],
        &vector_calls(),
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    for (name, signature, op) in VECTOR {
        let label = format!("f_{name}:");
        let body: Vec<String> = text
            .lines()
            .skip_while(|line| *line != label)
            .skip(1)
            .take_while(|line| line.starts_with('\t'))
            .filter(|line| !line.starts_with("\t."))
            .map(|line| line.trim().replace('\t', " "))
            .filter(|line| line != "return")
            .collect();
        let count = signature.matches(',').count() + 1;
        let mut want: Vec<String> = (0..count).map(|i| format!("local.get {i}")).collect();
        want.push(op.to_string());
        want.push("end_function".to_string());
        assert_eq!(body, want, "for `__builtin_wasm_{name}`");
    }
}

/// clang refuses a vector builtin without `-msimd128`, and a relaxed one without
/// `-mrelaxed-simd`, in these words.
#[test]
fn a_vector_wasm_builtin_needs_its_feature() {
    let source = format!("{LANES}sc16 f(sc16 a) {{ return __builtin_wasm_abs_i8x16(a); }}\n");
    let plain = refusal("wasm32-wasip1", &[], &source);
    assert!(plain.contains("`__builtin_wasm_abs_i8x16` needs target feature simd128"), "{plain}");
    let source = format!(
        "{LANES}sc16 f(sc16 a, sc16 b) {{ return __builtin_wasm_relaxed_swizzle_i8x16(a, b); }}\n"
    );
    let simd = refusal("wasm32-wasip1", &["-msimd128"], &source);
    assert!(
        simd.contains("`__builtin_wasm_relaxed_swizzle_i8x16` needs target feature relaxed-simd"),
        "{simd}"
    );
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
