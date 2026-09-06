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

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{fs, io};

use rucc_rules::{Matcher, parse};
use rucc_verify::{Model, Solver, admit};

const USAGE: &str = "\
usage: rucc-verify <path>...

Each path is a rule file or a directory of them. A rule file is verified against the model file
beside it with the same name and a `.model` extension, because the meaning of a target's terms
is a fact about that target and not something to be passed in from elsewhere. A model may
include another, which is how the two rule sets over the IR are read against one account of what
the IR means.
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
    for arg in args {
        let path = Path::new(arg);
        if path.is_dir() {
            files.extend(rule_files(path)?);
        } else {
            files.push(path.to_path_buf());
        }
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
    Ok(ExitCode::SUCCESS)
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
