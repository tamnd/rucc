//! `__attribute__((sseregparm))`, end to end: a 32-bit x86 function type whose floats travel in
//! SSE registers, which this compiler's i386 code never has, so each definition and each call is
//! refused as gcc built without SSE refuses it, and what gcc says about the attribute elsewhere.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const LINUX: &str = "i686-unknown-linux-gnu";
const MINGW: &str = "i686-w64-windows-gnu";
const X86_64: &str = "x86_64-unknown-linux-gnu";
const WIN64: &str = "x86_64-w64-windows-gnu";

/// A directory of this test's own, empty. The tests in this file run on threads of one process,
/// so the process id alone would give two of them the same one.
fn dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-sse-regparm-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

fn run(dir: &Path, target: &str, flags: &[&str], source: &str) -> (bool, String) {
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-S", "-o", "-"])
        .args(flags)
        .arg("a.c")
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Declaring one, a pointer to one and a typedef of one is quiet, and so is taking the address.
/// Defining one and calling one, by name or through a pointer, is gcc's error for each.
#[test]
fn sseregparm_is_refused_without_sse_as_gcc_refuses_it() {
    let declared = "__attribute__((sseregparm)) double f(double);\n\
        double (*p)(double) __attribute__((sseregparm)) = f;\n\
        typedef float sse_t(float) __attribute__((__sseregparm__));\n\
        [[gnu::sseregparm]] float g(float);\n\
        sse_t *q = g;\n\
        _Static_assert(__has_attribute(sseregparm), \"gcc 4.1\");\n";
    let used = format!(
        "{declared}\
         double by_name(void) {{ return f(1.0); }}\n\
         double through(void) {{ return p(1.0); }}\n\
         __attribute__((sseregparm)) double k(double x) {{ return x; }}\n"
    );
    let said = "with attribute sseregparm without SSE/SSE2 enabled";
    for target in [LINUX, MINGW] {
        let (ok, err) = run(&dir(), target, &["-Wall", "-Wextra", "-std=gnu23"], declared);
        assert!(ok, "{target}: {err}");
        assert_eq!(err, "", "{target}");

        let (ok, err) = run(&dir(), target, &["-std=gnu23"], &used);
        assert!(!ok, "{target}: {err}");
        assert_eq!(err.matches("error:").count(), 3, "{target}: {err}");
        assert_eq!(err.matches(said).count(), 3, "{target}: {err}");
        assert!(err.contains(&format!("error: calling 'f' {said}")), "{target}: {err}");
        assert!(err.contains(&format!("error: calling 'k' {said}")), "{target}: {err}");
        // A call through a pointer has no name to give, so gcc gives the type.
        assert!(!err.contains("calling 'p'"), "{target}: {err}");
    }

    // A function without a float to pass is still refused, as gcc refuses it.
    let source = "__attribute__((sseregparm)) int n(int x) { return x; }\n";
    let (ok, err) = run(&dir(), LINUX, &[], source);
    assert!(!ok, "{err}");
    assert!(err.contains(&format!("error: calling 'n' {said}")), "{err}");
}

/// What gcc says about the attribute where it is wrong or means nothing.
#[test]
fn sseregparm_is_checked_in_gcc_s_words() {
    let name = "'sseregparm' attribute";
    let wrong = format!("error: wrong number of arguments specified for {name}");
    for (target, source, said, errors, warnings) in [
        (LINUX, "__attribute__((sseregparm(1))) double f(double);\n", wrong.clone(), 1, 0),
        (
            LINUX,
            "__attribute__((sseregparm(1, 2))) double f(double);\n",
            "expected 0, found 2".to_owned(),
            1,
            0,
        ),
        (
            LINUX,
            "int v __attribute__((sseregparm));\n",
            format!("warning: {name} only applies to function types"),
            0,
            1,
        ),
        // Part of the type, so a declaration each way is two types.
        (
            LINUX,
            "double f(double);\n__attribute__((sseregparm)) double f(double);\n",
            "error: conflicting types for 'f'".to_owned(),
            1,
            0,
        ),
        (
            X86_64,
            "__attribute__((sseregparm)) double f(double);\ndouble g(void) { return f(1.0); }\n",
            format!("warning: {name} ignored"),
            0,
            1,
        ),
        (X86_64, "__attribute__((sseregparm(1))) double f(double);\n", wrong.clone(), 1, 0),
    ] {
        let (ok, err) = run(&dir(), target, &[], source);
        assert_eq!(ok, errors == 0, "{source}\n{err}");
        assert!(err.contains(&said), "{source}\nwanted {said:?}, got:\n{err}");
        assert_eq!(err.matches("error:").count(), errors, "{source}\n{err}");
        assert_eq!(err.matches("warning:").count(), warnings, "{source}\n{err}");
    }

    // A pointer to one is not a pointer to a plain one.
    let source = "__attribute__((sseregparm)) double f(double);\ndouble (*p)(double) = f;\n";
    let (_, err) = run(&dir(), LINUX, &[], source);
    assert!(err.contains("from incompatible pointer type"), "{err}");

    // On x86-64 Windows the attribute names nothing and gcc drops it without a word.
    let source =
        "__attribute__((sseregparm)) double f(double);\ndouble g(void) { return f(1.0); }\n";
    let (ok, err) = run(&dir(), WIN64, &["-Wall", "-Wextra"], source);
    assert!(ok, "{err}");
    assert_eq!(err, "");

    // gcc 3.4 has never heard of it.
    let (ok, err) = run(&dir(), LINUX, &["-fgnuc-version=3.4.6"], source);
    assert!(ok, "{err}");
    assert!(err.contains(&format!("warning: {name} directive ignored")), "{err}");
    assert!(!err.contains("SSE"), "{err}");
}
