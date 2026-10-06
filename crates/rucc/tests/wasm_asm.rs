//! The `-S` text of rucc for wasm goes back through rucc. `rucc -c` of the text writes the object
//! that `rucc -c` writes for the C source, byte for byte, and a line that the reader does not take
//! is an error at that line of the file.
//!
//! Design: #3141.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Data with addresses in it, a function pointer, a NaN with a payload, a `switch` with a table,
/// a local and a weak function, and an export.
const SOURCE: &str = r#"
static int counter = 3;
int table[4] = {1, 2, 0, 0};
const char *message = "hi\n";
extern int elsewhere;
int f(int x) { return x + counter + elsewhere; }
static int g(int x) { return x * 2; }
int (*pointer)(int) = g;
__attribute__((weak)) int w(void) { return 7; }
__attribute__((export_name("run"))) int run(int x) { return pointer(x) + table[1] + w(); }
float payload(void) { return __builtin_nanf("0x123"); }
int pick(int k) {
    switch (k) {
    case 0: return 4; case 1: return 9; case 2: return 1; case 3: return 6; case 4: return 2;
    case 5: return 8; case 6: return 3; case 7: return 5; default: return message[0];
    }
}
"#;

/// A directory of its own for this test, under the temporary directory.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-wasm-asm-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn rucc(args: &[&str], dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the compiler is built before its own tests run")
}

fn ok(out: &Output) {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn the_text_assembles_to_the_object_of_the_source() {
    let dir = scratch("same");
    std::fs::write(dir.join("t.c"), SOURCE).unwrap();
    for target in ["wasm32-wasip1", "wasm32-wasip3", "wasm32-none"] {
        for level in ["-O0", "-O2"] {
            let target = format!("--target={target}");
            ok(&rucc(&[&target, level, "-S", "t.c", "-o", "t.s"], &dir));
            ok(&rucc(&[&target, level, "-c", "t.c", "-o", "c.o"], &dir));
            ok(&rucc(&[&target, "-c", "t.s", "-o", "s.o"], &dir));
            let (from_c, from_s) = (dir.join("c.o"), dir.join("s.o"));
            let same = std::fs::read(from_c).unwrap() == std::fs::read(from_s).unwrap();
            assert!(same, "{target} {level}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_line_that_the_reader_does_not_take_is_an_error_at_that_line() {
    let dir = scratch("error");
    let text = "\t.functype\tf () -> ()\n\t.section\t.text.f,\"\",@\nf:\n\tbogus\n\tend_function\n";
    std::fs::write(dir.join("t.s"), text).unwrap();
    let out = rucc(&["--target=wasm32-wasip1", "-c", "t.s", "-o", "t.o"], &dir);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("t.s:4: error: the instruction `bogus`"), "{stderr}");
    std::fs::remove_dir_all(dir).unwrap();
}
