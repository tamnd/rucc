//! A frame that grows with `alloca` and spills a `__float128` on i386 NetBSD.
//!
//! NetBSD keeps the i386 stack pointer on four bytes, and a `__float128` lives in a vector
//! register. A frame that both grows and forces an alignment needs a second base register, which
//! nothing here keeps, so the spill slot stays on four bytes and the spill and reload are `movups`,
//! which takes any address. It stopped with a panic first and with E0653 after that (libquadmath's
//! `printf_fp.c`, tamnd/rucc#4449). The same function on i386 Linux, which keeps sixteen bytes,
//! keeps the aligned move.

use std::process::Command;

const SOURCE: &str = "\
extern void use(void *, int);
extern __float128 get(void);
extern void put(__float128);
int f(int n)
{
  __float128 v = get();
  char *p = __builtin_alloca(n);
  use(p, n);
  put(v);
  return p[0];
}
";

fn compile(target: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("rucc-grown-{}-{target}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("grown.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_spilled_quad_in_a_growing_netbsd_i386_frame_is_moved_unaligned() {
    let out = compile("i486--netbsdelf");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let asm = String::from_utf8_lossy(&out.stdout);
    assert!(asm.contains("movups"), "{asm}");
    // No aligned vector move reaches a slot, which a frame that grows counts from `%ebp`, and
    // nothing forces the stack pointer onto sixteen.
    assert!(!asm.lines().any(|line| line.contains("movaps") && line.contains("(%ebp)")), "{asm}");
    assert!(!asm.contains("$-16"), "{asm}");
}

#[test]
fn the_same_frame_on_i386_linux_compiles() {
    let out = compile("i686-unknown-linux-gnu");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
