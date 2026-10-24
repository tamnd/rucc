//! A constant argument of a narrow type that SysV x86-64 extends before the call.
//!
//! The callee reads the argument extended to 32 bits, so the caller writes the extended value. For
//! a constant gcc writes one `movl` of that value, and rucc wrote a `movb` of the narrow value and
//! then a `movsbl` or a `movzbl` of the register.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the listing is the same everywhere.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, under a directory of its own so two of these running at once do not collide.
fn fixture(source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-narrow-constant-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source at `-O2`.
fn asm(source: &str) -> String {
    let path = fixture(source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// Each argument is one `movl` of the value extended the way its type asks.
#[test]
fn a_narrow_constant_argument_is_one_move_of_the_extended_value() {
    let source = "void f(signed char, short, unsigned char, _Bool, unsigned short);\n\
        void g(void) { f(-3, -300, 200, 1, 65000); }\n";
    let listing = asm(source);
    for line in [
        "movl\t$-3, %edi",
        "movl\t$-300, %esi",
        "movl\t$200, %edx",
        "movl\t$1, %ecx",
        "movl\t$65000, %r8d",
    ] {
        assert!(listing.contains(line), "no `{line}`:\n{listing}");
    }
    for extension in ["movsbl", "movswl", "movzbl", "movzwl"] {
        assert!(!listing.contains(extension), "a `{extension}` is left:\n{listing}");
    }
}
