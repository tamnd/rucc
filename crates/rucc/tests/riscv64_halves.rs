//! `_Float16` and `__bf16` on RISC-V without the half extension.
//!
//! RV64GC has no half instructions, and gcc moves a half there as the low sixteen bits of a word
//! in a float register. It reads one with `lhu` and `fmv.w.x`, writes one with `fmv.x.w` and `sh`,
//! and widens and narrows one with a call to libgcc. gcc 15 also has `__bf16` there, done in
//! `float` the same way. rucc refused both before (tamnd/rucc#4276).

use std::path::PathBuf;
use std::process::Command;

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-riscv64-halves-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the temporary directory takes a directory");
    let path = dir.join("t.c");
    std::fs::write(&path, source).expect("the temporary directory takes a file");
    path
}

/// What the compiler writes for that source at `-O2` with those flags.
fn compile(what: &str, flags: &[&str], source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=riscv64-unknown-freebsd15.0", "-O2"])
        .args(flags)
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_half_is_added_in_float_through_libgcc() {
    let text = compile(
        "add",
        &["-S"],
        "_Float16 add(_Float16 a, _Float16 b) { return a + b; }\n\
         _Float16 load(_Float16 *p) { return p[3]; }\n\
         void store(_Float16 *p, float f) { p[2] = f; }\n",
    );
    assert!(text.contains("__extendhfsf2"), "{text}");
    assert!(text.contains("__truncsfhf2"), "{text}");
    assert!(text.contains("fadd.s"), "{text}");
    assert!(text.contains("lhu "), "{text}");
    assert!(text.contains("fmv.w.x "), "{text}");
    assert!(text.contains("fmv.x.w "), "{text}");
    assert!(text.contains("sh "), "{text}");
}

#[test]
fn a_half_with_no_register_left_is_the_word_of_its_slot() {
    let text = compile(
        "ninth",
        &["-S"],
        "_Float16 ninth(_Float16 a, _Float16 b, _Float16 c, _Float16 d, _Float16 e,\n\
                        _Float16 f, _Float16 g, _Float16 h, _Float16 i) { return i; }\n\
         _Float16 far(_Float16, _Float16, _Float16, _Float16, _Float16,\n\
                      _Float16, _Float16, _Float16, _Float16);\n\
         _Float16 call(_Float16 x) { return far(x, x, x, x, x, x, x, x, x); }\n",
    );
    assert!(text.contains("flw "), "{text}");
    assert!(text.contains("fsw "), "{text}");
}

#[test]
fn the_brain_float_is_a_type_from_gcc_15() {
    let source = "__bf16 add(__bf16 a, __bf16 b) { return a + b; }\n\
                  __bf16 one = 1.0bf16;\n\
                  int bits = __BFLT16_MANT_DIG__;\n";
    let text = compile("brain", &["-S", "-fgnuc-version=15"], source);
    assert!(text.contains("fadd.s"), "{text}");
    let macros = compile("macros", &["-E", "-dM", "-fgnuc-version=14"], "");
    assert!(!macros.contains("__BFLT16_"), "{macros}");
    let macros = compile("macros15", &["-E", "-dM", "-fgnuc-version=15"], "");
    assert!(macros.contains("#define __BFLT16_MANT_DIG__ 8"), "{macros}");
}
