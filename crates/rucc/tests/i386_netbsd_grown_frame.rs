//! A frame that grows with `alloca` and spills a `__float128` on i386 NetBSD.
//!
//! NetBSD keeps the i386 stack pointer on four bytes, and a `__float128` lives in a vector
//! register that is spilled with an aligned move into a slot of sixteen. A frame that both grows
//! and forces an alignment needs a second base register, which nothing here keeps, so the compile
//! stops with a diagnostic. It stopped with a panic before (libquadmath's `printf_fp.c`,
//! tamnd/rucc-bsd#3). The same function on i386 Linux, which keeps sixteen bytes, compiles.

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
fn a_spilled_quad_in_a_growing_netbsd_i386_frame_is_a_diagnostic_and_not_a_panic() {
    let out = compile("i486--netbsdelf");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(stderr.contains("[E0653]"), "{stderr}");
    assert!(stderr.contains("spills a value to a slot wanting more alignment"), "{stderr}");
}

#[test]
fn the_same_frame_on_i386_linux_compiles() {
    let out = compile("i686-unknown-linux-gnu");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
