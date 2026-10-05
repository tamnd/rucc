//! `__attribute__((indirect_return))`, end to end: a landing pad after each call that can come
//! back by a jump under `-fcf-protection=branch`, as gcc writes it, the tail calls `full` keeps
//! as calls, and what gcc says about the attribute on the way.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, since the listing is read.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-indirect-{}-{what}", std::process::id()));
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
fn listing(what: &str, flags: &[&str], source: &str) -> String {
    let (ok, out, err) = assembly(what, TARGET, flags, source);
    assert!(ok, "{flags:?}: {err}");
    out
}

/// The instructions of one function, trimmed, from its label to its `.size`.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("{name}:");
    let close = format!(".size\t{name},");
    text.lines()
        .map(str::trim)
        .skip_while(|line| *line != open)
        .skip(1)
        .take_while(|line| !line.starts_with(&close))
        .filter(|line| !line.starts_with('.') && !line.ends_with(':'))
        .collect()
}

/// What follows each call in that function: the next instruction, or nothing at the end.
fn after_calls<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let lines = body(text, name);
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("call"))
        .map(|(at, _)| lines.get(at + 1).copied().unwrap_or(""))
        .collect()
}

/// Every way gcc reads the attribute onto a call: the function's own declaration, a typedef, a
/// pointer with the attribute beside its star or after its declarator, and a member. Each call
/// is followed by an addition so that it is not a tail call.
const CALLS: &str = "int j(void) __attribute__((indirect_return));\n\
    typedef int ir(void) __attribute__((indirect_return));\n\
    extern ir *p;\n\
    int (*__attribute__((indirect_return)) q)(void);\n\
    int (*r)(void) __attribute__((indirect_return));\n\
    struct s { int (*m)(void) __attribute__((indirect_return)); } st;\n\
    int setjmp(void *);\n\
    void *buf[8];\n\
    int plain(void);\n\
    int direct(void) { return j() + 1; }\n\
    int typed(void) { return p() + 1; }\n\
    int starred(void) { return q() + 1; }\n\
    int after(void) { return r() + 1; }\n\
    int member(void) { return st.m() + 1; }\n\
    int saved(void) { return setjmp(buf) + 1; }\n\
    int ordinary(void) { return plain() + 1; }\n";

const NAMES: [&str; 6] = ["direct", "typed", "starred", "after", "member", "saved"];

#[test]
fn a_call_that_can_come_back_by_a_jump_is_followed_by_a_landing_pad() {
    for flags in [&["-fcf-protection=branch"][..], &["-fcf-protection=full"], &["-mmanual-endbr", "-fcf-protection=branch"]] {
        let text = listing("pads", flags, CALLS);
        for name in NAMES {
            assert_eq!(after_calls(&text, name), ["endbr64"], "{flags:?} {name}:\n{text}");
        }
        assert_ne!(after_calls(&text, "ordinary"), ["endbr64"], "{flags:?}:\n{text}");
    }
    for flags in [&[][..], &["-fcf-protection=none"], &["-fcf-protection=return"]] {
        let text = listing("bare", flags, CALLS);
        assert!(!text.contains("endbr64"), "{flags:?}:\n{text}");
    }
    // i386 has the same pad, under its own name.
    let (ok, text, err) = assembly("i686", "i686-unknown-linux-gnu", &["-fcf-protection=branch"], CALLS);
    assert!(ok, "{err}");
    for name in NAMES {
        assert_eq!(after_calls(&text, name), ["endbr32"], "{name}:\n{text}");
    }
}

/// gcc reads the attribute off the callee as well as off the type the call was made through,
/// so a call through a plain pointer the optimizer saw the target of has its pad, and a
/// declaration without a prototype has nothing to read it off.
#[test]
fn the_callee_s_own_type_counts_and_an_old_style_declaration_does_not() {
    let source = "int j(void) __attribute__((indirect_return));\n\
        int k() __attribute__((indirect_return));\n\
        int seen(void) { int (*q)(void) = j; return q() + 1; }\n\
        int old(void) { return k() + 1; }\n";
    let text = listing("callee", &["-fcf-protection=branch"], source);
    assert_eq!(after_calls(&text, "seen"), ["endbr64"], "{text}");
    assert_ne!(after_calls(&text, "old"), ["endbr64"], "{text}");
}

/// A call in tail position is still a jump under `branch`, where the callee comes back to this
/// function's caller and that caller's pad. Under `full` it stays a call with its pad, as in gcc,
/// unless the function making it can come back by a jump as well.
#[test]
fn full_keeps_a_tail_call_to_one_as_a_call() {
    let source = "int j(void) __attribute__((indirect_return));\n\
        int f(void) { return j(); }\n\
        __attribute__((indirect_return)) int g(void) { return j(); }\n";
    let text = listing("tail-branch", &["-fcf-protection=branch"], source);
    assert!(after_calls(&text, "f").is_empty(), "{text}");
    assert!(body(&text, "f").iter().any(|line| line.starts_with("jmp")), "{text}");
    let text = listing("tail-full", &["-fcf-protection=full"], source);
    assert_eq!(after_calls(&text, "f"), ["endbr64"], "{text}");
    assert!(after_calls(&text, "g").is_empty(), "{text}");
    assert!(body(&text, "g").iter().any(|line| line.starts_with("jmp")), "{text}");
}

/// What gcc says about it, in its words: on something that is not a function it is ignored with
/// a warning, an argument is an error, and a pointer of one type taken for the other is not a
/// difference worth a word.
#[test]
fn the_attribute_is_checked_in_gcc_s_words() {
    for (source, said, error) in [
        (
            "int v __attribute__((indirect_return));\n",
            "warning: 'indirect_return' attribute only applies to function types",
            false,
        ),
        (
            "struct t { int a __attribute__((indirect_return)); };\n",
            "warning: 'indirect_return' attribute only applies to function types",
            false,
        ),
        (
            "int f(void) __attribute__((indirect_return(1)));\n",
            "error: wrong number of arguments specified for 'indirect_return' attribute",
            true,
        ),
        ("int f(void) __attribute__((indirect_return(1)));\n", "expected 0, found 1", true),
    ] {
        let (ok, _, err) = assembly("words", TARGET, &[], source);
        assert_eq!(ok, !error, "{source}\n{err}");
        assert!(err.contains(said), "{source}\nwanted {said:?}, got:\n{err}");
    }
    let quiet = "int plain(void);\n\
        int j(void) __attribute__((indirect_return));\n\
        int j(void);\n\
        int j(void) { return 1; }\n\
        typedef int ir(void) __attribute__((indirect_return));\n\
        ir *p1 = plain;\n\
        int (*p2)(void) = j;\n\
        _Static_assert(__has_attribute(indirect_return), \"indirect_return\");\n";
    let (ok, _, err) = assembly("quiet", TARGET, &["-Wall", "-Wextra"], quiet);
    assert!(ok, "{err}");
    assert_eq!(err, "");
}

/// The object the compiler writes itself has the pad right after the call, which is `e8` and a
/// displacement, then `f3 0f 1e fa`.
#[test]
fn the_object_has_the_pad_after_the_call() {
    let dir = dir("object");
    std::fs::write(dir.join("a.c"), CALLS).expect("the fixture can be written");
    let target = format!("--target={TARGET}");
    let (ok, _, err) =
        run(&dir, &[&target, "-O2", "-fcf-protection=branch", "-c", "a.c", "-o", "a.o"]);
    assert!(ok, "{err}");
    let bytes = std::fs::read(dir.join("a.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    let endbr = [0xf3, 0x0f, 0x1e, 0xfa];
    let after_direct = bytes
        .windows(9)
        .filter(|window| window[0] == 0xe8 && window[5..] == endbr)
        .count();
    assert!(after_direct >= 2, "j and setjmp are called directly, found {after_direct}");
}
