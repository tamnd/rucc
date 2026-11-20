//! Inline assembly on x86-64 with an `__int128` operand, which gcc puts in a pair of registers: the
//! product of `mulq` in `rdx:rax`, and the value `cmpxchg16b` compares in the same pair.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
typedef unsigned __int128 u128;
u128 mul(unsigned long a, unsigned long b) { u128 r; asm(\"mulq %2\" : \"=A\"(r) : \"a\"(a), \"rm\"(b)); return r; }
u128 cx(u128 *p, u128 old, unsigned long lo, unsigned long hi)
{
    asm volatile(\"lock; cmpxchg16b %1\" : \"+A\"(old), \"+m\"(*p) : \"b\"(lo), \"c\"(hi) : \"memory\");
    return old;
}
u128 tie(u128 x)
{
    u128 r;
    asm(\"addq $1,%%rax; adcq $0,%%rdx\" : \"=A\"(r) : \"0\"(x));
    return r;
}
u128 hide(u128 x) { asm(\"\" : \"+r\"(x)); return x; }
u128 pinned(u128 x)
{
    register u128 v asm(\"rax\") = x;
    asm(\"addq $1,%%rax; adcq $0,%%rdx\" : \"+r\"(v));
    return v;
}
";

/// The listing of each function, by name.
fn bodies(level: &str) -> Vec<(String, String)> {
    // The tests in this file run on threads of one process and ask for the same levels, so the
    // process id and the level alone would give two of them the same directory, and one could
    // remove it while the other was still compiling the file in it.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("rucc-x86-64-wide-asm-{}-{level}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", level, "-fno-asynchronous-unwind-tables", "-S"])
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let listing = String::from_utf8(out.stdout).expect("a listing is text");
    let mut bodies: Vec<(String, String)> = Vec::new();
    for line in listing.lines() {
        if let Some(name) = line.strip_suffix(':').filter(|name| !name.starts_with('.')) {
            bodies.push((name.to_owned(), String::new()));
        } else if let Some((_, body)) = bodies.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    bodies
}

fn body<'a>(bodies: &'a [(String, String)], name: &str) -> &'a str {
    bodies.iter().find(|(named, _)| named == name).map(|(_, body)| body.as_str()).expect(name)
}

/// `A` is the pair `rdx:rax`. The product of `mulq` is there already and is the return value with
/// nothing to move, a `+A` operand is loaded into both halves before the instruction that reads it,
/// and the template's `%1` still names the memory operand after the pair became two operands.
#[test]
fn an_int128_written_a_is_rdx_and_rax() {
    for level in ["-O0", "-O2"] {
        let bodies = bodies(level);
        let mul = body(&bodies, "mul");
        assert!(mul.contains("mulq\t%rsi"), "{level}: {mul}");
        assert!(level == "-O0" || !mul.contains("%rdx"), "{level}: {mul}");
        let cx = body(&bodies, "cx");
        assert!(cx.contains("lock; cmpxchg16b (%rdi)"), "{level}: {cx}");
        let tie = body(&bodies, "tie");
        let low = tie.find("movq\t%rdi, %rax").zip(tie.find("addq\t$1, %rax"));
        let high = tie.find("movq\t%rsi, %rdx").zip(tie.find("adcq\t$0, %rdx"));
        assert!(low.zip(high).is_some_and(|((a, b), (c, d))| a < b && c < d), "{level}: {tie}");
    }
}

/// A pair the template does not name goes through the statement as two registers, and a local
/// register variable that starts at `rax` has its high half in `rdx`, as in gcc.
#[test]
fn an_int128_under_another_letter_is_two_registers() {
    for level in ["-O0", "-O2"] {
        let bodies = bodies(level);
        assert!(body(&bodies, "hide").contains("ret"), "{level}: {bodies:?}");
        let pinned = body(&bodies, "pinned");
        assert!(pinned.contains("addq\t$1, %rax"), "{level}: {pinned}");
        assert!(pinned.contains("adcq\t$0, %rdx"), "{level}: {pinned}");
    }
}
