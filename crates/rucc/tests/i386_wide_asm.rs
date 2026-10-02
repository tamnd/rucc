//! A `long long` handed to an i386 `asm` under a letter other than `A`, in a template that never
//! names it. The kernel's `__iter_div_u64_rem` writes `asm("" : "+rm"(dividend))` over a `u64` to
//! keep the loop from becoming a division, and `OPTIMIZER_HIDE_VAR` writes `"=r"` and `"0"`. Each
//! is two operands, one for each half, and the function used to be refused with an `i64` nothing
//! lowered.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned long long u64;
unsigned iter(u64 dividend, unsigned divisor, u64 *remainder) {
  unsigned ret = 0;
  while (dividend >= divisor) { asm(\"\" : \"+rm\"(dividend)); dividend -= divisor; ret++; }
  *remainder = dividend;
  return ret;
}
u64 hide(u64 x) { __asm__ (\"\" : \"=r\" (x) : \"0\" (x)); return x + 1; }
u64 seen(u64 x) { asm volatile(\"\" : : \"r\"(x)); asm volatile(\"\" : \"+g\"(x)); return x; }
";

fn build(level: &str, source: &str) -> std::process::Output {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-wide-asm-{}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-fno-pic", level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_wide_operand_the_template_does_not_name_is_two_operands() {
    for level in ["-O0", "-O2"] {
        let out = build(level, SOURCE);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn a_wide_operand_the_template_names_is_still_refused() {
    let source =
        "unsigned long long f(unsigned long long x) { asm(\"incl %0\" : \"+r\"(x)); return x; }\n";
    let out = build("-O2", source);
    assert!(!out.status.success());
}
