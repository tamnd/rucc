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
