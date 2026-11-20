//! `-mstrict-align` on AArch64, which the kernel asks for in the code that runs before the MMU is
//! on, where an access less aligned than its width faults.
//!
//! A packed member is read and written a piece at a time, a piece as wide as the member is
//! aligned, and a copy through two `char` pointers moves bytes rather than words. Without the flag
//! both stay one access, as before.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
struct __attribute__((packed)) p { char c; int i; long l; void *q; double d; };
struct __attribute__((packed, aligned(2))) h { short s; int i; };
int get(struct p *p) { return p->i; }
void *getq(struct p *p) { return p->q; }
void putl(struct p *p, long v) { p->l = v; }
void putd(struct p *p, double v) { p->d = v; }
int half(struct h *h) { return h->i; }
void cp(char *a, const char *b) { __builtin_memcpy(a, b, 8); }
#ifdef __ARM_FEATURE_UNALIGNED
int unaligned;
#endif
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-strict-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-fno-pic", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out
}

fn listing(flags: &[&str]) -> String {
    let flags = [flags, &["-S"]].concat();
    String::from_utf8(compile(&flags).stdout).expect("a listing is text")
}

/// The loads and stores of one function, without everything else.
fn accesses(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .map(str::trim)
        .filter(|line| line.starts_with("ldr") || line.starts_with("str"))
        .map(|line| line.split(' ').next().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn a_packed_member_is_moved_a_piece_at_a_time() {
    let text = listing(&["-mstrict-align"]);
    for (name, word, count) in [
        ("get", "ldrb", 4),
        ("getq", "ldrb", 8),
        ("putl", "strb", 8),
        ("putd", "strb", 8),
        ("half", "ldrh", 2),
        ("cp", "ldrb", 8),
    ] {
        let seen = accesses(&text, name);
        assert_eq!(seen.iter().filter(|it| *it == word).count(), count, "{name}:\n{text}");
        assert!(seen.iter().all(|it| it.ends_with('b') || it.ends_with('h')), "{name}:\n{text}");
    }
    assert!(!text.contains("unaligned"), "{text}");
}

#[test]
fn without_the_flag_a_member_is_one_access() {
    let text = listing(&[]);
    assert_eq!(accesses(&text, "get"), ["ldr"], "{text}");
    assert_eq!(accesses(&text, "putl"), ["str"], "{text}");
    assert!(text.contains("unaligned"), "{text}");
    // The last of the two spellings is the one that counts.
    let text = listing(&["-mstrict-align", "-mno-strict-align"]);
    assert_eq!(accesses(&text, "get"), ["ldr"], "{text}");
}

#[test]
fn the_object_assembles() {
    let out = compile(&["-mstrict-align", "-c"]);
    assert!(!out.stdout.is_empty());
}
