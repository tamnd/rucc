//! A division by zero is `-Wdiv-by-zero`, so a pragma or a flag naming that option silences it.
//! The kernel's `mul_u64_u64_div_u64` writes `return 1/0;` on purpose to trap on a zero divisor,
//! between `#pragma GCC diagnostic ignored "-Wdiv-by-zero"` and a pop, and builds with `-Werror`.
//! The warning used to share its code with an implicit declaration and answer to no option, so
//! `-Werror` made it fatal.

use std::process::Command;

const SOURCE: &str = "\
int f(int c) {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored \"-Wdiv-by-zero\"
  if (c == 0) return 1/0;
#pragma GCC diagnostic pop
  return 2/0;
}
";

fn build(flags: &[&str]) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "rucc-div-by-zero-{}-{}",
        std::process::id(),
        flags.len()
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-Werror", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn the_pragma_silences_only_the_division_it_surrounds() {
    let out = build(&[]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("one.c:6:"), "{stderr}");
    assert!(!stderr.contains("one.c:4:"), "{stderr}");
}

#[test]
fn no_div_by_zero_silences_every_one() {
    let out = build(&["-Wno-div-by-zero"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
