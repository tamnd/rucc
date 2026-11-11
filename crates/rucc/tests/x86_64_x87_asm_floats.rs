//! A `float` or a `double` that an `asm` statement on x86-64 puts on the x87 stack with `t` or
//! `u`. The value is in an xmm register, so it crosses to the stack through memory before the
//! template and back after it. compiler-rt's `i386/fp_mode.c`, which FreeBSD builds for amd64 too,
//! raises the inexact exception with `fdivs %1` on a `float` written `+t`.

use std::process::Command;

const SOURCE: &str = "\
float g = 3.0f;
float inexact(float f) { __asm__ __volatile__(\"fdivs %1\" : \"+t\" (f) : \"m\" (g)); return f; }
double root(double x) { double r; __asm__(\"fsqrt\" : \"=t\" (r) : \"0\" (x)); return r; }
double scale(double x, double y) {
    double r; __asm__(\"fscale\" : \"=t\" (r) : \"0\" (x), \"u\" (y)); return r; }
";

fn build(target: &str, level: &str) -> std::process::Output {
    let dir = std::env::temp_dir()
        .join(format!("rucc-x86-64-x87-asm-{}{target}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_float_in_an_xmm_register_crosses_to_the_x87_stack_and_back() {
    for target in ["x86_64-unknown-freebsd15.1", "x86_64-unknown-linux-gnu"] {
        for level in ["-O0", "-O2"] {
            let out = build(target, level);
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success(),
                "{target} {level}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            for line in ["fdivs", "fsqrt", "fscale", "flds", "fstps", "fldl", "fstpl"] {
                assert!(text.contains(line), "{target} {level}: no {line}:\n{text}");
            }
        }
    }
}
