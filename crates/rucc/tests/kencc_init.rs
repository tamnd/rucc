//! kencc's designators with no `=` under `-fkencc`, end to end.
//!
//! Design: tamnd/rucc#4148, and 06.8 of the rucc-plan9 plan.
//!
//! kencc's `qual` in `cc.y` is `[e]` or `.tag`, and the `=` after it is optional. gcc 16 takes
//! the old form only for one array designator, as in `[3] 7`, so `.y 1` and `[0][1] 7` are errors
//! in C and are taken under `-fkencc`.

use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "typedef struct Point Point;\n\
                      struct Point { int x; int y; };\n\
                      struct Rect { Point min; Point max; };\n\
                      struct Rect r = { .min .x 1, .max { .y 2 }, .min.y = 3 };\n\
                      int a[2][2] = { [1][0] 5, [0] 1 };\n";

fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-init-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, what it wrote and what it said.
fn run(what: &str, flags: &[&str]) -> (bool, String, String) {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    (out.status.success(), text(out.stdout), text(out.stderr))
}

/// The data lines of `name` in a listing.
fn data<'a>(listing: &'a str, name: &str) -> Vec<&'a str> {
    let start = listing.find(&format!("\n{name}:\n")).expect("the object is in the listing");
    listing[start..]
        .lines()
        .skip(2)
        .take_while(|line| !line.starts_with("\t.size"))
        .map(str::trim)
        .collect()
}

#[test]
fn a_designation_can_leave_out_the_equals_sign() {
    let (ok, out, err) = run("kencc", &["-fkencc"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    assert!(err.is_empty(), "{err}");
    assert_eq!(data(&out, "r"), [".long\t1", ".long\t3", ".space\t4", ".long\t2"]);
    assert_eq!(data(&out, "a"), [".long\t1", ".space\t4", ".long\t5", ".space\t4"]);
}

#[test]
fn without_kencc_only_one_array_designator_can() {
    let (ok, _, err) = run("c", &[]);
    assert!(!ok);
    assert!(err.contains("4:27: error: expected `=`, found an integer constant"), "{err}");
    assert!(err.contains("5:24: error: expected `=`, found an integer constant"), "{err}");
    // `[0] 1` is the old GNU form, which gcc 16 takes.
    assert!(!err.contains("5:31"), "{err}");
}
