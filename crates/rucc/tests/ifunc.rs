//! `__attribute__((ifunc("resolver")))`, end to end: the name written as an indirect function set
//! to its resolver, what gcc refuses and warns about on the way, and on an x86-64 Linux host the
//! program run, so that the dynamic linker's binding is what the calls reach.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, since the listing is read.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-ifunc-{}-{what}", std::process::id()));
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

/// The assembly of one source for a target, and what was said.
fn assembly(what: &str, target: &str, source: &str) -> (bool, String, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={target}");
    let said = run(&dir, &[&target, "-O2", "-S", "-o", "-", "a.c"]);
    let _ = std::fs::remove_dir_all(&dir);
    said
}

/// The directive lines of an assembly listing, trimmed, which is the shape asserted.
fn shape(out: &str) -> Vec<&str> {
    out.lines()
        .map(str::trim)
        .filter(|line| [".globl", ".weak", ".type", ".set"].iter().any(|d| line.starts_with(d)))
        .collect()
}

/// Two functions behind one resolver each, one of them `static`, and the resolver `static` and
/// called by nothing else, as glibc writes them.
const SOURCE: &str = "static int one(int x) { return x + 1; }\n\
    static int two(int x) { return x + 2; }\n\
    typedef int fn(int);\n\
    static fn *pick(void) { return two; }\n\
    static void *any(void) { return (void *)one; }\n\
    int f(int) __attribute__((ifunc(\"pick\")));\n\
    static int g(int) __attribute__((__ifunc__(\"any\")));\n\
    int call(int x) { return f(x) + g(x); }\n\
    int (*take(void))(int) { return g; }\n";

#[test]
fn the_name_is_an_indirect_function_set_to_its_resolver() {
    let (ok, out, err) = assembly("shape", TARGET, SOURCE);
    assert!(ok, "{err}");
    assert_eq!(err, "");
    let shape = shape(&out);
    for line in [".globl\tf", ".type\tf, @gnu_indirect_function", ".set\tf,pick"] {
        assert!(shape.contains(&line), "{line} missing:\n{out}");
    }
    for line in [".type\tg, @gnu_indirect_function", ".set\tg,any"] {
        assert!(shape.contains(&line), "{line} missing:\n{out}");
    }
    assert!(!shape.contains(&".globl\tg"), "{out}");
    // The resolvers are kept though nothing calls them, and stay local.
    assert!(out.contains("pick:") && out.contains("any:"), "{out}");
    assert!(!shape.contains(&".globl\tpick") && !shape.contains(&".globl\tany"), "{out}");
}

#[test]
fn ifunc_is_checked_in_gcc_s_words() {
    for (source, said, error) in [
        (
            "static void *r(void) { return 0; }\nint v __attribute__((ifunc(\"r\")));\n",
            "'ifunc' attribute ignored",
            false,
        ),
        (
            "static void *r(void) { return 0; }\n\
             typedef int t(void) __attribute__((ifunc(\"r\")));\n",
            "'ifunc' attribute ignored",
            false,
        ),
        (
            "static void *r(void) { return 0; }\n\
             int f(void) { int g(int) __attribute__((ifunc(\"r\"))); return g(1); }\n",
            "'ifunc' attribute ignored",
            false,
        ),
        (
            "int f(int) __attribute__((ifunc));\n",
            "wrong number of arguments specified for 'ifunc' attribute",
            true,
        ),
        (
            "static void *r(void) { return 0; }\nint f(int) __attribute__((ifunc(\"r\", \"r\")));\n",
            "expected 1, found 2",
            true,
        ),
        (
            "static void *r(void) { return 0; }\nint f(int) __attribute__((ifunc(r)));\n",
            "attribute 'ifunc' argument not a string",
            true,
        ),
        (
            "int f(int) __attribute__((ifunc(\"missing\")));\n",
            "'f' is aliased to undefined symbol 'missing'",
            true,
        ),
        (
            "static long r(void) { return 0; }\nint f(int) __attribute__((ifunc(\"r\")));\n",
            "'ifunc' resolver for 'f' must return 'int (*)(int)'",
            true,
        ),
        (
            "static int (*r(void))(long) { return 0; }\nint f(int) __attribute__((ifunc(\"r\")));\n",
            "'ifunc' resolver for 'f' should return 'int (*)(int)'",
            false,
        ),
        (
            "int v = 1;\nint f(int) __attribute__((ifunc(\"v\")));\n",
            "'f' alias between function and variable is not supported",
            true,
        ),
        (
            "static void *r(void) { return 0; }\n\
             int f(int) __attribute__((ifunc(\"r\")));\nint f(int x) { return x; }\n",
            "redefinition of 'f'",
            true,
        ),
        (
            "static void *r(void) { return 0; }\n\
             int f(int) __attribute__((ifunc(\"r\"), alias(\"r\")));\n",
            "'f' defined both normally and as 'alias' attribute",
            true,
        ),
    ] {
        let (ok, _, err) = assembly("words", TARGET, source);
        assert_eq!(ok, !error, "{source}\n{err}");
        assert!(err.contains(said), "{source}\nwanted {said:?}, got:\n{err}");
    }
    // `void *` is taken as it is, and so is a resolver that takes arguments, as in gcc.
    let source = "static void *r(int x) { return 0; }\nint f(int) __attribute__((ifunc(\"r\")));\n";
    let (ok, _, err) = assembly("quiet", TARGET, source);
    assert!(ok, "{err}");
    assert_eq!(err, "");
}

#[test]
fn a_target_without_the_symbol_type_refuses_it() {
    let source = "static void *r(void) { return 0; }\nint f(int) __attribute__((ifunc(\"r\")));\n";
    for target in ["x86_64-w64-mingw32", "x86_64-apple-darwin"] {
        let (ok, _, err) = assembly("elsewhere", target, source);
        assert!(!ok, "{target} took it");
        assert!(err.contains("'ifunc' is not supported on this target"), "{target}: {err}");
    }
    let (ok, out, err) = assembly("aarch64", "aarch64-unknown-linux-gnu", source);
    assert!(ok, "{err}");
    assert!(out.contains("gnu_indirect_function"), "{out}");
}

/// The program run: each call reaches what its resolver picked, by name and through a pointer.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn the_dynamic_linker_binds_the_call_to_what_the_resolver_picked() {
    let dir = dir("run");
    let program = format!(
        "{SOURCE}#include <stdio.h>\n\
         int main(void) {{ int (*h)(int) = f; printf(\"%d %d %d\\n\", call(10), h(10), take()(10)); \
         return 0; }}\n"
    );
    std::fs::write(dir.join("a.c"), program).expect("the fixture can be written");
    for flags in [&["-O0"][..], &["-O2"], &["-O2", "-fPIC", "-pie"], &["-O1", "-static"]] {
        let mut args = flags.to_vec();
        args.extend(["a.c", "-o", "prog"]);
        let (ok, _, said) = run(&dir, &args);
        assert!(ok, "{flags:?}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{flags:?}: {stdout}");
        assert_eq!(stdout, "23 12 11\n", "{flags:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
