//! `__FLT_EVAL_METHOD__` 2, which is what a unit that does its float arithmetic on the x87 stack
//! has: `-mfpmath=387` on i386, and every 32 bit unit for NetBSD and FreeBSD by default.
//!
//! A `float` or `double` expression is evaluated in `long double` there, as gcc does it with
//! `-fexcess-precision=standard`, and it is rounded to its own type only where the program
//! assigns it, casts it, passes it or returns it. So `a * b + c` on three `float` values is one
//! multiply and one add in `long double` and one rounding at the end, and the expression still
//! has the type `float` for `sizeof` and `_Generic`. A unit that does its arithmetic in SSE
//! registers computes every operation in the type of its operands, as before.

use std::process::Command;

const SOURCE: &str = "\
float sum(float a, float b, float c) { return a * b + c; }
int less(double a, double b, double c) { return a * b < c; }
double wide(float a, float b) { return a * b; }
int kept(float a, float b) { return sizeof(a * b) + _Generic(a * b, float: 10, default: 20); }
";

/// The units that evaluate in `long double`, as the target and flags that make each one.
const EXCESS: &[&[&str]] = &[
    &["--target=i686-unknown-linux-gnu", "-mfpmath=387"],
    &["--target=x86_64-unknown-netbsd", "-m32"],
    &["--target=x86_64-unknown-freebsd15.1", "-m32", "-march=k8", "-msse2"],
    &["--target=i486--netbsdelf"],
    &["--target=i386-unknown-freebsd14.5"],
];

/// The units that evaluate each operation in its own type.
const PLAIN: &[&[&str]] = &[
    &["--target=i686-unknown-linux-gnu"],
    &["--target=i386-unknown-freebsd14.5", "-msse2", "-mfpmath=sse"],
    &["--target=x86_64-unknown-netbsd"],
];

fn run(flags: &[&str], args: &[&str], name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-excess-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .args(args)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{flags:?}: {stderr}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The instructions of one function, without the name of it or the braces around it.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("func @{name}(");
    text.lines()
        .map(str::trim)
        .skip_while(|line| !line.starts_with(&open))
        .skip(1)
        .take_while(|line| *line != "}")
        .filter(|line| !line.is_empty())
        .collect()
}

fn count(lines: &[&str], what: &str) -> usize {
    lines.iter().filter(|line| line.contains(what)).count()
}

#[test]
fn a_float_expression_on_the_x87_stack_is_evaluated_in_long_double_and_rounded_once() {
    for (index, flags) in EXCESS.iter().enumerate() {
        let ir = run(flags, &["-O0", "--emit=ir", "-o", "-"], &format!("x{index}"), SOURCE);
        let sum = body(&ir, "sum");
        assert_eq!(count(&sum, "fpext.f80"), 3, "{flags:?}\n{ir}");
        assert_eq!(count(&sum, "fptrunc.f32"), 1, "{flags:?}\n{ir}");
        assert_eq!(count(&sum, "fmul") + count(&sum, "fadd"), 2, "{flags:?}\n{ir}");
        // The product is compared before it is rounded to a `double`.
        let less = body(&ir, "less");
        assert_eq!(count(&less, "fpext.f80"), 3, "{flags:?}\n{ir}");
        assert_eq!(count(&less, "fptrunc"), 0, "{flags:?}\n{ir}");
        // The product of two `float` values becomes a `double` from `long double`, and is not
        // rounded to a `float` on the way.
        let wide = body(&ir, "wide");
        assert_eq!(count(&wide, "fptrunc.f64"), 1, "{flags:?}\n{ir}");
        assert_eq!(count(&wide, "fptrunc.f32"), 0, "{flags:?}\n{ir}");
        // The expression is still a `float`: four bytes, and the `float` association.
        let kept = body(&ir, "kept");
        assert!(kept.iter().any(|line| line.ends_with("iconst.i32 4")), "{flags:?}\n{ir}");
        assert!(kept.iter().any(|line| line.ends_with("iconst.i32 10")), "{flags:?}\n{ir}");
    }
}

#[test]
fn a_float_expression_in_sse_registers_is_evaluated_in_its_own_type() {
    for (index, flags) in PLAIN.iter().enumerate() {
        let ir = run(flags, &["-O0", "--emit=ir", "-o", "-"], &format!("p{index}"), SOURCE);
        for name in ["sum", "less", "wide"] {
            assert_eq!(count(&body(&ir, name), ".f80"), 0, "{flags:?}\n{ir}");
        }
    }
}

#[test]
fn the_evaluation_method_is_two_where_the_arithmetic_is_on_the_x87_stack() {
    let macros = |flags: &[&str], name: &str| {
        let text = run(flags, &["-E", "-dM"], name, "");
        let value = |key: &str| {
            let line = text.lines().find(|line| line.starts_with(&format!("#define {key} ")));
            line.map(|line| line.rsplit(' ').next().unwrap_or_default().to_owned())
        };
        (value("__FLT_EVAL_METHOD__"), value("__FLT_EVAL_METHOD_TS_18661_3__"))
    };
    let two = (Some("2".to_owned()), Some("2".to_owned()));
    let zero = (Some("0".to_owned()), Some("0".to_owned()));
    for (index, flags) in EXCESS.iter().enumerate() {
        assert_eq!(macros(flags, &format!("mx{index}")), two, "{flags:?}");
    }
    for (index, flags) in PLAIN.iter().enumerate() {
        assert_eq!(macros(flags, &format!("mp{index}")), zero, "{flags:?}");
    }
}
