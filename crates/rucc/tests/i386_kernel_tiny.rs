//! Two shapes from an i386 kernel built with tinyconfig's `-Os -fomit-frame-pointer`, where every
//! register but `esp` is the allocator's.
//!
//! A call through a pointer with three arguments under `-mregparm=3` has all three registers a
//! callee may write holding arguments, so the address has none of those to wait in and the call
//! cannot become a jump. It stays a call and a `ret`, the way gcc writes it, where it used to stop
//! the compiler. lib/sort.c, kernel/params.c and lib/kobject.c each have one.
//!
//! A byte written with `"iq"` that is not a constant is in a register with a low byte. The
//! kernel's `unsafe_put_user` of a byte is that, and fs/readdir.c had it in `ebp`, so the
//! assembler stopped at `movb %bpl`.

use std::process::Command;

const SOURCE: &str = "\
typedef void (*swap_t)(void *, void *, int);
void sw(void *a, void *b, int n, swap_t f) { f(a, b, n); }
void (*hook)(int, int, int);
void call3(int a, int b, int c) { hook(a, b, c); }
struct d { unsigned long ino; unsigned short len; unsigned char type; char name[]; };
int put(struct d *u, const char *name, int len, unsigned long ino, unsigned char *tp, int *err)
{
    unsigned char t = *tp;
    unsigned long a = ino, b = len, c = (unsigned long)name;
    asm goto(\"1: movl %0,%1\" : : \"ir\"(a), \"m\"(u->ino) : : bad);
    asm goto(\"1: movw %w0,%1\" : : \"ir\"((unsigned short)b), \"m\"(u->len) : : bad);
    asm goto(\"1: movb %b0,%1\" : : \"iq\"(t), \"m\"(u->type) : : bad);
    asm goto(\"1: movb %b0,%1\" : : \"iq\"((unsigned char)c), \"m\"(u->name[len]) : : bad);
    asm goto(\"1: movb %b0,%1\" : : \"iq\"((unsigned char)7), \"m\"(u->name[0]) : : bad);
    *err = a + b + c;
    return 0;
bad:
    *err = -14;
    return -1;
}
";

/// The listing at `level`, written in a directory of its own for `test`, as the tests run at once.
fn listing(test: &str, level: &str) -> String {
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("rucc-i386-kernel-tiny-{id}-{test}{level}"));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-pic", "-mregparm=3"])
        .args(["-fomit-frame-pointer", "-march=i686", "-mpreferred-stack-boundary=2"])
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn a_call_with_every_scratch_register_taken_stays_a_call() {
    for level in ["-O1", "-O2", "-Os"] {
        let text = listing("call", level);
        for name in ["sw:", "call3:"] {
            let body = text.split(name).nth(1).expect("the function is in the listing");
            let body = body.split(".size").next().unwrap_or(body);
            assert!(body.contains("call\t*"), "{level} {name}\n{text}");
        }
    }
}

#[test]
fn a_byte_that_may_be_a_constant_has_a_low_byte_when_it_is_not() {
    for level in ["-O1", "-O2", "-Os"] {
        let text = listing("byte", level);
        for reg in ["%bpl", "%sil", "%dil", "%spl"] {
            assert!(!text.contains(reg), "{level} {reg}\n{text}");
        }
        assert!(text.contains("movb $7,"), "{level}\n{text}");
    }
}
