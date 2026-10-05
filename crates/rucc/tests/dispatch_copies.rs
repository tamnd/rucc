//! A computed `goto` does not copy the values it carries into the registers they are in already.
//!
//! Issue 1994. Every jump of an interpreter's dispatch copies the values it carries into one
//! register each, and the allocator hands each of those the register the value was in, so what
//! came out was a `movq %rax, %rax` per value in front of every jump. The unit tests in
//! `rucc-codegen` cover the pass. These run the compiler and read the dispatch it wrote.

use std::path::PathBuf;
use std::process::Command;

/// A stack machine with a dispatch table, which carries the code pointer, the stack and two
/// values from one instruction to the next.
const SOURCE: &str = "\
long run(const unsigned char *code, long *stack, long a, long b) {
    static void *const ops[] = { &&done, &&push, &&add, &&swap };
    long *top = stack;
    goto *ops[*code++];
push:
    *++top = a;
    goto *ops[*code++];
add:
    top[-1] += top[0];
    top--;
    goto *ops[*code++];
swap:
    a ^= b;
    b ^= a;
    goto *ops[*code++];
done:
    return *top + a + b;
}
";

/// The assembly the compiler writes for the fixture for that target.
fn assembly(target: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-dispatch-copies-{}-{target}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("dispatch.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-", "-O2"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture for {target}:\n{said}");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// Whether any line of the assembly is an instruction under that mnemonic.
fn has(asm: &str, mnemonic: &str) -> bool {
    asm.lines().any(|line| line.split_whitespace().next() == Some(mnemonic))
}

/// Every line that copies a whole register into itself under that mnemonic.
fn onto_itself<'a>(asm: &'a str, mnemonic: &str) -> Vec<&'a str> {
    asm.lines()
        .filter(|line| {
            let mut words = line.split_whitespace();
            if words.next() != Some(mnemonic) {
                return false;
            }
            let rest: String = words.collect();
            rest.split_once(',').is_some_and(|(to, from)| to == from)
        })
        .collect()
}

#[test]
fn the_dispatch_on_x86_64_copies_no_register_into_itself() {
    let asm = assembly("x86_64-unknown-linux-gnu");
    assert!(asm.contains("jmp\t*%"), "the fixture no longer has a computed goto:\n{asm}");
    let copies = onto_itself(&asm, "movq");
    assert!(copies.is_empty(), "{copies:?} in:\n{asm}");
}

#[test]
fn the_dispatch_on_aarch64_copies_no_register_into_itself() {
    let asm = assembly("aarch64-unknown-linux-gnu");
    assert!(has(&asm, "br"), "the fixture no longer has a computed goto:\n{asm}");
    // The whole register only. A copy of `w0` into itself clears the top half and is not nothing.
    let whole = |line: &&str| line.split_whitespace().nth(1).is_some_and(|to| to.starts_with('x'));
    let copies: Vec<&str> = onto_itself(&asm, "mov").into_iter().filter(whole).collect();
    assert!(copies.is_empty(), "{copies:?} in:\n{asm}");
}
