//! A call in a branch a constant condition never takes does not reach the object file.
//!
//! Issue 359, which is `execute/medce-1.c` out of the gcc torture suite. The unit tests in
//! `rucc-opt` cover what the pass does to a function. What is left is the thing the issue is
//! actually about, which is whether the name of a function nobody calls is in the output, so
//! these run the compiler and read what came out.
//!
//! The file is written the way it is on purpose. `case 1:` is a label inside the body of the
//! dead `if`, so control does reach `bar` and never reaches `link_error`. A compiler that
//! deletes the whole compound statement gets this as wrong as one that keeps all of it, which
//! is why both halves are asserted at every level.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the assertions are about the compiler and
/// not about the machine the suite ran on.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The reduced form of `execute/medce-1.c`.
const SOURCE: &str = "\
extern void link_error(void);
extern void bar(void);

void foo(int x) {
    switch (x) {
    case 0:
        if (0) { link_error(); case 1: bar(); }
    }
}
";

/// The fixture, in a directory of its own so two of these at once do not write one file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-medce-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("medce.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler emits for the fixture at this level, in the form asked for.
fn emit(level: &str, what: &str) -> String {
    emit_of(level, what, "medce", SOURCE)
}

/// The same for a source of the test's own.
fn emit_of(level: &str, what: &str, name: &str, source: &str) -> String {
    let path = fixture(&format!("{name}{}{}", level.trim_start_matches('-'), what), source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args([&format!("--emit={what}"), "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture at {level}:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// Every level, because this is a link error at all of them and not a missed optimization at
/// some of them.
const LEVELS: &[&str] = &["-O0", "-O1", "-O2", "-O3", "-Os", "-Oz"];

#[test]
fn the_call_the_program_never_makes_is_not_in_the_ir() {
    for level in LEVELS {
        let ir = emit(level, "ir");
        assert!(!ir.contains("call @link_error"), "the dead call survived {level}:\n{ir}");
        assert!(ir.contains("call @bar"), "the live call went with it at {level}:\n{ir}");
    }
}

#[test]
fn the_name_it_never_calls_is_not_in_the_assembly_either() {
    // Which is the form the linker sees. A declaration left in the IR costs nothing, a
    // relocation against it is the bug.
    for level in LEVELS {
        let asm = emit(level, "asm");
        assert!(!asm.contains("link_error"), "the dead call reached the assembler at {level}");
        assert!(asm.contains("bar"), "the live call did not reach the assembler at {level}");
    }
}

/// `f() && 0 && g()` is `(f() && 0) && g()`, and the left of the outer `&&` is false whatever `f`
/// returns. gcc never calls `g` and never names it, at `-O0` as well. tcc's `optimize_out_test`
/// is written that way about a function nothing defines.
#[test]
fn a_chain_its_left_side_already_ended_does_not_name_its_right_side() {
    let source = "extern int defined(void); extern int never(void);\n\
                  int f(void) { int i = defined() && 0 && never();\n\
                  return i + (defined() || 1 || never()); }\n";
    for level in LEVELS {
        let asm = emit_of(level, "asm", "chain", source);
        assert!(!asm.contains("never"), "the call nobody makes reached {level}:\n{asm}");
        assert!(asm.contains("defined"), "the call somebody makes went at {level}:\n{asm}");
    }
}

/// A `static inline` called only from an arm that is never lowered is not emitted, or its body
/// names a function nothing defines. `refer_to_undefined` in tcc's test is that.
#[test]
fn a_function_called_only_from_an_arm_never_taken_is_not_emitted() {
    let source = "extern int defined(void); extern void never(void);\n\
                  static inline void refer(void) { never(); }\n\
                  void f(void) { if (defined() && 0) refer(); }\n";
    for level in LEVELS {
        let asm = emit_of(level, "asm", "refer", source);
        assert!(!asm.contains("refer"), "the function nobody calls was emitted at {level}:\n{asm}");
        assert!(!asm.contains("never"), "its call reached {level}:\n{asm}");
    }
}

/// Unless a label is in the arm, since a `goto` from outside reaches it and the call after it is
/// a call the program makes.
#[test]
fn a_function_called_after_a_label_in_an_arm_never_taken_is_emitted() {
    let source = "extern void done(void);\n\
                  static void kept(void) { done(); }\n\
                  void f(int x) { if (x) goto in; if (0) { in: kept(); } }\n";
    for level in &LEVELS[..1] {
        let asm = emit_of(level, "asm", "label", source);
        assert!(
            asm.contains("kept:"),
            "the function the goto reaches is missing at {level}:\n{asm}"
        );
    }
}
