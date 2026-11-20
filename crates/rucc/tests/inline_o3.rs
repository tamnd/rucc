//! The limits gcc raises for the inliner at `-O3`, which are `max-inline-insns-auto` from 15 to
//! 30 and `early-inlining-insns` from 6 to 14 (tamnd/rucc#3174).
//!
//! The first two shapes are a `static` function nobody declared `inline`, called from three places.
//! gcc 16 keeps the three calls at `-O2` and copies the body into each caller at `-O3`, and so does
//! rucc. The bodies are sized to sit inside the range where both compilers do that, which is
//! narrower for rucc since it counts IR instructions rather than gcc's estimate. A body of straight
//! line arithmetic is no use for the first limit, since at `-O3` the copy saves enough of the time
//! for the second pass to take it whatever the limit. The third is a setter that `main` calls four
//! times, which the early limit of `-O3` lets in.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-o3-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source with those flags.
fn compiled(what: &str, flags: &[&str], source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(flags)
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// How many calls to `name` are left, a call in tail position being a jump.
fn calls(listing: &str, name: &str) -> usize {
    listing
        .lines()
        .filter(|line| {
            let line = line.trim();
            (line.starts_with("call") || line.starts_with("jmp"))
                && line.split_whitespace().nth(1) == Some(name)
        })
        .count()
}

/// `mix` with a loop of `inside` lines of arithmetic and `after` lines after it, called from three
/// functions, each written with that attribute in front of it.
fn source(inside: usize, after: usize, attribute: &str) -> String {
    let mut source = String::from("static int mix(int x, int y)\n{\n");
    if inside > 0 {
        source.push_str("  for (int i = 0; i < y; i++) {\n");
        for line in 1..=inside {
            let _ = writeln!(source, "    x = x * {} + (i >> {});", line + 2, line % 5 + 1);
        }
        source.push_str("  }\n");
    }
    for line in 1..=after {
        let _ = writeln!(source, "  x = x * {} + (y >> {});", line + 2, line % 5 + 1);
    }
    source.push_str("  return x;\n}\n");
    for (name, call, then) in [("f", "x, y", "+ 1"), ("g", "y, x", "- 1"), ("h", "x + y, x", "^ 3")]
    {
        let _ = writeln!(
            source,
            "{attribute}int {name}(int x, int y) {{ return mix({call}) {then}; }}"
        );
    }
    source
}

/// A loop of eight lines grows each caller by more than 15 and by less than 30, and the loop is
/// where the time goes, so copying it saves too little of the time for that to count. It is copied
/// at `-O3` only. gcc 16 does the same for a loop of 6 to 12 lines.
#[test]
fn a_function_nobody_declared_inline_is_copied_at_o3_and_not_at_o2() {
    let source = source(8, 0, "");
    assert_eq!(calls(&compiled("auto-o2", &["-O2"], &source), "mix"), 3);
    assert_eq!(calls(&compiled("auto-o3", &["-O3"], &source), "mix"), 0);
    // `--param` with gcc's name moves the row of both levels.
    let moved = compiled("auto-o3-param", &["-O3", "--param=max-inline-insns-auto=15"], &source);
    assert_eq!(calls(&moved, "mix"), 3);
}

/// A call from a `cold` function is copied only when it grows the caller by no more than
/// `early-inlining-insns`, so five lines are copied at `-O3` and not at `-O2`. gcc 16 does the same
/// for a body of 4 to 6 lines.
#[test]
fn a_call_from_a_cold_function_takes_the_larger_early_limit_at_o3() {
    let source = source(0, 5, "__attribute__((cold)) ");
    assert_eq!(calls(&compiled("early-o2", &["-O2"], &source), "mix"), 3);
    assert_eq!(calls(&compiled("early-o3", &["-O3"], &source), "mix"), 0);
    let moved = compiled("early-o3-param", &["-O3", "--param=early-inlining-insns=6"], &source);
    assert_eq!(calls(&moved, "mix"), 3);
}

/// The setter of a configuration table that `main` calls four times, from the `bundle` facet of
/// rucc-corpus. A call `main` makes outside its loops is not hot, so it is copied only when it
/// grows `main` by no more than `early-inlining-insns` (tamnd/rucc#3224), which the setter does
/// with the limit of `-O3` and not with that of `-O2`. gcc 16 keeps the four calls at `-O2` and
/// copies all four at `-O3`.
#[test]
fn a_setter_main_calls_four_times_is_copied_at_o3_only() {
    let source = r#"
struct guc { const char *name; int *addr; int min, max; };
static struct guc gucs[8];
static int ngucs;
static int a, b;
int printf(const char *, ...);
static int set(const char *name, int value)
{
  for (int i = 0; i < ngucs; i++) {
    if (gucs[i].name == name) {
      if (value > gucs[i].max)
        value = gucs[i].max;
      *gucs[i].addr = value;
      return 1;
    }
  }
  return 0;
}
int main(void)
{
  gucs[0] = (struct guc){"a", &a, 0, 100};
  gucs[1] = (struct guc){"b", &b, 0, 10};
  ngucs = 2;
  printf("%d\n", set(gucs[0].name, 150));
  printf("%d\n", set(gucs[1].name, 3));
  printf("%d\n", set("c", 3));
  printf("%d\n", set(gucs[1].name, 30));
  printf("%d %d\n", a, b);
  return 0;
}
"#;
    assert_eq!(calls(&compiled("set-o2", &["-O2"], source), "set"), 4);
    assert_eq!(calls(&compiled("set-o3", &["-O3"], source), "set"), 0);
}
