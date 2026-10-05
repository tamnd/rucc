//! `__attribute__((symver("name@node")))`, end to end: a second name for a function or an object
//! with a symbol version in its spelling, written back as the `.symver` gcc writes, and what gcc
//! says about the attribute on the way, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, since the listing is read.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-symver-{}-{what}", std::process::id()));
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

/// The assembly of one source for a target under those flags, and what was said.
fn assembly(what: &str, target: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={target}");
    let mut args = vec![target.as_str(), "-O2", "-S", "-o", "-"];
    args.extend_from_slice(flags);
    args.push("a.c");
    let said = run(&dir, &args);
    let _ = std::fs::remove_dir_all(&dir);
    said
}

/// The listing, from a compiler that took the fixture.
fn listing(what: &str, source: &str) -> String {
    let (ok, out, err) = assembly(what, TARGET, &[], source);
    assert!(ok, "{err}");
    out
}

/// gcc's manual's examples and the shapes beside them: a function, two versions of one, the
/// default version, an object, a constant, a version asked for on the declaration above the
/// definition, one on a second name `alias` made, which is how glibc and xz keep an old version
/// as well as the new, a function renamed with `__asm__`, and the base version.
const VERSIONS: &str = "__attribute__((symver(\"foo@VERS_1\"))) int foo_v1(void) { return 1; }\n\
    __attribute__((symver(\"foo@@VERS_2\"))) int foo_v2(void) { return 2; }\n\
    __attribute__((symver(\"bar@VERS_2\"), symver(\"bar@VERS_3\"))) int bar_v1(void) { return 3; }\n\
    __attribute__((symver(\"baz@VERS_1\"))) int var_v1 = 5;\n\
    __attribute__((symver(\"qux@VERS_1\"))) const char str_v1[] = \"x\";\n\
    __attribute__((__symver__(\"old@VERS_1\"))) int old(void);\n\
    int old(void) { return 4; }\n\
    int new_v2(void) { return 0; }\n\
    __attribute__((symver(\"new@VERS_3\"))) __attribute__((alias(\"new_v2\"))) int new_v3(void);\n\
    __attribute__((symver(\"renamed@VERS_1\"))) int renamed(void) __asm__(\"hidden_name\");\n\
    int renamed(void) { return 6; }\n\
    __attribute__((symver(\"base@\"))) int base(void) { return 7; }\n";

#[test]
fn a_version_is_a_second_name_written_back_as_the_symver_gcc_writes() {
    let text = listing("versions", VERSIONS);
    for line in [
        "foo_v1,foo@VERS_1",
        "foo_v2,foo@@VERS_2",
        "bar_v1,bar@VERS_2",
        "bar_v1,bar@VERS_3",
        "var_v1,baz@VERS_1",
        "str_v1,qux@VERS_1",
        "old,old@VERS_1",
        "new_v2,new@VERS_3",
        "hidden_name,renamed@VERS_1",
        "base,base@",
    ] {
        assert!(text.contains(&format!("\t.symver\t{line}\n")), "{line}:\n{text}");
    }
    assert!(!text.contains(".globl\tfoo@"), "{text}");
    // The same on the other ELF targets, which are the same assembler.
    for target in ["i686-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (ok, text, err) = assembly("elsewhere", target, &[], VERSIONS);
        assert!(ok, "{target}: {err}");
        assert!(text.contains("\t.symver\tfoo_v1,foo@VERS_1\n"), "{target}:\n{text}");
    }
}

/// The object the compiler writes itself has the versioned names in its symbol table, spelled the
/// way gas spells them, and nothing called `@@@`.
#[test]
fn the_object_has_each_versioned_name() {
    let dir = dir("object");
    std::fs::write(dir.join("a.c"), VERSIONS).expect("the fixture can be written");
    let target = format!("--target={TARGET}");
    let (ok, _, err) = run(&dir, &[&target, "-O2", "-c", "a.c", "-o", "a.o"]);
    assert!(ok, "{err}");
    let bytes = std::fs::read(dir.join("a.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    for name in ["foo@VERS_1", "foo@@VERS_2", "bar@VERS_2", "bar@VERS_3", "baz@VERS_1", "base@"] {
        let spelled = format!("\0{name}\0");
        assert!(
            bytes.windows(spelled.len()).any(|window| window == spelled.as_bytes()),
            "{name}"
        );
    }
}

/// What gcc 13 says about the attribute, in its words, and whether it stops there.
#[test]
fn the_attribute_is_checked_in_gcc_s_words() {
    for (source, said, error) in [
        (
            "struct s { int a __attribute__((symver(\"f@V1\"))); };\n",
            "warning: 'symver' attribute only applies to functions and variables",
            false,
        ),
        (
            "typedef int t __attribute__((symver(\"f@V1\")));\n",
            "warning: 'symver' attribute only applies to functions and variables",
            false,
        ),
        (
            "void h(void) { __attribute__((symver(\"f@V1\"))) int x = 0; (void)x; }\n",
            "warning: 'symver' attribute is only applicable to symbols",
            false,
        ),
        (
            "__attribute__((symver)) int f(void) { return 1; }\n",
            "error: wrong number of arguments specified for 'symver' attribute",
            true,
        ),
        ("__attribute__((symver)) int f(void) { return 1; }\n", "expected 1 or more, found 0", true),
        (
            "__attribute__((symver(1))) int f(void) { return 1; }\n",
            "error: 'symver' attribute argument not a string constant",
            true,
        ),
        (
            "__attribute__((symver(\"f\"))) int f(void) { return 1; }\n",
            "error: symver attribute argument must have format 'name@nodename'",
            true,
        ),
        (
            "__attribute__((symver(\"f@@@V1\"))) int f(void) { return 1; }\n",
            "error: 'symver' attribute argument 'f@@@V1' must contain one or two '@'",
            true,
        ),
        (
            "__attribute__((symver(\"1f@V1\"))) int f(void) { return 1; }\n",
            "error: 'symver' attribute argument '1f@V1' is not a name",
            true,
        ),
        (
            "int f(void) __attribute__((symver(\"f@V1\"))); int g(void) { return f(); }\n",
            "error: symbol needs to be defined to have a version",
            true,
        ),
        (
            "static int f(void) __attribute__((weakref(\"g\"), symver(\"f@V1\")));\n\
             int h(void) { return f(); }\n",
            "error: symbol needs to be defined to have a version",
            true,
        ),
        (
            "__attribute__((symver(\"f@V1\"))) static int f(void) { return 1; }\n\
             int g(void) { return f(); }\n",
            "error: versioned symbol must be public",
            true,
        ),
        (
            "void h(void) { static __attribute__((symver(\"f@V1\"))) int x = 0; (void)x; }\n",
            "error: versioned symbol must be public",
            true,
        ),
        (
            "__attribute__((symver(\"f@V1\"), visibility(\"hidden\"))) int f(void) { return 1; }\n",
            "error: versioned symbol must have default visibility",
            true,
        ),
        (
            "__attribute__((symver(\"f@V1\"))) int f(void) { return 1; }\n\
             __attribute__((symver(\"f@V1\"))) int g(void) { return 1; }\n",
            "a.c:1:37: error: duplicate definition of a symbol version",
            true,
        ),
        (
            "__attribute__((symver(\"f@V1\"))) int f(void) { return 1; }\n\
             __attribute__((symver(\"f@V1\"))) int g(void) { return 1; }\n",
            "a.c:2:37: note: same version was previously defined here",
            true,
        ),
    ] {
        let (ok, _, err) = assembly("words", TARGET, &[], source);
        assert_eq!(ok, !error, "{source}\n{err}");
        assert!(err.contains(said), "{source}\nwanted {said:?}, got:\n{err}");
    }
    for (flag, said) in [
        ("-fcommon", "error: common symbol cannot be versioned"),
        ("-fvisibility=hidden", "error: versioned symbol must have default visibility"),
    ] {
        let (ok, _, err) =
            assembly("flags", TARGET, &[flag], "__attribute__((symver(\"f@V1\"))) int v;\n");
        assert!(!ok, "{flag}");
        assert!(err.contains(said), "{flag}: wanted {said:?}, got:\n{err}");
    }
    // Nothing to say about a declaration nothing refers to, an inline definition this unit does
    // not emit, the same version asked for again on a second declaration of the same name, a
    // version and the default version of one name, or the attribute with its armour on.
    let quiet = "__attribute__((symver(\"d@V1\"))) int d(void);\n\
        __attribute__((symver(\"i@V1\"))) inline int i(void) { return 1; }\n\
        int calls(void) { return i(); }\n\
        __attribute__((symver(\"f@V1\"))) int f(void);\n\
        __attribute__((symver(\"f@V1\"))) int f(void) { return 1; }\n\
        __attribute__((__symver__(\"f@@V1\"))) int g(void) { return 1; }\n\
        __attribute__((symver(\"v@V1\"))) int v;\n\
        _Static_assert(__has_attribute(symver), \"symver\");\n";
    let (ok, text, err) = assembly("quiet", TARGET, &["-Wall", "-Wextra"], quiet);
    assert!(ok, "{err}");
    assert_eq!(err, "");
    assert!(text.contains("\t.symver\tf,f@V1\n") && text.contains("\t.symver\tg,f@@V1\n"));
    assert!(text.contains("\t.symver\tv,v@V1\n"), "{text}");
    assert!(!text.contains("i@V1") && !text.contains("d@V1"), "{text}");
    // Only ELF has symbol versions.
    let (ok, _, err) = assembly(
        "macho",
        "x86_64-apple-darwin",
        &[],
        "__attribute__((symver(\"f@V1\"))) int f(void) { return 1; }\n",
    );
    assert!(!ok);
    assert!(err.contains("error: symver is only supported on ELF platforms"), "{err}");
}
