//! What the x86-64 speculation hardening flags do to the code, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12. tamnd/rucc#2280.
//!
//! A test of the whole compiler rather than of `rucc_codegen::thunks`, because the flags are read
//! in the driver, the rewrite is done after the allocator has named every register, and the text
//! and the object are written by two different writers, so any one of them can be right while the
//! command line does nothing. The forms checked are gcc's, since objtool reads the object and
//! finds the call sites by them.

#![cfg(unix)]

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The target is written down rather than taken from the host, because every flag here is one
/// machine's.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// Three calls through three pointers, each called twice so that every pointer outlives a call
/// and has to be kept in a register the callee saves, and none of them in tail position. Those are `%rbx`, which is below `%r8`, and
/// `%r12` and `%r13`, which are not.
const CALLS: &str = "\
int three(void (*p)(void), void (*q)(void), void (*r)(void)) { p(); q(); r(); p(); q(); r(); return 0; }
";

/// A call through a pointer in tail position, which is a jump through a register once the frame
/// is given back.
const TAIL: &str = "\
int (*const *ops)(int);
int next(int x) { return ops[x & 3](x); }
";

/// A computed `goto`, which is an indirect jump rather than an indirect call.
const COMPUTED: &str = "\
int pick(int n) {
  static void *table[] = { &&odd, &&even };
  goto *table[n & 1];
odd:
  return 1;
even:
  return 2;
}
";

/// A `switch` dense enough, and with arms different enough, to be a jump table when one is
/// allowed.
const SWITCH: &str = "\
void g0(int); void g1(int); void g2(int);
void pick(int x) {
  switch (x) {
  case 0: g0(1); break; case 1: g1(2); break; case 2: g2(3); break; case 3: g0(4); break;
  case 4: g1(5); break; case 5: g2(6); break; case 6: g0(7); break; case 7: g1(8); break;
  case 8: g2(9); break; case 9: g0(10); break; case 10: g1(11); break; case 11: g2(12); break;
  }
}
";

/// The assembly the compiler writes for this C under these flags, at `-O2`.
fn asm(flags: &[&str], source: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args([TARGET, "-O2", "-S", "-o", "-"])
        .args(flags)
        .args(["-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the input into");
    stdin.write_all(source.as_bytes()).expect("the input can be written");
    drop(stdin);
    let out = child.wait_with_output().expect("the compiler finished");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The lines that are instructions, which is everything that is not a label and not something
/// said to the assembler.
fn insts(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.ends_with(':'))
        .collect()
}

/// The line after the first one that is `line`, if there is one.
fn after<'a>(lines: &[&'a str], line: &str) -> Option<&'a str> {
    let at = lines.iter().position(|one| *one == line)?;
    lines.get(at + 1).copied()
}

#[test]
fn an_indirect_call_goes_to_the_thunk_for_its_register() {
    let text = asm(&["-mindirect-branch=thunk-extern"], CALLS);
    let lines = insts(&text);
    for reg in ["rbx", "r12", "r13"] {
        let call = format!("call\t__x86_indirect_thunk_{reg}");
        assert_eq!(lines.iter().filter(|line| **line == call).count(), 2, "{text}");
    }
    assert!(!lines.iter().any(|line| line.contains('*')), "{text}");
    assert!(!lines.contains(&"cs"), "{text}");
    // Nothing is said about where the call sites are: objtool writes .retpoline_sites itself.
    assert!(!text.contains("retpoline_sites"), "{text}");
}

#[test]
fn a_tail_call_through_a_pointer_jumps_to_the_thunk_and_stops_speculation_after_it() {
    let text = asm(&["-mindirect-branch=thunk-extern", "-mharden-sls=all"], TAIL);
    let lines = insts(&text);
    assert_eq!(after(&lines, "jmp\t__x86_indirect_thunk_rax"), Some("int3"), "{text}");
    assert!(!lines.iter().any(|line| line.starts_with("call")), "{text}");
    assert!(!lines.contains(&"ret"), "{text}");
}

#[test]
fn the_segment_override_is_put_only_before_the_thunks_for_r8_to_r15() {
    let text = asm(&["-mindirect-branch=thunk-extern", "-mindirect-branch-cs-prefix"], CALLS);
    let lines = insts(&text);
    for (at, line) in lines.iter().enumerate() {
        if let Some(reg) = line.strip_prefix("call\t__x86_indirect_thunk_") {
            let padded = at > 0 && lines[at - 1] == "cs";
            assert_eq!(padded, reg.starts_with('r') && reg[1..].starts_with(char::is_numeric));
        }
    }
    assert_eq!(lines.iter().filter(|line| **line == "cs").count(), 4, "{text}");
}

#[test]
fn a_return_jumps_to_the_return_thunk() {
    let text = asm(&["-mfunction-return=thunk-extern", "-mharden-sls=all"], CALLS);
    let lines = insts(&text);
    assert!(!lines.contains(&"ret"), "{text}");
    // The jump to the thunk is a direct jump, so nothing stops speculation after it.
    assert_eq!(after(&lines, "jmp\t__x86_return_thunk"), None, "{text}");
    assert!(!text.contains("return_sites"), "{text}");
}

#[test]
fn a_breakpoint_follows_every_return_and_indirect_jump_under_sls() {
    let text = asm(&["-mharden-sls=all"], COMPUTED);
    let lines = insts(&text);
    let jump = lines.iter().position(|line| line.starts_with("jmp\t*")).expect("an indirect jump");
    assert_eq!(lines[jump + 1], "int3", "{text}");
    let rets = lines.iter().filter(|line| **line == "ret").count();
    let traps = lines.iter().filter(|line| **line == "int3").count();
    assert_eq!((rets, traps), (2, 3), "{text}");

    // Only the returns, and then only the jumps.
    let lines = insts(&asm(&["-mharden-sls=return"], COMPUTED)).join("\n");
    assert!(lines.contains("ret\nint3") && !lines.contains("*%rax\nint3"), "{lines}");
    let lines = insts(&asm(&["-mharden-sls=indirect-jmp"], COMPUTED)).join("\n");
    assert!(!lines.contains("ret\nint3") && lines.contains("int3"), "{lines}");

    // And after a jump to the thunk that took an indirect jump's place.
    let text = asm(&["-mharden-sls=all", "-mindirect-branch=thunk-extern"], COMPUTED);
    let lines = insts(&text);
    assert!(lines.iter().any(|line| line.starts_with("jmp\t__x86_indirect_thunk_")), "{text}");
    let jump = lines.iter().position(|line| line.starts_with("jmp\t__x86_indirect")).unwrap();
    assert_eq!(lines[jump + 1], "int3", "{text}");
}

#[test]
fn no_jump_table_is_written_under_no_jump_tables() {
    let tabled = asm(&[], SWITCH);
    assert!(insts(&tabled).iter().any(|line| line.starts_with("jmp\t*")), "{tabled}");
    let text = asm(&["-fno-jump-tables"], SWITCH);
    assert!(!insts(&text).iter().any(|line| line.starts_with("jmp\t*")), "{text}");
    assert!(!text.contains(".long\t.L"), "{text}");
}

#[test]
fn nothing_changes_when_nothing_is_asked_for() {
    let plain = asm(&[], CALLS);
    let kept =
        asm(&["-mindirect-branch=keep", "-mfunction-return=keep", "-mharden-sls=none"], CALLS);
    assert_eq!(plain, kept);
    assert!(insts(&plain).iter().any(|line| line.starts_with("call\t*%r12")), "{plain}");
}

/// The kernel writes these on code that runs before the thunks can, and gcc then leaves the branch
/// plain. `-mharden-sls=` still puts `int3` after it, since that asks for something else.
#[test]
fn a_function_that_says_keep_is_left_as_it_was() {
    let source = "\
__attribute__((function_return(\"keep\"))) int keepret(int x) { return x + 1; }
__attribute__((indirect_branch(\"keep\"))) int keepind(int (*p)(int)) { return p(1) + 1; }
int plain(int (*p)(int)) { return p(1) + 1; }
";
    let flags = ["-mindirect-branch=thunk-extern", "-mfunction-return=thunk-extern"];
    let text = asm(&[&flags[..], &["-mharden-sls=all"]].concat(), source);
    let body = |name: &str| -> Vec<&str> {
        let from = text.find(&format!("\n{name}:")).expect("the function is in the listing");
        let rest = &text[from + 1..];
        let to = rest.find(".size").unwrap_or(rest.len());
        insts(&rest[..to])
    };
    assert_eq!(body("keepret")[body("keepret").len() - 2..], ["ret", "int3"], "{text}");
    assert!(body("keepind").contains(&"call\t*%rax"), "{text}");
    assert_eq!(body("keepind").last(), Some(&"jmp\t__x86_return_thunk"), "{text}");
    assert!(body("plain").contains(&"call\t__x86_indirect_thunk_rax"), "{text}");
}

/// The object the compiler writes for this C under these flags, at `-O2`.
fn object(flags: &[&str], source: &str, what: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("rucc-thunks-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), source).expect("the input can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args([TARGET, "-O2", "-c", "-o"])
        .arg(dir.join("one.o"))
        .args(flags)
        .arg(dir.join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let bytes = std::fs::read(dir.join("one.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

/// Whether `bytes` has `run` in it somewhere.
fn holds(bytes: &[u8], run: &[u8]) -> bool {
    bytes.windows(run.len()).any(|window| window == run)
}

/// `pause; lfence`, the loop a retpoline catches a guessed return in.
const CAUGHT: &[u8] = &[0xf3, 0x90, 0x0f, 0xae, 0xe8];

#[test]
fn an_inline_thunk_is_written_where_the_branch_was() {
    let text = asm(&["-mindirect-branch=thunk-inline", "-mfunction-return=thunk-inline"], CALLS);
    let lines = insts(&text);
    assert!(!lines.iter().any(|line| line.starts_with("call\t*")), "{text}");
    assert!(!text.contains("__x86_indirect_thunk") && !text.contains("__x86_return_thunk"));
    // The three registers the pointers are kept in, each written over the pushed return address.
    for reg in ["%rbx", "%r12", "%r13"] {
        assert!(lines.contains(&format!("mov {reg}, (%rsp)").as_str()), "{reg}: {text}");
    }
    // One return in each of the six calls, and every other one is the return thunk's, which takes
    // the pushed address back off first.
    let rets = lines.iter().filter(|line| **line == "ret").count();
    let backs = lines.iter().filter(|line| **line == "lea 8(%rsp), %rsp").count();
    assert_eq!(rets, 6 + backs, "{text}");
    assert!(backs > 0, "{text}");
    // The object writer reads the same text, labels and all.
    let bytes = object(&["-mindirect-branch=thunk-inline"], CALLS, "inline");
    assert!(holds(&bytes, CAUGHT), "no pause and lfence in the object");
}

#[test]
fn a_unit_built_with_thunk_carries_each_thunk_it_calls() {
    let text = asm(&["-mindirect-branch=thunk", "-mfunction-return=thunk"], CALLS);
    let lines = insts(&text);
    assert!(lines.contains(&"call\t__x86_indirect_thunk_r12"), "{text}");
    assert!(lines.contains(&"jmp\t__x86_return_thunk"), "{text}");
    for name in ["__x86_indirect_thunk_rbx", "__x86_indirect_thunk_r12", "__x86_return_thunk"] {
        let section = format!(".section .text.{name},\"axG\",@progbits,{name},comdat");
        assert!(text.contains(&section), "{name}: {text}");
        assert!(text.contains(&format!(".hidden {name}")), "{name}: {text}");
        assert_eq!(text.matches(&format!("\n{name}:")).count(), 1, "{name}: {text}");
    }
    // Only the ones something here calls.
    assert!(!text.contains("__x86_indirect_thunk_rax:"), "{text}");
    let bytes = object(&["-mindirect-branch=thunk"], CALLS, "comdat");
    assert!(holds(&bytes, b".text.__x86_indirect_thunk_r12\0"), "no section for the thunk");
    assert!(holds(&bytes, CAUGHT), "no pause and lfence in the object");
}
