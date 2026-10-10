//! The stack protector's check on i386 compares the canary with the guard where the guard lives,
//! `cmpl %fs:__ref_stack_chk_guard, %ecx`, and branches on the compare, the way gcc writes it. It
//! used to read the guard into a second register, set a byte from the compare and test the byte,
//! and every return had a check and a call to `__stack_chk_fail` of its own. A function now has
//! one of each, in the one block every return jumps to.

use std::process::Command;

const SOURCE: &str = "\
extern void fill(char *p, int n);
int pick(int a, int b)
{
    char buf[32];
    fill(buf, a);
    if (a > b)
        return buf[a & 31];
    if (a == b)
        return 7;
    return buf[b & 31] + a;
}
";

fn listing(level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-protector-check-{}-{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-pic", "-mregparm=3"])
        .args(["-fstack-protector-all", "-mstack-protector-guard-reg=fs"])
        .args(["-mstack-protector-guard-symbol=__ref_stack_chk_guard"])
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn the_check_compares_with_the_guard_in_place() {
    for level in ["-O2", "-Os"] {
        let text = listing(level);
        assert_eq!(
            text.matches("\tcmpl\t%fs:__ref_stack_chk_guard, %e").count(),
            1,
            "{level}:\n{text}"
        );
        assert_eq!(text.matches("\tret").count(), 1, "{level}:\n{text}");
        assert!(!text.contains("setne"), "{level}:\n{text}");
        assert_eq!(text.matches("call\t__stack_chk_fail").count(), 1, "{level}:\n{text}");
    }
}
