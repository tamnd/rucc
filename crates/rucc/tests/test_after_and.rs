//! A branch on a byte an `and` has just worked out jumps on what the `and` left, with no test of
//! the byte in between.
//!
//! Issue 1994. `if (i1 >= 0 && i1 < var1ndigits)` in the digit loops of Postgres' `add_abs` and
//! `sub_abs` came out as two comparisons that keep their bytes, an `andb` of the two and then a
//! `testb` of what the `andb` wrote and a jump, twice for every digit. The `andb` sets the zero
//! flag the test would have set. `test_after_and.c` is those two loops. The unit tests in
//! `rucc-codegen` cover the pass. This runs the compiler, reads what it wrote and runs it.

use std::process::Command;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/test_after_and.c");

/// What the compiler says for the fixture with those arguments, and whether it finished.
fn compile(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .arg(FIXTURE)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), said)
}

/// Every test of a register against itself straight after an `and`, an `or` or an `xor` that
/// wrote that register, by the line of the test.
fn tested_again(asm: &str) -> Vec<String> {
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let mut found = Vec::new();
    for pair in lines.windows(2) {
        let (Some((op, args)), Some((test, tested))) =
            (pair[0].split_once('\t'), pair[1].split_once('\t'))
        else {
            continue;
        };
        let bitwise = ["and", "or", "xor"].iter().any(|front| {
            op.strip_prefix(front).is_some_and(|width| ["b", "w", "l", "q"].contains(&width))
        });
        let Some(written) = args.rsplit(", ").next() else { continue };
        if bitwise && test.starts_with("test") && tested == format!("{written}, {written}") {
            found.push(format!("{} {}", pair[0], pair[1]));
        }
    }
    found
}

#[test]
fn the_digit_loops_jump_on_what_the_and_left() {
    let (ok, asm, said) = compile(&["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"]);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    assert!(asm.contains("add_digits:") && asm.contains("sub_digits:"), "{asm}");
    let again = tested_again(&asm);
    assert!(again.is_empty(), "{again:?} in:\n{asm}");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn the_digit_loops_give_the_same_digits() {
    let dir = std::env::temp_dir().join(format!("rucc-test-after-and-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let prog = dir.join("prog");
    let prog = prog.to_str().expect("the temporary directory has a name that is text");
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let (ok, _, said) = compile(&[level, "-DRUN", "-o", prog]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(prog).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(got, "1 0 9999\n0 9999 9999 9999\n", "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
