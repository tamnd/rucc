//! `<tgmath.h>` on Apple and Windows targets is rucc's own, written with `_Generic`, since Apple's
//! copy needs clang's `overloadable` attribute and mingw-w64 has none. Meson's C99 check includes
//! it, and a meson build of Postgres on macOS or Windows stopped there. Each macro has to reach the function a type generic call names: the
//! real one for the argument's type, `double` for an integer, and the complex one for a complex
//! argument. Elsewhere the library's header is the one used.
//!
//! The library headers are small stand-ins, so the test needs no SDK.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The directory rucc's own headers are written to.
fn runtime_include() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("-print-file-name=include")
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a path is text").trim().to_string()
}

/// A directory of its own for one test, holding the given files.
fn directory(what: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-tgmath-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("a fixture can be written");
    }
    dir
}

/// The stand-in for the library's `<math.h>`, with every function a macro below can pick.
const MATH: &str = "\
float sinf(float);
double sin(double);
long double sinl(long double);
float powf(float, float);
double pow(double, double);
long double powl(long double, long double);
float fabsf(float);
double fabs(double);
long double fabsl(long double);
float fmaxf(float, float);
double fmax(double, double);
long double fmaxl(long double, long double);
float ldexpf(float, int);
double ldexp(double, int);
long double ldexpl(long double, int);
";

/// The stand-in for the library's `<complex.h>`.
const COMPLEX: &str = "\
float _Complex csinf(float _Complex);
double _Complex csin(double _Complex);
long double _Complex csinl(long double _Complex);
float _Complex cpowf(float _Complex, float _Complex);
double _Complex cpow(double _Complex, double _Complex);
long double _Complex cpowl(long double _Complex, long double _Complex);
float cabsf(float _Complex);
double cabs(double _Complex);
long double cabsl(long double _Complex);
float crealf(float _Complex);
double creal(double _Complex);
long double creall(long double _Complex);
";

/// Compile one source with the stand-in headers and return the listing.
fn listing(dir: &Path, target: &str, source: &str) -> String {
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O0", "-S", "-o", "-", "-nostdinc", "-isystem", &runtime_include(), "-idirafter"])
        .arg(dir)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

const CALLS: &str = "\
#include <tgmath.h>
float a(float x) { return sin(x); }
double b(int x) { return sin(x); }
long double c(long double x) { return sin(x); }
double _Complex d(double _Complex x) { return sin(x); }
float _Complex e(float x, float _Complex y) { return pow(x, y); }
float f(float _Complex x) { return fabs(x); }
long double g(long double x, int y) { return fmax(x, y); }
float h(float x, int n) { return ldexp(x, n); }
double i(double _Complex x) { return creal(x); }
";

/// The functions the calls in `CALLS` have to reach, in order.
const WANTED: [&str; 9] =
    ["sinf", "sin", "sinl", "csin", "cpowf", "cabsf", "fmaxl", "ldexpf", "creal"];

/// The calls in a listing, each as the instruction and its operand with single spaces.
fn calls(text: &str, instruction: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| line.starts_with(&format!("{instruction} ")))
        .collect()
}

#[test]
fn each_macro_calls_the_function_for_its_arguments_type_on_apple_targets() {
    let dir = directory("apple", &[("math.h", MATH), ("complex.h", COMPLEX)]);
    let text = listing(&dir, "aarch64-apple-darwin", CALLS);
    let _ = std::fs::remove_dir_all(&dir);
    let calls = calls(&text, "bl");
    for want in WANTED {
        assert!(calls.contains(&format!("bl _{want}")), "no call to {want} in {calls:#?}");
    }
}

#[test]
fn each_macro_calls_the_function_for_its_arguments_type_on_windows() {
    let dir = directory("windows", &[("math.h", MATH), ("complex.h", COMPLEX)]);
    let text = listing(&dir, "x86_64-windows-gnu", CALLS);
    let _ = std::fs::remove_dir_all(&dir);
    let calls = calls(&text, "call");
    for want in WANTED {
        assert!(calls.contains(&format!("call {want}")), "no call to {want} in {calls:#?}");
    }
}

#[test]
fn elsewhere_the_librarys_own_header_is_the_one_included() {
    let dir = directory("linux", &[("tgmath.h", "#define LIBRARY_TGMATH 1\n")]);
    let text = listing(
        &dir,
        "x86_64-unknown-linux-gnu",
        "#include <tgmath.h>\nint which = LIBRARY_TGMATH;\n",
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(text.contains("which"), "{text}");
}
