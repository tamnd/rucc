//! On i386 the `va_list` is a `char *`, and the front end writes the walk of `va_arg` as a load,
//! a step and a store of the list. The list is then not an object whose address an instruction
//! takes, so the optimizer keeps it in a register. Before this, each `va_arg` in a loop loaded the
//! list from its stack slot and stored it back. The NetBSD libsa `kdoprnt` did this in each case
//! of its switch.

use std::process::Command;

const SOURCE: &str = "\
typedef __builtin_va_list va_list;
int sum(int n, ...)
{
    va_list ap;
    int t = 0;
    __builtin_va_start(ap, n);
    while (n-- > 0)
        t += __builtin_va_arg(ap, int);
    __builtin_va_end(ap);
    return t;
}
";

#[test]
fn the_list_stays_in_a_register() {
    let dir = std::env::temp_dir().join(format!("rucc-i386-list-register-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    for level in ["-Os", "-O2"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=i686-unknown-linux-gnu", level, "-fno-pic", "-ffreestanding"])
            .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).expect("a listing is text");
        // `va_start` writes the slot once. No `va_arg` writes it again.
        let stores = text
            .lines()
            .filter(|line| line.trim_start().starts_with("movl") && line.ends_with("(%esp)"))
            .count();
        assert!(stores <= 1, "{level}:\n{text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
