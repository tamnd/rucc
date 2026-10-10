//! An ordered comparison of two `long long` on i386 is a `cmpl` of the low halves and an `sbbl` of
//! the high ones, and a branch on it is the jump on the flags the `sbbl` left. It used to be three
//! comparisons, three `setcc`, an `andb` and an `orb`, which drivers/md/md.c had hundreds of over
//! its `u64` sector numbers. The same goes for `__int128` on x86-64 with `cmpq` and `sbbq`.
//! `k < x` with a constant `k` is asked as `x >= k + 1`, since a constant is not something to
//! subtract from.

use std::process::Command;

const SOURCE: &str = "\
void f(void);
void lt(unsigned long long a, unsigned long long b) { if (a < b) f(); }
void sgt(long long a, long long b) { if (a > b) f(); }
void le(unsigned long long a, unsigned long long b) { if (a <= b) f(); }
void over(unsigned long long a) { if (a > 1000) f(); }
int value(long long a, long long b) { return a >= b; }
";

const QUAD: &str = "\
void f(void);
void lt(unsigned __int128 a, unsigned __int128 b) { if (a < b) f(); }
int sge(__int128 a, __int128 b) { return a >= b; }
";

fn listing(target: &str, source: &str, level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-ordered-pairs-{}-{target}-{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let mut command = Command::new(env!("CARGO_BIN_EXE_rucc"));
    command.arg(format!("--target={target}")).arg(level);
    if target.starts_with("i686") {
        command.args(["-fno-pic", "-mregparm=3"]);
    }
    let out = command
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

fn function<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text.find(&format!("\n{name}:\n")).unwrap_or_else(|| panic!("{name}:\n{text}"));
    let rest = &text[start + 1..];
    let end = rest.find("\t.size").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn a_long_long_comparison_is_a_borrow() {
    for level in ["-O2", "-Os"] {
        let text = listing("i686-unknown-linux-gnu", SOURCE, level);
        for (name, jump) in [("lt", "jb"), ("sgt", "jl"), ("le", "jae"), ("over", "jae")] {
            let body = function(&text, name);
            assert!(body.contains("\tsbbl\t"), "{level} {name}:\n{body}");
            assert!(body.contains(&format!("\t{jump}\t")), "{level} {name}:\n{body}");
            assert!(!body.contains("\tset"), "{level} {name}:\n{body}");
            assert!(!body.contains("orb"), "{level} {name}:\n{body}");
        }
        let body = function(&text, "value");
        assert!(body.contains("\tsbbl\t") && body.contains("\tsetge\t"), "{level}:\n{body}");
        assert!(!body.contains("andb") && !body.contains("orb"), "{level}:\n{body}");
    }
}

#[test]
fn an_int128_comparison_is_a_borrow() {
    let text = listing("x86_64-unknown-linux-gnu", QUAD, "-O2");
    let body = function(&text, "lt");
    assert!(body.contains("\tsbbq\t") && body.contains("\tjb\t"), "{body}");
    assert!(!body.contains("\tset"), "{body}");
    let body = function(&text, "sge");
    assert!(body.contains("\tsbbq\t") && body.contains("\tsetge\t"), "{body}");
    assert!(!body.contains("andb") && !body.contains("orb"), "{body}");
}
