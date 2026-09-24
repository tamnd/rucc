//! What `always_inline` does to the IR a unit comes out as, at `-O0` where nothing else moves it.
//!
//! Design: `spec/optimizer/33-inlining.md` section 33.7.
//!
//! gcc inlines a function marked `always_inline` at every level, and glibc's fortified wrappers
//! depend on it, since `__builtin_va_arg_pack` only means something once the body is where the
//! call was. These read the IR rather than run a program, so they hold on any host.

use std::path::PathBuf;
use std::process::Command;

/// The target the IR is asked for, which is the one whose convention the inliner forwards packs
/// under.
const TARGET: &str = "x86_64-linux-gnu";

/// The source, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-inline-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The IR the compiler produced for the source at `-O0`.
fn ir(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O0", "--emit=ir", "-w", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused it:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The instructions of one function, without the name of it or the braces around it.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("func @{name}(");
    text.lines()
        .map(str::trim)
        .skip_while(|line| !line.starts_with(&open))
        .skip(1)
        .take_while(|line| *line != "}")
        .filter(|line| !line.is_empty())
        .collect()
}

/// A store through a `float *` in a function that asked for no strict aliasing keeps no aliasing
/// node once it is inlined, so the load of the `int` after it cannot be moved past it. This is
/// `gcc.c-torture/execute/pr79043.c`, which aborts if it can.
#[test]
fn an_inlined_body_keeps_the_aliasing_it_asked_for() {
    let text = ir(
        "punning",
        r#"
int val;
float *ptr2 = (float *) &val;

static void __attribute__((always_inline, optimize ("-fno-strict-aliasing"))) typepun (void)
{
  *ptr2 = 0;
}

int main (void)
{
  typepun ();
  return val;
}
"#,
    );
    let main = body(&text, "main");
    assert!(!main.iter().any(|line| line.contains("call @typepun")), "{text}");
    let store = main.iter().find(|line| line.starts_with("store")).expect("the store was inlined");
    assert!(!store.contains("tbaa"), "{text}");
}

/// The pack is the anonymous arguments of the call and the length is how many there were, and a
/// wrapper every call was inlined into is not emitted.
#[test]
fn a_fortified_wrapper_forwards_its_arguments() {
    let text = ir(
        "pack",
        r#"
int sink (int n, ...);

extern inline __attribute__((always_inline, gnu_inline)) int wrap (int x, ...)
{
  return sink (__builtin_va_arg_pack_len (), x, __builtin_va_arg_pack ());
}

int main (void)
{
  return wrap (1, 7, 3.0);
}
"#,
    );
    let main = body(&text, "main");
    assert!(!main.iter().any(|line| line.contains("call @wrap")), "{text}");
    assert!(main.iter().any(|line| line.contains("iconst.i32 2")), "{text}");
    assert!(main.iter().any(|line| line.contains("call @sink(")), "{text}");
    assert!(!text.contains("va_arg_pack"), "{text}");
}
