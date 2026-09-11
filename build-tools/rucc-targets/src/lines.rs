//! Generating the recorded link line for every target.
//!
//! Design: `spec/cross-compile/11-linking.md` section 11.3, which ends with the test this writes:
//! "a golden-file test per target comparing the generated argv against a recorded one. That test is
//! cheap, catches regressions in the highest-consequence code in the driver, and needs no target
//! machine."
//!
//! One file per target, the same shape as `tests/abi-corpus`, because a file per target is a diff
//! per target. A change to one architecture's loader path shows up as one file changing, and a
//! change to the flags every line carries shows up as forty four files changing, which is the
//! difference between a review that reads and one that does not.
//!
//! Every mode is in each file, including the ones that are refused, because the refusals are the
//! interesting rows. A static glibc link is impossible rather than unimplemented, and a recorded
//! file saying so in the target's own words is the only place a reader finds that out without
//! running into it.
//!
//! The cache directory is a placeholder rather than the real one, so that the files are the same on
//! every machine. That is the same reason `spec/cross-compile/02-the-goal.md` claim 5 gives for the
//! sysroot path being a function of the tuple, one level out: a recorded file holding somebody's
//! home directory is a file that is out of date on every other machine.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rucc_sysroot::argv::{Invocation, Item, argv, emulation, pe_machine};
use rucc_sysroot::link::loader;
use rucc_sysroot::{LinkMode, Sysroot};
use rucc_tuple::{TARGETS, TargetTuple};

use crate::CACHE;

/// Where the recorded lines live, relative to the workspace root.
const DIR: &str = "tests/link-lines";

/// The modes, in the order they are recorded.
///
/// All five, for every target, because what the line is for a mode a target cannot use is as much
/// part of the answer as what it is for one it can.
const MODES: &[(LinkMode, &str)] = &[
    (LinkMode::Static, "static"),
    (LinkMode::StaticPie, "static-pie"),
    (LinkMode::Dynamic, "dynamic"),
    (LinkMode::DynamicNoPie, "dynamic-no-pie"),
    (LinkMode::Shared, "shared"),
];

/// What to do with the recorded lines.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Write them to `tests/link-lines`.
    Write,
    /// Check that what is there matches what would be written.
    Check,
}

/// Print the lines for one target on standard output.
pub(crate) fn one(target: TargetTuple) -> ExitCode {
    print!("{}", render(target));
    ExitCode::SUCCESS
}

/// Write or check every file.
pub(crate) fn run(root: &Path, mode: Mode) -> ExitCode {
    let dir = root.join(DIR);
    let mut written = 0;
    let mut stale = Vec::new();

    for entry in TARGETS {
        let Ok(target) = entry.tuple.parse::<TargetTuple>() else {
            eprintln!("error: the target table holds `{}`, which does not parse", entry.tuple);
            return ExitCode::FAILURE;
        };
        let path = dir.join(format!("{}.txt", target.to_canonical_string()));
        let wanted = render(target);

        if mode == Mode::Check {
            if std::fs::read_to_string(&path).unwrap_or_default() != wanted {
                stale.push(entry.tuple);
            }
            written += 1;
            continue;
        }

        if let Err(error) = std::fs::create_dir_all(&dir) {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
        if let Err(error) = std::fs::write(&path, wanted) {
            eprintln!("error: {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
        written += 1;
    }

    if mode == Mode::Check {
        if stale.is_empty() {
            println!("link-lines: {written} files are up to date");
            return ExitCode::SUCCESS;
        }
        println!("link-lines: {} files are out of date, run `cargo xtask link-lines`", stale.len());
        for tuple in stale {
            println!("  {tuple}");
        }
        return ExitCode::FAILURE;
    }
    println!("link-lines: wrote {written} files to {DIR}");
    ExitCode::SUCCESS
}

/// The recorded file for one target.
fn render(target: TargetTuple) -> String {
    let sysroot = Sysroot::in_cache(Path::new(CACHE), target);
    let mut out = String::new();

    let _ = writeln!(out, "target    {}", target.to_canonical_string());
    let _ = writeln!(out, "format    {}", target.object_format().as_str());
    // The machine flag under one name for both formats, because what the row records is what goes
    // after `-m` and a reader comparing two targets wants them in the same place.
    let machine = emulation(target).or_else(|| pe_machine(target)).unwrap_or("none");
    let _ = writeln!(out, "emulation {machine}");
    let _ = writeln!(out, "loader    {}", loader(target).unwrap_or("none"));
    let _ = writeln!(out, "sysroot   {}", sysroot.root().display());

    // A complete command rather than the flags alone: one object and an output, so that what is
    // recorded is what would actually be run and a reader can see where the caller's own files land
    // among the start files and the libraries.
    let inputs = [Item::File(PathBuf::from("main.o"))];
    for (mode, name) in MODES {
        let options = Invocation {
            inputs: &inputs,
            output: Some(Path::new("main")),
            mode: *mode,
            ..Invocation::default()
        };
        let _ = writeln!(out);
        let _ = writeln!(out, "[{name}]");
        match argv(target, &sysroot, &options) {
            // One argument per line, so that a change to one of them is one line of diff. The
            // shell would take them all on one line and a reviewer would not.
            Ok(args) => {
                for arg in args {
                    let _ = writeln!(out, "  {arg}");
                }
            }
            Err(why) => {
                let _ = writeln!(out, "  refused: {why}");
            }
        }
    }
    out
}
