//! A `bool` kept across a call in `esi` or `edi`, two registers i386 has no low byte for, is tested
//! with `testl $255` rather than with an exchange on either side of a `testb`. The two leave the
//! same zero flag, and the branch behind is all that reads it. drivers/md/md.c had twenty of the
//! three instruction form.

use std::process::Command;

const SOURCE: &str = "\
extern _Bool f(int);
extern void g(int);
extern void lock(int *);
int k(int *m, int a, int b)
{
    _Bool x = 0;
    if (a > 3)
        x = f(a);
    lock(m);
    g(*m);
    if (x)
        g(a);
    g(b);
    if (x)
        g(*m + a);
    return b;
}
";

#[test]
fn a_bool_in_a_register_with_no_low_byte_is_tested_whole() {
    let dir = std::env::temp_dir().join(format!("rucc-i386-bool-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-mregparm=3", "-O2", "-fno-pic"])
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    assert!(!text.contains("xchgl"), "{text}");
    assert_eq!(text.matches("testl\t$255, %e").count(), 2, "{text}");
}
