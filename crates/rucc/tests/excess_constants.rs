//! A floating constant where `float` and `double` are evaluated in `long double`, which is
//! `-mfpmath=387` on i386 and every 32 bit NetBSD and FreeBSD unit by default.
//!
//! gcc gives a `float` or `double` constant the precision of `long double` there under
//! `-fexcess-precision=standard` and `16`, and the precision of its own type under `fast`, and an
//! operation on two constants is folded in their own type under `fast`. Without the flag an ISO
//! dialect is `standard` and a GNU dialect is `fast`. Each value below is what
//! `i686-linux-gnu-gcc -mfpmath=387` 13 gives.

use std::process::Command;

const SOURCE: &str = "\
int eq = 0.1f == 0.1;
int big = 1e308 * 10 / 10 == 1e308;
double h = 0.1 * 3;
int same(double x) { return x == 0.1; }
";

/// The units that evaluate `float` and `double` in `long double`.
const EXCESS: &[&[&str]] = &[
    &["--target=i686-unknown-linux-gnu", "-mfpmath=387"],
    &["--target=x86_64-unknown-netbsd", "-m32"],
];

fn ir(flags: &[&str], args: &[&str], name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-excess-const-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .args(args)
        .args(["-O0", "--emit=ir", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{flags:?} {args:?}: {stderr}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The initializer of each global, in the order of the source.
fn globals(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("global @"))
        .filter_map(|line| line.split(" = ").nth(1)?.split(',').next().map(str::to_owned))
        .collect()
}

/// What `eq`, `big` and `h` hold, and whether `same` compares in `long double`.
fn answers(text: &str) -> (Vec<String>, bool) {
    (globals(text), text.contains("fconst.f80 0x3ffbcccccccccccccccd"))
}

fn standard() -> (Vec<String>, bool) {
    (vec!["1".to_owned(), "1".to_owned(), "0x3fd3333333333333".to_owned()], true)
}

fn fast() -> (Vec<String>, bool) {
    (vec!["0".to_owned(), "0".to_owned(), "0x3fd3333333333334".to_owned()], false)
}

#[test]
fn a_constant_has_the_precision_of_long_double_in_an_iso_dialect_and_its_own_in_a_gnu_one() {
    for (index, flags) in EXCESS.iter().enumerate() {
        let iso = ir(flags, &["-std=c11"], &format!("iso{index}"));
        assert_eq!(answers(&iso), standard(), "{flags:?}\n{iso}");
        let gnu = ir(flags, &["-std=gnu17"], &format!("gnu{index}"));
        assert_eq!(answers(&gnu), fast(), "{flags:?}\n{gnu}");
    }
}

#[test]
fn the_flag_decides_over_the_dialect() {
    for (index, flags) in EXCESS.iter().enumerate() {
        for (how, std, want) in [
            ("standard", "-std=gnu17", standard()),
            ("16", "-std=gnu17", standard()),
            ("fast", "-std=c11", fast()),
        ] {
            let flag = format!("-fexcess-precision={how}");
            let text = ir(flags, &[std, &flag], &format!("{how}{index}"));
            assert_eq!(answers(&text), want, "{flags:?} {flag} {std}\n{text}");
        }
    }
}

#[test]
fn a_unit_with_its_arithmetic_in_sse_registers_is_not_changed_by_any_of_it() {
    for args in [&["-std=c11"][..], &["-std=c11", "-fexcess-precision=standard"], &["-std=gnu17"]] {
        let text = ir(&["--target=i686-unknown-linux-gnu"], args, "sse");
        assert_eq!(answers(&text), fast(), "{args:?}\n{text}");
    }
}
