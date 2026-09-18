//! Whether the compiler writes the same thing every time it is asked the same question.
//!
//! Design: `spec/03-architecture.md` section 3.7, which asks for byte-identical output from
//! byte-identical input on every host and at every `-j`. The `determinism` job in CI had the tree's
//! own half of that, which is the compiler building twice to the same bytes. This is the other half,
//! which is what the compiler makes out of a C file having to be the same in every run as well. A
//! build nobody can repeat cannot be signed for, a difference between two builds cannot be read as a
//! change somebody made, and every measurement in this repository is a count taken from one run and
//! compared against a count taken from another.
//!
//! What it does is compile each program under `tests/repeatable` eight times, each one a fresh
//! process, and compare what came out. The IR rather than the assembly, because the IR is where a
//! difference appears first and the assembly is where two different inputs can still land on the
//! same answer: the one bug this has caught so far moved the numbering of values in the IR, and in
//! most functions the register allocator made the same choices out of both and only some of them
//! came out different.
//!
//! Eight because the difference is a coin rather than a rule. The failure this was written for comes
//! out of a hash table's order, so two runs agree about as often as they disagree, and eight runs of
//! a program that takes a few milliseconds is cheaper than a check that finds a real difference in
//! one run out of three.
//!
//! # Why a check rather than a test
//!
//! Because it needs more than one process. Rust gives each hash table in a process a key of its own
//! and gives each process its own seed, so a test that calls a function twice is a test of two hash
//! tables and not of two runs, and a difference that depends on the seed is invisible from inside
//! one run of the test binary.
//!
//! # What the programs are for
//!
//! Each one is a shape that has been seen to come out differently, written down so that it stays
//! checked. `labels.c` is the first: a function whose labels are inside a loop and jump to each
//! other, which is tamnd/rucc#1396 and is what the SQLite amalgamation was doing in the function
//! nobody could get two matching builds of. The file itself says what each part of it is for.
//!
//! A program here is never run. What it prints is not the question, and one that loops forever
//! would be as good a fixture as any.

use std::collections::BTreeMap;
use std::process::Command;

use crate::cost::compiler;
use crate::{Error, Result, root};

/// What this task is called, for the messages.
const TASK: &str = "repeatable";

/// How many times each program is compiled.
const RUNS: usize = 8;

/// Compiles every program under `tests/repeatable` several times over and compares the runs.
///
/// # Errors
///
/// [`crate::Error::Io`] when the fixtures cannot be read or the compiler will not run, and
/// [`crate::Error::Failed`] when two runs over one program did not write the same IR.
pub(crate) fn repeatable() -> Result<()> {
    let rucc = compiler()?;
    let dir = root().join("tests").join("repeatable");
    let mut programs: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| Error::Io(format!("could not read {}: {e}", dir.display())))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|end| end == "c"))
        .collect();
    programs.sort();
    if programs.is_empty() {
        return Err(Error::Io(format!("no programs under {}", dir.display())));
    }

    // Somewhere to write the IR that is not beside the fixtures, since a check that leaves files in
    // `tests` is a check that makes the tree look changed.
    let work = root().join("target").join("repeatable");
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let mut differed = Vec::new();
    for program in &programs {
        // Every run's answer, and how many runs gave it. A map rather than a count of the
        // differences, because what is worth printing when this fails is how many answers there
        // were and how the runs split between them, which is the difference between a compiler that
        // is unsteady and one that changed halfway through.
        let mut answers: BTreeMap<String, usize> = BTreeMap::new();
        let into = work.join("out.ir");
        for _ in 0..RUNS {
            let out = Command::new(&rucc)
                .args(["-O1", "--emit=ir", "-o"])
                .arg(&into)
                .arg(program)
                .current_dir(root())
                .output()
                .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
            if !out.status.success() {
                return Err(Error::Io(format!(
                    "{}: did not compile\n{}",
                    program.display(),
                    String::from_utf8_lossy(&out.stderr).trim_end()
                )));
            }
            let written = std::fs::read_to_string(&into)
                .map_err(|e| Error::Io(format!("could not read {}: {e}", into.display())))?;
            *answers.entry(written).or_default() += 1;
        }
        if answers.len() > 1 {
            let split: Vec<String> = answers.values().map(usize::to_string).collect();
            differed.push(format!(
                "{}: {RUNS} runs wrote {} different IR files, {} runs each",
                program.display(),
                answers.len(),
                split.join(" and ")
            ));
        }
    }

    if !differed.is_empty() {
        return Err(Error::Failed { task: TASK, problems: differed });
    }
    println!(
        "xtask: repeatable compiled {} program{} {RUNS} times each and every run agreed",
        programs.len(),
        if programs.len() == 1 { "" } else { "s" }
    );
    Ok(())
}
