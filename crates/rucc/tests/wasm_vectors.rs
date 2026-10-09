//! How a GNU vector crosses a call on wasm32 without `-msimd128`. clang gives each lane a parameter
//! of its own and returns a vector of more than one lane through memory, and rucc must do the same
//! for the two to call each other. A structure that holds one vector and nothing else goes the same
//! way as the vector.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

/// The output of rucc for `source` on stdin, with `-S` to stdout at `-O2` on wasm32-wasip1.
fn assembly(source: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=wasm32-wasip1", "-O2", "-x", "c", "-", "-S", "-o", "-"])
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
         v4qi f4qi(v4qi a) { return a; }\n\
         v4si f4si(v4si a) { return a; }\n\
         v8si f8si(v8si a) { return a; }\n\
         v2df f2df(v2df a) { return a; }\n\
         v1di f1di(v1di a) { return a; }\n\
         struct s fs(struct s a) { return a; }\n",
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
    ] {
        assert!(text.contains(line), "no {line:?} in\n{text}");
    }
}
