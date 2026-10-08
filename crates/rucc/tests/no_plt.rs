//! `-fno-plt`, end to end.
//!
//! Arch and Fedora build each package with `-fno-plt`. A call to a function in another object then
//! reads the address from the GOT and calls through it, and the PLT stub is not used. A function
//! that cannot be in another object, such as a hidden one, is still called directly. The flag is
//! read in the driver and used in the lowering, so this test reads the assembly.

use std::path::PathBuf;
use std::process::Command;

/// A call to a function in another object, a tail call to it, and a call to a hidden function.
const SOURCE: &str = "\
int other(int);
__attribute__((visibility(\"hidden\"))) int near(int);
int calls(int a) { return other(a) + near(a) + 1; }
int tail(int a) { return other(a + 1); }
";

fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-no-plt-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("calls.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The assembly for the target under those flags.
fn asm(target: &str, what: &str, flags: &[&str]) -> String {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The lines that call or jump to the symbol.
fn uses<'a>(text: &'a str, symbol: &str) -> Vec<&'a str> {
    text.lines()
        .filter(|line| line.contains(symbol) && !line.trim_start().starts_with('.'))
        .collect()
}

#[test]
fn x86_64_calls_through_the_got() {
    for level in ["-O0", "-O2"] {
        let text = asm("x86_64-unknown-linux-gnu", level, &[level, "-fno-plt"]);
        let other = uses(&text, "other");
        assert!(!other.is_empty(), "{level}:\n{text}");
        assert!(other.iter().all(|line| line.contains("other@GOTPCREL(%rip)")), "{level}:\n{text}");
        assert!(!text.contains("other@PLT"), "{level}:\n{text}");
        let near = uses(&text, "near");
        assert!(near.iter().all(|line| !line.contains("GOTPCREL")), "{level}:\n{text}");
    }
    let text = asm("x86_64-unknown-linux-gnu", "x86-64-pic", &["-O2", "-fPIC", "-fno-plt"]);
    assert!(uses(&text, "other").iter().all(|line| line.contains("GOTPCREL")), "pic:\n{text}");
}

#[test]
fn x86_64_calls_through_the_plt_by_default() {
    for flags in [&["-O2"][..], &["-O2", "-fno-plt", "-fplt"][..]] {
        let text = asm("x86_64-unknown-linux-gnu", "default", flags);
        assert!(!text.contains("GOTPCREL"), "{flags:?}:\n{text}");
    }
}

#[test]
fn aarch64_calls_through_the_got_in_position_independent_code() {
    let text = asm("aarch64-unknown-linux-gnu", "aarch64-pic", &["-O2", "-fPIC", "-fno-plt"]);
    assert!(text.contains(":got:other"), "pic:\n{text}");
    assert!(text.contains(":got_lo12:other"), "pic:\n{text}");
    assert!(text.contains("blr"), "pic:\n{text}");
    assert!(!text.contains(":got:near"), "pic:\n{text}");
    let direct = |line: &str| line.split_whitespace().eq(["bl", "other"]);
    assert!(!text.lines().any(direct), "pic:\n{text}");

    // gcc keeps the direct call when the code is not position independent, and so does rucc.
    let text = asm("aarch64-unknown-linux-gnu", "static", &["-O2", "-fno-pic", "-fno-plt"]);
    assert!(!text.contains(":got:other"), "static:\n{text}");
}
