//! `__attribute__((expected_throw))`, end to end: gcc 14 takes it on a function, drops it with a
//! warning anywhere else and refuses an argument, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A directory of this test's own, empty. The tests in this file run on threads of one process,
/// so the process id alone would give two of them the same one.
fn dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-expected-throw-{}-{n}", std::process::id()));
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

#[test]
fn expected_throw_is_taken_on_a_function_and_warned_about_elsewhere() {
    let quiet = "__attribute__((expected_throw)) void fail(const char *);\n\
        void __attribute__((__expected_throw__)) fail2(int);\n\
        [[gnu::expected_throw]] void fail3(void);\n\
        __attribute__((expected_throw, noreturn)) void die(void) { for (;;) {} }\n\
        void check(int x) { if (x) fail(\"x\"); fail2(x); }\n\
        _Static_assert(__has_attribute(expected_throw), \"gcc 14\");\n";
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "i686-w64-windows-gnu"]
    {
        let (ok, err) = run(&dir(), target, &["-Wall", "-Wextra", "-std=gnu23"], quiet);
        assert!(ok, "{target}: {err}");
        assert_eq!(err, "", "{target}");
    }

    let target = "x86_64-unknown-linux-gnu";
    let name = "'expected_throw' attribute";
    for (source, said, errors, warnings) in [
        ("int v __attribute__((expected_throw));\n", format!("warning: {name} ignored"), 0, 1),
        ("typedef void f_t(void) __attribute__((expected_throw));\n", format!("warning: {name} ignored"), 0, 1),
        (
            "__attribute__((expected_throw(1))) void f(void);\n",
            format!("error: wrong number of arguments specified for {name}"),
            1,
            0,
        ),
        ("__attribute__((expected_throw(1, 2))) void f(void);\n", "expected 0, found 2".to_owned(), 1, 0),
    ] {
        let (ok, err) = run(&dir(), target, &[], source);
        assert_eq!(ok, errors == 0, "{source}\n{err}");
        assert!(err.contains(&said), "{source}\nwanted {said:?}, got:\n{err}");
        assert_eq!(err.matches("error:").count(), errors, "{source}\n{err}");
        assert_eq!(err.matches("warning:").count(), warnings, "{source}\n{err}");
    }

    // gcc 13 has never heard of it.
    let source = "__attribute__((expected_throw)) void f(void);\n";
    let (ok, err) = run(&dir(), target, &["-fgnuc-version=13.3.0"], source);
    assert!(ok, "{err}");
    assert!(err.contains(&format!("warning: {name} directive ignored")), "{err}");
}
