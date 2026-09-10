//! What a build whose functions call a profiler on the way in looks like, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.7 and `spec/10-backend.md` section 10.7.
//!
//! The same reason `cf_protection.rs` beside this is a test of the whole compiler rather than of
//! one crate. The flag is read in the driver, the name of what is called is the target's, the call
//! is written after the allocator has run, and where it goes decides whether the function is given
//! a frame pointer, so a test in any one of those can be green while the flag does nothing.
//!
//! Where the call goes is the whole of the feature and it is what is easy to get almost right. The
//! earlier hook is worth having because the stack at that instruction is exactly what a `call`
//! leaves, which is what lets a tracer replace it while the program runs, and a hook written one
//! instruction later is a hook that no longer has that property and that nothing complains about.
//! So the position is asserted rather than the presence.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because the two names and the rule
/// about which comes first are one platform's.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Three functions, none of which the flag has anything to say about on its own.
///
/// A leaf that takes no frame, one that calls something, and one whose frame is deep enough to be
/// taken a page at a time, which is the one case where the call has somewhere else it could
/// wrongly end up.
const THREE: &str = "\
void use(void *);
int leaf(int x) { return x + 1; }
void calls(void) { use(0); }
void deep(void) { char b[100000]; use(b); }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-pg-{}-{what}", std::process::id()));
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

/// Where in a list of instructions the first one that matches is, which is what the ordering tests
/// are all written in terms of.
fn at(lines: &[&str], want: &str) -> usize {
    lines
        .iter()
        .position(|line| line.contains(want))
        .unwrap_or_else(|| panic!("{want} is not in {lines:?}"))
}

/// Every function calls the earlier hook as its first instruction.
///
/// First, before anything the prologue does, because what makes this hook worth replacing while
/// the program runs is that the stack at that instruction is what a `call` left: the return
/// address on top and the arguments still in the registers they arrived in. A prologue that had
/// already pushed something would have taken that away.
///
/// Every function, including the leaf, since a profile with a function missing from it is a
/// profile that attributes that function's time to whoever called it.
#[test]
fn every_function_opens_with_the_earlier_hook_when_a_profile_is_asked_for() {
    for flags in [&["-pg"][..], &["-p"], &["-pg", "-mfentry"], &["-mfentry", "-pg"]] {
        let text = asm("early", flags, THREE);
        for name in ["leaf", "calls", "deep"] {
            let lines = insts(&text, name);
            assert_eq!(lines.first(), Some(&"call\t__fentry__"), "{flags:?} on {name}: {lines:?}");
            assert_eq!(
                lines.iter().filter(|line| line.contains("__fentry__")).count(),
                1,
                "{flags:?} on {name}: one function, one hook"
            );
        }
    }
}

/// And nothing calls anything when no profile was asked for.
///
/// `-mfentry` on its own is the case worth the test. It says where the call goes and a command
/// line that asked for no call has nowhere to put one, so gcc accepts it and does nothing with it,
/// and a build system that sets it globally and asks for the profile per directory is one that
/// would otherwise fail everywhere else.
#[test]
fn nothing_is_called_when_no_profile_was_asked_for() {
    let plain = asm("plain", &[], THREE);
    for flags in [&[][..], &["-mfentry"], &["-mno-fentry"]] {
        let text = asm("quiet", flags, THREE);
        assert!(!text.contains("__fentry__"), "{flags:?}");
        assert!(!text.contains("mcount"), "{flags:?}");
        for name in ["leaf", "calls", "deep"] {
            assert_eq!(insts(&text, name), insts(&plain, name), "{flags:?} changed {name}");
        }
    }
}

/// The older hook goes after the frame is taken, and takes a frame pointer with it.
///
/// It reads the frame pointer to find out which function called this one, so it has to run once
/// there is one, and a function that calls it is given one whatever the rest of the command line
/// said. The leaf is the case that shows it: nothing else in that function needs a frame pointer
/// and it has one anyway.
#[test]
fn the_older_hook_goes_after_the_frame_and_forces_a_frame_pointer() {
    let text = asm("late", &["-pg", "-mno-fentry"], THREE);
    for name in ["leaf", "calls", "deep"] {
        let lines = insts(&text, name);
        assert!(!lines.iter().any(|line| line.contains("__fentry__")), "{name}: {lines:?}");
        assert!(at(&lines, "movq\t%rsp, %rbp") < at(&lines, "call\tmcount"), "{name}: {lines:?}");
        assert_eq!(lines[0], "pushq\t%rbp", "{name} is given a frame pointer: {lines:?}");
    }
}

/// The hook is the first instruction even when the prologue walks the stack.
///
/// A prologue that takes its frame a page at a time puts the walk in blocks of its own in front of
/// the block the function began with, so what a reader would expect to be the first instruction of
/// the function is not. The hook has to move with them for the reason it is first at all, and it
/// has to stay behind the landing pad, since the address a pointer to this function holds is the
/// address of the pad.
#[test]
fn the_earlier_hook_stays_in_front_of_a_prologue_that_walks_the_stack() {
    let flags = ["-pg", "-fcf-protection=branch", "-fstack-clash-protection"];
    let text = asm("deep", &flags, THREE);
    let lines = insts(&text, "deep");
    assert_eq!(lines[0], "endbr64", "{lines:?}");
    assert_eq!(lines[1], "call\t__fentry__", "{lines:?}");
    assert!(at(&lines, "call\t__fentry__") < at(&lines, "orb"), "the walk is after: {lines:?}");
}

/// The older hook goes in front of the canary rather than behind it.
///
/// Which is where gcc puts it, and the reason is that the hook is a call: it has to run before
/// anything this function is keeping in its frame could be read back out of it, and the canary is
/// the first thing the function keeps there.
#[test]
fn the_older_hook_goes_in_front_of_the_canary() {
    let text = asm("canary", &["-pg", "-mno-fentry", "-fstack-protector-all"], THREE);
    let lines = insts(&text, "calls");
    assert!(at(&lines, "call\tmcount") < at(&lines, "%fs:40"), "{lines:?}");
}

/// A leaf that is profiled the earlier way keeps the red zone.
///
/// The hook runs before the prologue has written anything, so the bytes below the stack pointer it
/// uses are ones this function has not put anything in yet, and a leaf that keeps its locals down
/// there is still a leaf afterwards. gcc leaves such a function alone too, which is the whole
/// reason the earlier hook is cheap enough to build a kernel with.
#[test]
fn a_leaf_profiled_the_earlier_way_still_takes_no_frame() {
    let text = asm("leaf", &["-pg"], "int leaf(int x) { return x + 1; }\n");
    let lines = insts(&text, "leaf");
    assert_eq!(lines[0], "call\t__fentry__", "{lines:?}");
    assert!(!lines.iter().any(|line| line.contains("%rbp")), "no frame: {lines:?}");
    assert!(!lines.iter().any(|line| line.starts_with("subq\t$")), "no frame: {lines:?}");
}

/// A target whose profiler asks for something else is refused rather than built to call a name
/// nothing defines.
///
/// Windows profiles a build by having the compiler call `_penter`, which is asked for by a switch
/// of its own and takes its argument in a register rather than off the stack, so it is not this
/// hook under another name. Writing this one there would produce a link error a long way from the
/// flag that caused it.
#[test]
fn a_target_whose_profiler_is_a_different_one_is_refused() {
    let (ok, _, err) = run("windows", "x86_64-pc-windows-msvc", &["-pg"], THREE);
    assert!(!ok, "a flag that cannot be honoured is news rather than nothing");
    assert!(err.contains("-pg is not supported"), "{err}");
}
