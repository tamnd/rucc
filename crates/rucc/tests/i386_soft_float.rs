//! What the i386 kernel says about floats. `arch/x86/Makefile` passes `-msoft-float` and turns
//! every extension off with `-mno-sse`, `-mno-mmx` and the rest. The extensions are already off on
//! i386, so those change nothing. `-msoft-float` takes the x87 stack away, which is where every
//! float is on i386, so a function with no float in it is built as before and one with a float
//! in it is refused.

use std::process::Command;

const KERNEL: &[&str] = &[
    "--target=i686-unknown-linux-gnu",
    "-fno-pic",
    "-msoft-float",
    "-mregparm=3",
    "-mno-sse",
    "-mno-mmx",
    "-mno-sse2",
    "-mno-3dnow",
    "-mno-avx",
    "-c",
];

fn build(name: &str, source: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("rucc-i386-soft-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(KERNEL)
        .arg("-o")
        .arg(dir.join("one.o"))
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_function_with_no_float_is_built() {
    let out = build("int", "int f(int x) { return x * 3 + 1; }\n");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_function_with_a_float_is_refused() {
    for (name, source) in [
        ("ret", "float f(float x) { return x * 2; }\n"),
        ("local", "int f(int x) { double d = x; return (int)(d / 3); }\n"),
    ] {
        let out = build(name, source);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{name}");
        assert!(
            said.contains("x87 registers, which the command line turned off"),
            "{name}: {said}"
        );
    }
}
