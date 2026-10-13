//! An i386 asm that names `ecx`, `esi` and `edi` and asks for three more registers, with `ebp`
//! holding the frame. That is every register the machine has, and the compiler panicked with "an
//! instruction naming every register of its class at once". The C `memmove` in
//! arch/x86/lib/memcpy_32.c up to Linux 6.1 is the kernel's asm of that shape.
//!
//! Two things were wrong. The allocator put the answer in `ecx` somewhere else, and a register an
//! answer is only moved into behind the asm was not one a spilled answer could use, so the six
//! answers wanted seven registers. And the kernel's template writes `%%dx` and `%%bx` without
//! naming them, which took two more away as clobbers. An output nothing reads now takes such a
//! register, the way gcc's allocator gives it one.

use std::process::Command;

const NAMED: &str = "\
typedef unsigned int size_t;
void *memmove(void *dest, const void *src, size_t n) {
  int d0, d1, d2, d3, d4, d5;
  char *ret = dest;
  asm volatile(\"cmp $0x10, %0\\n\\tmov 0(%1), %3\\n\\tmov 4(%1), %4\\n\\tmov 8(%1), %5\\n\\t\"
               \"mov %3, 0(%2)\\n\\tmov %4, 4(%2)\\n\\tmov %5, 8(%2)\"
               : \"=&c\"(d0), \"=&S\"(d1), \"=&D\"(d2), \"=r\"(d3), \"=r\"(d4), \"=r\"(d5)
               : \"0\"(n), \"1\"(src), \"2\"(dest)
               : \"memory\");
  return ret;
}
";

const SPELLED: &str = "\
typedef unsigned int size_t;
void *memmove(void *dest, const void *src, size_t n) {
  int d0, d1, d2, d3, d4, d5;
  char *ret = dest;
  asm volatile(\"cmp $2, %0\\n\\tjb 1f\\n\\tmov 0(%1), %3\\n\\tmov 4(%1), %4\\n\\tmov 8(%1), %5\\n\\t\"
               \"movw 0*2(%1), %%dx\\n\\tmovw -1*2(%1, %0), %%bx\\n\\t\"
               \"movw %%dx, 0*2(%2)\\n\\tmovw %%bx, -1*2(%2, %0)\\n1:\"
               : \"=&c\"(d0), \"=&S\"(d1), \"=&D\"(d2), \"=r\"(d3), \"=r\"(d4), \"=r\"(d5)
               : \"0\"(n), \"1\"(src), \"2\"(dest)
               : \"memory\");
  return ret;
}
";

fn compile(name: &str, source: &str, level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-every-register-{name}-{}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-fno-pic", "-mregparm=3"])
        .args(["-fno-omit-frame-pointer", "-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name} {level}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn an_asm_naming_every_register_is_allocated() {
    for level in ["-O0", "-O1", "-O2"] {
        let text = compile("named", NAMED, level);
        assert!(text.contains("cmp"), "{level}: the asm is in the output\n{text}");
    }
}

#[test]
fn a_register_the_template_only_spells_goes_to_an_output_nothing_reads() {
    for level in ["-O1", "-O2"] {
        let text = compile("spelled", SPELLED, level);
        assert!(text.contains("%dx"), "{level}: the asm is in the output\n{text}");
    }
}
