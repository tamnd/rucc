//! A byte or a half word read from memory into a register is widened on the way in on x86, so the
//! load writes the whole register rather than its low part.
//!
//! tamnd/rucc#1994. Postgres' tuple deforming loop read `attlen` with `movw 4(%rdi), %r12w`,
//! which keeps the rest of `%r12` and so waits on the last value that was in it. gcc and clang
//! read it with `movzwl` or `movswl`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-narrow-loads-{}-{what}", std::process::id()));
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

/// A field of two bytes tested against constants, sums kept at the width they were read at, and
/// bytes moved one at a time. `main` prints what each comes to.
const PROGRAM: &str = r#"
int printf(const char *, ...);

typedef struct { int off; short len; char byval; char align; } Att;

__attribute__((noinline)) long walk(const Att *atts, int n) {
  long off = 0;
  for (int i = 0; i < n; i++) {
    if (atts[i].len == 1) off += atts[i].byval;
    else if (atts[i].len == 2) off += atts[i].align * 3;
    else off += atts[i].len > 0 ? atts[i].len : 4;
  }
  return off;
}

__attribute__((noinline)) short shorts(const short *p, int n) {
  short s = 0;
  for (int i = 0; i < n; i++)
    s += p[i] ^ s;
  return s;
}

__attribute__((noinline)) unsigned char bytes(unsigned char *to, const unsigned char *from, int n) {
  unsigned char x = 0;
  for (int i = 0; i < n; i++) {
    to[i] = from[n - 1 - i];
    x ^= to[i] + x;
  }
  return x;
}

int main(void) {
  Att atts[12];
  short s[12];
  unsigned char from[12], to[12];
  for (int i = 0; i < 12; i++) {
    atts[i].off = i;
    atts[i].len = (short) (i % 5 - 1);
    atts[i].byval = (char) (i * 37);
    atts[i].align = (char) (-i * 11);
    s[i] = (short) (i * 4099 - 20000);
    from[i] = (unsigned char) (i * 29 + 200);
  }
  for (int n = 0; n <= 12; n += 4)
    printf("%d %ld %d %d\n", n, walk(atts, n), shorts(s, n), bytes(to, from, n));
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_narrow_load_widened_on_the_way_in_gives_the_same_answers() {
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

/// No `movb` or `movw` reads memory into a register, on either x86 target at any level.
#[test]
fn no_byte_or_word_load_writes_part_of_a_register() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for target in ["--target=x86_64-unknown-linux-gnu", "--target=i686-unknown-linux-gnu"] {
        for level in ["-O0", "-O2"] {
            let (ok, said) = run(&dir, &[target, level, "-S", "a.c", "-o", "a.s"]);
            assert!(ok, "{target} {level}: {said}");
            let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
            let partial = asm.lines().map(str::trim).find(|line| {
                let Some(args) =
                    line.strip_prefix("movb\t").or_else(|| line.strip_prefix("movw\t"))
                else {
                    return false;
                };
                args.rsplit_once(", ")
                    .is_some_and(|(from, to)| from.ends_with(')') && to.starts_with('%'))
            });
            assert!(partial.is_none(), "{target} {level}: {partial:?} in\n{asm}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
