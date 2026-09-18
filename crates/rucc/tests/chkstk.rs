//! What a prologue looks like on a platform that reaches the pages of a frame by calling a routine.
//!
//! Design: `spec/10-backend.md` section 10.7.
//!
//! The same reason `stack_clash.rs` beside this is a test of the whole compiler rather than of one
//! crate. Which convention a tuple has is decided in the target, how big the frame is comes out of
//! the frame layout, and what the prologue is made of is written after the allocator has run, so a
//! test in any one of those can be green while the assembly a Windows target gets is a function
//! that faults on its own locals.
//!
//! Windows commits the pages of a stack by faulting on a guard page that then moves down, so a
//! frame larger than a page has to be reached a page at a time by every function that takes one and
//! not only by the ones somebody passed a flag about. The routine that does the reaching comes with
//! the C runtime, which is why the two runtimes are two targets here rather than one.

use std::path::PathBuf;
use std::process::Command;

/// Microsoft's runtime.
const MSVC: &str = "x86_64-pc-windows-msvc";

/// The GNU one, which provides the same routine under a different name.
const MINGW: &str = "x86_64-pc-windows-gnu";

/// The three sizes of frame the convention has two different answers for.
///
/// `small` fits in a page, `onepage` is just over one, and `many` is far enough over that a walk
/// written out in a straight line would not be worth it. Each hands the address of its array away,
/// so nothing here can decide the array is unused and the frame is not needed.
const THREE: &str = "\
void use(void *);
void small(void) { char b[64]; use(b); }
void onepage(void) { char b[4100]; use(b); }
void many(void) { char b[100000]; use(b); }
";

/// A frame that grows while it runs, which is the one case the routine cannot answer.
const GROWING: &str = "\
void use(void *);
void one(int n) { char b[n]; use(b); }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-chkstk-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source for that target under those flags.
fn asm(what: &str, target: &str, flags: &[&str], source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-"])
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

/// The lines of one function that are instructions, which is everything that is not a label and not
/// something said to the assembler.
///
/// A listing for this object format has no directive saying where a function ends, so the end is
/// where the next one is made visible.
fn insts<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("{name}:");
    text.lines()
        .skip_while(|line| **line != open)
        .skip(1)
        .take_while(|line| !line.trim().starts_with(".globl"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.ends_with(':'))
        .collect()
}

/// A frame larger than a page hands its size to the routine and takes the frame afterwards.
///
/// Three instructions: the size into the register the routine reads it in, the call, and the
/// subtraction. The subtraction is of the register rather than of the constant because the routine
/// hands the size back where it found it and leaves the stack pointer alone.
///
/// The size goes in with the thirty-two bit move, which writes the low half of `%rax` and clears
/// the high half, so what the routine reads in `%rax` is the number. That is `rucc_codegen::shorten`
/// writing the shorter of the two moves and it is why the register is named `%eax` here.
#[test]
fn a_frame_larger_than_a_page_is_reached_by_the_routine_the_runtime_provides() {
    let text = asm("many", MSVC, &[], THREE);
    let lines = insts(&text, "many");
    assert_eq!(
        &lines[..3],
        ["movl\t$100040, %eax", "call\t__chkstk", "subq\t%rax, %rsp"],
        "{lines:?}"
    );
    assert!(lines.contains(&"addq\t$100040, %rsp"), "{lines:?}");

    // One page over, which is the smallest frame the convention changes anything for.
    let lines = insts(&text, "onepage");
    assert_eq!(
        &lines[..3],
        ["movl\t$4136, %eax", "call\t__chkstk", "subq\t%rax, %rsp"],
        "{lines:?}"
    );
}

/// A frame that fits in a page is taken in one subtraction, the way it is everywhere else.
///
/// The far end of such a frame is the near end of the page below it, so the guard page is reached
/// by writing to it rather than stepped over. Most functions are this one and the convention costs
/// them nothing.
#[test]
fn a_frame_that_fits_in_a_page_calls_nothing() {
    let text = asm("small", MSVC, &[], THREE);
    let lines = insts(&text, "small");
    assert_eq!(lines[0], "subq\t$104, %rsp", "{lines:?}");
    assert!(!lines.iter().any(|line| line.contains("chkstk")), "{lines:?}");
}

/// Which runtime the program is being built against decides what the routine is called.
///
/// The routine lives in the C runtime rather than in the compiler, so a build against mingw-w64 and
/// a build against Microsoft's runtime want different names for the same thing. It is the one
/// question the environment of a tuple decides about a convention, and everything else about the
/// two is the same.
#[test]
fn the_name_of_the_routine_comes_from_the_runtime_being_built_against() {
    let text = asm("mingw", MINGW, &[], THREE);
    let lines = insts(&text, "many");
    assert_eq!(
        &lines[..3],
        ["movl\t$100040, %eax", "call\t___chkstk_ms", "subq\t%rax, %rsp"],
        "{lines:?}"
    );
    assert!(!text.contains("call\t__chkstk\n"), "{text}");
}

/// The flag asks for nothing extra here, because the convention already asked for all of it.
///
/// What `-fstack-clash-protection` wants is that no page below the stack is stepped over, and the
/// routine is the platform's own way of doing exactly that. So a prologue under the flag is the
/// same prologue, and in particular it is not a walk written around a call.
#[test]
fn the_hardening_flag_leaves_such_a_prologue_alone() {
    let plain = asm("plain", MSVC, &[], THREE);
    let asked = asm("asked", MSVC, &["-fstack-clash-protection"], THREE);
    assert_eq!(insts(&plain, "many"), insts(&asked, "many"), "{asked}");
    assert!(!asked.contains("orb"), "{asked}");
}

/// A frame that grows while it runs is walked here whatever the command line said.
///
/// The routine cannot be what reaches those pages: it takes its size in a register the allocator
/// hands out and destroys two more, which is answerable in a prologue and not in the middle of a
/// function. So the declaration gets the same loop it gets under the flag elsewhere, and it gets it
/// with no flag passed, because on this platform reaching the pages is the convention.
#[test]
fn a_variable_length_array_walks_its_pages_with_no_flag_passed() {
    let text = asm("growing", MSVC, &[], GROWING);
    let lines = insts(&text, "one");
    let at = |what: &str| {
        lines.iter().position(|line| line.starts_with(what)).unwrap_or_else(|| panic!("{lines:?}"))
    };
    // Where it is going, worked out from the bytes before the stack pointer moves, and then a page
    // at a time with the question behind each step.
    let limit = at("subq\t%r");
    assert_eq!(lines[limit + 1], "subq\t$4096, %rsp", "{lines:?}");
    assert!(lines[limit + 2].starts_with("cmpq\t%r"), "{lines:?}");
    assert!(lines.contains(&"orb\t$0, (%rsp)"), "{lines:?}");
    // And nothing was handed to the routine, since this function's own frame is small.
    assert!(!lines.iter().any(|line| line.contains("chkstk")), "{lines:?}");
}

/// Nothing about any of this reaches a target whose convention did not ask for it.
///
/// A System V frame is taken in one subtraction however large it is, and what touches each page of
/// it is the flag in `spec/04-driver-and-cli.md` section 4.7 rather than the platform.
#[test]
fn a_system_v_target_takes_its_frame_in_one_subtraction_as_it_always_did() {
    let text = asm("sysv", "x86_64-unknown-linux-gnu", &[], THREE);
    assert!(!text.contains("chkstk"), "{text}");
    assert!(text.contains("subq\t$100008, %rsp"), "{text}");
}
