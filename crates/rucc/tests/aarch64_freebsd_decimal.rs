//! The decimal floating types on AArch64 FreeBSD, which the gcc packages of FreeBSD have from
//! gcc 14 on. gcc uses the binary integer encoding there, the same as on x86-64, so the
//! operations are the `__bid_*` routines of libgcc. AAPCS64 passes and returns a decimal in a
//! vector register, as it does a binary float of the same width, and a structure of up to four
//! decimals of one type is a homogeneous aggregate.
//!
//! x86-64 and RISC-V FreeBSD and every NetBSD row have no decimal types in gcc, so rucc refuses
//! them there.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const TARGET: &str = "--target=aarch64-unknown-freebsd15.1";

fn run(flags: &[&str], source: &str) -> Output {
    // The tests in this file run on threads of one process, so each takes its own directory.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-fbsd-dec-{}-{n}", std::process::id()));
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

const TYPES: &str = "\
_Static_assert(sizeof(_Decimal32) == 4 && _Alignof(_Decimal32) == 4, \"32\");
_Static_assert(sizeof(_Decimal64) == 8 && _Alignof(_Decimal64) == 8, \"64\");
_Static_assert(sizeof(_Decimal128) == 16 && _Alignof(_Decimal128) == 16, \"128\");
_Decimal64 x = 1.5DD;
";

#[test]
fn freebsd_has_the_decimal_types_on_aarch64_only() {
    let out = run(&[TARGET, "-fsyntax-only"], TYPES);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    for target in [
        "--target=x86_64-unknown-freebsd15.1",
        "--target=riscv64-unknown-freebsd15.1",
        "--target=aarch64--netbsd",
    ] {
        let out = run(&[target, "-fsyntax-only"], TYPES);
        assert!(!out.status.success(), "{target} took the decimal types");
    }
}

#[test]
fn the_macros_are_there_from_gcc_14() {
    let text = stdout(&[TARGET, "-dM", "-E"], "");
    for line in [
        "#define __DECIMAL_BID_FORMAT__ 1",
        "#define __DEC32_MANT_DIG__ 7",
        "#define __DEC64_MAX__ 9.999999999999999E384DD",
        "#define __DEC128_SUBNORMAL_MIN__ 0.000000000000000000000000000000001E-6143DL",
        "#define __DEC_EVAL_METHOD__ 2",
    ] {
        assert!(text.lines().any(|got| got == line), "{line} is missing");
    }
    // gcc 13 has no decimal types on AArch64, and says only how it would evaluate them.
    let text = stdout(&[TARGET, "-fgnuc-version=13", "-dM", "-E"], "");
    let decimal: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("#define __DEC") && !line.contains("DECIMAL_DIG"))
        .collect();
    assert_eq!(decimal, ["#define __DEC_EVAL_METHOD__ 2"]);
}

#[test]
fn a_decimal_travels_in_a_vector_register_and_its_arithmetic_is_a_libgcc_routine() {
    let source = "\
_Decimal64 second(_Decimal64 a, _Decimal64 b) { return b; }
_Decimal32 product(_Decimal32 a, _Decimal32 b) { return a * b; }
_Decimal128 wide(_Decimal128 a, _Decimal128 b) { return b - a; }
struct three { _Decimal64 a, b, c; };
_Decimal64 middle(struct three s) { return s.b; }
int less(_Decimal64 a, _Decimal64 b) { return a < b; }
";
    let text = stdout(&[TARGET, "-O2", "-S", "-o", "-"], source);
    // The second argument is in v1 and the answer goes back in v0, the same as for a `double`.
    let moved = |name: &str| {
        let body = body(&text, name);
        assert!(
            body.iter().any(|line| line == "mov v0.16b, v1.16b" || line == "fmov d0, d1"),
            "{name} does not return what came in v1:\n{body:#?}"
        );
    };
    moved("second");
    // The three members of the structure are a homogeneous aggregate in v0, v1 and v2.
    moved("middle");
    for (name, routine) in
        [("product", "__bid_mulsd3"), ("wide", "__bid_subtd3"), ("less", "__bid_ltdd2")]
    {
        let body = body(&text, name);
        assert!(body.iter().any(|line| line.ends_with(routine)), "{name}:\n{body:#?}");
    }
}
