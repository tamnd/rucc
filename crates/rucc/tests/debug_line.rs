//! What `-g` puts in an object, which is a line table and the least that makes it findable.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4. See tamnd/rucc#1558.
//!
//! The claim being made to anybody who passes `-g` is about the object, so this asserts on the
//! bytes of one rather than on a field in the options. What is checked is the shape: the sections
//! that have to be there are there, a build that did not ask gets none of them, and every path
//! inside them has been through the prefix map. Whether the addresses in the table are the right
//! addresses is a different question and a harder one, and it is checked against a debugger rather
//! than here.
//!
//! The names are looked for as bytes because they are section names, and a section name is in the
//! file as itself. That is the same way `unwind.rs` looks for `.eh_frame` and it needs no reader.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A program with two functions in it, so that the table has more than one sequence in it.
const SOURCE: &str = "\
int twice(int n) { return n + n; }
int total(const int *of, int many) {
    int sum = 0;
    for (int i = 0; i < many; i++) sum += twice(of[i]);
    return sum;
}
";

/// The target the object is built for, written down rather than taken from the host, because an
/// object is only produced for the one this compiler has a back end for.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// Every section a reader has to find before it can answer a program counter.
const SECTIONS: [&str; 5] =
    [".debug_line", ".debug_line_str", ".debug_abbrev", ".debug_info", ".debug_rnglists"];

/// A directory of this test's own, so that two of these running at once do not write the same
/// file, with the source already in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-dl-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    dir
}

/// That object, built with those flags.
fn build(dir: &Path, flags: &[&str], object: &str) -> Vec<u8> {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-c"])
        .args(flags)
        .arg("-o")
        .arg(dir.join(object))
        .arg(dir.join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::read(dir.join(object)).expect("the object was written")
}

/// Whether those bytes are somewhere in the file.
fn holds(bytes: &[u8], what: &str) -> bool {
    bytes.windows(what.len()).any(|window| window == what.as_bytes())
}

#[test]
fn asking_for_debug_information_writes_the_sections_a_reader_needs() {
    let dir = fixture("shape");
    let with = build(&dir, &["-g"], "with.o");
    let without = build(&dir, &[], "without.o");
    let _ = std::fs::remove_dir_all(&dir);

    for name in SECTIONS {
        assert!(holds(&with, name), "a build with -g has no {name}");
    }
    // And the other direction, which is the half that says the flag is doing the work rather than
    // something else being on by default. A section nothing asked for is a bigger object for no
    // reason, and `.debug_line` is the largest thing in most of them.
    assert!(!holds(&without, ".debug_"), "a build that did not ask for -g got debug sections");
}

#[test]
fn the_table_names_the_file_the_way_the_prefix_map_says() {
    let dir = fixture("paths");
    let plain = build(&dir, &["-g"], "plain.o");
    let here = dir.to_string_lossy().into_owned();
    let mapped = build(&dir, &["-g", &format!("-ffile-prefix-map={here}=SRC")], "mapped.o");
    let _ = std::fs::remove_dir_all(&dir);

    // Without the flag the table says where the file really was, which is what a debugger on the
    // machine that built it needs.
    // The path is the one the compiler was handed, so it is joined the way this platform joins one.
    let file = dir.join("one.c").to_string_lossy().into_owned();
    assert!(holds(&plain, &file), "the table does not name the file");

    // With it, the rewritten path is in the file and the real one is nowhere in it. Both halves
    // matter: a build that rewrote the unit's name and left a directory behind is still a build
    // whose output depends on where it ran, which is the whole thing the flag exists to stop.
    let file = Path::new("SRC").join("one.c").to_string_lossy().into_owned();
    assert!(holds(&mapped, &file), "the mapping did not reach the table");
    assert!(!holds(&mapped, &here), "the build directory is still in the object");
}

#[test]
fn a_build_with_no_unwind_table_keeps_its_frame_rules_where_a_debugger_looks() {
    let dir = fixture("frames");
    let off = ["-g", "-fno-asynchronous-unwind-tables", "-fno-unwind-tables"];
    let quiet = build(&dir, &off, "quiet.o");
    let loud = build(&dir, &["-g"], "loud.o");
    let _ = std::fs::remove_dir_all(&dir);

    // Without an unwind table the rules go in `.debug_frame`, which is what the frame base of
    // every function is read through. With one they stay where they were and are not written a
    // second time, since a debugger reads either.
    assert!(holds(&quiet, ".debug_frame"), "no table for the frame base to be read through");
    assert!(!holds(&quiet, ".eh_frame"), "an unwind table nobody asked for");
    assert!(holds(&loud, ".eh_frame"), "the unwind table went missing");
    assert!(!holds(&loud, ".debug_frame"), "the frame rules were written twice");
}
