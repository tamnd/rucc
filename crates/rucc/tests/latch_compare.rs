//! A loop that counts and walks a pointer ends in a comparison and a jump on what it found.
//!
//! Issue 1994. The step of the walked pointer came out between the comparison that ends the loop
//! and the branch on it, so the bottom of every turn was `cmpl`, `setl`, `addq $16`, `testb` and
//! `jne`. Postgres' tuple deforming loop had three of those. The unit tests in `rucc-codegen` cover
//! the pass. This runs the compiler and reads the loop it wrote.

use std::path::PathBuf;
use std::process::Command;

/// A loop over an array of sixteen byte records that writes two arrays indexed by the count, the
/// shape of the loop in Postgres that fills a tuple's values and nulls.
const SOURCE: &str = "\
struct att { int len; short off; char align; char byval; long pad; };
void fill(const struct att *a, char *isnull, long *values, int n, const char *tp) {
    for (int i = 0; i < n; i++, a++) {
        isnull[i] = 0;
        values[i] = (long)(tp + a->off);
    }
}
";

/// The assembly the compiler writes for the fixture for x86-64 with those flags.
fn assembly(flags: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rucc-latch-compare-{}-{}",
        std::process::id(),
        flags.join("")
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("latch.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(["-S", "-o", "-", "-O2"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// Every line that keeps the answer to a comparison in a byte.
fn kept(asm: &str) -> Vec<&str> {
    asm.lines()
        .filter(|line| line.split_whitespace().next().is_some_and(|word| word.starts_with("set")))
        .collect()
}

#[test]
fn the_loop_ends_in_a_comparison_and_a_jump() {
    for flags in [&[][..], &["-fwrapv"][..]] {
        let asm = assembly(flags);
        assert!(asm.contains("$16"), "the fixture no longer walks the pointer:\n{asm}");
        let bytes = kept(&asm);
        assert!(bytes.is_empty(), "{bytes:?} with {flags:?} in:\n{asm}");
    }
}
