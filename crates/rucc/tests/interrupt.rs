//! What an x86 interrupt handler and a function that saves every register look like, end to end.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! A test of the whole compiler rather than of one crate, because the two attributes are read in
//! the checker, carried through the IR as bits, and spent in the backend in four places that each
//! could be green on its own while the function still came out wrong: the convention the handler
//! is compiled against, where its parameters are read from, which registers its frame saves, and
//! how its epilogue returns. What would go wrong without any one of them is a handler that links,
//! runs and breaks the machine later, so the instructions are asserted rather than the exit code.
//!
//! Every fixture is built with `-mgeneral-regs-only`, which is how the kernel builds the files
//! these are in, since a function of either kind that may use a vector register is refused.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because both attributes are x86's
/// and the instructions asserted are x86-64's.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-int-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took that source for that target under those flags, what it wrote and
/// what it said.
fn run(what: &str, target: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-"])
        .args(flags)
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

/// The instructions of one function, optimized and without the vector registers, with the
/// labels, the directives and the leading tabs taken off.
fn body(what: &str, source: &str, name: &str) -> Vec<String> {
    let (ok, out, err) = run(what, TARGET, &["-O2", "-mgeneral-regs-only"], source);
    assert!(ok, "the compiler refused the fixture:\n{err}");
    let open = format!("{name}:");
    let close = format!(".size\t{name},");
    out.lines()
        .map(str::trim)
        .skip_while(|line| *line != open)
        .skip(1)
        .take_while(|line| !line.starts_with(&close))
        .filter(|line| !line.is_empty() && !line.starts_with('.'))
        .map(str::to_owned)
        .collect()
}

/// The registers a list of instructions pushes, in the order it pushes them.
fn pushed(lines: &[String]) -> Vec<&str> {
    lines.iter().filter_map(|line| line.strip_prefix("pushq\t")).collect()
}

/// The registers a list of instructions pops, in the order it pops them.
fn popped(lines: &[String]) -> Vec<&str> {
    lines.iter().filter_map(|line| line.strip_prefix("popq\t")).collect()
}

/// The nine registers the System V convention lets a callee write, which is what a function that
/// saves every register has to save around a call, since the callee may write any of them.
const CALLER_SAVED: [&str; 9] =
    ["%rax", "%rcx", "%rdx", "%rsi", "%rdi", "%r8", "%r9", "%r10", "%r11"];

#[test]
fn an_interrupt_handler_returns_with_iretq() {
    let source = "\
struct frame;
struct frame *last;
__attribute__((interrupt)) void handler(struct frame *frame) { last = frame; }
";
    let lines = body("plain", source, "handler");
    // The address of the frame is the entry stack pointer, which is where the address to go back
    // to is, and the one register written to hold it is saved around it.
    assert_eq!(lines.last().map(String::as_str), Some("iretq"), "{lines:#?}");
    assert!(!lines.iter().any(|line| line == "ret"), "{lines:#?}");
    assert!(!lines.iter().any(|line| line == "cld"), "a handler that calls nothing: {lines:#?}");
    let saved = pushed(&lines);
    assert!(!saved.is_empty(), "the register the address went through is saved: {lines:#?}");
    let mut back = popped(&lines);
    back.reverse();
    assert_eq!(saved, back, "{lines:#?}");
    let lea = lines.iter().find(|line| line.starts_with("leaq\t")).expect("the frame's address");
    assert_eq!(lea, &format!("leaq\t{}(%rsp), %rax", 8 * saved.len()), "{lines:#?}");
}

#[test]
fn an_interrupt_handler_with_an_error_code_takes_it_off_before_returning() {
    let source = "\
struct frame;
struct frame *last;
unsigned long code;
__attribute__((interrupt)) void handler(struct frame *frame, unsigned long error)
{
    last = frame;
    code = error;
}
";
    let lines = body("coded", source, "handler");
    let tail: Vec<&str> = lines.iter().rev().take(2).rev().map(String::as_str).collect();
    assert_eq!(tail, ["addq\t$8, %rsp", "iretq"], "{lines:#?}");
    // The code is the word at the entry stack pointer and the frame is the word above it, so
    // counted from where the pushes left the stack pointer they are that many words further up.
    let pushes = pushed(&lines).len();
    let error = format!("movq\t{}(%rsp)", 8 * pushes);
    let frame = format!("leaq\t{}(%rsp)", 8 * pushes + 8);
    assert!(lines.iter().any(|line| line.starts_with(&error)), "{error} in {lines:#?}");
    assert!(lines.iter().any(|line| line.starts_with(&frame)), "{frame} in {lines:#?}");
}

#[test]
fn an_interrupt_handler_that_calls_clears_the_direction_flag() {
    let source = "\
struct frame;
void use(struct frame *);
__attribute__((interrupt)) void handler(struct frame *frame) { use(frame); }
";
    let lines = body("calls", source, "handler");
    let cld = lines.iter().position(|line| line == "cld").expect("a cld");
    let call = lines.iter().position(|line| line == "call\tuse").expect("the call");
    assert!(cld < call, "{lines:#?}");
    let mut saved = pushed(&lines);
    saved.sort_unstable();
    let mut every = CALLER_SAVED.to_vec();
    every.sort_unstable();
    assert_eq!(saved, every, "{lines:#?}");
    assert_eq!(lines.last().map(String::as_str), Some("iretq"), "{lines:#?}");
}

#[test]
fn a_function_that_saves_every_register_puts_back_what_it_writes() {
    let source = "\
int counter;
__attribute__((no_caller_saved_registers)) void tick(void) { counter++; }
";
    let lines = body("tick", source, "tick");
    let saved = pushed(&lines);
    assert!(!saved.is_empty(), "the registers the increment goes through: {lines:#?}");
    let mut back = popped(&lines);
    back.reverse();
    assert_eq!(saved, back, "{lines:#?}");
    // Every register the body writes is one of the saved ones.
    for line in lines.iter().filter(|line| !line.starts_with("pushq") && !line.starts_with("popq"))
    {
        let Some((_, written)) = line.rsplit_once(", ") else { continue };
        if written.starts_with('%') {
            let wide = written.replace("%e", "%r");
            assert!(saved.contains(&wide.as_str()), "{written} is written in {lines:#?}");
        }
    }
    assert_eq!(lines.last().map(String::as_str), Some("ret"), "{lines:#?}");
}

#[test]
fn a_function_that_saves_every_register_and_calls_saves_every_caller_saved_one() {
    let source = "\
int counter;
void use(void *);
__attribute__((no_caller_saved_registers)) int calling(void) { use(0); return counter; }
";
    let lines = body("calling", source, "calling");
    // All nine but the one the value comes back in, which the caller is about to read. No tail
    // call either, since the callee would be handed registers this function promised to keep.
    let mut saved = pushed(&lines);
    saved.sort_unstable();
    let mut every: Vec<&str> = CALLER_SAVED.into_iter().filter(|reg| *reg != "%rax").collect();
    every.sort_unstable();
    assert_eq!(saved, every, "{lines:#?}");
    assert!(lines.iter().any(|line| line == "call\tuse"), "{lines:#?}");
    assert!(!lines.iter().any(|line| line == "cld"), "{lines:#?}");
    assert_eq!(lines.last().map(String::as_str), Some("ret"), "{lines:#?}");
}

#[test]
fn an_interrupt_handler_is_checked_the_way_gcc_checks_it() {
    let source = "\
struct frame;
__attribute__((interrupt)) void a(int x) { (void)x; }
__attribute__((interrupt)) void b(struct frame *f, int c) { (void)f; (void)c; }
__attribute__((interrupt)) void c(struct frame *f, unsigned long c, int d) { (void)f; }
__attribute__((interrupt)) int d(struct frame *f) { (void)f; return 0; }
__attribute__((interrupt, naked)) void e(struct frame *f) { (void)f; }
__attribute__((interrupt)) void g(struct frame *f) { (void)f; }
void h(void) { g(0); }
void through(void) { void (*p)(struct frame *) = g; p(0); }
";
    let (ok, _, err) = run("checked", TARGET, &["-mgeneral-regs-only"], source);
    assert!(!ok, "{err}");
    for (line, said) in [
        (2, "interrupt service routine should have a pointer as the first argument"),
        (3, "interrupt service routine should have 'unsigned long int' as the second argument"),
        (4, "interrupt service routine can only have a pointer argument and an optional integer"),
        (5, "interrupt service routine must return 'void'"),
        (6, "interrupt and naked attributes are not compatible"),
        (8, "interrupt service routine cannot be called directly"),
    ] {
        let found = err.lines().any(|it| it.contains(&format!(":{line}:")) && it.contains(said));
        assert!(found, "line {line} says {said}:\n{err}");
    }
    // A call through a pointer is how a handler is meant to be reached, if at all.
    assert!(!err.contains(":9:"), "{err}");

    // And one that may use the vector registers is gcc's sorry, naming an exception service
    // routine when the handler takes an error code.
    let source = "\
struct frame;
__attribute__((interrupt)) void f(struct frame *f) { (void)f; }
__attribute__((interrupt)) void g(struct frame *f, unsigned long c) { (void)f; (void)c; }
__attribute__((no_caller_saved_registers)) void h(void) {}
";
    let (ok, _, err) = run("sorry", TARGET, &[], source);
    assert!(!ok, "{err}");
    assert!(
        err.contains("SSE instructions aren't allowed in an interrupt service routine"),
        "{err}"
    );
    assert!(
        err.contains("SSE instructions aren't allowed in an exception service routine"),
        "{err}"
    );
    let mmx = "MMX/3Dnow instructions aren't allowed in a function with the \
               'no_caller_saved_registers' attribute";
    assert!(err.contains(mmx), "{err}");
}

#[test]
fn an_interrupt_handler_is_refused_on_a_target_without_one() {
    let source = "\
struct frame;
__attribute__((interrupt)) void handler(struct frame *frame) { (void)frame; }
";
    let (ok, _, err) = run("elsewhere", "aarch64-unknown-linux-gnu", &[], source);
    assert!(!ok, "{err}");
    assert!(err.contains("'interrupt' attribute is not supported"), "{err}");
}
