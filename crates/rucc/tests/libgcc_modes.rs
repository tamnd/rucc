//! The two machine modes libgcc names for itself, `__libgcc_cmp_return__` and
//! `__libgcc_shift_count__`, at the width gcc gives them on each target.
//!
//! Both are the word, except that AArch64 makes a comparison result an `SI`. compiler-rt's
//! `fp_compare_impl.inc` asserts that size, so a compiler that makes it the word stops FreeBSD's
//! world. The widths are the ones the gcc 13 cross compilers
//! print.

use std::process::Command;

fn widths(target: &str, cmp: usize, shift: usize) {
    let dir =
        std::env::temp_dir().join(format!("rucc-libgcc-modes-{}-{target}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let source = format!(
        "typedef int cmp __attribute__((__mode__(__libgcc_cmp_return__)));\n\
         typedef int shift __attribute__((__mode__(__libgcc_shift_count__)));\n\
         _Static_assert(sizeof(cmp) == {cmp}, \"cmp\");\n\
         _Static_assert(sizeof(shift) == {shift}, \"shift\");\n"
    );
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-fsyntax-only", "a.c"])
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.is_empty(), "{target}: {err}");
}

#[test]
fn a_comparison_result_is_an_int_on_aarch64() {
    widths("aarch64-unknown-linux-gnu", 4, 8);
    widths("aarch64-unknown-freebsd15.1", 4, 8);
    widths("aarch64-unknown-netbsd", 4, 8);
}

#[test]
fn both_are_the_word_elsewhere() {
    widths("x86_64-unknown-linux-gnu", 8, 8);
    widths("i686-unknown-linux-gnu", 4, 4);
    widths("riscv64-unknown-linux-gnu", 8, 8);
    widths("riscv64-unknown-freebsd15.1", 8, 8);
}
