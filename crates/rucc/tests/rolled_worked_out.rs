//! A copy or fill whose size the program works out, a whole number of words no larger than 8192,
//! is written as `rep movsq` or `rep stosq` under `-mno-sse` on x86-64, as gcc does.
//!
//! The maple tree behind `brk` and `mmap` moves parts of a node by `size * sizeof(unsigned long)`
//! with `size` an `unsigned char`. rucc called `memcpy` 17 times and `memset` 7 times for each
//! grow and shrink of `brk` where gcc has one call. A size the range does not bound, or one that
//! is not a whole number of words, still calls the library as it does under gcc.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SIZES: &str = r#"
typedef unsigned long size_t;
extern void *memcpy(void *, const void *, size_t);
extern void *memset(void *, int, size_t);
void copy(unsigned long *d, unsigned long *s, unsigned char n) { memcpy(d, s, n * sizeof(long)); }
void clear(unsigned long *d, unsigned char e) {
    unsigned char n = 16 - e;
    memset(d + e, 0, n * sizeof(long));
}
void fill(unsigned long *d, unsigned char n) { memset(d, 0x5a, (size_t)n << 3); }
void *kept(unsigned long *d, unsigned long *s, unsigned short n) {
    return memcpy(d, s, (n & 0x3ff) * 8);
}
void unbounded(unsigned long *d, unsigned long *s, unsigned char n) {
    memcpy(d, s, (n - 1) * sizeof(long));
}
void halves(unsigned int *d, unsigned int *s, unsigned char n) { memcpy(d, s, n * 4); }
void wide(unsigned long *d, unsigned long *s, unsigned short n) { memcpy(d, s, n * 8); }
"#;

const RUN: &str = r#"
typedef unsigned long size_t;
extern void *memcpy(void *, const void *, size_t);
extern void *memset(void *, int, size_t);
extern int printf(const char *, ...);
__attribute__((noinline)) void copy(unsigned long *d, unsigned long *s, unsigned char n) {
    memcpy(d, s, n * sizeof(long));
}
__attribute__((noinline)) void *kept(unsigned long *d, unsigned long *s, unsigned short n) {
    return memcpy(d, s, (n & 0x3ff) * 8);
}
__attribute__((noinline)) void fill(unsigned long *d, unsigned char n) {
    memset(d, 0x5a, (size_t)n << 3);
}
static unsigned long from[1100], to[1100];
static int check(unsigned n, unsigned long byte) {
    for (unsigned i = 0; i < 1100; i++)
        if (to[i] != (i < n ? byte ? byte : from[i] : 7)) return 1;
    return 0;
}
int main(void) {
    int bad = 0;
    unsigned sizes[] = { 0, 1, 2, 7, 16, 31, 255 };
    for (unsigned k = 0; k < sizeof sizes / sizeof *sizes; k++) {
        unsigned n = sizes[k];
        for (unsigned i = 0; i < 1100; i++) { from[i] = i * 0x9e3779b97f4a7c15ul; to[i] = 7; }
        copy(to, from, n);
        bad |= check(n, 0);
        for (unsigned i = 0; i < 1100; i++) to[i] = 7;
        fill(to, n);
        bad |= check(n, 0x5a5a5a5a5a5a5a5aul);
    }
    for (unsigned i = 0; i < 1100; i++) to[i] = 7;
    bad |= kept(to, from, 0x1400 | 1023) != to;
    bad |= check(1023, 0);
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
fn a_bounded_number_of_words_is_a_string_instruction() {
    let dir = std::env::temp_dir().join(format!("rucc-worked-out-{}", std::process::id()));
    let flags = ["--target=x86_64-unknown-linux-gnu", "-O2", "-mno-sse", "-S"];
    let out = compile(&dir, SIZES, &flags, "one.s");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let asm = std::fs::read_to_string(dir.join("one.s")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let strings = [
        ("copy", "rep movsq"),
        ("clear", "rep stosq"),
        ("fill", "rep stosq"),
        ("kept", "rep movsq"),
    ];
    for (name, string) in strings {
        let body = function(&asm, name);
        assert!(body.contains(string), "{name}:\n{body}");
        assert!(!body.contains("mem"), "{name}:\n{body}");
    }
    // The count is the number of words, not the bytes shifted down again.
    assert!(!function(&asm, "copy").contains("shr"), "{asm}");
    for name in ["unbounded", "halves", "wide"] {
        assert!(function(&asm, name).contains("memcpy"), "{name}:\n{asm}");
    }
}

#[test]
fn the_string_instruction_moves_the_words_the_library_would() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("rucc-worked-out-run-{}", std::process::id()));
    for level in ["-O1", "-O2", "-O3"] {
        let out = compile(&dir, RUN, &[level, "-mno-sse"], "one");
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let ran = Command::new(dir.join("one")).output().expect("the program runs");
        assert!(ran.status.success(), "{level}: {}", String::from_utf8_lossy(&ran.stdout));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
