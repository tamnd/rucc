//! `-Wnonnull`, end to end: a null pointer handed to a parameter `nonnull` or
//! `nonnull_if_nonzero` says must not be one is warned about at the call in gcc's words, with a
//! note at the function, under the flags gcc says it under.

use std::path::{Path, PathBuf};
use std::process::Command;

const X86_64: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-nonnull-call-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished and what it said, for one source under those flags.
fn compile(what: &str, flags: &[&str], source: &str) -> (bool, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={X86_64}");
    let mut args = vec![target.as_str(), "-S", "-o", "-"];
    args.extend_from_slice(flags);
    args.push("a.c");
    let out = run(&dir, &args);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The calls gcc 13 warns about, measured line by line, and the ones it does not: a null pointer
/// in a named argument, under a cast, in either arm of a `?:` whose condition does not fold and
/// among the variable arguments of a function whose `nonnull` names none, and through a pointer
/// marked `nonnull`, which has no function to give the note at. An integer `0` among the variable
/// arguments is not a pointer, and the arm a folded condition does not take is never passed.
const CALLS: &str = "void f(char *p, int n, char *q) __attribute__((nonnull(1, 3)));\n\
    void g(char *p) __attribute__((nonnull));\n\
    void v(const char *f, ...) __attribute__((nonnull));\n\
    void (*fp)(char *) __attribute__((nonnull(1)));\n\
    void h(int c, char *p) {\n\
      f(0, 0, p);\n\
      f(p, 0, (char *)0);\n\
      g((void *)0);\n\
      f(p, 0, c ? 0 : p);\n\
      f(p, 0, 1 ? p : 0);\n\
      v(\"x\", (char *)0, 0);\n\
      fp(0);\n\
      f(p, 0, 0L);\n\
      f(p, 0, p);\n\
    }\n";

#[test]
fn a_null_argument_is_warned_about_in_gcc_s_words() {
    let (ok, said) = compile("named", &["-Wall"], CALLS);
    assert!(ok, "{said}");
    let warned = [(6, 1), (7, 3), (8, 1), (9, 3), (11, 2), (12, 1), (13, 3)];
    for (line, number) in warned {
        let what = format!(
            "a.c:{line}:1: warning: argument {number} null where non-null expected [-Wnonnull]"
        );
        assert!(said.contains(&what), "{what}\n{said}");
    }
    assert_eq!(said.matches("[-Wnonnull]").count(), warned.len(), "{said}");
    let f = "a.c:1:6: note: in a call to function 'f' declared 'nonnull'";
    assert_eq!(said.matches(f).count(), 4, "{said}");
    assert!(said.contains("a.c:2:6: note: in a call to function 'g' declared 'nonnull'"), "{said}");
    assert!(said.contains("a.c:3:6: note: in a call to function 'v' declared 'nonnull'"), "{said}");
    assert_eq!(said.matches("note: in a call").count(), 6, "{said}");
}

/// gcc turns it on with `-Wall` and `-Wformat`, and `-Wnonnull` by name turns it on whatever was
/// said about `-Wformat`. It is off without them, which is upstream gcc's default whatever a
/// distribution's gcc turns on.
#[test]
fn the_warning_is_heard_under_the_flags_gcc_says_it_under() {
    for (flags, heard) in [
        (&[][..], false),
        (&["-Wformat"][..], true),
        (&["-Wnonnull"][..], true),
        (&["-Wall", "-Wno-format"][..], false),
        (&["-Wall", "-Wno-nonnull"][..], false),
        (&["-Wno-format", "-Wnonnull"][..], true),
        (&["-Wnonnull", "-Wno-format"][..], true),
    ] {
        let (ok, said) = compile("flags", flags, CALLS);
        assert!(ok, "{flags:?}: {said}");
        assert_eq!(said.contains("[-Wnonnull]"), heard, "{flags:?}: {said}");
    }
    let (ok, said) = compile("error", &["-Werror=nonnull"], CALLS);
    assert!(!ok, "{said}");
    assert!(said.contains("error: argument 1 null where non-null expected"), "{said}");
}

/// `nonnull_if_nonzero` asks for the pointer only where the counts are integer constants other
/// than zero, in gcc 15's words, which name both counts of the three-number form. A count that is
/// not a constant is not known to be anything, so nothing is said. A `nonnull` without numbers
/// checks every argument, and gcc then reads no `nonnull_if_nonzero` for the call.
#[test]
fn a_conditional_nonnull_is_heard_where_the_count_is_not_zero() {
    let source = "typedef __SIZE_TYPE__ size_t;\n\
        void *cp(void *d, const void *s, size_t n) \
            __attribute__((nonnull_if_nonzero(1, 3), nonnull_if_nonzero(2, 3)));\n\
        void *fill(void *d, size_t n, int m) __attribute__((nonnull_if_nonzero(1, 2, 3)));\n\
        void *both(void *d, size_t n) __attribute__((nonnull, nonnull_if_nonzero(1, 2)));\n\
        void use(void *d, size_t n) {\n\
          cp(0, d, 4);\n\
          cp(d, 0, n);\n\
          cp(0, 0, 0);\n\
          fill(0, 2, 1);\n\
          fill(0, 2, 0);\n\
          both(0, 1);\n\
        }\n";
    let (ok, said) = compile("conditional", &["-Wall"], source);
    assert!(ok, "{said}");
    for what in [
        "a.c:6:1: warning: argument 1 null where non-null expected because argument 3 is nonzero \
         [-Wnonnull]",
        "a.c:2:7: note: in a call to function 'cp' declared 'nonnull_if_nonzero'",
        "a.c:9:1: warning: argument 1 null where non-null expected because arguments 2 and 3 \
         are nonzero [-Wnonnull]",
        "a.c:3:7: note: in a call to function 'fill' declared 'nonnull_if_nonzero'",
        "a.c:11:1: warning: argument 1 null where non-null expected [-Wnonnull]",
        "a.c:4:7: note: in a call to function 'both' declared 'nonnull'",
    ] {
        assert!(said.contains(what), "{what}\n{said}");
    }
    assert_eq!(said.matches("[-Wnonnull]").count(), 3, "{said}");
}
