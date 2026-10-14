//! `__attribute__((no_callee_saved_registers))` and `__attribute__((preserve_none))`, gcc's two
//! calling conventions that keep nothing across a call, read from the whole compiler.
//!
//! A test of the whole compiler rather than of one crate for the reason `conventions.rs` beside
//! this gives: the convention is read in the front end, carried in the function type and the IR
//! signature, and turned into registers and saved registers in the back end. The registers each
//! one takes and every diagnostic were taken from gcc 16 for x86-64 Linux. On an x86-64 Linux host
//! the functions are also called, from a caller that keeps values across each call, out of a
//! callee that writes every register it is free to.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The one place the two are built.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-unsaved-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, what it wrote on its standard output and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The assembly of one source for a target at `-O2`, and what was said.
fn assembly(what: &str, target: &str, source: &str) -> (bool, String, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={target}");
    let said = run(&dir, &[&target, "-O2", "-S", "-o", "-", "a.c"]);
    let _ = std::fs::remove_dir_all(&dir);
    said
}

/// The listing of a source the compiler has to accept without a word.
fn listing(what: &str, source: &str) -> String {
    let (ok, out, err) = assembly(what, TARGET, source);
    assert!(ok, "the compiler refused the fixture:\n{err}");
    assert_eq!(err, "");
    out
}

/// The instructions of one function, which are the lines from its label to the next function's
/// label or the end, less the ones said to the assembler.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let label = format!("{name}:");
    let mut out = Vec::new();
    for line in text.lines().skip_while(|line| line.trim() != label).skip(1) {
        let trimmed = line.trim();
        if !line.starts_with(char::is_whitespace)
            && trimmed.ends_with(':')
            && !trimmed.starts_with('.')
        {
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('.') || trimmed.ends_with(':') {
            continue;
        }
        out.push(trimmed);
    }
    assert!(!out.is_empty(), "no function {name} in the listing:\n{text}");
    out
}

/// Whether an instruction in the body has all of those words in it.
fn has(body: &[&str], words: &[&str]) -> bool {
    body.iter().any(|line| words.iter().all(|word| line.contains(word)))
}

/// A call puts the first six integers in `r12`, `r13`, `r14`, `r15`, `rdi` and `rsi`, the seventh
/// on the stack and a `double` in `xmm0`, as gcc 16 does. The C23 spelling is the same attribute.
#[test]
fn a_preserve_none_call_puts_the_integers_in_r12_first() {
    let out = listing(
        "args",
        "__attribute__((preserve_none)) long p(long, long, long, long, long, long, long, \
         double);\n\
         [[gnu::preserve_none]] long q(long, long);\n\
         long call(void) { return p(1, 2, 3, 4, 5, 6, 7, 0.5); }\n\
         long callq(void) { return q(8, 9); }\n",
    );
    let call = body(&out, "call");
    for (value, reg) in
        [("$1", "%r12"), ("$2", "%r13"), ("$3", "%r14"), ("$4", "%r15"), ("$5", "di"), ("$6", "si")]
    {
        assert!(has(&call, &[value, reg]), "{value} is not put in {reg}:\n{out}");
    }
    assert!(has(&call, &["$7", "(%rsp)"]), "the seventh is not on the stack:\n{out}");
    assert!(has(&call, &["%xmm0"]), "{out}");
    assert!(!has(&call, &["%rdx"]) && !has(&call, &["%rcx"]), "{out}");
    let callq = body(&out, "callq");
    assert!(has(&callq, &["$8", "%r12"]) && has(&callq, &["$9", "%r13"]), "{out}");
}

/// A function of either convention writes `rbx` and `r12` to `r15` and gives none of them back,
/// so its prologue pushes nothing but the frame pointer.
#[test]
fn a_function_that_keeps_nothing_pushes_nothing_it_writes() {
    let out = listing(
        "pushes",
        "#define TRASH __asm__ volatile(\"\" ::: \"rbx\", \"r12\", \"r13\", \"r14\", \"r15\")\n\
         __attribute__((no_callee_saved_registers)) long n(long a, long b) {\n\
         TRASH; return a + b; }\n\
         __attribute__((preserve_none)) long p(long a, long b) { TRASH; return a + b; }\n\
         long plain(long a, long b) { TRASH; return a + b; }\n",
    );
    for name in ["n", "p"] {
        let pushed: Vec<&str> = body(&out, name)
            .into_iter()
            .filter(|line| line.starts_with("push") && !line.contains("%rbp"))
            .collect();
        assert!(pushed.is_empty(), "{name} saves {pushed:?}:\n{out}");
    }
    assert!(has(&body(&out, "plain"), &["push", "%rbx"]), "{out}");
    // The two arguments are where each convention puts them.
    assert!(has(&body(&out, "n"), &["%rdi"]) && has(&body(&out, "p"), &["%r12"]), "{out}");
}

/// What gcc 16 says, in its words: the attribute on an object, with an argument, beside the
/// other, beside `no_caller_saved_registers`, and the two places the type differs from a plain
/// function's.
#[test]
fn what_gcc_says_of_the_unsaved_conventions_is_said_in_its_words() {
    let cases: [(&str, &[&str]); 8] = [
        (
            "__attribute__((preserve_none)) int x;\n",
            &["warning: 'preserve_none' attribute only applies to function types"],
        ),
        (
            "__attribute__((no_callee_saved_registers)) int y;\n",
            &["warning: 'no_callee_saved_registers' attribute only applies to function types"],
        ),
        (
            "__attribute__((preserve_none(1))) void f(void);\n",
            &[
                "error: wrong number of arguments specified for 'preserve_none' attribute",
                "expected 0, found 1",
            ],
        ),
        (
            "__attribute__((preserve_none, no_callee_saved_registers)) void g(void);\n",
            &["error: 'no_callee_saved_registers' and 'preserve_none' attributes are not \
                 compatible"],
        ),
        (
            "__attribute__((preserve_none, no_caller_saved_registers)) void h(void);\n",
            &["error: 'no_caller_saved_registers' and 'preserve_none' attributes are not \
                 compatible"],
        ),
        (
            "__attribute__((no_callee_saved_registers)) void k(void) \
             __attribute__((no_caller_saved_registers));\n",
            &["error: 'no_caller_saved_registers' and 'no_callee_saved_registers' attributes \
                 are not compatible"],
        ),
        (
            "typedef void (__attribute__((preserve_none)) *pn_t)(void);\n\
             void plain(void);\npn_t q = plain;\n",
            &["incompatible pointer type", "void (__attribute__((preserve_none)) *)(void)"],
        ),
        (
            "void r(void);\n__attribute__((no_callee_saved_registers)) void r(void);\n",
            &["error: conflicting types for 'r'"],
        ),
    ];
    for (source, said) in cases {
        let (ok, _, err) = assembly("words", TARGET, source);
        for words in said {
            assert!(err.contains(words), "{source}: {words} not said:\n{err}");
        }
        // How severe an incompatible pointer is depends on the dialect, and is not asked here.
        if !said[0].starts_with("incompatible") {
            assert_eq!(ok, !said[0].starts_with("error"), "{source}:\n{err}");
        }
    }
}

/// With `ms_abi` gcc passes the arguments the Windows way and still keeps nothing, which is a
/// convention of its own, so it is refused rather than built as either. With `sysv_abi`, which is
/// the target's own, it is no conflict.
#[test]
fn an_unsaved_convention_beside_ms_abi_is_refused() {
    let (ok, _, err) = assembly(
        "ms",
        TARGET,
        "__attribute__((preserve_none, ms_abi)) long pm(long a) { return a; }\n",
    );
    assert!(!ok);
    assert!(err.contains("'preserve_none' with 'ms_abi' is not supported yet"), "{err}");
    let out = listing(
        "sysv",
        "__attribute__((sysv_abi, preserve_none)) long ps(long);\n\
         long call(void) { return ps(4); }\n",
    );
    assert!(has(&body(&out, "call"), &["$4", "%r12"]), "{out}");
}

/// `__has_attribute` answers yes on x86-64 ELF alone, and anywhere else the attribute is refused,
/// since dropping it would call the function the wrong way.
#[test]
fn the_unsaved_conventions_are_there_on_x86_64_elf_alone() {
    let source = "#if __has_attribute(preserve_none) \
                  && __has_attribute(__no_callee_saved_registers__)\nint asked;\n#endif\n";
    let (ok, out, err) = assembly("asked", TARGET, source);
    assert!(ok && err.is_empty(), "{err}");
    assert!(out.contains("asked"), "{out}");
    for target in ["i686-unknown-linux-gnu", "x86_64-w64-mingw32", "aarch64-unknown-linux-gnu"] {
        let (ok, out, err) = assembly("asked", target, source);
        assert!(ok && err.is_empty(), "{target}: {err}");
        assert!(!out.contains("asked"), "{target}:\n{out}");
        for name in ["preserve_none", "no_callee_saved_registers"] {
            let (ok, _, err) =
                assembly("refused", target, &format!("__attribute__(({name})) void f(void);\n"));
            assert!(!ok, "{target}: {name}");
            assert!(err.contains(&format!("'{name}' attribute is not supported")), "{err}");
        }
    }
}

/// Every function the program calls writes all the registers its convention lets it, and the
/// callers keep values across each call: a plain one calling both kinds, one of the kinds calling a
/// plain one, a structure in two registers, one coming back through memory, a variadic definition
/// with more arguments than registers, and a call through a pointer.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROGRAM: &str = r#"
#include <stdarg.h>
#include <stdio.h>
#define TRASH __asm__ volatile("movq $-1, %%rbx\n\tmovq $-1, %%r12\n\tmovq $-1, %%r13\n\t" \
    "movq $-1, %%r14\n\tmovq $-1, %%r15" ::: "rbx", "r12", "r13", "r14", "r15")
struct two { long a, b; };
struct big { long a, b, c; };
__attribute__((no_callee_saved_registers)) long n(long a, long b) { TRASH; return a * 10 + b; }
__attribute__((preserve_none)) long p(long a, long b, long c, long d, long e, long f, long g,
                                      double x) {
  TRASH;
  return a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g + (long)x;
}
__attribute__((preserve_none)) long ps(struct two t, long c) {
  TRASH;
  return t.a * 100 + t.b * 10 + c;
}
__attribute__((preserve_none)) struct big pb(long a) {
  TRASH;
  struct big r = {a, a + 1, a + 2};
  return r;
}
__attribute__((preserve_none)) long pv(int count, ...) {
  va_list ap;
  va_start(ap, count);
  long sum = 0;
  for (int i = 0; i < count; i++) sum = sum * 10 + va_arg(ap, long);
  va_end(ap);
  TRASH;
  return sum;
}
typedef long (__attribute__((preserve_none)) *pp)(long, long, long, long, long, long, long, double);
long keep(long a, long b, long c, long d, long e) {
  long x = n(a, b);
  long y = n(c, d);
  return x + y + a * b * c * d * e;
}
__attribute__((preserve_none)) long chain(long a) {
  long x = keep(a, 1, 1, 1, 1);
  return x + n(a, a) + a;
}
int main(void) {
  pp volatile f = p;
  long k = keep(1, 2, 3, 4, 5);
  long q = p(1, 2, 3, 4, 5, 6, 7, 8.75);
  long s = ps((struct two){1, 2}, 3);
  struct big b = pb(5);
  long v4 = pv(4, 1L, 2L, 3L, 4L);
  long v7 = pv(7, 1L, 2L, 3L, 4L, 5L, 6L, 7L);
  long ff = f(1, 2, 3, 4, 5, 6, 7, 8.75);
  long c = chain(2);
  printf("%ld %ld %ld %ld %ld %ld %ld %ld %ld %ld %ld\n", n(3, 4), k, q, s, b.a, b.c, v4, v7, ff,
         c, b.b);
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_caller_of_one_that_keeps_nothing_gets_its_values_back() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for flags in [&["-O0"][..], &["-O2"], &["-O2", "-fPIC", "-pie"]] {
        let mut args = flags.to_vec();
        args.extend(["a.c", "-o", "prog"]);
        let (ok, _, said) = run(&dir, &args);
        assert!(ok, "{flags:?}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{flags:?}: {:?} {stdout}", out.status.code());
        assert_eq!(stdout, "34 166 148 123 5 7 1234 1234567 148 58 6\n", "{flags:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
