//! A branch inside a handler of a computed `goto` is threaded.
//!
//! Issue 1994. Every block a computed `goto` dispatches to is in one irreducible region, and jump
//! threading used to refuse every edge there. So an `||` in a handler was worked out as a truth
//! value and tested again, on every step of the interpreter. The unit tests in `rucc-opt` cover the
//! rule. This one runs the compiler on a small interpreter and reads what the pass reports.

use std::path::PathBuf;
use std::process::Command;

/// Three handlers, so the dispatch has more than one way into the cycle, and an `||` in one.
const SOURCE: &str = "\
typedef struct step { void *op; _Bool *isnull; long *value; int jump; } step;
long run(step *op, step *steps, _Bool *resnull) {
    static void *const ops[] = { &&done, &&qual, &&next };
    goto *op->op;
qual:
    if (*op->isnull || !*op->value) {
        *resnull = 0;
        op = &steps[op->jump];
        goto *op->op;
    }
    op++;
    goto *op->op;
next:
    op++;
    goto *op->op;
done:
    return (long)ops;
}
";

/// What the compiler says about the passes it ran on the fixture, at `-O2` on x86-64.
fn report() -> String {
    let dir = std::env::temp_dir().join(format!("rucc-dispatch-threading-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("dispatch.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(["-S", "-o", "-", "-O2", "-fopt-info-all"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(&dir);
    said
}

#[test]
fn the_or_in_a_handler_is_threaded() {
    let said = report();
    let threaded = said.lines().any(|line| {
        line.contains("optimized: edge pointed straight at") && line.ends_with("[thread]")
    });
    assert!(threaded, "nothing was threaded:\n{said}");
    assert!(!said.contains("would give a loop a second way in"), "an edge was refused:\n{said}");
}
