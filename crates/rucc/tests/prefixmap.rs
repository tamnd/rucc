//! What the prefix mapping flags rewrite, which is `__FILE__` and nothing else yet.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6.
//!
//! `-ffile-prefix-map=`, `-fmacro-prefix-map=`, `-fdebug-prefix-map=` and `-fprofile-prefix-map=`
//! exist so that a package built in `/build/thing-1.2` and the same package built in
//! `/home/someone/thing-1.2` produce the same bytes. There are three lists because there are three
//! places a path can reach the output: a string literal the preprocessor made, the debug
//! information, and the profile data. This compiler has only the first of those, so only the macro
//! list acts and the other two are recorded for the work that will read them.
//!
//! The line that matters is which paths are rewritten and which are left alone, because getting it
//! wrong in either direction is bad in a different way. Rewriting too little ships the build
//! directory's name inside every `assert` message in the binary. Rewriting too much makes an error
//! message name a file the user cannot open and makes `-E` output that no longer compiles. gcc 16
//! draws the line in one place and this file is where that place is written down.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The source, which asks for all three spellings of the file so that the one gcc leaves alone can
/// be told apart from the two it rewrites.
const SOURCE: &str = "\
const char *file = __FILE__;
const char *base = __BASE_FILE__;
const char *name = __FILE_NAME__;
#include \"beneath.h\"
";

/// The header, so that a path the search found rather than a path the command line named is asked
/// about too. A build maps its source root and expects its own headers to be under it.
const HEADER: &str = "const char *header = __FILE__;\n";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file. The nested directory is the point: a mapping rewrites the front of a path, so there
/// has to be something behind the front for it to leave alone.
fn fixture(what: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("rucc-pm-{}-{what}", std::process::id()));
    let dir = root.join("under");
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    std::fs::write(dir.join("beneath.h"), HEADER).expect("the header can be written");
    root
}

/// The fixture's root as the command line spells it, which is the string the flags are written
/// against. Taken as text once here because every assertion below is about the text of a path.
fn spelling(path: &Path) -> String {
    path.to_str().expect("a temporary directory has a name we can write").to_string()
}

/// What the compiler said for the fixture under those flags, both halves of it, so that a test
/// about a diagnostic and a test about output can use the same call.
fn run(what: &str, flags: &[&str], form: &str) -> (String, String) {
    let root = fixture(what);
    let source = root.join("under").join("one.c");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([form, "-o", "-"])
        .args(flags)
        .arg(&source)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&root);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The string literal one of those declarations was given, with the separators as one forward
/// slash each so that an assertion about a path reads the same on both kinds of host.
///
/// A rewrite replaces the front of a native path and leaves everything behind it alone, so the
/// answer holds this host's separator, and `__FILE__` escapes a backslash, so there are two of them
/// in the text for every one in the path. Neither of those is what any test here is about.
fn literal(text: &str, which: &str) -> String {
    let open = format!("*{which} = \"");
    let at = text.find(&open).unwrap_or_else(|| panic!("`{which}` is declared: {text}"));
    let rest = &text[at + open.len()..];
    slashes(&rest[..rest.find('"').expect("the literal is closed")])
}

/// Any spelling of a path as forward slashes, doubled backslashes included.
fn slashes(text: &str) -> String {
    text.replace("\\\\", "/").replace('\\', "/")
}

/// A path this host's own way, which is what a rewrite produces and what lands in an object.
fn native(parts: &[&str]) -> String {
    parts.iter().collect::<PathBuf>().to_str().expect("the parts are plain names").to_string()
}

#[test]
fn the_macro_map_rewrites_the_file_and_the_base_file_and_leaves_the_last_component_alone() {
    let root = fixture("macro");
    let old = spelling(&root);
    let source = root.join("under").join("one.c");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-E", "-o", "-"])
        .arg(format!("-fmacro-prefix-map={old}=SRC"))
        .arg(&source)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&root);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);

    assert_eq!(literal(&text, "file"), "SRC/under/one.c", "the file the source is in");
    assert_eq!(literal(&text, "base"), "SRC/under/one.c", "and the file at the bottom");
    assert_eq!(literal(&text, "header"), "SRC/under/beneath.h", "and one the search found");

    // And not the last component, because a mapping rewrites the front of a path and this is what
    // is left when the front has been taken off. gcc leaves this macro alone for the same reason:
    // a build asking for a name with no directories in it has already got what the flag is for.
    assert_eq!(literal(&text, "name"), "one.c", "the last component is not a path");

    // Nor the line markers, which are how the rest of the compilation and anything reading this
    // output find the file again. A mapped marker would be output that no longer compiles.
    let markers: Vec<&str> = text.lines().filter(|line| line.starts_with("# ")).collect();
    assert!(!markers.is_empty(), "there are markers to check: {text}");
    assert!(markers.iter().any(|line| slashes(line).contains(&slashes(&old))), "{markers:?}");
    assert!(
        !markers.iter().any(|line| line.contains("SRC")),
        "a marker was rewritten: {markers:?}"
    );
}

#[test]
fn the_file_map_is_the_three_of_them_and_the_other_two_touch_no_macro() {
    let root = fixture("which");
    let old = spelling(&root);
    let source = root.join("under").join("one.c");
    let ask = |flag: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["-E", "-o", "-"])
            .arg(format!("{flag}={old}=SRC"))
            .arg(&source)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{flag}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // The one that names everything does what the one that names macros does.
    assert_eq!(literal(&ask("-ffile-prefix-map"), "file"), "SRC/under/one.c");

    // And the two that name an output this compiler does not have yet leave the macro alone, which
    // is what gcc does: `-fdebug-prefix-map=` has never touched `__FILE__`. They are taken rather
    // than refused because there is no debug information and no profile data for them to rewrite,
    // so taking them promises nothing that is not kept. The day either of those arrives, the list
    // is already sitting in the options waiting to be read.
    for flag in ["-fdebug-prefix-map", "-fprofile-prefix-map"] {
        let whole = format!("{}/under/one.c", slashes(&old));
        assert_eq!(literal(&ask(flag), "file"), whole, "{flag}");
    }
}

#[test]
fn a_diagnostic_names_the_file_the_way_it_was_opened() {
    let root = fixture("says");
    let old = spelling(&root);
    let source = root.join("under").join("one.c");
    std::fs::write(&source, "int missing = ;\n").expect("the fixture can be rewritten");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-c", "-o", "-"])
        .arg(format!("-ffile-prefix-map={old}=SRC"))
        .arg(&source)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&root);
    let said = String::from_utf8_lossy(&out.stderr);

    // The mapping is about what goes in the output, not about what is said to the person running
    // the compiler, and an editor that jumped to `SRC/under/one.c` would find nothing there. gcc
    // reports the real path under all four flags and so does this.
    assert!(!out.status.success(), "the fixture does not compile: {said}");
    let whole = native(&[&old, "under", "one.c"]);
    assert!(said.contains(&whole), "the real path is named: {said}");
    assert!(!said.contains("SRC"), "the mapped path was said instead: {said}");
}

#[test]
fn the_mapped_name_is_what_reaches_the_object() {
    // The whole point of the flag, asserted on the bytes that ship rather than on the `-E` text:
    // the build directory's name must not be anywhere in the object, and the name that replaced it
    // must. The target is written down rather than taken from the host, because an object is only
    // produced for the one this compiler has a back end for.
    let root = fixture("ships");
    let old = spelling(&root);
    let source = root.join("under").join("one.c");
    let object = root.join("one.o");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-c"])
        .arg(format!("-ffile-prefix-map={old}=SRC"))
        .args(["-o".as_ref(), object.as_os_str()])
        .arg(&source)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let bytes = std::fs::read(&object).expect("the object was written");
    let _ = std::fs::remove_dir_all(&root);

    let has = |what: &str| bytes.windows(what.len()).any(|run| run == what.as_bytes());
    assert!(has(&native(&["SRC", "under", "one.c"])), "the mapped name is in the object");
    assert!(!has(&old), "the build directory is not");
}

#[test]
fn nothing_that_was_not_asked_for_is_rewritten() {
    // No flag at all, which is the case every build that does not care about this is in, and the
    // one where a bug in the matching would show up as a path silently losing its front.
    let (text, said) = run("plain", &[], "-E");
    assert!(said.is_empty(), "{said}");
    assert!(slashes(&text).contains("/under/one.c"), "the path came through whole: {text}");

    // And a flag that matches nothing, since the answer for a path the rewrites say nothing about
    // has to be the path, not an empty string and not the first rewrite applied to the front of
    // something else.
    let root = fixture("elsewhere");
    let source = root.join("under").join("one.c");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-E", "-o", "-", "-ffile-prefix-map=/somewhere/else=SRC"])
        .arg(&source)
        .output()
        .expect("the compiler is built before its own tests run");
    let old = spelling(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let whole = format!("{}/under/one.c", slashes(&old));
    assert_eq!(literal(&text, "file"), whole, "no rewrite matched");
}
