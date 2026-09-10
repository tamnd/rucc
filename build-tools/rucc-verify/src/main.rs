//! The gate: a rule the solver cannot discharge does not enter the rule set.
//!
//! `spec/15-testing.md` section 15.5 puts the verification in CI rather than in the compiler,
//! so this is a program that reads the rule files, asks the solver about every rule in them,
//! and fails the build when anything comes back as less than a proof. What it prints is the
//! count of rules discharged and the count that needed a bounded proof, which is the metric the
//! specification asks to be reported rather than merely known.
//!
//! Every file is also compiled into the matcher it will be matched with, because a rule that can
//! never fire is a mistake whatever a solver says about it and this is the one place the whole
//! file is read at once.
//!
//! `--report` is the other half, which `spec/optimizer/41-correctness.md` section 41.8 asks for:
//! the rules that only got a bounded proof, written out by name with their reasons, so that the
//! set of rules nobody has proved at the running width is a file somebody reviews rather than a
//! count nobody reads.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{fs, io};

use rucc_rules::{Matcher, parse};
use rucc_verify::{Model, Solver, Unverified, admit, difference, listed, render};

const USAGE: &str = "\
usage: rucc-verify [--report FILE [--check]] <path>...

Each path is a rule file or a directory of them. A rule file is verified against the model file
beside it with the same name and a `.model` extension, because the meaning of a target's terms
is a fact about that target and not something to be passed in from elsewhere. A model may
include another, which is how the two rule sets over the IR are read against one account of what
the IR means.

--report FILE   write the list of rules that only got a bounded proof to FILE
--check         with --report, compare against what is there instead of writing it

The list is one file over all the paths given, so a run that writes it has to be given every
rule file in the tree or it will report the ones it was not shown as having left the list.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") || args.is_empty() {
        print!("{USAGE}");
        return if args.is_empty() { ExitCode::FAILURE } else { ExitCode::SUCCESS };
    }
    match run(&args) {
        Ok(code) => code,
        Err(problem) => {
            eprintln!("rucc-verify: {problem}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> io::Result<ExitCode> {
    let mut files = Vec::new();
    let mut report_path = None;
    let mut check_only = false;
    let mut waiting = false;
    for arg in args {
        if waiting {
            report_path = Some(PathBuf::from(arg));
            waiting = false;
            continue;
        }
        match arg.as_str() {
            "--report" => waiting = true,
            "--check" => check_only = true,
            // Anything else that looks like a flag is a mistake rather than a path, and a
            // misspelled flag that gets read as a rule file is a gate that quietly verified
            // nothing.
            other if other.starts_with('-') => {
                eprintln!("rucc-verify: no such option: {other}");
                return Ok(ExitCode::FAILURE);
            }
            other => {
                let path = Path::new(other);
                if path.is_dir() {
                    files.extend(rule_files(path)?);
                } else {
                    files.push(path.to_path_buf());
                }
            }
        }
    }
    if waiting {
        eprintln!("rucc-verify: --report wants a file to write the list to");
        return Ok(ExitCode::FAILURE);
    }
    if check_only && report_path.is_none() {
        eprintln!("rucc-verify: --check is about --report, and there is no --report here");
        return Ok(ExitCode::FAILURE);
    }
    files.sort();

    // Nothing to verify is worth saying out loud rather than passing quietly, because a gate
    // that has stopped seeing the thing it guards looks exactly like a gate that is happy.
    if files.is_empty() {
        println!("rucc-verify: no rule files under {}", args.join(", "));
        return Ok(ExitCode::SUCCESS);
    }

    let Some(solver) = Solver::find() else {
        eprintln!("rucc-verify: no solver on PATH, and this is the one place that is an error");
        return Ok(ExitCode::FAILURE);
    };
    println!("rucc-verify: asking {}", solver.name());

    let mut refused = 0;
    let mut bounded = 0;
    let mut listing: Vec<Unverified> = Vec::new();
    for file in &files {
        let shown = file.display().to_string();
        let text = fs::read_to_string(file)?;
        let model_path = file.with_extension("model");
        if !model_path.is_file() {
            eprintln!("{shown}: no {} beside it to say what its terms mean", {
                model_path.display()
            });
            refused += 1;
            continue;
        }

        let rules = match parse(&shown, &text) {
            Ok(rules) => rules,
            Err(errors) => {
                report(&errors);
                refused += 1;
                continue;
            }
        };
        // A rule that can never fire is a mistake whatever the solver says about it, and this is
        // the one place the whole file is read, so it is the place to find out.
        if let Err(errors) = Matcher::build(&shown, &rules) {
            report(&errors);
            refused += 1;
            continue;
        }
        let model = match Model::open(&model_path) {
            Ok(model) => model,
            Err(errors) => {
                report(&errors);
                refused += 1;
                continue;
            }
        };

        match admit(&shown, &rules, &model, &solver) {
            Ok(report) => {
                println!("{shown}: {report}");
                bounded += report.bounded();
                listing.extend(listed(&shown, &rules, &report));
            }
            Err(errors) => {
                report(&errors);
                let count = rules.len();
                println!("{shown}: {count} {}, and not every one is proved", named(count, "rule"));
                refused += 1;
            }
        }
    }

    if refused > 0 {
        let files = named(refused, "rule file");
        eprintln!("rucc-verify: {refused} {files} may not enter the rule set");
        return Ok(ExitCode::FAILURE);
    }
    println!("rucc-verify: every rule is proved, {bounded} of them at bounded widths");

    // The list is written last, after everything has been verified, because a list produced
    // alongside a failure would be a list of what happened to be reached before the failure.
    match report_path {
        None => Ok(ExitCode::SUCCESS),
        Some(path) => keep(&path, &listing, check_only),
    }
}

/// Write the list of bounded rules, or say how what is on disk differs from it.
///
/// The difference is spelled out rather than reported as a mismatch, because the two directions
/// mean opposite things. A rule that joined the list is the thing this file exists to catch and
/// wants an argument about whether the reason is good enough. A rule that left it is somebody
/// having proved it properly, which is the direction to go in and only needs the file updating.
fn keep(path: &Path, listing: &[Unverified], check_only: bool) -> io::Result<ExitCode> {
    let shown = path.display();
    let wanted = render(listing);
    if !check_only {
        fs::write(path, &wanted)?;
        println!("rucc-verify: wrote {shown}");
        return Ok(ExitCode::SUCCESS);
    }

    let found = fs::read_to_string(path).unwrap_or_default();
    if found == wanted {
        println!("rucc-verify: {shown} is up to date");
        return Ok(ExitCode::SUCCESS);
    }
    let (added, removed) = difference(&found, &wanted);
    eprintln!("rucc-verify: {shown} is not what the solver just said");
    for line in &added {
        eprintln!("  now proved only at narrow widths: {}", line.trim_start_matches("- "));
    }
    for line in &removed {
        eprintln!("  no longer on the list: {}", line.trim_start_matches("- "));
    }
    if added.is_empty() && removed.is_empty() {
        eprintln!("  the same rules, so what changed is the wording around them");
    }
    eprintln!("rucc-verify: run the same command without --check to write it");
    Ok(ExitCode::FAILURE)
}

/// Every `.rules` file in a directory, one level deep, which is how the rule sets are laid out.
fn rule_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|kind| kind == "rules") {
            out.push(path);
        }
    }
    Ok(out)
}

/// The plural of a word, when there is not exactly one of the thing.
fn named(count: usize, word: &str) -> String {
    if count == 1 { word.to_owned() } else { format!("{word}s") }
}

fn report(errors: &[rucc_rules::Error]) {
    for error in errors {
        eprintln!("{error}");
    }
}
