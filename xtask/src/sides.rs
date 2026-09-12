//! What each build of a fixture printed, and whether the builds agree.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8, which is where the runtime routines are and
//! where what runs them is.
//!
//! Two tasks hold a fixture's output against the system compiler's: [`crate::wide`] for arithmetic
//! on a hundred and twenty eight bit integer and [`crate::quad`] for arithmetic at binary128. Both
//! are the same shape, because both are about a pass that turns an operation into a call: compile
//! one C file with the compiler this tree builds and with the system compiler, run every build,
//! and compare what each one printed group by group. What differs between them is the fixture and
//! the list of routines, so the reading and the comparing are here and each task is what is left.
//!
//! This was part of `xtask/src/wide.rs` until the second task wanted it, which is the point at
//! which the three constants below stopped being that task's business and became the shape both
//! follow.
//!
//! # Three optimization levels
//!
//! Because one of them is where a pass has already been wrong. tamnd/rucc#1054 was the splitting
//! walking the blocks in the order the function happened to hold them, which is the order they were
//! made in, which at `-O0` is the order they run in and above `-O0` is not: any pass that gives a
//! loop a preheader puts a block that runs early at the end of the list. So the function was refused
//! at `-O1` and compiled at `-O0`, and the optimizer is what makes the difference. Running one level
//! would be a check that did not know that.
//!
//! # Why the answers are the system compiler's rather than written down here
//!
//! The same reason section 12.8 gives for the runtime routines. A table of expected digests in a
//! file here would be a table somebody computed with one of the two compilers, and the case that
//! matters is the one nobody thought to write down. gcc has had both of these widths working for
//! years and libgcc is what every other compiler on the platform is checked against, so what it
//! prints is the answer. The routines on our side are ours rather than libgcc's, because the
//! archive is on the link line ahead of it, which is also what `cargo xtask builtins-diff` holds
//! against the Rust reference one layer down.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::TRIPLE;
use crate::{Error, Result, root};

/// The optimization levels a fixture is compiled at by the compiler under test.
pub(crate) const LEVELS: [&str; 3] = ["0", "1", "2"];

/// What the system compiler's run is called in the output.
pub(crate) const REFERENCE: &str = "reference";

/// Lays out the directory the runner is pointed at: the fixture, one object per level, the archive
/// the calls are resolved from, and the script.
///
/// The work directory is `target/<task>`, cleared first, so a run says nothing about the run before
/// it.
///
/// # Errors
///
/// [`Error::Io`] when the directory cannot be made, the compiler will not build or the script cannot
/// be written, and [`Error::Failed`] when the fixture does not compile at one of the levels.
pub(crate) fn build(task: &'static str, fixture: &Path, script: &str) -> Result<PathBuf> {
    let work = root().join("target").join(task);
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let status = Command::new("cargo")
        .args(["build", "-q", "--release", "-p", "rucc"])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !status.success() {
        return Err(Error::Io("the compiler did not build".to_owned()));
    }
    let rucc = root().join("target").join("release").join("rucc");

    let named = fixture.file_name().map_or_else(|| "fixture.c".into(), std::ffi::OsStr::to_os_string);
    for level in LEVELS {
        let object = work.join(format!("ours-O{level}.o"));
        let out = Command::new(&rucc)
            .args(["-c", &format!("--target={TRIPLE}"), &format!("-O{level}")])
            .arg("-o")
            .arg(&object)
            .arg(fixture)
            .current_dir(root())
            .output()
            .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
        if !out.status.success() {
            return Err(Error::Failed {
                task,
                problems: vec![format!(
                    "{} did not compile at -O{level}\n{}",
                    named.to_string_lossy(),
                    crate::indent(String::from_utf8_lossy(&out.stderr).trim_end())
                )],
            });
        }
    }

    let archive = crate::builtins_archive(TRIPLE)?;
    copy(&archive, &work.join("librucc_builtins.a"))?;
    copy(fixture, &work.join(named))?;
    std::fs::write(work.join("run.sh"), script)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// One file into the work directory, which is copied rather than read from the tree because the
/// container mounts the directory read only and cannot reach anything outside it.
fn copy(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", from.display())))
}

/// What one program's run said.
#[derive(Default)]
pub(crate) struct Side {
    /// One digest per group, in the order the groups ran, as `("udiv 3", "1099...")`.
    pub(crate) groups: Vec<(String, String)>,
    /// How many cases the run got through, where it got to the end.
    pub(crate) cases: Option<String>,
    /// The runtime routines the object this side was built from goes looking for.
    pub(crate) calls: Vec<String>,
}

/// Holds every build of a fixture against the system compiler's build of it.
///
/// `task` is the name in the messages and `routines` is what every object is expected to call, which
/// is how a build whose operations did not become calls is told from one that agreed.
///
/// # Errors
///
/// [`Error::Failed`] when a build does not call one of the routines, when a program did not reach
/// the end, or when any group of cases came out differently from the system compiler's.
pub(crate) fn compare(
    task: &'static str,
    routines: &[&str],
    printed: &str,
    runner: &str,
) -> Result<()> {
    let said = read(printed);
    let mut problems = Vec::new();
    let theirs = said.get(REFERENCE);
    let groups = theirs.map_or(0, |side| side.groups.len());
    // A reference that did not finish is the apparatus rather than a result, and it is said once.
    // What the levels printed is not held against half a run, because every group past the point it
    // stopped at would come out as a difference and the one line worth reading would be the last of
    // thirty.
    let comparable = theirs.is_some_and(|side| side.cases.is_some());
    if !comparable {
        problems.push(format!(
            "the {REFERENCE} program did not reach the end, so there is nothing to compare against"
        ));
    }
    for level in LEVELS {
        let name = format!("ours-O{level}");
        let Some(ours) = said.get(&name) else {
            problems.push(format!("{name} printed nothing at all"));
            continue;
        };
        for routine in routines {
            if !ours.calls.iter().any(|called| called == routine) {
                problems.push(format!(
                    "the object behind {name} does not call {routine}, and the fixture reaches \
                     every one of these routines, so an operation became something other than \
                     that call"
                ));
            }
        }
        let Some(theirs) = theirs.filter(|_| comparable) else { continue };
        if ours.cases.is_none() {
            problems.push(format!(
                "{name} did not reach the end, which is a program that stopped rather than one \
                 that gave a different answer"
            ));
        } else if ours.cases != theirs.cases {
            problems.push(format!(
                "{name} got through {} cases and {REFERENCE} got through {}, for the same fixture \
                 and the same seed",
                shown(&ours.cases),
                shown(&theirs.cases)
            ));
        }
        // Every group that differs rather than the first, because which operations disagree is the
        // most useful thing about a difference: one of them is an edge case and all of them is the
        // pass itself.
        for ((what, ours), (_, theirs)) in ours.groups.iter().zip(&theirs.groups) {
            if ours != theirs {
                problems.push(format!(
                    "{name} {what}: digested {ours} where {REFERENCE} digested {theirs}"
                ));
            }
        }
        if ours.groups.len() != theirs.groups.len() {
            problems.push(format!(
                "{name} printed {} groups and {REFERENCE} printed {}",
                ours.groups.len(),
                theirs.groups.len()
            ));
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task, problems });
    }

    let cases = theirs.and_then(|side| side.cases.clone()).unwrap_or_default();
    println!(
        "{task}: {cases} cases in {groups} groups, and every one of the {} levels agrees with the \
         system compiler, {runner}",
        LEVELS.len()
    );
    Ok(())
}

/// A count a run printed, or a word saying it did not.
fn shown(cases: &Option<String>) -> String {
    cases.clone().unwrap_or_else(|| "no".to_owned())
}

/// Splits what a script printed into one entry per program.
///
/// A line the format does not fit is dropped rather than reported, because a script is allowed to
/// print whatever a compiler or a linker says on its way through and those messages are not results.
pub(crate) fn read(printed: &str) -> BTreeMap<String, Side> {
    let mut sides: BTreeMap<String, Side> = BTreeMap::new();
    for line in printed.lines() {
        let Some((side, rest)) = line.split_once(' ') else { continue };
        if side != REFERENCE && !LEVELS.iter().any(|level| side == format!("ours-O{level}")) {
            continue;
        }
        let said = sides.entry(side.to_owned()).or_default();
        let rest = rest.trim();
        if let Some(routine) = rest.strip_prefix("calls ") {
            said.calls.push(routine.to_owned());
        } else if let Some(cases) = rest.strip_prefix("cases ") {
            said.cases = Some(cases.to_owned());
        } else {
            let words: Vec<&str> = rest.split_whitespace().collect();
            if let [what, round, digest] = words[..] {
                said.groups.push((format!("{what} {round}"), digest.to_owned()));
            }
        }
    }
    sides
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The task and the routines the tests below are written against, which stand in for a real
    /// fixture's because what is being tested is the comparing and not either list.
    const TASK: &str = "example";
    const ROUTINES: [&str; 2] = ["__udivti3", "__modti3"];

    /// What a run where everything agreed looks like, with every call each object makes.
    fn agreed() -> String {
        let mut printed = String::new();
        printed.push_str("reference udiv 0 1111111111\n");
        printed.push_str("reference add 0 2222222222\n");
        printed.push_str("reference cases 40\n");
        for level in LEVELS {
            for routine in ROUTINES {
                printed.push_str(&format!("ours-O{level} calls {routine}\n"));
            }
            printed.push_str(&format!("ours-O{level} udiv 0 1111111111\n"));
            printed.push_str(&format!("ours-O{level} add 0 2222222222\n"));
            printed.push_str(&format!("ours-O{level} cases 40\n"));
        }
        printed
    }

    /// The problems a run produced, which is what every test below reads.
    fn problems_of(printed: &str) -> Vec<String> {
        match compare(TASK, &ROUTINES, printed, "here") {
            Ok(()) => Vec::new(),
            Err(Error::Failed { problems, .. }) => problems,
            Err(other) => panic!("the wrong kind of failure: {other}"),
        }
    }

    #[test]
    fn four_runs_that_agree_are_the_whole_of_a_pass() {
        let printed = agreed();
        let said = read(&printed);
        assert_eq!(said.len(), LEVELS.len() + 1, "one entry per program");
        assert_eq!(said[REFERENCE].groups.len(), 2);
        assert_eq!(said["ours-O2"].calls.len(), ROUTINES.len());
        assert!(problems_of(&printed).is_empty());
    }

    #[test]
    fn a_group_that_came_out_differently_names_the_level_and_both_digests() {
        let printed = agreed().replace("ours-O1 udiv 0 1111111111", "ours-O1 udiv 0 3333333333");
        let problems = problems_of(&printed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("ours-O1 udiv 0: "), "{}", problems[0]);
        assert!(problems[0].contains("3333333333"), "{}", problems[0]);
        assert!(problems[0].contains("1111111111"), "{}", problems[0]);
    }

    /// A build whose operations did not become calls is a pass for the wrong reason.
    ///
    /// Every digest would still agree, because a fixture's answers are C's answers and an
    /// optimizer that worked one out some other way would work out the same number. What the check
    /// is for is the day the lowering stops being a call and nobody notices which code ran.
    #[test]
    fn a_build_that_does_not_call_the_routine_is_a_failure() {
        let printed = agreed().replace("ours-O2 calls __modti3\n", "");
        let problems = problems_of(&printed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("__modti3"), "{}", problems[0]);
        assert!(problems[0].contains("ours-O2"), "{}", problems[0]);
    }

    #[test]
    fn a_program_that_stopped_is_not_a_program_that_agreed() {
        let printed = agreed().replace("ours-O0 cases 40\n", "");
        let problems = problems_of(&printed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("ours-O0 did not reach the end"), "{}", problems[0]);
    }

    /// The reference not finishing is one problem and not four, because nothing can be compared.
    #[test]
    fn a_reference_that_did_not_finish_is_reported_once() {
        let printed = agreed().replace("reference cases 40\n", "");
        let problems = problems_of(&printed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("nothing to compare against"), "{}", problems[0]);
    }

    #[test]
    fn two_runs_that_got_through_different_numbers_of_cases_are_reported() {
        let printed = agreed().replace("ours-O1 cases 40", "ours-O1 cases 36");
        let problems = problems_of(&printed);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("36"), "{}", problems[0]);
        assert!(problems[0].contains("40"), "{}", problems[0]);
    }
}
