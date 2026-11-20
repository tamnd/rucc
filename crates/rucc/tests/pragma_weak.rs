//! `#pragma weak`, end to end, measured against gcc 13: which names reach the assembler weak,
//! which second names are set, and what is said about a line that is not one.
//!
//! Design: `weak_line` in `crates/rucc-parse/src/weak.rs` and `pragma_weak` in
//! `crates/rucc-sema/src/check/weak.rs`.

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

/// The assembly for source the compiler accepts.
fn asm(input: &str) -> String {
    let out = run(&["-S", "-o", "-"], input);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    String::from_utf8_lossy(&out.stdout).into_owned()
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

/// A name the pragma marks is weak whether the pragma comes before its declaration or after
/// it, a definition is a weak definition, and a tentative one is a weak definition rather than
/// a common block. A name the file never uses, declared or not, is not written at all.
#[test]
fn the_names_the_pragma_marks_are_written_weak() {
    let text = asm("\
#pragma weak early
#pragma weak undecl
extern int early(void);
extern int late(void);
extern int unused(void);
#pragma weak late
#pragma weak unused
int defd(void) { return 1; }
#pragma weak defd
int var;
#pragma weak var
int use(void) { return early() + late() + var; }
");

    for name in ["early", "late", "defd", "var"] {
        assert!(text.contains(&format!("\t.weak\t{name}\n")), "{name}: {text}");
        assert!(!text.contains(&format!("\t.globl\t{name}\n")), "{name}: {text}");
    }
    assert!(!text.contains(".comm\tvar"), "{text}");
    assert!(!text.contains("undecl"), "{text}");
    assert!(!text.contains("unused"), "{text}");
    assert!(text.contains("\t.globl\tuse\n"), "{text}");
}

/// `#pragma weak a = b` makes `a` a weak second name for `b`, whether or not the file declared
/// `a` and whether `b` is a function or an object.
#[test]
fn the_pragma_with_a_target_sets_a_weak_second_name() {
    let text = asm("\
int defd(void) { return 1; }
int obj = 3;
extern int declared(void);
#pragma weak declared = defd
#pragma weak al = defd
#pragma weak alobj = obj
");

    for name in ["declared", "al", "alobj"] {
        assert!(text.contains(&format!("\t.weak\t{name}\n")), "{name}: {text}");
    }
    assert!(text.contains("\t.set\tdeclared,defd\n"), "{text}");
    assert!(text.contains("\t.set\tal,defd\n"), "{text}");
    assert!(text.contains("\t.set\talobj,obj\n"), "{text}");
}

/// A line that names nothing is ignored with a warning at the word, and words after the name
/// are warned about but the name is still marked.
#[test]
fn a_line_that_is_not_one_is_warned_about_at_the_word() {
    let input = "\
#pragma weak
#pragma weak 1
#pragma weak a3 =
#pragma weak (paren)
extern int junk(void);
#pragma weak junk junk2
int use(void) { return junk(); }
";
    let out = run(&["-S", "-o", "-"], input);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        said(&out),
        [
            "1:9: warning: malformed `#pragma weak`, ignored",
            "2:9: warning: malformed `#pragma weak`, ignored",
            "3:9: warning: malformed `#pragma weak`, ignored",
            "4:9: warning: malformed `#pragma weak`, ignored",
            "6:9: warning: junk at end of `#pragma weak`",
        ]
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\t.weak\tjunk\n"), "{text}");
    assert!(!text.contains("a3"), "{text}");
}

/// A name with internal linkage has nobody to lose to, as with the attribute.
#[test]
fn a_static_name_is_refused() {
    let out = run(
        &["-fsyntax-only"],
        "static int s(void) { return 1; }\n#pragma weak s\nint use(void) { return s(); }\n",
    );
    assert!(!out.status.success());
    assert_eq!(said(&out), ["1:12: error: weak declaration of 's' must be public"]);
}

/// A second name for something nothing defines is refused when the file is lowered.
#[test]
fn a_second_name_for_nothing_is_refused() {
    let out = run(&["-S", "-o", "-"], "#pragma weak al2 = nothere\nint x;\n");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("'al2' is aliased to undefined symbol 'nothere'"), "{err}");
}
