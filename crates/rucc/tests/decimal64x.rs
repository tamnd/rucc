//! `_Decimal64x`, which gcc has from gcc 15 on every target with the decimal types. It is the
//! decimal128 format, sixteen bytes on a sixteen byte boundary, and still a type of its own:
//! `_Generic` tells it from `_Decimal128`, and in an operation on both the interchange type
//! `_Decimal128` wins. A constant of the type has the suffix `d64x` or `D64x`, with the `x` in
//! lower case only.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

fn run(flags: &[&str], source: &str) -> Output {
    // The tests in this file run on threads of one process, so each takes its own directory.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-dec64x-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn stdout(flags: &[&str], source: &str) -> String {
    let out = run(flags, source);
    assert!(out.status.success(), "{flags:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("the output is text")
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn the_type_is_the_decimal128_format_and_a_type_of_its_own() {
    let source = "\
_Static_assert(sizeof(_Decimal64x) == 16 && _Alignof(_Decimal64x) == 16, \"layout\");
_Static_assert(_Generic((_Decimal64x)0, _Decimal64x: 1, _Decimal128: 2, default: 3) == 1, \"own\");
_Static_assert(_Generic(1.5d64x, _Decimal64x: 1, default: 0), \"d64x\");
_Static_assert(_Generic(1.5D64x, _Decimal64x: 1, default: 0), \"D64x\");
_Static_assert(_Generic(1.0DL + 1.0d64x, _Decimal128: 1, default: 0), \"interchange wins\");
_Static_assert(_Generic(1.0DD + 1.0d64x, _Decimal64x: 1, default: 0), \"the wider type wins\");
_Static_assert(_Generic(1.0DF * 1.0d64x, _Decimal64x: 1, default: 0), \"the wider type wins\");
_Static_assert(_Generic(1 + 1.0d64x, _Decimal64x: 1, default: 0), \"an integer converts\");
_Decimal64x x = 9.999999999999999999999999999999999E6144D64x;
";
    let out = run(&[TARGET, "-std=gnu23", "-fsyntax-only"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // Two types, so one name cannot be declared as both.
    let out = run(&[TARGET, "-fsyntax-only"], "_Decimal64x x;\n_Decimal128 x;\n");
    assert!(!out.status.success(), "_Decimal64x and _Decimal128 were one type");
    // gcc reads only a lower case `x`.
    let out = run(&[TARGET, "-fsyntax-only"], "_Decimal64x x = 1.5d64X;\n");
    assert!(!out.status.success(), "d64X was read as a suffix");
    // Where the target has no decimal types there is no `_Decimal64x` either.
    let out = run(&["--target=aarch64-apple-darwin", "-fsyntax-only"], "_Decimal64x x;\n");
    assert!(!out.status.success(), "aarch64-apple-darwin took _Decimal64x");
}

#[test]
fn gcc_15_says_the_limits_of_the_type() {
    let text = stdout(&[TARGET, "-fgnuc-version=15", "-dM", "-E"], "");
    let mut extended: Vec<&str> = text.lines().filter(|line| line.contains("__DEC64X_")).collect();
    extended.sort_unstable();
    assert_eq!(
        extended,
        [
            "#define __DEC64X_EPSILON__ 1E-33D64x",
            "#define __DEC64X_MANT_DIG__ 34",
            "#define __DEC64X_MAX_EXP__ 6145",
            "#define __DEC64X_MAX__ 9.999999999999999999999999999999999E6144D64x",
            "#define __DEC64X_MIN_EXP__ (-6142)",
            "#define __DEC64X_MIN__ 1E-6143D64x",
            "#define __DEC64X_SUBNORMAL_MIN__ 0.000000000000000000000000000000001E-6143D64x",
        ]
    );
    // gcc 14 has no `_Decimal64x`, and so none of its macros.
    let text = stdout(&[TARGET, "-fgnuc-version=14", "-dM", "-E"], "");
    assert!(!text.contains("__DEC64X_"), "gcc 14 has no __DEC64X_ macros");
    // The macros are constants the compiler reads.
    let source = "_Decimal64x lo = __DEC64X_MIN__, hi = __DEC64X_MAX__, e = __DEC64X_EPSILON__;\n";
    let out = run(&[TARGET, "-fgnuc-version=15", "-fsyntax-only"], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn the_arithmetic_is_the_decimal128_routines_of_libgcc() {
    let source = "\
_Decimal64x product(_Decimal64x a, _Decimal64x b) { return a * b; }
_Decimal128 same(_Decimal64x a) { return a; }
_Decimal64x wider(_Decimal64 a) { return a; }
";
    let text = stdout(&[TARGET, "-O2", "-S", "-o", "-"], source);
    let product = body(&text, "product");
    assert!(product.iter().any(|line| line.contains("__bid_multd3")), "{product:#?}");
    // `_Decimal128` is the same format, so the conversion is no call at all.
    let same = body(&text, "same");
    assert!(same.iter().all(|line| !line.starts_with("call")), "{same:#?}");
    let wider = body(&text, "wider");
    assert!(wider.iter().any(|line| line.contains("__bid_extendddtd2")), "{wider:#?}");
}
