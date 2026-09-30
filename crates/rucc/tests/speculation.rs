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
/// and has to be kept in a register the callee saves. Those are `%rbx`, which is below `%r8`, and
/// `%r12` and `%r13`, which are not.
const CALLS: &str = "\
void three(void (*p)(void), void (*q)(void), void (*r)(void)) { p(); q(); r(); p(); q(); r(); }
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
