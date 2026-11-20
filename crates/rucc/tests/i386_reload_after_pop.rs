//! An i386 call whose callee takes bytes off the stack is followed by a `subl` that puts them
//! back, and a value read back from a spill slot behind the call has to come after that. It came
//! before it, from a slot four bytes off, and in position independent code that value is the table
//! base in `%ebx`, so the next call through the PLT crashed. A function returning a structure on
//! i386 Linux pops the pointer its result goes through, which is the case here. tamnd/rucc#3027.

use std::process::Command;

const SOURCE: &str = "\
int printf(const char *, ...);
struct Q { int a[4]; };
struct Q q(int);
int main(void) { struct Q a = q(1); struct Q b = q(10); printf(\"%d %d\\n\", a.a[3], b.a[2]); return 0; }
";

#[test]
fn the_stack_goes_back_before_anything_is_read_back() {
    for level in ["-O0", "-O1", "-O2"] {
        let dir = std::env::temp_dir()
            .join(format!("rucc-i386-reload-after-pop-{}{level}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
        let path = dir.join("one.c");
        std::fs::write(&path, SOURCE).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=i686-unknown-linux-gnu", "-fPIE", "-S", "-o", "-", level])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> =
            text.lines().map(str::trim).filter(|line| !line.starts_with('.')).collect();
        let calls: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| **line == "call\tq@PLT")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(calls.len(), 2, "{level}: two calls to q\n{text}");
        for at in calls {
            assert_eq!(
                lines[at + 1],
                "subl\t$4, %esp",
                "{level}: the stack goes back first\n{text}"
            );
        }
    }
}
