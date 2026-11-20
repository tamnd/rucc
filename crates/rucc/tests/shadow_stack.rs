//! The shadow stack intrinsics of `<cetintrin.h>`, from the command line to the listing.
//!
//! pcre2 turns on `-mshstk` when the build has `-fcf-protection`, and its JIT then calls
//! `_get_ssp`. So the flag has to be taken, `__SHSTK__` has to be defined, and the call has to
//! become the instruction.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because the instructions are
/// x86-64's.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Each intrinsic once, in the way pcre2 reaches them.
const SOURCE: &str = "\
#include <x86intrin.h>
#ifndef __SHSTK__
#error no shadow stack
#endif
int on(void) { return _get_ssp() != 0; }
void step(unsigned int n) { _inc_ssp(n); }
void rest(void *p) {
  _saveprevssp();
  _rstorssp(p);
  _wrssd(1, p);
  _wrssq(2, p);
  _wrussd(3, p);
  _wrussq(4, p);
  _setssbsy();
  _clrssbsy(p);
}
";

/// What the compiler wrote and what it said, for that source under those flags.
fn run(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("rucc-ssp-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// With `-mshstk` each intrinsic is its instruction.
#[test]
fn each_intrinsic_is_its_instruction() {
    let (ok, text, err) = run("all", &["-mshstk"], SOURCE);
    assert!(ok, "{err}");
    for inst in [
        "rdsspq",
        "incsspq",
        "saveprevssp",
        "rstorssp",
        "wrssd",
        "wrssq",
        "wrussd",
        "wrussq",
        "setssbsy",
        "clrssbsy",
    ] {
        assert!(text.contains(inst), "{inst} is missing:\n{text}");
    }
}

/// `_get_ssp` sets the register to zero before `rdsspq`, because the instruction does nothing
/// when the shadow stack is off, and the answer must then be zero.
#[test]
fn the_shadow_stack_pointer_starts_at_zero() {
    let (ok, text, err) = run("zero", &["-mshstk"], SOURCE);
    assert!(ok, "{err}");
    let on = text.split("\non:\n").nth(1).expect("the function is there");
    let read = on.find("rdsspq").expect("the read is there");
    let first = &on[..read];
    assert!(
        first.contains("xor") || first.contains("$0,"),
        "nothing sets the register first:\n{on}"
    );
}

/// Without the flag, `__SHSTK__` is not defined and a call is refused, as gcc refuses it.
#[test]
fn without_the_flag_there_is_no_shadow_stack() {
    let (ok, _, err) = run("none", &[], SOURCE);
    assert!(!ok && err.contains("no shadow stack"), "{err}");
    let (ok, _, err) =
        run("call", &[], "#include <x86intrin.h>\nint on(void) { return _get_ssp() != 0; }\n");
    assert!(!ok, "a call from a function not built for shstk is refused");
    assert!(err.contains("target specific option mismatch"), "{err}");
}
