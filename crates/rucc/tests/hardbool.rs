//! `__attribute__((hardbool))`, end to end: gcc 14's hardened booleans, which are stored as two
//! numbers of the program's choosing, read as the `bool` they stand for, and stop the program
//! where the object holds anything else, and what gcc says about the attribute where it is wrong.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A directory of this test's own, empty. The tests in this file run on threads of one process,
/// so the process id alone would give two of them the same one.
fn dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-hardbool-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// Whether the compiler agreed, and what it said, for `source` in `dir`.
fn compile(dir: &Path, flags: &[&str], source: &str) -> (bool, String) {
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .arg("a.c")
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// What the compiler said about `source`, and whether it agreed, with `-fsyntax-only`.
fn said(source: &str) -> (bool, String) {
    let dir = dir();
    let said = compile(&dir, &["--target=x86_64-unknown-linux-gnu", "-fsyntax-only"], source);
    let _ = std::fs::remove_dir_all(&dir);
    said
}

/// Representations written and read back, as a variable, a global, an array, a parameter, a
/// return value and a bit-field narrower than its type, through `++`, `--`, compound assignment
/// and casts, with constants folded and a `_Generic` choosing by the type itself. What it prints
/// is what gcc 16's build of it prints. Run with `i` or `b` it puts a number that is neither
/// representation in an object and reads it, and gcc stops it with a trap there.
const PROGRAM: &str = r#"
int printf(const char *, ...);
void *memcpy(void *, const void *, unsigned long);

typedef int __attribute__((hardbool)) hint;
typedef unsigned char __attribute__((hardbool(0x5a, 0xa5))) hbyte;
typedef signed char __attribute__((hardbool(1))) hsc;
struct S { hbyte b : 4; int pad; hint i; };

_Static_assert(sizeof(hint) == sizeof(int) && sizeof(hbyte) == 1, "the integer's size");
_Static_assert((hbyte)5 && !(hbyte)0 && (int)(hint)7 == 1, "a bool as a constant");
_Static_assert(_Generic((hbyte)0, hbyte: 1, default: 0), "chosen by its own type");

static hint gt = 1, gf = 0;
static hbyte bt = 7;
hsc scs[2] = {0, 1};

__attribute__((noinline)) hbyte flip(hbyte b) { return !b; }
__attribute__((noinline)) int raw(const void *p, int n) {
  unsigned char c[4] = {0};
  memcpy(c, p, n);
  return c[0] | c[1] << 8 | c[2] << 16 | c[3] << 24;
}
__attribute__((noinline)) int truth(hint h) { return h ? 7 : 3; }

int main(int argc, char **argv) {
  hint i = 42;
  hbyte b = 0;
  hsc s = 1;
  printf("%d %d %d\n", raw(&i, 4), raw(&b, 1), raw(&s, 1));
  printf("%d %d %d %d\n", raw(&gt, 4), raw(&gf, 4), raw(&bt, 1), raw(scs, 1) | raw(scs + 1, 1) << 8);
  printf("%d %d %d %d\n", i == 1, b, s + 1, (int)sizeof(i + 0));
  b = flip(b);
  printf("%d %d\n", b, raw(&b, 1));
  b--;
  printf("%d %d\n", b, raw(&b, 1));
  b++;
  b++;
  printf("%d %d\n", b, raw(&b, 1));
  i += 2;
  printf("%d %d\n", i, raw(&i, 4));
  i -= 1;
  printf("%d %d\n", i, raw(&i, 4));
  i ^= 1;
  printf("%d %d %d\n", i, raw(&i, 4), truth(b) + truth(0));
  hbyte c = (hbyte)2;
  hint j = (hint)b;
  printf("%d %d %d %d\n", raw(&c, 1), (int)c, raw(&j, 4), (int)(unsigned char)c);
  struct S st = {0};
  printf("%d %d %d\n", st.b, raw(&st, 1) & 15, st.i);
  st.b = 1;
  printf("%d %d\n", st.b, raw(&st, 1) & 15);
  st.b--;
  printf("%d %d\n", st.b, raw(&st, 1) & 15);
  int sum = (st.b = 9) + 1;
  printf("%d %d %d\n", sum, st.b, raw(&st, 1) & 15);
  if (argc > 1 && argv[1][0] == 'i') {
    int five = 5;
    memcpy(&i, &five, 4);
    printf("%d\n", i ? 1 : 2);
  }
  if (argc > 1 && argv[1][0] == 'b') {
    unsigned char bad = 0x5b;
    memcpy(&b, &bad, 1);
    b++;
    printf("%d\n", raw(&b, 1));
    b--;
    printf("unreached\n");
  }
  return 0;
}
"#;

const PRINTS: &str = "\
-1 90 254
-1 0 165 65025
1 0 2 4
1 165
0 90
1 165
1 -1
0 0
1 -1 10
165 1 -1 1
0 10 0
1 5
0 10
2 1 5
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_hardened_bool_is_stored_as_its_two_numbers_and_traps_on_anything_else() {
    use std::os::unix::process::ExitStatusExt;
    let dir = dir();
    for level in ["-O0", "-O2"] {
        let (ok, said) = compile(&dir, &[level, "-o", "prog"], PROGRAM);
        assert!(ok, "{level}: {said}");
        assert_eq!(said, "", "{level}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        assert_eq!(String::from_utf8_lossy(&out.stdout), PRINTS, "{level}");
        for corrupt in ["i", "b"] {
            let out = Command::new(dir.join("prog"))
                .arg(corrupt)
                .output()
                .expect("what was linked can be run");
            // `ud2`, which is what gcc's `__builtin_trap` is on x86-64.
            assert_eq!(out.status.signal(), Some(4), "{level} {corrupt}: {:?}", out.status);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// What gcc 16 says about the attribute where it is wrong, which is every one of these but the
/// argument that is not a constant: gcc stops with an internal error on that one.
#[test]
fn hardbool_is_checked_in_gcc_s_words() {
    let integral = "error: 'hardbool' attribute only supported on integral types";
    let different = "error: 'hardbool' attribute requires different values for 'false' and 'true'";
    for (source, wanted, errors, warnings) in [
        (
            "int __attribute__((hardbool(0, 1, 2))) a;\n",
            "error: wrong number of arguments specified for 'hardbool' attribute",
            1,
            0,
        ),
        ("int __attribute__((hardbool(0, 1, 2))) a;\n", "expected between 0 and 2, found 3", 1, 0),
        ("_Bool __attribute__((hardbool)) a;\n", integral, 1, 0),
        ("float __attribute__((hardbool)) a;\n", integral, 1, 0),
        ("int *a __attribute__((hardbool));\n", integral, 1, 0),
        ("typedef enum E { X } e;\ne __attribute__((hardbool)) a;\n", integral, 1, 0),
        ("_BitInt(8) __attribute__((hardbool)) a;\n", integral, 1, 0),
        ("struct S { int __attribute__((hardbool(2))) m; };\n", "", 0, 0),
        ("void f(int __attribute__((hardbool)) p);\n", "", 0, 0),
        ("typedef int __attribute__((hardbool)) h;\nh __attribute__((hardbool)) a;\n", integral, 1, 0),
        ("int __attribute__((hardbool(1, 1))) a;\n", &format!("{different} for type 'int'"), 1, 0),
        (
            "signed char __attribute__((hardbool(1445))) a;\n",
            "warning: overflows in conversion from 'int' to 'signed char' changes value from \
             '1445' to '-91'",
            0,
            1,
        ),
        ("unsigned char __attribute__((hardbool(1445))) a;\n", "", 0, 0),
        (
            "signed char __attribute__((hardbool(0, 0x100000000))) a;\n",
            "warning: overflows in conversion from 'long' to 'signed char' changes value from \
             '4294967296' to '0'",
            1,
            1,
        ),
        ("enum E { X, Y };\nint __attribute__((hardbool(X, Y))) a;\n", "", 0, 0),
        (
            "int x;\nint __attribute__((hardbool(x))) a;\n",
            "error: 'hardbool' attribute argument is not an integer constant",
            1,
            0,
        ),
        (
            "typedef unsigned char __attribute__((hardbool)) hb;\n\
             typedef hb v __attribute__((vector_size(16)));\n",
            "error: invalid vector type for attribute 'vector_size'",
            1,
            0,
        ),
        (
            "typedef signed char __attribute__((hardbool(90))) hs;\nstruct S { hs b : 3; };\n",
            "warning: overflows in conversion from 'int' to 'enum <anonymous>' changes value from \
             '90' to '2'",
            0,
            1,
        ),
        (
            "typedef unsigned char __attribute__((hardbool(1, 3))) hb;\nstruct T { hb b : 1; };\n",
            &format!("{different} for type 'enum <anonymous>'"),
            1,
            0,
        ),
    ] {
        let (ok, err) = said(source);
        assert_eq!(ok, errors == 0, "{source}\n{err}");
        assert!(err.contains(wanted), "{source}\nwanted {wanted:?}, got:\n{err}");
        assert_eq!(err.matches("error:").count(), errors, "{source}\n{err}");
        assert_eq!(err.matches("warning:").count(), warnings, "{source}\n{err}");
    }
}
