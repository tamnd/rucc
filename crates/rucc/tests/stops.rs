//! Where control stops, and that nothing is written after it.
//!
//! objtool walks every instruction of a kernel object and reports one no path reaches. gcc writes
//! nothing after a `ud2` from `__builtin_trap()`, after `__builtin_unreachable()`, or after a call
//! to a `noreturn` function, and the kernel's `BUG()` and `panic()` are those three. Each test is
//! one of them, checked in the assembly rucc writes at `-O2` for x86-64. See tamnd/rucc-kernel#4.

use std::path::PathBuf;
use std::process::Command;

const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The assembly for that source at `-O2`, under a directory of its own.
fn assembly(what: &str, source: &str) -> String {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rucc-stops-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={TARGET}"), "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    String::from_utf8(done.stdout).expect("the listing is text")
}

/// The instructions of one function, without directives or labels.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.contains(".cfi_endproc") && !line.starts_with(".Lfunc_end"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.ends_with(':'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn nothing_follows_a_trap_at_the_end_of_a_function() {
    let text = assembly("trap", "void f(void) { __builtin_trap(); }\n");
    assert_eq!(body(&text, "f"), ["ud2"], "{text}");
}

#[test]
fn nothing_follows_a_stop_written_in_asm_and_promised() {
    let source = "void f(void) { __asm__ __volatile__(\"ud2\"); __builtin_unreachable(); }\n";
    let text = assembly("asm", source);
    assert_eq!(body(&text, "f"), ["ud2"], "{text}");
}

#[test]
fn a_function_that_is_only_unreachable_is_empty() {
    let text = assembly("empty", "void f(void) { __builtin_unreachable(); }\n");
    assert!(body(&text, "f").is_empty(), "{text}");
}

#[test]
fn a_call_that_does_not_come_back_is_the_last_thing_on_its_path() {
    let source = "\
__attribute__((noreturn)) void panic(const char *);
void f(void) { panic(\"no\"); }
void g(int x) { if (x) panic(\"x\"); }
";
    let text = assembly("noreturn", source);
    let f = body(&text, "f");
    assert_eq!(f.last().map(String::as_str), Some("call\tpanic"), "{text}");
    let g = body(&text, "g");
    let call = g.iter().position(|line| line == "call\tpanic").expect("g calls panic");
    assert_eq!(call + 1, g.len(), "{text}");
}

#[test]
fn a_branch_to_where_control_never_arrives_is_not_written() {
    let source = "\
int f(int x) { if (x > 3) __builtin_unreachable(); return x * 2; }
int g(int x) { switch (x) { case 1: return 5; case 2: return 7; default: __builtin_unreachable(); } }
";
    let text = assembly("branch", source);
    let f = body(&text, "f");
    assert!(!f.iter().any(|line| line.starts_with('j') || line.starts_with("cmp")), "{text}");
    let g = body(&text, "g");
    assert_eq!(g.iter().filter(|line| line.starts_with("cmp")).count(), 1, "{text}");
}

#[test]
fn a_parameter_read_before_an_early_call_stays_where_it_arrived() {
    let source = "\
extern int g(int);
int h(int x) { if (x == 1) return g(1); if (x == 2) return g(3); return 0; }
";
    let text = assembly("early", source);
    let h = body(&text, "h");
    assert!(!h.iter().any(|line| line.starts_with("push")), "{text}");
    assert!(h.iter().any(|line| line == "cmpl\t$1, %edi"), "{text}");
}
