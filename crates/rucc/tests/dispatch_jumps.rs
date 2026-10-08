//! A computed `goto` to a step found by number goes to that step.
//!
//! Issue 1994. Postgres' `ExecInterpExpr` jumps with `op = &state->steps[op->d.jump.jumpdone]`,
//! an `add` that writes its answer over `steps`. Once the step a handler works out was written
//! straight into the register the next handler reads it in, that `add` was too, and the number it
//! added could be in that register, since `op` is read for the last time when the number is read
//! off it. The copy of `steps` in front of the `add` went over the number and the jump went to
//! `steps` plus `steps`, which crashed the server on the first query that took such a jump.
//! `dispatch_jumps.c` has that shape. The unit tests in `rucc-codegen` cover the pass. This runs
//! the compiler and the program it wrote.

use std::process::Command;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/dispatch_jumps.c");

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn the_jumps_go_to_the_steps_they_name() {
    let dir = std::env::temp_dir().join(format!("rucc-dispatch-jumps-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let prog = dir.join("prog");
    let prog = prog.to_str().expect("the temporary directory has a name that is text");
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([level, "-DRUN", "-o", prog, FIXTURE])
            .output()
            .expect("the compiler is built before its own tests run");
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{level}: {said}");
        let out = Command::new(prog).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(got, "1500500 1000\n", "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_fixture_compiles_for_x86_64() {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-", FIXTURE])
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{said}");
    let asm = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(asm.contains("jmp\t*"), "the fixture no longer has a computed goto:\n{asm}");
}
