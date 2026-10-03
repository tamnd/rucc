//! `deprecated` on a typedef, a tag, a member and an enumerator, end to end, measured against
//! gcc 13. Functions and objects are in `gnu_matrix.rs`.
//!
//! Design: `crates/rucc-sema/src/check/advice.rs`.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

fn run(flags: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=x86_64-unknown-linux-gnu", "-std=gnu2x", "-S", "-o", "-"])
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

/// Every kind of name that is not a declaration, marked in each spelling, and used once each.
const EVERY_KIND: &str = "\
typedef int T __attribute__((deprecated));
typedef int U [[deprecated(\"use int\")]];
struct __attribute__((deprecated)) S { int a; };
union [[gnu::deprecated]] W { int a; };
struct P { int old __attribute__((deprecated)); [[deprecated(\"why\")]] int c23; int b; };
struct Q { struct { int deep __attribute__((deprecated)); }; };
enum { E1 __attribute__((deprecated)) = 1, E2 [[deprecated]], E3 };
enum __attribute__((deprecated)) G { G1 };
int use(struct P *p, struct Q *q) {
  T t = 0;
  U u = 0;
  struct S s;
  union W w;
  enum G g = G1;
  return t + u + p->old + p->c23 + p->b + q->deep + E1 + E2 + E3 + s.a + w.a + g + (int)sizeof s;
}
";

/// The lines of what the compiler said that are warnings, with the position taken off.
fn warnings(said: &str) -> Vec<String> {
    said.lines()
        .filter_map(|line| line.split_once(": warning: ").map(|(_, rest)| rest.to_owned()))
        .collect()
}

#[test]
fn a_use_of_a_deprecated_type_tag_member_or_enumerator_is_warned_about_the_way_gcc_does() {
    let out = run(&[], EVERY_KIND);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    let expected = [
        "'T' is deprecated [E0770]",
        "'U' is deprecated: use int [E0770]",
        "'S' is deprecated [E0770]",
        "'W' is deprecated [E0770]",
        "'G' is deprecated [E0770]",
        "'old' is deprecated [E0770]",
        "'c23' is deprecated: why [E0770]",
        "'deep' is deprecated [E0770]",
        "'E1' is deprecated [E0770]",
        "'E2' is deprecated [E0770]",
    ];
    assert_eq!(warnings(&said), expected, "{said}");
    // A typedef gets no note: eight notes for ten warnings.
    assert_eq!(said.matches("note: declared here").count(), 8, "{said}");
}

#[test]
fn a_deprecated_member_answers_to_wno_deprecated_declarations() {
    let source = "struct P { int old __attribute__((deprecated)); };\n\
                  int g(struct P *p) { return p->old; }\n";
    let quiet = run(&["-Werror", "-Wno-deprecated-declarations"], source);
    assert!(quiet.status.success(), "{}", String::from_utf8_lossy(&quiet.stderr));
    assert!(quiet.stderr.is_empty(), "{}", String::from_utf8_lossy(&quiet.stderr));
    let fatal = run(&["-Werror=deprecated-declarations"], source);
    assert!(!fatal.status.success());
    assert!(String::from_utf8_lossy(&fatal.stderr).contains("error: 'old' is deprecated"));
}
