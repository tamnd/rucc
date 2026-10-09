//! The linear scan's answer on i386 is not kept when it puts a value in a register the value is
//! barred from. The scan meets such a register with a move either side of every instruction that
//! names the byte, which on i386 is an exchange, and the weighing that picks between its answer and
//! the backtracking one did not count those. `recovery_start_show` in drivers/md/md.c came out with
//! four `xchgl` around the `setne` and `sete` of its condition.

use std::process::Command;

const SOURCE: &str = "\
int sprintf(char *, const char *, ...);
struct rdev { char pad[124]; unsigned long flags; char p2[28]; unsigned long long offset; };
int show(struct rdev *rdev, char *page)
{
    unsigned long long start = rdev->offset;
    if ((rdev->flags & 2) || start == ~0ULL)
        return sprintf(page, \"none\\n\");
    return sprintf(page, \"%llu\\n\", start);
}
";

fn listing(level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-barred-linear-{}-{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-pic", "-mregparm=3"])
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn a_byte_condition_needs_no_exchange() {
    for level in ["-O2", "-Os"] {
        let show = listing(level);
        assert!(!show.contains("xchg"), "{level}:\n{show}");
    }
}
