//! What the linker is told about a name, end to end: `static` on a function has to reach the
//! object file, and a name two files each define their own of has to stay inside each of them.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3.
//!
//! The unit tests in `rucc-codegen`, `rucc-asm` and `rucc-object` each cover one step of the trip
//! this fact makes: the linkage narrows where a function is lowered, the machine function carries
//! it, and the two output paths read it. What is left is the trip itself, which is only visible
//! from the outside, so this runs the compiler over C and reads the listing it writes.
//!
//! The listing rather than the object, because the listing is what a person debugging this reads
//! and because reading a symbol table would mean a dependency the top crate does not otherwise
//! have. A missing `.globl` in the text is the same fact as a global symbol in the table.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the directives this
/// compares are the same directives on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A file of one name nothing outside it may reach and one name everything may.
const SOURCE: &str = "\
static int helper(int x) {
    return x + 1;
}

int reachable(int x) {
    return helper(x);
}
";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-linkage-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source.
fn asm(what: &str, source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

#[test]
fn a_static_function_is_not_offered_to_the_linker() {
    let text = asm("static", SOURCE);
    // Defined, and at the alignment a function gets, because a local name is one the object file
    // keeps and does not let another file reach rather than one it leaves out.
    assert!(text.contains("\nhelper:\n"), "{text}");
    assert!(text.contains("\t.type\thelper, @function\n"), "{text}");
    assert!(!text.contains("\t.globl\thelper\n"), "static did not reach the object file:\n{text}");
    // The other one, so that the case is about which name rather than about whether any name is
    // announced at all.
    assert!(text.contains("\t.globl\treachable\n"), "{text}");
}

#[test]
fn two_files_may_each_have_their_own_function_of_one_name() {
    // The link this stands for is `rucc a.c b.c`, where both files define `static int helper` and
    // the linker refuses the program if either of them offered the name. Running a linker here
    // would mean a linker on every machine that runs the suite, so what is checked is the thing
    // the linker would have complained about, in both files, at once.
    let other = "\
static int helper(void) {
    return 2;
}

int also_reachable(void) {
    return helper();
}
";
    for (what, source) in [("first", SOURCE), ("second", other)] {
        let text = asm(what, source);
        assert!(!text.contains(".globl\thelper"), "{what} offered helper:\n{text}");
    }
}
