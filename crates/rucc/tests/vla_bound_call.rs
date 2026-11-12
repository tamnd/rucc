//! A `static` function that only the bound of a variable length array calls is emitted.
//!
//! The bound is an expression in the type of the declaration and not in the body, and the walk
//! that decides which `static` functions the file uses did not look in types. So the object
//! called the function and did not define it. FreeBSD's `mmcspi.c` declares
//! `uint8_t txbuf[max(MMCSPI_POLL_LEN, 2)]` with `max` a `static inline` from `libkern.h`, and the
//! kernel did not link. These compile a file like that and read the assembly.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the assertions are about the compiler and
/// not about the machine the suite ran on.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// One function for each place a bound can be: an array, a pointer to an array, and a name for
/// an array type. Each calls a `static` function nothing else in the file calls.
const SOURCE: &str = "\
static unsigned first(unsigned a, unsigned b) { return a > b ? a : b; }
static unsigned second(unsigned a) { return a + 1; }
static unsigned third(unsigned a) { return a * 2; }
void take(void *);

void array(void) { unsigned char buf[first(8, 2)]; take(buf); }
void pointer(unsigned n) { int (*rows)[second(n)] = 0; take(rows); }
void named(unsigned n) { typedef char T[third(n)]; T buf; take(buf); }
";

/// The fixture, in a directory of its own so two of these at once do not write one file.
fn fixture(level: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rucc-vla-bound-{}-{}",
        std::process::id(),
        level.trim_start_matches('-')
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("bound.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for the fixture at this level.
fn assembly(level: &str) -> String {
    let path = fixture(level);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["--emit=asm", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture at {level}:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// Whether the assembly has a call to `name`.
fn calls(asm: &str, name: &str) -> bool {
    asm.lines().any(|line| line.split_whitespace().eq(["call", name]))
}

#[test]
fn a_function_only_a_bound_calls_is_defined_at_o0() {
    // Nothing is inlined at `-O0`, so each call is in the output and so is each definition.
    let asm = assembly("-O0");
    for name in ["first", "second", "third"] {
        assert!(calls(&asm, name), "no call to {name}:\n{asm}");
        assert!(asm.contains(&format!("\n{name}:")), "{name} is called and not defined:\n{asm}");
    }
}

#[test]
fn a_function_a_bound_still_calls_is_defined_at_every_level() {
    // The optimizer may inline a call or fold it away. A call that is left is the bug.
    for level in ["-O1", "-O2", "-O3", "-Os"] {
        let asm = assembly(level);
        for name in ["first", "second", "third"] {
            if calls(&asm, name) {
                assert!(
                    asm.contains(&format!("\n{name}:")),
                    "{name} is called and not defined at {level}:\n{asm}"
                );
            }
        }
    }
}
