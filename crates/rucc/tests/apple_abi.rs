//! The places Apple's arm64 ABI parts from AAPCS64, each checked in the assembly rucc writes.
//!
//! Every one of these is a disagreement a program only notices when half of it is built by
//! another compiler, which is the usual way on a Mac: the system libraries are clang's. So the
//! assertions are about the instructions clang's half relies on, and each names the function
//! clang would get wrong if the instruction were missing.

use std::path::PathBuf;
use std::process::Command;

const DARWIN: &str = "aarch64-apple-darwin";

const LINUX: &str = "aarch64-unknown-linux-gnu";

/// Functions that hand a `char` or a `short` to another one or back to their caller.
const NARROW: &str = "\
unsigned char ru(unsigned char x) { return x + 1; }
signed char rs(signed char x) { return x - 1; }
int wu(unsigned char x);
int ws(signed char x, short y);
int pu(int x) { return wu(x); }
int ps(int x, int y) { return ws(x, y); }
";

/// The fixture, under a directory of its own so that two of these running at once do not write
/// the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-apple-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly for one fixture on one target, which needs no assembler and no linker.
fn listing(what: &str, source: &str, target: &str, level: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args([level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// The instructions of one function, from its label to its first `ret`.
fn body<'a>(listing: &'a str, label: &str) -> Vec<&'a str> {
    let mut lines = listing.lines().skip_while(|line| *line != format!("{label}:"));
    let mut found = Vec::new();
    for line in lines.by_ref().skip(1) {
        let line = line.trim();
        if line.starts_with('.') || line.ends_with(':') {
            continue;
        }
        found.push(line);
        if line == "ret" {
            break;
        }
    }
    found
}

/// Whether one of those instructions starts with this mnemonic and writes this register.
fn has(body: &[&str], mnemonic: &str, register: &str) -> bool {
    body.iter().any(|line| line.starts_with(&format!("{mnemonic} {register},")))
}

/// Apple extends a `char` or a `short` to 32 bits by its own sign, the caller for an argument
/// and the callee for a return value. clang's callee of `wu(unsigned char x)` returns `x` with a
/// bare `ret`, and its caller of `ru` compares the whole of `w0`, so a missing extension is a
/// wrong answer and not a slow one.
#[test]
fn a_narrow_integer_crosses_a_call_extended_to_32_bits_on_darwin() {
    for level in ["-O0", "-O2"] {
        let asm = listing(&format!("narrow{level}"), NARROW, DARWIN, level);
        assert!(has(&body(&asm, "_ru"), "uxtb", "w0"), "{level}\n{asm}");
        assert!(has(&body(&asm, "_rs"), "sxtb", "w0"), "{level}\n{asm}");
        assert!(has(&body(&asm, "_pu"), "uxtb", "w0"), "{level}\n{asm}");
        let ps = body(&asm, "_ps");
        assert!(has(&ps, "sxtb", "w0") && has(&ps, "sxth", "w1"), "{level}\n{asm}");
    }
}

/// AAPCS64 says nothing about those bits, so extending them there is work with no reader.
#[test]
fn a_narrow_integer_crosses_a_call_as_itself_on_linux() {
    let asm = listing("narrow-linux", NARROW, LINUX, "-O2");
    for label in ["ru", "rs", "pu", "ps"] {
        let found = body(&asm, label);
        assert!(!found.iter().any(|line| line.starts_with("uxt") || line.starts_with("sxt")));
    }
}

/// A function says how to unwind through it in the same directives as on Linux, which Apple's
/// assembler and the one here both turn into the DWARF table ld64 reads. Without one, a C++
/// exception thrown by clang's code stops the program at the first function of ours it meets.
#[test]
fn a_darwin_function_says_how_to_unwind_through_it() {
    let asm = listing("unwind", NARROW, DARWIN, "-O2");
    assert_eq!(asm.matches(".cfi_startproc").count(), 4, "{asm}");
    assert_eq!(asm.matches(".cfi_endproc").count(), 4, "{asm}");
}

/// A build that asks for debug information gets it, in the `__DWARF` segment where `dsymutil` and
/// a debugger look for it, rather than an object with none or no object at all.
#[test]
fn a_darwin_object_carries_its_debug_information() {
    let path = fixture("debug", NARROW);
    let object = path.with_extension("o");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={DARWIN}"))
        .args(["-g", "-O1", "-c", "-o"])
        .arg(&object)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let bytes = std::fs::read(&object).unwrap_or_default();
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let has = |name: &[u8]| bytes.windows(name.len()).any(|at| at == name);
    for section in ["__debug_info", "__debug_line", "__debug_abbrev"] {
        assert!(has(section.as_bytes()), "no {section}");
    }
    assert!(has(b"__DWARF"));
}
