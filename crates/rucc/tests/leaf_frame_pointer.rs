//! `-momit-leaf-frame-pointer` and `-mno-omit-leaf-frame-pointer`, end to end.
//!
//! Ubuntu, Fedora and Arch build each package with `-fno-omit-frame-pointer
//! -mno-omit-leaf-frame-pointer`, so that `perf` can walk each stack. The first flag asks for a frame
//! pointer in each function. The second one says that a leaf function keeps it too. The flag is read
//! in the driver and used in the frame layout, so this test reads the assembly.

use std::path::PathBuf;
use std::process::Command;

/// A leaf, which calls nothing, and a function that calls a function in another file, so that
/// inlining cannot make it a leaf.
const SOURCE: &str = "\
int other(int);
int leaf(int a, int b) { return a * b + 1; }
int caller(int a) { return other(a) + 2; }
";

fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-leaf-fp-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("leaf.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The assembly for the target under those flags.
fn asm(target: &str, what: &str, flags: &[&str]) -> String {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-", "-O2", "-fno-omit-frame-pointer"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The text from the label of the function to the next `ret`.
fn body<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text.find(&format!("\n{name}:")).unwrap_or_else(|| panic!("no {name}:\n{text}"));
    let rest = &text[start..];
    let end = rest.find("ret").unwrap_or_else(|| panic!("no ret in {name}:\n{text}"));
    &rest[..end]
}

#[test]
fn a_leaf_keeps_the_frame_pointer_unless_the_flag_says_not_to() {
    for (target, setup) in
        [("x86_64-unknown-linux-gnu", "%rbp"), ("aarch64-unknown-linux-gnu", "x29")]
    {
        for (what, flags) in [("default", &[][..]), ("keep", &["-mno-omit-leaf-frame-pointer"][..])]
        {
            let text = asm(target, what, flags);
            assert!(body(&text, "leaf").contains(setup), "{target} {what}:\n{text}");
            assert!(body(&text, "caller").contains(setup), "{target} {what}:\n{text}");
        }
        let text = asm(target, "omit", &["-momit-leaf-frame-pointer"]);
        assert!(!body(&text, "leaf").contains(setup), "{target} omit:\n{text}");
        assert!(body(&text, "caller").contains(setup), "{target} omit, not a leaf:\n{text}");
    }
}
