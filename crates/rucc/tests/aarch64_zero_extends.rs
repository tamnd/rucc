//! A zero widening of a `w` register on AArch64 left out when the register is already zero above
//! its thirty two bits, as gcc writes it.
//!
//! Every instruction that writes a `w` register clears the thirty two bits above it, so `return
//! x + y;` from two `unsigned` into an `unsigned long` is one `add w0, w0, w1` and nothing after
//! it. Before this it was followed by `mov w0, w0`, which is the widening and does nothing. A loop
//! counter is the same when every value it starts or steps from is one of those, and then the
//! subscript it scales goes into the load with no widening in front of it. An argument is not:
//! the caller is free to leave anything in the upper half, so its widening stays.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
unsigned long sum(unsigned x, unsigned y) { return x + y; }
unsigned long down(unsigned x) { return x >> 3; }
unsigned long arg(unsigned x) { return x; }
long walk(int *p, unsigned n) { long s = 0; for (unsigned i = 0; i < n; i++) s += p[i]; return s; }
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-zext-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-fno-pic", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out
}

fn listing() -> String {
    String::from_utf8(compile(&["-S"]).stdout).expect("a listing is text")
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn a_word_the_function_wrote_is_not_widened_again() {
    let text = listing();
    assert_eq!(body(&text, "sum"), ["add w0, w0, w1", "ret"], "{text}");
    assert_eq!(body(&text, "down"), ["lsr w0, w0, #3", "ret"], "{text}");
}

#[test]
fn an_argument_keeps_its_widening() {
    let text = listing();
    assert_eq!(body(&text, "arg"), ["mov w0, w0", "ret"], "{text}");
}

/// The counter starts at a `mov` of zero and steps with an `add`, both of which write a `w`
/// register, so the load reads it as it is.
#[test]
fn a_loop_counter_goes_into_the_load_as_it_is() {
    let text = listing();
    let walk = body(&text, "walk");
    assert!(
        walk.iter().any(|line| line.starts_with("ldrsw ") && line.ends_with(", lsl #2]")),
        "{text}"
    );
    assert!(!walk.iter().any(|line| line.starts_with("mov w") && !line.contains('#')), "{text}");
    assert!(!walk.iter().any(|line| line.starts_with("lsl ")), "{text}");
}

/// The integrated assembler takes the result too.
#[test]
fn the_object_assembles() {
    let out = compile(&["-c"]);
    assert!(!out.stdout.is_empty());
}
