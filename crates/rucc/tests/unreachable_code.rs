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

/// A conditional expression whose condition folds builds only the arm it takes, so a call in the
/// other arm names nothing. busybox's `FETCH_LE32` puts a function nothing defines there so that a
/// field of the wrong size fails the link, and the struct and `void` forms go the same way.
#[test]
fn a_conditional_whose_condition_folds_does_not_name_the_other_arm() {
    let source = "extern unsigned never(void); extern void gone(void); extern int defined(void);\n\
                  struct s { int a; }; extern struct s lost(void); extern struct s kept;\n\
                  static unsigned only(void) { return never(); }\n\
                  unsigned f(unsigned x) { return sizeof(x) == 4 ? x : never(); }\n\
                  unsigned g(unsigned x) { return sizeof(x) != 4 ? only() : x + 1; }\n\
                  void h(void) { (defined() && 0) ? gone() : (void)defined(); }\n\
                  int k(void) { return (1 ? kept : lost()).a; }\n";
    for level in LEVELS {
        let asm = emit_of(level, "asm", "cond", source);
        for name in ["never", "only", "gone", "lost"] {
            assert!(!asm.contains(name), "'{name}' reached the assembler at {level}:\n{asm}");
        }
        assert!(asm.contains("defined"), "the call somebody makes went at {level}:\n{asm}");
        assert!(asm.contains("kept"), "the arm that runs went at {level}:\n{asm}");
    }
}

/// A `static` function whose one call the optimizer took away is not emitted, so what its body
/// names is not named either. The kernel's `load_vdso32` is this, after an `if` on
/// `IS_ENABLED(CONFIG_X86_64)` that returns, and its body names a `vdso32_image` that a sixty
/// four bit only kernel does not define. From `-O1`, where something takes the call away.
#[test]
fn a_static_function_whose_only_call_went_is_not_emitted() {
    let source = "extern int vdso32_image;\nint map(int *p, int x);\n\
                  static int load32(void) { return map(&vdso32_image, 0); }\n\
                  static int helper(void) { return load32(); }\n\
                  int setup(int n) {\n  if (sizeof(long) == 8) { return map(0, n); }\n\
                  return helper();\n}\n";
    for level in &LEVELS[1..] {
        let asm = emit_of(level, "asm", "static-gone", source);
        assert!(!asm.contains("vdso32_image"), "the dead body reached {level}:\n{asm}");
        assert!(!asm.contains("load32"), "{level}:\n{asm}");
        assert!(!asm.contains("helper"), "the caller of the caller stayed at {level}:\n{asm}");
        assert!(asm.contains("setup:"), "{level}:\n{asm}");
    }
}

/// And a `static` function marked `used` is kept however nothing calls it, which is how the
/// kernel's `asm-offsets.c` writes every offset the assembly reads.
#[test]
fn a_static_function_marked_used_stays_with_no_caller() {
    let source = "static void __attribute__((__used__)) common(void) {\n\
                  asm volatile(\"\\n.ascii \\\"->X %0\\\"\" : : \"i\"(5)); }\n\
                  int main(void) { return 0; }\n";
    for level in LEVELS {
        let asm = emit_of(level, "asm", "static-used", source);
        assert!(asm.contains("common:"), "{level}:\n{asm}");
        assert!(asm.contains("->X $5"), "{level}:\n{asm}");
    }
}
