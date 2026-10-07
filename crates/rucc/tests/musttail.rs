//! `[[gnu::musttail]] return f(x);`, end to end: the jump at every level of optimization, a chain
//! of them that runs in one frame, and what gcc 15 says where the attribute is in the wrong place
//! or the call cannot be made, in its words.

use std::path::PathBuf;
use std::process::Command;

const X86_64: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-musttail-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// Whether the compiler finished, the listing and what it said, for one source under those flags.
fn compile(what: &str, target: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg("a.c")
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The instructions of one function, without directives or labels.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.contains(".cfi_endproc") && !line.starts_with(".Lfunc_end"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.ends_with(':'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// A local in the frame whose address another call took, a `void` call from a `void` function,
/// a call through a pointer and each of gcc's spellings.
const CALLS: &str = "int g(int); void h(int *); void v(void); int (*p)(int);
int f(int a) { int b[2] = {a, a}; h(b); [[gnu::musttail]] return g(b[0]); }
void w(void) { __attribute__((musttail)) return v(); }
int i(int a) { [[clang::musttail]] return (p(a)); }
int j(int a) { __attribute__((__musttail__)) return g(a); }
_Static_assert(__has_attribute(musttail) && __has_c_attribute(gnu::musttail), \"gcc 15\");
";

/// Each call is a jump with optimization and without, on x86-64 and on AArch64.
#[test]
fn a_musttail_call_is_a_jump_at_every_level() {
    for level in ["-O0", "-O2"] {
        let (ok, text, err) = compile("x64", X86_64, &[level], CALLS);
        assert!(ok, "{level}\n{err}");
        assert_eq!(err, "", "{level}");
        for (name, callee) in [("f", "g"), ("w", "v"), ("j", "g")] {
            let lines = body(&text, name);
            assert!(lines.contains(&format!("jmp {callee}")), "{level} {name}: {lines:#?}");
            assert!(!lines.contains(&format!("call {callee}")), "{level}: {lines:#?}");
        }
        let lines = body(&text, "i");
        assert!(lines.iter().any(|line| line.starts_with("jmp *")), "{level}: {lines:#?}");

        let (ok, text, err) = compile("a64", "aarch64-unknown-linux-gnu", &[level], CALLS);
        assert!(ok, "{level}\n{err}");
        for (name, callee) in [("f", "g"), ("w", "v"), ("j", "g")] {
            let lines = body(&text, name);
            assert!(lines.contains(&format!("b {callee}")), "{level} {name}: {lines:#?}");
        }
        let lines = body(&text, "i");
        assert!(lines.iter().any(|line| line.starts_with("br x")), "{level}: {lines:#?}");

        // i386 passes its arguments on the stack, so it is the call that takes none.
        let none = "void v(void); void w(void) { [[gnu::musttail]] return v(); }\n";
        let (ok, text, err) = compile("x86", "i386-unknown-linux-gnu", &[level, "-fno-pic"], none);
        assert!(ok, "{level}\n{err}");
        let lines = body(&text, "w");
        assert!(lines.contains(&"jmp v".to_owned()), "{level}: {lines:#?}");
    }
}

/// Two functions that hand a count back and forth ten million times run in the stack of one
/// call without optimization, where each would otherwise take a frame.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn a_chain_of_musttail_calls_runs_in_one_frame() {
    let source = "long odd(long n, long acc);
long even(long n, long acc) {
  if (n == 0) return acc;
  [[gnu::musttail]] return odd(n - 1, acc + 1);
}
long odd(long n, long acc) {
  if (n == 0) return -acc;
  [[gnu::musttail]] return even(n - 1, acc + 2);
}
int main(void) { return even(10000000, 0) == 15000000 ? 0 : 1; }
";
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .args(["-O0", "a.c", "-o", "a"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ran = Command::new(dir.join("a")).status().expect("what was linked can be run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(ran.code(), Some(0), "the chain did not come back with its count");
}

/// What gcc 15 says about a `musttail` that cannot be made, at every level, and about one
/// anywhere but in front of a `return`, which it ignores.
#[test]
fn a_musttail_is_checked_in_gcc_s_words() {
    let refused = [
        (
            "int g(int); int f(int a) { [[gnu::musttail]] return a + 1; }\n",
            "error: cannot tail-call: return value must be a call",
        ),
        (
            "int g(int); int f(int a) { [[gnu::musttail]] return g(a) + 1; }\n",
            "error: cannot tail-call: return value must be a call",
        ),
        (
            "void f(void) { [[gnu::musttail]] return; }\n",
            "error: cannot tail-call: return value must be a call",
        ),
        (
            "long g(int); int f(int a) { [[gnu::musttail]] return g(a); }\n",
            "error: cannot tail-call: return value changed after call",
        ),
        (
            "int g(int); int f(int a, ...) { __builtin_va_list ap; __builtin_va_start(ap, a); \
                __builtin_va_end(ap); [[gnu::musttail]] return g(a); }\n",
            "error: cannot tail-call: caller uses stdargs",
        ),
        (
            "struct s { long a[4]; }; struct s g(int); \
                struct s f(int a) { [[gnu::musttail]] return g(a); }\n",
            "error: cannot tail-call: callee returns a structure",
        ),
        (
            "int g(int); int f(int a) { [[gnu::musttail(1)]] return g(a); }\n",
            "error: 'musttail' attribute does not take any arguments",
        ),
    ];
    for level in ["-O0", "-O2"] {
        for (source, said) in refused {
            let (ok, _, err) = compile("refused", X86_64, &[level], source);
            assert!(!ok, "{level} {source}");
            assert!(err.contains(said), "{level} {source}\nwanted {said:?}, got:\n{err}");
            assert_eq!(err.matches("error:").count(), 1, "{level} {source}\n{err}");
        }
    }
    let ignored = [
        "int g(int); int f(int a) { [[gnu::musttail]] g(a); return 0; }\n",
        "void f(void) { [[gnu::musttail]]; }\n",
        "[[gnu::musttail]] int g(int);\n",
    ];
    for source in ignored {
        let (ok, _, err) = compile("ignored", X86_64, &[], source);
        assert!(ok, "{source}\n{err}");
        assert!(err.contains("warning: 'musttail' attribute ignored"), "{source}\n{err}");
    }
    let mixed = "int g(int); int f(int a) { [[gnu::musttail, gnu::hot]] return g(a); }\n";
    let (ok, text, err) = compile("mixed", X86_64, &[], mixed);
    assert!(ok, "{err}");
    let said = "warning: attribute 'musttail' mixed with other attributes on 'return' statement";
    assert!(err.contains(said), "{err}");
    assert!(body(&text, "f").contains(&"jmp g".to_owned()), "{text}");
    // gcc 14 has never heard of it.
    let old = "int g(int); int f(int a) { [[gnu::musttail]] return g(a); }\n";
    let (ok, _, err) = compile("old", X86_64, &["-fgnuc-version=14.2.0"], old);
    assert!(ok, "{err}");
    assert!(!err.contains("tail-call"), "{err}");
}

/// gcc 15's `-Wmusttail-local-addr`: an argument that points into the frame the jump gives back,
/// once for each, in its words, with the call made all the same. An address the call cannot be
/// handed without running something first, and one of anything outside the frame, say nothing.
#[test]
fn a_pointer_into_the_frame_is_warned_about_in_gcc_s_words() {
    let lead = "int g(void *); int g2(void *, void *); struct s { int m[2]; };\n";
    let warned = [
        ("int f(int a) { int b = a; [[gnu::musttail]] return g(&b); }", "automatic variable 'b'"),
        ("int f(int a) { [[gnu::musttail]] return g(&a); }", "parameter 'a'"),
        (
            "int f(void) { struct s s; [[gnu::musttail]] return g(&s.m[1]); }",
            "automatic variable 's'",
        ),
        (
            "int f(void) { int b[2]; [[gnu::musttail]] return g((char *)b); }",
            "automatic variable 'b'",
        ),
        ("int f(void) { [[gnu::musttail]] return g((int[]){1, 2}); }", "local variable"),
        ("int f(void) { goto l; l:; [[gnu::musttail]] return g(&&l); }", "label"),
    ];
    for level in ["-O0", "-O2"] {
        for (def, what) in warned {
            let source = format!("{lead}{def}\n");
            let (ok, text, err) = compile("frame", X86_64, &[level], &source);
            assert!(ok, "{level} {def}\n{err}");
            let said = format!("warning: address of {what} passed to 'musttail' call argument");
            assert!(err.contains(&said), "{level} {def}\nwanted {said:?}, got:\n{err}");
            assert_eq!(err.matches("warning:").count(), 1, "{level} {def}\n{err}");
            assert!(body(&text, "f").contains(&"jmp g".to_owned()), "{level} {def}\n{text}");
        }
    }
    let both = format!("{lead}int f(int a) {{ int b; [[gnu::musttail]] return g2(&a, &b); }}\n");
    let (ok, _, err) = compile("both", X86_64, &[], &both);
    assert!(ok, "{err}");
    assert!(err.contains("address of parameter 'a' passed"), "{err}");
    assert!(err.contains("address of automatic variable 'b' passed"), "{err}");
    assert_eq!(err.matches("warning:").count(), 2, "{err}");

    let quiet = [
        "int f(int i) { int b[2]; [[gnu::musttail]] return g(&b[i]); }",
        "int f(void) { static int b; [[gnu::musttail]] return g(&b); }",
        "extern int e; int f(void) { [[gnu::musttail]] return g(&e); }",
        "int f(int *p) { [[gnu::musttail]] return g2(p, &p[1]); }",
        "int f(struct s *p) { [[gnu::musttail]] return g(&p->m[1]); }",
        "int f(void) { [[gnu::musttail]] return g(\"text\"); }",
        "int f(int a) { int b = a; return g(&b); }",
    ];
    for def in quiet {
        let source = format!("{lead}{def}\n");
        let (ok, _, err) = compile("quiet", X86_64, &["-Wall"], &source);
        assert!(ok, "{def}\n{err}");
        assert_eq!(err, "", "{def}");
    }
    // A caller with variable arguments is refused before gcc looks at what the call is handed.
    let variadic = format!("{lead}int f(int a, ...) {{ [[gnu::musttail]] return g(&a); }}\n");
    let (ok, _, err) = compile("variadic", X86_64, &[], &variadic);
    assert!(!ok);
    assert!(err.contains("cannot tail-call: caller uses stdargs"), "{err}");
    assert!(!err.contains("warning:"), "{err}");

    let one = format!("{lead}int f(int a) {{ [[gnu::musttail]] return g(&a); }}\n");
    let (ok, _, err) = compile("off", X86_64, &["-Wno-musttail-local-addr"], &one);
    assert!(ok, "{err}");
    assert_eq!(err, "");
    let (ok, _, err) = compile("error", X86_64, &["-Werror=musttail-local-addr"], &one);
    assert!(!ok);
    assert!(err.contains("error: address of parameter 'a' passed"), "{err}");
}

/// gcc 15's own test of `-Wmaybe-musttail-local-addr`, `c-c++-common/musttail30.c`, with the
/// `dg-warning` comments taken off, which are what [`PASSED`] and [`ESCAPED`] say, and two
/// functions of this compiler's after it: a parameter whose address is taken after the call, an
/// array that is only indexed, one whose element's address is computed, and the variable declared
/// last being the one named.
const ESCAPES: &str = r"int foo (int, void *);
int bar (int, int *);
struct S { int a, b, c; };
struct T { int d; struct S e; };

int
baz (int x, void *y)
{
  [[gnu::musttail]] return bar (2, &x);
}

int
qux (int x, void *y)
{
  __label__ lab;
  lab:;
  if (*(int *) y == 1)
    [[gnu::musttail]] return foo (1, &&lab);
  if (x == 1)
    [[gnu::musttail]] return foo (3, 0);
  else if (x == 2)
    {
      {
        int a = 42;
        bar (4, &a);
      }
      [[gnu::musttail]] return bar (5, 0);
    }
  else if (x == 3)
    {
      int a = 42;
      bar (4, &a);
      [[gnu::musttail]] return bar (6, 0);
    }
  else if (x == 4)
    {
      int a = 42;
      [[gnu::musttail]] return bar (7, &a);
    }
  else if (x == 5)
    {
      struct T b;
      [[gnu::musttail]] return bar (8, &b.e.b);
    }
  else if (x == 6)
    {
      struct T b;
      bar (9, &b.e.a);
      [[gnu::musttail]] return bar (10, 0);
    }
  else if (x == 7)
    {
      {
        struct T b;
        bar (9, &b.e.a);
      }
      [[gnu::musttail]] return bar (11, 0);
    }
  else if (x == 8)
    {
      {
        int a = 42;
        bar (4, &a);
      }
      [[gnu::musttail]] return foo (12, 0);
    }
  else if (x == 9)
    {
      int a = 42;
      bar (4, &a);
      [[gnu::musttail]] return foo (13, 0);
    }
  else if (x == 10)
    {
      int a = 42;
      [[gnu::musttail]] return foo (14, &a);
    }
  else if (x == 11)
    {
      struct T b;
      [[gnu::musttail]] return foo (15, &b.e.b);
    }
  else if (x == 12)
    {
      struct T b;
      bar (9, &b.e.a);
      [[gnu::musttail]] return foo (16, 0);
    }
  else if (x == 13)
    {
      {
        struct T b;
        bar (9, &b.e.a);
      }
      [[gnu::musttail]] return foo (17, 0);
    }
  return 0;
}

int
corge (int x, void *y)
{
  if (*(int *) y == 1)
    bar (18, &x);
  [[gnu::musttail]] return bar (2, 0);
}

int
late (int x, int i)
{
  int b[4], c[4];
  b[1] = 0;
  if (x)
    [[gnu::musttail]] return bar (19, 0);
  bar (20, &c[i]);
  [[gnu::musttail]] return foo (21, &c[i]);
  bar (22, &x);
  return b[1];
}

int
last (int x)
{
  int a, d[2];
  bar (23, &a);
  bar (24, d);
  {
    int e;
    bar (25, &e);
    [[gnu::musttail]] return bar (26, 0);
  }
  return 0;
}
";

/// What gcc 15 says under `-Wmusttail-local-addr` about [`ESCAPES`], without the columns, which
/// is said as each call is read where the rest is said once its function has been.
const PASSED: &[&str] = &[
    "a.c:9: warning: address of parameter 'x' passed to 'musttail' call argument [E0850]",
    "a.c:18: warning: address of label passed to 'musttail' call argument [E0850]",
    "a.c:38: warning: address of automatic variable 'a' passed to 'musttail' call argument [E0850]",
    "a.c:43: warning: address of automatic variable 'b' passed to 'musttail' call argument [E0850]",
    "a.c:76: warning: address of automatic variable 'a' passed to 'musttail' call argument [E0850]",
    "a.c:81: warning: address of automatic variable 'b' passed to 'musttail' call argument [E0850]",
];

/// What it says under `-Wmaybe-musttail-local-addr`, without the columns.
const ESCAPED: &[&str] = &[
    "a.c:33: warning: address of automatic variable 'a' can escape to 'musttail' call [E0857]",
    "a.c:49: warning: address of automatic variable 'b' can escape to 'musttail' call [E0857]",
    "a.c:71: warning: address of automatic variable 'a' can escape to 'musttail' call [E0857]",
    "a.c:87: warning: address of automatic variable 'b' can escape to 'musttail' call [E0857]",
    "a.c:105: warning: address of parameter 'x' can escape to 'musttail' call [E0857]",
    "a.c:114: warning: address of parameter 'x' can escape to 'musttail' call [E0857]",
    "a.c:116: warning: address of automatic variable 'c' can escape to 'musttail' call [E0857]",
    "a.c:130: warning: address of automatic variable 'e' can escape to 'musttail' call [E0857]",
];

/// The lines about the frame, without their columns, which gcc takes from the call as it has
/// rewritten it.
fn frame_lines(err: &str) -> Vec<String> {
    err.lines()
        .filter(|line| line.contains("[E0850]") || line.contains("[E0857]"))
        .map(|line| {
            let mut parts: Vec<&str> = line.splitn(4, ':').collect();
            parts.remove(2);
            parts.join(":")
        })
        .collect()
}

/// gcc 15's `-Wmaybe-musttail-local-addr`, which `-Wextra` turns on: a call handed no address
/// in the frame is warned about once where the function took the address of a variable still
/// in scope at the call, or of a parameter, at any level of optimization, and the calls are made
/// all the same.
#[test]
fn an_address_that_escaped_before_the_jump_is_warned_about_in_gcc_s_words() {
    let mut both: Vec<&str> = PASSED.iter().chain(ESCAPED).copied().collect();
    both.sort_by_key(|line| line[4..].split(':').next().unwrap().parse::<u32>().unwrap());
    for flags in [&["-O0", "-Wextra"][..], &["-O2", "-Wmaybe-musttail-local-addr"][..]] {
        let (ok, text, err) = compile("escape", X86_64, flags, ESCAPES);
        assert!(ok, "{flags:?}\n{err}");
        let said = frame_lines(&err);
        let mut sorted = said.clone();
        sorted.sort_by_key(|line| line[4..].split(':').next().unwrap().parse::<u32>().unwrap());
        assert_eq!(sorted, both, "{flags:?}\n{err}");
        assert_eq!(err.matches("warning:").count(), said.len(), "{flags:?}\n{err}");
        assert!(body(&text, "corge").contains(&"jmp bar".to_owned()), "{flags:?}\n{text}");
    }
    for flags in [&["-Wall"][..], &["-Wextra", "-Wno-maybe-musttail-local-addr"][..]] {
        let (ok, _, err) = compile("escape-off", X86_64, flags, ESCAPES);
        assert!(ok, "{flags:?}\n{err}");
        assert_eq!(frame_lines(&err), PASSED, "{flags:?}\n{err}");
    }
    let (ok, _, err) =
        compile("escape-error", X86_64, &["-Werror=maybe-musttail-local-addr"], ESCAPES);
    assert!(!ok, "{err}");
    assert!(err.contains("error: address of parameter 'x' can escape to 'musttail' call"), "{err}");
}
