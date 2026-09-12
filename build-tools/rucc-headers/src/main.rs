//! The command line around the merge. See the crate documentation for what it is for.

use std::path::PathBuf;
use std::process::ExitCode;

use rucc_headers::tree::{Input, merge_trees};

const USAGE: &str = "\
usage: cargo run -q -p rucc-headers -- --release <version>=<dir> ... --out <dir>

  --release <version>=<dir>  an installed header tree and the release it is, oldest first
  --out <dir>                where to write the merged tree, which has to be empty
  --listing <file>           write one line per file as well, for review
  --help

A version is spelled the way glibc spells it, so 2.28. A directory is the one the headers are
under, which is where usr/include ends in an install. At least two releases, because merging one
release is copying it.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(why) => {
            eprintln!("rucc-headers: error: {why}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let mut inputs: Vec<Input> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut listing: Option<PathBuf> = None;
    let mut at = 0;
    while at < args.len() {
        let arg = &args[at];
        at += 1;
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "--release" => inputs.push(release(value(args, &mut at, arg)?)?),
            "--out" => out = Some(PathBuf::from(value(args, &mut at, arg)?)),
            "--listing" => listing = Some(PathBuf::from(value(args, &mut at, arg)?)),
            _ => return Err(format!("{arg} is not an argument this takes\n\n{USAGE}")),
        }
    }
    let out = out.ok_or("--out says where to write the tree, and there is no default")?;
    if inputs.is_empty() {
        return Err(format!("there is nothing to merge\n\n{USAGE}"));
    }

    let report = merge_trees(&inputs, &out)?;
    print!("{report}");
    if let Some(file) = listing {
        std::fs::write(&file, report.listing())
            .map_err(|why| format!("{}: {why}", file.display()))?;
        println!("headers: the list of files is in {}", file.display());
    }
    if report.problems.is_empty() {
        println!("headers: every file gives every release back");
        return Ok(ExitCode::SUCCESS);
    }
    // The tree is written either way, because looking at what is wrong with it means reading it.
    for problem in &report.problems {
        eprintln!("rucc-headers: {problem}");
    }
    eprintln!("rucc-headers: {} problems, so this tree is not one to ship", report.problems.len());
    Ok(ExitCode::FAILURE)
}

/// The value after a flag.
fn value<'a>(args: &'a [String], at: &mut usize, flag: &str) -> Result<&'a str, String> {
    let value = args.get(*at).ok_or(format!("{flag} wants a value after it"))?;
    *at += 1;
    Ok(value)
}

/// One `2.28=<dir>` argument.
fn release(text: &str) -> Result<Input, String> {
    let (version, dir) =
        text.split_once('=').ok_or(format!("--release wants <version>=<dir> and got {text}"))?;
    let minor = version
        .strip_prefix("2.")
        .ok_or(format!("{version} is not a glibc release; they are all 2.something"))?;
    let minor = minor.parse().map_err(|_| format!("{version} is not a glibc release"))?;
    if dir.is_empty() {
        return Err(format!("--release {version} has no directory after the ="));
    }
    Ok(Input { minor, root: PathBuf::from(dir) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_argument_is_a_version_and_a_directory() {
        let input = release("2.28=/tmp/install-2.28").expect("a release");
        assert_eq!(input.minor, 28);
        assert_eq!(input.root, PathBuf::from("/tmp/install-2.28"));
    }

    #[test]
    fn what_is_not_a_release_argument() {
        for text in ["2.28", "28=/tmp/x", "3.1=/tmp/x", "2.x=/tmp/x", "2.28="] {
            assert!(release(text).is_err(), "{text}");
        }
    }

    #[test]
    fn the_usage_says_what_the_arguments_are() {
        for flag in ["--release", "--out", "--listing", "--help"] {
            assert!(USAGE.contains(flag), "{flag}");
        }
    }

    #[test]
    fn nothing_to_merge_is_told_rather_than_done() {
        let why = run(&["--out".to_owned(), "/tmp/whatever".to_owned()]).expect_err("no input");
        assert!(why.contains("nothing to merge"), "{why}");
        let why = run(&[]).expect_err("no output either");
        assert!(why.contains("--out"), "{why}");
    }

    #[test]
    fn an_argument_it_does_not_take_is_refused_with_the_usage() {
        let why = run(&["--merge-everything".to_owned()]).expect_err("not an argument");
        assert!(why.contains("is not an argument this takes"), "{why}");
        assert!(why.contains("--release"), "{why}");
    }
}
