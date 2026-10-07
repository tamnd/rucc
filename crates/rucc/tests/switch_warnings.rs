//! `-Wswitch`, `-Wswitch-enum`, `-Wswitch-default` and `-Wswitch-bool`, end to end: what gcc 13
//! says about a whole `switch` once its body has been read, in its words, its order and its
//! places, under the flags it says it under.

use std::path::{Path, PathBuf};
use std::process::Command;

const X86_64: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-switch-warn-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished and what it said, for one source under those flags.
fn compile(what: &str, flags: &[&str], source: &str) -> (bool, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={X86_64}");
    let mut args = vec![target.as_str(), "-std=c23", "-S", "-o", "-"];
    args.extend_from_slice(flags);
    args.push("a.c");
    let out = run(&dir, &args);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The lines about switches, which are the ones with these codes.
fn switch_lines(said: &str) -> Vec<&str> {
    said.lines()
        .filter(|line| ["E0852", "E0853", "E0854", "E0855"].iter().any(|code| line.contains(code)))
        .collect()
}

/// One `switch` a line, each something gcc 13 has a rule for: enumerators left out with and
/// without a `default`, a case range whose ends are enumerators and one whose ends are not, the
/// enumerators marked `unused` and `[[maybe_unused]]`, the typedefs gcc names the enumeration
/// by, a controlling expression that folds, and the truth values a `switch` is worth saying about
/// or not, a cast to `int` or a `default` that only 0 can take being the last of those.
const SWITCHES: &str = "enum e { A, B, C, D = 10 };\n\
    typedef enum { X, Y } t;\n\
    enum u { P, Q __attribute__((unused)), R [[maybe_unused]], S };\n\
    typedef enum e2 { M, N } t2;\n\
    typedef t2 t3;\n\
    enum { K, L } anon;\n\
    enum lone : int { O };\n\
    int f(enum e v, t w, enum u z, _Bool b, int i, t2 a, t3 c, enum lone n) {\n\
      switch (v) { case A: case B: return 1; }\n\
      switch (v) { case A: return 1; default: return 2; }\n\
      switch (v) { case A ... C: case 5: return 3; }\n\
      switch (w) { case X: case 7: return 4; }\n\
      switch (z) { case P: return 5; }\n\
      switch (b) { case 0: case 1: default: return 6; }\n\
      switch (b) { case 0: case 1: return 6; }\n\
      switch (i < 3) { case 0: case 1: default: return 7; }\n\
      switch ((enum e)2) { case B: return 8; }\n\
      switch ((enum e)i) { case A: case B: case C: case D: case 4 ... 5: return 9; }\n\
      switch (i) { case 1: return 10; }\n\
      switch (a) { case M: case 9: return 11; }\n\
      switch (anon) { case K: case -1: return 12; }\n\
      switch (c) { case M: case N: case 3: return 13; }\n\
      switch ((const enum e2)i) { case M: case 4: return 14; }\n\
      switch ((int)b) { case 0: default: return 15; }\n\
      switch (i, !i) { case 0: default: return 16; }\n\
      switch (b) { case 0: case 2: return 17; }\n\
      switch (n) { case O: return 18; }\n\
      switch (v) { case 11: case B ... D: default: return 19; }\n\
      return 0;\n\
    }\n";

/// What gcc 13 says about [`SWITCHES`] under `-Wall -Wswitch-enum -Wswitch-default`, line for
/// line, with this compiler's code where gcc gives the option.
const SAID: &[&str] = &[
    "a.c:9:3: warning: switch missing default case [E0854]",
    "a.c:9:3: warning: enumeration value 'C' not handled in switch [E0852]",
    "a.c:9:3: warning: enumeration value 'D' not handled in switch [E0852]",
    "a.c:10:3: warning: enumeration value 'B' not handled in switch [E0853]",
    "a.c:10:3: warning: enumeration value 'C' not handled in switch [E0853]",
    "a.c:10:3: warning: enumeration value 'D' not handled in switch [E0853]",
    "a.c:11:3: warning: switch missing default case [E0854]",
    "a.c:11:3: warning: enumeration value 'D' not handled in switch [E0852]",
    "a.c:11:30: warning: case value '5' not in enumerated type 'enum e' [E0852]",
    "a.c:12:3: warning: switch missing default case [E0854]",
    "a.c:12:3: warning: enumeration value 'Y' not handled in switch [E0852]",
    "a.c:12:24: warning: case value '7' not in enumerated type 't' [E0852]",
    "a.c:13:3: warning: switch missing default case [E0854]",
    "a.c:13:3: warning: enumeration value 'S' not handled in switch [E0852]",
    "a.c:14:3: warning: switch condition has boolean value [E0855]",
    "a.c:15:3: warning: switch missing default case [E0854]",
    "a.c:16:3: warning: switch condition has boolean value [E0855]",
    "a.c:17:3: warning: switch missing default case [E0854]",
    "a.c:17:3: warning: enumeration value 'C' not handled in switch [E0852]",
    "a.c:18:3: warning: switch missing default case [E0854]",
    "a.c:18:56: warning: case value '4' not in enumerated type 'enum e' [E0852]",
    "a.c:18:56: warning: case value '5' not in enumerated type 'enum e' [E0852]",
    "a.c:19:3: warning: switch missing default case [E0854]",
    "a.c:20:3: warning: switch missing default case [E0854]",
    "a.c:20:3: warning: enumeration value 'N' not handled in switch [E0852]",
    "a.c:20:24: warning: case value '9' not in enumerated type 't2' {aka 'enum e2'} [E0852]",
    "a.c:21:3: warning: switch missing default case [E0854]",
    "a.c:21:3: warning: enumeration value 'L' not handled in switch [E0852]",
    "a.c:21:27: warning: case value '4294967295' not in enumerated type [E0852]",
    "a.c:22:3: warning: switch missing default case [E0854]",
    "a.c:22:32: warning: case value '3' not in enumerated type 't3' {aka 'enum e2'} [E0852]",
    "a.c:23:3: warning: switch missing default case [E0854]",
    "a.c:23:3: warning: enumeration value 'N' not handled in switch [E0852]",
    "a.c:23:39: warning: case value '4' not in enumerated type 'enum e2' [E0852]",
    "a.c:26:3: warning: switch missing default case [E0854]",
    "a.c:26:3: warning: switch condition has boolean value [E0855]",
    "a.c:27:3: warning: switch missing default case [E0854]",
    "a.c:28:3: warning: enumeration value 'A' not handled in switch [E0853]",
    "a.c:28:16: warning: case value '11' not in enumerated type 'enum e' [E0852]",
];

/// The lines of [`SAID`] with one of these codes.
fn said_with(codes: &[&str]) -> Vec<&'static str> {
    SAID.iter().copied().filter(|line| codes.iter().any(|code| line.contains(code))).collect()
}

#[test]
fn a_switch_is_warned_about_in_gcc_s_words_order_and_places() {
    let (ok, said) = compile("all", &["-Wall", "-Wswitch-enum", "-Wswitch-default"], SWITCHES);
    assert!(ok, "{said}");
    assert_eq!(switch_lines(&said), SAID, "{said}");
}

/// `-Wswitch` waits for `-Wall`, `-Wswitch-enum` and `-Wswitch-default` for their own names, and
/// `-Wswitch-bool` is on until it is turned off. What `-Wswitch` says is said under
/// `-Wswitch-enum` where only that one is on, and an enumerator a `switch` with a `default`
/// leaves out is only ever worth `-Wswitch-enum`.
#[test]
fn each_warning_is_heard_under_the_flags_gcc_says_it_under() {
    for (flags, codes) in [
        (&[][..], &["E0855"][..]),
        (&["-Wall"][..], &["E0852", "E0855"][..]),
        (&["-Wswitch-enum"][..], &["E0852", "E0853", "E0855"][..]),
        (&["-Wall", "-Wno-switch", "-Wswitch-enum"][..], &["E0852", "E0853", "E0855"][..]),
        (&["-Wall", "-Wno-switch-bool"][..], &["E0852"][..]),
        (&["-Wswitch-default"][..], &["E0854", "E0855"][..]),
    ] {
        let (ok, said) = compile("flags", flags, SWITCHES);
        assert!(ok, "{flags:?}: {said}");
        assert_eq!(switch_lines(&said), said_with(codes), "{flags:?}: {said}");
    }
}

/// `-Werror=switch-enum` makes an error of what is said under `-Wswitch-enum`, which is what
/// `-Wswitch` would have said where `-Wswitch` is not on, and not where it is.
#[test]
fn an_error_of_switch_enum_follows_the_name_the_warning_was_said_under() {
    let (ok, said) = compile("error-enum", &["-Werror=switch-enum"], SWITCHES);
    assert!(!ok, "{said}");
    assert!(said.contains("a.c:11:30: error: case value '5' not in enumerated type"), "{said}");
    assert!(said.contains("a.c:9:3: error: enumeration value 'C' not handled"), "{said}");
    let (ok, said) = compile("error-wall", &["-Wall", "-Werror=switch-enum"], SWITCHES);
    assert!(!ok, "{said}");
    assert!(said.contains("a.c:11:30: warning: case value '5' not in enumerated type"), "{said}");
    assert!(said.contains("a.c:10:3: error: enumeration value 'B' not handled"), "{said}");
    assert!(said.contains("a.c:9:3: warning: enumeration value 'C' not handled"), "{said}");
}
