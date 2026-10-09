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

/// A function the optimizer copies into its caller. It is static, so no body of its own is left,
/// and it makes a call, which no pass can fold into the code around it. Every row on line 3 is
/// then a row of the copy.
const INLINED: &str = "\
int g(int);
static int twice(int n) {
    return g(n) + 1;
}
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

/// `-gdwarf-4` writes the sections DWARF 4 has in place of the DWARF 5 ones, which is what a
/// kernel built with `CONFIG_DEBUG_INFO_DWARF4` asks for, and naming a version turns debug
/// information on the same way `-g` does. See tamnd/rucc#2287.
#[test]
fn asking_for_dwarf_4_writes_the_sections_dwarf_4_has() {
    let dir = fixture("four");
    let four = build(&dir, &["-gdwarf-4"], "four.o");
    let back = build(&dir, &["-gdwarf-4", "-gdwarf-5"], "back.o");
    let _ = std::fs::remove_dir_all(&dir);

    for name in [".debug_line", ".debug_abbrev", ".debug_info", ".debug_ranges", ".debug_str"] {
        assert!(holds(&four, name), "a build with -gdwarf-4 has no {name}");
    }
    for name in [".debug_line_str", ".debug_rnglists", ".debug_loclists"] {
        assert!(!holds(&four, name), "a DWARF 4 build has {name}, which is a DWARF 5 section");
    }
    // The last version named is the one written, as with gcc.
    for name in SECTIONS {
        assert!(holds(&back, name), "a build that asked for 5 last has no {name}");
    }
}

/// A version this compiler does not write is refused rather than quietly written as another one.
#[test]
fn a_dwarf_version_this_does_not_write_is_refused() {
    let dir = fixture("three");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-c", "-gdwarf-3", "-o"])
        .arg(dir.join("three.o"))
        .arg(dir.join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("DWARF 4 and 5"));
}

/// At `-O0` each function builds a frame, and the line table marks the end of its prologue where
/// the body starts. GDB 13 and later put a breakpoint on the function there, which is past the
/// moves out of the argument registers. The mark is never on the first row of a function, since
/// that row covers the prologue.
#[cfg(target_os = "linux")]
#[test]
fn the_end_of_each_prologue_is_marked_where_the_body_starts() {
    let dir = fixture("prologue");
    build(&dir, &["-g", "-O0"], "one.o");
    let out = Command::new("readelf")
        .arg("--debug-dump=rawline")
        .arg(dir.join("one.o"))
        .output()
        .expect("readelf starts");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);

    // A sequence starts at its function, and a row is written by a special opcode or a copy.
    let mut marks = 0;
    let mut first = false;
    for line in text.lines() {
        if line.contains("set Address to") {
            first = true;
        } else if line.contains("prologue_end") {
            assert!(!first, "the first row of a function is marked:\n{text}");
            marks += 1;
        } else if line.contains("Special opcode") || line.contains("Copy") {
            first = false;
        }
    }
    assert_eq!(marks, 2, "{text}");
}

/// A body the optimizer inlined still says the lines it came from. Each copy has positions of its
/// own, past the last file, and the table is written from what the source map says about them, so
/// a copy it could not place would leave its rows out.
#[cfg(target_os = "linux")]
#[test]
fn an_inlined_body_says_the_lines_it_came_from() {
    let dir = fixture("inlined");
    std::fs::write(dir.join("one.c"), INLINED).expect("the fixture can be written");
    build(&dir, &["-g", "-O2"], "one.o");
    let out = Command::new("readelf")
        .arg("--debug-dump=decodedline")
        .arg(dir.join("one.o"))
        .output()
        .expect("readelf starts");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);

    let lines: Vec<u32> = text
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            words.next().filter(|file| file.ends_with("one.c"))?;
            words.next()?.parse().ok()
        })
        .collect();
    assert!(lines.iter().all(|&line| (1..=9).contains(&line)), "{text}");
    // The call to `g` is on line 3, and only the copy of `twice` in the loop is left to make it.
    assert!(lines.contains(&3), "{text}");
}

/// Each body the optimizer inlined is a `DW_TAG_inlined_subroutine` that names the function it is
/// a copy of and the line the call was on, which is what a debugger reads to show the call in a
/// backtrace and to stop there on a breakpoint on the function.
#[cfg(target_os = "linux")]
#[test]
fn an_inlined_body_is_an_entry_that_names_the_call() {
    let dir = fixture("inlined-entry");
    std::fs::write(dir.join("one.c"), INLINED).expect("the fixture can be written");
    build(&dir, &["-g", "-O2"], "one.o");
    let out = Command::new("readelf")
        .arg("--debug-dump=info")
        .arg(dir.join("one.o"))
        .output()
        .expect("readelf starts");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(text.contains("DW_TAG_inlined_subroutine"), "{text}");
    assert!(text.contains("DW_AT_abstract_origin"), "{text}");
    assert!(text.contains("DW_AT_inline"), "{text}");
    let line = text.lines().find(|line| line.contains("DW_AT_call_line")).expect("a call line");
    assert!(line.trim_end().ends_with(": 7"), "{line}");
}
