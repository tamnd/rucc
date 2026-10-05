//! `__attribute__((no_reorder))`, end to end: the functions and objects that say it are written
//! first and in the order the source wrote them, the rest in the order `-ftoplevel-reorder` puts
//! them in, and what gcc says about the attribute on the way, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, since the listing is read.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-no-reorder-{}-{what}", std::process::id()));
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

/// The names whose labels the listing has, in the order it has them.
fn labels<'a>(text: &str, names: &[&'a str]) -> Vec<&'a str> {
    let mut found: Vec<(usize, &str)> = names
        .iter()
        .map(|&name| {
            let at = text.find(&format!("\n{name}:\n"));
            (at.unwrap_or_else(|| panic!("{name} is in the listing:\n{text}")), name)
        })
        .collect();
    found.sort_unstable();
    found.into_iter().map(|(_, name)| name).collect()
}

/// Objects in each of the sections an object can land in and functions, some of each saying it.
const UNITS: &str = "__attribute__((no_reorder)) int a1 = 1;\n\
    int a2 = 2;\n\
    __attribute__((no_reorder)) int a3 = 3;\n\
    int a4 = 4;\n\
    __attribute__((no_reorder)) int bss1;\n\
    int bss2;\n\
    __attribute__((__no_reorder__)) int bss3;\n\
    [[gnu::no_reorder]] const int r1 = 1;\n\
    const int r2 = 2;\n\
    __attribute__((no_reorder)) const int r3 = 3;\n\
    extern int r4 __attribute__((no_reorder));\n\
    const int r4 = 4;\n\
    int f1(void) { return a2; }\n\
    __attribute__((no_reorder)) int f2(void) { return a1; }\n\
    __attribute__((no_reorder)) int f3(void) { return f1() + a3; }\n\
    int f4(void) { return a4; }\n\
    __attribute__((no_reorder)) int f5(void);\n\
    int f5(void) { return f4() + bss1 + bss2 + bss3 + r1 + r2 + r3 + r4; }\n";

/// gcc 13's order at `-O2`, where everything else is turned around.
#[test]
fn what_says_it_is_written_first_in_the_order_the_source_wrote() {
    let (ok, text, err) = assembly("units", &["-O2"], UNITS);
    assert!(ok, "{err}");
    assert_eq!(labels(&text, &["a1", "a2", "a3", "a4"]), ["a1", "a3", "a4", "a2"]);
    assert_eq!(labels(&text, &["bss1", "bss2", "bss3"]), ["bss1", "bss3", "bss2"]);
    assert_eq!(labels(&text, &["r1", "r2", "r3", "r4"]), ["r1", "r3", "r4", "r2"]);
    let functions = labels(&text, &["f1", "f2", "f3", "f4", "f5"]);
    assert_eq!(functions[..3], ["f2", "f3", "f5"], "{functions:?}");
}

/// Without `-ftoplevel-reorder` everything is where the source wrote it, which is `-O0` and the
/// flag's `-fno-` form.
#[test]
fn without_the_reordering_the_source_s_order_is_kept() {
    for flags in [&["-O0"][..], &["-O2", "-fno-toplevel-reorder"]] {
        let (ok, text, err) = assembly("kept", flags, UNITS);
        assert!(ok, "{flags:?}: {err}");
        assert_eq!(labels(&text, &["a1", "a2", "a3", "a4"]), ["a1", "a2", "a3", "a4"], "{flags:?}");
        assert_eq!(labels(&text, &["r1", "r2", "r3", "r4"]), ["r1", "r2", "r3", "r4"], "{flags:?}");
        let functions = labels(&text, &["f1", "f2", "f3", "f4", "f5"]);
        assert_eq!(functions, ["f1", "f2", "f3", "f4", "f5"], "{flags:?}");
    }
}

/// What gcc 13 says about the attribute, in its words, and whether it stops there.
#[test]
fn no_reorder_is_checked_in_gcc_s_words() {
    let top = "warning: 'no_reorder' attribute only affects top level objects";
    for (source, said, error) in [
        ("struct s { int a __attribute__((no_reorder)); };\n", top, false),
        ("typedef int t __attribute__((no_reorder));\n", top, false),
        ("void g(int p __attribute__((no_reorder))) { (void)p; }\n", top, false),
        ("void g(__attribute__((no_reorder)) int p);\n", top, false),
        (
            "__attribute__((no_reorder(1))) int z;\n",
            "error: wrong number of arguments specified for 'no_reorder' attribute",
            true,
        ),
        ("__attribute__((no_reorder(1))) int z;\n", "expected 0, found 1", true),
    ] {
        let (ok, _, err) = assembly("words", &[], source);
        assert_eq!(ok, !error, "{source}\n{err}");
        assert!(err.contains(said), "{source}\nwanted {said:?}, got:\n{err}");
        assert_eq!(err.matches("warning:").count(), usize::from(!error), "{source}\n{err}");
    }
    // Nothing to say about an object in a block, `static` or not, a pointer to a function, an
    // object in a section of its own or a declaration of something defined elsewhere, and the
    // attribute is there to ask about.
    let quiet = "void h(void) { __attribute__((no_reorder)) int x = 0; (void)x; }\n\
        int k(void) { __attribute__((no_reorder)) static int y; return y; }\n\
        void (*fp)(void) __attribute__((no_reorder));\n\
        __attribute__((no_reorder, section(\".foo\"))) int w = 1;\n\
        __attribute__((no_reorder)) extern int e;\n\
        __attribute__((no_reorder)) int ef(void);\n\
        int use(void) { return e + ef(); }\n\
        _Static_assert(__has_attribute(no_reorder), \"no_reorder\");\n";
    let (ok, _, err) = assembly("quiet", &["-Wall", "-Wextra"], quiet);
    assert!(ok, "{err}");
    assert_eq!(err, "");
}
