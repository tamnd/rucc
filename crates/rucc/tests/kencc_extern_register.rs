//! kencc's `extern register`, end to end.
//!
//! Design: tamnd/rucc#4155, and 06.9 and 08.4 of the rucc-plan9 plan.
//!
//! The Plan 9 kernel keeps `m` and `up` in registers for the whole program. `6c` gives them `R15`
//! and `R14`, and `7c` gives them `R27` and `R26`, in the order of declaration. A read is a move
//! out of the register and a write is a move into it, and the address of one is an error.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("rucc-kencc-extern-register-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, the assembly it wrote and what it said.
fn compile(target: &str, what: &str, source: &str) -> (bool, String, String) {
    let path = fixture(&format!("{target}-{what}"), source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-fkencc", "-O2", "-S", "-o", "-"])
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

/// The instructions of one function, from its label to the first `ret`.
fn body<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    asm.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .take_while(|line| !line.trim_start().starts_with("ret"))
        .map(str::trim)
        .filter(|line| !line.starts_with('.'))
        .collect()
}

const KERNEL: &str = "typedef struct Mach Mach;\n\
                      typedef struct Proc Proc;\n\
                      struct Mach { int machno; Proc *proc; };\n\
                      struct Proc { int pid; };\n\
                      extern register Mach *m;\n\
                      extern register Proc *up;\n\
                      extern void sched(void);\n\
                      int machno(void) { return m->machno; }\n\
                      int pid(void) { return up->pid; }\n\
                      void ready(Proc *p) { up = p; }\n\
                      int moved(void) { int a = m->machno; sched(); return a + m->machno; }\n";

#[test]
fn the_variables_are_in_the_registers_kencc_gives_them() {
    for (target, first, second) in
        [("x86_64-plan9", "%r15", "%r14"), ("aarch64-plan9", "x27", "x26")]
    {
        let (ok, asm, err) = compile(target, "read", KERNEL);
        assert!(ok, "{target}: {err}");
        let machno = body(&asm, "machno");
        assert!(machno.iter().any(|line| line.contains(first)), "{target}: {machno:?}");
        let pid = body(&asm, "pid");
        assert!(pid.iter().any(|line| line.contains(second)), "{target}: {pid:?}");
    }
}

#[test]
fn a_write_is_a_move_into_the_register() {
    for (target, write) in [("x86_64-plan9", ", %r14"), ("aarch64-plan9", "mov x26, ")] {
        let (ok, asm, err) = compile(target, "write", KERNEL);
        assert!(ok, "{target}: {err}");
        let ready = body(&asm, "ready");
        assert!(ready.iter().any(|line| line.contains(write)), "{target}: {ready:?}");
    }
}

#[test]
fn a_read_after_a_call_reads_the_register_again() {
    // A process may move to another processor in `sched`, so `m` after the call is not `m`
    // before it.
    for (target, first) in [("x86_64-plan9", "%r15"), ("aarch64-plan9", "x27")] {
        let (ok, asm, err) = compile(target, "call", KERNEL);
        assert!(ok, "{target}: {err}");
        let moved = body(&asm, "moved");
        let call = moved.iter().position(|line| line.contains("sched")).expect("the call");
        assert!(moved[..call].iter().any(|line| line.contains(first)), "{target}: {moved:?}");
        assert!(moved[call..].iter().any(|line| line.contains(first)), "{target}: {moved:?}");
    }
}

#[test]
fn the_address_of_one_is_refused() {
    let source = "extern register int *a;\nint **f(void) { return &a; }\n";
    let (ok, _, err) = compile("x86_64-plan9", "address", source);
    assert!(!ok, "the address of an extern register variable was taken");
    assert!(err.contains("address of global register variable 'a' requested"), "{err}");
}

#[test]
fn a_third_one_and_one_in_a_function_are_refused() {
    let source = "extern register int *a;\nextern register int *b;\nextern register int *c;\n\
                  void f(void) { extern register int *d; }\n";
    let (ok, _, err) = compile("aarch64-plan9", "many", source);
    assert!(!ok, "a third extern register variable was taken");
    assert!(err.contains("'c' is one more"), "{err}");
    assert!(err.contains("'d' is declared 'extern register' inside a function"), "{err}");
}

#[test]
fn a_target_that_is_not_plan9_has_no_register_for_one() {
    let source = "extern register int *a;\nint f(void) { return *a; }\n";
    let (ok, _, err) = compile("x86_64-linux-gnu", "linux", source);
    assert!(!ok, "an extern register variable was taken on Linux");
    assert!(err.contains("no register for the 'extern register' 'a'"), "{err}");
}
