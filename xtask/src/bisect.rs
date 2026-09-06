//! The bisection driver.
//!
//! Section 4.5 of `spec/optimizer/04-pass-manager.md` asks for this by name. `-fpass-fuel` and
//! `-fpass-fuel-global` are the mechanism, and the mechanism on its own is a flag somebody has
//! to remember to halve by hand at two in the morning. This is the halving.
//!
//! The shape is the one that document describes: two searches. The first is over the whole
//! pipeline, and it says which transformation out of all of them is the wrong one. The second is
//! over one pass, and it says which of that pass's rewrites it is. Each is about twenty
//! compilations, and twenty plus twenty beats one search over a space nobody knows the shape of.
//!
//! What this does not do is compile anything itself. It runs a command somebody else wrote, with
//! `RUCC_FUEL` set to the flag for that step, and reads the exit status: zero is a program that
//! behaved and anything else is one that did not. That is deliberate. A miscompilation is found
//! by building a program, running it and comparing what it printed, and every part of that is
//! specific to the program. A driver that tried to do it would be a build system, and the one
//! thing it would not be able to express is whatever the next bug needs.

use std::process::{Command, Stdio};

use crate::{Error, Result};

/// Where the doubling stops.
///
/// A search that gets this far is not going to finish, and the honest answer is that the command
/// never behaves rather than a number arrived at by exhaustion. It is also well past the number
/// of transformations any single translation unit has in it.
const LIMIT: u32 = 1 << 24;

/// Runs the bisection.
pub(crate) fn bisect(args: &[String]) -> Result<()> {
    let mut pass: Option<String> = None;
    let mut loud = false;
    let mut rest: &[String] = &[];
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--pass" => {
                let name = args.get(at + 1).ok_or_else(|| usage("--pass wants a pass name"))?;
                pass = Some(name.clone());
                at += 2;
            }
            "-v" | "--verbose" => {
                loud = true;
                at += 1;
            }
            "--" => {
                rest = &args[at + 1..];
                break;
            }
            other => return Err(usage(&format!("`{other}` is not an option this task has"))),
        }
    }
    if rest.is_empty() {
        return Err(usage("there is no command after `--` to run"));
    }

    let run = Run { argv: rest, pass: pass.as_deref(), loud };
    // Both ends first, because a search whose ends do not disagree is a search over nothing and
    // the answer it would give is the end it started at. This is also where the two common
    // mistakes are caught: a command that fails for its own reasons, and a bug that is not in
    // the optimizer at all.
    if !run.behaves(0)? {
        return Err(Error::Io(format!(
            "the command already fails with {}, so whatever is wrong is not a rewrite this \
             finds. Check the command, and check the program at -O0.",
            run.flag(0)
        )));
    }
    if run.behaves_unlimited()? {
        return Err(Error::Io(
            "the command succeeds with the optimizer turned all the way up, so there is nothing \
             to bisect"
                .to_owned(),
        ));
    }

    // A bound to search inside, found by doubling, because the number of rewrites in a file is
    // not something anyone knows in advance and asking for it as an argument would be asking
    // for a number to guess.
    // The last step of the doubling that behaved is the bottom of the search, so the answer to
    // it is not asked for twice.
    let mut lo = 0;
    let mut hi = 1;
    while run.behaves(hi)? {
        lo = hi;
        if hi >= LIMIT {
            return Err(Error::Io(format!(
                "the command still behaves at {hi} transformations, which is further than this \
                 goes. Either the failure is not deterministic or it is not the optimizer."
            )));
        }
        hi = hi.saturating_mul(2).min(LIMIT);
    }
    // Behaves at `lo` and does not at `hi`, and the two stay that way, so the answer is `hi` the
    // moment they are next to each other.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if run.behaves(mid)? {
            lo = mid;
        } else {
            hi = mid;
        }
    }

    println!();
    match run.pass {
        Some(name) => {
            println!("xtask: rewrite {hi} of the {name} pass is the one that breaks it");
            println!("xtask: the command behaves at -fpass-fuel={name}={lo} and does not at {hi}");
            println!(
                "xtask: `-fdump-ir=before-{name} -fdump-ir=after-{name}` with the fuel at {hi} \
                 shows it"
            );
        }
        None => {
            println!("xtask: transformation {hi} of the whole pipeline is the one that breaks it");
            println!("xtask: the command behaves at {} and does not at {hi}", run.flag(lo));
            println!(
                "xtask: `-fopt-info=all` at {lo} and at {hi} differ by one line, which names the \
                 pass"
            );
            println!(
                "xtask: then `cargo xtask bisect --pass <that pass> -- <command>` for the rewrite"
            );
        }
    }
    Ok(())
}

/// What is being searched, and how to ask it a question.
struct Run<'a> {
    /// The command, which is run once per step.
    argv: &'a [String],
    /// The pass being searched, or `None` for the whole pipeline.
    pass: Option<&'a str>,
    /// Whether the command's own output is shown. Off by default, because twenty compilations
    /// of a program that prints its answer would bury the search in the answers.
    loud: bool,
}

impl Run<'_> {
    /// The flag one step of the search is asking about.
    fn flag(&self, fuel: u32) -> String {
        match self.pass {
            Some(name) => format!("-fpass-fuel={name}={fuel}"),
            None => format!("-fpass-fuel-global={fuel}"),
        }
    }

    /// Whether the command is happy with this much fuel.
    fn behaves(&self, fuel: u32) -> Result<bool> {
        self.once(&self.flag(fuel))
    }

    /// Whether the command is happy with no limit at all, which is the compilation somebody
    /// actually asked for and the one that is wrong.
    fn behaves_unlimited(&self) -> Result<bool> {
        self.once("")
    }

    /// One compilation and one run of whatever the command does with it.
    fn once(&self, flag: &str) -> Result<bool> {
        let (first, args) = self.argv.split_first().expect("the caller checked it is not empty");
        let mut cmd = Command::new(first);
        cmd.args(args).env("RUCC_FUEL", flag);
        if !self.loud {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        let status = cmd
            .status()
            .map_err(|e| Error::Io(format!("could not run `{}`: {e}", self.argv.join(" "))))?;
        let ok = status.success();
        println!(
            "xtask: {:<28} {}",
            if flag.is_empty() { "no limit" } else { flag },
            if ok { "behaves" } else { "does not" }
        );
        Ok(ok)
    }
}

/// A mistake in how the task was asked for, with the shape of the answer attached.
fn usage(why: &str) -> Error {
    Error::Io(format!(
        "{why}\n\
         \n\
         usage: cargo xtask bisect [--pass <name>] [-v] -- <command> [args...]\n\
         \n\
         The command is run once per step with RUCC_FUEL holding the flag for that step, and it\n\
         has to put that flag on the rucc command line. It exits zero when the program behaves\n\
         and non-zero when it does not, which is usually a compile, a run and a comparison:\n\
         \n\
         \x20 #!/bin/sh\n\
         \x20 rucc -O2 $RUCC_FUEL bug.c -o bug || exit 1\n\
         \x20 test \"$(./bug)\" = \"$(cat bug.expected)\"\n\
         \n\
         Without --pass the search is over the whole pipeline and the answer is which\n\
         transformation out of all of them is wrong. With it the search is inside one pass and\n\
         the answer is which of its rewrites it is."
    ))
}
