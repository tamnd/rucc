//! An instrumented SQLite, built, linked, run against a real workload, and held to its answers.
//!
//! Design: `spec/safe-memory/14-verification.md` section 14.9 and the seventh box of
//! tamnd/rucc#1307.
//!
//! Everything in `tests/safety` is a few lines of C written to provoke one judgement, which is the
//! right shape for asking whether a check fires. It is the wrong shape for asking whether a real
//! library survives the monitor, and the two are different questions. tamnd/rucc#1307 is what the
//! gap between them costs: every interposed library write recorded the init plane and said nothing
//! on the type plane, which no case in the suite could see, and an instrumented SQLite aborted
//! within a few hundred calls because its lookaside allocator hands the same block out as three
//! different structures. Three more holes of the same shape have been found since, and every one
//! of them was found by running this program by hand.
//!
//! So it stops being by hand. A library that recycles its own storage, walks its own trees and
//! answers questions whose answers are arithmetic is a thing the suite cannot imitate and does not
//! have to: SQLite ships as one C file, rucc compiles it, and the answers are either right or they
//! are not.
//!
//! # Why the amalgamation is not in the tree
//!
//! It is nine megabytes of somebody else's C and it has a version. Vendoring it would put a copy
//! of SQLite in a compiler's history forever and make every bump of it a commit here, and the
//! check does not care which version it gets: the workload is ordinary SQL and the answers are the
//! same against any of them. So the file is looked for and the check says what to fetch when it is
//! not there, which is the same bargain `xtask/src/real_libc.rs` makes about a glibc abilist and
//! for the same reason.
//!
//! # What it is checking
//!
//! That the program links, runs, and prints the answers the workload's own arithmetic says it
//! should, at `-O0` and at `-O2`. Both levels, because the two failures are different: at `-O0` a
//! wrong answer is the instrumentation or the runtime, and at `-O2` it is either of those or a
//! check the optimizer removed that was holding something up. A refusal is a failure here rather
//! than a result, because the workload does nothing wrong, so every report this produces is a
//! false positive by construction.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::safety::{self, BANNER};
use crate::{Error, Result, cost, indent, root};

/// What the program has to say before its answers count.
///
/// The workload checks its own arithmetic and prints this when all of it came out, which keeps the
/// expected numbers in the C file beside the queries that produce them rather than here, where
/// they would be a second copy to keep in step.
const CORRECT: &str = "all answers correct";

/// The levels it is built and run at.
const LEVELS: [&str; 2] = ["-O0", "-O2"];

/// Where the amalgamation is looked for when nothing said.
///
/// The directory `sqlite.org`'s own tarball unpacks to, under the two places a person is likely to
/// have put it. Neither is searched recursively, because a check that goes hunting through a home
/// directory for a nine megabyte C file is a check that will one day find the wrong one.
const USUAL: [&str; 2] = ["sqlite-autoconf", "sqlite"];

/// Builds an instrumented SQLite at each level, runs the workload, and holds it to its answers.
///
/// # Errors
///
/// [`Error::Failed`] when a build did not link, a run did not exit cleanly, or the answers came
/// back wrong, with one line per level that went wrong. [`Error::Io`] when the check could not be
/// run at all, which is the compiler not building or no way to run an x86-64 Linux program.
pub(crate) fn sqlite() -> Result<()> {
    let Some(amalgamation) = found() else {
        // Not a failure, the same way a mac with no glibc is not one. What is worth printing is
        // where to get the file, because a person reading this line is a person about to go and
        // look for it.
        println!(
            "sqlite: no amalgamation on this machine, so nothing was built. Unpack the one from \
             https://sqlite.org/download.html and point RUCC_SQLITE_AMALGAMATION at its sqlite3.c."
        );
        return Ok(());
    };
    let runner = Runner::find("this check")?;
    println!("sqlite: {}, -fsafety=detect at -O0 and -O2, {runner}", amalgamation.display());

    let work = build(&amalgamation)?;
    let ran = safety::read(&runner.run(&work, "the workload")?);

    let mut problems = Vec::new();
    for level in LEVELS {
        let Some(ran) = ran.get(level) else {
            problems.push(format!("{level}: did not run"));
            continue;
        };
        if ran.output.contains(BANNER) {
            problems.push(format!(
                "{level}: the monitor refused a workload that does nothing wrong, so this is a \
                 false positive.\n{}",
                indent(ran.output.trim_end())
            ));
            continue;
        }
        match ran.status {
            Some(0) if ran.output.contains(CORRECT) => {}
            None => {
                problems.push(format!("{level}: did not link.\n{}", indent(ran.output.trim_end())))
            }
            _ => problems.push(format!(
                "{level}: ran and got the wrong answers, or did not finish.\n{}",
                indent(ran.output.trim_end())
            )),
        }
    }

    if problems.is_empty() {
        println!("sqlite: both levels link, run and answer correctly");
        return Ok(());
    }
    Err(Error::Failed { task: "sqlite", problems })
}

/// Where the amalgamation is, if it is anywhere this knows to look.
fn found() -> Option<PathBuf> {
    if let Some(said) = std::env::var_os("RUCC_SQLITE_AMALGAMATION") {
        if let Some(said) = declared(Path::new(&said)) {
            return Some(said);
        }
    }
    usual()
}

/// What a path somebody set is worth, which is nothing when there is no file at it.
///
/// A variable pointing at a file that is not there reads as the variable not being set, because
/// the sentence that prints in that case names the variable and is the right thing to read either
/// way.
fn declared(said: &Path) -> Option<PathBuf> {
    said.is_file().then(|| said.to_path_buf())
}

/// Where the amalgamation is when nobody said, which is the directory the tarball unpacks to under
/// one of the two places a person is likely to have put it.
fn usual() -> Option<PathBuf> {
    let homes =
        [std::env::var_os("HOME").map(PathBuf::from), Some(PathBuf::from("/usr/local/src"))];
    for home in homes.into_iter().flatten() {
        for stem in USUAL {
            let Ok(entries) = std::fs::read_dir(&home) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if !name.starts_with(stem) {
                    continue;
                }
                let candidate = entry.path().join("sqlite3.c");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Compiles the amalgamation and the workload at each level and leaves a directory the runner can
/// take.
///
/// Assembly rather than objects, so that the link is the runner's and a machine that cannot run an
/// x86-64 program can still do everything up to it. That is the same arrangement the safety suite
/// has, and it is what lets the container do one link and one run rather than being handed a
/// binary built somewhere it cannot read.
fn build(amalgamation: &Path) -> Result<PathBuf> {
    let work = root().join("target").join("sqlite");
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let rucc = cost::compiler()?;
    let archive = crate::staticlib("rucc-safe-rt", TRIPLE)?;
    std::fs::copy(&archive, work.join("safe-rt.a"))
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", archive.display())))?;

    let driver = root().join("tests").join("sqlite").join("a-real-workload.c");
    let mut problems = Vec::new();
    for level in LEVELS {
        for (source, stem) in [(amalgamation, "sqlite3"), (driver.as_path(), "driver")] {
            let out = Command::new(&rucc)
                .args(["-S", &format!("--target={TRIPLE}"), "-fsafety=detect", level])
                .arg("-o")
                .arg(work.join(format!("{stem}{level}.s")))
                .arg(source)
                .current_dir(root())
                .output()
                .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
            if !out.status.success() {
                problems.push(format!(
                    "{level}: {} did not compile\n{}",
                    source.display(),
                    indent(String::from_utf8_lossy(&out.stderr).trim_end())
                ));
            }
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "sqlite", problems });
    }

    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// The script that links each level and runs it.
///
/// `gcc` rather than rucc's own driver for the link, because what is being checked is the code
/// rucc generated and not the link line it writes, and because the runtime's static library wants
/// the four libraries SQLite's configure script asks for. `-no-pie` for the same reason the safety
/// suite uses it: the runtime's planes are addressed absolutely.
///
/// The case names are the levels, so that what comes back reads the way the safety suite's does
/// and can be split by the same reader.
const SCRIPT: &str = "\
#!/bin/sh
exec 2>/dev/null
here=$(pwd)
out=/tmp/rucc-${here##*/}
mkdir -p \"$out\"
for level in -O0 -O2; do
    printf '<<<case %s>>>\\n' \"$level\"
    if gcc -no-pie \"sqlite3$level.s\" \"driver$level.s\" safe-rt.a -lpthread -lm -ldl \
        -o \"$out/run$level\" >\"$out/$level.log\" 2>&1; then
        \"$out/run$level\" >\"$out/$level.out\" 2>&1
        status=$?
        cat \"$out/$level.out\"
        printf '<<<status %s>>>\\n' \"$status\"
    else
        cat \"$out/$level.log\"
        printf '<<<status nolink>>>\\n'
    fi
done
";

#[cfg(test)]
mod tests {
    use super::*;

    /// The script and the compile loop have to agree about what the files are called, and they are
    /// written out in two places because one is shell and the other is Rust.
    #[test]
    fn the_script_links_the_files_the_build_writes() {
        for level in LEVELS {
            assert!(SCRIPT.contains("\"sqlite3$level.s\""), "{SCRIPT}");
            assert!(SCRIPT.contains("\"driver$level.s\""), "{SCRIPT}");
            assert!(SCRIPT.contains(level), "{SCRIPT}");
        }
    }

    /// Four runs share `/tmp` under the gate and a fixed name would be four of them in one
    /// directory, which is the hazard the safety suite's own test is about.
    #[test]
    fn the_programs_go_somewhere_named_after_the_run() {
        assert!(SCRIPT.contains("out=/tmp/rucc-${here##*/}"), "{SCRIPT}");
    }

    /// The sentence the workload prints when its arithmetic came out is the thing this check
    /// reads, so it has to be the sentence the workload actually prints.
    #[test]
    fn the_workload_prints_the_words_this_looks_for() {
        let source =
            std::fs::read_to_string(root().join("tests").join("sqlite").join("a-real-workload.c"))
                .expect("the workload is in the tree");
        assert!(source.contains(CORRECT), "{CORRECT} is not what the workload prints");
    }

    /// An unset variable and a variable pointing at nothing are the same answer, because the
    /// sentence that prints names the variable either way.
    #[test]
    fn a_path_that_is_not_there_reads_as_nothing_being_there() {
        assert_eq!(declared(Path::new("/nowhere/at/all/sqlite3.c")), None);
        assert_eq!(declared(&root().join("Cargo.toml")), Some(root().join("Cargo.toml")));
        // A directory is not a file, which matters because the variable is easy to point at the
        // unpacked tree rather than at the one file inside it.
        assert_eq!(declared(&root()), None);
    }
}
