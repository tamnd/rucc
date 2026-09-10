//! Which functions get a stack protector and what one looks like, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6 and `spec/10-backend.md` section 10.7.
//!
//! Two questions and they are answered in two different crates, which is why they are checked
//! together here rather than apart in each. Which functions get one is a question about the locals
//! a function declares, so it is settled in the lowering while the types are still around. What one
//! is made of is a slot in the frame and a comparison before every return, so it is settled in the
//! back end after the allocator has finished. A test in either crate alone can be green while the
//! flag on the command line does nothing.
//!
//! The listing rather than the object, for the reason `pic.rs` beside this reads the listing: it is
//! what a person debugging this reads. That the bytes come out right is checked by the encoder's
//! own tests, which have the one address this feature adds written out in hex.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because where the word a canary is
/// copied from lives is a fact about the platform and the answer elsewhere is a different one.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The four kinds of function the levels disagree about.
///
/// `leaf` has nothing in it at all, `buf` has an array big enough for anybody, `small` has one
/// under the size the plain flag asks for, and `taken` has no array but hands the address of a
/// local to something that could write through it.
const FOUR: &str = "\
void use(void *);
int leaf(int x) { return x + 1; }
int buf(void) { char b[16]; use(b); return 0; }
int small(void) { char b[4]; use(b); return 0; }
int taken(void) { int n = 0; use(&n); return n; }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-ssp-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Runs the compiler over that source under those flags, for that target.
fn run(what: &str, target: &str, flags: &[&str], source: &str) -> std::process::Output {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    out
}

/// The assembly the compiler writes for that source under those flags.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let out = run(what, TARGET, flags, source);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Which of the four functions in the listing were given a canary.
///
/// Read off the listing rather than counted, because a count would be the same for two different
/// sets and the whole question here is which ones.
fn protected(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(name) = line.strip_suffix(':') {
            if !name.starts_with('.') {
                current = Some(name);
            }
        }
        if line.contains("%fs:40") {
            if let Some(name) = current.take() {
                out.push(name);
            }
        }
    }
    out
}

/// The plain flag protects a function with an array in it and nothing else.
#[test]
fn the_plain_flag_protects_the_functions_with_a_buffer_in_them() {
    let text = asm("plain", &["-fstack-protector"], FOUR);
    assert_eq!(protected(&text), ["buf"], "{text}");
}

/// The strong one adds the small array and the address that got away.
///
/// This is the one every distribution builds with, so it is the one worth being exact about. An
/// address-taken local is in because whatever it was handed to can write through it and nothing
/// here knows how far, which is the same argument the array makes.
#[test]
fn the_strong_flag_adds_the_small_arrays_and_the_locals_that_escape() {
    let text = asm("strong", &["-fstack-protector-strong"], FOUR);
    assert_eq!(protected(&text), ["buf", "small", "taken"], "{text}");
}

/// The last one protects everything, including a leaf with no memory in it at all.
#[test]
fn the_all_flag_protects_every_function_there_is() {
    let text = asm("all", &["-fstack-protector-all"], FOUR);
    assert_eq!(protected(&text), ["leaf", "buf", "small", "taken"], "{text}");
}

/// And nothing is protected unless something asked, which is gcc's default and this one.
#[test]
fn nothing_is_protected_unless_the_command_line_asked_for_it() {
    for flags in [&[][..], &["-fstack-protector-strong", "-fno-stack-protector"]] {
        let text = asm("off", flags, FOUR);
        assert_eq!(protected(&text), Vec::<&str>::new(), "{flags:?}: {text}");
    }
}

/// What a protected function is made of, in the order it is made of it.
///
/// The prologue reads the word out of the block the thread has to itself and puts a copy above
/// every byte a local reaches. Before the return it reads the word again and compares, and the arm
/// where the two differ calls the function that does not come back. The order matters more than
/// the mnemonics: a check written after the frame was given back would be checking a slot the
/// function no longer owns.
#[test]
fn a_protected_function_copies_the_word_and_compares_it_before_it_returns() {
    let text = asm("shape", &["-fstack-protector-strong"], FOUR);
    let body: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("buf:"))
        .take_while(|line| !line.starts_with("small:"))
        .map(str::trim)
        .collect();

    let at = |what: &str| {
        body.iter().position(|line| line.contains(what)).unwrap_or_else(|| panic!("{what}: {text}"))
    };
    // Two reads of the same address, one in the prologue and one before the return, with the store
    // into the frame behind the first of them.
    assert!(at("movq\t%fs:40") < at("__stack_chk_fail"), "{text}");
    assert!(at("__stack_chk_fail") < at("ret"), "{text}");
    assert_eq!(body.iter().filter(|line| line.contains("%fs:40")).count(), 2, "{text}");
    // And the frame is given back after the check rather than before it, so the slot the check
    // reads is still this function's when it reads it.
    assert!(at("__stack_chk_fail") < at("addq\t$"), "{text}");
}

/// A target whose protector is a different mechanism is told so rather than quietly left open.
///
/// Windows has one and it is not this one: the cookie is a global the loader writes, what goes in
/// the frame is that global exclusive-ored with the frame pointer, and the check is a call rather
/// than a comparison. Accepting the flag and emitting nothing would be the one outcome worse than
/// the error, because the build would look protected and not be.
#[test]
fn a_target_whose_protector_is_a_different_mechanism_refuses_the_flag() {
    let out = run("windows", "x86_64-pc-windows-msvc", &["-fstack-protector"], FOUR);
    assert!(!out.status.success(), "the flag does nothing on that target and it says so");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("-fstack-protector"), "{stderr}");
    // The triple as the compiler spells it back, which is the normalized one rather than the one
    // the command line wrote.
    assert!(stderr.contains("x86_64-windows-msvc"), "{stderr}");
}
