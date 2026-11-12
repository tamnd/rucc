//! The type generic bit counts of gcc 14, `__builtin_clzg` and its five siblings, which count at
//! the width of the integer they are given rather than at the width of a parameter.
//!
//! tamnd/rucc#4514. rucc said no to `__has_builtin(__builtin_clzg)` where gcc 14 says yes, so a
//! header that asks first took its fallback, and one that did not ask failed to compile. Every
//! number below is what gcc 15 prints for the same program on x86-64.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-generic-counts-{}-{what}", std::process::id()));
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

/// Each of the six at widths of 8, 16, 32, 37, 64 and 128 bits, read through `volatile` so that
/// the count is made when the program runs, and a few that must fold where a constant is wanted.
const PROGRAM: &str = r#"
int printf(const char *, ...);
#define P(e) printf("%lld\n", (long long)(e))

static int n;
static int nine(void) { n++; return 9; }

_Static_assert(__builtin_clzg((unsigned char)1) == 7, "");
_Static_assert(__builtin_clzg((unsigned __int128)1) == 127, "");
_Static_assert(__builtin_clzg(0u, 5) == 5, "");
_Static_assert(__builtin_popcountg((unsigned _BitInt(100))-1) == 100, "");
_Static_assert(__builtin_clrsbg((signed char)-1) == 7, "");
_Static_assert(__builtin_ffsg((__int128)1 << 90) == 91, "");

int main(void) {
  volatile unsigned char uc = 1;
  volatile unsigned short us = 3;
  volatile unsigned u = 0x80;
  volatile unsigned long ul = 1ul << 40;
  volatile unsigned __int128 q = (unsigned __int128)1 << 100;
  volatile unsigned __int128 q0 = 5;
  volatile signed char sc = -2;
  volatile __int128 sq = -1;
  volatile unsigned _BitInt(37) b = 3;
  volatile _BitInt(37) sb = -3;
  volatile unsigned z = 0;
  volatile unsigned char zc = 0;
  volatile unsigned x = 4;
  P(__builtin_clzg(uc)); P(__builtin_clzg(us)); P(__builtin_clzg(u)); P(__builtin_clzg(ul));
  P(__builtin_clzg(q)); P(__builtin_clzg(q0)); P(__builtin_clzg(b)); P(__builtin_clzg(z, -7));
  P(__builtin_ctzg(uc)); P(__builtin_ctzg(q)); P(__builtin_ctzg(b)); P(__builtin_ctzg(zc, 99));
  P(__builtin_popcountg(q)); P(__builtin_popcountg((unsigned char)255));
  P(__builtin_popcountg(b));
  P(__builtin_parityg(us)); P(__builtin_parityg(q0));
  P(__builtin_ffsg(sc)); P(__builtin_ffsg(sq)); P(__builtin_ffsg((int)z)); P(__builtin_ffsg(sb));
  P(__builtin_clrsbg(sc)); P(__builtin_clrsbg(sq)); P(__builtin_clrsbg((short)1));
  P(__builtin_clrsbg(sb)); P(__builtin_clrsbg((__int128)z));
  P(__builtin_ffsg((__int128)1 << 90));
  int r = __builtin_clzg(x, nine());
  printf("%d %d\n", r, n);
  return 0;
}
"#;

/// What gcc 15 prints for [`PROGRAM`], at every level.
const PRINTED: &str = "7\n14\n24\n23\n27\n125\n35\n-7\n0\n100\n0\n99\n1\n8\n2\n0\n0\n2\n1\n0\n1\n\
                       6\n127\n14\n34\n127\n91\n29 1\n";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_type_generic_count_counts_at_the_width_of_its_argument() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O2"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        assert_eq!(String::from_utf8_lossy(&out.stdout), PRINTED, "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Calls gcc refuses, each with the words gcc refuses it in.
const WRONG: &str = r#"
enum e { A };
struct s { int x; };
int f(unsigned x, long l, char c, unsigned u) {
  int r = 0;
  r += __builtin_clzg(1);
  r += __builtin_clzg(1u, 2, 3);
  r += __builtin_clzg();
  r += __builtin_clzg(1.0);
  r += __builtin_clzg((_Bool)1);
  r += __builtin_clzg((enum e)0);
  r += __builtin_clzg(1u, 2.0);
  r += __builtin_ffsg(1u);
  r += __builtin_popcountg(1u, 2);
  r += __builtin_ctzg((void *)0);
  r += __builtin_clzg(x, 5u);
  r += __builtin_clzg(x, l);
  r += __builtin_clzg(x, c);
  r += __builtin_ctzg(x, (short)1);
  r += __builtin_popcountg((unsigned _BitInt(129))1);
  return r;
}
"#;

#[test]
fn a_type_generic_count_says_what_gcc_says_of_a_wrong_argument() {
    let dir = dir("wrong");
    std::fs::write(dir.join("a.c"), WRONG).expect("the fixture can be written");
    let (ok, said) = run(&dir, &["--target=x86_64-unknown-linux-gnu", "-fsyntax-only", "a.c"]);
    assert!(!ok, "{said}");
    let clzg = "in call to function '__builtin_clzg'";
    for want in [
        format!("6:23: error: argument 1 {clzg} has signed type"),
        "7:8: error: too many arguments to function '__builtin_clzg'".to_string(),
        "8:8: error: too few arguments to function '__builtin_clzg'".to_string(),
        format!("9:23: error: argument 1 {clzg} does not have integral type"),
        format!("10:23: error: argument 1 {clzg} has boolean type"),
        format!("11:23: error: argument 1 {clzg} has enumerated type"),
        format!("12:27: error: argument 2 {clzg} does not have integral type"),
        "argument 1 in call to function '__builtin_ffsg' has unsigned type".to_string(),
        "too many arguments to function '__builtin_popcountg'".to_string(),
        "argument 1 in call to function '__builtin_ctzg' does not have integral type".to_string(),
        format!("16:26: error: argument 2 {clzg} does not have 'int' type"),
        format!("17:26: error: argument 2 {clzg} does not have 'int' type"),
        "argument 1 in call to function '__builtin_popcountg' is wider than 128 bits".to_string(),
    ] {
        assert!(said.contains(&want), "{want}\n{said}");
    }
    // A `char` and a `short` promote to `int`, which is all the second argument has to be.
    for line in ["18:", "19:"] {
        assert!(!said.contains(&format!("a.c:{line}")), "{line}\n{said}");
    }
    assert_eq!(said.matches("error:").count(), 13, "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}
