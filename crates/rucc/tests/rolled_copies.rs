//! A copy or fill of a known size too large for a run of moves under `-mno-sse` on x86-64, which
//! gcc writes as `rep movsq` or `rep stosq` up to 8192 bytes and rucc called `memcpy` or `memset`
//! for, and then wrote as a loop of four words (tamnd/rucc-kernel#109).
//!
//! Every kernel file is built with `-mno-sse`, and the copy of `struct pt_regs` on a page fault
//! and the zeroing of `struct kstat` on a `stat` were calls to the kernel's `__memcpy` and
//! `__memset`. Each case below is what gcc 13 does with the same source and flags.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SIZES: &str = r#"
typedef unsigned long size_t;
extern void *memcpy(void *, const void *, size_t);
extern void *memset(void *, int, size_t);
struct regs { unsigned long r[21]; };
struct stat { char a[192]; };
struct big { char a[8192]; };
struct huge { char a[8200]; };
struct six { unsigned long a[6]; };
void copy(struct regs *to, const struct regs *from) { *to = *from; }
void clear(struct stat *s) { memset(s, 0, sizeof *s); }
void *kept(char *to, const char *from) { return memcpy(to, from, 200); }
void large(struct big *to, const struct big *from) { *to = *from; }
void small(struct six *s) { memset(s, 0, sizeof *s); }
void huge(struct huge *to, const struct huge *from) { *to = *from; }
"#;

const RUN: &str = r#"
extern void *memcpy(void *, const void *, unsigned long);
extern void *memset(void *, int, unsigned long);
extern int printf(const char *, ...);
#define SIZE(n) \
    static int copy##n(void) { \
        unsigned char from[n + 16], to[n + 16]; \
        for (int i = 0; i < n + 16; i++) { from[i] = i * 7 + n; to[i] = 0xee; } \
        if (memcpy(to + 3, from + 5, n) != to + 3) return 1; \
        for (int i = 0; i < n + 16; i++) \
            if (to[i] != (i >= 3 && i < n + 3 ? from[i + 2] : 0xee)) return 1; \
        if (memset(to + 1, 0x5a, n) != to + 1) return 1; \
        for (int i = 0; i < n + 16; i++) \
            if (to[i] != (i >= 1 && i < n + 1 ? 0x5a : i >= 3 && i < n + 3 ? from[i + 2] : 0xee)) \
                return 1; \
        return 0; \
    }
#define ZERO(n) \
    static int zero##n(void) { \
        unsigned char to[n + 16]; \
        for (int i = 0; i < n + 16; i++) to[i] = 0xee; \
        if (memset(to + 7, 0, n) != to + 7) return 1; \
        for (int i = 0; i < n + 16; i++) \
            if (to[i] != (i >= 7 && i < n + 7 ? 0 : 0xee)) return 1; \
        return 0; \
    }
SIZE(97) SIZE(127) SIZE(128) SIZE(129) SIZE(160) SIZE(168) SIZE(192) SIZE(200) SIZE(255) SIZE(256)
SIZE(257) SIZE(1001) SIZE(8192)
ZERO(41) ZERO(47) ZERO(48) ZERO(95) ZERO(301) ZERO(8191)
int main(void) {
    int bad = copy97() | copy127() | copy128() | copy129() | copy160() | copy168() | copy192()
        | copy200() | copy255() | copy256() | copy257() | copy1001() | copy8192();
    bad |= zero41() | zero47() | zero48() | zero95() | zero301() | zero8191();
    printf("%d\n", bad);
    return bad;
}
"#;

fn compile(dir: &Path, source: &str, flags: &[&str], out: &str) -> Output {
    std::fs::create_dir_all(dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .arg("-o")
        .arg(dir.join(out))
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run")
}

fn function<'a>(asm: &'a str, name: &str) -> &'a str {
    let start = asm.find(&format!("\n{name}:")).unwrap_or_else(|| panic!("no {name} in:\n{asm}"));
    let body = &asm[start + 1..];
    body.find(".size").map_or(body, |end| &body[..end])
}

#[test]
fn a_copy_or_fill_up_to_8192_bytes_is_a_string_instruction_without_the_vector_registers() {
    let dir = std::env::temp_dir().join(format!("rucc-rolled-{}", std::process::id()));
    let flags = ["--target=x86_64-unknown-linux-gnu", "-O2", "-mno-sse", "-S"];
    let out = compile(&dir, SIZES, &flags, "one.s");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let asm = std::fs::read_to_string(dir.join("one.s")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    for (name, string) in [
        ("copy", "rep movsq"),
        ("clear", "rep stosq"),
        ("kept", "rep movsq"),
        ("large", "rep movsq"),
        ("small", "rep stosq"),
    ] {
        let body = function(&asm, name);
        assert!(body.contains(string), "{name}:\n{body}");
        assert!(!body.contains("call"), "{name}:\n{body}");
        assert!(!body.contains("jmp\tmem") && !body.contains("jmp mem"), "{name}:\n{body}");
    }
    // Above 8192 bytes gcc calls the library too.
    assert!(function(&asm, "huge").contains("memcpy"), "{asm}");
}

#[test]
fn a_function_with_the_vector_registers_keeps_the_threshold() {
    let dir = std::env::temp_dir().join(format!("rucc-rolled-sse-{}", std::process::id()));
    let flags = ["--target=x86_64-unknown-linux-gnu", "-O2", "-S"];
    let out = compile(&dir, SIZES, &flags, "one.s");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let asm = std::fs::read_to_string(dir.join("one.s")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(function(&asm, "copy").contains("memcpy"), "{asm}");
}

#[test]
fn the_loop_copies_and_fills_the_bytes_the_library_would() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("rucc-rolled-run-{}", std::process::id()));
    for level in ["-O1", "-O2", "-O3"] {
        let out = compile(&dir, RUN, &[level, "-mno-sse"], "one");
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let ran = Command::new(dir.join("one")).output().expect("the program runs");
        assert!(ran.status.success(), "{level}: {}", String::from_utf8_lossy(&ran.stdout));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
