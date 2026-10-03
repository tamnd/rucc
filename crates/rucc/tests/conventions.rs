//! What `__attribute__((ms_abi))` and `__attribute__((sysv_abi))` do to a listing and to a
//! diagnostic, read from the whole compiler.
//!
//! A test of the whole compiler rather than of one crate for the reason `chkstk.rs` beside this
//! gives: the convention is read in the front end, carried in the function type and the IR
//! signature, and turned into registers, shadow space and saved registers in the back end, and each
//! of those could be right on its own while the listing is wrong.
//!
//! Every expectation here was taken from gcc 13 for Linux and MinGW GCC for Windows, compiling the
//! same fixture at `-O2`. The listings are not compared line for line, since the two compilers
//! allocate registers differently, so each test names the few lines the convention decides.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Linux, where `ms_abi` is the other convention.
const LINUX: &str = "x86_64-unknown-linux-gnu";

/// Windows, where `sysv_abi` is the other convention.
const WINDOWS: &str = "x86_64-windows-gnu";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-conventions-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The compiler run on that source for that target, writing assembly to its standard output.
fn run(what: &str, target: &str, source: &str) -> Output {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    out
}

/// The listing, from a fixture the compiler has to accept.
fn asm(what: &str, target: &str, source: &str) -> String {
    let out = run(what, target, source);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What the compiler said about a fixture, and whether it went on to accept it.
fn diagnose(what: &str, target: &str, source: &str) -> (bool, String) {
    let out = run(what, target, source);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The instructions of one function, which are the lines from its label to the next function's
/// label or the end, less the ones said to the assembler.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let label = format!("{name}:");
    let mut lines = text.lines().skip_while(|line| line.trim() != label).skip(1);
    let mut out = Vec::new();
    for line in lines.by_ref() {
        let trimmed = line.trim();
        // The next function starts at a label in the first column that is not a local one, which
        // is `.L` on ELF and `L` on i386 COFF.
        if !line.starts_with(char::is_whitespace)
            && trimmed.ends_with(':')
            && !trimmed.starts_with('.')
            && !trimmed.starts_with('L')
        {
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('.') || trimmed.ends_with(':') {
            continue;
        }
        out.push(trimmed);
    }
    assert!(!out.is_empty(), "no function {name} in the listing:\n{text}");
    out
}

/// Whether an instruction in the body has all of those words in it.
fn has(body: &[&str], words: &[&str]) -> bool {
    body.iter().any(|line| words.iter().all(|word| line.contains(word)))
}

/// How many bytes the prologue takes off the stack pointer with a `subq`, or nought.
fn taken(body: &[&str]) -> u32 {
    body.iter()
        .find_map(|line| line.strip_prefix("subq\t$")?.strip_suffix(", %rsp")?.parse().ok())
        .unwrap_or(0)
}

/// A call from a plain Linux function into an `ms_abi` one. gcc puts the first four in `rcx`,
/// `rdx`, `r8` and `r9`, the fifth 32 bytes up the stack, and leaves the 32 below it for the
/// callee.
#[test]
fn a_linux_call_to_an_ms_abi_function_uses_the_windows_registers_and_shadow_space() {
    let text = asm(
        "linux-call",
        LINUX,
        "__attribute__((ms_abi)) long callee(long, long, long, long, long);\n\
         long caller(long x) { return callee(x, 2, 3, 4, 5) + x; }\n",
    );
    let caller = body(&text, "caller");
    assert!(has(&caller, &["%rdi, %rcx"]), "the first argument is not in rcx: {caller:#?}");
    assert!(has(&caller, &["$2, %edx"]), "the second is not in rdx: {caller:#?}");
    assert!(has(&caller, &["$3, %r8d"]), "the third is not in r8: {caller:#?}");
    assert!(has(&caller, &["$4, %r9d"]), "the fourth is not in r9: {caller:#?}");
    assert!(has(&caller, &["32(%rsp)"]), "the fifth is not above the shadow space: {caller:#?}");
    assert!(taken(&caller) >= 40, "no room for shadow space and the fifth: {caller:#?}");
}

/// The same call through a pointer type written the way UEFI writes one, which is a typedef with
/// the attribute beside the star.
#[test]
fn a_call_through_a_uefi_style_typedef_is_an_ms_abi_call() {
    let text = asm(
        "linux-typedef",
        LINUX,
        "#define EFIAPI __attribute__((ms_abi))\n\
         typedef long (EFIAPI *F)(long, long);\n\
         long caller(F f) { return f(1, 2); }\n",
    );
    let caller = body(&text, "caller");
    assert!(has(&caller, &["$1, %ecx"]), "the first argument is not in rcx: {caller:#?}");
    assert!(has(&caller, &["$2, %edx"]), "the second is not in rdx: {caller:#?}");
    assert!(taken(&caller) >= 32, "no shadow space under the call: {caller:#?}");
}

/// An `ms_abi` function on Linux owes its caller `rsi`, `rdi` and `xmm6` to `xmm15`, and the plain
/// function it calls owes it none of them, so all twelve are put away around the call, as gcc does.
#[test]
fn an_ms_abi_function_on_linux_saves_what_a_plain_callee_may_write() {
    let text = asm(
        "linux-saves",
        LINUX,
        "void plain(void);\n\
         __attribute__((ms_abi)) double keeps(double a) { plain(); return a; }\n",
    );
    let keeps = body(&text, "keeps");
    assert!(has(&keeps, &["pushq", "%rsi"]), "rsi is not saved: {keeps:#?}");
    assert!(has(&keeps, &["pushq", "%rdi"]), "rdi is not saved: {keeps:#?}");
    for n in 6..16 {
        let reg = format!("%xmm{n}, ");
        assert!(has(&keeps, &["movaps", &reg, "(%rsp)"]), "xmm{n} is not saved: {keeps:#?}");
    }
}

/// The same function under `-mno-sse` saves `rsi` and `rdi` and none of the vector registers, as
/// gcc does: it has nothing in one to lose, and the kernel's EFI stub, whose `efi_pe_entry` is one
/// of these, is checked for having no vector instruction in it.
#[test]
fn an_ms_abi_function_without_vector_registers_saves_none_of_them() {
    let path = fixture(
        "linux-no-sse",
        "void plain(void);\n\
         __attribute__((ms_abi)) long keeps(long a) { plain(); return a; }\n",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={LINUX}"))
        .args(["-O2", "-mno-sse", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let keeps = body(&text, "keeps");
    assert!(has(&keeps, &["pushq", "%rsi"]), "rsi is not saved: {keeps:#?}");
    assert!(!keeps.iter().any(|line| line.contains("xmm")), "{keeps:#?}");
}

/// A plain function on Linux with nothing live across the call has nothing to save, which is the
/// contrast that shows the saves above come from the convention.
#[test]
fn a_plain_function_on_linux_saves_none_of_them() {
    let text = asm(
        "linux-plain",
        LINUX,
        "void plain(void);\n\
         double keeps(double a) { plain(); return a; }\n",
    );
    let keeps = body(&text, "keeps");
    assert!(!has(&keeps, &["%rsi"]) && !has(&keeps, &["%rdi"]), "{keeps:#?}");
    assert!(!has(&keeps, &["%xmm6, "]), "{keeps:#?}");
}

/// A variadic function of the other convention is called the Windows way: no count in `al`, and a
/// `double` in both the vector register and the integer register of its position.
#[test]
fn a_variadic_ms_abi_call_puts_a_double_in_both_registers() {
    let text = asm(
        "linux-variadic",
        LINUX,
        "__attribute__((ms_abi)) int vcall(int, ...);\n\
         int callv(void) { return vcall(1, 2.5); }\n",
    );
    let callv = body(&text, "callv");
    assert!(has(&callv, &["$1, %ecx"]), "the count is not in rcx: {callv:#?}");
    assert!(has(&callv, &["%xmm1"]), "the double is not in xmm1: {callv:#?}");
    assert!(has(&callv, &["%rdx"]), "the double is not in rdx as well: {callv:#?}");
    assert!(!has(&callv, &["%al"]), "a Windows callee is not told a count: {callv:#?}");
}

/// A `sysv_abi` function on Windows owes its caller none of `rsi`, `rdi` and `xmm6` to `xmm15`, so
/// it saves none of them, and the Windows function it calls keeps them anyway.
#[test]
fn a_sysv_abi_function_on_windows_saves_none_of_them() {
    let text = asm(
        "windows-saves",
        WINDOWS,
        "void plain(void);\n\
         __attribute__((sysv_abi)) double keeps(double a) { plain(); return a; }\n\
         __attribute__((sysv_abi)) long spin(long a, long b) { plain(); return a * b; }\n",
    );
    for name in ["keeps", "spin"] {
        let got = body(&text, name);
        assert!(!has(&got, &["pushq", "%rsi"]), "{name} saves rsi: {got:#?}");
        assert!(!has(&got, &["pushq", "%rdi"]), "{name} saves rdi: {got:#?}");
        for n in 6..16 {
            let reg = format!("%xmm{n}, ");
            assert!(!has(&got, &[&reg, "(%rsp)"]), "{name} saves xmm{n}: {got:#?}");
        }
        // The Windows callee is still owed its 32 bytes.
        assert!(taken(&got) >= 32, "{name} leaves no shadow space: {got:#?}");
    }
}

/// A Windows function calling a `sysv_abi` one passes six in registers and the seventh at the
/// bottom of the stack with no shadow space under it, and has to keep `rsi`, `rdi` and the upper
/// ten vector registers itself, since the callee may write them.
#[test]
fn a_windows_call_to_a_sysv_abi_function_treats_the_windows_saved_registers_as_clobbered() {
    let text = asm(
        "windows-call",
        WINDOWS,
        "__attribute__((sysv_abi)) long long callee(long long, long long, long long, long long,\n\
                                                    long long, long long, long long);\n\
         long long caller(long long x) { return callee(x, 2, 3, 4, 5, 6, 7) + x; }\n",
    );
    let caller = body(&text, "caller");
    assert!(has(&caller, &[", %rdi"]), "the first argument is not in rdi: {caller:#?}");
    assert!(has(&caller, &["$2, %esi"]), "the second is not in rsi: {caller:#?}");
    assert!(has(&caller, &["$3, %edx"]), "the third is not in rdx: {caller:#?}");
    assert!(has(&caller, &["$4, %ecx"]), "the fourth is not in rcx: {caller:#?}");
    assert!(has(&caller, &["$5, %r8d"]), "the fifth is not in r8: {caller:#?}");
    assert!(has(&caller, &["$6, %r9d"]), "the sixth is not in r9: {caller:#?}");
    assert!(has(&caller, &[", (%rsp)"]), "the seventh is not at the bottom: {caller:#?}");
    assert!(has(&caller, &["pushq", "%rsi"]), "rsi is not kept: {caller:#?}");
    assert!(has(&caller, &["pushq", "%rdi"]), "rdi is not kept: {caller:#?}");
    for n in 6..16 {
        let reg = format!("%xmm{n}, ");
        assert!(has(&caller, &[&reg, "(%rsp)"]), "xmm{n} is not kept: {caller:#?}");
    }
}

/// The convention is part of the type, so two declarations that differ in it conflict, and a
/// pointer of one convention is not a pointer of the other. gcc's words, and gcc's spelling of the
/// type.
#[test]
fn two_conventions_are_two_types() {
    let (ok, said) = diagnose(
        "types",
        LINUX,
        "__attribute__((ms_abi)) int f(int);\n\
         int f(int);\n\
         typedef int (*plain)(int);\n\
         __attribute__((ms_abi)) int g(int x) { return x; }\n\
         plain p = g;\n",
    );
    assert!(!ok, "{said}");
    assert!(said.contains("conflicting types for 'f'"), "{said}");
    assert!(said.contains("'int (__attribute__((ms_abi)) *)(int)'"), "{said}");
}

/// The target's own convention written out changes nothing, on either platform.
#[test]
fn the_target_s_own_convention_is_the_same_type() {
    for (target, name) in [(LINUX, "sysv_abi"), (WINDOWS, "ms_abi")] {
        let (ok, said) = diagnose(
            &format!("own-{name}"),
            target,
            &format!(
                "__attribute__(({name})) int f(int);\n\
                 int f(int);\n\
                 int (*p)(int) = f;\n"
            ),
        );
        assert!(ok && said.is_empty(), "{target}: {said}");
    }
}

/// Both at once is refused in gcc's words, and a variadic definition in the other convention is
/// refused naming the attribute, since there is no `va_list` of that convention to read it with.
#[test]
fn what_is_refused_is_refused_by_name() {
    let (ok, said) = diagnose("both", LINUX, "__attribute__((ms_abi, sysv_abi)) int h(int);\n");
    assert!(!ok);
    assert!(said.contains("'ms_abi' and 'sysv_abi' attributes are not compatible"), "{said}");
    assert!(said.contains("E0740"), "{said}");

    let (ok, said) =
        diagnose("variadic", LINUX, "__attribute__((ms_abi)) int v(int n, ...) { return n; }\n");
    assert!(!ok);
    assert!(said.contains("'__attribute__((ms_abi))'") && said.contains("E0741"), "{said}");

    let (ok, said) = diagnose(
        "variadic-windows",
        WINDOWS,
        "__attribute__((sysv_abi)) int v(int n, ...) { return n; }\n",
    );
    assert!(!ok);
    assert!(said.contains("'__attribute__((sysv_abi))'") && said.contains("E0741"), "{said}");
}

/// The 32-bit conventions are ignored with a warning on x86-64 Linux, as gcc does, and accepted in
/// silence on Windows, as MinGW GCC does.
#[test]
fn the_32_bit_conventions_warn_on_linux_and_are_silent_on_windows() {
    for name in ["stdcall", "cdecl", "fastcall", "thiscall", "vectorcall"] {
        let source = format!("__attribute__(({name})) int f(int);\n");
        let (ok, said) = diagnose(&format!("linux-{name}"), LINUX, &source);
        assert!(ok, "{said}");
        assert!(said.contains(&format!("'{name}' attribute ignored")), "{name}: {said}");
        assert!(said.contains("E0703"), "{said}");
        let (ok, said) = diagnose(&format!("windows-{name}"), WINDOWS, &source);
        assert!(ok && said.is_empty(), "{name} on Windows: {said}");
    }
    let (ok, said) = diagnose("regparm", LINUX, "__attribute__((regparm(2))) int f(int);\n");
    assert!(ok && said.contains("'regparm' attribute ignored"), "{said}");
}

/// Off x86-64 there is no second convention, so both are ignored with a warning, and the
/// attribute on something that is not a function is gcc's other warning.
#[test]
fn off_x86_64_and_off_a_function_they_are_ignored_with_a_warning() {
    let (ok, said) = diagnose(
        "aarch64",
        "aarch64-unknown-linux-gnu",
        "__attribute__((ms_abi)) int f(int);\n__attribute__((sysv_abi)) int g(int);\n",
    );
    assert!(ok, "{said}");
    assert!(said.contains("'ms_abi' attribute ignored"), "{said}");
    assert!(said.contains("'sysv_abi' attribute ignored"), "{said}");

    let (ok, said) = diagnose("object", LINUX, "__attribute__((ms_abi)) int k;\n");
    assert!(ok, "{said}");
    assert!(said.contains("'ms_abi' attribute only applies to function types"), "{said}");
}

/// 32-bit Windows, where `stdcall` and `fastcall` are conventions of their own.
const WINDOWS_32: &str = "i686-w64-windows-gnu";

/// `stdcall` and `fastcall` on 32-bit Windows, as i686-w64-mingw32-gcc has them: the callee takes
/// its arguments off with `ret $n`, `fastcall` passes the first two small integers in `ecx` and
/// `edx`, and each name carries the bytes of its arguments, `_s@8` and `@f@12`. A variadic one is
/// plain cdecl, name and all.
#[test]
fn stdcall_and_fastcall_pop_their_arguments_and_say_so_in_the_name() {
    let text = asm(
        "i686-conventions",
        WINDOWS_32,
        "__attribute__((stdcall, noinline)) int s(int a, int b) { return a - b; }\n\
         __attribute__((fastcall, noinline)) int f(int a, int b, int c) { return a - b - c; }\n\
         __attribute__((stdcall, noinline)) int v(int n, ...) { return n; }\n\
         __attribute__((fastcall, noinline)) long long w(long long a, int b) { return a + b; }\n\
         int g(void) { return s(1, 2) + f(3, 4, 5) + v(6) + (int)w(7, 8); }\n",
    );
    let s = body(&text, "_s@8");
    assert!(has(&s, &["ret", "$8"]), "{s:?}\n{text}");
    let f = body(&text, "@f@12");
    assert!(has(&f, &["ret", "$4"]), "{f:?}\n{text}");
    assert!(has(&f, &["%ecx"]) && has(&f, &["%edx"]), "{f:?}");
    let v = body(&text, "_v");
    assert!(v.contains(&"ret") && !has(&v, &["ret", "$"]), "{v:?}\n{text}");
    // A `long long` on the stack takes both registers with it, so `b` is on the stack too.
    let w = body(&text, "@w@12");
    assert!(has(&w, &["ret", "$12"]), "{w:?}\n{text}");
    let g = body(&text, "_g");
    for callee in ["_s@8", "@f@12", "_v", "@w@12"] {
        assert!(has(&g, &["call", callee]), "{callee}: {g:?}");
    }
    assert!(has(&g, &["$3", "%ecx"]) && has(&g, &["$4", "%edx"]), "{g:?}");
    // The stack pointer goes back down right after each call that took its arguments with it,
    // before a result can be stored anywhere the frame reaches through it.
    for (callee, bytes) in [("_s@8", "$8"), ("@f@12", "$4"), ("@w@12", "$12")] {
        let at = g.iter().position(|line| line.contains("call") && line.contains(callee));
        let next = at.and_then(|at| g.get(at + 1)).copied().unwrap_or_default();
        assert!(next.starts_with("subl") && next.contains(bytes), "{callee}: {g:?}");
    }
}

/// A structure holding a `double` is eight byte aligned on 32-bit Windows, and it still goes at the
/// next word of the argument area, as i686-w64-mingw32-gcc has it: `g(1, y)` has `1` at `(%esp)`
/// and the first word of `y` at `4(%esp)`, with no padding between them. Padding there is what
/// a `va_arg` of the structure, which steps a word at a time, read as the structure.
#[test]
fn an_eight_byte_aligned_structure_goes_at_the_next_word_on_32_bit_windows() {
    let text = asm(
        "i686-aligned-struct",
        WINDOWS_32,
        "struct h { double a, b; };\n\
         _Static_assert(_Alignof(struct h) == 8, \"aligned as on Windows\");\n\
         double g(int n, ...);\n\
         double c(struct h y) { return g(1, y); }\n",
    );
    let c = body(&text, "_c");
    let at = c.iter().position(|line| line.starts_with("call") && line.contains("_g"));
    let before = &c[..at.unwrap_or_else(|| panic!("no call to _g: {c:?}"))];
    for slot in [", 4(%esp)", ", 8(%esp)", ", 12(%esp)", ", 16(%esp)"] {
        assert!(has(before, &["movl", slot]), "{slot}: {c:?}\n{text}");
    }
    assert!(!has(before, &[", 20(%esp)"]), "{c:?}\n{text}");
}

/// Two of the three on one function is gcc's error, and on 32-bit Linux they are not this
/// compiler's conventions and are left as they are.
#[test]
fn two_32_bit_conventions_on_one_function_are_refused() {
    let (ok, said) =
        diagnose("i686-both", WINDOWS_32, "__attribute__((stdcall, fastcall)) int f(int);\n");
    assert!(!ok, "{said}");
    assert!(said.contains("'stdcall' and 'fastcall' attributes are not compatible"), "{said}");
    let (ok, said) =
        diagnose("i686-cdecl", WINDOWS_32, "__attribute__((cdecl)) int f(int);\nint f(int);\n");
    assert!(ok && said.is_empty(), "{said}");
}
