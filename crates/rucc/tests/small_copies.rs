//! A `memcpy`, `memset` or `memmove` with a small constant length is written as moves, not a call.
//!
//! gcc does this at every level, with general purpose registers when the vector ones are off, and
//! the kernel counts on it. A call to `memcpy` in `noinstr` code is an objtool error, and
//! `memcpy(&a, &b, sizeof(a))` is how a lot of kernel code copies a structure. tamnd/rucc#2285.

use std::path::PathBuf;
use std::process::Command;

/// The kernel's target, where the moves are checked by what they look like.
const TARGET: &str = "x86_64-unknown-linux-gnu";

const DECLS: &str = "\
typedef unsigned long size_t;
void *memcpy(void *, const void *, size_t);
void *memset(void *, int, size_t);
void *memmove(void *, const void *, size_t);
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-small-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, format!("{DECLS}{source}")).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source under those flags.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-fno-pie", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The lines of one function, from its label to its `.size`.
fn body<'a>(text: &'a str, name: &str) -> &'a str {
    let from = text.find(&format!("\n{name}:\n")).unwrap_or_else(|| panic!("{name} in\n{text}"));
    let rest = &text[from..];
    &rest[..rest.find("\t.size\t").unwrap_or(rest.len())]
}

#[test]
fn a_copy_or_fill_of_up_to_64_bytes_is_moves_at_every_level() {
    let source = "\
void c8(void *d, const void *s) { memcpy(d, s, 8); }
void c13(void *d, const void *s) { __builtin_memcpy(d, s, 13); }
void c64(void *d, const void *s) { memcpy(d, s, 64); }
void f40(void *d) { memset(d, 0xab, 40); }
void fv(void *d, int c) { memset(d, c, 16); }
void m24(void *d, const void *s) { memmove(d, s, 24); }
";
    for level in ["-O0", "-O2"] {
        let text = asm("small", &[level], source);
        assert!(!text.contains("call"), "{level}:\n{text}");
        // gcc's plan for thirteen bytes, a word at 0 and a word that overlaps it at 5.
        let c13 = body(&text, "c13");
        assert!(c13.contains("5(%rsi)") && c13.contains("5(%rdi)"), "{level}:\n{c13}");
        assert_eq!(body(&text, "c64").matches("(%rdi)\n").count(), 8, "{level}:\n{text}");
        // The byte spread across a word once and stored five times.
        assert_eq!(body(&text, "f40").matches("-6076574518398440533").count(), 1, "{text}");
        assert!(body(&text, "fv").contains("imulq"), "{level}:\n{text}");
    }
}

/// `read_header` from tamnd/rucc#2298. The copy into `h` is an operation rather than a call, and
/// once it is, scalar replacement takes `h` out of memory, so what is left is the load from `rec`
/// and the call through `out`, which is gcc's two instructions.
#[test]
fn a_local_filled_by_a_small_copy_stays_off_the_stack() {
    let source = "\
void read_header(const char *rec, void (*out)(unsigned)) {
    unsigned h;
    memcpy(&h, rec, sizeof(h));
    out(h);
}
";
    let text = asm("header", &["-O2"], source);
    let header = body(&text, "read_header");
    assert!(!header.contains("memcpy"), "{header}");
    assert!(!header.contains("%rsp"), "{header}");
    assert!(header.contains("(%rdi), %edi"), "{header}");
}

#[test]
fn a_longer_one_is_still_a_call() {
    let source = "\
void c65(void *d, const void *s) { memcpy(d, s, 65); }
void f4k(void *d) { memset(d, 0, 4096); }
void cn(void *d, const void *s, size_t n) { memcpy(d, s, n); }
";
    let text = asm("long", &["-O2"], source);
    assert!(body(&text, "c65").contains("call\tmemcpy"), "{text}");
    assert!(body(&text, "f4k").contains("call\tmemset"), "{text}");
    assert!(body(&text, "cn").contains("call\tmemcpy"), "{text}");
}

#[test]
fn only_the_builtin_spelling_is_expanded_under_no_builtin() {
    let source = "\
void plain(void *d, const void *s) { memcpy(d, s, 8); }
void spelled(void *d, const void *s) { __builtin_memcpy(d, s, 8); }
void fill(void *d) { memset(d, 0, 8); }
";
    let text = asm("nobuiltin", &["-O2", "-fno-builtin"], source);
    assert!(body(&text, "plain").contains("call\tmemcpy"), "{text}");
    assert!(!body(&text, "spelled").contains("call"), "{text}");
    assert!(body(&text, "fill").contains("call\tmemset"), "{text}");

    let text = asm("nobuiltin-one", &["-O2", "-fno-builtin-memset"], source);
    assert!(!body(&text, "plain").contains("call"), "{text}");
    assert!(body(&text, "fill").contains("call\tmemset"), "{text}");
}

/// Every length from 0 to 64 at four misalignments, with bytes on either side that must not
/// change, and a `memmove` both ways over itself, checked by running it.
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
#[test]
fn every_small_length_gives_the_right_bytes() {
    let mut cases = String::new();
    for n in 0..=64 {
        cases.push_str(&format!("T({n}) "));
    }
    let source = format!(
        "\
static unsigned char buf[256], ref[256];
static void reset(void) {{ for (int i = 0; i < 256; i++) buf[i] = ref[i] = (unsigned char)(i * 7 + 3); }}
static void slow(unsigned char *d, const unsigned char *s, int n) {{
    unsigned char t[256];
    for (int i = 0; i < n; i++) t[i] = s[i];
    for (int i = 0; i < n; i++) d[i] = t[i];
}}
static int same(void) {{ for (int i = 0; i < 256; i++) if (buf[i] != ref[i]) return 0; return 1; }}
__attribute__((noinline)) int byte(int c) {{ return c; }}
static int bad;
#define ONE(n, o) \\
    reset(); if (memcpy(buf + o, buf + 129 + o, n) != buf + o) bad++; slow(ref + o, ref + 129 + o, n); if (!same()) bad++; \\
    reset(); memset(buf + o, 0xab, n); for (int i = 0; i < n; i++) ref[o + i] = 0xab; if (!same()) bad++; \\
    reset(); memset(buf + o, byte(0x15a), n); for (int i = 0; i < n; i++) ref[o + i] = 0x5a; if (!same()) bad++; \\
    reset(); memmove(buf + o + 3, buf + o, n); slow(ref + o + 3, ref + o, n); if (!same()) bad++; \\
    reset(); memmove(buf + o, buf + o + 5, n); slow(ref + o, ref + o + 5, n); if (!same()) bad++;
#define T(n) for (int o = 0; o < 8; o += 1 + o) {{ ONE(n, o) }}
int main(void) {{ {cases} return bad; }}
"
    );
    for level in ["-O0", "-O2"] {
        let path = fixture(&format!("run{level}"), &source);
        let prog = path.with_extension("");
        let built = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .arg(level)
            .arg("-o")
            .arg(&prog)
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&prog).output().expect("what was linked can be run");
        let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
        assert_eq!(ran.status.code(), Some(0), "wrong bytes at {level}");
    }
}
