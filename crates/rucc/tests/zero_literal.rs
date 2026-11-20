//! The zeros an initializer names inside an object the lowering has already filled with zero.
//!
//! `*d = (block){ { 0 } }` fills a temporary with zero and copies it into `*d`. The lowering stored
//! the named zero again after the fill, dead store elimination then narrowed the fill to start one
//! byte in, and a 64 byte block was cleared with fifty misaligned stores and a copy, about nine
//! times as slow as a `memset` of it (tamnd/rucc#3586). Now the zero is left to the fill, a
//! narrowed fill keeps an aligned start, and a copy of a fill is a fill of the destination.

use std::path::{Path, PathBuf};
use std::process::Command;

const LITERAL: &str = "
typedef struct { unsigned char b[64]; } __attribute__((aligned(8))) block;
void literal(block *d) { *d = (block){ { 0 } }; }
";

/// An initializer can name the same element twice, and the later one is what the element holds.
/// A zero that overrides an earlier value has to be stored even though the fill wrote zero there
/// first, because the earlier value was stored after the fill. The exit status is zero when every
/// function gives what C says it does.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const OVERRIDES: &str = r#"
__attribute__((noinline)) int twice(int v) {
    int a[4] = { [1] = v, [1] = 0 };
    return a[0] + a[1] + a[2] + a[3];
}
__attribute__((noinline)) int inside(int v) {
    struct { int a; int b[3]; } x = { .b = { v, v, v }, .b[1] = 0 };
    return x.b[0] + x.b[1] * 10 + x.b[2] * 100;
}
union u { int i; char c[8]; };
__attribute__((noinline)) int rest(int v) {
    union u x = { 0 };
    x.c[7] += v;
    return x.c[5] + x.c[7];
}
__attribute__((noinline)) int bits(int v) {
    struct { unsigned a : 3, b : 5; int c; } x = { .a = v, .a = 0 };
    return x.a + x.b + x.c;
}
__attribute__((noinline)) void dirty(void) {
    volatile int junk[64];
    for (int i = 0; i < 64; i++)
        junk[i] = -1;
}
__attribute__((noinline)) int again(int v) {
    int a[2] = { [0] = v, [0] = v + 1 };
    return a[1];
}
int main(void) {
    dirty();
    int left = again(3);
    return twice(9) == 0 && inside(3) == 303 && rest(4) == 4 && bits(5) == 0 && left == 0 ? 0 : 1;
}
"#;

fn compile(dir: &Path, source: &str, args: &[&str]) -> std::process::Output {
    std::fs::create_dir_all(dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run")
}

fn scratch(what: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rucc-zero-literal-{}-{what}", std::process::id()))
}

#[test]
fn a_zero_literal_assigned_to_a_block_is_eight_aligned_stores_into_it() {
    let dir = scratch("asm");
    let out = compile(
        &dir,
        LITERAL,
        &["--target=x86_64-unknown-linux-gnu", "-fno-pie", "-O2", "-S", "-o", "-"],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let from = text.find("\nliteral:\n").unwrap_or_else(|| panic!("literal in\n{text}"));
    let body = &text[from..];
    let body = &body[..body.find("\t.size\t").unwrap_or(body.len())];
    assert_eq!(body.matches("(%rdi)\n").count(), 8, "{body}");
    for offset in (8..64).step_by(8) {
        assert!(body.contains(&format!(" {offset}(%rdi)\n")), "{offset}:\n{body}");
    }
    // No temporary on the stack and nothing copied out of one.
    assert!(!body.contains("%rsp") && !body.contains("call"), "{body}");
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn a_zero_that_overrides_an_earlier_value_is_still_stored() {
    for level in ["-O0", "-O1", "-O2", "-Os"] {
        let dir = scratch(&format!("run{level}"));
        let exe = dir.join("one");
        let exe_arg = exe.to_string_lossy().into_owned();
        let out = compile(&dir, OVERRIDES, &[level, "-o", &exe_arg]);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let status = Command::new(&exe).status().expect("the program runs");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(status.success(), "{level}: {status}");
    }
}
