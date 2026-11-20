//! A call `main` makes outside its loops is not hot, since `main` runs once, and gcc inlines a
//! call that is not hot only when that does not make the program larger (tamnd/rucc#3224).
//!
//! The shapes are the setter of a configuration table, from the `bundle` facet of rucc-corpus,
//! where `main` calls `SetConfigOption` four times and gcc 16 keeps the four calls at `-O2` and
//! `-O3`, a lookup small enough that gcc copies it into `main` anyway, and a helper that is one add
//! once its constants are put together.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-main-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source at `-O2`.
fn compiled(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "-S", "-o", "-"])
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

/// The setter, with `main` written by the caller of this.
const SET: &str = r#"
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
"#;

/// Each copy of the setter grows `main` by more than `early-inlining-insns`, and four copies grow
/// the program by more than the one body they let go, so the four calls stay. gcc 16 says "call
/// is cold and code would grow" of each.
#[test]
fn a_setter_main_calls_four_times_stays_a_call() {
    let source = format!(
        "{SET}int main(void)\n{{\n  gucs[0] = (struct guc){{\"a\", &a, 0, 100}};\n  ngucs = 1;\n  \
         printf(\"%d\\n\", set(gucs[0].name, 150));\n  printf(\"%d\\n\", set(gucs[0].name, 3));\n  \
         printf(\"%d\\n\", set(\"c\", 3));\n  printf(\"%d\\n\", set(gucs[0].name, 30));\n  \
         return a + b;\n}}\n"
    );
    assert_eq!(calls(&compiled("set", &source), "set"), 4);
}

/// The same four calls from a loop in `main` are hot, since the loop runs them many times, and go
/// in as they would from any other function.
#[test]
fn the_setter_called_from_a_loop_in_main_is_copied() {
    let source = format!(
        "{SET}int main(void)\n{{\n  gucs[0] = (struct guc){{\"a\", &a, 0, 100}};\n  ngucs = 1;\n  \
         for (int i = 0; i < 100; i++) {{\n    b += set(gucs[0].name, i);\n    \
         b += set(gucs[0].name, i + 1);\n    b += set(\"c\", i);\n    \
         b += set(gucs[0].name, 2 * i);\n  }}\n  return a + b;\n}}\n"
    );
    assert_eq!(calls(&compiled("loop", &source), "set"), 0);
}

/// The same setter called from a function other than `main` is copied, which is what the first
/// pass did with the calls in `main` before.
#[test]
fn the_setter_called_from_another_function_is_copied() {
    let source = format!(
        "{SET}int setup(void)\n{{\n  gucs[0] = (struct guc){{\"a\", &a, 0, 100}};\n  ngucs = 1;\n  \
         return set(gucs[0].name, 150) + set(gucs[0].name, 3) + set(\"c\", 3)\n    + \
         set(gucs[0].name, 30);\n}}\n"
    );
    assert_eq!(calls(&compiled("setup", &source), "set"), 0);
}

/// A body that grows `main` by no more than `early-inlining-insns` goes in all the same. gcc 16
/// copies this lookup into each of the four calls.
#[test]
fn a_lookup_small_enough_still_goes_into_main() {
    let source = "int printf(const char *, ...);\n\
                  static int tab[16] = {3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3};\n\
                  static int pick(int key, int lo)\n{\n  for (int i = 0; i < 16; i++)\n    \
                  if (tab[i] == key)\n      return i < lo ? lo : i;\n  return -1;\n}\n\
                  int main(void)\n{\n  printf(\"%d\\n\", pick(5, 2));\n  \
                  printf(\"%d\\n\", pick(9, 1));\n  printf(\"%d\\n\", pick(7, 3));\n  \
                  printf(\"%d\\n\", pick(4, 0));\n  return 0;\n}\n";
    assert_eq!(calls(&compiled("pick", source), "pick"), 0);
}

/// Sixteen lines that each add a constant are one add once gcc's early passes put the constants
/// together, which they do before the early inliner weighs the body, so gcc copies the helper into
/// all four calls. The `inline` facet of rucc-corpus has this shape.
#[test]
fn a_chain_of_constant_adds_goes_into_main() {
    let mut source =
        String::from("int printf(const char *, ...);\nstatic int helper(int value)\n{\n");
    for step in 1..=16 {
        let _ = writeln!(source, "  value = value + {step};");
    }
    source.push_str(
        "  return value;\n}\nstatic volatile int seed_in;\nint main(void)\n{\n  int seed = seed_in;\n  \
         int total = helper(seed) + helper(seed + 1) + helper(seed + 2) + helper(seed + 3);\n  \
         printf(\"%d\\n\", total);\n  return 0;\n}\n",
    );
    assert_eq!(calls(&compiled("chain", &source), "helper"), 0);
}
