//! What a build that promises a patcher room at the top of every function looks like, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.7 and `spec/10-backend.md` section 10.7.
//!
//! The same reason `profile.rs` beside this is a test of the whole compiler rather than of one
//! crate. The flag is read in the driver, the instruction that fills the room is the target's, the
//! room is written after the allocator has run, and the record saying where it is comes out of the
//! object writer, so a test in any one of those can be green while the flag does nothing.
//!
//! What is easy to get almost right is where the room is rather than how much of it there is. Room
//! after the function's own label is room a patcher can redirect a call into, and room in front of
//! it is somewhere to put a whole instruction the first one can reach; a compiler that put all of
//! it on one side would still reserve the number of bytes that was asked for. So the sides are
//! asserted, and so is what the symbol covers, since a patcher writing over room the symbol
//! includes would be writing over the function a debugger shows.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because the section the record goes
/// in is ELF's and the instruction that fills the room is x86-64's.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Two functions, neither of which the flag has anything to say about on its own.
const TWO: &str = "\
void use(void *);
int leaf(int x) { return x + 1; }
void calls(void) { use(0); }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-pfe-{}-{what}", std::process::id()));
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

/// Everything the compiler wrote about one function, from what it says to the assembler about it
/// down to the end of it, with the blank lines and the leading tabs taken off.
///
/// The whole of it rather than the instructions alone, which is what `profile.rs` beside this
/// takes, because half of what this asserts is where the labels are.
fn about<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!(".globl\t{name}");
    let close = format!(".size\t{name},");
    text.lines()
        .map(str::trim)
        .skip_while(|line| **line != open)
        .take_while(|line| !line.starts_with(&close))
        .filter(|line| !line.is_empty())
        .collect()
}

/// Where in those lines the first one that matches is.
fn at(lines: &[&str], want: &str) -> usize {
    lines
        .iter()
        .position(|line| line.contains(want))
        .unwrap_or_else(|| panic!("{want} is not in {lines:?}"))
}

/// Where the function's own label is, which is the line that is exactly it.
///
/// Not `at` above, because the label the record points at has the function's name in it too and is
/// written first.
fn label(lines: &[&str], name: &str) -> usize {
    let want = format!("{name}:");
    lines
        .iter()
        .position(|line| **line == want)
        .unwrap_or_else(|| panic!("{name} has no label in {lines:?}"))
}

/// How many of them do nothing, which is how much room there is.
fn nops(lines: &[&str]) -> usize {
    lines.iter().filter(|line| **line == "nop").count()
}

#[test]
fn the_room_asked_for_is_the_room_reserved() {
    for (flag, want) in [
        ("-fpatchable-function-entry=2", 2),
        ("-fpatchable-function-entry=5", 5),
        ("-fpatchable-function-entry=16", 16),
        ("-fpatchable-function-entry=5,3", 5),
        ("-fpatchable-function-entry=3,3", 3),
    ] {
        let text = asm("room", &[flag], TWO);
        for name in ["leaf", "calls"] {
            assert_eq!(nops(&about(&text, name)), want, "{flag} on {name}");
        }
    }
}

#[test]
fn nothing_is_reserved_in_a_build_that_asked_for_none() {
    // `=0` is a command line gcc takes and writes nothing for, so it is asserted beside the one
    // that did not write the flag at all rather than refused.
    for flags in [&[][..], &["-fpatchable-function-entry=0"][..]] {
        let text = asm("none", flags, TWO);
        for name in ["leaf", "calls"] {
            let lines = about(&text, name);
            assert_eq!(nops(&lines), 0, "{flags:?} on {name}");
            assert!(!text.contains("__patchable_function_entries"), "{flags:?}");
            assert!(!lines.iter().any(|line| line.contains("pfe_")), "{flags:?} on {name}");
        }
    }
}

#[test]
fn the_second_number_is_how_much_of_it_goes_in_front_of_the_function() {
    let text = asm("sides", &["-fpatchable-function-entry=5,3"], TWO);
    for name in ["leaf", "calls"] {
        let lines = about(&text, name);
        let own = label(&lines, name);
        let before = lines[..own].iter().filter(|line| **line == "nop").count();
        assert_eq!(before, 3, "in front of {name}");
        assert_eq!(nops(&lines[own..]), 2, "after {name}");
    }
}

#[test]
fn one_number_puts_all_of_it_after_the_function() {
    let text = asm("after", &["-fpatchable-function-entry=4"], TWO);
    let lines = about(&text, "leaf");
    let own = label(&lines, "leaf");
    assert_eq!(lines[..own].iter().filter(|line| **line == "nop").count(), 0);
    assert_eq!(nops(&lines[own..]), 4);
}

/// And the record says where the room begins, which is not where the function begins.
///
/// The two are the same address only when nothing was asked for in front of the label. A record
/// that pointed at the function instead would be one a patcher could still write over the room
/// with and would put the wrong number of bytes there.
#[test]
fn what_is_recorded_is_the_front_of_the_room() {
    let text = asm("record", &["-fpatchable-function-entry=5,3"], TWO);
    for name in ["leaf", "calls"] {
        let lines = about(&text, name);
        let section = at(&lines, "__patchable_function_entries");
        // The section it is ordered after is named by the label rather than by the section, which
        // is how an assembler is told about one it has not seen yet.
        assert!(lines[section].ends_with(&format!(",.Lpfe_{name}")), "{}", lines[section]);
        assert_eq!(lines[section + 1], ".align\t8");
        assert_eq!(lines[section + 2], format!(".quad\t.Lpfe_{name}"));
        // And the label is at the first byte of the room, which is in front of the function.
        let front = at(&lines, &format!(".Lpfe_{name}:"));
        assert!(front < label(&lines, name), "{lines:?}");
        assert_eq!(lines[front + 1], "nop");
    }
}

/// The same when the room is all after the label, where the front of it is not the function's
/// first instruction either.
///
/// A landing pad goes between them. It has to be first, since the address an indirect branch may
/// arrive at is the address of the function, and the room a patcher writes a call over must not
/// include it: a patched function still has to be one an indirect call can reach.
#[test]
fn a_landing_pad_stays_in_front_of_the_room_and_out_of_it() {
    let text = asm("landing", &["-fpatchable-function-entry=2", "-fcf-protection=branch"], TWO);
    for name in ["leaf", "calls"] {
        let lines = about(&text, name);
        let own = label(&lines, name);
        let pad = at(&lines, "endbr64");
        let front = at(&lines, &format!(".Lpfe_{name}:"));
        assert!(own < pad, "the pad is inside {name}");
        assert!(pad < front, "the pad is in front of the room in {name}");
        assert_eq!(lines[front + 1], "nop");
        assert_eq!(nops(&lines), 2);
    }
}

/// And the profiler's hook goes after the room rather than in front of it.
///
/// Both want to be near the top and only one of them can be first. gcc puts the room first, and it
/// is the right way round: what a patcher writes over the room is usually a call to the same kind
/// of thing the hook calls, so a hook in front of the room would be a second call nothing asked
/// for on every patched function.
#[test]
fn the_room_comes_before_the_profilers_hook() {
    let text = asm("hook", &["-fpatchable-function-entry=2", "-pg"], TWO);
    for name in ["leaf", "calls"] {
        let lines = about(&text, name);
        let front = at(&lines, &format!(".Lpfe_{name}:"));
        assert!(front < at(&lines, "__fentry__"), "{lines:?}");
        assert_eq!(nops(&lines), 2);
    }
}

/// A section of its own per function under `-ffunction-sections`, tied to that function's own
/// text rather than to the one every function would otherwise share.
#[test]
fn each_function_gets_its_own_record_when_it_gets_its_own_section() {
    let text = asm("split", &["-fpatchable-function-entry=2", "-ffunction-sections"], TWO);
    for name in ["leaf", "calls"] {
        let lines = about(&text, name);
        let section = at(&lines, "__patchable_function_entries");
        // What comes back after the record is this function's section, not `.text`, or the room
        // would be measured into a section the function is not in.
        assert_eq!(lines[section + 3], format!(".section\t.text.{name}"));
    }
}

/// And a target with nowhere to put the record is refused rather than compiled without one.
///
/// A build that took the flag and wrote the room and no record is the worst of the three answers:
/// every function is bigger and nothing can find any of them.
#[test]
fn a_target_with_no_section_to_record_it_in_is_refused() {
    let (ok, _, err) =
        run("refused", "x86_64-pc-windows-msvc", &["-fpatchable-function-entry=2"], TWO);
    assert!(!ok, "a target that cannot record the room took the flag anyway");
    assert!(err.contains("-fpatchable-function-entry="), "{err}");
    assert!(err.contains("is not the section this compiler writes"), "{err}");
}
