//! A subscript with a constant added to a narrow index, `a[i - 1]`, reads the address with the
//! constant as its displacement, on x86-64.
//!
//! tamnd/rucc#1994. Postgres reads attributes at `attnum - 1` all through the executor. rucc wrote
//! the subtraction, its own `movslq` and the load, where gcc writes the `movslq` of `attnum` and a
//! load eight bytes back from the index, and shares the `movslq` with any `a[i]` beside it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-subscript-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Three subscripts with a constant in them, and a `main` that asks each about every index of an
/// array, the ends included.
const PROGRAM: &str = r#"
int printf(const char *, ...);

typedef struct { int len; short num; char align, byval; } Attribute;
typedef struct { int natts; Attribute attrs[8]; } Descriptor;

__attribute__((noinline)) long before(long *a, int i) { return a[i - 1]; }

__attribute__((noinline)) long around(long *a, int i) { return a[i + 2] + a[i]; }

__attribute__((noinline)) int width(const Descriptor *desc, short attnum) {
  const Attribute *att = &desc->attrs[attnum - 1];
  return att->len * 4 + att->align;
}

int main(void) {
  long a[12];
  Descriptor d;
  d.natts = 8;
  for (int i = 0; i < 12; i++)
    a[i] = (long) i * 1000003 - 77;
  for (int i = 0; i < 8; i++) {
    d.attrs[i].len = i * 3 - 5;
    d.attrs[i].num = (short) (i + 1);
    d.attrs[i].align = (char) ('c' + i);
    d.attrs[i].byval = (char) (i & 1);
  }
  for (int i = 1; i < 10; i++)
    printf("%ld %ld\n", before(a, i), around(a, i));
  for (short attnum = 1; attnum <= 8; attnum++)
    printf("%d\n", width(&d, attnum));
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_subscript_with_its_constant_in_the_address_gives_the_same_answers() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let mut want = None;
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        let want = want.get_or_insert_with(|| got.clone());
        assert_eq!(&got, want, "{level} printed something -O0 did not");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lines of one function's body at `-O2`, from its label to the next function's.
fn body<'a>(asm: &'a str, name: &str, next: &str) -> Vec<&'a str> {
    let start = asm.find(&format!("\n{name}:")).expect("the function is there");
    let end = asm[start..].find(&format!("\n{next}:")).map_or(asm.len(), |at| start + at);
    asm[start..end].lines().map(str::trim).filter(|line| !line.is_empty()).collect()
}

/// At `-O2` nothing adds to `i` before it is widened, and `a[i]` and `a[i + 2]` share one widening.
#[test]
fn the_constant_of_a_subscript_is_a_displacement() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    let before = body(&asm, "before", "around");
    assert!(before.iter().any(|line| line.contains("-8(%rdi,")), "{before:#?}");
    assert!(
        !before.iter().any(|line| line.starts_with("sub") || line.starts_with("lea")),
        "{before:#?}"
    );
    let around = body(&asm, "around", "width");
    let widened = around.iter().filter(|line| line.starts_with("movslq")).count();
    assert_eq!(widened, 1, "{around:#?}");
    assert!(around.iter().any(|line| line.contains("16(%rdi,")), "{around:#?}");
    assert!(!around.iter().any(|line| line.starts_with("lea")), "{around:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}
