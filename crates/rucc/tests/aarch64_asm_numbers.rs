//! A number handed to an `asm` statement on AArch64 and spelled into its text, as gcc spells it.
//!
//! `rZ` is a register or the zero register, and a zero under it is `xzr` or `wzr`, which is how
//! the kernel's `smp_store_release` stores a null pointer. A number under `%w` or `%x` is the
//! number, and a zero under either is the zero register at that width, which is how the kernel's
//! atomics write `add w0, w0, 1` against `"Ir"`. A local register variable may be named `r0` to
//! `r30` as well as `x0` to `x30`, as the kernel's SMC calls name theirs.

use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "--target=aarch64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-a64-numbers-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// The `-O2` listing of a source that has to compile without a word.
fn listing(what: &str, source: &str) -> String {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-S", "-O2", "-o", "-", "a.c"])
        .current_dir(Path::new(&dir))
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.is_empty(), "{err}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn a_zero_under_rz_is_the_zero_register() {
    let text = listing(
        "rz",
        "void word(int *p) { asm volatile(\"stlr %w1, %0\" : \"=Q\"(*p) : \"rZ\"(0) : \"memory\"); }
void null(void **p) { asm volatile(\"stlr %x1, %0\" : \"=Q\"(*p) : \"rZ\"((void *)0) : \"memory\"); }
void bare(long *p) { asm volatile(\"str %1, %0\" : \"=Q\"(*p) : \"rZ\"(0L) : \"memory\"); }
void some(int *p) { asm volatile(\"stlr %w1, %0\" : \"=Q\"(*p) : \"rZ\"(5) : \"memory\"); }
",
    );
    assert_eq!(body(&text, "word"), ["stlr wzr, [x0]", "ret"], "{text}");
    assert_eq!(body(&text, "null"), ["stlr xzr, [x0]", "ret"], "{text}");
    assert_eq!(body(&text, "bare"), ["str xzr, [x0]", "ret"], "{text}");
    assert_eq!(body(&text, "some"), ["mov w1, #5", "stlr w1, [x0]", "ret"], "{text}");
}

#[test]
fn a_number_under_a_width_is_the_number_and_a_zero_is_the_zero_register() {
    let text = listing(
        "width",
        "static inline __attribute__((always_inline)) void add(int i, int *v) {
  unsigned long tmp; int r;
  asm volatile(\"1: ldxr %w0, %2\\n add %w0, %w0, %w3\\n stxr %w1, %w0, %2\\n cbnz %w1, 1b\"
               : \"=&r\"(r), \"=&r\"(tmp), \"+Q\"(*v) : \"Ir\"(i));
}
void one(int *v) { add(1, v); }
void none(int *v) { add(0, v); }
long wide(long a) { long r; asm(\"orr %x0, %x1, %x2\" : \"=r\"(r) : \"r\"(a), \"Lr\"(0xff00L)); return r; }
",
    );
    assert!(body(&text, "one").iter().any(|line| line == "add w1, w1, 1"), "{text}");
    assert!(body(&text, "none").iter().any(|line| line == "add w1, w1, wzr"), "{text}");
    assert_eq!(body(&text, "wide"), ["orr x0, x0, 65280", "ret"], "{text}");
}

#[test]
fn a_register_variable_may_be_named_from_r0() {
    let text = listing(
        "smccc",
        "unsigned long call(unsigned long fn) {
  register unsigned long r0 asm(\"r0\") = fn;
  register unsigned long r1 asm(\"r1\") = 7;
  asm volatile(\"hvc #0\" : \"+r\"(r0) : \"r\"(r1) : \"memory\");
  return r0;
}
",
    );
    let call = body(&text, "call");
    assert!(call.iter().any(|line| line == "mov x1, #7"), "{text}");
    assert_eq!(call[call.len() - 2..], ["hvc #0", "ret"], "{text}");
}
