//! A function on i386 that fits in neither of the two narrower register sets is given `ebx` and
//! `esi` to read its spilled values back into, rather than `esi` and `edi`. Neither `esi` nor `edi`
//! has a low byte, so a comparison of `unsigned long long` fields, whose results are bytes put
//! together with `andb` and `orb`, read each spilled byte into `esi` with an exchange either side.
//! drivers/md/md.c had 145 of those exchanges before this and the barred register work.

use std::process::Command;

const SOURCE: &str = "\
struct m { char pad[300]; unsigned long long a, b; char p2[20]; unsigned long long c; };
void f(void);
int g(struct m *m, unsigned long long x, unsigned long long y)
{
    int r = (m->a < m->c) | (m->b < x && m->c == y);
    f();
    return r;
}
";

fn listing(level: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-last-scratch-{}-{level}", std::process::id()));
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
fn spilled_bytes_are_read_back_without_an_exchange() {
    for level in ["-O2", "-Os"] {
        let text = listing(level);
        assert!(!text.contains("xchg"), "{level}:\n{text}");
    }
}
