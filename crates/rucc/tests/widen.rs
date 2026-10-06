//! A loop counting in `int` and indexing arrays with the count reads them off a sixty four bit
//! twin of the counter on x86-64 above `-O1`, the way it would with a `long` counter.
//!
//! tamnd/rucc#1994. The loop that deforms a Postgres tuple counts its attributes in an `int` and
//! indexes two arrays with the count. Every turn sign extended the counter again, and the cost of
//! that pushed `ivopts` to walk a pointer per array beside the counter, which ran the loop out of
//! registers. gcc and clang widen the counter once.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-widen-{}-{what}", std::process::id()));
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

/// Loops counting in `int`, `unsigned`, `short` and `int` again from a start the caller hands in,
/// each indexing more than one array. `main` prints what they wrote over a few lengths, negative
/// and empty ones among them.
const PROGRAM: &str = r#"
int printf(const char *, ...);

__attribute__((noinline)) void copy(long *restrict v, char *restrict z, const long *a, int n) {
  for (int i = 0; i < n; i++) { v[i] = a[i] + i; z[i] = (char) i; }
}

__attribute__((noinline)) long from(const long *a, const int *b, int at, int n) {
  long s = 0;
  for (int i = at; i < n; i++) s += a[i] * b[i] + i;
  return s;
}

__attribute__((noinline)) long down(const long *a, int n) {
  long s = 0;
  for (int i = n - 1; i >= 0; i--) s = s * 3 + a[i] - i;
  return s;
}

__attribute__((noinline)) unsigned uns(const unsigned *a, unsigned *b, unsigned n) {
  unsigned s = 0;
  for (unsigned i = 0; i < n; i++) { b[i] = a[i] ^ i; s += b[i]; }
  return s;
}

__attribute__((noinline)) long shorts(const long *a, short n) {
  long s = 0;
  for (short i = 0; i < n; i += 2) s += a[i] + a[i + 1] + i;
  return s;
}

int main(void) {
  long a[64], v[64];
  int b[64];
  unsigned ua[64], ub[64];
  char z[64];
  for (int i = 0; i < 64; i++) {
    a[i] = (long) i * 2654435761L ^ 977;
    b[i] = i * 7 - 100;
    ua[i] = (unsigned) i * 40503u;
  }
  for (int n = -3; n <= 60; n += 7) {
    for (int i = 0; i < 64; i++) { v[i] = 0; z[i] = 0; }
    copy(v, z, a, n);
    long t = 0;
    for (int i = 0; i < 64; i++) t = t * 31 + v[i] + z[i];
    printf("%d %ld %ld %ld %u %ld\n", n, t, from(a, b, n / 3, n), down(a, n),
           uns(ua, ub, n < 0 ? 0 : n), shorts(a, (short) n));
  }
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_counter_read_off_its_twin_gives_the_same_answers() {
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

/// The lines of the loop in a function: from the label a jump goes back to, to that jump.
fn the_loop<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    let start = asm.find(&format!("\n{name}:")).expect("the function is there");
    let end = asm[start..].find(".size").map_or(asm.len(), |at| start + at);
    let lines: Vec<&str> = asm[start..end].lines().map(str::trim).collect();
    for (at, line) in lines.iter().enumerate() {
        let Some(target) = line.strip_prefix('j').and_then(|rest| rest.split_whitespace().nth(1))
        else {
            continue;
        };
        let label = format!("{target}:");
        if let Some(top) = lines[..at].iter().position(|line| *line == label) {
            return lines[top..=at].to_vec();
        }
    }
    panic!("no loop in\n{}", &asm[start..end]);
}

/// At `-O2` nothing in the loop sign extends the counter, and the arrays are read with a scaled
/// index.
#[test]
fn nothing_in_the_loop_widens_the_counter() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    for name in ["copy", "from"] {
        let body = the_loop(&asm, name);
        let text = body.join("\n");
        assert!(!body.iter().any(|line| line.starts_with("movslq")), "{name}:\n{text}");
        assert!(body.iter().any(|line| line.contains(",8)")), "{name}:\n{text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
