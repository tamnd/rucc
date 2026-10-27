//! A constant handed to an x86-64 `asm` under a constraint that allows one is written into the
//! instruction, the way gcc prints it. The kernel's `__set_bit` is `btsq %1, %0` against `"Ir"`,
//! and rucc put the bit number in a register first and used the slower `bts` that takes one. An
//! `int` constant against a `q` instruction was refused outright, since the register it went in
//! was the wrong width for the instruction.

use std::process::Command;

const SOURCE: &str = "\
void set8(unsigned long *p) { asm volatile(\"btsq %1,%0\" : : \"m\"(*p), \"Ir\"(8L) : \"memory\"); }
void set40(unsigned long *p) { asm volatile(\"btsq %1,%0\" : : \"m\"(*p), \"Ir\"(40L) : \"memory\"); }
void add5(long *p) { asm(\"addq %1,%0\" : \"+m\"(*p) : \"i\"(5)); }
void dec(int *p) { asm(\"lock; addl %1,%0\" : \"+m\"(*p) : \"ir\"(-1)); }
";

fn assembly() -> String {
    let dir = std::env::temp_dir().join(format!("rucc-asm-constant-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// The lines of one function, from its label to its `ret`.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let label = format!("{name}:");
    let lines = text.lines().skip_while(|line| *line != label).skip(1);
    let lines = lines.take_while(|line| !line.contains("ret"));
    lines.map(str::trim).filter(|line| !line.starts_with('.')).collect()
}

#[test]
fn a_constant_the_constraint_allows_is_written_into_the_instruction() {
    let text = assembly();
    assert_eq!(body(&text, "set8"), ["btsq\t$8, (%rdi)"], "{text}");
    assert_eq!(body(&text, "add5"), ["addq\t$5, (%rdi)"], "{text}");
    assert_eq!(body(&text, "dec"), ["lock; addl $-1,(%rdi)"], "{text}");
}

#[test]
fn a_constant_too_big_for_its_range_letter_goes_in_a_register() {
    let text = assembly();
    assert_eq!(body(&text, "set40"), ["movl\t$40, %eax", "btsq\t%rax, (%rdi)"], "{text}");
}
