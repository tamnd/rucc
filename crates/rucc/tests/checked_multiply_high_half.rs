//! `__builtin_mul_overflow` on a `long` takes the high half of the product from the one widening
//! multiply x86 has, on i386 and on x86-64. It used to be split into halves and multiplied out
//! long hand, about 30 instructions where gcc writes two.

use std::process::Command;

const SOURCE: &str = "\
unsigned long f(unsigned long a, unsigned long b) { unsigned long r; if (__builtin_mul_overflow(a, b, &r)) return -1; return r; }
long g(long a, long b) { long r; if (__builtin_mul_overflow(a, b, &r)) return -1; return r; }
";

fn build(target: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-checked-high-{}{target}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-fno-pic", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{target}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_checked_multiply_of_a_long_takes_the_high_half_in_one_instruction() {
    for target in ["i686-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        let text = build(target);
        let code: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
            .collect();
        let multiplies = code.iter().filter(|line| line.contains("mul")).count();
        assert_eq!(multiplies, 4, "{target}: a low and a high multiply in each function\n{text}");
        assert!(
            !code.iter().any(|line| line.contains("shr")),
            "{target}: no halves are taken\n{text}"
        );
    }
}
