//! The lanewise operators of GNU vectors on wasm32 with `-msimd128`. An add, a subtract, a multiply,
//! a divide of floats, the bitwise operators, the compares and a shift by a scalar on a vector of
//! sixteen bytes are each one SIMD instruction, which is what clang writes. wasm has no multiply of
//! sixteen `char` lanes, so that one stays a lane at a time, and without `-msimd128` each operator is
//! a lane at a time.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// One function for each operator, over each lane type of sixteen bytes.
const OPERATORS: &str = "\
typedef unsigned char v16qu __attribute__((vector_size(16)));
typedef short v8hi __attribute__((vector_size(16)));
typedef unsigned short v8hu __attribute__((vector_size(16)));
typedef int v4si __attribute__((vector_size(16)));
typedef long long v2di __attribute__((vector_size(16)));
typedef unsigned long long v2du __attribute__((vector_size(16)));
typedef float v4sf __attribute__((vector_size(16)));
typedef double v2df __attribute__((vector_size(16)));
v16qu add16(v16qu a, v16qu b) { return a + b; }
v16qu mul16(v16qu a, v16qu b) { return a * b; }
v8hi mul8(v8hi a, v8hi b) { return a * b; }
v4si sub4(v4si a, v4si b) { return a - b; }
v4si mul4(v4si a, v4si b) { return a * b; }
v2di bits2(v2di a, v2di b) { return (a & b) | (a ^ b); }
v4sf fdiv4(v4sf a, v4sf b) { return a / b; }
v2df fmul2(v2df a, v2df b) { return a * b + a; }
v4si lt4(v4si a, v4si b) { return a < b; }
v16qu eq16(v16qu a, v16qu b) { return (v16qu)(a == b); }
v8hi ge8u(v8hi a, v8hi b) { return (v8hu)a >= (v8hu)b; }
v2di gt2u(v2di a, v2di b) { return (v2du)a > (v2du)b; }
v4si ne4f(v4sf a, v4sf b) { return a != b; }
v2di le2f(v2df a, v2df b) { return a <= b; }
v4si shl4(v4si a, int n) { return a << n; }
v8hi sar8(v8hi a) { return a >> 3; }
v16qu shr16(v16qu a, int n) { return a >> n; }
v2di shl2(v2di a, int n) { return a << n; }
";

/// The output of rucc for `source` on stdin, with `-S` to stdout at `-O2` on wasm32-wasip1.
fn assembly(source: &str, flags: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=wasm32-wasip1", "-O2", "-x", "c", "-", "-S", "-o", "-"])
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(source.as_bytes()).unwrap();
    child.wait_with_output().expect("the compiler finished")
}

/// The instructions of the function `name` in the `-S` text, one for each line.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let start = format!("{name}:");
    text.lines()
        .skip_while(|line| *line != start)
        .skip(1)
        .take_while(|line| !line.starts_with("\tend_function"))
        .map(str::trim)
        .collect()
}

#[test]
fn with_simd128_each_operator_is_one_instruction() {
    let out = assembly(OPERATORS, &["-msimd128"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    for (name, want) in [
        ("add16", &["i8x16.add"][..]),
        ("mul8", &["i16x8.mul"]),
        ("sub4", &["i32x4.sub"]),
        ("mul4", &["i32x4.mul"]),
        ("bits2", &["v128.and", "v128.xor", "v128.or"]),
        ("fdiv4", &["f32x4.div"]),
        ("fmul2", &["f64x2.mul", "f64x2.add"]),
        ("lt4", &["i32x4.lt_s"]),
        ("eq16", &["i8x16.eq"]),
        ("ge8u", &["i16x8.ge_u"]),
        ("gt2u", &["v128.xor", "i64x2.gt_s"]),
        ("ne4f", &["f32x4.ne"]),
        ("le2f", &["f64x2.le"]),
        ("shl4", &["i32x4.shl"]),
        ("sar8", &["i32.const\t3", "i16x8.shr_s"]),
        ("shr16", &["i8x16.shr_u"]),
        ("shl2", &["i32.wrap_i64", "i64x2.shl"]),
    ] {
        let code = body(&text, name);
        for op in want {
            assert!(code.contains(op), "no `{op}` in {name}:\n{}", code.join("\n"));
        }
        assert!(!code.iter().any(|op| op.contains("replace_lane")), "{name}:\n{}", code.join("\n"));
    }
    let code = body(&text, "mul16");
    assert_eq!(code.iter().filter(|op| **op == "i32.mul").count(), 16, "{}", code.join("\n"));
}

#[test]
fn without_simd128_each_operator_is_a_lane_at_a_time() {
    let out = assembly(OPERATORS, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("x16.") && !text.contains("x8.") && !text.contains("x4."), "{text}");
    assert!(!text.contains("x2.") && !text.contains("v128"), "{text}");
}

/// Vectors in local variables, read again as another lane type and at one lane. The negated float
/// vector of `n` is the shape of `wasm_f32x4_relaxed_nmadd`, which stopped the compiler before #3480.
const LOCALS: &str = "\
typedef unsigned char v16qu __attribute__((vector_size(16)));
typedef int v4si __attribute__((vector_size(16)));
typedef float v4sf __attribute__((vector_size(16)));
v4si f(v4si a, v4si b) { v4si t = a * b; return t + a - (b << 2); }
v4sf g(v4sf a, v4sf b) { v4sf t = a * b; t = t + a; return t / b; }
int h(v16qu a) { v16qu t = a + a; v4si u = (v4si)t; return u[2]; }
v4sf n(v4sf a, v4sf b, v4sf c) { return -(a * b) + c; }
";

/// With `-msimd128` a vector stays a `v128` value at `-O2`, as with clang, and the function has no
/// frame on the stack. The cast between two vector types is no instruction, and a read of one lane
/// is one `extract_lane`.
#[test]
fn with_simd128_a_vector_local_stays_a_value() {
    let out = assembly(LOCALS, &["-msimd128"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let want = [
        "local.get\t0",
        "local.get\t1",
        "i32x4.mul",
        "local.get\t0",
        "i32x4.add",
        "local.get\t1",
        "i32.const\t2",
        "i32x4.shl",
        "i32x4.sub",
        "return",
    ];
    let code = body(&text, "f");
    assert_eq!(code.iter().filter(|op| !op.starts_with('.')).copied().collect::<Vec<_>>(), want);
    for name in ["f", "g", "h"] {
        let code = body(&text, name);
        assert!(
            !code.iter().any(|op| op.contains("v128.store") || op.contains("__stack_pointer")),
            "{name}:\n{}",
            code.join("\n")
        );
    }
    assert!(body(&text, "g").contains(&"f32x4.div"), "{text}");
    assert_eq!(
        body(&text, "h").iter().filter(|op| op.contains("extract_lane")).count(),
        1,
        "{text}"
    );
}

/// Vectors built from their lanes, a lane put in or read out, and the negation and the bitwise
/// not, through `<wasm_simd128.h>` and as GNU vector operators.
const BUILT: &str = "\
#include <wasm_simd128.h>
typedef int v4si __attribute__((vector_size(16)));
v128_t neg(v128_t a) { return wasm_i32x4_neg(a); }
v128_t not(v128_t a) { return wasm_v128_not(a); }
v128_t splat(int16_t x) { return wasm_i16x8_splat(x); }
v128_t make(float a, float b, float c, float d) { return wasm_f32x4_make(a, b, c, d); }
v128_t konst(void) { return wasm_i32x4_const(1, 2, 3, 4); }
v128_t put(v128_t v, int8_t x) { return wasm_i8x16_replace_lane(v, 5, x); }
int8_t get_s(v128_t v) { return wasm_i8x16_extract_lane(v, 3); }
uint16_t get_u(v128_t v) { return wasm_u16x8_extract_lane(v, 1); }
v4si minus(v4si a) { return -a; }
v4si inv(v4si a) { return ~a; }
v4si four(int x) { return (v4si){x, x, x, x}; }
";

/// Each function of [`BUILT`] is the instructions that clang 23 writes for it at `-O2`, with a
/// `return` at the end where clang falls through. A vector of one value is a `splat`, a vector of
/// values is a `splat` and a `replace_lane` for each other lane, a vector of constants is one
/// `v128.const`, and a `signed char` lane that is returned is read with `extract_lane_s`.
#[test]
fn with_simd128_a_vector_built_from_its_lanes_is_what_clang_writes() {
    let out = assembly(BUILT, &["-msimd128"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let want: [(&str, &[&str]); 11] = [
        ("neg", &["local.get\t0", "i32x4.neg"]),
        ("not", &["local.get\t0", "v128.not"]),
        ("splat", &["local.get\t0", "i16x8.splat"]),
        (
            "make",
            &[
                "local.get\t0",
                "f32x4.splat",
                "local.get\t1",
                "f32x4.replace_lane\t1",
                "local.get\t2",
                "f32x4.replace_lane\t2",
                "local.get\t3",
                "f32x4.replace_lane\t3",
            ],
        ),
        ("konst", &["v128.const\t1, 2, 3, 4"]),
        ("put", &["local.get\t0", "local.get\t1", "i8x16.replace_lane\t5"]),
        ("get_s", &["local.get\t0", "i8x16.extract_lane_s\t3"]),
        ("get_u", &["local.get\t0", "i16x8.extract_lane_u\t1"]),
        ("minus", &["local.get\t0", "i32x4.neg"]),
        ("inv", &["local.get\t0", "v128.not"]),
        ("four", &["local.get\t0", "i32x4.splat"]),
    ];
    for (name, ops) in want {
        let code: Vec<&str> = body(&text, name)
            .into_iter()
            .filter(|op| !op.starts_with('.') && *op != "return")
            .collect();
        assert_eq!(code, ops, "{name}");
    }
}

/// Functions of `<wasm_simd128.h>` that clang writes as a `__builtin_wasm_*` builtin, and the
/// negation of a float vector.
const BUILTIN: &str = "\
#include <wasm_simd128.h>
v128_t abs8(v128_t a) { return wasm_i8x16_abs(a); }
bool all16(v128_t a) { return wasm_i16x8_all_true(a); }
int any(v128_t a) { return wasm_v128_any_true(a) ? 7 : 9; }
uint32_t bits(v128_t a) { return wasm_i32x4_bitmask(a); }
v128_t fneg(v128_t a) { return wasm_f32x4_neg(a); }
v128_t dmin(v128_t a, v128_t b) { return wasm_f64x2_min(a, b); }
v128_t narrow(v128_t a, v128_t b) { return wasm_u8x16_narrow_i16x8(a, b); }
v128_t pick(v128_t a, v128_t b, v128_t m) { return wasm_v128_bitselect(a, b, m); }
v128_t madd(v128_t a, v128_t b, v128_t c) { return wasm_f32x4_relaxed_madd(a, b, c); }
";

/// Each function of [`BUILTIN`] is the instructions that clang 23 writes for it at `-O2` with
/// `-msimd128 -mrelaxed-simd`. The answer of `all_true` and `any_true` is 0 or 1, so it is not
/// compared with 0 again. Without `-mrelaxed-simd`, the relaxed multiply add is a multiply and an
/// add, which is one of the answers that the relaxed instruction allows.
#[test]
fn with_simd128_a_function_of_the_header_is_the_builtin_of_clang() {
    let out = assembly(BUILTIN, &["-msimd128", "-mrelaxed-simd"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let want: [(&str, &[&str]); 9] = [
        ("abs8", &["local.get\t0", "i8x16.abs"]),
        ("all16", &["local.get\t0", "i16x8.all_true"]),
        ("any", &["i32.const\t7", "i32.const\t9", "local.get\t0", "v128.any_true", "i32.select"]),
        ("bits", &["local.get\t0", "i32x4.bitmask"]),
        ("fneg", &["local.get\t0", "f32x4.neg"]),
        ("dmin", &["local.get\t0", "local.get\t1", "f64x2.min"]),
        ("narrow", &["local.get\t0", "local.get\t1", "i8x16.narrow_i16x8_u"]),
        ("pick", &["local.get\t0", "local.get\t1", "local.get\t2", "v128.bitselect"]),
        ("madd", &["local.get\t0", "local.get\t1", "local.get\t2", "f32x4.relaxed_madd"]),
    ];
    for (name, ops) in want {
        let code: Vec<&str> = body(&text, name)
            .into_iter()
            .filter(|op| !op.starts_with('.') && *op != "return")
            .collect();
        assert_eq!(code, ops, "{name}");
    }
    let out = assembly(BUILTIN, &["-msimd128"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let madd: Vec<&str> = body(&text, "madd")
        .into_iter()
        .filter(|op| !op.starts_with('.') && *op != "return")
        .collect();
    assert_eq!(madd, ["local.get\t0", "local.get\t1", "f32x4.mul", "local.get\t2", "f32x4.add"]);
}

/// Functions that load or store one lane, and `wasm_v128_andnot`. `keep` stores between the
/// load and the vector, so its load stays where it is.
const MEMORY: &str = "\
#include <wasm_simd128.h>
typedef int v4si __attribute__((vector_size(16)));
v128_t splat8(const void *p) { return wasm_v128_load8_splat(p); }
v128_t zero64(const void *p) { return wasm_v128_load64_zero(p); }
v128_t lane16(const void *p, v128_t v) { return wasm_v128_load16_lane(p, v, 1); }
void store32(void *p, v128_t v) { wasm_v128_store32_lane(p, v, 3); }
v128_t andnot(v128_t a, v128_t b) { return wasm_v128_andnot(a, b); }
v4si at(const int *p) { return (v4si){p[2], p[2], p[2], p[2]}; }
v4si keep(int *p, int *q) { int x = *p; *q = 0; return (v4si){x, x, x, x}; }
";

/// Each function of [`MEMORY`] but `keep` is the instructions that clang 23 writes for it at
/// `-O2` with `-msimd128`. The load of a lane, of a splat and of lane 0 with zero in the others
/// is one SIMD load, the store of one lane is one SIMD store, and `a & ~b` is `v128.andnot`. In
/// `keep` the load is not moved past the store.
#[test]
fn with_simd128_a_load_or_a_store_of_a_lane_is_one_instruction() {
    let out = assembly(MEMORY, &["-msimd128"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let want: [(&str, &[&str]); 7] = [
        ("splat8", &["local.get\t0", "v128.load8_splat\t0"]),
        ("zero64", &["local.get\t0", "v128.load64_zero\t0:p2align=0"]),
        ("lane16", &["local.get\t0", "local.get\t1", "v128.load16_lane\t0:p2align=0, 1"]),
        ("store32", &["local.get\t0", "local.get\t1", "v128.store32_lane\t0:p2align=0, 3"]),
        ("andnot", &["local.get\t0", "local.get\t1", "v128.andnot"]),
        ("at", &["local.get\t0", "v128.load32_splat\t8"]),
        (
            "keep",
            &[
                "local.get\t0",
                "i32.load\t0",
                "local.set\t0",
                "local.get\t1",
                "i32.const\t0",
                "i32.store\t0",
                "local.get\t0",
                "i32x4.splat",
            ],
        ),
    ];
    for (name, ops) in want {
        let code: Vec<&str> = body(&text, name)
            .into_iter()
            .filter(|op| !op.starts_with('.') && *op != "return")
            .collect();
        assert_eq!(code, ops, "{name}");
    }
}
/// Functions of the header that clang 23 writes as a `__builtin_elementwise_*` builtin, the
/// builtins on vectors with lanes of each width, and the builtins on one integer.
const ELEMENTWISE: &str = "\
#include <wasm_simd128.h>
typedef unsigned char v16u __attribute__((vector_size(16)));
typedef short v8s __attribute__((vector_size(16)));
typedef unsigned v4u __attribute__((vector_size(16)));
typedef long long v2s __attribute__((vector_size(16)));
v128_t min8(v128_t a, v128_t b) { return wasm_i8x16_min(a, b); }
v128_t max16(v128_t a, v128_t b) { return wasm_u16x8_max(a, b); }
v128_t add8(v128_t a, v128_t b) { return wasm_u8x16_add_sat(a, b); }
v128_t sub16(v128_t a, v128_t b) { return wasm_i16x8_sub_sat(a, b); }
v128_t pop8(v128_t a) { return wasm_i8x16_popcnt(a); }
v16u subu(v16u a, v16u b) { return __builtin_elementwise_sub_sat(a, b); }
v8s adds(v8s a, v8s b) { return __builtin_elementwise_add_sat(a, b); }
v4u minu(v4u a, v4u b) { return __builtin_elementwise_min(a, b); }
v2s max2(v2s a, v2s b) { return __builtin_elementwise_max(a, b); }
int maxi(int a, int b) { return __builtin_elementwise_max(a, b); }
";

/// With `-msimd128`, each function of [`ELEMENTWISE`] whose lanes have an instruction is that
/// instruction, as clang 23 writes it. wasm has no `min` or `max` of 64-bit lanes, so `max2` is a
/// lane at a time, and a builtin on one integer is a compare and a `select`.
#[test]
fn with_simd128_an_elementwise_builtin_is_one_instruction() {
    let out = assembly(ELEMENTWISE, &["-msimd128"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let want: [(&str, &[&str]); 8] = [
        ("min8", &["local.get\t0", "local.get\t1", "i8x16.min_s"]),
        ("max16", &["local.get\t0", "local.get\t1", "i16x8.max_u"]),
        ("add8", &["local.get\t0", "local.get\t1", "i8x16.add_sat_u"]),
        ("sub16", &["local.get\t0", "local.get\t1", "i16x8.sub_sat_s"]),
        ("pop8", &["local.get\t0", "i8x16.popcnt"]),
        ("subu", &["local.get\t0", "local.get\t1", "i8x16.sub_sat_u"]),
        ("adds", &["local.get\t0", "local.get\t1", "i16x8.add_sat_s"]),
        ("minu", &["local.get\t0", "local.get\t1", "i32x4.min_u"]),
    ];
    for (name, ops) in want {
        let code: Vec<&str> = body(&text, name)
            .into_iter()
            .filter(|op| !op.starts_with('.') && *op != "return")
            .collect();
        assert_eq!(code, ops, "{name}");
    }
    let max2 = body(&text, "max2");
    assert!(max2.contains(&"i64.lt_s") && max2.contains(&"i64x2.replace_lane\t1"), "{max2:?}");
    let maxi = body(&text, "maxi");
    assert!(maxi.contains(&"i32.lt_s") && maxi.contains(&"i32.select"), "{maxi:?}");
}

/// The words of clang 23 for an operand that is not an integer, for two operands of different
/// types, and for a wrong number of operands. clang takes `_Bool` and `char` operands as they are.
#[test]
fn the_elementwise_builtins_are_checked_in_the_words_of_clang() {
    let source = "\
int f(int a, long b) { return __builtin_elementwise_min(a, b); }
float g(float a) { return __builtin_elementwise_add_sat(a, a); }
int h(int a) { return __builtin_elementwise_max(a); }
int *k(int *p) { return __builtin_elementwise_popcount(p); }
int n(int a) { return __builtin_elementwise_popcount((_Bool)a) + __builtin_elementwise_add_sat((char)a, (char)1); }
";
    let out = assembly(source, &[]);
    let errors = String::from_utf8_lossy(&out.stderr);
    let want = [
        "<stdin>:1:57: error: arguments are of different types ('int' vs 'long') [E0685]",
        "<stdin>:2:57: error: 1st argument must be a scalar or vector of integer types (was 'float') [E0685]",
        "<stdin>:3:23: error: too few arguments to function call, expected 2, have 1 [E0511]",
        "<stdin>:4:56: error: 1st argument must be a scalar or vector of integer types (was 'int *') [E0685]",
    ];
    let said: Vec<&str> = errors.lines().filter(|line| line.contains("error:")).collect();
    assert_eq!(said, want, "{errors}");
}

/// A program that does each operator on vectors of edge values, and the same operator on each
/// lane as a scalar, and exits with 1 if a lane differs. The scalar side is in a function that is
/// not inlined, so it is plain scalar code. A compare gives -1 or 0 in each lane. Every second lane
/// of `c` is the lane of `a`, so that each compare has equal lanes too. The shift count is less than
/// the width of the lane, and it comes from a `volatile`, so that it is not a constant.
const CHECK: &str = "\
#include <string.h>
#define V(n, t) typedef t n __attribute__((vector_size(16)));
V(v16s, signed char) V(v16u, unsigned char) V(v8s, short) V(v8u, unsigned short)
V(v4s, int) V(v4u, unsigned) V(v2s, long long) V(v2u, unsigned long long)
V(v4f, float) V(v2d, double)
static const unsigned char edge[] = {0, 1, 0x7f, 0x80, 0xff, 0xfe, 0x55, 0xaa};
static void fill(void *p, int k) {
    unsigned char *b = p;
    for (int i = 0; i < 16; i++) b[i] = edge[(i * 3 + k) & 7] ^ (unsigned char)(i * k);
}
static int bad;
static volatile int count = 13;
#define SCALAR(n, t, op) \\
    __attribute__((noinline)) static void n(t *r, const t *a, const t *b, int lanes) { \\
        for (int i = 0; i < lanes; i++) r[i] = (t)(a[i] op b[i]); }
#define COMPARE(n, t, m, op) \\
    __attribute__((noinline)) static void n(m *r, const t *a, const t *b, int lanes) { \\
        for (int i = 0; i < lanes; i++) r[i] = a[i] op b[i] ? -1 : 0; }
#define COMPARES(v, t, m) \\
    COMPARE(eq_##v, t, m, ==) COMPARE(ne_##v, t, m, !=) COMPARE(lt_##v, t, m, <) \\
    COMPARE(gt_##v, t, m, >) COMPARE(le_##v, t, m, <=) COMPARE(ge_##v, t, m, >=) \\
    static void compare_##v(v a, v b) { \\
        v c = a; __typeof__(a < b) r, s; int n = 16 / sizeof(t); \\
        for (int i = 0; i < n; i += 2) c[i + 1] = b[i + 1]; \\
        r = a == c; eq_##v((m *)&s, (t *)&a, (t *)&c, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a != c; ne_##v((m *)&s, (t *)&a, (t *)&c, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a < c; lt_##v((m *)&s, (t *)&a, (t *)&c, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a > c; gt_##v((m *)&s, (t *)&a, (t *)&c, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a <= c; le_##v((m *)&s, (t *)&a, (t *)&c, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a >= c; ge_##v((m *)&s, (t *)&a, (t *)&c, n); bad |= memcmp(&r, &s, 16) != 0; }
#define SHIFT(n, t, u, op) \\
    __attribute__((noinline)) static void n(t *r, const t *a, int k, int lanes) { \\
        for (int i = 0; i < lanes; i++) r[i] = (t)((u)a[i] op k); }
#define INT(v, t, m) \\
    SCALAR(add_##v, t, +) SCALAR(sub_##v, t, -) SCALAR(mul_##v, t, *) \\
    SCALAR(and_##v, t, &) SCALAR(or_##v, t, |) SCALAR(xor_##v, t, ^) \\
    COMPARES(v, t, m) SHIFT(shl_##v, t, unsigned long long, <<) SHIFT(shr_##v, t, t, >>) \\
    static void check_##v(void) { \\
        v a, b, r, s; fill(&a, 1); fill(&b, 2); int n = 16 / sizeof(t); \\
        int k = count % (8 * sizeof(t)); compare_##v(a, b); \\
        r = a << k; shl_##v((t *)&s, (t *)&a, k, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a >> k; shr_##v((t *)&s, (t *)&a, k, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a << 3; shl_##v((t *)&s, (t *)&a, 3, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a >> 3; shr_##v((t *)&s, (t *)&a, 3, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a + b; add_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a - b; sub_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a * b; mul_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a & b; and_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a | b; or_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a ^ b; xor_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; }
#define FLT(v, t, m) \\
    SCALAR(add_##v, t, +) SCALAR(sub_##v, t, -) SCALAR(mul_##v, t, *) SCALAR(div_##v, t, /) \\
    COMPARES(v, t, m) \\
    static void check_##v(void) { \\
        v a, b, r, s; int n = 16 / sizeof(t); \\
        for (int i = 0; i < n; i++) { a[i] = (t)(i * 7 - 9) / 3; b[i] = (t)(5 - i * 11) / 7; } \\
        compare_##v(a, b); b[0] = __builtin_nan(\"\"); compare_##v(a, b); compare_##v(b, a); \\
        r = a + b; add_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a - b; sub_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a * b; mul_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a / b; div_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; }
INT(v16s, signed char, signed char) INT(v16u, unsigned char, signed char)
INT(v8s, short, short) INT(v8u, unsigned short, short) INT(v4s, int, int) INT(v4u, unsigned, int)
INT(v2s, long long, long long) INT(v2u, unsigned long long, long long)
FLT(v4f, float, int) FLT(v2d, double, long long)
int main(void) {
    check_v16s(); check_v16u(); check_v8s(); check_v8u(); check_v4s(); check_v4u();
    check_v2s(); check_v2u(); check_v4f(); check_v2d();
    return bad;
}
";

fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(program)).find(|file| file.is_file())
}

fn run(program: &Path, args: &[&str], dir: &Path) {
    let out = Command::new(program)
        .env("LC_ALL", "C")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the program starts");
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn each_lane_has_the_value_of_the_scalar_operator() {
    if std::env::var_os("WASI_SDK_PATH").is_none() {
        eprintln!("WASI_SDK_PATH is not set, so the program was not linked");
        return;
    }
    let Some(wasmtime) = on_path("wasmtime") else {
        eprintln!("there is no wasmtime on PATH, so the program was not run");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rucc-wasm-simd-lanes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("check.c"), CHECK).unwrap();
    let rucc = PathBuf::from(env!("CARGO_BIN_EXE_rucc"));
    for flags in [&["-O0"][..], &["-O2"], &["-O0", "-msimd128"], &["-O2", "-msimd128"]] {
        // The program needs nothing from librucc_builtins.a, so the test does not need it built.
        let link = ["--target=wasm32-wasip1", "-fno-builtins-lib", "check.c", "-o", "check.wasm"];
        let args = [&link[..], flags].concat();
        run(&rucc, &args, &dir);
        let status = Command::new(&wasmtime).arg(dir.join("check.wasm")).status().unwrap();
        assert!(status.success(), "{flags:?}: {status}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// A program that compares each `__builtin_elementwise_*` builtin with the same operation on one
/// lane at a time, written in plain C, on vectors of each lane type and on each scalar.
const SATURATE: &str = "\
#include <limits.h>
#define V(n, t) typedef t n __attribute__((vector_size(16)));
V(v16s, signed char) V(v16u, unsigned char) V(v8s, short) V(v8u, unsigned short)
V(v4s, int) V(v4u, unsigned) V(v2s, long long) V(v2u, unsigned long long)
static const unsigned char edge[] = {0, 1, 0x7f, 0x80, 0xff, 0xfe, 0x55, 0xaa};
static void fill(void *p, int k) {
    unsigned char *b = p;
    for (int i = 0; i < 16; i++) b[i] = edge[(i * 3 + k) & 7] ^ (unsigned char)(i * k);
}
static int bad;
#define TYPE(v, t, lo, hi) \\
    __attribute__((noinline)) static t add_##v(t a, t b) { \\
        t r; return __builtin_add_overflow(a, b, &r) ? (lo < 0 && a < 0 ? lo : hi) : r; } \\
    __attribute__((noinline)) static t sub_##v(t a, t b) { \\
        t r; return __builtin_sub_overflow(a, b, &r) ? (lo == 0 || a < 0 ? lo : hi) : r; } \\
    __attribute__((noinline)) static t pop_##v(t a) { \\
        unsigned long long x = 0; __builtin_memcpy(&x, &a, sizeof a); \\
        return (t)__builtin_popcountll(x); } \\
    static void check_##v(int k) { \\
        v a, b, r; fill(&a, k); fill(&b, k + 1); int n = 16 / sizeof(t); \\
        for (int i = 0; i < n; i++) { \\
            t x = a[i], y = b[i]; \\
            bad |= __builtin_elementwise_min(x, y) != (x < y ? x : y); \\
            bad |= __builtin_elementwise_max(x, y) != (x < y ? y : x); \\
            bad |= __builtin_elementwise_add_sat(x, y) != add_##v(x, y); \\
            bad |= __builtin_elementwise_sub_sat(x, y) != sub_##v(x, y); \\
            bad |= __builtin_elementwise_popcount(x) != pop_##v(x); } \\
        r = __builtin_elementwise_min(a, b); \\
        for (int i = 0; i < n; i++) bad |= r[i] != (a[i] < b[i] ? a[i] : b[i]); \\
        r = __builtin_elementwise_max(a, b); \\
        for (int i = 0; i < n; i++) bad |= r[i] != (a[i] < b[i] ? b[i] : a[i]); \\
        r = __builtin_elementwise_add_sat(a, b); \\
        for (int i = 0; i < n; i++) bad |= r[i] != add_##v(a[i], b[i]); \\
        r = __builtin_elementwise_sub_sat(a, b); \\
        for (int i = 0; i < n; i++) bad |= r[i] != sub_##v(a[i], b[i]); \\
        r = __builtin_elementwise_popcount(a); \\
        for (int i = 0; i < n; i++) bad |= r[i] != pop_##v(a[i]); }
TYPE(v16s, signed char, SCHAR_MIN, SCHAR_MAX) TYPE(v16u, unsigned char, 0, UCHAR_MAX)
TYPE(v8s, short, SHRT_MIN, SHRT_MAX) TYPE(v8u, unsigned short, 0, USHRT_MAX)
TYPE(v4s, int, INT_MIN, INT_MAX) TYPE(v4u, unsigned, 0, UINT_MAX)
TYPE(v2s, long long, LLONG_MIN, LLONG_MAX) TYPE(v2u, unsigned long long, 0, ULLONG_MAX)
int main(void) {
    for (int k = 0; k < 8; k++) {
        check_v16s(k); check_v16u(k); check_v8s(k); check_v8u(k);
        check_v4s(k); check_v4u(k); check_v2s(k); check_v2u(k);
    }
    return bad;
}
";

#[test]
fn the_elementwise_builtins_have_the_value_of_each_lane() {
    let dir = std::env::temp_dir().join(format!("rucc-wasm-elementwise-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("check.c"), SATURATE).unwrap();
    let rucc = PathBuf::from(env!("CARGO_BIN_EXE_rucc"));
    // On the host first, where each builtin is a lane at a time.
    for level in ["-O0", "-O2"] {
        run(&rucc, &[level, "check.c", "-o", "check"], &dir);
        let status = Command::new(dir.join("check")).status().unwrap();
        assert!(status.success(), "{level}: {status}");
    }
    let wasmtime = on_path("wasmtime");
    if std::env::var_os("WASI_SDK_PATH").is_none() || wasmtime.is_none() {
        eprintln!("WASI_SDK_PATH is not set or there is no wasmtime, so wasm was not run");
        std::fs::remove_dir_all(dir).unwrap();
        return;
    }
    for flags in [&["-O0"][..], &["-O2"], &["-O0", "-msimd128"], &["-O2", "-msimd128"]] {
        let link = ["--target=wasm32-wasip1", "-fno-builtins-lib", "check.c", "-o", "check.wasm"];
        run(&rucc, &[&link[..], flags].concat(), &dir);
        let status =
            Command::new(wasmtime.as_ref().unwrap()).arg(dir.join("check.wasm")).status().unwrap();
        assert!(status.success(), "{flags:?}: {status}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

const CONVERT: &str = "\
#include <wasm_simd128.h>
typedef int v4s __attribute__((vector_size(16)));
typedef unsigned v4u __attribute__((vector_size(16)));
typedef float v4f __attribute__((vector_size(16)));
typedef double v2d __attribute__((vector_size(16)));
typedef unsigned short v8u __attribute__((vector_size(16)));
typedef unsigned char v8b __attribute__((vector_size(8)));
v4f fromu(v4u a) { return __builtin_convertvector(a, v4f); }
v4s trunc(v4f a) { return __builtin_convertvector(a, v4s); }
v4u same(v4s a) { return __builtin_convertvector(a, v4u); }
v8u widen(const v8b *p) { return __builtin_convertvector(*p, v8u); }
v128_t froms(v128_t a) { return wasm_f32x4_convert_i32x4(a); }
v128_t load8(const void *p) { return wasm_u16x8_load8x8(p); }
v128_t load16(const void *p) { return wasm_i32x4_load16x4(p); }
v128_t load32(const void *p) { return wasm_u64x2_load32x2(p); }
";

/// With `-msimd128`, a conversion of [`CONVERT`] that wasm has an instruction for is that
/// instruction, and an extend of eight bytes that are loaded is one extending load, as clang 23
/// writes them, with the alignment of the vector or of the packed structure in the header. A
/// conversion that keeps the bits of each lane is no instruction at all.
#[test]
fn with_simd128_a_convertvector_is_one_instruction() {
    let out = assembly(CONVERT, &["-msimd128"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let want: [(&str, &[&str]); 8] = [
        ("fromu", &["local.get\t0", "f32x4.convert_i32x4_u"]),
        ("trunc", &["local.get\t0", "i32x4.trunc_sat_f32x4_s"]),
        ("same", &["local.get\t0"]),
        ("widen", &["local.get\t0", "i16x8.load8x8_u\t0"]),
        ("froms", &["local.get\t0", "f32x4.convert_i32x4_s"]),
        ("load8", &["local.get\t0", "i16x8.load8x8_u\t0:p2align=0"]),
        ("load16", &["local.get\t0", "i32x4.load16x4_s\t0:p2align=0"]),
        ("load32", &["local.get\t0", "i64x2.load32x2_u\t0:p2align=0"]),
    ];
    for (name, ops) in want {
        let code: Vec<&str> = body(&text, name)
            .into_iter()
            .filter(|op| !op.starts_with('.') && *op != "return")
            .collect();
        assert_eq!(code, ops, "{name}");
    }
}

/// The words of clang 23 for an operand that is not a vector, for a type that is not a vector
/// type, and for two vectors with a different number of lanes. Each error is at the start of the
/// call.
#[test]
fn convertvector_is_checked_in_the_words_of_clang() {
    let source = "\
typedef int v4si __attribute__((vector_size(16)));
typedef float v2sf __attribute__((vector_size(8)));
v4si f(int a) { return __builtin_convertvector(a, v4si); }
int g(v4si a) { return __builtin_convertvector(a, int); }
v2sf h(v4si a) { return __builtin_convertvector(a, v2sf); }
";
    let out = assembly(source, &[]);
    let errors = String::from_utf8_lossy(&out.stderr);
    let want = [
        "<stdin>:3:24: error: first argument to __builtin_convertvector must be a vector [E0685]",
        "<stdin>:4:24: error: second argument to __builtin_convertvector must be of vector type [E0685]",
        "<stdin>:5:25: error: first two arguments to __builtin_convertvector must have the same number of elements [E0685]",
    ];
    let said: Vec<&str> = errors.lines().filter(|line| line.contains("error:")).collect();
    assert_eq!(said, want, "{errors}");
}

/// A program that compares `__builtin_convertvector` with a cast of each lane, for each pair of
/// lane types that wasm has an instruction for and for some that it does not. Each value fits in
/// the type that it is converted to, so that each cast has a value in C.
const CONVERTED: &str = "\
#define V(n, t, s) typedef t n __attribute__((vector_size(s)));
V(v16s, signed char, 16) V(v8sb, signed char, 8) V(v8ub, unsigned char, 8) V(v4sb, signed char, 4)
V(v8s, short, 16) V(v8u, unsigned short, 16) V(v4sh, short, 8) V(v4uh, unsigned short, 8)
V(v4s, int, 16) V(v4u, unsigned, 16) V(v2si, int, 8) V(v2ui, unsigned, 8)
V(v2s, long long, 16) V(v2u, unsigned long long, 16)
V(v4f, float, 16) V(v2f, float, 8) V(v2d, double, 16)
static const int edge[] = {0, 1, -1, 127, -128, 255, 32767, -32768, 65535, 100000, -7, 42};
static int bad;
#define CHECK(name, from, into, lanes, t, k) \\
    __attribute__((noinline)) static into name(from a) { return __builtin_convertvector(a, into); } \\
    static void check_##name(void) { \\
        from a; for (int i = 0; i < lanes; i++) a[i] = (t)(edge[(i * 5 + __LINE__) % 12] k); \\
        into r = name(a); \\
        for (int i = 0; i < lanes; i++) bad |= r[i] != (__typeof__(r[0]))a[i]; }
CHECK(s8s16, v8sb, v8s, 8, signed char, +0)
CHECK(u8u16, v8ub, v8u, 8, unsigned char, +0)
CHECK(s16s32, v4sh, v4s, 4, short, +0)
CHECK(u16u32, v4uh, v4u, 4, unsigned short, +0)
CHECK(s32s64, v2si, v2s, 2, int, +0)
CHECK(u32u64, v2ui, v2u, 2, unsigned, +0)
CHECK(s32f32, v4s, v4f, 4, int, *3)
CHECK(u32f32, v4u, v4f, 4, unsigned, *3)
CHECK(f32s32, v4f, v4s, 4, float, +1)
CHECK(s32f64, v2si, v2d, 2, int, *9)
CHECK(u32f64, v2ui, v2d, 2, unsigned, *9)
CHECK(f32f64, v2f, v2d, 2, float, +2)
CHECK(f64f32, v2d, v2f, 2, double, +3)
CHECK(s32s8, v4s, v4sb, 4, int, %100)
CHECK(f64s64, v2d, v2s, 2, double, -5)
CHECK(s32u32, v4s, v4u, 4, int, +4)
__attribute__((noinline)) static v8s load(const void *p) { return __builtin_convertvector(*(const v8sb *)p, v8s); }
int main(void) {
    check_s8s16(); check_u8u16(); check_s16s32(); check_u16u32(); check_s32s64(); check_u32u64();
    check_s32f32(); check_u32f32(); check_f32s32(); check_s32f64(); check_u32f64();
    check_f32f64(); check_f64f32(); check_s32s8(); check_f64s64(); check_s32u32();
    signed char raw[9] = {9, -1, -2, 3, 4, -128, 127, 0, 9};
    v8s w = load(raw + 1);
    for (int i = 0; i < 8; i++) bad |= w[i] != raw[i + 1];
    return bad;
}
";

#[test]
fn convertvector_casts_each_lane() {
    let dir = std::env::temp_dir().join(format!("rucc-wasm-convert-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("check.c"), CONVERTED).unwrap();
    let rucc = PathBuf::from(env!("CARGO_BIN_EXE_rucc"));
    // On the host first, where each conversion is a lane at a time.
    for level in ["-O0", "-O2"] {
        run(&rucc, &[level, "check.c", "-o", "check"], &dir);
        let status = Command::new(dir.join("check")).status().unwrap();
        assert!(status.success(), "{level}: {status}");
    }
    let wasmtime = on_path("wasmtime");
    if std::env::var_os("WASI_SDK_PATH").is_none() || wasmtime.is_none() {
        eprintln!("WASI_SDK_PATH is not set or there is no wasmtime, so wasm was not run");
        std::fs::remove_dir_all(dir).unwrap();
        return;
    }
    for flags in [&["-O0"][..], &["-O2"], &["-O0", "-msimd128"], &["-O2", "-msimd128"]] {
        let link = ["--target=wasm32-wasip1", "-fno-builtins-lib", "check.c", "-o", "check.wasm"];
        run(&rucc, &[&link[..], flags].concat(), &dir);
        let status =
            Command::new(wasmtime.as_ref().unwrap()).arg(dir.join("check.wasm")).status().unwrap();
        assert!(status.success(), "{flags:?}: {status}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Shuffles of vectors of 16 bytes, each written in a different way.
const SHUFFLE: &str = "\
#include <wasm_simd128.h>
typedef int v4s __attribute__((vector_size(16)));
typedef short v8s __attribute__((vector_size(16)));
typedef double v2d __attribute__((vector_size(16)));
typedef signed char v16 __attribute__((vector_size(16)));
v4s pick(v4s a, v4s b) { return __builtin_shufflevector(a, b, 1, 4, 7, 2); }
v8s reverse(v8s a) { return __builtin_shufflevector(a, a, 7, 6, 5, 4, 3, 2, 1, 0); }
v2d high(v2d a, v2d b) { return __builtin_shufflevector(a, b, 1, 2); }
v16 bytes(v16 a, v16 b) {
    return __builtin_wasm_shuffle_i8x16(a, b, 0, 17, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 40);
}
v128_t header(v128_t a, v128_t b) { return wasm_i16x8_shuffle(a, b, 0, 8, 1, 9, 2, 10, 3, 11); }
";

/// With `-msimd128`, each shuffle of [`SHUFFLE`] is one `i8x16.shuffle` of its two operands, with
/// the bytes of each lane, as clang 23 writes it. A lane of the wasm builtin past 31 is 0, as clang
/// writes it.
#[test]
fn with_simd128_a_shufflevector_is_one_instruction() {
    let out = assembly(SHUFFLE, &["-msimd128"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let want = [
        ("pick", "1", "4, 5, 6, 7, 16, 17, 18, 19, 28, 29, 30, 31, 8, 9, 10, 11"),
        ("reverse", "0", "14, 15, 12, 13, 10, 11, 8, 9, 6, 7, 4, 5, 2, 3, 0, 1"),
        ("high", "1", "8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23"),
        ("bytes", "1", "0, 17, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 0"),
        ("header", "1", "0, 1, 16, 17, 2, 3, 18, 19, 4, 5, 20, 21, 6, 7, 22, 23"),
    ];
    for (name, second, lanes) in want {
        let code: Vec<&str> = body(&text, name)
            .into_iter()
            .filter(|op| !op.starts_with('.') && *op != "return")
            .collect();
        let shuffle = format!("i8x16.shuffle\t{lanes}");
        let second = format!("local.get\t{second}");
        assert_eq!(code, ["local.get\t0", &second, &shuffle], "{name}");
    }
}

/// The words of gcc 16 for each call to `__builtin_shufflevector` that it refuses, in the order
/// that gcc checks, and the words of clang 23 for a lane of `__builtin_wasm_shuffle_i8x16` that is
/// not a constant. Each error is at the start of the call.
#[test]
fn shufflevector_is_checked_in_the_words_of_gcc() {
    let source = "\
typedef int v4si __attribute__((vector_size(16)));
typedef short v8hi __attribute__((vector_size(16)));
typedef signed char v16 __attribute__((vector_size(16)));
v4si a(v4si a, v8hi b) { return __builtin_shufflevector(a, b, 0, 1, 2, 3); }
v4si b(v4si a, int n) { return __builtin_shufflevector(a, a, n, 1, 2, 3); }
v4si c(v4si a) { return __builtin_shufflevector(a, a, 0, 1, 2, 8); }
v4si d(int a) { return __builtin_shufflevector(a, a, 0, 1, 2, 3); }
v4si e(v4si a) { return __builtin_shufflevector(a); }
v4si f(v4si a) { return __builtin_shufflevector(a, a, 0, 1, 2); }
v4si g(v4si a) { return __builtin_shufflevector(a, a, 0, 1, 2, -2); }
v16 h(v16 a, int n) { return __builtin_wasm_shuffle_i8x16(a, a, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, n, 15); }
";
    let out = assembly(source, &["-msimd128"]);
    let errors = String::from_utf8_lossy(&out.stderr);
    let want = [
        "<stdin>:4:33: error: '__builtin_shufflevector' argument vectors must have the same element type [E0715]",
        "<stdin>:5:32: error: invalid element index 'n' to '__builtin_shufflevector' [E0715]",
        "<stdin>:6:25: error: invalid element index '8' to '__builtin_shufflevector' [E0715]",
        "<stdin>:7:24: error: '__builtin_shufflevector' arguments must be vectors [E0715]",
        "<stdin>:8:25: error: wrong number of arguments to '__builtin_shuffle' [E0511]",
        "<stdin>:9:25: error: '__builtin_shufflevector' must specify a result with a power of two number of elements [E0715]",
        "<stdin>:10:25: error: invalid element index '-2' to '__builtin_shufflevector' [E0715]",
        "<stdin>:11:30: error: argument to '__builtin_wasm_shuffle_i8x16' must be a constant integer [E0715]",
    ];
    let said: Vec<&str> = errors.lines().filter(|line| line.contains("error:")).collect();
    assert_eq!(said, want, "{errors}");
}

/// A program that compares each lane of `__builtin_shufflevector` with the lane that it picks, for
/// operands and answers of 4, 8, 16 and 32 bytes, operands with a different number of lanes, a
/// lane of -1, which has no value to compare, and an answer that is written over its operand.
const SHUFFLED: &str = "\
#define V(n, t, s) typedef t n __attribute__((vector_size(s)));
V(v16, signed char, 16) V(v8b, signed char, 8) V(v8s, short, 16) V(v4s, int, 16) V(v2i, int, 8)
V(v1i, int, 4) V(v8i, int, 32) V(v2s, long long, 16) V(v4f, float, 16) V(v2d, double, 16)
static const int edge[] = {0, 1, -1, 127, -128, 100, 32767, -32768, 65535, 100000, -7, 42};
static int bad;
#define LANES(v) ((int)(sizeof (v) / sizeof (v)[0]))
#define CHECK(name, ta, tb, tr, ...) \\
    __attribute__((noinline)) static tr name(ta a, tb b) { return __builtin_shufflevector(a, b, __VA_ARGS__); } \\
    static void check_##name(void) { \\
        static const int pick[] = {__VA_ARGS__}; \\
        ta a; tb b; \\
        for (int i = 0; i < LANES(a); i++) a[i] = edge[(i + __LINE__) % 12]; \\
        for (int i = 0; i < LANES(b); i++) b[i] = edge[(i * 7 + __LINE__) % 12] + 1; \\
        tr r = name(a, b); \\
        for (int i = 0; i < LANES(r); i++) \\
            if (pick[i] >= 0) bad |= r[i] != (pick[i] < LANES(a) ? a[pick[i]] : b[pick[i] - LANES(a)]); }
CHECK(ints, v4s, v4s, v4s, 1, 4, 7, 2)
CHECK(shorts, v8s, v8s, v8s, 7, 6, 5, 4, 3, 2, 1, 0)
CHECK(bytes, v16, v16, v16, 0, 17, 2, 19, 4, 21, 6, 23, 8, 25, 10, 27, 12, 29, 14, 31)
CHECK(floats, v4f, v4f, v4f, 3, 3, 4, 0)
CHECK(doubles, v2d, v2d, v2d, 1, 2)
CHECK(longs, v2s, v2s, v2s, 3, 0)
CHECK(half, v4s, v4s, v2i, 2, 7)
CHECK(join, v2i, v2i, v4s, 0, 2, 1, 3)
CHECK(mixed, v2i, v4s, v2i, 5, 1)
CHECK(wide, v4s, v4s, v8i, 0, 4, 1, 5, 2, 6, 3, 7)
CHECK(unknown, v4s, v4s, v4s, 0, -1, 5, -1)
CHECK(one, v4s, v4s, v1i, 6)
CHECK(small, v8b, v8b, v8b, 15, 0, 9, 1, 2, 3, 4, 5)
__attribute__((noinline)) static void turn(v4s *p) { *p = __builtin_shufflevector(*p, *p, 3, 2, 1, 0); }
int main(void) {
    check_ints(); check_shorts(); check_bytes(); check_floats(); check_doubles(); check_longs();
    check_half(); check_join(); check_mixed(); check_wide(); check_unknown(); check_one();
    check_small();
    v4s v = {10, 11, 12, 13};
    turn(&v);
    bad |= v[0] != 13 || v[1] != 12 || v[2] != 11 || v[3] != 10;
    return bad;
}
";

#[test]
fn shufflevector_picks_each_lane() {
    let dir = std::env::temp_dir().join(format!("rucc-wasm-shuffle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("check.c"), SHUFFLED).unwrap();
    let rucc = PathBuf::from(env!("CARGO_BIN_EXE_rucc"));
    // On the host first, where each shuffle is a lane at a time.
    for level in ["-O0", "-O2"] {
        run(&rucc, &[level, "check.c", "-o", "check"], &dir);
        let status = Command::new(dir.join("check")).status().unwrap();
        assert!(status.success(), "{level}: {status}");
    }
    let wasmtime = on_path("wasmtime");
    if std::env::var_os("WASI_SDK_PATH").is_none() || wasmtime.is_none() {
        eprintln!("WASI_SDK_PATH is not set or there is no wasmtime, so wasm was not run");
        std::fs::remove_dir_all(dir).unwrap();
        return;
    }
    for flags in [&["-O0"][..], &["-O2"], &["-O0", "-msimd128"], &["-O2", "-msimd128"]] {
        let link = ["--target=wasm32-wasip1", "-fno-builtins-lib", "check.c", "-o", "check.wasm"];
        run(&rucc, &[&link[..], flags].concat(), &dir);
        let status =
            Command::new(wasmtime.as_ref().unwrap()).arg(dir.join("check.wasm")).status().unwrap();
        assert!(status.success(), "{flags:?}: {status}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
