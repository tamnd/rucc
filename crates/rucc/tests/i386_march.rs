//! `-march=` on i386 for the processors before the Pentium Pro, which have no `cmov`. The kernel's
//! `arch/x86/Makefile_32.cpu` passes `-march=i486`, `-march=i586`, `-march=k6` and others like
//! them, and a kernel built that way has to boot on that processor, where a `cmov` is an invalid
//! opcode. gcc writes a branch for every select there and so does this.

use std::process::Command;

const SOURCE: &str = "\
int least(int a, int b) { return a < b ? a : b; }
int size(int x) { return x < 0 ? -x : x; }
unsigned long long moved(unsigned long long x, int k) { return x << k; }
long long most(long long a, long long b) { return a > b ? a : b; }
";

fn listing(march: &str, level: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-march-{}-{march}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-asynchronous-unwind-tables", "-S"])
        .arg(format!("-march={march}"))
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn a_processor_before_the_pentium_pro_gets_no_conditional_move() {
    for march in ["i486", "i586", "pentium-mmx", "k6", "winchip-c6", "c3"] {
        for level in ["-O0", "-O2"] {
            let text = listing(march, level);
            assert!(!text.contains("\tcmov"), "{march} {level}: {text}");
        }
    }
}

#[test]
fn the_pentium_pro_and_after_keep_it() {
    for march in ["i686", "pentium4", "athlon", "geode", "c3-2"] {
        let text = listing(march, "-O2");
        assert!(text.contains("\tcmov"), "{march}: {text}");
    }
}
