//! Temporary: which fixture shows the latch on main.

use std::process::Command;

const SIMPLE: &str = "\
struct att { int len; short off; char align; char byval; long pad; };
void fill(const struct att *a, char *isnull, long *values, int n, const char *tp) {
    for (int i = 0; i < n; i++, a++) {
        isnull[i] = 0;
        values[i] = (long)(tp + a->off);
    }
}
";

const EARLY: &str = "\
struct att { int off; short len; char byval; char align; long pad; };
int fill(struct att *a, char *isnull, long *values, int i, int n, const char *tp) {
    for (; i < n; i++, a++) {
        isnull[i] = 0;
        values[i] = (long)(tp + a->off);
        if (a->len <= 0)
            return i + 1;
    }
    return n;
}
";

const NULLS: &str = "\
struct att { int off; short len; char byval; char align; long pad; };
int fill(struct att *a, const unsigned char *bp, char *isnull, long *values, int i, int n,
         const char *tp, int hasnulls) {
    for (; i < n; i++, a++) {
        if (hasnulls && !(bp[i >> 3] & (1 << (i & 7)))) {
            values[i] = 0;
            isnull[i] = 1;
            return i + 1;
        }
        isnull[i] = 0;
        values[i] = (long)(tp + a->off);
        if (a->len <= 0)
            return i + 1;
    }
    return n;
}
";

fn assembly(name: &str, source: &str, flags: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-latch-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("latch.c");
    std::fs::write(&path, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(["-S", "-o", "-", "-O2"])
        .args(flags)
        .arg(&path)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn which() {
    let mut said = String::new();
    for (name, source) in [("simple", SIMPLE), ("early", EARLY), ("nulls", NULLS)] {
        for flags in [&[][..], &["-fwrapv"][..]] {
            let asm = assembly(name, source, flags);
            let sets = asm.lines().filter(|l| l.trim_start().starts_with("set")).count();
            said += &format!("LATCH {name} {flags:?} sets={sets}\n{asm}\n");
        }
    }
    panic!("{said}");
}
