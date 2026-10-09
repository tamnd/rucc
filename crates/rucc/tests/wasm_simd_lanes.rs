//! The lanewise operators of GNU vectors on wasm32 with `-msimd128`. An add, a subtract, a multiply,
//! a divide of floats and the bitwise operators on a vector of sixteen bytes are each one SIMD
//! instruction, which is what clang writes. wasm has no multiply of sixteen `char` lanes, so that
//! one stays a lane at a time, and without `-msimd128` each operator is a lane at a time.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// One function for each operator, over each lane type of sixteen bytes.
const OPERATORS: &str = "\
typedef unsigned char v16qu __attribute__((vector_size(16)));
typedef short v8hi __attribute__((vector_size(16)));
typedef int v4si __attribute__((vector_size(16)));
typedef long long v2di __attribute__((vector_size(16)));
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
    ] {
        let code = body(&text, name);
        for op in want {
            assert!(code.contains(op), "no `{op}` in {name}:\n{}", code.join("\n"));
        }
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

/// A program that does each operator on vectors of edge values, and the same operator on each
/// lane as a scalar, and exits with 1 if a lane differs. The scalar side is in a function that is
/// not inlined, so it is plain scalar code.
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
#define SCALAR(n, t, op) \\
    __attribute__((noinline)) static void n(t *r, const t *a, const t *b, int lanes) { \\
        for (int i = 0; i < lanes; i++) r[i] = (t)(a[i] op b[i]); }
#define INT(v, t) \\
    SCALAR(add_##v, t, +) SCALAR(sub_##v, t, -) SCALAR(mul_##v, t, *) \\
    SCALAR(and_##v, t, &) SCALAR(or_##v, t, |) SCALAR(xor_##v, t, ^) \\
    static void check_##v(void) { \\
        v a, b, r, s; fill(&a, 1); fill(&b, 2); int n = 16 / sizeof(t); \\
        r = a + b; add_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a - b; sub_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a * b; mul_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a & b; and_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a | b; or_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a ^ b; xor_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; }
#define FLT(v, t) \\
    SCALAR(add_##v, t, +) SCALAR(sub_##v, t, -) SCALAR(mul_##v, t, *) SCALAR(div_##v, t, /) \\
    static void check_##v(void) { \\
        v a, b, r, s; int n = 16 / sizeof(t); \\
        for (int i = 0; i < n; i++) { a[i] = (t)(i * 7 - 9) / 3; b[i] = (t)(5 - i * 11) / 7; } \\
        r = a + b; add_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a - b; sub_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a * b; mul_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; \\
        r = a / b; div_##v((t *)&s, (t *)&a, (t *)&b, n); bad |= memcmp(&r, &s, 16) != 0; }
INT(v16s, signed char) INT(v16u, unsigned char) INT(v8s, short) INT(v8u, unsigned short)
INT(v4s, int) INT(v4u, unsigned) INT(v2s, long long) INT(v2u, unsigned long long)
FLT(v4f, float) FLT(v2d, double)
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
