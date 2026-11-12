//! The library's `sqrt` and `sqrtf` as the instruction on x86-64, as gcc writes them.
//!
//! gcc writes a call to `sqrt` as `sqrtsd` when it optimizes. Under `-fmath-errno`, which is the
//! default, the call stays for an operand below zero, since the routine sets `errno` for it. Under
//! `-fno-math-errno` the instruction is the whole answer. At `-O0` the call stays. A function of
//! the program's own named `sqrt` is not the library's and is always called.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the mnemonics are the same
/// wherever this runs.
const TARGET: &str = "x86_64-unknown-freebsd15.1";

const SOURCE: &str = "\
double sqrt(double);
float sqrtf(float);
double root(double x) { return sqrt(x) + 1.0; }
float rootf(float x) { return sqrtf(x) + 1.0f; }
";

const OWN: &str = "\
double sqrt(double x) { return x; }
double root(double x) { return sqrt(x) + 1.0; }
";

/// The assembly for `source` under those flags.
fn assembly(what: &str, source: &str, flags: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-sqrt-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("rucc runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("the assembly is text")
}

/// The lines of the function `name`, from its label to the next label of a function.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let label = format!("{name}:");
    text.lines()
        .skip_while(|line| line.trim() != label)
        .skip(1)
        .take_while(|line| !line.contains(".size"))
        .collect()
}

fn has(lines: &[&str], word: &str) -> bool {
    lines.iter().any(|line| line.split([' ', '\t', ',']).any(|part| part == word))
}

fn calls(lines: &[&str], name: &str) -> bool {
    lines.iter().any(|line| {
        let line = line.trim();
        (line.starts_with("call") || line.starts_with("jmp"))
            && line.split([' ', '\t', '@']).any(|part| part == name)
    })
}

#[test]
fn the_square_root_is_the_instruction_with_the_call_below_zero() {
    let text = assembly("errno", SOURCE, &["-O2"]);
    let root = body(&text, "root");
    assert!(has(&root, "sqrtsd"), "{text}");
    assert!(calls(&root, "sqrt"), "the call is kept for errno: {text}");
    let rootf = body(&text, "rootf");
    assert!(has(&rootf, "sqrtss"), "{text}");
    assert!(calls(&rootf, "sqrtf"), "the call is kept for errno: {text}");
}

#[test]
fn the_square_root_is_the_instruction_alone_without_errno() {
    let text = assembly("noerrno", SOURCE, &["-O2", "-fno-math-errno"]);
    for (name, mnemonic, routine) in [("root", "sqrtsd", "sqrt"), ("rootf", "sqrtss", "sqrtf")] {
        let lines = body(&text, name);
        assert!(has(&lines, mnemonic), "{name}: {text}");
        assert!(!calls(&lines, routine), "{name}: {text}");
    }
}

#[test]
fn the_square_root_is_a_call_at_o0_and_for_a_function_of_the_program() {
    let text = assembly("o0", SOURCE, &["-O0"]);
    let root = body(&text, "root");
    assert!(!has(&root, "sqrtsd"), "{text}");
    assert!(calls(&root, "sqrt"), "{text}");
    let text = assembly("own", OWN, &["-O2", "-fno-math-errno", "-fno-inline"]);
    let root = body(&text, "root");
    assert!(!has(&root, "sqrtsd"), "{text}");
}
