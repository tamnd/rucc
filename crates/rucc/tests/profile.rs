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
        // The frames read here are the ones taken without a frame pointer, which `-O0` would
        // otherwise keep as gcc does. A test that wants one says so after this and wins.
        .arg("-fomit-frame-pointer")
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

/// With `-mrecord-mcount` each hook's call is named and the name goes in `__mcount_loc` right
/// after it, which is gcc's layout: a kernel before 5.12 reads that section at boot to find every
/// call it can turn into a nop. Without the flag there is no such section.
#[test]
fn every_hook_is_listed_in_mcount_loc_when_asked() {
    let text = asm("record", &["-pg", "-mfentry", "-mrecord-mcount"], THREE);
    for name in ["leaf", "calls", "deep"] {
        let label = format!(".Lmcount_{name}:");
        let listed = format!("\t.quad\t.Lmcount_{name}\n\t.previous");
        let at = text.find(&label).unwrap_or_else(|| panic!("{label} in\n{text}"));
        let call = &text[at..];
        assert!(call[label.len()..].trim_start().starts_with("call"), "{name}:\n{call}");
        assert!(text.contains(&listed), "{name}:\n{text}");
    }
    assert_eq!(text.matches("\t.section\t__mcount_loc,\"a\",@progbits").count(), 3, "{text}");
    let plain = asm("unrecorded", &["-pg", "-mfentry"], THREE);
    assert!(!plain.contains("__mcount_loc"), "{plain}");
}

/// `-mnop-mcount` writes gcc's five byte nop where the call would be, so nothing is called until a
/// tracer writes the call back, and the nop is still what `__mcount_loc` points at.
#[test]
fn the_hook_is_a_nop_of_the_same_length_when_asked() {
    let text =
        asm("nop", &["-pg", "-mfentry", "-fno-pie", "-mnop-mcount", "-mrecord-mcount"], THREE);
    assert!(!text.contains("__fentry__"), "{text}");
    for name in ["leaf", "calls", "deep"] {
        let nop = format!(".Lmcount_{name}:\n\t.byte\t0x0f, 0x1f, 0x44, 0x00, 0x00\n");
        assert!(text.contains(&nop), "{name}:\n{text}");
    }
}

/// gcc refuses `-mnop-mcount` in position independent code, the default included, and so does
/// rucc, in gcc's words.
#[test]
fn the_nop_is_refused_beside_position_independent_code() {
    for flags in
        [&["-mnop-mcount"][..], &["-fPIC", "-pg", "-mnop-mcount"], &["-fpie", "-mnop-mcount"]]
    {
        let (ok, _, err) = run("nop-pic", TARGET, flags, THREE);
        assert!(!ok, "{flags:?}");
        assert!(err.contains("'-mnop-mcount' is not implemented for '-fPIC'"), "{flags:?}: {err}");
    }
    asm("nop-abs", &["-pg", "-fno-pic", "-mnop-mcount"], THREE);
}

/// A function that said `no_instrument_function` gets no hook and no entry in `__mcount_loc`, as
/// with gcc. It is the kernel's `notrace`, which is on the tracer itself and on what runs before
/// the tracer can, so a hook there would call back into the code handling one.
#[test]
fn a_function_that_said_no_instrument_function_gets_no_hook() {
    let source = "\
int g(int);
__attribute__((no_instrument_function)) int quiet(int x) { return g(x); }
int loud(int x) { return g(x); }
";
    for flags in [
        &["-pg", "-mfentry"][..],
        &["-pg", "-mno-fentry"],
        &["-pg", "-mfentry", "-mrecord-mcount"],
        &["-pg", "-fno-pie", "-mrecord-mcount", "-mnop-mcount"],
    ] {
        let text = asm("notrace", flags, source);
        let body = |name: &str| -> String {
            let from = text.find(&format!("\n{name}:\n")).unwrap_or_else(|| panic!("{name}"));
            text[from..].split("\t.size\t").next().unwrap_or("").to_owned()
        };
        let hooked =
            |body: &str| ["mcount", "fentry", "0x0f, 0x1f"].iter().any(|h| body.contains(h));
        assert!(!hooked(&body("quiet")), "{flags:?}:\n{text}");
        assert!(hooked(&body("loud")), "{flags:?}:\n{text}");
    }
}

/// The flags are x86 flags, as they are to gcc, and unknown anywhere else.
#[test]
fn the_mcount_flags_are_unknown_off_x86() {
    let (ok, _, err) = run("arm", "aarch64-unknown-linux-gnu", &["-mrecord-mcount"], THREE);
    assert!(!ok && err.contains("unknown option"), "{err}");
}

/// The object is the listing's: one eight byte address per hook in an allocated `__mcount_loc`,
/// each an `R_X86_64_64` against the section the function is in, pointing at the call, for either
/// hook and with a section per function or without.
#[test]
fn the_object_lists_the_address_of_every_call() {
    for (what, flags) in [
        ("obj-fentry", &["-mfentry"][..]),
        ("obj-mcount", &["-mno-fentry"]),
        ("obj-split", &["-mfentry", "-ffunction-sections"]),
    ] {
        let path = fixture(what, THREE);
        let object = path.with_extension("o");
        let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .arg(format!("--target={TARGET}"))
            .args(["-O2", "-pg", "-mrecord-mcount", "-c", "-o"])
            .arg(&object)
            .args(flags)
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
        let bytes = std::fs::read(&object).expect("the object was written");
        let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
        let elf = Elf(&bytes);
        let loc = elf.section("__mcount_loc").unwrap_or_else(|| panic!("{what}: no __mcount_loc"));
        assert_eq!((elf.kind(loc), elf.flags(loc) & 2, elf.size(loc)), (1, 2, 24), "{what}");
        let rela = elf.section(".rela__mcount_loc").expect("its relocations");
        let symtab = elf.word(elf.header(rela) + 40, 4);
        let mut calls = 0;
        for n in 0..elf.size(rela) / 24 {
            let entry = elf.offset(rela) + n * 24;
            let info = elf.word(entry + 8, 8);
            assert_eq!(info & 0xffff_ffff, 1, "{what}: R_X86_64_64");
            let symbol = elf.offset(symtab) + (info >> 32) * 24;
            let target = elf.word(symbol + 6, 2);
            let at = elf.offset(target) + elf.word(entry + 16, 8);
            assert!(elf.name(target).starts_with(".text"), "{what}: {}", elf.name(target));
            assert_eq!(bytes[at], 0xe8, "{what}: entry {n} points at a call");
            calls += 1;
        }
        assert_eq!(calls, 3, "{what}");
    }
}

/// Just enough of a 64-bit little endian ELF file to find a section and read its relocations.
struct Elf<'a>(&'a [u8]);

impl Elf<'_> {
    fn word(&self, at: usize, width: usize) -> usize {
        self.0[at..at + width].iter().rev().fold(0, |sum, &byte| sum << 8 | usize::from(byte))
    }

    fn header(&self, index: usize) -> usize {
        self.word(0x28, 8) + index * self.word(0x3a, 2)
    }

    fn kind(&self, index: usize) -> usize {
        self.word(self.header(index) + 4, 4)
    }

    fn flags(&self, index: usize) -> usize {
        self.word(self.header(index) + 8, 8)
    }

    fn offset(&self, index: usize) -> usize {
        self.word(self.header(index) + 24, 8)
    }

    fn size(&self, index: usize) -> usize {
        self.word(self.header(index) + 32, 8)
    }

    fn name(&self, index: usize) -> String {
        let strings = self.offset(self.word(0x3e, 2));
        let at = strings + self.word(self.header(index), 4);
        let end = self.0[at..].iter().position(|&byte| byte == 0).expect("a name ends");
        String::from_utf8_lossy(&self.0[at..at + end]).into_owned()
    }

    fn section(&self, name: &str) -> Option<usize> {
        (0..self.word(0x3c, 2)).find(|&index| self.name(index) == name)
    }
}

/// The text of one function in a listing, from its label to its `.size`.
fn body<'a>(text: &'a str, name: &str) -> &'a str {
    let from = text.find(&format!("\n{name}:\n")).unwrap_or_else(|| panic!("{name} in\n{text}"));
    text[from..].split("\t.size\t").next().unwrap_or("")
}

/// Whether that function's hook calls `hook`, and the section it is listed in, if any.
fn hook<'a>(text: &'a str, name: &str) -> (String, Option<&'a str>) {
    let body = body(text, name);
    let call = body
        .lines()
        .find(|line| line.trim_start().starts_with("call"))
        .unwrap_or_else(|| panic!("{name} calls nothing:\n{body}"));
    let callee = call.trim_start()["call".len()..].trim().trim_end_matches("@PLT").to_owned();
    let listed = body.lines().find_map(|line| {
        let rest = line.strip_prefix("\t.section\t")?;
        rest.strip_suffix(",\"a\",@progbits")
    });
    (callee, listed)
}

/// Three functions, two of which name their own hook or their own list, as the kernel does for the
/// code it patches some other way.
const NAMED: &str = "\
__attribute__((fentry_name(\"hook\"))) int one(int x) { return x + 1; }
__attribute__((__fentry_section__(\"calls\"))) int two(int x) { return x + 2; }
int three(int x) { return x + 3; }
";

/// `fentry_name` is the function the call goes to and `fentry_section` the section the call is
/// listed in, which it is whether or not `-mrecord-mcount` was given; the flags of the same names
/// do the same for every function that did not say, as in gcc.
#[test]
fn a_function_can_name_its_own_hook_and_its_own_list() {
    let text = asm("named", &["-pg", "-mfentry"], NAMED);
    assert_eq!(hook(&text, "one"), ("hook".to_owned(), None), "{text}");
    assert_eq!(hook(&text, "two"), ("__fentry__".to_owned(), Some("calls")), "{text}");
    assert_eq!(hook(&text, "three"), ("__fentry__".to_owned(), None), "{text}");

    let text = asm("recorded", &["-pg", "-mfentry", "-mrecord-mcount"], NAMED);
    assert_eq!(hook(&text, "one"), ("hook".to_owned(), Some("__mcount_loc")), "{text}");
    assert_eq!(hook(&text, "two"), ("__fentry__".to_owned(), Some("calls")), "{text}");
    assert_eq!(hook(&text, "three"), ("__fentry__".to_owned(), Some("__mcount_loc")), "{text}");

    let flags = ["-pg", "-mfentry", "-mrecord-mcount", "-mfentry-name=all", "-mfentry-section=gs"];
    let text = asm("flags", &flags, NAMED);
    assert_eq!(hook(&text, "one"), ("hook".to_owned(), Some("gs")), "{text}");
    assert_eq!(hook(&text, "two"), ("all".to_owned(), Some("calls")), "{text}");
    assert_eq!(hook(&text, "three"), ("all".to_owned(), Some("gs")), "{text}");

    // The section from the command line lists nothing on its own, as in gcc.
    let text = asm("unlisted", &["-pg", "-mfentry", "-mfentry-section=gs"], NAMED);
    assert_eq!(hook(&text, "three"), ("__fentry__".to_owned(), None), "{text}");

    // The later hook is named by it too, and a function that wants no hook gets none.
    let text = asm("late", &["-pg", "-mno-fentry"], NAMED);
    assert_eq!(hook(&text, "one").0, "hook", "{text}");
    let quiet = "__attribute__((no_instrument_function, fentry_name(\"hook\"))) void q(void) {}\n";
    let text = asm("quiet", &["-pg", "-mfentry"], quiet);
    assert!(!body(&text, "q").contains("call"), "{text}");
}

/// The last name said is the one taken, from a declaration after the definition as well, and the
/// nop of `-mnop-mcount` is listed in the section named.
#[test]
fn the_last_name_said_is_the_one_taken() {
    let source = "\
int f(int) __attribute__((fentry_name(\"a\")));
int f(int x) { return x; }
int f(int) __attribute__((fentry_name(\"b\")));
__attribute__((fentry_name(\"c\"), fentry_name(\"d\"))) int g(int x) { return x; }
";
    let text = asm("last", &["-pg", "-mfentry"], source);
    assert_eq!(hook(&text, "f").0, "b", "{text}");
    assert_eq!(hook(&text, "g").0, "d", "{text}");

    let flags = ["-pg", "-mfentry", "-fno-pie", "-mnop-mcount"];
    let text = asm("nop-named", &flags, NAMED);
    let two = body(&text, "two");
    assert!(two.contains("\t.byte\t0x0f, 0x1f, 0x44, 0x00, 0x00\n"), "{text}");
    assert!(two.contains("\t.section\tcalls,\"a\",@progbits"), "{text}");
}

/// What gcc warns about and what it refuses, in its words: a name on anything but a function, or
/// not a string, is ignored with a warning, and the wrong number of names is an error.
#[test]
fn the_names_are_checked_in_gcc_s_words() {
    for (source, said) in [
        ("int v __attribute__((fentry_name(\"h\")));\n", "'fentry_name' attribute ignored"),
        ("typedef void t(void) __attribute__((fentry_section(\"s\")));\n", "'fentry_section' attribute ignored"),
        ("__attribute__((fentry_name(1))) void f(void) {}\n", "'fentry_name' attribute ignored"),
        ("__attribute__((fentry_section(\"\"))) void f(void) {}\n", "'fentry_section' attribute ignored"),
    ] {
        let (ok, _, err) = run("warned", TARGET, &["-pg"], source);
        assert!(ok, "{source}\n{err}");
        assert!(err.contains(said), "{source}\nwanted {said:?}, got:\n{err}");
    }
    for (source, said) in [
        ("__attribute__((fentry_name)) void f(void) {}\n", "wrong number of arguments specified for 'fentry_name' attribute"),
        ("__attribute__((fentry_section(\"a\", \"b\"))) void f(void) {}\n", "expected 1, found 2"),
    ] {
        let (ok, _, err) = run("refused", TARGET, &["-pg"], source);
        assert!(!ok, "{source}");
        assert!(err.contains(said), "{source}\nwanted {said:?}, got:\n{err}");
    }
    let (ok, _, err) = run("plain", TARGET, &["-pg"], NAMED);
    assert!(ok && err.is_empty(), "{err}");
}

/// The object has the section named, allocated, with the address of the one call listed there,
/// and `__mcount_loc` keeps the others.
#[test]
fn the_object_lists_the_call_in_the_section_named() {
    let path = fixture("obj-named", NAMED);
    let object = path.with_extension("o");
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "-pg", "-mfentry", "-mrecord-mcount", "-c", "-o"])
        .arg(&object)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    let bytes = std::fs::read(&object).expect("the object was written");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let elf = Elf(&bytes);
    let calls = elf.section("calls").expect("the section named");
    assert_eq!((elf.kind(calls), elf.flags(calls) & 2, elf.size(calls)), (1, 2, 8));
    assert!(elf.section(".relacalls").is_some(), "its relocation");
    let loc = elf.section("__mcount_loc").expect("the others' section");
    assert_eq!(elf.size(loc), 16);
}
