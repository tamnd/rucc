//! The three flags that change the ABI rather than the code.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6.
//!
//! `-fsigned-char`, `-funsigned-char` and `-fshort-enums` change what a type is, so they change
//! the size of a structure, the meaning of a comparison and the calling convention of anything
//! that passes one. That makes them different from every other flag in that section: a program
//! built half with them and half without is not a slower program, it is one whose two halves
//! disagree about memory, and the disagreement is silent because nothing about it is a link error.
//!
//! So what is asserted here is what a program can see, rather than what the driver recorded. Each
//! case is a `_Static_assert` that holds under one answer and fails under the other, compiled both
//! ways, so a flag that was taken and then dropped somewhere between the driver and the type
//! system fails here rather than in somebody's object file. The values are gcc 16's, measured on
//! the linux box in both `-std=c17` and `-std=c23`, which agree.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because two of these three are
/// about disagreeing with the target's own answer and the host's answer would hide that.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The target whose ABI says a plain `char` is unsigned, which is the other half of the same
/// question. AAPCS64 says unsigned and Apple overrides it back, so Linux's arm64 is the one to
/// ask.
const UNSIGNED_TARGET: &str = "aarch64-unknown-linux-gnu";

/// What `-fshort-enums` is, as sizes a program can assert.
///
/// The pairs are the ones that show the two decisions the rule makes and the order it makes them
/// in. `200` and `-200` are the same magnitude and are one byte and two, because the signedness is
/// settled by whether anything is negative before the width is settled by what fits. `300` is two
/// bytes unsigned where `200` is one. The last two are past what a narrower type holds and are the
/// same size under both answers, which is why a program whose enumerators are all large cannot
/// tell the flag was given. The structure is what the flag costs and what it is for: a `char` and
/// an enumeration is eight bytes by default and two here.
const ENUMS: &str = "\
enum one { A = 1 };
enum minus_one { B = -1 };
enum two_hundred { C = 200 };
enum minus_two_hundred { D = -200 };
enum three_hundred { E = 300 };
enum four_billion { F = 4294967295u };
enum enormous { G = 9223372036854775807L, H };
struct holder { char c; enum one e; };
_Static_assert(sizeof(enum one) == (SHORT_ENUMS ? 1 : 4), \"one\");
_Static_assert(sizeof(enum minus_one) == (SHORT_ENUMS ? 1 : 4), \"minus one\");
_Static_assert(sizeof(enum two_hundred) == (SHORT_ENUMS ? 1 : 4), \"two hundred\");
_Static_assert(sizeof(enum minus_two_hundred) == (SHORT_ENUMS ? 2 : 4), \"minus two hundred\");
_Static_assert(sizeof(enum three_hundred) == (SHORT_ENUMS ? 2 : 4), \"three hundred\");
_Static_assert(sizeof(enum four_billion) == 4, \"four billion\");
_Static_assert(sizeof(enum enormous) == 8, \"enormous\");
_Static_assert(_Alignof(enum one) == (SHORT_ENUMS ? 1 : 4), \"alignment follows the size\");
_Static_assert(sizeof(struct holder) == (SHORT_ENUMS ? 2 : 8), \"what it costs\");
";

/// An enumeration the program wrote an underlying type for, which the flag does not touch.
///
/// This is the escape hatch and it has to keep working, since it is the only way to write down a
/// size and have it survive a command line that says otherwise. It is separate from the block
/// above because the spelling is C23's and the block above is compiled under both dialects.
const FIXED: &str = "\
enum written : int { A = 1 };
_Static_assert(sizeof(enum written) == 4, \"what the program wrote\");
";

/// What the signedness of a plain `char` is, as things a program can assert.
///
/// A plain `char` is a third type whichever answer is given, distinct from both `signed char` and
/// `unsigned char` wherever types are compared, so the generic selection is the same line under
/// both and is here to say so. `__CHAR_UNSIGNED__` is gcc's spelling of the answer and is defined
/// only for the unusual one, so a header that tests it sees what was decided.
const CHARS: &str = "\
_Static_assert(((char)-1 < 0) ? CHAR_SIGNED : !CHAR_SIGNED, \"the range of a plain char\");
_Static_assert(_Generic((char)0, char: 1, default: 0), \"a plain char is still its own type\");
#ifdef __CHAR_UNSIGNED__
_Static_assert(!CHAR_SIGNED, \"the macro is defined when it is unsigned\");
#else
_Static_assert(CHAR_SIGNED, \"and not when it is signed\");
#endif
";

/// The fixture, under a directory of its own so that two of these running at once do not write
/// the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-abi-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Compiles one fixture as far as the IR, which is far enough for a static assertion to be
/// folded and not far enough to need a linker.
///
/// The answer is what the compiler said rather than what it produced, since every case here is a
/// `_Static_assert` and the question is whether it held.
fn compiles(what: &str, source: &str, target: &str, flags: &[&str]) -> (bool, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["--emit=ir", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The source with the answer the assertions are written against filled in.
fn under(source: &str, name: &str, yes: bool) -> String {
    format!("#define {name} {}\n{source}", u32::from(yes))
}

#[test]
fn an_enumeration_is_as_small_as_it_can_be_where_the_flag_was_given() {
    for std in ["-std=c17", "-std=c23"] {
        for (flags, short) in [(vec![std], false), (vec![std, "-fshort-enums"], true)] {
            let source = under(ENUMS, "SHORT_ENUMS", short);
            let (ok, stderr) = compiles("enums", &source, TARGET, &flags);
            assert!(ok, "{flags:?}: {stderr}");

            // And the other answer's assertions fail, which is what says the first lot held
            // because the sizes are right rather than because the assertions are vacuous.
            let other = under(ENUMS, "SHORT_ENUMS", !short);
            let (ok, _) = compiles("enums-other", &other, TARGET, &flags);
            assert!(!ok, "{flags:?} accepted the other answer's sizes");
        }
    }
}

#[test]
fn an_enumeration_the_program_wrote_a_type_for_keeps_it() {
    for flags in [vec![], vec!["-fshort-enums"]] {
        let (ok, stderr) = compiles("fixed", FIXED, TARGET, &flags);
        assert!(ok, "{flags:?}: {stderr}");
    }
}

#[test]
fn a_plain_char_has_the_range_the_command_line_asked_for() {
    // Both spellings of each answer, since gcc reads the negative of one as the other, and the
    // last one written, since a build that sets one globally and the other for a directory is
    // relying on that.
    let signed = [
        vec!["-fsigned-char"],
        vec!["-fno-unsigned-char"],
        vec!["-funsigned-char", "-fsigned-char"],
    ];
    let unsigned = [
        vec!["-funsigned-char"],
        vec!["-fno-signed-char"],
        vec!["-fsigned-char", "-funsigned-char"],
    ];
    for (cases, is_signed) in [(signed, true), (unsigned, false)] {
        for flags in cases {
            let source = under(CHARS, "CHAR_SIGNED", is_signed);
            let (ok, stderr) = compiles("chars", &source, TARGET, &flags);
            assert!(ok, "{flags:?}: {stderr}");
        }
    }
}

#[test]
fn the_target_answers_where_the_command_line_did_not() {
    // Two targets that disagree, so a compiler that had quietly picked one answer for both would
    // fail one of these. The flag then overrides each of them in the direction it is not already
    // going, which is the thing a cross build needs and the reason the option is not a plain bool.
    for (target, is_signed) in [(TARGET, true), (UNSIGNED_TARGET, false)] {
        let source = under(CHARS, "CHAR_SIGNED", is_signed);
        let (ok, stderr) = compiles("target", &source, target, &[]);
        assert!(ok, "{target}: {stderr}");

        let flag = if is_signed { "-funsigned-char" } else { "-fsigned-char" };
        let flipped = under(CHARS, "CHAR_SIGNED", !is_signed);
        let (ok, stderr) = compiles("flipped", &flipped, target, &[flag]);
        assert!(ok, "{target} {flag}: {stderr}");
    }
}
