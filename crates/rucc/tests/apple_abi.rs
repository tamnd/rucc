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

/// Calls that go past the eight argument registers, and a variadic one.
const STACKED: &str = "\
int pk(int a, int b, int c, int d, int e, int f, int g, int h, char i, short j, int k, char l);
int callpk(void) { return pk(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12); }
int va(int n, ...);
int callva(void) { return va(1, 42, 3.0); }
";

/// Whether one of those instructions is a store to this place in the outgoing arguments.
fn stores_at(body: &[&str], place: &str) -> bool {
    body.iter().any(|line| line.starts_with("str") && line.ends_with(&format!(", {place}")))
}

/// Apple packs arguments on the stack to their own size and alignment where AAPCS64 gives each
/// one an eight byte slot. clang's `pk` reads `j` from `[sp, #2]`, so a caller that wrote it at
/// `[sp, #8]` hands it `l` instead.
#[test]
fn a_stack_argument_is_packed_to_its_own_alignment_on_darwin() {
    for level in ["-O0", "-O2"] {
        let asm = listing(&format!("packed{level}"), STACKED, DARWIN, level);
        let call = body(&asm, "_callpk");
        for place in ["[sp]", "[sp, #2]", "[sp, #4]", "[sp, #8]"] {
            assert!(stores_at(&call, place), "{level} {place}\n{asm}");
        }
    }
    let asm = listing("packed-linux", STACKED, LINUX, "-O2");
    let call = body(&asm, "callpk");
    for place in ["[sp]", "[sp, #8]", "[sp, #16]", "[sp, #24]"] {
        assert!(stores_at(&call, place), "{place}\n{asm}");
    }
    assert!(!stores_at(&call, "[sp, #2]"), "{asm}");
}

/// An `__int128` in the argument area after a packed `char` and a `long`, as the function and as
/// a call. The `char` leaves the area a byte long and the `long` aligns itself to the next word,
/// and laying the area out again must not put a word of filler in between, or the function is
/// refused for a width no register holds (#1992).
const WIDE: &str = "\
__attribute__((noinline)) long wq(long a1, long a2, long a3, long a4, long a5, long a6, long a7, long a8, char c, long l, __int128 q) {
    return (long)(q >> 64) + l + c;
}
long callwq(__int128 q) { return wq(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, q); }
";

#[test]
fn a_wide_argument_after_a_packed_one_is_where_clang_looks_on_darwin() {
    for level in ["-O0", "-O2"] {
        let asm = listing(&format!("wide{level}"), WIDE, DARWIN, level);
        let call = body(&asm, "_callwq");
        for place in ["[sp, #8]", "[sp, #16]"] {
            let stored = call.iter().any(|line| line.starts_with("st") && line.contains(place));
            assert!(stored, "{level} {place}\n{asm}");
        }
    }
}

/// Apple passes every argument a `...` stands for on the stack, eight bytes each, and its
/// `va_list` is a plain pointer that walks them. clang's `va` never looks in `w1` or `d0`, so the
/// `42` and the `3.0` have to be in memory before the call.
#[test]
fn a_variadic_argument_goes_on_the_stack_on_darwin() {
    for level in ["-O0", "-O2"] {
        let asm = listing(&format!("variadic{level}"), STACKED, DARWIN, level);
        let call = body(&asm, "_callva");
        assert!(stores_at(&call, "[sp]") && stores_at(&call, "[sp, #8]"), "{level}\n{asm}");
    }
    let asm = listing("variadic-linux", STACKED, LINUX, "-O2");
    let call = body(&asm, "callva");
    assert!(!call.iter().any(|line| line.starts_with("str") && line.contains("[sp")), "{asm}");
}

/// The word a data label is given, as written in the listing.
fn word<'a>(listing: &'a str, label: &str) -> Option<&'a str> {
    let mut lines = listing.lines().skip_while(|line| *line != format!("{label}:"));
    lines.nth(1).map(str::trim)
}

/// The types whose size or sign Apple chose differently: a plain `char` is signed, `wchar_t` is a
/// signed `int`, and `long double` is the same eight bytes as `double`. A structure holding any
/// of them is laid out differently by clang if rucc got one wrong.
const TYPES: &str = "\
#include <stddef.h>
int chars = (char)-1 < 0;
int wides = sizeof(wchar_t) * 10 + ((wchar_t)-1 < 0);
int longs = sizeof(long double);
long double sum(long double a, long double b) { return a + b; }
";

#[test]
fn a_plain_char_is_signed_and_a_long_double_is_a_double_on_darwin() {
    let asm = listing("types", TYPES, DARWIN, "-O2");
    assert_eq!(word(&asm, "_chars"), Some(".long\t1"), "{asm}");
    assert_eq!(word(&asm, "_wides"), Some(".long\t41"), "{asm}");
    assert_eq!(word(&asm, "_longs"), Some(".long\t8"), "{asm}");
    assert!(has(&body(&asm, "_sum"), "fadd", "d0"), "{asm}");
    let asm = listing("types-linux", TYPES, LINUX, "-O2");
    assert_ne!(word(&asm, "chars"), Some(".long\t1"), "{asm}");
    assert_eq!(word(&asm, "wides"), Some(".long\t40"), "{asm}");
    assert_eq!(word(&asm, "longs"), Some(".long\t16"), "{asm}");
    assert!(body(&asm, "sum").iter().any(|line| line.contains("__addtf3")), "{asm}");
}

/// Apple keeps `x18` for the system, which may change it at any moment, so a value left there is
/// lost. Twenty live arguments are more than the registers a call destroys, which is when an
/// allocator reaches for it.
#[test]
fn the_platform_register_is_never_used_on_darwin() {
    let many = "\
long many(long a, long b, long c, long d, long e, long f, long g, long h, long i, long j,
          long k, long l, long m, long n, long o, long p, long q, long r, long s, long t) {
    return (a * b + c * d + e * f + g * h + i * j + k * l + m * n + o * p + q * r + s * t)
        * (a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p + q + r + s + t);
}
";
    for level in ["-O0", "-O2"] {
        let asm = listing(&format!("x18{level}"), many, DARWIN, level);
        assert!(body(&asm, "_many").len() > 20, "{asm}");
        assert!(!asm.contains("x18") && !asm.contains("w18"), "{level}\n{asm}");
    }
}

/// A tentative definition is a common symbol on Darwin, which is what Apple's clang does without
/// being asked, so two files that each write `int g;` link there the way they do with clang.
#[test]
fn a_tentative_definition_is_common_on_darwin() {
    let source = "int tentative;\nint zero = 0;\nstatic int hidden;\n";
    let apple = listing("common", source, DARWIN, "-O0");
    assert!(
        apple.lines().any(|line| line.trim().starts_with(".comm") && line.contains("_tentative")),
        "{apple}"
    );
    assert!(
        !apple.lines().any(|line| line.trim().starts_with(".comm") && line.contains("_zero")),
        "{apple}"
    );
    assert!(
        !apple.lines().any(|line| line.trim().starts_with(".comm") && line.contains("_hidden")),
        "{apple}"
    );
    let linux = listing("common-linux", source, LINUX, "-O0");
    assert!(!linux.contains(".comm"), "{linux}");
}

/// A name given with `__asm__` is the name the linker sees, so on Darwin it is written as it was
/// spelled and not given a second underscore, which is how the system headers name the variants
/// of `fopen` and the rest.
#[test]
fn an_asm_label_is_not_decorated_again_on_darwin() {
    let source = "int renamed(int) __asm(\"_real\");\nint call(int x) { return renamed(x); }\n";
    let apple = listing("label", source, DARWIN, "-O0");
    assert!(apple.contains("_real"), "{apple}");
    assert!(!apple.contains("__real"), "{apple}");
}
