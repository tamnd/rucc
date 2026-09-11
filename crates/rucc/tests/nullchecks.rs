//! What a test of a pointer against null survives, which is everything.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6.
//!
//! `-fno-delete-null-pointer-checks` asks a compiler not to conclude that a pointer is not null
//! from the fact that the program dereferenced it. gcc draws that conclusion by default and the
//! kernel turns it off, because a kernel dereferences addresses that are null on purpose and
//! because the conclusion turns a test somebody wrote into nothing. This compiler never draws it,
//! so the flag is a description of what happens here rather than a request being granted, the same
//! shape of thing `-fno-strict-aliasing` and `-fPIC` are.
//!
//! That is only allowed to be the answer while it is true, which is what this file is for. Each
//! shape below is one gcc folds away with the conclusion in hand, and each is compiled at the
//! optimization levels where a pass would take it. The day one of these stops having its test, the
//! flag has stopped being a description and whoever made that change has to make the flag turn the
//! conclusion off in the same change, which is the rule `spec/04-driver-and-cli.md` section 4.1
//! states for exactly this case.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the width of an address is
/// the same wherever this runs.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The three shapes, each of which gcc answers with the conclusion rather than with the test.
///
/// The first is the conclusion itself: the load says the pointer is not null, so the test after it
/// is folded. The second is the one the kernel's own `container_of` is: the address of a member is
/// the pointer plus an offset, and an offset from a pointer is inside an object or one past it, so
/// the sum is not null either. The third is a call that was handed the pointer, which gcc concludes
/// nothing from on its own but which a callee marked as taking a pointer that is never null would
/// settle.
const SHAPES: &str = "\
extern void taken(int *);
int deref_then_check(int *p) { int v = *p; if (p) return v; return -1; }
struct s { int a; int b; };
int member_then_check(struct s *p) { int *q = &p->b; if (q) return 1; return 0; }
int call_then_check(int *p) { taken(p); if (p) return 1; return 0; }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-null-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SHAPES).expect("the fixture can be written");
    path
}

/// The IR for the shapes at that optimization level, under those flags.
fn ir(what: &str, level: &str, flags: &[&str]) -> String {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args([level, "--emit=ir"])
        .args(["-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The instructions of one function, without the name of it or the braces around it.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("func @{name}(");
    text.lines()
        .map(str::trim)
        .skip_while(|line| !line.starts_with(&open))
        .skip(1)
        .take_while(|line| *line != "}")
        .filter(|line| !line.is_empty())
        .collect()
}

/// Whether that function still asks the question the source asked.
///
/// Both halves are wanted. A comparison nothing branches on is a comparison a later pass will
/// remove, and a branch on something else is not this test, so the answer is that there is a
/// comparison and that something goes two ways.
fn tests_the_pointer(text: &str, name: &str) -> bool {
    let body = body(text, name);
    body.iter().any(|line| line.contains("icmp")) && body.iter().any(|line| line.contains("br_if"))
}

#[test]
fn a_pointer_that_was_dereferenced_is_still_tested_afterwards() {
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let text = ir("levels", level, &[]);
        for name in ["deref_then_check", "member_then_check", "call_then_check"] {
            assert!(tests_the_pointer(&text, name), "{level} on {name}: {:?}", body(&text, name));
        }
    }
}

/// And the flag changes nothing, which is what makes it a description rather than a request.
///
/// Asserted as the whole module coming out the same rather than as the tests surviving, because a
/// compiler where the flag did something would have two answers and this has one. Both spellings
/// are compared, since the one that asks for the conclusion is the one a build writes by accident
/// through `-O2` and it has to be the same answer as well.
#[test]
fn neither_spelling_of_the_flag_changes_what_comes_out() {
    // Without the line that names the module, which is the path the fixture was written to and is
    // different for each of these because two of them running at once must not share a file.
    let module = |what: &str, flags: &[&str]| {
        let text = ir(what, "-O2", flags);
        text.lines().filter(|line| !line.starts_with("; ModuleID")).collect::<Vec<_>>().join("\n")
    };
    let plain = module("plain", &[]);
    assert_eq!(module("off", &["-fno-delete-null-pointer-checks"]), plain);
    assert_eq!(module("on", &["-fdelete-null-pointer-checks"]), plain);
}
