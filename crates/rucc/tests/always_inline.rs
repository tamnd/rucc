//! What `always_inline` does to the IR a unit comes out as, at `-O0` where nothing else moves it.
//!
//! Design: `spec/optimizer/33-inlining.md` section 33.7.
//!
//! gcc inlines a function marked `always_inline` at every level, and glibc's fortified wrappers
//! depend on it, since `__builtin_va_arg_pack` only means something once the body is where the
//! call was. These read the IR rather than run a program, so they hold on any host. The last of
//! them read what the compiler says instead, about a body gcc says can never be copied into a
//! caller, which gcc 16 refuses with an error at every level and so does this.

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

/// A structure passed by value is inlined as a copy made in the caller just before the call, so a
/// body that writes to its parameter writes to the copy and the caller's own is left alone. The
/// AVX-512 intrinsics take 64 byte vectors this way, and gcc inlines them at every level.
#[test]
fn a_structure_passed_by_value_is_inlined_as_a_copy() {
    let text = ir(
        "byvalue",
        r#"
struct three { long a, b, c; };

static inline __attribute__((always_inline)) long bump (struct three t)
{
  t.a += 1;
  return t.a + t.b + t.c;
}

int main (void)
{
  struct three t = { 1, 2, 3 };
  long r = bump (t);
  return (int) (r * 10 + t.a);
}
"#,
    );
    let main = body(&text, "main");
    assert!(!main.iter().any(|line| line.contains("call @bump")), "{text}");
    assert!(main.iter().any(|line| line.contains("memcpy")), "no copy was made:\n{text}");
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

/// What the compiler says about the source at `level`, and whether it compiled it.
fn compile(what: &str, source: &str, level: &str) -> (bool, String) {
    let path = fixture(&format!("{what}{level}"), source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args([level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Checks that the source is refused at `-O0` and at `-O2` with gcc 16's error, at the place gcc 16
/// puts it, and that the error comes once.
fn refused(what: &str, source: &str, at: &str, error: &str) {
    for level in ["-O0", "-O2"] {
        let (built, said) = compile(what, source, level);
        assert!(!built, "{what} compiled at {level}:\n{said}");
        let line = format!("one.c:{at}: error: {error}");
        assert_eq!(said.matches(&line).count(), 1, "{what} at {level}:\n{said}");
    }
}

/// Checks that the source compiles at `-O0` and at `-O2` without a word, as it does with gcc 16.
fn accepted(what: &str, source: &str) {
    for level in ["-O0", "-O2"] {
        let (built, said) = compile(what, source, level);
        assert!(built && said.is_empty(), "{what} at {level}:\n{said}");
    }
}

/// gcc's example: a body that starts its own variable argument list needs a frame of its own.
#[test]
fn an_always_inline_function_that_calls_va_start_is_refused() {
    let source = r"static inline __attribute__((always_inline)) int g(int n, ...)
{
  __builtin_va_list ap;
  __builtin_va_start(ap, n);
  int r = __builtin_va_arg(ap, int);
  __builtin_va_end(ap);
  return r;
}
int f(void) { return g(1, 2); }
";
    let error = "function 'g' can never be inlined because it uses variable argument lists";
    refused("va-start", source, "1:50", error);
    let (_, said) = compile("va-start", source, "-O2");
    assert!(said.contains("one.c:4:3: note: the variable argument list is started here"), "{said}");
}

/// A call to `setjmp` by name is refused whatever it was declared as, and so is a definition
/// nothing in the file calls, since one with external linkage is compiled on its own.
#[test]
fn an_always_inline_function_that_calls_setjmp_is_refused() {
    let called = r"typedef long jmp_buf[8];
int setjmp(jmp_buf);
static jmp_buf b;
static inline __attribute__((always_inline)) int g(int n)
{
  if (setjmp(b))
    return 0;
  return n;
}
int f(void) { return g(1); }
";
    let error = "function 'g' can never be inlined because it uses setjmp";
    refused("setjmp", called, "4:50", error);
    let alone = r"typedef long jmp_buf[8];
int _setjmp(jmp_buf);
static jmp_buf b;
__attribute__((always_inline)) inline int g(int n)
{
  if (_setjmp(b))
    return 0;
  return n;
}
extern int g(int);
";
    refused("setjmp-alone", alone, "4:43", error);
}

/// A jump through a label's address, and a label's address in a `static`, which gcc asks about
/// first and says in words of its own.
#[test]
fn an_always_inline_function_with_a_computed_goto_is_refused() {
    let local = r"static inline __attribute__((always_inline)) int g(int n)
{
  void *t[] = { &&a, &&b };
  goto *t[n & 1];
a:
  return 1;
b:
  return 2;
}
int f(int n) { return g(n); }
";
    let error = "function 'g' can never be inlined because it contains a computed goto";
    refused("computed-goto", local, "1:50", error);
    let kept = local.replace("  void *t[]", "  static void *t[]");
    let error = "function 'g' can never be copied because it saves address of local label in a \
                 static variable";
    refused("static-label", &kept, "1:50", error);
}

/// The builtins that only mean something in a frame of the function's own.
#[test]
fn an_always_inline_function_that_saves_its_frame_is_refused() {
    let apply = r"static inline __attribute__((always_inline)) void *g(int n)
{
  return __builtin_apply_args();
}
void *f(int n) { return g(n); }
";
    let error = "function 'g' can never be inlined because it uses '__builtin_return' or \
                 '__builtin_apply_args'";
    refused("apply-args", apply, "1:52", error);
    let save = r"static inline __attribute__((always_inline)) int g(void **b)
{
  if (__builtin_setjmp(b))
    return 1;
  return 0;
}
int f(void **b) { return g(b); }
";
    let error = "function 'g' can never be copied because it receives a non-local goto";
    refused("builtin-setjmp", save, "1:50", error);
    let restore = r"static inline __attribute__((always_inline)) void g(void **b)
{
  __builtin_longjmp(b, 1);
}
void f(void **b) { g(b); }
";
    let error =
        "function 'g' can never be inlined because it uses setjmp-longjmp exception handling";
    refused("builtin-longjmp", restore, "1:51", error);
}

/// Of two reasons in one body the one written first is the one named, as in gcc.
#[test]
fn the_first_reason_in_the_body_is_the_one_named() {
    let source = r"typedef long jmp_buf[8];
int setjmp(jmp_buf);
static jmp_buf b;
static inline __attribute__((always_inline)) int g(int n, ...)
{
  __builtin_va_list ap;
  if (setjmp(b))
    return 0;
  __builtin_va_start(ap, n);
  int r = __builtin_va_arg(ap, int);
  __builtin_va_end(ap);
  return r;
}
int f(void) { return g(1, 2) + g(3, 4); }
";
    let error = "function 'g' can never be inlined because it uses setjmp";
    refused("two-reasons", source, "4:50", error);
}

/// What gcc 16 compiles without a word: a body nothing in the file reaches, an inline definition
/// nothing names, `alloca`, which `always_inline` overrides, and a call inside a cycle, which
/// stays a call.
#[test]
fn what_gcc_does_not_refuse_is_not_refused() {
    let body = r"
{
  __builtin_va_list ap;
  __builtin_va_start(ap, n);
  int r = __builtin_va_arg(ap, int);
  __builtin_va_end(ap);
  return r;
}
int f(void) { return 0; }
";
    let unused = format!("static inline __attribute__((always_inline)) int g(int n, ...){body}");
    accepted("unused", &unused);
    let inline = format!("__attribute__((always_inline)) inline int g(int n, ...){body}");
    accepted("inline-definition", &inline);
    let alloca = r"static inline __attribute__((always_inline)) int g(int n)
{
  char *p = __builtin_alloca(n);
  p[0] = 1;
  return p[0];
}
int f(int n) { return g(n); }
";
    accepted("alloca", alloca);
    let cycle = r"static inline __attribute__((always_inline)) int g(int n)
{
  if (n <= 0)
    return 0;
  return n + g(n - 1);
}
int f(int n) { return g(n); }
";
    let (built, said) = compile("cycle", cycle, "-O2");
    assert!(built && said.is_empty(), "{said}");
}
