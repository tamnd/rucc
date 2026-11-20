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

/// The signature that kencc gives a structure that `#pragma incomplete` marked, which is the hash
/// of `TSTRUCT` alone. `signof(Mouse)` gives it in a program that includes draw.h.
const MARKED_STRUCT: u32 = 0x7c2d_a3bf;

#[test]
fn pragma_incomplete_hides_the_members_from_that_line_on() {
    let source = format!(
        "typedef struct Mouse Mouse;\n\
         struct Mouse {{ int x; char *p; }};\n\
         _Static_assert(signof(Mouse) != {MARKED_STRUCT:#x}, \"\");\n\
         #pragma incomplete Mouse\n\
         _Static_assert(signof(Mouse) == {MARKED_STRUCT:#x}, \"\");\n\
         _Static_assert(signof(struct Mouse) == {MARKED_STRUCT:#x}, \"\");\n\
         #pragma incomplete struct Later\n\
         struct Later {{ double d; }};\n\
         _Static_assert(signof(struct Later) == {MARKED_STRUCT:#x}, \"\");\n\
         union A {{ int i; }};\n\
         union B {{ char *p; double d; }};\n\
         #pragma incomplete union A\n\
         #pragma incomplete union B\n\
         _Static_assert(signof(union A) == signof(union B), \"\");\n\
         _Static_assert(signof(union A) != {MARKED_STRUCT:#x}, \"\");\n\
         Mouse m;\n\
         #pragma incomplete m\n\
         #pragma incomplete _off_\n"
    );
    for target in ["x86_64-plan9", "aarch64-plan9"] {
        let (ok, err) = compile(target, "incomplete", &["-fkencc"], &source);
        assert!(ok, "{target}: {err}");
    }
}

#[test]
fn pragma_incomplete_gives_the_messages_of_kencc() {
    let source = "typedef int I;\n#pragma incomplete I\n#pragma incomplete nosuch\n";
    let (ok, err) = compile("x86_64-plan9", "incomplete-bad", &["-fkencc"], source);
    assert!(!ok, "a bad pragma incomplete was taken");
    assert!(err.contains("not struct/union type in pragma incomplete: I"), "{err}");
    assert!(err.contains("unknown type nosuch in pragma incomplete"), "{err}");
    let (ok, err) =
        compile("x86_64-plan9", "incomplete-tag", &["-fkencc"], "#pragma incomplete struct\n");
    assert!(!ok, "a pragma incomplete with no tag was taken");
    assert!(err.contains("missing struct/union tag in pragma incomplete"), "{err}");
}

#[test]
fn without_kencc_pragma_incomplete_is_ignored() {
    let (ok, err) = compile("x86_64-plan9", "incomplete-gcc", &[], "#pragma incomplete nosuch\n");
    assert!(ok, "{err}");
}

/// The object that `-c` writes for the source with the flags.
fn object(what: &str, flags: &[&str], source: &str) -> Vec<u8> {
    let path = fixture(what, source);
    let out_path = path.with_extension("6");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-plan9", "-fkencc", "-c", "-o"])
        .arg(&out_path)
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let bytes = std::fs::read(&out_path);
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    bytes.expect("a compiler that succeeds writes the object")
}

/// The signature of the `ASIGNAME` record of the name, which is 338 in 6.out.h, then the
/// signature, the type and the slot, then the name.
fn signature(bytes: &[u8], name: &str) -> Option<u32> {
    let mut wanted = name.as_bytes().to_vec();
    wanted.push(0);
    (0..bytes.len().saturating_sub(8 + wanted.len())).find_map(|at| {
        let record = &bytes[at..];
        (record[..2] == [0x52, 0x01] && record[8..].starts_with(&wanted))
            .then(|| u32::from_le_bytes([record[2], record[3], record[4], record[5]]))
    })
}

#[test]
fn t_puts_the_signatures_in_the_object() {
    let source = "typedef struct Mouse Mouse;\nstruct Mouse { int x; };\n#pragma incomplete Mouse\n\
                  Mouse m;\nint x;\nstatic int hidden;\nextern int use(int);\n\
                  int f(void) { return use(x + hidden + m.x); }\n";
    let bytes = object("t", &["-fkencc-signatures"], source);
    assert_eq!(signature(&bytes, "m"), Some(MARKED_STRUCT));
    assert_eq!(signature(&bytes, "x"), Some(0x4151_d5bd));
    assert!(signature(&bytes, "use").is_some());
    assert!(signature(&bytes, "f").is_some());
    assert_eq!(signature(&bytes, "hidden"), None, "a static name has a signature");
    // A name declared only in a block has no type at the end of the file in kencc.
    let block = "int f(void) { extern int runerrs; extern void main(int, char**); main(0, 0); \
                 return runerrs; }\n";
    let bytes = object("t-block", &["-fkencc-signatures"], block);
    assert_eq!(signature(&bytes, "runerrs"), Some(0));
    assert_eq!(signature(&bytes, "main"), Some(0));
    let bytes = object("no-t", &[], source);
    assert_eq!(signature(&bytes, "x"), None, "an object with no -T has a signature");
}

#[test]
fn a_definition_with_no_parameters_has_the_signature_of_the_prototype() {
    // games/mandel.c defines `void threadmain()` where thread.h declares the prototype. kencc's
    // `tmerge` gives the definition the parameters of the prototype, and 6l refuses the link
    // with libthread when the two signatures are not the same.
    let empty = "void threadmain(int argc, char *argv[]);\nvoid threadmain() { }\n";
    let full = "void threadmain(int argc, char *argv[]) { }\n";
    let a = signature(&object("tmerge-empty", &["-fkencc-signatures"], empty), "threadmain");
    let b = signature(&object("tmerge-full", &["-fkencc-signatures"], full), "threadmain");
    assert!(a.is_some());
    assert_eq!(a, b);
}
