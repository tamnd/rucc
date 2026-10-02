//! An i386 asm operand that names `esi`, on a value that lives a long way before it. `esi` is the
//! register the allocator reloads through on i386 and hands out to nothing, and the backtracking
//! allocator took the `"S"` as a hint and kept the whole value there, where the first reload of
//! anything else wrote over it. The EFI stub's `enter_kernel` hands `boot_params` to the kernel
//! that way, and `efi_stub_entry` lost it.

use std::process::Command;

const SOURCE: &str = "\
struct bp { int a, b, c; unsigned long long w; };
void g(struct bp *); int h(int); void k(unsigned long long);
static void __attribute__((noreturn)) enter(unsigned long addr, struct bp *p) {
  asm(\"jmp *%0\" :: \"r\"(addr), \"S\"(p));
  __builtin_unreachable();
}
void __attribute__((noreturn)) entry(int x, struct bp *p, unsigned long addr) {
  int a = h(x), b = h(a), c = h(b), d = h(c);
  p->w = (unsigned long long)p->a << 32 | p->b;
  k(p->w * a);
  g(p);
  p->c = a + b + c + d;
  enter(addr, p);
}
";

#[test]
fn a_value_an_asm_wants_in_esi_is_kept_elsewhere_until_then() {
    for level in ["-O0", "-O1", "-O2"] {
        let dir = std::env::temp_dir()
            .join(format!("rucc-i386-scratch-operand-{}{level}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
        let path = dir.join("one.c");
        std::fs::write(&path, SOURCE).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=i686-unknown-linux-gnu", "-fno-pic", "-S", "-o", "-", level])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let jump = text.find("jmp *").expect("the asm is written");
        let before = &text[..jump];
        let last = before.rfind("%esi").expect("something is put in esi");
        assert!(before[last..].lines().count() <= 3, "{level}: esi set far from the asm\n{text}");
    }
}
