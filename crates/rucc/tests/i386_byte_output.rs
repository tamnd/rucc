//! A `q` output on i386 when the statement takes every register with a low byte. The kernel's
//! `__arch_try_cmpxchg64` hands `cmpxchg8b` `edx:eax` and `ecx:ebx` and reads the answer back
//! with `CC_OUT(e)`, which is a `"=q"` written with `sete`. Only those four registers have a low
//! byte on i386, and the output used to land in `esi`, where `sete %sil` is not an instruction
//! the machine has. gcc puts it in `ebx` or `ecx`, which an output not written early may share
//! with an input, and so does this.

use std::process::Command;

const SOURCE: &str = "\
unsigned long long v;
int f(unsigned long long o, unsigned lo, unsigned hi) {
  int ret;
  asm volatile(\"lock cmpxchg8b %[ptr]\" : \"=@ccz\"(ret), [ptr] \"+m\"(v), \"+A\"(o)
               : \"b\"(lo), \"c\"(hi) : \"memory\");
  return ret + lo + hi;
}
int g(unsigned long long *p, unsigned long long o, unsigned long long n) {
  unsigned char ok;
  asm volatile(\"lock; cmpxchg8b %1\\n\\tsete %0\" : \"=q\"(ok), \"+m\"(*p), \"+A\"(o)
               : \"b\"((unsigned)n), \"c\"((unsigned)(n >> 32)) : \"memory\");
  return ok;
}
";

fn listing(level: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-i386-byte-{}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-fno-pic", level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn the_byte_output_shares_a_register_with_an_input() {
    for level in ["-O0", "-O2"] {
        let text = listing(level);
        for byteless in ["%sil", "%dil", "%bpl", "%spl"] {
            assert!(!text.contains(byteless), "{level}: {byteless}:\n{text}");
        }
        let set = |name: &str| {
            text.contains(&format!("{name} %bl")) || text.contains(&format!("{name} %cl"))
        };
        assert!(set("setz") && set("sete"), "{level}:\n{text}");
    }
}
