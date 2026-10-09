//! An operand in memory that is a constant past a pointer, which the template reads at that
//! displacement from the pointer, as gcc writes it. The kernel's atomics and bit operations are
//! all `"+m" (v->counter)` or `"+m" (*(addr + nr / 8))`, and each one was an `add` of the offset
//! into a register of its own in front of the `lock`.

use std::process::Command;

const SOURCE: &str = "\
struct s { int pad[31]; unsigned long flags; int counter; };
static inline void hook(const volatile void *v) {
    unsigned long p;
    __asm__ (\"\" : \"=r\" (p) : \"0\" ((void *)v));
    (void)p;
}
static inline void clear_bit(long nr, volatile unsigned long *addr) {
    hook(addr + nr / 32);
    asm volatile (\"lock andb %b1,%0\"
        : \"+m\" (*(volatile char *)((void *)addr + (nr >> 3)))
        : \"iq\" (~(1 << (nr & 7))));
}
static inline void inc(int *v) { asm volatile (\"lock incl %0\" : \"+m\" (*v)); }
void two(struct s *p) { clear_bit(0, &p->flags); clear_bit(9, &p->flags); }
void one(struct s *p) { inc(&p->counter); }
";

fn listing(level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-asm-memory-offset-{}-{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-mregparm=3", "-fno-pic"])
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

fn body(listing: &str, name: &str) -> String {
    let start = listing.find(&format!("\n{name}:\n")).expect(name);
    let rest = &listing[start + name.len() + 3..];
    let end = rest.find("\n\t.size").unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[test]
fn a_constant_past_a_pointer_is_the_displacement() {
    for level in ["-O2", "-Os"] {
        let listing = listing(level);
        let two = body(&listing, "two");
        assert!(two.contains("lock andb $-2,124(%eax)"), "{level}:\n{two}");
        assert!(two.contains("lock andb $-3,125(%eax)"), "{level}:\n{two}");
        let one = body(&listing, "one");
        assert!(one.contains("lock incl 128(%eax)"), "{level}:\n{one}");
        for body in [two, one] {
            assert!(!body.contains("addl\t"), "{level}:\n{body}");
            assert!(!body.contains("movl\t"), "{level}:\n{body}");
            assert!(!body.contains("leal\t"), "{level}:\n{body}");
        }
    }
}
