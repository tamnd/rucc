//! GNU's nested functions, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.3.
//!
//! A test of the whole compiler rather than of one crate, because a nested function is three
//! crates agreeing: the checking gives the definition no linkage, the lowering builds the blocks
//! and passes the chain, and the backend puts the chain in a register the trampoline libgcc makes
//! agrees with. Each of those can be right on its own and the program still call the function
//! with the wrong chain.

use std::path::PathBuf;
use std::process::Command;

/// A nested function that reaches a parameter and a local of the function around it, called by
/// name and through its address.
const NESTED: &str = "\
int apply(int (*)(int), int);
int outer(int n) {
  int total = 0;
  int add(int k) { total += k * n; return total; }
  add(1);
  return apply(add, 2);
}
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-nested-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler wrote and what it said, for that source under those flags.
fn run(what: &str, target: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The assembly for x86-64 has the stub the trampoline goes to, the two calls into libgcc, and the
/// chain in `rax` on the direct call.
///
/// The stub is what moves the chain from `r10`, where gcc's trampoline leaves it, to `rax`, where
/// this compiler passes it, so a file without it is one whose nested functions work when called by
/// name and crash when called through a pointer.
#[test]
fn x86_64_passes_the_chain_in_rax_and_points_the_trampoline_at_a_stub() {
    let (ok, text, err) = run("x86", "x86_64-unknown-linux-gnu", &[], NESTED);
    assert!(ok, "the compiler refused the fixture:\n{err}");
    assert!(text.contains("movq %r10, %rax"), "the stub is missing:\n{text}");
    assert!(text.contains(".chain"), "the stub has no name:\n{text}");
    assert!(text.contains("__gcc_nested_func_ptr_created"), "no trampoline is made:\n{text}");
    assert!(text.contains("__gcc_nested_func_ptr_deleted"), "the trampoline is kept:\n{text}");
}

/// AArch64 Linux takes the chain in `x18`, which is where gcc's trampoline leaves it, so there is
/// no stub.
#[test]
fn aarch64_linux_passes_the_chain_in_x18_and_needs_no_stub() {
    let (ok, text, err) = run("a64", "aarch64-unknown-linux-gnu", &[], NESTED);
    assert!(ok, "the compiler refused the fixture:\n{err}");
    assert!(text.contains("x18"), "the chain is not in x18:\n{text}");
    assert!(!text.contains(".chain"), "a stub where none is needed:\n{text}");
    assert!(text.contains("__gcc_nested_func_ptr_created"), "no trampoline is made:\n{text}");
}

/// The targets with no register for the chain, or no libgcc to make the trampoline, say so rather
/// than build something that calls the function with garbage for a chain.
#[test]
fn a_target_with_nowhere_to_put_the_chain_refuses_with_a_reason() {
    for (target, flags) in [
        ("aarch64-apple-darwin", &[][..]),
        ("x86_64-pc-windows-msvc", &[][..]),
        ("i686-unknown-linux-gnu", &[][..]),
        ("aarch64-unknown-linux-gnu", &["-ffixed-x18"][..]),
    ] {
        let (ok, _, err) = run("refused", target, flags, NESTED);
        assert!(!ok, "{target} {flags:?} built a nested function");
        assert!(
            err.contains("a nested function cannot be built for this target"),
            "{target} {flags:?}: {err}"
        );
    }
}

/// A function with no nested function in it is built the way it always was, with no block and no
/// calls into libgcc, which is what keeps the feature from costing anything where it is not used.
#[test]
fn a_function_with_no_nested_function_is_untouched() {
    let (ok, text, err) =
        run("plain", "x86_64-unknown-linux-gnu", &[], "int f(int n) { return n + 1; }\n");
    assert!(ok, "the compiler refused the fixture:\n{err}");
    assert!(!text.contains("__gcc_nested_func_ptr"), "{text}");
}
