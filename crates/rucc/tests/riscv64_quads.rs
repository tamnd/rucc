//! `long double` on RISC-V, which is a quad float that travels in integer registers.
//!
//! LP64D passes and returns a `long double` the way it passes an `__int128`: in a pair of general
//! purpose registers, and the arithmetic is a call into libgcc that takes and gives back the same
//! pairs. rucc refused every function that moved one before, because nothing lowered a quad that
//! was not computed with.

use std::path::PathBuf;
use std::process::Command;

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-riscv64-quads-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the temporary directory takes a directory");
    let path = dir.join("t.c");
    std::fs::write(&path, source).expect("the temporary directory takes a file");
    path
}

/// The assembly the compiler writes for that source at `-O2`.
fn compile(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=riscv64-unknown-netbsd11.1", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_long_double_is_added_through_libgcc_in_integer_registers() {
    let text = compile(
        "add",
        "long double add(long double a, long double b) { return a + b; }\n\
         long double g;\n\
         void store(long double x) { g = x; }\n\
         long double load(void) { return g; }\n\
         long double pick(int c, long double a, long double b) { return c ? a : b; }\n",
    );
    assert!(text.contains("__addtf3"), "{text}");
    assert!(!text.contains("fsd "), "{text}");
    assert!(!text.contains("fld "), "{text}");
}

#[test]
fn a_long_double_read_off_a_list_is_on_a_boundary_of_sixteen() {
    let text = compile(
        "va",
        "#include <stdarg.h>\n\
         long double va(int n, ...) {\n\
             va_list ap;\n\
             va_start(ap, n);\n\
             long double x = va_arg(ap, long double);\n\
             va_end(ap);\n\
             return x;\n\
         }\n",
    );
    assert!(text.contains("andi a0, a0, -16"), "{text}");
}
