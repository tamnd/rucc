//! At `-Os` and `-Oz` what both arms of a branch compute first is computed once above it. FreeBSD's
//! `cgstart` divides by `fs_ipg` and multiplies by `fs_fpg` in both arms of the UFS1 and UFS2
//! conditional, and `boot2`, which reads it in `fsread`, has to fit in 7680 bytes.

use std::process::Command;

const SOURCE: &str = "\
struct fs { int pad[6]; int off; int mask; int more[38]; unsigned ipg; int fpg; int magic; } fs;
long long group(unsigned c) {
  return fs.magic == 5 ? (long long)fs.fpg * (c / fs.ipg)
                       : (long long)fs.fpg * (c / fs.ipg) + (long long)fs.off * ((c / fs.ipg) & ~fs.mask);
}
";

fn assembly(name: &str, target: &str, level: &str, extra: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-arm-hoist-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-fno-pic", level, "-S", "-o", "-"])
        .args(extra)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// How many instructions of the function divide.
fn divisions(text: &str) -> usize {
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with("div") || line.starts_with("call") && line.contains("udiv"))
        .count()
}

#[test]
fn the_division_both_arms_make_is_made_once_for_size() {
    for target in ["i686-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        for level in ["-Os", "-Oz"] {
            let name = format!("{level}-{target}");
            let text = assembly(&name, target, level, &[]);
            let off = assembly(&format!("{name}-off"), target, level, &["-fno-arm-hoist"]);
            assert_eq!(divisions(&text), 1, "{name}: one division\n{text}");
            assert_eq!(divisions(&off), 2, "{name}: two without the pass\n{off}");
        }
    }
}
