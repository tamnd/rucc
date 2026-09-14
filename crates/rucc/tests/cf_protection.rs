//! What a build whose control flow transfers are checked looks like, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6 and `spec/10-backend.md` section 10.7.
//!
//! The same reason `stack_clash.rs` beside this is a test of the whole compiler rather than of one
//! crate. The flag is read in the driver, the landing pad is written after the allocator has run,
//! and the note that says what the file was built for is written by the two output paths, so a
//! test in any one of them can be green while the flag on the command line does nothing.
//!
//! The note is half the feature and it is the half that is easy to leave out. A file with landing
//! pads in it and no note is a file the linker cannot tell was built for this, and one input like
//! that turns the check off for the whole program. So what is built and what is recorded are
//! tested together here, against the same command line.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because the landing pad and the
/// bits in the note are one machine's and the note itself is one object format's.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Three functions, none of which the flag has anything to say about on its own.
///
/// A leaf that takes no frame, one that calls something, and one whose frame is deep enough to be
/// taken a page at a time, which is the one case where the pad has somewhere else it could wrongly
/// end up.
const THREE: &str = "\
void use(void *);
int leaf(int x) { return x + 1; }
void calls(void) { use(0); }
void deep(void) { char b[100000]; use(b); }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-cf-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler wrote and what it said, for that source under those flags.
fn run(what: &str, target: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The assembly the compiler writes for that source under those flags.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let (ok, out, err) = run(what, TARGET, flags, source);
    assert!(ok, "the compiler refused the fixture:\n{err}");
    out
}

/// The lines of one function that are instructions, which is everything that is not a label and
/// not something said to the assembler.
fn insts<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("{name}:");
    let close = format!("\t.size\t{name},");
    text.lines()
        .skip_while(|line| **line != open)
        .take_while(|line| !line.starts_with(&close))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with('.') && !line.ends_with(':'))
        .collect()
}

/// The feature word the file records, or `None` in a file that records nothing.
///
/// Read out of the listing by finding the note and taking the word in the place a property's value
/// goes, which is after the key and the length. Written this way rather than by looking for the
/// number anywhere in the file, so that a note whose key and value were the wrong way round would
/// not pass.
fn recorded(text: &str) -> Option<u32> {
    let mut lines =
        text.lines().skip_while(|line| !line.contains(".note.gnu.property")).map(str::trim_start);
    // The key, and then the length and the value, which is what a property is made of.
    lines.find(|line| line.starts_with(".long\t0xc0000002"))?;
    let value = lines.nth(1)?;
    let word = value.strip_prefix(".long\t")?;
    u32::from_str_radix(word.trim_start_matches("0x"), 16).ok()
}

/// Every function opens with a landing pad when the forward edge is what was asked for.
///
/// Every function, including the leaf that takes no frame, because the address a pointer to a
/// function holds is the address of the function and a pointer is what the check is about. A
/// function that a compiler thought too simple to bother with is one the program cannot call
/// through a pointer any more.
#[test]
fn every_function_opens_with_a_landing_pad_when_the_forward_edge_is_asked_for() {
    for flag in ["-fcf-protection=branch", "-fcf-protection=full", "-fcf-protection"] {
        let text = asm("branch", &[flag], THREE);
        for name in ["leaf", "calls", "deep"] {
            let lines = insts(&text, name);
            assert_eq!(lines.first(), Some(&"endbr64"), "{flag} on {name}: {lines:?}");
            assert_eq!(
                lines.iter().filter(|line| **line == "endbr64").count(),
                1,
                "{flag} on {name}: one function, one pad"
            );
        }
    }
}

/// And nothing opens with one when it was not.
///
/// `return` is the case worth the test. It asks for the backward edge, which the machine checks
/// against a copy of the return address that nothing in the program maintains, so it is a mode
/// that changes no instruction and only records that it was asked for.
#[test]
fn nothing_opens_with_a_landing_pad_when_the_forward_edge_was_not_asked_for() {
    let plain = asm("plain", &[], THREE);
    for flags in [&[][..], &["-fcf-protection=none"], &["-fcf-protection=return"]] {
        let text = asm("none", flags, THREE);
        for name in ["leaf", "calls", "deep"] {
            let lines = insts(&text, name);
            assert!(!lines.contains(&"endbr64"), "{flags:?} on {name}: {lines:?}");
            assert_eq!(lines, insts(&plain, name), "{flags:?} changed {name}");
        }
    }
}

/// What the file records is what was asked for, and the two edges are two bits.
///
/// The bits are the linker's business rather than the loader's first: it keeps only the ones every
/// input has, so a file that records the wrong one is a file that turns off the half of the check
/// the rest of the program was built for.
#[test]
fn the_file_records_which_edges_it_was_built_to_have_checked() {
    for (flag, want) in [
        ("-fcf-protection=branch", 1),
        ("-fcf-protection=return", 2),
        ("-fcf-protection=full", 3),
        ("-fcf-protection", 3),
    ] {
        let text = asm("note", &[flag], THREE);
        assert_eq!(recorded(&text), Some(want), "{flag}");
    }
}

/// And a file built to have nothing checked records nothing at all.
///
/// `check` is the one worth writing down. It asks that the compilation be looked at for whether it
/// could be built this way rather than built this way, so it produces neither the pads nor the
/// note, which is what gcc does with it.
#[test]
fn a_file_built_to_have_nothing_checked_records_nothing() {
    for flags in [&[][..], &["-fcf-protection=none"], &["-fcf-protection=check"]] {
        let text = asm("quiet", flags, THREE);
        assert!(!text.contains(".note.gnu.property"), "{flags:?}");
        assert!(!text.contains("endbr64"), "{flags:?}");
    }
}

/// The pad is the first instruction of the function even when the prologue walks the stack.
///
/// A prologue that takes its frame a page at a time puts the walk in blocks of its own in front of
/// the block the function began with, so what a reader would expect to be the first instruction of
/// the function is not. The pad has to move with them: the address a pointer to this function holds
/// is where the walk starts, and a pad written anywhere else is a pad an indirect call never
/// reaches.
#[test]
fn the_landing_pad_stays_in_front_of_a_prologue_that_walks_the_stack() {
    let text = asm("deep", &["-fcf-protection=branch", "-fstack-clash-protection"], THREE);
    let lines = insts(&text, "deep");
    assert_eq!(lines[0], "endbr64", "{lines:?}");
    assert!(lines.iter().any(|line| line.starts_with("orb")), "the walk is still there: {lines:?}");
}

/// The two hardening flags that write into a prologue do not get in each other's way.
///
/// One goes at the very front and the other at the very back, so a function that asks for both
/// gets the pad first and the canary last with the frame taken in between.
#[test]
fn a_landing_pad_and_a_stack_protector_are_written_one_after_the_other() {
    let text = asm("both", &["-fcf-protection=branch", "-fstack-protector-all"], THREE);
    let lines = insts(&text, "calls");
    assert_eq!(lines[0], "endbr64", "{lines:?}");
    assert!(lines.iter().any(|line| line.contains("%fs:40")), "the canary is read: {lines:?}");
}

/// A target with nowhere to record it is refused rather than built without the record.
///
/// What says a file was built for this is an ELF note, and a file that is not ELF has nowhere to
/// put one. Landing pads with no record is the worst of both: the code is a little larger, nothing
/// turns the check on, and nothing says so. Windows has the same hardware and asks for it as a bit
/// in the image the linker is told to set, which is not something a compiler writes into an object.
#[test]
fn a_target_with_nowhere_to_record_it_is_refused() {
    let (ok, _, err) = run("windows", "x86_64-pc-windows-msvc", &["-fcf-protection=full"], THREE);
    assert!(!ok, "a flag that cannot be honoured is news rather than nothing");
    assert!(err.contains("-fcf-protection=full is not supported"), "{err}");
}
