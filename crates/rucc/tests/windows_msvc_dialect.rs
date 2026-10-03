//! The C that the MSVC rows take: the macros they predefine, the keywords the SDK headers are
//! written in, and the intrinsics that need no declaration.
//!
//! Design: `platform/windows/11-headers-and-dialect.md` sections 11.2, 11.3 and 11.5.
//!
//! A test of the whole compiler because the pieces are in four crates. The lexer decides that
//! `__declspec` is a keyword, the parser turns it into the attribute it means, the checker reads
//! the attribute, and the preprocessor decides which of `_MSC_VER` and `__GNUC__` a header sees,
//! and a header only works when all four agree about which row they are on.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Microsoft's runtime, which is the row all of this is for.
const MSVC: &str = "--target=x86_64-windows-msvc";

/// The same on arm64.
const MSVC_ARM64: &str = "--target=aarch64-windows-msvc";

/// The GNU one, which has to be left exactly as it was.
const MINGW: &str = "--target=x86_64-windows-gnu";

/// The fixture, under a directory of its own so that two of these running at once do not write
/// the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-msvc-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler agreed, what it wrote and what it said.
fn run(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The macros a row predefines, as `-dM -E` prints them.
fn macros(target: &str, flags: &[&str]) -> String {
    let mut all = vec![target, "-dM", "-E"];
    all.extend_from_slice(flags);
    // Tests run on threads of one process, so each call takes a number of its own for its file.
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, Ordering::Relaxed);
    let (ok, out, said) = run(&format!("macros{call}"), &all, "");
    assert!(ok, "{target}: {said}");
    out
}

fn has(text: &str, line: &str) -> bool {
    text.lines().any(|l| l == line)
}

#[test]
fn the_msvc_row_predefines_what_clang_does_for_it() {
    let msvc = macros(MSVC, &[]);
    for line in [
        "#define _MSC_VER 1940",
        "#define _MSC_FULL_VER 194000000",
        "#define _MSC_EXTENSIONS 1",
        "#define _M_X64 100",
        "#define _M_AMD64 100",
        "#define _WIN32 1",
        "#define _WIN64 1",
    ] {
        assert!(has(&msvc, line), "no {line} in\n{msvc}");
    }
    for name in ["__GNUC__", "__clang__", "__MINGW32__", "__MINGW64__", "__MSVCRT__", "WIN32"] {
        assert!(!msvc.contains(&format!("#define {name} ")), "{name} on msvc");
    }
    let arm64 = macros(MSVC_ARM64, &[]);
    assert!(has(&arm64, "#define _M_ARM64 1"));
    assert!(!arm64.contains("_M_X64"));
}

#[test]
fn the_flags_move_the_msvc_macros_the_way_they_move_clangs() {
    let older = macros(MSVC, &["-fms-compatibility-version=19.29", "-fno-ms-extensions"]);
    assert!(has(&older, "#define _MSC_VER 1929"));
    assert!(has(&older, "#define _MSC_FULL_VER 192900000"));
    assert!(!older.contains("_MSC_EXTENSIONS"));
    let gcc = macros(MSVC, &["-fgnuc-version=13.2"]);
    assert!(has(&gcc, "#define __GNUC__ 13"));
}

#[test]
fn the_mingw_row_still_predefines_the_gcc_set() {
    let mingw = macros(MINGW, &[]);
    assert!(has(&mingw, "#define __GNUC__ 16"));
    assert!(has(&mingw, "#define __MINGW64__ 1"));
    assert!(has(&mingw, "#define __declspec(x) __attribute__((x))"));
    assert!(!mingw.contains("_MSC_VER"));
}

/// What the SDK headers write, one construct at a time.
const SDK: &str = "\
#pragma warning(push)
#pragma warning(disable: 4996 4820)
#pragma region Declarations
#pragma intrinsic(memcpy)
#pragma function(memset)
#pragma optimize(\"\", off)
typedef unsigned __int64 size_t;
__declspec(dllimport) void * __cdecl malloc(size_t);
__declspec(dllimport) __declspec(noreturn) void __cdecl exit(int);
__declspec(noinline) __declspec(restrict) void *__stdcall make(size_t);
__declspec(deprecated(\"use the other one\")) int _old(void);
__declspec(selectany) int one_copy = 1;
__declspec(thread) int per_thread;
struct __declspec(align(16)) aligned { char c; } a16;
struct __declspec(novtable) __declspec(empty_bases) plain { int x; };
_Static_assert(_Alignof(struct aligned) == 16, \"__declspec(align)\");
typedef int (__cdecl *compare)(const void *, const void *);
void __fastcall fast(int);
void __vectorcall vec(int);
int * __ptr64 __unaligned wide;
void * __ptr32 narrow;
__declspec(allocator) __declspec(safebuffers) void *pool(size_t);
static __forceinline int twice(int x) { return 2 * x; }
static __inline int thrice(int x) { return 3 * x; }
int __declspec(noalias) use(int * __restrict p) { return twice(*p) + thrice(*p); }
__declspec(dllexport) int exported(void) { return one_copy + per_thread; }
#pragma endregion
#pragma optimize(\"\", on)
#pragma warning(pop)
";

#[test]
fn the_msvc_keywords_are_read_without_a_word() {
    for target in [MSVC, MSVC_ARM64] {
        let (ok, _, said) = run("sdk", &[target, "-std=c11", "-fsyntax-only"], SDK);
        assert!(ok, "{target}: {said}");
        assert!(said.is_empty(), "{target} said something:\n{said}");
    }
}

#[test]
fn an_unknown_declspec_is_a_warning_and_nothing_else() {
    let source = "__declspec(frobnicate) int x;\n";
    let (ok, _, said) = run("unknown", &[MSVC, "-fsyntax-only"], source);
    assert!(ok, "{said}");
    assert!(said.contains("`__declspec` attribute `frobnicate` is not supported"), "{said}");
}

#[test]
fn structured_exception_handling_is_refused_with_a_pointer_to_why() {
    let source = "int f(void) { __try { return 1; } __except (1) { return 0; } }\n";
    let (ok, _, said) = run("seh", &[MSVC, "-fsyntax-only"], source);
    assert!(!ok);
    let message =
        "structured exception handling (`__try`) is not supported; see docs/DIVERGENCE.md";
    assert!(said.contains(message), "{said}");
}

/// The three intrinsics, each next to a call that would be there if they evaluated anything.
const INTRINSICS: &str = "\
int g(void);
int noop(void) { return __noop(g(), 1); }
int assume(int x) { __assume(g()); __assume(x > 0); return x; }
int never(int x) { switch (x) { case 1: return 5; default: __assume(0); } }
void brk(void) { __debugbreak(); }
";

#[test]
fn the_intrinsics_do_what_msvc_does_with_them() {
    let (ok, asm, said) = run("intrinsics", &[MSVC, "-O1", "-S", "-o", "-"], INTRINSICS);
    assert!(ok, "{said}");
    assert!(!asm.contains("call"), "an argument was evaluated:\n{asm}");
    assert!(asm.contains("0xcc") || asm.contains("int3"), "no breakpoint:\n{asm}");
    // The instructions have to assemble as well as print.
    let out = std::env::temp_dir().join(format!("rucc-msvc-{}-obj.o", std::process::id()));
    let out = out.to_str().expect("a temporary path is text").to_owned();
    let (ok, _, said) = run("object", &[MSVC, "-c", "-o", &out], INTRINSICS);
    let _ = std::fs::remove_file(&out);
    assert!(ok, "{said}");
    // The arm64 row cannot generate code for a function yet, so there the names are only checked.
    let (ok, _, said) = run("intrinsics-arm", &[MSVC_ARM64, "-fsyntax-only"], INTRINSICS);
    assert!(ok && said.is_empty(), "{said}");
}

#[test]
fn on_the_mingw_row_the_msvc_words_are_what_the_headers_make_them() {
    // No keywords and no intrinsics: `__declspec` and `__cdecl` come from the predefined macros,
    // and `__try` and `__noop` are names a program may use.
    let source = "\
__declspec(dllexport) int __cdecl f(void) { return 0; }
int __try = 1, __noop = 2, __forceinline = 3;
";
    let (ok, _, said) = run("mingw", &[MINGW, "-fsyntax-only"], source);
    assert!(ok, "{said}");
}

#[test]
fn a_selectany_definition_is_one_the_link_may_find_elsewhere_too() {
    // The universal CRT's <wchar.h> defines a variable this way that libucrt.lib defines as well,
    // so a program that includes it has to give way to the library's copy rather than clash.
    let source = "__declspec(selectany) int one_copy = 1;\nint ordinary = 2;\n";
    let (ok, asm, said) = run("selectany", &[MSVC, "-S", "-o", "-"], source);
    assert!(ok, "{said}");
    assert!(asm.contains("\t.weak\tone_copy\n"), "{asm}");
    assert!(!asm.contains("\t.globl\tone_copy\n"), "{asm}");
    assert!(asm.contains("\t.globl\tordinary\n"), "{asm}");
}

/// Enough of Microsoft's `<setjmp.h>` for i386 to see what `setjmp` turns into: the header names it
/// `_setjmp` and declares it with one parameter and no attribute, since cl.exe knows it by name.
const MSVC_SETJMP_H: &str = "\
#ifndef _INC_SETJMP
#define _INC_SETJMP
typedef int jmp_buf[16];
#define setjmp _setjmp
int __cdecl setjmp(jmp_buf _Buf);
__declspec(noreturn) void __cdecl longjmp(jmp_buf _Buf, int _Value);
#endif
";

/// On i386 `setjmp` is a call to `_setjmp3` with a count of nothing after the buffer, the call cl.exe
/// and clang make.
#[test]
fn on_i386_setjmp_is_setjmp3_with_nothing_after_the_buffer() {
    let headers = std::env::temp_dir().join(format!("rucc-msvc-{}-setjmp-h", std::process::id()));
    std::fs::create_dir_all(&headers).expect("a temporary directory can be created");
    std::fs::write(headers.join("setjmp.h"), MSVC_SETJMP_H).expect("the header can be written");
    let source = "\
#include <setjmp.h>
jmp_buf env;
int g(void);
int f(void) {
    int seen = g();
    if (setjmp(env))
        return seen;
    return 0;
}
";
    let include = headers.to_str().expect("a temporary path is text").to_owned();
    let (ok, asm, said) = run(
        "setjmp3",
        // After the compiler's own headers, where the toolset's are, so that ours is the one
        // `#include <setjmp.h>` finds and it goes on to this one.
        &["--target=i686-windows-msvc", "-idirafter", &include, "-O1", "-S", "-o", "-"],
        source,
    );
    let _ = std::fs::remove_dir_all(&headers);
    assert!(ok, "{said}");
    assert!(asm.contains("\tcall\t__setjmp3\n"), "{asm}");
    assert!(!asm.contains("\tcall\t__setjmp\n"), "{asm}");
    // The count is the second argument, so something goes into the word after the buffer's.
    let call = asm.find("\tcall\t__setjmp3\n").expect("checked above");
    let before = &asm[..call];
    assert!(before.contains(", 4(%esp)\n") || before.contains("\tpushl\t"), "a count: {asm}");
}
