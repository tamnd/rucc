//! `__attribute__((ms_hook_prologue))`, end to end: a function a Windows hot patcher can redirect
//! opens with the bytes gcc writes and has `int3` in front of its label, on x86-64 and on i386, in
//! the listing and in the object the compiler writes itself, and what gcc says about the attribute
//! on the way, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The two targets gcc has the attribute on, written down rather than taken from the host, since
/// the listing is read, with the bytes the function opens with and how many lines of `int3` go in
/// front of its label.
const TARGETS: [(&str, &str, usize); 2] = [
    ("x86_64-unknown-linux-gnu", "\t.byte\t0x48, 0x8d, 0xa4, 0x24, 0x00, 0x00, 0x00, 0x00\n", 8),
    ("i686-unknown-linux-gnu", "\t.byte\t0x8b, 0xff, 0x55, 0x8b, 0xec\n", 4),
];

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-ms-hook-{}-{what}", std::process::id()));
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
    let mut args = vec![target.as_str(), "-S", "-o", "-"];
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

/// How many lines of `int3` are straight in front of one function's label.
fn room(text: &str, name: &str) -> usize {
    let label = format!("\n{name}:\n");
    let start = text.find(&label).unwrap_or_else(|| panic!("{name} is in the listing:\n{text}"));
    text[..start].lines().rev().take_while(|line| *line == "\t.long\t 0xcccccccc").count()
}

/// A function that calls something, a leaf, an empty one, the attribute on a prototype above a
/// definition that does not say it, the other two spellings, and one without it.
const HOOKED: &str = "extern int g(int);\n\
    __attribute__((ms_hook_prologue)) int calls(int x) { return g(x) + 1; }\n\
    __attribute__((ms_hook_prologue)) int leaf(int x) { return x * 3; }\n\
    __attribute__((ms_hook_prologue)) void empty(void) { }\n\
    __attribute__((ms_hook_prologue)) int declared(int);\n\
    int declared(int x) { return g(x) * 2; }\n\
    __attribute__((__ms_hook_prologue__)) int armoured(int x) { return x + 2; }\n\
    [[gnu::ms_hook_prologue]] int standard(int x) { return x - 1; }\n\
    int plain(int x) { return g(x) + 1; }\n";

#[test]
fn a_hooked_function_opens_with_the_bytes_gcc_writes() {
    for (target, opening, lines) in TARGETS {
        for level in ["-O0", "-O2"] {
            let (ok, text, err) = assembly("listing", target, &[level], HOOKED);
            assert!(ok, "{target} {level}: {err}");
            for name in ["calls", "leaf", "empty", "declared", "armoured", "standard"] {
                let body = function(&text, name);
                assert!(body.starts_with(opening), "{target} {level}: {name}:\n{body}");
                assert_eq!(room(&text, name), lines, "{target} {level}: {name}:\n{text}");
                // The frame pointer the i386 bytes pushed is taken back off before anything else.
                if target.starts_with("i686") {
                    let next = body[opening.len()..].lines().find(|line| !line.starts_with("\t."));
                    assert_eq!(next, Some("\tpopl\t%ebp"), "{target} {level}: {name}:\n{body}");
                }
            }
            let body = function(&text, "plain");
            assert!(!body.contains("\t.byte\t"), "{target} {level}: plain:\n{body}");
            assert_eq!(room(&text, "plain"), 0, "{target} {level}:\n{text}");
        }
        // The landing pad is after the bytes, which is where gcc puts it.
        let (ok, text, err) = assembly("landing", target, &["-O2", "-fcf-protection=branch"], HOOKED);
        assert!(ok, "{target}: {err}");
        let body = function(&text, "leaf");
        assert!(body.starts_with(opening), "{target}:\n{body}");
        let next = body[opening.len()..].lines().find(|line| !line.starts_with("\t."));
        assert!(next.is_some_and(|line| line.starts_with("\tendbr")), "{target}:\n{body}");
    }
    // The attribute is x86's, so another target writes nothing for it.
    let (ok, text, err) = assembly("elsewhere", "aarch64-unknown-linux-gnu", &["-O2"], HOOKED);
    assert!(ok, "{err}");
    assert!(!text.contains("0xcccccccc") && !text.contains("\t.byte\t"), "{text}");
}

/// The object the compiler writes itself has the same bytes, with the symbol at the opening and
/// the room in front of it, in one text section and in a section per function.
#[test]
fn the_object_has_the_same_bytes() {
    let source = "__attribute__((ms_hook_prologue)) int leaf(int x) { return x * 3; }\n";
    let mut wanted = vec![0xcc; 32];
    wanted.extend_from_slice(&[0x48, 0x8d, 0xa4, 0x24, 0, 0, 0, 0]);
    for flags in [&[][..], &["-ffunction-sections"]] {
        let dir = dir("object");
        std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
        let mut args = vec!["--target=x86_64-unknown-linux-gnu", "-O2", "-c", "a.c", "-o", "a.o"];
        args.extend_from_slice(flags);
        let (ok, _, err) = run(&dir, &args);
        assert!(ok, "{flags:?}: {err}");
        let bytes = std::fs::read(dir.join("a.o")).expect("the object was written");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            bytes.windows(wanted.len()).any(|window| window == wanted.as_slice()),
            "{flags:?}"
        );
    }
}

/// What gcc 13 says about the attribute, in its words, and whether it stops there.
#[test]
fn ms_hook_prologue_is_checked_in_gcc_s_words() {
    let functions = "warning: 'ms_hook_prologue' attribute only applies to functions";
    for (source, said, error) in [
        ("__attribute__((ms_hook_prologue)) int v;\n", functions, false),
        ("struct s { int a __attribute__((ms_hook_prologue)); };\n", functions, false),
        ("typedef void t(void) __attribute__((ms_hook_prologue));\n", functions, false),
        ("void (*fp)(void) __attribute__((ms_hook_prologue));\n", functions, false),
        ("void g(int p __attribute__((ms_hook_prologue))) { (void)p; }\n", functions, false),
        ("void h(void) { __attribute__((ms_hook_prologue)) int x; (void)x; }\n", functions, false),
        (
            "__attribute__((ms_hook_prologue(1))) void f(void) {}\n",
            "error: wrong number of arguments specified for 'ms_hook_prologue' attribute",
            true,
        ),
        ("__attribute__((ms_hook_prologue(1))) void f(void) {}\n", "expected 0, found 1", true),
    ] {
        for (target, _, _) in TARGETS {
            let (ok, _, err) = assembly("words", target, &[], source);
            assert_eq!(ok, !error, "{target}: {source}\n{err}");
            assert!(err.contains(said), "{target}: {source}\nwanted {said:?}, got:\n{err}");
            assert_eq!(err.matches("warning:").count(), usize::from(!error), "{source}\n{err}");
        }
    }
    // Nothing to say about a function that is never defined, a naked one, which gets the bytes and
    // nothing after them, or a function declared in a block, and the attribute is there to ask
    // about.
    let quiet = "__attribute__((ms_hook_prologue)) void elsewhere(void);\n\
        __attribute__((ms_hook_prologue, naked)) void bare(void) { __asm__(\"ret\"); }\n\
        void outer(void) { __attribute__((ms_hook_prologue)) void inner(void); inner(); }\n\
        _Static_assert(__has_attribute(ms_hook_prologue), \"x86\");\n";
    for (target, opening, _) in TARGETS {
        let (ok, text, err) = assembly("quiet", target, &["-Wall", "-Wextra"], quiet);
        assert!(ok, "{target}: {err}");
        assert_eq!(err, "", "{target}");
        let body = function(&text, "bare");
        assert!(body.starts_with(opening), "{target}:\n{body}");
        assert!(!body.contains("popl"), "{target}:\n{body}");
    }
}
