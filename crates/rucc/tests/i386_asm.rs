//! Inline assembly on i386, which the kernel's 32 bit build has in nearly every header: port I/O,
//! control registers, `cpuid`, and the `long long` in `edx:eax` that `rdtsc` and `cmpxchg8b` use.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
unsigned long cr0(void) { unsigned long v; asm volatile(\"movl %%cr0,%0\" : \"=r\"(v)); return v; }
void outb(unsigned char v, unsigned short port) { asm volatile(\"outb %b0,%w1\" : : \"a\"(v), \"Nd\"(port)); }
unsigned long long tsc(void) { unsigned long long v; asm volatile(\"rdtsc\" : \"=A\"(v)); return v; }
unsigned long long cx(unsigned long long *p, unsigned long long old, unsigned lo, unsigned hi)
{
    asm volatile(\"lock; cmpxchg8b %1\" : \"+A\"(old), \"+m\"(*p) : \"b\"(lo), \"c\"(hi) : \"memory\");
    return old;
}
unsigned long long tie(unsigned long long x)
{
    unsigned long long r;
    asm(\"addl $1,%%eax; adcl $0,%%edx\" : \"=A\"(r) : \"0\"(x));
    return r;
}
";

/// The listing of each function, by name.
fn bodies(level: &str) -> Vec<(String, String)> {
    // The tests in this file run on threads of one process and ask for the same levels, so the
    // process id and the level alone would give two of them the same directory, and one could
    // remove it while the other was still compiling the file in it.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-asm-{}-{level}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-asynchronous-unwind-tables", "-S"])
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

/// The operands are written with their 32 bit names, and a modifier picks the byte or word one.
#[test]
fn operands_are_the_thirty_two_bit_registers() {
    for level in ["-O0", "-O2"] {
        let bodies = bodies(level);
        assert!(body(&bodies, "cr0").contains("movl %cr0,%e"), "{bodies:?}");
        assert!(body(&bodies, "outb").contains("outb %al,%dx"), "{bodies:?}");
    }
}

/// `A` is the pair `edx:eax`: `rdtsc` leaves the result there with nothing to move, a `+A`
/// operand is loaded into both halves first, and the template's `%1` still names the memory
/// operand after the pair became two operands.
#[test]
fn a_long_long_written_a_is_edx_and_eax() {
    for level in ["-O0", "-O2"] {
        let bodies = bodies(level);
        let tsc = body(&bodies, "tsc");
        assert!(!tsc.contains(", %eax") && !tsc.contains(", %edx"), "{level}: {tsc}");
        let cx = body(&bodies, "cx");
        assert!(cx.contains("lock; cmpxchg8b ("), "{level}: {cx}");
        let tie = body(&bodies, "tie");
        let loads = tie.find("%eax\n").zip(tie.find("%edx\n"));
        let asm = tie.find("addl $1,%eax");
        assert!(loads.zip(asm).is_some_and(|((a, d), asm)| a < asm && d < asm), "{level}: {tie}");
    }
}
