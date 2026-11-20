//! `#pragma GCC poison`, end to end, measured against gcc 13: which later uses of a name are an
//! error, which are not, and what happens to a name that was a macro.
//!
//! Design: `poison_pragma` and `check_poisoned` in `crates/rucc-pp/src/directive.rs`.

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

/// Each diagnostic about poison, as `line:column: severity: message`, in line order.
fn poison(out: &Output) -> Vec<String> {
    let err = String::from_utf8_lossy(&out.stderr);
    let mut said: Vec<(u32, u32, String)> = err
        .lines()
        .filter_map(|line| line.strip_prefix("<stdin>:"))
        .filter(|line| line.contains("poison"))
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            let row = parts.next()?.parse().ok()?;
            let col = parts.next()?.parse().ok()?;
            let rest = parts.next()?.trim();
            let rest = rest.split(" [").next()?.to_owned();
            Some((row, col, rest))
        })
        .collect();
    said.sort_by_key(|&(row, col, _)| (row, col));
    said.into_iter().map(|(row, col, rest)| format!("{row}:{col}: {rest}")).collect()
}

const INPUT: &str = "\
#define OLD strcpy
#define GONE 1
#pragma GCC poison memcpy strcpy GONE
#pragma GCC poison memcpy
#pragma GCC poison
#pragma GCC poison 42 strcat
#pragma GCC poison \"x\"
char *OLD;
int memcpy;
#define USE memcpy
#ifdef strcpy
#endif
#if defined(memcpy)
#endif
#undef strcpy
#define F(x) x
int F(memcpy);
#if 0
memcpy
#elif 1
#endif
int strcat;
_Pragma(\"GCC poison late\") int late; int late2 = late;
int late;
";

/// Every use the file writes after the pragma is an error, in text, in a macro's body, in a
/// conditional and in the arguments of an invocation, and the rest of the line a `_Pragma` is
/// on counts. What a macro defined before the pragma writes does not, nor does a skipped
/// region, naming a name again, or the names before one that is not an identifier.
#[test]
fn what_gcc_says_about_a_poisoned_name() {
    let out = run(&["-fsyntax-only"], INPUT);
    assert_eq!(
        poison(&out),
        [
            "3:34: warning: poisoning existing macro `GONE`",
            "6:20: error: invalid `#pragma GCC poison` directive",
            "7:20: error: invalid `#pragma GCC poison` directive",
            "9:5: error: attempt to use poisoned `memcpy`",
            "10:13: error: attempt to use poisoned `memcpy`",
            "11:8: error: attempt to use poisoned `strcpy`",
            "13:13: error: attempt to use poisoned `memcpy`",
            "15:8: error: attempt to use poisoned `strcpy`",
            "17:7: error: attempt to use poisoned `memcpy`",
            "23:32: error: attempt to use poisoned `late`",
            "23:50: error: attempt to use poisoned `late`",
            "24:5: error: attempt to use poisoned `late`",
        ]
    );
    assert!(!out.status.success());
}

/// The pragma is the preprocessor's own, so `-E` does not print it, and the warning about a
/// macro answers to no option: `-w` silences it and `-pedantic-errors` leaves it a warning.
#[test]
fn the_pragma_is_consumed_and_its_warning_is_plain() {
    let src = "#define A 1\n#pragma GCC poison A\nint x;\n";
    let out = run(&["-E", "-P"], src);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("pragma"), "{text}");
    assert_eq!(poison(&out), ["2:20: warning: poisoning existing macro `A`"]);
    let out = run(&["-fsyntax-only", "-pedantic-errors"], src);
    assert!(out.status.success());
    let out = run(&["-fsyntax-only", "-w"], src);
    assert_eq!(poison(&out), Vec::<String>::new());
}
