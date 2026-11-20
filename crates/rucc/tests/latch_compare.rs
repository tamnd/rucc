//! A loop that counts and walks a pointer ends in a comparison and a jump on what it found.
//!
//! Issue 1994. The step of the walked pointer came out between the comparison that ends the loop
//! and the branch on it, so the bottom of every turn was `cmpl`, `setl`, `addq $16`, `testb` and
//! `jne`. Postgres' tuple deforming loop had three of those, one for each copy of the loop the
//! inlining makes, and `latch_compare.c` is that loop cut down from REL_18_6. The unit tests in
//! `rucc-codegen` cover the pass. This runs the compiler and reads the loops it wrote.

use std::process::Command;

/// The assembly the compiler writes for the fixture for x86-64 with those flags.
fn assembly(flags: &[&str]) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/latch_compare.c");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(["-S", "-o", "-", "-O2", "-fno-strict-aliasing"])
        .args(flags)
        .arg(path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// Every byte a comparison kept and a test reads again for a jump, with something else written
/// between the two in the same block, by the line that kept it: a `set` into a register, one or
/// two instructions that are not a label, then a `testb` of that register and a jump.
///
/// That is the shape of a comparison the layout could not fold into its branch because something
/// came between them. A byte that is kept across a label, like `hasnulls` above the loop, or one
/// tested straight after the `set` because something else reads it too, is not.
fn tested_again(asm: &str) -> Vec<&str> {
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let mut found = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let mut words = line.split_whitespace();
        let (Some(op), Some(reg)) = (words.next(), words.next()) else { continue };
        if !op.starts_with("set") {
            continue;
        }
        let test = format!("testb\t{reg}, {reg}");
        for after in at + 2..lines.len().min(at + 4) {
            if lines[at + 1..after].iter().any(|between| between.ends_with(':')) {
                break;
            }
            let jumps = lines.get(after + 1).is_some_and(|jump| jump.starts_with('j'));
            if lines[after] == test && jumps {
                found.push(*line);
                break;
            }
        }
    }
    found
}

#[test]
fn the_deforming_loops_end_in_a_comparison_and_a_jump() {
    for flags in [&[][..], &["-fwrapv"][..]] {
        let asm = assembly(flags);
        assert!(asm.contains("$16"), "the fixture no longer walks the attributes:\n{asm}");
        let bytes = tested_again(&asm);
        assert!(bytes.is_empty(), "{bytes:?} with {flags:?} in:\n{asm}");
    }
}
