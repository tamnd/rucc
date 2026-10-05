//! A constant stored to memory on x86-64 is carried in the store.
//!
//! Issue 1994. Every store of a constant used to put the constant in a register first and store
//! the register, which is a register and an instruction the machine never needed, and an
//! interpreter that clears a flag on every step paid that on every step. The unit tests in
//! `rucc-codegen` cover the rules. This one runs the compiler and reads the stores it wrote.

use std::path::PathBuf;
use std::process::Command;

/// A structure with a field at each width, and a store through an index for the addressing mode
/// the rules leave to the fold.
const SOURCE: &str = "\
struct s { int a; _Bool n; short h; long l; char c; void *p; long big; };
void fill(struct s *s, long *v, long i) {
    s->a = 5;
    s->n = 1;
    s->h = 7;
    s->l = -1;
    s->c = 'x';
    s->p = 0;
    s->big = 0x123456789;
    v[i] = 3;
}
";

/// The assembly the compiler writes for the fixture.
fn assembly() -> String {
    let dir = std::env::temp_dir().join(format!("rucc-constant-stores-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("stores.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(["-S", "-o", "-", "-O2"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The instructions, one to a line with the space in them made single.
fn instructions(asm: &str) -> Vec<String> {
    asm.lines().map(|line| line.split_whitespace().collect::<Vec<_>>().join(" ")).collect()
}

#[test]
fn a_constant_that_fits_is_stored_without_a_register() {
    let asm = assembly();
    let lines = instructions(&asm);
    for want in [
        "movl $5, (%rdi)",
        "movb $1, 4(%rdi)",
        "movw $7, 6(%rdi)",
        "movq $-1, 8(%rdi)",
        "movb $120, 16(%rdi)",
        "movq $0, 24(%rdi)",
        "movq $3, (%rsi,%rdx,8)",
    ] {
        assert!(lines.iter().any(|line| line == want), "no `{want}` in:\n{asm}");
    }
}

#[test]
fn a_constant_too_wide_for_the_instruction_still_goes_through_a_register() {
    let asm = assembly();
    let lines = instructions(&asm);
    let big = lines.iter().find(|line| line.ends_with(", 32(%rdi)"));
    assert!(
        big.is_some_and(|line| line.starts_with("movq %r")),
        "the wide constant was not stored from a register in:\n{asm}"
    );
}
