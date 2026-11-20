//! The constants that kencc reads in a different way from C, under `-fkencc`, end to end.
//!
//! - An enumerator can have a floating value. It is then a constant of that type, and the next
//!   enumerator with no value is that value plus one (`doenum` in `cc/dcl.c`). libdraw's
//!   mkwarp.c has `enum { ε = 1e-6 }`.
//! - An enumerator has the type of its value, and one with no value the type of the one before
//!   it. libc's date.c has `Daysec = (vlong)24*3600` and then `2440588 * Daysec`, which must
//!   not overflow.
//! - kencc has no hexadecimal floating constant, so a sign after an `e` or a `p` in a
//!   hexadecimal number ends the number. libc/port/pool.c has `0xCAB00D1E+1`.
//! - `'''` is the character constant of a quote, as `escchar` in `cc/lex.c` reads it.
//!   The file dossubs.c of dossrv has `isdos['''] = 1`.

use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "enum { eps = 1e-6, half = 2.5, next, seven = 7, eight };\n\
                      double a = eps;\n\
                      double b = next;\n\
                      int c = eight;\n\
                      unsigned d = 0xCAB00D1E+1;\n\
                      enum { Daysec = (long long)24*3600, Next, Small = (char)3 };\n\
                      long long e = 2440588 * Daysec;\n\
                      int f = sizeof(Next) + sizeof(Small);\n";

/// The quote, which is in its own line because C stops at it before the checks of the rest.
const QUOTE: &str = "int g = ''';\n";

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-kencc-constants-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, the assembly, and what it said.
fn compile(what: &str, source: &str, flags: &[&str]) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = path.with_extension("s");
    let run = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-S", "-o"])
        .arg(&out)
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let asm = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (run.status.success(), asm, String::from_utf8_lossy(&run.stderr).into_owned())
}

/// The value that the assembly gives the object `name`.
fn value<'a>(asm: &'a str, name: &str) -> &'a str {
    let label = format!("{name}:");
    let mut lines = asm.lines().skip_while(|l| *l != label);
    lines.next();
    lines.next().map_or("", str::trim)
}

#[test]
fn kencc_reads_the_constants() {
    let (ok, asm, err) = compile("kencc", &format!("{SOURCE}{QUOTE}"), &["-fkencc", "-w"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    // 1e-6 and 3.5, as the bits of a double.
    assert_eq!(value(&asm, "a"), ".quad\t4517329193108106637");
    assert_eq!(value(&asm, "b"), ".quad\t4615063718147915776");
    assert_eq!(value(&asm, "c"), ".long\t8");
    assert_eq!(value(&asm, "d"), ".long\t3400535327");
    assert_eq!(value(&asm, "e"), ".quad\t210866803200");
    assert_eq!(value(&asm, "f"), ".long\t12");
    assert_eq!(value(&asm, "g"), ".long\t39");
}

#[test]
fn c_refuses_them() {
    let (ok, _, err) = compile("c", SOURCE, &[]);
    assert!(!ok);
    assert!(err.contains("enumerator value for 'eps' is not an integer constant"), "{err}");
    assert!(err.contains("invalid suffix"), "{err}");
    let (ok, _, err) = compile("quote", QUOTE, &[]);
    assert!(!ok);
    assert!(err.contains("empty character constant"), "{err}");
}
