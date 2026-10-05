//! `__attribute__((force_align_arg_pointer))`, end to end: a function that does not trust its
//! caller to have aligned the stack aligns its own frame when it calls anything or keeps something
//! that wants more than a word, on x86-64 and on i386, and what gcc says about the attribute on
//! the way, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The two targets gcc has the attribute on, written down rather than taken from the host, since
/// the listing is read, with how the realignment is spelled on each.
const TARGETS: [(&str, &str); 2] =
    [("x86_64-unknown-linux-gnu", "$-16, %rsp"), ("i686-unknown-linux-gnu", "$-16, %esp")];

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-force-align-{}-{what}", std::process::id()));
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

/// The lines of one function in a listing, from its label to the next `.size`.
fn function<'a>(text: &'a str, name: &str) -> &'a str {
    let label = format!("\n{name}:\n");
    let start = text.find(&label).unwrap_or_else(|| panic!("{name} is in the listing:\n{text}"));
    let rest = &text[start + label.len()..];
    &rest[..rest.find("\t.size\t").unwrap_or(rest.len())]
}

/// A function that calls something, one that keeps a sixteen byte object, a leaf that keeps
/// nothing wide, the same attribute on a prototype above a definition that does not say it, and
/// the same three without it.
const FRAMES: &str = "extern int g(int);\n\
    __attribute__((force_align_arg_pointer)) int calls(int x) { return g(x) + 1; }\n\
    __attribute__((__force_align_arg_pointer__)) int wide(int x) {\n\
        _Alignas(16) volatile char buf[16];\n\
        buf[0] = (char)x;\n\
        return buf[0];\n\
    }\n\
    __attribute__((force_align_arg_pointer)) int leaf(int x, int y) { return x * y + 3; }\n\
    __attribute__((force_align_arg_pointer)) int declared(int);\n\
    int declared(int x) { return g(x) * 2; }\n\
    [[gnu::force_align_arg_pointer]] int standard(int x) { return g(x) - 1; }\n\
    int plain_calls(int x) { return g(x) + 1; }\n\
    int plain_leaf(int x, int y) { return x * y + 3; }\n";

#[test]
fn a_forced_frame_is_realigned_when_it_calls_or_keeps_something_wide() {
    for (target, realign) in TARGETS {
        let (ok, text, err) = assembly("frames", target, &[], FRAMES);
        assert!(ok, "{target}: {err}");
        for name in ["calls", "wide", "declared", "standard"] {
            let body = function(&text, name);
            assert!(body.contains(realign), "{target}: {name} is realigned:\n{body}");
        }
        for name in ["leaf", "plain_calls", "plain_leaf"] {
            let body = function(&text, name);
            assert!(!body.contains(realign), "{target}: {name} is left alone:\n{body}");
        }
    }
}

/// The attribute is x86's, so another target takes it as gcc does on one: as an attribute it
/// has never heard of.
#[test]
fn another_target_does_not_realign() {
    let (ok, text, err) = assembly("elsewhere", "aarch64-unknown-linux-gnu", &[], FRAMES);
    assert!(ok, "{err}");
    assert!(!text.contains("and\tsp"), "{text}");
}

/// What gcc 13 says about the attribute, in its words, and whether it stops there.
#[test]
fn force_align_arg_pointer_is_checked_in_gcc_s_words() {
    let types = "'force_align_arg_pointer' attribute only applies to function types";
    for (source, said, error) in [
        ("__attribute__((force_align_arg_pointer)) int v;\n", types, false),
        ("struct s { int a __attribute__((force_align_arg_pointer)); };\n", types, false),
        ("typedef int t __attribute__((force_align_arg_pointer));\n", types, false),
        (
            "__attribute__((force_align_arg_pointer(1))) int f(void) { return 1; }\n",
            "error: wrong number of arguments specified for 'force_align_arg_pointer' attribute",
            true,
        ),
        (
            "__attribute__((force_align_arg_pointer(1))) int f(void) { return 1; }\n",
            "expected 0, found 1",
            true,
        ),
    ] {
        for (target, _) in TARGETS {
            let (ok, _, err) = assembly("words", target, &[], source);
            assert_eq!(ok, !error, "{target}: {source}\n{err}");
            assert!(err.contains(said), "{target}: {source}\nwanted {said:?}, got:\n{err}");
        }
    }
    // Nothing to say about a typedef of a function type, a pointer to one, a naked function or a
    // function that is never defined, and the attribute is there to ask about.
    let quiet = "typedef void handler(void) __attribute__((force_align_arg_pointer));\n\
        void (*hook)(void) __attribute__((force_align_arg_pointer));\n\
        __attribute__((force_align_arg_pointer, naked)) void bare(void) { __asm__(\"ret\"); }\n\
        __attribute__((force_align_arg_pointer)) void elsewhere(void);\n\
        _Static_assert(__has_attribute(force_align_arg_pointer), \"x86\");\n";
    for (target, realign) in TARGETS {
        let (ok, text, err) = assembly("quiet", target, &["-Wall", "-Wextra"], quiet);
        assert!(ok, "{target}: {err}");
        assert_eq!(err, "", "{target}");
        assert!(!text.contains(realign), "{target}:\n{text}");
    }
}
