//! The calling convention of kencc on amd64 Plan 9, end to end.
//!
//! Design: tamnd/rucc#4154, and 08.2 of the rucc-plan9 plan.
//!
//! An object from rucc must call and be called by objects from `6c`. Each argument has an 8-byte
//! slot in the argument area, and an integer or a pointer first also goes in `rbp`, with its slot
//! left empty by the caller. A parameter whose address is taken lives in its slot, because the
//! `va_start` of the Plan 9 `u.h` steps from `&start` to the next slot. No function keeps a frame
//! pointer, because `rbp` holds the first argument and no register survives a call.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-kencc-convention-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, the assembly it wrote and what it said.
fn compile(what: &str, level: &str, source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-plan9", "-fkencc", level, "-S", "-o", "-"])
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

const SOURCE: &str = "extern int h(int, int, int);\n\
                      int c(void) { return h(1, 2, 3); }\n\
                      typedef char *va_list;\n\
                      int vf(char *fmt, ...) { va_list ap = (char *)(&fmt + 1); \
                      return *(int *)ap + fmt[0]; }\n\
                      extern void use(int *);\n\
                      int keep(int a, int b) { use(&b); use(&a); return a + b; }\n";

#[test]
fn the_first_integer_goes_in_rbp_and_the_rest_in_their_slots() {
    for level in ["-O0", "-O2"] {
        let (ok, asm, err) = compile("call", level, SOURCE);
        assert!(ok, "{level}: {err}");
        let c = body(&asm, "c");
        assert!(c.iter().any(|line| line.ends_with(", %rbp")), "{level}: {c:?}");
        assert!(c.iter().any(|line| line.ends_with(", 8(%rsp)")), "{level}: {c:?}");
        assert!(c.iter().any(|line| line.ends_with(", 16(%rsp)")), "{level}: {c:?}");
        assert!(!c.iter().any(|line| line.ends_with(", (%rsp)")), "{level}: {c:?}");
    }
}

#[test]
fn a_parameter_whose_address_is_taken_lives_in_its_slot() {
    let (ok, asm, err) = compile("home", "-O2", SOURCE);
    assert!(ok, "{err}");
    // The register is stored to the first slot, which is 8(%rsp) at entry, and the slot after it
    // is where the first unnamed argument is read from.
    let vf = body(&asm, "vf");
    assert!(vf.iter().any(|line| line.contains("%rbp,")), "{vf:?}");
    assert!(vf.iter().any(|line| line.ends_with(", 8(%rsp)")), "{vf:?}");
    assert!(vf.iter().any(|line| line.contains("16(%rsp),")), "{vf:?}");
    // With a frame of 24 bytes the two slots are at 32 and 40.
    let keep = body(&asm, "keep");
    assert!(keep.contains(&"subq\t$24, %rsp"), "{keep:?}");
    assert!(keep.iter().any(|line| line.starts_with("leaq\t32(%rsp),")), "{keep:?}");
    assert!(keep.iter().any(|line| line.contains("40(%rsp)")), "{keep:?}");
}

#[test]
fn no_function_keeps_a_frame_pointer() {
    for level in ["-O0", "-O2"] {
        let (ok, asm, err) = compile("frame", level, SOURCE);
        assert!(ok, "{level}: {err}");
        assert!(!asm.contains("pushq\t%rbp"), "{level}: {asm}");
        assert!(!asm.contains("movq\t%rsp, %rbp"), "{level}: {asm}");
    }
}

#[test]
fn a_frame_only_a_frame_pointer_reaches_is_refused() {
    let source = "extern void use(void *);\nint vla(int n) { int a[n]; use(a); return a[0]; }\n";
    let (ok, _, err) = compile("vla", "-O2", source);
    assert!(!ok, "a variable length array was compiled");
    assert!(err.contains("needs a frame pointer"), "{err}");
    assert!(!err.contains("not lowered yet"), "{err}");
}
