//! `__builtin_va_arg_pack` and `__builtin_va_arg_pack_len` in a `static` function, which gcc
//! fills in wherever it inlines the function and refuses in a copy it has to compile.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! gcc's documentation puts them in a function marked `always_inline`, and its front end takes
//! them in any function at all. What it refuses is a body that is still compiled with one in it,
//! once the inliner is done, in the words `invalid use of '__builtin_va_arg_pack ()'`. A `static`
//! function whose every call went in is not compiled, so a `static` one is the other place they
//! may stand besides GNU's `extern inline`. Each answer here is what gcc 13 gives for the same
//! source, at the level it is asked at.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The target the IR is asked for, which is the one whose convention the inliner forwards packs
/// under.
const TARGET: &str = "x86_64-linux-gnu";

/// The source, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-pack-static-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler made of the source with those arguments.
fn rucc(what: &str, source: &str, args: &[&str]) -> Output {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    out
}

/// The IR for the source at that level, which has to compile without a word.
fn ir(what: &str, source: &str, level: &str) -> String {
    let target = format!("--target={TARGET}");
    let out = rucc(what, source, &[&target, level, "--emit=ir", "-o", "-"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && said.is_empty(), "{what} at {level}:\n{said}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Checks that the source is refused at that level with the error at that place, once.
fn refused(what: &str, source: &str, level: &str, at: &str, error: &str) {
    let target = format!("--target={TARGET}");
    let out = rucc(what, source, &[&target, level, "-S", "-o", "-"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{what} compiled at {level}:\n{said}");
    let line = format!("one.c:{at}: error: {error}");
    assert_eq!(said.matches(&line).count(), 1, "{what} at {level}:\n{said}");
}

/// The wrapper glibc would write, made `static`: every call goes in at every level, with the
/// anonymous arguments in place of the pack and their count in place of the length, and what is
/// left of the wrapper is a declaration.
#[test]
fn a_static_always_inline_function_forwards_its_arguments() {
    let source = r"int sink (int n, ...);

static inline __attribute__((always_inline)) int
wrap (int x, ...)
{
  return sink (__builtin_va_arg_pack_len (), x, __builtin_va_arg_pack ());
}

int main (void)
{
  return wrap (1, 7, 3.0);
}
";
    for level in ["-O0", "-O2"] {
        let text = ir("forwards", source, level);
        assert!(!text.contains("va_arg_pack"), "{level}:\n{text}");
        assert!(!text.contains("call @wrap"), "{level}:\n{text}");
        assert!(text.contains("iconst.i32 2"), "{level}:\n{text}");
        assert!(text.contains("call @sink("), "{level}:\n{text}");
    }
}

/// The same, run: what `printf` is handed is what the call to the wrapper was.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_static_always_inline_wrapper_around_printf_prints_what_it_was_handed() {
    let source = r#"#include <stdio.h>

static inline __attribute__((always_inline)) int
say (const char *fmt, ...)
{
  return printf (fmt, __builtin_va_arg_pack ()) + __builtin_va_arg_pack_len ();
}

int main (void)
{
  return say ("%d %s %.1f\n", 42, "hi", 2.5) != 13;
}
"#;
    for level in ["-O0", "-O2"] {
        let path = fixture(&format!("printf{level}"), source);
        let prog = path.with_extension("");
        let built = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([level, "-o"])
            .arg(&prog)
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(built.status.success(), "{level}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&prog).output().expect("what was linked can be run");
        let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "42 hi 2.5\n", "{level}");
        assert_eq!(ran.status.code(), Some(0), "{level}");
    }
}

/// A wrapper whose address is taken is compiled out of line whatever else is inlined, and that
/// copy has nothing to fill the pack in with. gcc says so at the call the pack is an argument of.
#[test]
fn a_static_wrapper_whose_address_is_taken_is_refused_in_gccs_words() {
    let source = r"int printf (const char *, ...);
static inline __attribute__((always_inline)) int
say (const char *fmt, ...)
{
  return printf (fmt, __builtin_va_arg_pack ());
}
int (*p) (const char *, ...) = say;
int main (void) { return say (0, 1); }
";
    let error = "invalid use of '__builtin_va_arg_pack ()'";
    for level in ["-O0", "-O2"] {
        refused("taken", source, level, "5:10", error);
    }
}

/// A `static` function that is not `always_inline` is inlined only from `-O1` up, so its copy out
/// of line is compiled at `-O0` and refused there, and its one call goes in at `-O2`.
#[test]
fn a_static_function_called_once_holds_a_pack_only_from_o1_up() {
    let source = r"static int
say (const char *fmt, ...)
{
  return __builtin_va_arg_pack_len ();
}
int main (void) { return say (0, 1) != 1; }
";
    let error = "invalid use of '__builtin_va_arg_pack_len ()'";
    refused("once", source, "-O0", "4:10", error);
    let text = ir("once", source, "-O2");
    assert!(!text.contains("va_arg_pack"), "{text}");
}

/// A definition with external linkage is compiled whatever the inliner does, so the pack is
/// refused in it where it is written, before anything is compiled.
#[test]
fn a_pack_in_a_function_other_units_can_call_is_refused_where_it_is_written() {
    let source = r"__attribute__((always_inline)) inline int say (const char *fmt, ...);
__attribute__((always_inline)) int
say (const char *fmt, ...)
{
  return __builtin_va_arg_pack_len ();
}
";
    let error = "'__builtin_va_arg_pack_len' is only accepted in a variadic function that is \
                 'static' or that nothing is emitted for, which are the places it can be \
                 forwarded from";
    for level in ["-O0", "-O2"] {
        refused("external", source, level, "5:10", error);
    }
}
