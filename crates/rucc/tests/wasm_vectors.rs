//! How a GNU vector crosses a call on wasm32. Without `-msimd128` clang gives each lane a parameter
//! of its own and returns a vector of more than one lane through memory. With it, a vector is one
//! `v128` for each sixteen bytes, and a vector of more than sixteen bytes comes back through memory.
//! rucc must do the same for the two to call each other. A structure or a union that holds one
//! vector and nothing else goes the same way as the vector.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The output of rucc for `source` on stdin, with `-S` to stdout at `-O2` on wasm32-wasip1.
fn assembly(source: &str) -> Output {
    assembly_with(source, &[])
}

/// The same with more flags.
fn assembly_with(source: &str, flags: &[&str]) -> Output {
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

#[test]
fn each_lane_of_a_vector_is_a_parameter_of_its_own() {
    let out = assembly(
        "typedef char v4qi __attribute__((vector_size(4)));\n\
         typedef int v4si __attribute__((vector_size(16)));\n\
         typedef int v8si __attribute__((vector_size(32)));\n\
         typedef double v2df __attribute__((vector_size(16)));\n\
         typedef long long v1di __attribute__((vector_size(8)));\n\
         struct s { v4si v; };\n\
         union u { v4si v; };\n\
         v4qi f4qi(v4qi a) { return a; }\n\
         v4si f4si(v4si a) { return a; }\n\
         v8si f8si(v8si a) { return a; }\n\
         v2df f2df(v2df a) { return a; }\n\
         v1di f1di(v1di a) { return a; }\n\
         struct s fs(struct s a) { return a; }\n\
         union u fu(union u a) { return a; }\n",
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    // The lines that clang 23 writes for the same source.
    for line in [
        "\t.functype\tf4qi (i32, i32, i32, i32, i32) -> ()\n",
        "\t.functype\tf4si (i32, i32, i32, i32, i32) -> ()\n",
        "\t.functype\tf8si (i32, i32, i32, i32, i32, i32, i32, i32, i32) -> ()\n",
        "\t.functype\tf2df (i32, f64, f64) -> ()\n",
        "\t.functype\tf1di (i64) -> (i64)\n",
        "\t.functype\tfs (i32, i32, i32, i32, i32) -> ()\n",
        "\t.functype\tfu (i32, i32, i32, i32, i32) -> ()\n",
    ] {
        assert!(text.contains(line), "no {line:?} in\n{text}");
    }
}

#[test]
fn with_simd128_a_vector_is_one_v128_for_each_sixteen_bytes() {
    let out = assembly_with(
        "typedef char v2qi __attribute__((vector_size(2)));\n\
         typedef short v2hi __attribute__((vector_size(4)));\n\
         typedef int v4si __attribute__((vector_size(16)));\n\
         typedef int v8si __attribute__((vector_size(32)));\n\
         typedef long long v1di __attribute__((vector_size(8)));\n\
         struct s { v4si v; };\n\
         union u { v4si v; };\n\
         union two { v4si a, b; };\n\
         v2qi f2qi(v2qi a) { return a; }\n\
         v2hi f2hi(v2hi a) { return a; }\n\
         v4si f4si(int x, v4si a, double d) { return a; }\n\
         v8si f8si(v8si a) { return a; }\n\
         v1di f1di(v1di a) { return a; }\n\
         struct s fs(struct s a) { return a; }\n\
         union u fu(union u a) { return a; }\n\
         union two ftwo(union two a) { return a; }\n",
        &["-msimd128"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    // The lines that clang 23 writes for the same source with `-msimd128`.
    for line in [
        "\t.functype\tf2qi (v128) -> (v128)\n",
        "\t.functype\tf2hi (v128) -> (v128)\n",
        "\t.functype\tf4si (i32, v128, f64) -> (v128)\n",
        "\t.functype\tf8si (i32, v128, v128) -> ()\n",
        "\t.functype\tf1di (v128) -> (v128)\n",
        "\t.functype\tfs (v128) -> (v128)\n",
        "\t.functype\tfu (v128) -> (v128)\n",
        "\t.functype\tftwo (i32, i32) -> ()\n",
    ] {
        assert!(text.contains(line), "no {line:?} in\n{text}");
    }
}

#[test]
fn past_the_dots_the_bytes_of_a_vector_are_in_the_variadic_area() {
    let source = "typedef int v4si __attribute__((vector_size(16)));\n\
                  void take(int n, ...);\n\
                  void give(v4si v) { take(1, 5, v, 7); }\n";
    for flags in [&[][..], &["-msimd128"]] {
        let out = assembly_with(source, flags);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).unwrap();
        // The `5` at 0, the sixteen bytes at 16, which is the alignment of the vector, and the
        // `7` after them at 32, as clang puts them.
        for line in
            ["\ti32.store\t0\n", "\ti64.store\t16\n", "\ti64.store\t24\n", "\ti32.store\t32\n"]
        {
            assert!(text.contains(line), "{flags:?}: no {line:?} in\n{text}");
        }
    }
}

/// The callee of the run test, which reads a vector of 16 bytes and one of 32 past the `...`.
const TAKE: &str = "\
#include <stdarg.h>
typedef int v4si __attribute__((vector_size(16)));
typedef int v8si __attribute__((vector_size(32)));
int take(int n, ...) {
    va_list ap;
    va_start(ap, n);
    int a = va_arg(ap, int);
    v4si v = va_arg(ap, v4si);
    int b = va_arg(ap, int);
    v8si w = va_arg(ap, v8si);
    int c = va_arg(ap, int);
    va_end(ap);
    return a == 5 && v[0] == 1 && v[3] == 4 && b == 7 && w[0] == 10 && w[7] == 80 && c == 9 ? 0 : 1;
}
";

/// The caller of the run test.
const GIVE: &str = "\
typedef int v4si __attribute__((vector_size(16)));
typedef int v8si __attribute__((vector_size(32)));
int take(int n, ...);
int main(void) {
    v4si v = {1, 2, 3, 4};
    v8si w = {10, 20, 30, 40, 50, 60, 70, 80};
    return take(0, 5, v, 7, w, 9);
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
fn a_vector_past_the_dots_reaches_a_callee_from_rucc_or_clang() {
    let Some(sdk) = std::env::var_os("WASI_SDK_PATH").map(PathBuf::from) else {
        eprintln!("WASI_SDK_PATH is not set, so the program was not linked");
        return;
    };
    let Some(wasmtime) = on_path("wasmtime") else {
        eprintln!("there is no wasmtime on PATH, so the program was not run");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rucc-wasm-vectors-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("take.c"), TAKE).unwrap();
    std::fs::write(dir.join("give.c"), GIVE).unwrap();
    let rucc = PathBuf::from(env!("CARGO_BIN_EXE_rucc"));
    let clang = sdk.join("bin/clang");
    let sysroot = format!("--sysroot={}", sdk.join("share/wasi-sysroot").display());
    let target = "--target=wasm32-wasip1";
    for flags in [&[][..], &["-msimd128"]] {
        let compile = |out: &'static str, file: &'static str| {
            [&[target, "-O2", "-c", file, "-o", out], flags].concat()
        };
        run(&rucc, &compile("give.o", "give.c"), &dir);
        run(&rucc, &compile("take.o", "take.c"), &dir);
        let mut callees = vec!["take.o"];
        if clang.exists() {
            let mut args = compile("clang.o", "take.c");
            args.push(&sysroot);
            run(&clang, &args, &dir);
            callees.push("clang.o");
        }
        for callee in callees {
            run(&rucc, &[target, "give.o", callee, "-o", "t.wasm"], &dir);
            let status = Command::new(&wasmtime).arg(dir.join("t.wasm")).status().unwrap();
            assert!(status.success(), "{flags:?} with {callee}: {status}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
