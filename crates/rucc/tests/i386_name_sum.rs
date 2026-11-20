//! On i386 outside position independent code, `types[i].f` reads `types+8(%eax)` once the index
//! is worked out, the way gcc writes it. The address of `types` used to stay a `leal` in front of
//! the load, `leal types, %ecx` and `movl 8(%ecx,%eax,1), %ecx`, because the sum of it and the
//! index was folded into the load first. An index at a scale of one with no base beside it is
//! written as the base, which needs no index byte.

use std::process::Command;

const SOURCE: &str = "\
struct t { int a, b; int (*f)(int); int (*g)(int); char pad[12]; };
extern struct t types[];
int one(int i, int x) { return types[i].f(x); }
int two(int i, int x) { if (x) return types[i].f(x); return types[i].g(x + 1); }
";

fn listing(level: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-name-sum-{}-{level}", std::process::id()));
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
fn the_name_goes_into_the_load_beside_the_index() {
    for level in ["-O2", "-Os"] {
        let text = listing(level);
        assert!(!text.contains("leal\ttypes"), "{level}:\n{text}");
        assert!(text.contains("types+8(%e"), "{level}:\n{text}");
        assert!(text.contains("types+12(%e"), "{level}:\n{text}");
        assert!(!text.contains(",1)"), "{level}:\n{text}");
    }
}
