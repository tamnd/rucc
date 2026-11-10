//! kencc's rules for two declarations of one name, under `-fkencc`, end to end.
//!
//! - Without `-V`, a pointer to `void` is the same type as any other pointer. APE's
//!   `sys/socket.h` declares `getpeername(int, void *, int *)`, and the definition takes a
//!   `struct sockaddr *`. With `-V`, which is `-fkencc-strict-void`, the rule holds only outside
//!   a function type.
//! - A declaration at file scope after an `extern` in a block with another type is a new
//!   declaration. pc.y has `extern void yyparse(void);` in main, and yaccpar defines
//!   `int yyparse(void)`.

use std::process::Command;

const PARAMETERS: &str = "struct sockaddr { int x; };\n\
                          extern int getpeername(int, void *, int *);\n\
                          int getpeername(int fd, struct sockaddr *a, int *l) { return a->x + *l + fd; }\n";

const OBJECTS: &str = "struct S { int x; };\n\
                       extern void *p;\n\
                       struct S *p;\n\
                       extern void **q;\n\
                       struct S **q;\n";

const BLOCK: &str = "void use(void) { extern void yyparse(void); yyparse(); }\n\
                     int yyparse(void) { return 3; }\n";

/// Whether the compiler took `source` with `flags`, and what it said.
fn compile(what: &str, source: &str, flags: &[&str]) -> (bool, String) {
    let dir =
        std::env::temp_dir().join(format!("rucc-kencc-declarations-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let run = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-plan9", "-O2", "-w", "-S", "-o"])
        .arg(dir.join("one.s"))
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (run.status.success(), String::from_utf8_lossy(&run.stderr).into_owned())
}

#[test]
fn a_void_pointer_parameter_agrees_with_any_pointer() {
    let (ok, err) = compile("lax", PARAMETERS, &["-fkencc"]);
    assert!(ok, "{err}");
    let (ok, err) = compile("strict", PARAMETERS, &["-fkencc", "-fkencc-strict-void"]);
    assert!(!ok);
    assert!(err.contains("conflicting types for 'getpeername'"), "{err}");
    let (ok, _) = compile("c", PARAMETERS, &[]);
    assert!(!ok);
}

#[test]
fn a_void_pointer_object_agrees_with_any_pointer_also_with_v() {
    let (ok, err) = compile("objects", OBJECTS, &["-fkencc", "-fkencc-strict-void"]);
    assert!(ok, "{err}");
}

#[test]
fn an_extern_in_a_block_does_not_bind_a_later_definition() {
    let (ok, err) = compile("block", BLOCK, &["-fkencc"]);
    assert!(ok, "{err}");
    let (ok, err) = compile("block-c", BLOCK, &[]);
    assert!(!ok);
    assert!(err.contains("conflicting types for 'yyparse'"), "{err}");
}
