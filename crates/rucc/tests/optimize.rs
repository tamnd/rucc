//! What `__attribute__((optimize (...)))` does to the IR of the one function it is written on.
//!
//! gcc reads the level off the function rather than off the unit, so a function that asks for
//! `-O0` in a unit built at `-O2` is compiled the way `-O0` compiles it: nothing is inlined into
//! it, and a question like `__builtin_constant_p` is answered the way that level answers it. These
//! read the IR rather than run a program, so they hold on any host.

use std::path::PathBuf;
use std::process::Command;

/// The target the IR is asked for. Nothing here depends on which one it is.
const TARGET: &str = "x86_64-linux-gnu";

/// The source, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-optimize-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The IR the compiler produced for the source at `-O2`.
fn ir(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "--emit=ir", "-w", "-o", "-"])
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

/// The line a function starts with, which is where its attributes are written.
fn head<'a>(text: &'a str, name: &str) -> &'a str {
    let open = format!("func @{name}(");
    text.lines().map(str::trim).find(|line| line.starts_with(&open)).unwrap_or("")
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

/// A function that asks for `-O0`, by name or by number, keeps its call to a small `static inline`
/// function that `-O2` inlines everywhere else, and its `__builtin_constant_p` of a variable that
/// holds a constant is zero where the same question elsewhere is one. Both are what gcc 16 does
/// with the same unit at `-O2`. A level above the unit's is accepted and changes nothing, and
/// `always_inline` written beside it wins, since the promise made to the callers is the one that
/// has to be kept.
#[test]
fn a_function_that_asks_for_o0_is_compiled_at_o0() {
    let text = ir(
        "levels",
        r#"
static inline int twice (int x) { return x + x; }
__attribute__((optimize ("O0"))) int held (int x) { return twice (x) + 12; }
__attribute__((optimize (0))) int number (int x) { return twice (x) + 12; }
__attribute__((optimize ("-O0"))) int dashed (int x) { return twice (x) + 12; }
__attribute__((optimize ("O3"))) int higher (int x) { return twice (x) + 12; }
int plain (int x) { return twice (x) + 12; }
__attribute__((optimize ("O0"))) int asked (void) { int v = 3; return __builtin_constant_p (v); }
int answered (void) { int v = 3; return __builtin_constant_p (v); }
static inline __attribute__((always_inline, optimize ("O0"))) int kept (int x) { return x + 1; }
int caller (int x) { return kept (x); }
"#,
    );
    for name in ["held", "number", "dashed"] {
        assert!(head(&text, name).contains("optnone"), "{name}: {text}");
        assert!(body(&text, name).iter().any(|line| line.contains("@twice(")), "{name}: {text}");
    }
    for name in ["higher", "plain", "caller"] {
        assert!(!head(&text, name).contains("optnone"), "{name}: {text}");
        assert!(!body(&text, name).iter().any(|line| line.contains("call")), "{name}: {text}");
    }
    assert!(body(&text, "asked").iter().any(|line| line.ends_with("iconst.i32 0")), "{text}");
    assert!(body(&text, "answered").iter().any(|line| line.ends_with("iconst.i32 1")), "{text}");
}

/// A function that asks for `wrapv` is compiled as if the unit had `-fwrapv`: its signed additions
/// do not say they cannot wrap, where the same line in a function under the rule does. A body that
/// wraps still wraps once it is inlined into one that does not, which is why the answer is carried
/// on the instructions rather than on the function.
#[test]
fn a_function_that_asks_for_wrapv_wraps() {
    let text = ir(
        "wrapv",
        r#"
__attribute__((optimize ("wrapv"))) int wraps (int x) { return x + 1 > x; }
__attribute__((optimize ("-fwrapv"))) int dashed (int x) { return x + 1 > x; }
int plain (int x) { return x + 1 > x; }
static inline __attribute__((optimize ("wrapv"))) int inner (int x) { return x + 1 > x; }
int outer (int x) { return inner (x); }
"#,
    );
    for name in ["wraps", "dashed", "outer"] {
        let lines = body(&text, name);
        assert!(lines.iter().any(|line| line.contains("= add ")), "{name}: {text}");
        assert!(!lines.iter().any(|line| line.contains("add.nsw")), "{name}: {text}");
        assert!(!lines.iter().any(|line| line.contains("call")), "{name}: {text}");
    }
    assert!(body(&text, "plain").iter().any(|line| line.contains("add.nsw")), "{text}");
}

/// A function that asks for `no-tree-loop-distribute-patterns` keeps the loop that clears a range,
/// which `-O2` turns into a `memset` everywhere else. It is what a freestanding `memset`
/// written under another name says so that it is not compiled into a call to itself, and a caller
/// the body is inlined into keeps the loop as well.
#[test]
fn a_function_that_asks_for_its_loops_keeps_them() {
    let text = ir(
        "loops",
        r#"
__attribute__((optimize ("no-tree-loop-distribute-patterns")))
void clear (char *p, int n) { for (int i = 0; i < n; i++) p[i] = 0; }
void plain (char *p, int n) { for (int i = 0; i < n; i++) p[i] = 0; }
static inline __attribute__((optimize ("-fno-tree-loop-distribute-patterns")))
void inner (char *p, int n) { for (int i = 0; i < n; i++) p[i] = 0; }
void outer (char *p, int n) { inner (p, n); }
"#,
    );
    for name in ["clear", "outer"] {
        assert!(head(&text, name).contains("no_loop_idiom"), "{name}: {text}");
        assert!(
            !body(&text, name).iter().any(|line| line.starts_with("memset ")),
            "{name}: {text}"
        );
    }
    assert!(body(&text, "plain").iter().any(|line| line.starts_with("memset ")), "{text}");
}
