//! The enumerators and the tags that the parameter list of a function definition declares, end
//! to end.
//!
//! C 6.2.1p4 puts them in the block scope of the body, with the parameters. gcc warns that such a
//! type is not visible outside the function, and it lets the body use it. 9front's `devvmx.c` has
//! `enum { TRAPRD = 1, TRAPWR = 2 } state` as a parameter of `vmxtrapmsr` and tests
//! `state & TRAPRD` in the body.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A directory of this test's own, empty.
fn dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-parameter-types-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// Whether the compile of `source` for `target` passed, the assembly and the messages.
fn compile(target: &str, source: &str) -> (bool, String, String) {
    let dir = dir();
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-fno-asynchronous-unwind-tables", "-S", "-o", "-", "a.c"])
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_body_uses_an_enumerator_of_the_parameter_list() {
    let source = "int f(enum { RD = 1, WR = 2 } s)\n\
                  {\n\
                  \tif((s & RD) != 0)\n\
                  \t\treturn 40;\n\
                  \treturn 50 + WR;\n\
                  }\n";
    for target in ["x86_64-unknown-linux-gnu", "x86_64-unknown-plan9"] {
        let (ok, asm, err) = compile(target, source);
        assert!(ok, "{target}: {err}");
        assert!(
            asm.contains("40") && asm.contains("52"),
            "{target}: the constants are not there:\n{asm}"
        );
    }
}

#[test]
fn the_body_uses_a_tag_of_the_parameter_list() {
    let source = "int g(struct P { int a, b; } *p)\n\
                  {\n\
                  \tstruct P q = *p;\n\
                  \treturn q.b + sizeof(struct P);\n\
                  }\n\
                  int h(enum E { A = 3 } e)\n\
                  {\n\
                  \tenum E x = A;\n\
                  \treturn x + e;\n\
                  }\n";
    let (ok, _, err) = compile("x86_64-unknown-linux-gnu", source);
    assert!(ok, "{err}");
}

#[test]
fn a_prototype_alone_still_keeps_its_enumerators_to_itself() {
    let source = "int f(enum { RD = 1 } s);\n\
                  int z(void)\n\
                  {\n\
                  \treturn RD;\n\
                  }\n";
    let (ok, _, err) = compile("x86_64-unknown-linux-gnu", source);
    assert!(!ok, "RD is visible after the prototype");
    assert!(err.contains("'RD' undeclared"), "{err}");
}
