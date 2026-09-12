//! Holds the runtime support routines against the reference implementation of them.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8, which asks for the library to be held against
//! a reference over randomized inputs rather than against expectations written down beside it, and
//! which names the reference: `runtime/rucc-builtins` is the Rust crate kept for exactly this after
//! tamnd/rucc#912 decided that what ships is the C in `runtime/builtins` compiled by rucc.
//!
//! The comparison is two processes rather than two sets of symbols in one. `tests/builtins/
//! differential.c` is compiled twice by the system compiler, once linked against the archive rucc
//! wrote and once against the same routines built from Rust, and each run prints a digest per group
//! of cases. Neither program knows which side it got, which is what makes their agreement worth
//! something, and there is no renaming of symbols anywhere, which is what keeps the thing under
//! test the archive people are actually handed.
//!
//! # The trap this walked into first
//!
//! On a distribution that turns `_FORTIFY_SOURCE` on by default, `memcpy` in the harness becomes a
//! call to glibc's `__memcpy_chk` and the archive on the link line is never reached. The first run
//! of this check passed against a deliberately broken `memcpy` for that reason. So the harness is
//! compiled with the fortification explicitly off, and the script then reads the undefined symbols
//! of each program back and reports any of the four names it finds there, because a check that
//! silently tests the C library instead is worse than no check.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, root};

/// What this task is called, for the messages.
const TASK: &str = "builtins-diff";

/// The two sides, spelled the way the script prefixes their output.
const SIDES: [&str; 2] = ["ours", "reference"];

/// Builds both archives, runs the harness against each, and compares what they printed.
///
/// # Errors
///
/// [`Error::Io`] when either archive will not build or the script will not run, and
/// [`Error::Failed`] when a program could not reach the routines it was linked against, when the
/// two runs did not get through the same number of cases, or when any group of cases came out
/// differently on the two sides.
pub(crate) fn builtins_diff() -> Result<()> {
    let work = build()?;
    let runner = Runner::find("the builtins differential")?;
    let printed = runner.run(&work, "the builtins differential")?;
    compare(&printed)
}

/// Lays out the directory the runner is pointed at: both archives, the harness, and the script.
fn build() -> Result<PathBuf> {
    let work = root().join("target").join(TASK);
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let reference = reference_archive()?;
    copy(&reference, &work.join("libreference.a"))?;
    let ours = crate::builtins_archive(TRIPLE)?;
    copy(&ours, &work.join("librucc_builtins.a"))?;

    let harness = root().join("tests").join("builtins").join("differential.c");
    copy(&harness, &work.join("differential.c"))?;
    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// Builds the Rust crate as a static library for the target, into a directory of its own.
///
/// A directory of its own rather than the usual one, and this is the mistake that was worth finding
/// the hard way: `cargo rustc --crate-type staticlib` writes `librucc_builtins.a` under
/// `target/<triple>/release`, which is the same path `cargo xtask builtins` writes the C archive to.
/// Run in the other order, cargo finds its own output already there, decides nothing has changed,
/// and leaves whatever was written last in place. The first version of this check compared the C
/// archive against a stale copy of the C archive and said the two sides agreed.
///
/// # Errors
///
/// [`Error::Io`] when cargo cannot be run, and [`Error::Failed`] when the build fails, which on a
/// fresh machine is almost always the standard library for the target not being installed.
fn reference_archive() -> Result<PathBuf> {
    let into = root().join("target").join(format!("{TASK}-rust"));
    let status = Command::new("cargo")
        .args(["rustc", "-q", "-p", "rucc-builtins", "--release", "--crate-type", "staticlib"])
        .args(["--target", TRIPLE])
        .env("CARGO_TARGET_DIR", &into)
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !status.success() {
        return Err(Error::Failed {
            task: TASK,
            problems: vec![format!(
                "the reference did not build for {TRIPLE}. If the message above is about `core`, \
                 the standard library for that target is not installed: `rustup target add \
                 {TRIPLE}`"
            )],
        });
    }
    let archive = into.join(TRIPLE).join("release").join("librucc_builtins.a");
    if !archive.is_file() {
        return Err(Error::Failed {
            task: TASK,
            problems: vec![format!("cargo reported success but {} is not there", archive.display())],
        });
    }
    Ok(archive)
}

/// One file into the work directory, which is copied rather than read from the tree because the
/// runner mounts the directory read only and a container cannot reach anything outside it.
fn copy(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", from.display())))
}

/// What the runner runs.
///
/// `-fno-builtin` so the calls in the harness are calls, and the fortification off so they are
/// calls to the names the archive defines rather than to glibc's checked versions of them. `-O1`
/// rather than `-O0` because a harness nobody optimized is a harness whose loops are not the loops
/// a program would make, and the digest is computed from memory either way.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/builtins-diff
mkdir -p \"$out\"
flags=\"-O1 -fno-builtin -U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=0\"
gcc $flags -o \"$out/ours\" differential.c librucc_builtins.a || exit 1
gcc $flags -o \"$out/reference\" differential.c libreference.a || exit 1
for side in ours reference; do
    nm -u \"$out/$side\" | awk -v side=$side '/mem(cpy|move|set|cmp)/ { print side \" reached \" $NF }'
done
\"$out/ours\" | sed 's/^/ours /'
\"$out/reference\" | sed 's/^/reference /'
";

/// What one side of the run said.
#[derive(Default)]
struct Side {
    /// One digest per group, in the order the groups ran, as `("memcpy 65", "3ff0...")`.
    groups: Vec<(String, String)>,
    /// How many cases the run got through, where it got to the end.
    cases: Option<String>,
    /// The four names this side went looking for outside itself, which should be none of them.
    reached: Vec<String>,
}

/// Holds the two sides against each other.
fn compare(printed: &str) -> Result<()> {
    let (ours, theirs) = read(printed);
    let mut problems = Vec::new();
    for (side, said) in SIDES.into_iter().zip([&ours, &theirs]) {
        for name in &said.reached {
            problems.push(format!(
                "the {side} program calls {name}, which is not in the archive it was linked \
                 against, so that routine is the C library's and this comparison is not about it"
            ));
        }
    }
    match (&ours.cases, &theirs.cases) {
        (Some(ours), Some(theirs)) if ours == theirs => {}
        (Some(ours), Some(theirs)) => problems.push(format!(
            "ours got through {ours} cases and the reference got through {theirs}, for the same \
             harness and the same seed"
        )),
        (ours, theirs) => {
            for (side, cases) in SIDES.into_iter().zip([ours, theirs]) {
                if cases.is_none() {
                    problems.push(format!(
                        "the {side} program did not reach the end, which is a routine that took \
                         the program down rather than one that gave a different answer"
                    ));
                }
            }
        }
    }
    // Every group, not the first few, because the set of lengths a routine is wrong at is the most
    // useful thing about a difference: one length is an edge case and all of them is the loop.
    for ((what, ours), (_, theirs)) in ours.groups.iter().zip(&theirs.groups) {
        if ours != theirs {
            problems.push(format!("{what}: ours digested {ours} and the reference {theirs}"));
        }
    }
    if ours.groups.len() != theirs.groups.len() {
        problems.push(format!(
            "ours printed {} groups and the reference {}",
            ours.groups.len(),
            theirs.groups.len()
        ));
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: TASK, problems });
    }

    let cases = ours.cases.unwrap_or_default();
    println!("{TASK}: {cases} cases in {} groups, both sides agree", ours.groups.len());
    Ok(())
}

/// Splits what the script printed into the two sides.
///
/// A line the format does not fit is dropped rather than reported, because the script is allowed to
/// print whatever a compiler or a linker says on its way through and those messages are not
/// results.
fn read(printed: &str) -> (Side, Side) {
    let mut sides = [Side::default(), Side::default()];
    for line in printed.lines() {
        let Some((side, rest)) = line.split_once(' ') else { continue };
        let Some(at) = SIDES.iter().position(|name| *name == side) else { continue };
        let said = &mut sides[at];
        let rest = rest.trim();
        if let Some(name) = rest.strip_prefix("reached ") {
            said.reached.push(name.to_owned());
        } else if let Some(cases) = rest.strip_prefix("cases ") {
            said.cases = Some(cases.to_owned());
        } else {
            let words: Vec<&str> = rest.split_whitespace().collect();
            if let [what, length, digest] = words[..] {
                said.groups.push((format!("{what} {length}"), digest.to_owned()));
            }
        }
    }
    let [ours, theirs] = sides;
    (ours, theirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two runs that agree, which is what the task prints a line about.
    const AGREED: &str = "\
ours memcpy 0 1111111111111111
ours memcpy 1 2222222222222222
ours cases 40
reference memcpy 0 1111111111111111
reference memcpy 1 2222222222222222
reference cases 40
";

    #[test]
    fn two_runs_that_agree_are_the_whole_of_a_pass() {
        let (ours, theirs) = read(AGREED);
        assert_eq!(ours.groups.len(), 2);
        assert_eq!(ours.cases.as_deref(), Some("40"));
        assert!(theirs.reached.is_empty());
        assert!(compare(AGREED).is_ok());
    }

    #[test]
    fn a_group_that_came_out_differently_is_named_with_both_digests() {
        let printed =
            AGREED.replace("reference memcpy 1 2222222222222222", "reference memcpy 1 3333333333333333");
        let problems = problems_of(&printed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("memcpy 1: "), "{}", problems[0]);
        assert!(problems[0].contains("2222222222222222"), "{}", problems[0]);
        assert!(problems[0].contains("3333333333333333"), "{}", problems[0]);
    }

    /// The failure the fortification trap produces, which is the one a passing check hid once.
    #[test]
    fn a_routine_that_came_from_the_c_library_is_a_failure_rather_than_a_pass() {
        let printed = format!("ours reached __memcpy_chk@GLIBC_2.3.4\n{AGREED}");
        let problems = problems_of(&printed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("__memcpy_chk"), "{}", problems[0]);
        assert!(problems[0].contains("the ours program"), "{}", problems[0]);
    }

    #[test]
    fn a_program_that_did_not_finish_is_not_a_program_that_agreed() {
        let printed = AGREED.replace("ours cases 40\n", "");
        let problems = problems_of(&printed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("did not reach the end"), "{}", problems[0]);
    }

    #[test]
    fn one_side_running_fewer_cases_than_the_other_is_a_failure_on_its_own() {
        let printed = AGREED.replace("reference cases 40", "reference cases 39");
        let problems = problems_of(&printed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("got through 40 cases"), "{}", problems[0]);
    }

    /// What a compiler printed on its way through is not a result and is not read as one.
    #[test]
    fn a_line_that_is_not_a_result_is_left_alone() {
        let printed = format!("differential.c: In function 'main':\n{AGREED}");
        assert!(compare(&printed).is_ok());
    }

    fn problems_of(printed: &str) -> Vec<String> {
        match compare(printed) {
            Err(Error::Failed { problems, .. }) => problems,
            other => panic!("wanted a failure, got {other:?}"),
        }
    }
}
