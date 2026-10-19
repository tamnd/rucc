//! An i386 `u64` equality test against a value in memory reads both halves of that value as the
//! memory operands of the two `xorl`s. Folding a load into its arithmetic only took the last access
//! in front of the arithmetic, which was the high half, and the low half went into a register of
//! its own at the point the comparison needs the most of them. The fold walks the function again
//! now while the walk before folded something, and the second walk takes the low half.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned long long u64;
int ne(u64 *a, u64 *b, int i) { return *a != b[i]; }
int eq(u64 *a, u64 *b, int i) { return *a == b[i]; }
";

#[test]
fn both_halves_of_a_compared_value_are_read_by_the_xors() {
    for regparm in ["-mregparm=0", "-mregparm=3"] {
        let dir = std::env::temp_dir()
            .join(format!("rucc-i386-compare-halves-{}{regparm}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
        let path = dir.join("one.c");
        std::fs::write(&path, SOURCE).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=i686-unknown-linux-gnu", "-fno-pic", "-O2", "-S", "-o", "-", regparm])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{regparm}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let indexed = |line: &&str| line.contains(",8)");
        let reads: Vec<&str> = text.lines().map(str::trim).filter(indexed).collect();
        assert_eq!(reads.len(), 4, "{regparm}: two reads of b[i] in each function\n{text}");
        for read in reads {
            assert!(read.starts_with("xorl"), "{regparm}: b[i] read outside a xor\n{text}");
        }
    }
}
