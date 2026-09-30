//! What `-Wno-`, `-Werror=` and `-Wno-error=` of a warning's group do to it, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.1, and #485.
//!
//! The build this is for is `-Werror -Wno-deprecated-declarations`, and every variant of it: a
//! project that decided to live with one kind of warning and fail on the rest. Before warnings had
//! groups the `-Wno-` was accepted and did nothing, so the one warning the project had decided to
//! live with was the one that stopped the build.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A warning in the `attributes` group: gcc says "'constructor' attribute ignored" about an
/// object, since only a function can be called before `main`.
const GROUPED: &str = "int x __attribute__((constructor));\n";

/// A warning gcc puts in no group, which only `-w` silences and only `-Werror` makes fatal.
const PLAIN: &str = "int;\n";

fn fixture(source: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-warning-groups-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compile succeeded, and what it said.
fn compile(source: &str, args: &[&str]) -> (bool, String) {
    let path = fixture(source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-linux-gnu", "-S", "-o", "-"])
        .args(args)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn a_grouped_warning_names_its_group() {
    let (ok, said) = compile(GROUPED, &[]);
    assert!(ok, "{said}");
    assert!(said.contains("warning: 'constructor' attribute ignored"), "{said}");
    assert!(said.contains("[-Wattributes]"), "{said}");
}

#[test]
fn turning_the_group_off_silences_it_and_the_last_word_wins() {
    let (ok, said) = compile(GROUPED, &["-Wno-attributes"]);
    assert!(ok && said.is_empty(), "{said}");
    let (ok, said) = compile(GROUPED, &["-Wno-attributes", "-Wattributes"]);
    assert!(ok && said.contains("[-Wattributes]"), "{said}");
    // Off wins over fatal when it comes after, so there is nothing to fail on.
    let (ok, said) = compile(GROUPED, &["-Werror=attributes", "-Wno-attributes"]);
    assert!(ok && said.is_empty(), "{said}");
}

#[test]
fn werror_of_the_group_is_fatal_and_says_so() {
    let (ok, said) = compile(GROUPED, &["-Werror=attributes"]);
    assert!(!ok, "{said}");
    assert!(said.contains("error: 'constructor' attribute ignored"), "{said}");
    assert!(said.contains("[-Werror=attributes]"), "{said}");
    // Only that group: a warning in no group stays a warning.
    let (ok, said) = compile(PLAIN, &["-Werror=attributes"]);
    assert!(ok && said.contains("warning:"), "{said}");
}

#[test]
fn werror_with_one_group_let_off_fails_on_everything_else() {
    let args = ["-Werror", "-Wno-error=attributes"];
    let (ok, said) = compile(GROUPED, &args);
    assert!(ok, "{said}");
    assert!(said.contains("warning: 'constructor' attribute ignored"), "{said}");
    let (ok, said) = compile(PLAIN, &args);
    assert!(!ok, "{said}");
}

#[test]
fn werror_with_one_group_turned_off_is_the_build_this_is_for() {
    let (ok, said) = compile(GROUPED, &["-Werror", "-Wno-attributes"]);
    assert!(ok && said.is_empty(), "{said}");
    let (ok, said) = compile(PLAIN, &["-Werror", "-Wno-attributes"]);
    assert!(!ok, "{said}");
}

#[test]
fn wno_error_takes_werror_back() {
    let (ok, said) = compile(PLAIN, &["-Werror", "-Wno-error"]);
    assert!(ok && said.contains("warning:"), "{said}");
}
