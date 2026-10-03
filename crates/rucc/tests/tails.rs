//! A function whose calls all became jumps keeps no frame for them.
//!
//! A tail call leaves nothing below the stack pointer for the callee to find, so a function that
//! makes no other call owes nobody an aligned stack pointer and has no link register to put away.
//! gcc writes `int f(int a) { return g(a + 1); }` as an add and a jump on both machines, and the
//! kernel is full of small wrappers like it. See tamnd/rucc-kernel#4.

use std::path::PathBuf;
use std::process::Command;

/// The assembly for that source at `-O2` for that target, under a directory of its own.
fn assembly(what: &str, target: &str, extra: &[&str], source: &str) -> String {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rucc-tails-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-O2", "-S", "-o", "-"])
        .args(extra)
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
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

const WRAPPERS: &str = "int g(int); int h(int, int);
int f(int a) { if (a > 3) return g(a + 1); return h(a, 2); }
int k(int a) { return g(a) + 1; }
";

#[test]
fn a_function_that_only_jumps_away_takes_no_frame_on_x86_64() {
    let text = assembly("x64", "x86_64-unknown-linux-gnu", &[], WRAPPERS);
    let f = body(&text, "f");
    assert!(!f.iter().any(|line| line.contains("%rsp")), "{f:#?}");
    assert!(f.contains(&"jmp g".to_owned()) && f.contains(&"jmp h".to_owned()), "{f:#?}");
    // One real call is enough to need the aligned frame again.
    let k = body(&text, "k");
    assert!(k.contains(&"call g".to_owned()), "{k:#?}");
    assert!(k.iter().any(|line| line.contains("%rsp")), "{k:#?}");
}

#[test]
fn a_function_that_only_jumps_away_saves_no_link_register_on_aarch64() {
    let text = assembly("a64", "aarch64-unknown-linux-gnu", &[], WRAPPERS);
    let f = body(&text, "f");
    assert!(!f.iter().any(|line| line.contains("x30") || line.contains("sp")), "{f:#?}");
    assert!(f.contains(&"b g".to_owned()) && f.contains(&"b h".to_owned()), "{f:#?}");
    let k = body(&text, "k");
    assert!(k.contains(&"bl g".to_owned()), "{k:#?}");
    assert!(k.iter().any(|line| line.contains("x30")), "{k:#?}");
}

#[test]
fn a_frame_pointer_asked_for_is_kept_and_put_back_before_the_jump() {
    let text = assembly("fp", "x86_64-unknown-linux-gnu", &["-fno-omit-frame-pointer"], WRAPPERS);
    let f = body(&text, "f");
    assert_eq!(f.first().map(String::as_str), Some("pushq %rbp"), "{f:#?}");
    let pop = f.iter().position(|line| line == "popq %rbp").expect("the pointer is put back");
    let jump = f.iter().position(|line| line.starts_with("jmp")).expect("a jump away");
    assert!(pop < jump, "{f:#?}");
}

/// Each arm assigns a variable and one `return` gives it back, so both calls jump to a block that
/// only returns, and both are in tail position all the same. Postgres's `print_action` is this
/// shape. See #2205.
const JOINED: &str = "int on_true(int); int on_false(int); int report(const char *, ...);
int join(int x) { int r; if (x > 10) r = on_true(x); else r = on_false(x - 1); return r; }
void drop(int x) { report(\"%d\", x); }
";

#[test]
fn calls_that_meet_at_one_return_both_jump_away() {
    let text = assembly("join", "x86_64-unknown-linux-gnu", &[], JOINED);
    let join = body(&text, "join");
    assert!(join.contains(&"jmp on_true".to_owned()), "{join:#?}");
    assert!(join.contains(&"jmp on_false".to_owned()), "{join:#?}");
    assert!(
        !join.iter().any(|line| line.starts_with("call") || line.contains("%rsp")),
        "{join:#?}"
    );
}

#[test]
fn a_void_function_drops_what_its_last_call_gives_back_and_jumps() {
    let text = assembly("drop", "x86_64-unknown-linux-gnu", &[], JOINED);
    let drop = body(&text, "drop");
    assert!(drop.contains(&"jmp report".to_owned()), "{drop:#?}");
    assert!(!drop.iter().any(|line| line.starts_with("call")), "{drop:#?}");
}

/// On i386 a `double` comes back on the x87 stack, which a jump would leave full for a caller that
/// expects it empty, so the call stays and the answer is popped.
#[test]
fn a_void_function_keeps_a_call_whose_answer_is_on_the_x87_stack() {
    let source = "double weigh(int); void drop(int x) { weigh(x); }\n";
    let text = assembly("x87", "i386-unknown-linux-gnu", &[], source);
    let drop = body(&text, "drop");
    assert!(
        drop.iter().any(|line| line.starts_with("call") && line.contains("weigh")),
        "{drop:#?}"
    );
    assert!(
        !drop.iter().any(|line| line.starts_with("jmp") && line.contains("weigh")),
        "{drop:#?}"
    );
}
