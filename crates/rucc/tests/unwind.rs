//! What the assembly listing tells an unwinder about a frame.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! An unwinder is handed a return address and has to answer two questions about the function it
//! landed in: where the frame it is standing in ends, and where the callee saved registers went.
//! The answer is a table, and the assembler builds that table out of `.cfi_` directives placed
//! between the instructions that change the answer. Everything here reads the listing, because the
//! listing is where the directives are and because a build that goes through `as` has to get the
//! same table as one that does not.
//!
//! The target is written down rather than taken from the host, because this table is an ELF thing.

use std::process::Command;

/// The one target these directives are written for.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A leaf that takes no frame, a call that takes a small one, and a call that takes a big one.
///
/// `bottom` is left undefined so nothing can be inlined into anything and the three shapes survive
/// to the machine.
const SHAPES: &str = "\
void bottom(void);
int leaf(int a, int b) { return a + b; }
void small(void) { bottom(); }
long big(long a, long b, long c, long d, long e, long f, long g) {
    long s = 0;
    for (long i = 0; i < g; i++) s += a * b + c * d + e * f + i;
    bottom();
    return s;
}
";

/// The assembly the compiler produces for that source under those flags.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-unwind-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-O1", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The lines of one function, from its label to the end of its record.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let start = text.lines().position(|line| line == format!("{name}:")).expect("{name} is here");
    let rest = text.lines().skip(start);
    let mut out = Vec::new();
    for line in rest {
        out.push(line.trim());
        if line.trim() == ".cfi_endproc" {
            return out;
        }
    }
    panic!("{name} has no end to its record");
}

/// Every function is wrapped, including the ones with nothing to say.
///
/// An unwinder that lands on an address no record covers has to give up, and it has no way to tell
/// a function that needs no directives from one that was never described. The empty record is how a
/// leaf asks for the defaults, which are already right for it: the frame ends at `rsp+8` and the
/// return address is the word below that.
#[test]
fn every_function_gets_a_record() {
    let text = asm("all", &[&format!("--target={TARGET}")], SHAPES);
    for name in ["leaf", "small", "big"] {
        let lines = body(&text, name);
        assert_eq!(lines[1], ".cfi_startproc", "{name}:\n{}", lines.join("\n"));
        assert_eq!(lines[lines.len() - 1], ".cfi_endproc", "{name}:\n{}", lines.join("\n"));
    }
    let leaf = body(&text, "leaf").join("\n");
    assert!(!leaf.contains(".cfi_def_cfa"), "{leaf}");
}

/// Moving the stack pointer moves the far end of the frame with it, and the directive goes after
/// the instruction that moved it, because a return address in the middle of an instruction is not
/// a thing that happens.
#[test]
fn a_frame_says_where_it_moved_the_stack_pointer() {
    let text = asm("moved", &[&format!("--target={TARGET}")], SHAPES);
    let lines = body(&text, "small");
    let sub = lines.iter().position(|line| line.starts_with("subq")).expect("small takes a frame");
    assert_eq!(lines[sub + 1], ".cfi_def_cfa_offset 16", "{}", lines.join("\n"));
    let add = lines.iter().position(|line| line.starts_with("addq")).expect("small gives it back");
    assert_eq!(lines[add + 1], ".cfi_def_cfa_offset 8", "{}", lines.join("\n"));
}

/// A saved register is written as a DWARF number rather than a name, and DWARF numbers the first
/// eight registers in an order that is not the machine's. `rbx` is 3 either way, but `r12` through
/// `r15` are 12 through 15 in both, which is the half that agrees.
#[test]
fn a_saved_register_says_where_it_went() {
    let text = asm("saved", &[&format!("--target={TARGET}")], SHAPES);
    let lines = body(&text, "big").join("\n");
    assert!(lines.contains(".cfi_offset 3, -16"), "{lines}");
    assert!(lines.contains(".cfi_offset 12, -24"), "{lines}");
    assert!(lines.contains(".cfi_restore 3"), "{lines}");
    assert!(lines.contains(".cfi_restore 12"), "{lines}");
}

/// A function with two returns has two epilogues, and the second one has to start from the state
/// the body was in rather than the state the first one left behind. The prologue remembers that
/// state and each epilogue puts it back, then remembers it again, because the assembler keeps a
/// stack of them and a restore takes one off.
#[test]
fn a_second_return_puts_the_body_state_back() {
    let text = asm(
        "twice",
        &[&format!("--target={TARGET}")],
        "\
void bottom(void);
long twice(long a, long b) {
    if (a > b) { bottom(); return a; }
    bottom();
    return b;
}
",
    );
    let lines = body(&text, "twice");
    let joined = lines.join("\n");
    assert!(joined.contains(".cfi_remember_state"), "{joined}");
    let restore = lines.iter().position(|line| *line == ".cfi_restore_state").expect("{joined}");
    assert_eq!(lines[restore - 1], "ret", "{joined}");
    assert_eq!(lines[restore + 1], ".cfi_remember_state", "{joined}");
}

/// Nothing is said after the last instruction, because a record covers the function and not the
/// byte after it. The epilogue on the end of the last block would otherwise put back a state that
/// no return address can be in.
#[test]
fn nothing_is_said_after_the_last_instruction() {
    let text = asm("tail", &[&format!("--target={TARGET}")], SHAPES);
    for name in ["leaf", "small", "big"] {
        let lines = body(&text, name);
        let end = lines.len() - 1;
        assert!(!lines[end - 1].starts_with(".cfi"), "{name}:\n{}", lines.join("\n"));
    }
}

/// The other formats do not read these directives, and Mach-O has its own answer to the same
/// question. Handing them ELF's would be a file their assembler refuses.
#[test]
fn a_format_without_this_table_is_not_given_one() {
    let text = asm("darwin", &["--target=x86_64-apple-darwin"], SHAPES);
    assert!(!text.contains(".cfi"), "{text}");
}

/// A build can say nothing will ever walk it, which is what a kernel says, and then there is no
/// table at all rather than an empty one.
#[test]
fn a_build_that_says_nothing_walks_it_gets_no_table() {
    let off = &[&format!("--target={TARGET}"), "-fno-asynchronous-unwind-tables"];
    assert!(!asm("off", off, SHAPES).contains(".cfi"), "the table was written anyway");
    let back = &[off[0], off[1], "-fasynchronous-unwind-tables"];
    assert!(asm("back", back, SHAPES).contains(".cfi_startproc"), "the last flag did not win");
}

/// The two requests are answered with the same table, so asking for one and against the other
/// leaves a table standing. That is gcc's arrangement, and the line comes up when a build turns the
/// asynchronous one off globally and a directory turns a table back on.
#[test]
fn asking_for_a_table_and_against_an_asynchronous_one_leaves_a_table() {
    let both =
        &[&format!("--target={TARGET}"), "-fno-asynchronous-unwind-tables", "-funwind-tables"];
    assert!(asm("both", both, SHAPES).contains(".cfi_startproc"), "the weaker request was dropped");
}

/// The section goes with the directives. An object written without them and one written with them
/// differ by a section a linker collects and an unwinder reads, and the name of it is in the file.
#[test]
fn the_section_follows_the_directives() {
    let with = object("on", &[&format!("--target={TARGET}")]);
    let without =
        object("off", &[&format!("--target={TARGET}"), "-fno-asynchronous-unwind-tables"]);
    assert!(with.windows(9).any(|w| w == b".eh_frame"), "no section in an ordinary build");
    assert!(!without.windows(9).any(|w| w == b".eh_frame"), "a section nothing asked for");
}

/// The object the compiler produces for [`SHAPES`] under those flags.
fn object(what: &str, flags: &[&str]) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("rucc-unwind-obj-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SHAPES).expect("the fixture can be written");
    let out = dir.join("one.o");
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-O1", "-c"])
        .args(flags)
        .arg("-o")
        .arg(&out)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(
        done.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let bytes = std::fs::read(&out).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}
