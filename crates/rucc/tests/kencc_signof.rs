//! kencc's `signof`, end to end.
//!
//! Design: tamnd/rucc#4157, and 06.10 of the rucc-plan9 plan.
//!
//! `signof` gives the type signature that `6c` and `7c` write for `-T`, as an `unsigned long`
//! constant. The values here are the ones that `signat` in `cc/dcl.c` gives for the same types.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-signof-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source and what it said.
fn compile(target: &str, what: &str, flags: &[&str], source: &str) -> (bool, String) {
    let path = fixture(&format!("{target}-{what}"), source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(flags)
        .args(["-fsyntax-only"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

const VALUES: &str = "typedef struct List List;\n\
                      struct List { List *next; int v; };\n\
                      typedef struct S { int a; char *b; } S;\n\
                      extern int print(char*, ...);\n\
                      int x[10];\n\
                      List l;\n\
                      _Static_assert(sizeof(signof(int)) == 4, \"\");\n\
                      _Static_assert(signof(int) == 0x4151d5bd, \"\");\n\
                      _Static_assert(signof(unsigned char) == 0x552c4454, \"\");\n\
                      _Static_assert(signof(long long) == 0x6e5e9590, \"\");\n\
                      _Static_assert(signof(double) == 0x7c40a1b2, \"\");\n\
                      _Static_assert(signof(void) == 0x44112eff, \"\");\n\
                      _Static_assert(signof(char*) == 0xf2611a7a, \"\");\n\
                      _Static_assert(signof(void(*)(int)) == 0xc56357ae, \"\");\n\
                      _Static_assert(signof(S) == 0xe7170706, \"\");\n\
                      _Static_assert(signof(List) == 0xba1b092c, \"\");\n\
                      _Static_assert(signof l == 0xba1b092c, \"\");\n\
                      _Static_assert(signof x == 0xb3a1d3d5, \"\");\n\
                      _Static_assert(signof print == 0x00918875, \"\");\n\
                      _Static_assert(signof(x) == signof(int[10]), \"\");\n";

#[test]
fn the_values_are_the_ones_kencc_gives() {
    for target in ["x86_64-plan9", "aarch64-plan9"] {
        let (ok, err) = compile(target, "values", &["-fkencc"], VALUES);
        assert!(ok, "{target}: {err}");
    }
}

#[test]
fn a_bitfield_and_an_undefined_type_are_refused() {
    let source = "struct B { int f : 3; } b;\nstruct U;\nextern struct U u;\n\
                  unsigned long one = signof b.f;\nunsigned long two = signof(struct U);\n";
    let (ok, err) = compile("x86_64-plan9", "refused", &["-fkencc"], source);
    assert!(!ok, "signof of a bitfield was taken");
    assert!(err.contains("signof bitfield"), "{err}");
    assert!(err.contains("signof undefined type"), "{err}");
}

#[test]
fn without_kencc_signof_is_a_name() {
    let source = "int signof(int);\nint f(void) { return signof(3); }\n";
    let (ok, err) = compile("x86_64-plan9", "name", &[], source);
    assert!(ok, "{err}");
}
