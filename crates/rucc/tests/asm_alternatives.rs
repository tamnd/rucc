//! An `asm` constraint with alternatives, and a general purpose register named with no modifier
//! on AArch64.
//!
//! GMP's `longlong.h` gives `sub_ddmmss` on AArch64 two alternatives for each operand, `"r,Z"` and
//! `"rI,r"`, and the statement takes the same one for every operand. The first in which no operand
//! has to be a constant is taken, so every operand that is not a constant goes in a register.
//!
//! With no modifier an AArch64 register is `x` whatever the width of its type, as gcc and clang
//! name it, and libgcc's `__aarch64_sync_cache_range` reads `ctr_el0` into an `unsigned int` with
//! `mrs %0`, which takes only an `x` register.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-asm-alt-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// The `-O2` listing of a source that has to compile without a word.
fn listing(what: &str, target: &str, source: &str) -> String {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([target, "-S", "-O2", "-o", "-", "a.c"])
        .current_dir(Path::new(&dir))
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.is_empty(), "{err}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        // A template may put a tab after its mnemonic, as GMP's do.
        .map(|line| line.trim().replacen('\t', " ", 1))
        .collect()
}

/// GMP's `sub_ddmmss` for AArch64, as `longlong.h` writes it.
const GMP: &str = r#"
typedef unsigned long UDItype;
#define sub_ddmmss(sh, sl, ah, al, bh, bl) \
  do { \
    if (__builtin_constant_p (bl) && ~(UDItype)(bl) <= (UDItype)(bl)) \
      __asm__ ("adds\t%1, %x4, %5\n\tsbc\t%0, %x2, %x3" \
	       : "=r,r" (sh), "=&r,&r" (sl) \
	       : "rZ,rZ" ((UDItype)(ah)), "rZ,rZ" ((UDItype)(bh)), \
		 "r,Z"   ((UDItype)(al)), "rI,r" (-(UDItype)(bl)) : "cc"); \
    else \
      __asm__ ("subs\t%1, %x4, %5\n\tsbc\t%0, %x2, %x3" \
	       : "=r,r" (sh), "=&r,&r" (sl) \
	       : "rZ,rZ" ((UDItype)(ah)), "rZ,rZ" ((UDItype)(bh)), \
		 "r,Z"   ((UDItype)(al)), "rI,r"  ((UDItype)(bl)) : "cc"); \
  } while (0)
void sub(UDItype *r, UDItype ah, UDItype al, UDItype bh, UDItype bl) {
  UDItype h, l; sub_ddmmss(h, l, ah, al, bh, bl); r[0] = h; r[1] = l;
}
void one(UDItype *r, UDItype ah, UDItype al) {
  UDItype h, l; sub_ddmmss(h, l, ah, al, 0, 1); r[0] = h; r[1] = l;
}
"#;

#[test]
fn every_operand_takes_the_same_alternative() {
    let text = listing("gmp", "--target=aarch64-unknown-linux-gnu", GMP);
    let sub = body(&text, "sub");
    let at =
        sub.iter().position(|line| line.starts_with("subs ")).unwrap_or_else(|| panic!("{text}"));
    // Every operand is a register, the first alternative's.
    let operands =
        |line: &str| line.split_once(' ').map(|(_, rest)| rest.to_owned()).unwrap_or_default();
    for line in &sub[at..at + 2] {
        assert!(operands(line).split(", ").all(|op| op.starts_with('x')), "{line}\n{text}");
    }
    assert!(sub[at + 1].starts_with("sbc "), "{text}");
    // A constant under `"rI"` goes in the instruction, and a zero under `"rZ"` is the zero register.
    let one = body(&text, "one");
    let subs =
        one.iter().find(|line| line.starts_with("subs ")).unwrap_or_else(|| panic!("{text}"));
    assert!(subs.ends_with(", 1"), "{subs}\n{text}");
    assert!(one.iter().any(|line| line.starts_with("sbc ") && line.ends_with(", xzr")), "{text}");
}

#[test]
fn an_alternative_with_a_register_is_taken_on_x86_and_riscv() {
    let source = "unsigned long add(unsigned long a, unsigned long b) {
  unsigned long r; __asm__(\"add %0, %1, %2\" : \"=r,r\"(r) : \"r,r\"(a), \"i,r\"(b)); return r;
}
";
    let text = listing("x86", "--target=x86_64-unknown-linux-gnu", source);
    assert!(body(&text, "add").iter().any(|line| line.starts_with("add %r")), "{text}");
    let text = listing("rv", "--target=riscv64-unknown-linux-gnu", source);
    assert!(body(&text, "add").iter().any(|line| line.starts_with("add a")), "{text}");
}

#[test]
fn a_register_with_no_modifier_is_x_whatever_its_width() {
    let text = listing(
        "mrs",
        "--target=aarch64-unknown-linux-gnu",
        "unsigned int ctr(void) { unsigned int v; __asm__ volatile(\"mrs %0, ctr_el0\" : \"=r\"(v)); return v; }
unsigned char byte(unsigned char c) { unsigned char r; __asm__(\"add %0, %1, 1\" : \"=r\"(r) : \"r\"(c)); return r; }
int word(int c) { int r; __asm__(\"add %w0, %w1, 1\" : \"=r\"(r) : \"r\"(c)); return r; }
",
    );
    assert!(body(&text, "ctr").iter().any(|line| line == "mrs x0, ctr_el0"), "{text}");
    assert!(body(&text, "byte").iter().any(|line| line == "add x0, x0, 1"), "{text}");
    assert!(body(&text, "word").iter().any(|line| line == "add w0, w0, 1"), "{text}");
}
