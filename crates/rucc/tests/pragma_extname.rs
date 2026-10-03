//! `#pragma redefine_extname`, end to end, measured against gcc 13: which names the object file
//! gets in place of the ones the file writes, and what is said about a line that is not one.
//!
//! Design: `extname_line` in `crates/rucc-parse/src/extname.rs` and `pragma_extname` in
//! `crates/rucc-sema/src/check/extname.rs`.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

fn run(flags: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=x86_64-unknown-linux-gnu", "-std=gnu17"])
        .args(flags)
        .args(["-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the input into");
    stdin.write_all(input.as_bytes()).expect("the input can be written");
    drop(stdin);
    child.wait_with_output().expect("the compiler finished")
}

/// Each diagnostic, as `line:column: severity: message`, in line order.
fn said(out: &Output) -> Vec<String> {
    let err = String::from_utf8_lossy(&out.stderr);
    let mut said: Vec<(u32, u32, String)> = err
        .lines()
        .filter_map(|line| line.strip_prefix("<stdin>:"))
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            let row = parts.next()?.parse().ok()?;
            let col = parts.next()?.parse().ok()?;
            let rest = parts.next()?.trim();
            let rest = rest.split(" [").next()?.to_owned();
            Some((row, col, rest))
        })
        .filter(|(_, _, rest)| !rest.starts_with("note:"))
        .collect();
    said.sort_by_key(|&(row, col, _)| (row, col));
    said.into_iter().map(|(row, col, rest)| format!("{row}:{col}: {rest}")).collect()
}

/// The assembly and the diagnostics for source the compiler accepts.
fn asm(input: &str) -> (String, Vec<String>) {
    let out = run(&["-S", "-o", "-"], input);
    assert!(out.status.success(), "the compiler refused the fixture:\n{}", {
        String::from_utf8_lossy(&out.stderr)
    });
    (String::from_utf8_lossy(&out.stdout).into_owned(), said(&out))
}

/// A name with external linkage gets the new name wherever the line stands, before the
/// declaration or after it, after a call was already written, and through a block-scope
/// `extern`. One with internal linkage keeps its own without a word, and one that already has
/// another assembler name keeps that one with a warning.
#[test]
fn the_names_the_pragma_names_are_renamed() {
    let (text, said) = asm("\
#pragma redefine_extname before newb
extern int before(void);
extern int after(void);
#pragma redefine_extname after newa
int var;
#pragma redefine_extname var newv
#pragma redefine_extname never newn
extern int lab(void) __asm__(\"explicit\");
#pragma redefine_extname lab newl
extern int same(void) __asm__(\"tgt\");
#pragma redefine_extname same tgt
static int st(void) { return 2; }
#pragma redefine_extname st news
extern int used(void);
int u1(void) { return used(); }
#pragma redefine_extname used nused
void blk(void) { extern int inner(void); inner(); }
#pragma redefine_extname inner ninner
int iv = 1;
#pragma redefine_extname iv niv
extern int h(void);
#pragma redefine_extname h k
int h(void) { return 3; }
int use(void) { return before() + after() + var + lab() + same() + st() + iv; }
");

    assert_eq!(
        said,
        ["9:9: warning: `#pragma redefine_extname` ignored due to conflict with previous rename"]
    );
    for call in ["newb", "newa", "explicit", "tgt", "st", "nused", "ninner"] {
        assert!(text.contains(&format!("\tcall\t{call}")), "{call}: {text}");
    }
    for name in ["newv", "niv", "k"] {
        assert!(text.contains(&format!("\t.globl\t{name}\n")), "{name}: {text}");
    }
    for gone in
        ["before", "after", "\tvar", "\tused", "\tinner", "\tiv", "\th\n", "newn", "newl", "news"]
    {
        assert!(!text.contains(gone), "{gone:?}: {text}");
    }
}

/// A line that is not two names is ignored with a warning at the word, words after the two
/// are warned about but the line is still applied, and a second line for a name already
/// renamed to something else is ignored. The same line twice is not a conflict.
#[test]
fn a_line_that_is_not_one_is_warned_about_at_the_word() {
    let (text, said) = asm("\
#pragma redefine_extname
#pragma redefine_extname one
#pragma redefine_extname 1 2
#pragma redefine_extname a b c
#pragma redefine_extname x y
#pragma redefine_extname x z
#pragma redefine_extname q r
#pragma redefine_extname q r
extern int a(void); extern int x(void); extern int q(void);
int u(void) { return a() + x() + q(); }
");

    assert_eq!(
        said,
        [
            "1:9: warning: malformed `#pragma redefine_extname`, ignored",
            "2:9: warning: malformed `#pragma redefine_extname`, ignored",
            "3:9: warning: malformed `#pragma redefine_extname`, ignored",
            "4:9: warning: junk at end of `#pragma redefine_extname`",
            "6:9: warning: `#pragma redefine_extname` ignored due to conflict with previous \
             `#pragma redefine_extname`",
        ]
    );
    for call in ["b", "y", "r"] {
        assert!(text.contains(&format!("\tcall\t{call}")), "{call}: {text}");
    }
}

/// A header asks whether the pragma is read before it writes one, as gcc lets it.
#[test]
fn the_pragma_is_announced() {
    let out = run(&["-dM", "-E"], "");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("#define __PRAGMA_REDEFINE_EXTNAME 1\n"), "{text}");
}
