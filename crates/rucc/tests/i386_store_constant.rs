//! A constant stored to memory on i386 is carried in the store, `movl $0, 4(%eax)`, the way gcc
//! writes it and the way x86-64 already did. The i386 rules had no such form, so every one was a
//! `movl` or an `xorl` into a register and a store of the register, which in drivers/md/md.c was
//! about five hundred instructions.

use std::process::Command;

const SOURCE: &str = "\
struct s { int a, b; short h; char c; _Bool f; };
void set(struct s *q) { q->a = 5; q->b = 0; q->h = 7; q->c = 9; q->f = 1; }
void first(int *p) { *p = -1; }
";

fn listing(level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-store-constant-{}-{level}", std::process::id()));
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
fn a_constant_is_stored_by_the_instruction_that_carries_it() {
    for level in ["-O2", "-Os"] {
        let text = listing(level);
        for store in [
            "\tmovl\t$5, (%eax)",
            "\tmovl\t$0, 4(%eax)",
            "\tmovw\t$7, 8(%eax)",
            "\tmovb\t$9, 10(%eax)",
            "\tmovb\t$1, 11(%eax)",
            "\tmovl\t$-1, (%eax)",
        ] {
            assert!(text.contains(store), "{level}: no {store}\n{text}");
        }
        assert!(!text.contains("xorl"), "{level}:\n{text}");
    }
}
