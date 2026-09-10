//! What a prologue that takes its frame a page at a time looks like, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6 and `spec/10-backend.md` section 10.7.
//!
//! The same reason `stack_protector.rs` beside this is a test of the whole compiler rather than of
//! one crate. The flag is read in the driver, the size of the frame is worked out in the frame
//! layout, and what the prologue is made of is decided after the allocator has run, so a test in
//! any one of them can be green while the flag on the command line does nothing.
//!
//! What is being defended against is a frame larger than the page the operating system leaves
//! unmapped below the stack. A prologue that takes such a frame in one subtraction moves the stack
//! pointer clean over that page, and the first local it writes to is written past a guard that was
//! never touched, into whatever the program mapped next. Which page each store lands on is the
//! whole of the question, so this reads the offsets rather than counting instructions.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because how far apart the touches
/// are is a fact about the kernel that runs the program.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The four sizes of frame the flag has four different answers for.
///
/// `small` fits in a page, `onepage` is just over one, `twopage` is a straight line of touches, and
/// `many` is more pages than a straight line is worth. Each hands the address of its array away, so
/// nothing here can decide the array is unused and the frame is not needed.
const FOUR: &str = "\
void use(void *);
void small(void) { char b[64]; use(b); }
void onepage(void) { char b[4100]; use(b); }
void twopage(void) { char b[9000]; use(b); }
void many(void) { char b[100000]; use(b); }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-clash-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source under those flags.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
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

/// The lines of one function of the listing, trimmed, without its directives.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("{name}:");
    let close = format!("\t.size\t{name},");
    text.lines()
        .skip_while(|line| **line != open)
        .take_while(|line| !line.starts_with(&close))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

/// The lines of one function that are instructions, which is everything that is not a label and
/// not something said to the assembler.
fn insts<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    body(text, name)
        .into_iter()
        .filter(|line| !line.starts_with('.') && !line.ends_with(':'))
        .collect()
}

/// A frame that fits in one page is taken in one subtraction, flag or no flag.
///
/// The far end of such a frame is the near end of the page below it, so anything written in it is
/// written to a page that is there. Most functions are this one, and the flag costs them nothing.
#[test]
fn a_frame_that_fits_in_a_page_is_taken_the_way_it_always_was() {
    let text = asm("small", &["-fstack-clash-protection"], FOUR);
    let lines = insts(&text, "small");
    assert!(lines.contains(&"subq\t$72, %rsp"), "{lines:?}");
    assert!(!lines.iter().any(|line| line.starts_with("orb")), "{lines:?}");
}

/// A frame a few pages deep is one subtraction of a page and one touch, over and over.
///
/// The order is what matters: the stack pointer never moves further than one page without
/// something being written where it landed, so the first page that is not there is the one that
/// faults. The last subtraction is the remainder and is smaller than a page, so it needs no touch.
#[test]
fn a_frame_a_few_pages_deep_touches_each_page_as_it_reaches_it() {
    let text = asm("twopage", &["-fstack-clash-protection"], FOUR);
    let lines = insts(&text, "twopage");
    assert_eq!(
        &lines[..5],
        [
            "subq\t$4096, %rsp",
            "orb\t$0, (%rsp)",
            "subq\t$4096, %rsp",
            "orb\t$0, (%rsp)",
            "subq\t$808, %rsp",
        ],
        "{lines:?}"
    );

    // And one page over, which is the smallest frame the flag changes anything for.
    let text = asm("onepage", &["-fstack-clash-protection"], FOUR);
    let lines = insts(&text, "onepage");
    assert_eq!(
        &lines[..3],
        ["subq\t$4096, %rsp", "orb\t$0, (%rsp)", "subq\t$8, %rsp"],
        "{lines:?}"
    );
}

/// A frame deeper than that works out where it is going and then walks there.
///
/// A straight line of touches is two instructions a page and a loop is four however many pages it
/// walks, so past a handful of pages the loop is smaller. The address it stops at is worked out
/// before the stack pointer moves, because after that there is nothing to work it out from.
#[test]
fn a_frame_deeper_than_a_handful_of_pages_walks_them_in_a_loop() {
    let text = asm("many", &["-fstack-clash-protection"], FOUR);
    let lines = insts(&text, "many");
    assert_eq!(lines[0], "leaq\t-98304(%rsp), %r10", "{lines:?}");
    assert_eq!(&lines[1..3], ["subq\t$4096, %rsp", "orb\t$0, (%rsp)"], "{lines:?}");
    // The comparison, whatever it takes to get from it to a branch on this target, and then the
    // remainder, which is what is left of the frame after the pages the loop walked.
    assert!(lines[3].starts_with("cmpq\t%r10, %rsp"), "{lines:?}");
    let jump = lines.iter().position(|line| line.starts_with("jne")).expect("a loop ends");
    assert_eq!(lines[jump + 1], "subq\t$1704, %rsp", "{lines:?}");
    // The whole of the frame, and none of it taken in a step larger than a page.
    assert_eq!(98304 + 1704, 100008, "the two steps are the frame this function has");
}

/// The unwinder is told where the frame is throughout, which is the part a loop makes hard.
///
/// Inside the loop the stack pointer is moving and the register the limit is in is not, so the
/// frame is described in terms of that register for as long as the walk lasts. It goes back to
/// being described in terms of the stack pointer at the subtraction that takes the remainder,
/// which is the first instruction after the walk where the stack pointer is where it will stay.
#[test]
fn the_unwinder_is_told_the_frame_is_off_the_limit_register_while_the_walk_lasts() {
    let text = asm("cfi", &["-fstack-clash-protection"], FOUR);
    let lines = body(&text, "many");
    let at = |what: &str| {
        lines.iter().position(|line| line.starts_with(what)).unwrap_or_else(|| panic!("{lines:?}"))
    };
    // Ten is the limit register and seven is the stack pointer, in the numbers a table is written
    // in. The first row is behind the instruction that loads the limit, and the second behind the
    // subtraction that ends the walk.
    assert!(lines.contains(&".cfi_def_cfa 10, 98312"), "{lines:?}");
    assert!(lines.contains(&".cfi_def_cfa 7, 100016"), "{lines:?}");
    assert!(at(".cfi_def_cfa 10") < at(".cfi_def_cfa 7"), "{lines:?}");

    // A frame taken in a straight line never leaves the stack pointer, so every row in one of
    // those is the offset alone and there is one behind every step.
    let lines = body(&text, "twopage");
    assert_eq!(lines.iter().filter(|line| line.starts_with(".cfi_def_cfa_offset")).count(), 4);
    assert!(!lines.iter().any(|line| line.starts_with(".cfi_def_cfa ")), "{lines:?}");
}

/// Nothing is touched unless something asked, which is gcc's default and this one.
#[test]
fn a_frame_is_taken_in_one_subtraction_until_the_flag_asks_otherwise() {
    for flags in [&[][..], &["-fstack-clash-protection", "-fno-stack-clash-protection"]] {
        let text = asm("off", flags, FOUR);
        assert!(!text.contains("orb"), "{flags:?}: {text}");
        assert!(insts(&text, "many").contains(&"subq\t$100008, %rsp"), "{flags:?}: {text}");
    }
}

/// The two hardening flags are about two different things and compose.
///
/// One is about how the frame is taken and the other about what is put in it, so a function under
/// both walks its pages and then writes its canary, in that order, because there is no slot to
/// write the canary into until the frame has been taken.
#[test]
fn a_probing_prologue_and_a_stack_protector_are_written_one_after_the_other() {
    let text = asm("both", &["-fstack-clash-protection", "-fstack-protector-strong"], FOUR);
    let lines = insts(&text, "twopage");
    let at = |what: &str| {
        lines.iter().position(|line| line.contains(what)).unwrap_or_else(|| panic!("{lines:?}"))
    };
    assert!(at("orb\t$0, (%rsp)") < at("%fs:40"), "{lines:?}");
    assert!(at("%fs:40") < at("__stack_chk_fail"), "{lines:?}");
}
