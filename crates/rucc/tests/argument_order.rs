//! The order in which the arguments of a call are evaluated, which is gcc's on each target.
//!
//! C leaves the order unspecified, and gcc takes it from the target: on i386 and x86-64 its
//! gimplifier goes from the last argument to the first, and on AArch64 and RISC-V from the first
//! to the last. A program can see the order when two arguments call functions that change the
//! same state. NetBSD's flex calls `mkor (mkstate (c), mkstate (reverse_case (c)))` for a letter
//! under `-i`, so a flex built by rucc in the other order wrote other tables for the Ada lexer of
//! gdb, and the gdb of an R-host build of NetBSD was not the gdb of the G-host build
//! (tamnd/rucc-bsd#13).

use std::process::Command;

const SOURCE: &str = "int first(void);\nint second(void);\nint take(int, int);\n\
                      int f(void) { return take(first(), second()); }\n";

/// The names `first` and `second` in the order the calls in the assembly of `f` make them.
fn calls(target: &str, level: &str) -> Vec<&'static str> {
    let dir = std::env::temp_dir()
        .join(format!("rucc-argument-order-{}-{target}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("order.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args([level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{target} {level}: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("the assembly is text");
    let mut names = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(op), Some(callee)) = (words.next(), words.next()) else { continue };
        if !matches!(op, "call" | "calll" | "callq" | "bl" | "jal") {
            continue;
        }
        let callee = callee.split('@').next().unwrap_or(callee);
        for name in ["first", "second"] {
            if callee == name {
                names.push(name);
            }
        }
    }
    names
}

#[test]
fn x86_evaluates_the_last_argument_first() {
    for target in ["x86_64-unknown-linux-gnu", "i686-unknown-linux-gnu", "x86_64-unknown-netbsd"] {
        for level in ["-O0", "-O2"] {
            assert_eq!(calls(target, level), ["second", "first"], "{target} {level}");
        }
    }
}

#[test]
fn aarch64_and_riscv64_evaluate_the_first_argument_first() {
    for target in ["aarch64-unknown-linux-gnu", "riscv64-unknown-linux-gnu"] {
        for level in ["-O0", "-O2"] {
            assert_eq!(calls(target, level), ["first", "second"], "{target} {level}");
        }
    }
}
