//! `-fzero-call-used-regs=` and the `zero_call_used_regs` attribute, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12, and tamnd/rucc#2281.
//!
//! What is compared is the run of clearing instructions in front of each `ret`, against what gcc
//! 13 writes for the same function. The rest of the body is this compiler's own and is not the
//! same as gcc's, so the functions here are ones where both put their values in the same
//! registers.

use std::process::Command;

/// The listing of `source` compiled for `target` Linux at `-O2` with `flags`.
fn asm(target: &str, what: &str, flags: &[&str], source: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-zero-{}-{target}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}-unknown-linux-gnu"))
        .args(["-nostdinc", "-O2", "-S", "-o", "-", "-fno-asynchronous-unwind-tables"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("the listing is text")
}

/// The instructions of one function, with the labels and directives left out and the spaces
/// between an instruction and its operands made one.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.ends_with(':') || line.starts_with(".L"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// The registers cleared in front of the last instruction of one function, in order.
fn cleared(text: &str, name: &str) -> Vec<String> {
    let body = body(text, name);
    let mut found: Vec<String> = body[..body.len() - 1]
        .iter()
        .rev()
        .map_while(|line| {
            let x86 = line.strip_prefix("xorl %").and_then(|rest| rest.split_once(','));
            let a64 = line.strip_prefix("mov ").and_then(|rest| rest.strip_suffix(", #0"));
            x86.map(|(reg, _)| reg.to_string()).or(a64.map(str::to_string))
        })
        .collect();
    found.reverse();
    found
}

const USED: &[&str] = &["-fzero-call-used-regs=used-gpr"];

const SOURCE: &str = "\
long g(long);
long f(long a, long b) { return a * b + 3; }
long callr(long a, long b) { return g(a) + b; }
long six(long a, long b, long c, long d, long e, long f) { return a+b+c+d+e+f; }
long div(long a, long b) { return a / b; }
long mid(long a) { long r = g(a); g(0); return r; }
";

#[test]
fn used_gpr_clears_what_gcc_clears_on_x86_64() {
    let text = asm("x86_64", "used", USED, SOURCE);
    for (name, gcc) in [
        ("f", &["esi", "edi"][..]),
        ("callr", &["esi", "edi"]),
        ("six", &["edx", "ecx", "esi", "edi", "r8d", "r9d"]),
        ("div", &["edx", "esi", "edi"]),
        ("mid", &["edi"]),
    ] {
        assert_eq!(cleared(&text, name), gcc, "{name}: {text}");
        assert_eq!(body(&text, name).last().map(String::as_str), Some("ret"), "{text}");
    }
    let text = asm("x86_64", "none", &[], SOURCE);
    for name in ["f", "callr", "six", "div", "mid"] {
        assert_eq!(cleared(&text, name), Vec::<String>::new(), "{name}: {text}");
    }
}

/// A function that makes a call has used `x16` and `x17`, since a veneer the linker puts between
/// the call and its target may write them.
#[test]
fn used_gpr_clears_what_gcc_clears_on_aarch64() {
    let text = asm("aarch64", "used", USED, SOURCE);
    for (name, gcc) in [
        ("f", &["x1"][..]),
        ("callr", &["x1", "x16", "x17"]),
        ("six", &["x1", "x2", "x3", "x4", "x5"]),
        ("div", &["x1"]),
    ] {
        assert_eq!(cleared(&text, name), gcc, "{name}: {text}");
    }
}

/// `all-gpr` is every register a call may clobber but the one the answer is in. gcc also clears
/// `x18` on AArch64, which this compiler keeps for the platform.
#[test]
fn a_function_that_names_a_choice_gets_that_choice() {
    let source = "\
__attribute__((zero_call_used_regs(\"skip\"))) long s(long a, long b) { return a + b; }
__attribute__((zero_call_used_regs(\"all-gpr\"))) void ag(void) { }
__attribute__((zero_call_used_regs(\"all-gpr-arg\"))) long aga(long a) { return a; }
__attribute__((zero_call_used_regs(\"used-gpr-arg\"))) long uga(long a, long b) { return a*b+7; }
";
    let text = asm("x86_64", "attr", USED, source);
    assert_eq!(cleared(&text, "s"), Vec::<String>::new(), "{text}");
    let all = ["eax", "edx", "ecx", "esi", "edi", "r8d", "r9d", "r10d", "r11d"];
    assert_eq!(cleared(&text, "ag"), all, "{text}");
    assert_eq!(cleared(&text, "aga"), all[1..7], "{text}");
    assert_eq!(cleared(&text, "uga"), ["esi", "edi"], "{text}");
    // The attribute is read when the command line said nothing as well.
    assert_eq!(cleared(&asm("x86_64", "alone", &[], source), "ag"), all);

    let text = asm("aarch64", "attr", USED, source);
    let all: Vec<String> = (0..18).map(|n| format!("x{n}")).collect();
    assert_eq!(cleared(&text, "s"), Vec::<String>::new(), "{text}");
    assert_eq!(cleared(&text, "ag"), all, "{text}");
    assert_eq!(cleared(&text, "aga"), all[1..8], "{text}");
    assert_eq!(cleared(&text, "uga"), ["x1"], "{text}");
}

/// A call that became a jump leaves through the callee's `ret`, and a result nobody reads is not
/// a register the body used.
#[test]
fn a_tail_call_clears_nothing_and_a_thrown_away_result_is_not_cleared() {
    let source = "long g(long); long tail(long a) { return g(a + 1); } \
                  void ign(long a) { g(a); }\n";
    let text = asm("x86_64", "tail", USED, source);
    assert_eq!(body(&text, "tail").last().map(String::as_str), Some("jmp g"), "{text}");
    assert_eq!(cleared(&text, "tail"), Vec::<String>::new(), "{text}");
    assert_eq!(cleared(&text, "ign"), ["edi"], "{text}");
}

/// The registers are clear before the return thunk is jumped to, as they are before `ret`.
#[test]
fn the_registers_are_cleared_before_the_return_thunk() {
    let flags = [USED[0], "-mfunction-return=thunk-extern"];
    let text = asm("x86_64", "thunk", &flags, "long f(long a, long b) { return a * b + 3; }\n");
    let body = body(&text, "f");
    assert_eq!(body.last().map(String::as_str), Some("jmp __x86_return_thunk"), "{text}");
    assert_eq!(cleared(&text, "f"), ["esi", "edi"], "{text}");
}

#[test]
fn a_choice_that_clears_the_vector_registers_is_refused_with_its_issue() {
    let dir = std::env::temp_dir().join(format!("rucc-zero-{}-vector", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    let source = "__attribute__((zero_call_used_regs(\"used\"))) long u(long a) { return a; }\n";
    std::fs::write(&path, source).expect("the fixture can be written");
    let run = |flags: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=x86_64-unknown-linux-gnu", "-nostdinc", "-S", "-o", "-"])
            .args(flags)
            .output()
            .expect("the compiler is built before its own tests run")
    };
    let out = run(&[path.to_str().expect("a temporary path is text")]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("tamnd/rucc#2335"));
}
