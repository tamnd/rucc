//! A 64-bit value compared with all ones on i386 is the `and` of its two halves compared with all
//! ones, an `andl` and a `cmpl $-1`. The pair used to be split into an `xor` of each half with all
//! ones, which came out as a `notl` apiece and an `orl`. The kernel writes `x == ~0ULL` for unset
//! sector numbers and masks all over drivers/md.

use std::process::Command;

const SOURCE: &str = "\
struct rdev { unsigned long long sector; };
extern void hit(void);
void g(struct rdev *r) { if (r->sector == ~0ULL) hit(); }
int h(unsigned long long x) { return x != ~0ULL; }
";

fn listing(level: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-all-ones-{}-{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-mregparm=3", level, "-fno-pic"])
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn both_halves_are_anded_and_compared_with_all_ones() {
    for level in ["-O2", "-Os"] {
        let text = listing(level);
        assert!(text.contains("\tandl\t"), "{level}:\n{text}");
        assert!(text.matches("\tcmpl\t$-1, ").count() >= 2, "{level}:\n{text}");
        assert!(!text.contains("\tnotl\t"), "{level}:\n{text}");
    }
}
