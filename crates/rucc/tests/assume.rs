//! `__attribute__((assume(expression)));`, end to end: a promise the optimizer reads the way it
//! reads the `__builtin_unreachable` spelling of the same promise, an expression that is never
//! computed, and what gcc says about the attribute on the way, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, since the listing is read.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-assume-{}-{what}", std::process::id()));
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

/// The assembly of one source under those flags, and what was said.
fn assembly(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={TARGET}");
    let mut args = vec![target.as_str(), "-S", "-o", "-"];
    args.extend_from_slice(flags);
    args.push("a.c");
    let said = run(&dir, &args);
    let _ = std::fs::remove_dir_all(&dir);
    said
}

/// The listing of a source that has to compile without a word.
fn listing(what: &str, flags: &[&str], source: &str) -> String {
    let (ok, text, err) = assembly(what, flags, source);
    assert!(ok, "{flags:?}: {err}\n{source}");
    assert_eq!(err, "", "{flags:?}\n{source}");
    text
}

/// Functions with a promise in them, each written three ways: with the attribute, with the
/// spelling programs used before it, and with neither. The promise is between the `@`s.
const PURE: &str = "int f(int x) { @x > 0@ return x / 4; }\n\
    int g(int x) { @x == 3@ return x; }\n\
    int k(int *p) { @p != 0@ return p ? *p : 9; }\n\
    unsigned m(unsigned x) { @x < 8@ return x % 8; }\n\
    int n(int x, int y) { @x > 0 && y > 0@ return x / y; }\n";

/// The source with each promise written one of the three ways.
fn promised(source: &str, how: &str) -> String {
    let mut text = String::new();
    for (index, part) in source.split('@').enumerate() {
        if index % 2 == 0 {
            text.push_str(part);
            continue;
        }
        match how {
            "attribute" => text.push_str(&format!("__attribute__((assume({part})));")),
            "unreachable" => text.push_str(&format!("if (!({part})) __builtin_unreachable();")),
            _ => {}
        }
    }
    text
}

#[test]
fn an_assumption_is_a_promise_the_optimizer_reads_and_never_computed() {
    // Read as the older spelling of the same promise when the optimizer runs.
    let assumed = listing("pure", &["-O2"], &promised(PURE, "attribute"));
    let unreachable = listing("unreachable", &["-O2"], &promised(PURE, "unreachable"));
    assert_eq!(assumed, unreachable);
    // And nothing at all when it does not, which is gcc's `-O0`.
    let assumed = listing("pure-o0", &["-O0"], &promised(PURE, "attribute"));
    let plain = listing("plain-o0", &["-O0"], &promised(PURE, "neither"));
    assert_eq!(assumed, plain);
    // What would do something is never done: the call is not made, the increment does not
    // happen and the `volatile` object is not read, at any level.
    let effects = "extern int g(int);\n\
        int c(int x) { __attribute__((assume(g(x) > 0))); return x; }\n\
        int i(int x) { __attribute__((assume(++x > 0))); return x; }\n\
        int j(int x) { __attribute__((assume(x-- > 0), assume(g(x)))); return x; }\n\
        int v(volatile int *p, int x) { __attribute__((assume(*p == x))); return x; }\n\
        int s(int x) { __attribute__((assume(({ x = 1; x; })))); return x; }\n";
    let without = "extern int g(int);\n\
        int c(int x) { return x; }\n\
        int i(int x) { return x; }\n\
        int j(int x) { return x; }\n\
        int v(volatile int *p, int x) { return x; }\n\
        int s(int x) { return x; }\n";
    for level in ["-O0", "-O2"] {
        let assumed = listing("effects", &[level], effects);
        assert!(!assumed.contains("call"), "{level}:\n{assumed}");
        assert_eq!(assumed, listing("without", &[level], without), "{level}");
    }
    // The other two spellings, a lone name, and a statement position of its own.
    let spelled = "int a(int x) { __attribute__((__assume__(x > 0))); return x / 4; }\n\
        int b(int x) { [[gnu::assume(x > 0)]]; return x / 4; }\n\
        int d(_Bool x) { __attribute__((assume(x))); return x; }\n\
        int e(int x, int y) { if (y) __attribute__((assume(x > 0))); return x / 4; }\n";
    let older = "int a(int x) { if (!(x > 0)) __builtin_unreachable(); return x / 4; }\n\
        int b(int x) { if (!(x > 0)) __builtin_unreachable(); return x / 4; }\n\
        int d(_Bool x) { if (!(x)) __builtin_unreachable(); return x; }\n\
        int e(int x, int y) { if (y) if (!(x > 0)) __builtin_unreachable(); return x / 4; }\n";
    assert_eq!(listing("spelled", &["-O2"], spelled), listing("older", &["-O2"], older));
}

/// What gcc 13 says about the attribute, in its words, and whether it stops there.
#[test]
fn assume_is_checked_in_gcc_s_words() {
    let arity = "error: wrong number of arguments specified for 'assume' attribute";
    let ignored = "warning: 'assume' attribute ignored";
    let followed = "warning: 'assume' attribute not followed by ';'";
    for (source, said, errors, warnings) in [
        ("void a(int x) { __attribute__((assume)); (void)x; }\n", arity, 1, 0),
        ("void a(int x) { __attribute__((assume)); (void)x; }\n", "expected 1, found 0", 1, 0),
        ("void a(int x) { __attribute__((assume(x, x))); }\n", "expected 1, found 2", 1, 0),
        (
            "struct s { int a; } sv;\nvoid a(void) { __attribute__((assume(sv))); }\n",
            "error: used struct type value where scalar is required",
            1,
            0,
        ),
        (
            "void a(void) { __attribute__((assume(\"s\"))); }\n",
            "error: used array that cannot be converted to pointer where scalar is required",
            1,
            0,
        ),
        (
            "void a(void) { __attribute__((assume((void)0))); }\n",
            "error: void value not ignored as it ought to be",
            1,
            0,
        ),
        ("__attribute__((assume(1)));\n", "warning: 'assume' attribute at top level", 0, 1),
        ("__attribute__((assume(1))) int v;\n", followed, 0, 2),
        ("__attribute__((assume(1))) int v;\n", ignored, 0, 2),
        ("__attribute__((assume(1))) void f(void) {}\n", followed, 0, 2),
        ("void f(int x) { __attribute__((assume(x > 0))) int y = x; (void)y; }\n", followed, 0, 2),
        ("void f(int __attribute__((assume(1))) p) { (void)p; }\n", ignored, 0, 1),
        ("void f(int x) { [[assume(x > 0)]]; }\n", ignored, 0, 1),
    ] {
        let (ok, _, err) = assembly("words", &["-O2"], source);
        assert_eq!(ok, errors == 0, "{source}\n{err}");
        assert!(err.contains(said), "{source}\nwanted {said:?}, got:\n{err}");
        assert_eq!(err.matches("error:").count(), errors, "{source}\n{err}");
        assert_eq!(err.matches("warning:").count(), warnings, "{source}\n{err}");
    }
    // An assignment is not a conditional expression, so it does not parse, as in gcc.
    let (ok, _, err) =
        assembly("assign", &[], "void f(int x) { __attribute__((assume(x = 1))); }\n");
    assert!(!ok, "{err}");
    // Nothing to say about two in one list, one in front of a case label, a name the promise
    // uses and nothing else does, or about asking for it.
    let quiet = "int f(int x) { [[gnu::assume(x > 0), gnu::assume(x < 9)]]; return x; }\n\
        int g(int x) { switch (x) { case 1: __attribute__((assume(x == 1))); case 2: break; }\n\
            return 0; }\n\
        void h(int x) { __attribute__((assume(x > 0))); }\n\
        _Static_assert(__has_attribute(assume), \"gcc 13\");\n";
    for level in ["-O0", "-O2"] {
        let (ok, _, err) = assembly("quiet", &[level, "-Wall", "-Wextra"], quiet);
        assert!(ok, "{level}: {err}");
        assert_eq!(err, "", "{level}");
    }
    // gcc 12 has never heard of it.
    let old = "void f(int x) { __attribute__((assume(x > 0))); }\n";
    let (ok, _, err) = assembly("old", &["-fgnuc-version=12.2.0"], old);
    assert!(ok, "{err}");
    assert!(err.contains("warning: 'assume' attribute directive ignored"), "{err}");
}
