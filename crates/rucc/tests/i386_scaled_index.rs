//! A shifted index two blocks read as an address is written again in the second block, so both
//! loads take the index and its scale in the memory operand and no `shl` is left. The optimizer
//! keeps one `i << 2` for `a[i]` and `b[i]`, and selection only folds an instruction into readers
//! in its own block, so the second load used to read the shifted value out of a register.

use std::process::Command;

const SOURCE: &str = "\
int f(void);
int pick(int *a, int *b, int i) { if (a[i]) f(); return b[i]; }
";

#[test]
fn an_index_shifted_once_for_two_blocks_is_scaled_in_both_loads() {
    for target in ["--target=i686-unknown-linux-gnu", "--target=x86_64-unknown-linux-gnu"] {
        let dir = std::env::temp_dir()
            .join(format!("rucc-i386-scaled-index-{}{target}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
        let path = dir.join("one.c");
        std::fs::write(&path, SOURCE).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([target, "-fno-pic", "-O2", "-S", "-o", "-"])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{target}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        assert!(
            !lines.iter().any(|line| line.starts_with("shl")),
            "{target}: a shift is left\n{text}"
        );
        let scaled = lines.iter().filter(|line| line.contains(",4)")).count();
        assert_eq!(scaled, 2, "{target}: a[i] and b[i] each read with a scale of four\n{text}");
    }
}
