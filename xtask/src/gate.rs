//! What `cargo xtask ci` runs, and how much of it runs at the same time.
//!
//! The gate is thirty odd checks and it used to be a list of them run one after another. Measured
//! on an eight core machine with everything already built, that list is around ten minutes, and
//! almost none of it is eight cores busy: `cargo doc` is rustdoc walking a deep chain of crates,
//! the eight checks that compile C programs and run them are each one process doing one thing at a
//! time, and the dozen checks that read the tree finish in under a second each and were still
//! taking their turn in a queue.
//!
//! So the order here is by what a check needs rather than by what it is about. There are three
//! things a check can need, and two checks that need different things can run together.
//!
//! # The build directory
//!
//! Cargo takes a lock on its target directory for the length of a build, so two cargo commands
//! against one directory do not overlap, they queue. Every check that builds something is
//! therefore one lane, run in order, and that lane is the spine of the gate.
//!
//! The documentation is the exception, and it is the exception worth making: it is the single
//! longest check in the gate, around four minutes, and none of that is compiling. It is given a
//! target directory of its own so that it runs beside the spine instead of behind it, which costs
//! a second copy of the dependencies on disk and saves the whole four minutes. That is the one
//! place this trades space for time, and it is worth saying out loud rather than discovering from
//! a full disk.
//!
//! # A compiler to run programs through
//!
//! The eight checks that compile C and run it all want `target/release/rucc`, so they cannot start
//! until the spine has built it. Once it exists they want nothing from each other, they each work
//! in a directory of their own under `target/`, and they are started all at once. That is the
//! moment the compiler is built and not the moment the spine is finished, so they run beside the
//! test run rather than after it, which is the difference between a warm gate of two minutes and
//! one of ninety seconds. They are told where the compiler is for the same reason: left to
//! themselves each would ask cargo to build it, and eight of those waiting behind the tests for
//! the lock would put the lane back where it started.
//!
//! # Nothing at all
//!
//! The rest read files and compare them, and `cargo fmt` is one of them because rustfmt reads the
//! tree and builds nothing. They start immediately and are usually finished before the spine has
//! got past clippy.
//!
//! # Why each check is a process rather than a call
//!
//! Because two checks printing at once is two checks nobody can read. Each one is this same binary
//! started again with the task name, its output goes to a file under `target/gate/`, and the
//! output is printed when the check finishes, under a line saying which check it was and how long
//! it took. That also gives the gate something it never had, which is an answer to where the time
//! goes: every run ends with the checks in the order they took.
//!
//! The cost is one process per check, which is a few milliseconds against checks measured in
//! seconds, and the gain is that a failure is a block of output with a name on it.
//!
//! # What a failure does
//!
//! The spine stops at its first failure, because there is no sense compiling a corpus with a
//! compiler that did not pass clippy, and the checks that need a compiler are then not started at
//! all. Everything already running is waited for and reported, and every check that did run is
//! reported whether it passed or not, so one run tells you everything that is wrong rather than
//! the first thing.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use crate::{Error, Result, indent, root};

/// One check, as the command that runs it.
struct Step {
    /// What it is called in the report, which is what you would type to run it on its own.
    name: String,
    /// The program and everything after it.
    argv: Vec<String>,
    /// What the check needs in its environment, which is the documentation's own target directory
    /// and the denial of its warnings, and nothing at all for every other check.
    env: Vec<(String, String)>,
}

impl Step {
    /// A check the tree owns, run by starting this binary again with the task name.
    ///
    /// `cargo xtask <name>` would do the same thing and would go through cargo to do it, which
    /// means taking the build directory's lock to work out that there is nothing to build. That is
    /// the one lock this whole arrangement is about, so the gate reaches the task the way the
    /// shell would if the binary were on the path.
    fn task(name: &str, args: &[&str]) -> Result<Step> {
        let me = std::env::current_exe()
            .map_err(|e| Error::Io(format!("could not find this binary: {e}")))?;
        let mut argv = vec![me.display().to_string(), name.to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        Ok(Step { name: name.to_owned(), argv, env: Vec::new() })
    }

    /// One of the checks the toolchain provides rather than one of ours.
    fn cargo(name: &str, args: &[&str]) -> Step {
        let mut argv = vec!["cargo".to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        Step { name: name.to_owned(), argv, env: Vec::new() }
    }

    /// The same with something added to the environment.
    fn with(mut self, key: &str, value: &str) -> Step {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }
}

/// How a check ended.
#[derive(PartialEq, Eq)]
enum Verdict {
    /// It ran and found nothing.
    Passed,
    /// It ran and found something.
    Failed,
    /// It could not run, which is a sentence about this machine rather than about the tree.
    ///
    /// The check itself says so by exiting with `COULD_NOT_RUN`, which is what the two halves of
    /// the error type have always meant and never had to say out loud before the gate started
    /// every check as a process. A machine with no container cannot run the eight that compile C
    /// and run it, and a copy of the tree that is not a checkout cannot run `paths`, and both of
    /// those are the same answer.
    Skipped,
}

/// What a check left behind.
struct Done {
    /// The check's name, so that a block of output has one.
    name: String,
    /// How long it took, in whole seconds, which is the unit everything here is measured in.
    seconds: u64,
    /// How it ended.
    verdict: Verdict,
    /// Everything it printed, both streams in the order it wrote them.
    output: String,
}

impl Done {
    /// Whether the gate can carry on past it, which a check that could not run does not stop.
    fn ok(&self) -> bool {
        self.verdict != Verdict::Failed
    }
}

/// Where a check's output goes while it is running.
///
/// A file rather than a pipe, because a pipe has to be read while the check is running or the
/// check stops when it fills, and reading a dozen pipes is a thread each. A file is one open call
/// and the kernel does the rest, and it leaves the output on disk for anybody who wants to look at
/// a check that passed.
fn log_of(name: &str) -> Result<PathBuf> {
    let dir = root().join("target").join("gate");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", dir.display())))?;
    Ok(dir.join(format!("{}.log", name.replace(' ', "-"))))
}

/// Starts a check and answers what is needed to wait for it.
fn start(step: &Step) -> Result<(Instant, Child, PathBuf)> {
    let path = log_of(&step.name)?;
    let out = File::create(&path)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", path.display())))?;
    let err = out
        .try_clone()
        .map_err(|e| Error::Io(format!("could not share {}: {e}", path.display())))?;
    let (program, args) = step.argv.split_first().expect("a step is a program and its arguments");
    let mut command = Command::new(program);
    command.args(args).current_dir(root()).stdin(Stdio::null()).stdout(out).stderr(err);
    for (key, value) in &step.env {
        command.env(key, value);
    }
    let child =
        command.spawn().map_err(|e| Error::Io(format!("could not run {}: {e}", step.name)))?;
    Ok((Instant::now(), child, path))
}

/// Waits for one check and reads back what it printed.
fn wait(step: &Step, began: Instant, mut child: Child, path: PathBuf) -> Result<Done> {
    let status =
        child.wait().map_err(|e| Error::Io(format!("could not wait for {}: {e}", step.name)))?;
    let mut output = String::new();
    File::open(&path)
        .and_then(|mut file| file.read_to_string(&mut output))
        .map_err(|e| Error::Io(format!("could not read {}: {e}", path.display())))?;
    let verdict = match status.code() {
        Some(0) => Verdict::Passed,
        Some(crate::COULD_NOT_RUN) => Verdict::Skipped,
        _ => Verdict::Failed,
    };
    Ok(Done { name: step.name.clone(), seconds: began.elapsed().as_secs(), verdict, output })
}

/// Runs every one of them at once and answers them in the order they were given.
fn together(steps: &[Step]) -> Result<Vec<Done>> {
    let mut running = Vec::new();
    for step in steps {
        running.push(start(step)?);
    }
    let mut done = Vec::new();
    for (step, (began, child, path)) in steps.iter().zip(running) {
        done.push(wait(step, began, child, path)?);
    }
    Ok(done)
}

/// Runs them one after another and stops at the first that fails.
fn in_order(steps: &[Step]) -> Result<Vec<Done>> {
    let mut done = Vec::new();
    for step in steps {
        let (began, child, path) = start(step)?;
        let one = wait(step, began, child, path)?;
        let failed = !one.ok();
        done.push(one);
        if failed {
            break;
        }
    }
    Ok(done)
}

/// The checks that build nothing and can start the moment the gate does.
///
/// `cargo fmt` is here because rustfmt reads the tree and builds nothing, so it wants the build
/// directory as little as the checks around it do. `paths` is first of them for the reason it was
/// first of the whole list: it is the one check about the tree rather than about the code in it,
/// and what it catches stops the Windows job before that job can report anything.
fn tree() -> Result<Vec<Step>> {
    Ok(vec![
        Step::task("paths", &[])?,
        Step::task("layers", &[])?,
        Step::task("style", &[])?,
        Step::task("thresholds", &[])?,
        Step::task("malformed", &[])?,
        Step::task("interpose", &[])?,
        Step::task("version", &[])?,
        Step::cargo("fmt", &["fmt", "--all", "--check"]),
    ])
}

/// The front of the spine, which is everything the checks that run programs are waiting for.
///
/// Clippy first because it is the cheapest thing in the gate that says no, six seconds against the
/// tests' minute or two, and there is no sense compiling a corpus with a compiler that did not
/// pass it. The compiler second because it is what the eight checks after this need and nothing
/// else in the gate does, and getting it built is what lets them start while the tests are still
/// running.
fn head() -> Vec<Step> {
    vec![
        Step::cargo(
            "clippy",
            &["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"],
        ),
        Step::cargo("rucc", &["build", "--release", "-p", "rucc"]),
    ]
}

/// The rest of the spine, which is the longest check in the gate and nothing else.
fn tail() -> Vec<Step> {
    vec![Step::cargo("test", &["test", "--workspace", "--all-features"])]
}

/// The documentation, in a target directory of its own so that it runs beside the spine.
///
/// The warnings are denied here, which they were not before this: the gate ran `cargo doc` with
/// rustdoc's lints left at their default, which is to warn, so a public item linking to a private
/// one printed a warning into a log nobody reads and the gate went green. That is how
/// tamnd/rucc#1064 put sixteen such links on main. A lint that does not fail is a lint that is off.
fn docs() -> Step {
    let into = root().join("target").join("gate-doc");
    Step::cargo("doc", &["doc", "--workspace", "--all-features", "--no-deps"])
        .with("CARGO_TARGET_DIR", &into.display().to_string())
        .with("RUSTDOCFLAGS", "-D warnings")
}

/// The checks that read a generated file and say whether it still matches what generates it.
///
/// They go after the whole spine rather than beside it because each of them starts `cargo run`,
/// which wants the lock the spine is holding for as long as the tests are building. After it they
/// are a few seconds together.
fn generated() -> Result<Vec<Step>> {
    ["targets", "abi-corpus", "abi-signatures", "link-lines", "provenance"]
        .iter()
        .map(|name| Step::task(name, &["--check"]))
        .collect()
}

/// The checks that compile a C program and run it, in the order they take.
///
/// Every one of them wants `target/release/rucc` and none of them wants anything from another, and
/// they each work in a directory of their own under `target/` and under `/tmp`, so they are
/// started together as soon as the compiler exists. That is while the tests are still running, and
/// it is worth the contention: they are one process each doing one thing at a time, so eight of
/// them beside a test run is a machine kept busy rather than a machine oversubscribed. The order
/// is still cheapest first, because that is the order the report reads best in when they all pass
/// and the order the failures arrive in when they do not.
///
/// `sqlite` is last because it is the longest by a wide margin, nine megabytes of C compiled twice,
/// and because it is the only one of them that does nothing at all on a machine without the
/// amalgamation on it.
///
/// They are also told where the compiler is, so that none of them goes back to cargo for it.
fn programs() -> Result<Vec<Step>> {
    // Each of these would otherwise ask cargo to build the compiler before using it, and asking
    // while the tests hold the build directory's lock is eight waits for a build that already
    // happened. The spine built it, so the lane is told where rather than left to find out.
    let built = root().join("target").join("release").join("rucc");
    let built = built.display().to_string();
    ["unwind", "quad", "wide", "fuzz", "safety", "dso", "accounting", "sqlite"]
        .iter()
        .map(|name| Ok(Step::task(name, &[])?.with("RUCC_XTASK_COMPILER", &built)))
        .collect()
}

/// Prints one check's line, and its output where there is a reason to read it.
///
/// A check that passed and said nothing gets one line, which is what almost all of them are. A
/// check that failed gets everything it printed, indented, so that its output reads as its output
/// and not as the gate's, and so does a check that could not run, because the reason it could not
/// is the only useful thing it has to say.
fn report(one: &Done) {
    let verdict = match one.verdict {
        Verdict::Passed => "ok",
        Verdict::Failed => "FAILED",
        Verdict::Skipped => "did not run",
    };
    println!("xtask: ci {} {} in {}s", one.name, verdict, one.seconds);
    if one.verdict != Verdict::Passed && !one.output.trim().is_empty() {
        print!("{}", indent(one.output.trim_end()));
    }
}

/// What to print about the checks that did not run.
///
/// Always a line, including when there is nothing to report. `cargo xtask ci` saying nothing about
/// a check it skipped is how the command came to read as a complete account of the tree when it was
/// not one, so the case where everything ran says so out loud rather than staying quiet and letting
/// the absence of bad news stand in for good news.
fn accounted(skipped: &[&str]) -> String {
    if skipped.is_empty() {
        return "xtask: ci ran every check it has".to_owned();
    }
    format!("xtask: ci did not run {}, each of which said why above", skipped.join(", "))
}

/// The line that says where the time went, which is the four slowest checks and the whole run.
///
/// Four because that is enough to see the shape of a run and short enough to read, and because on
/// a machine where everything is built the gate has four checks that are most of it and thirty
/// that are the rest.
///
/// The whole run rather than the sum of the checks, because the checks overlap now and a sum of
/// them would be longer than the run and would read as though the gate were slower than it is.
fn spent(done: &[Done], whole: u64) -> String {
    let mut slowest: Vec<&Done> = done.iter().collect();
    slowest.sort_by_key(|one| std::cmp::Reverse(one.seconds));
    let named: Vec<String> =
        slowest.iter().take(4).map(|one| format!("{} {}s", one.name, one.seconds)).collect();
    format!("xtask: ci took {whole}s, the longest being {}", named.join(", "))
}

/// Runs the gate.
///
/// # Errors
///
/// [`Error::Failed`] naming every check that ran and said no, which is every one of them rather
/// than the first, because a run that stops at the first failure is a run you have to do again to
/// find the second.
pub(crate) fn ci() -> Result<()> {
    let began = Instant::now();
    let tree = tree()?;
    let head = head();
    let tail = tail();
    let programs = programs()?;
    let docs = [docs()];
    let mut done: Vec<Done> = Vec::new();

    // Four lanes. The documentation and the checks that read the tree need nothing and start at
    // once. This thread takes the spine, because the spine is what everything else is waiting on,
    // and it tells the fourth lane when the compiler is there for it to use.
    let (told, hear) = std::sync::mpsc::channel::<bool>();
    let (documented, walked, ran, built) = std::thread::scope(|scope| {
        let documented = scope.spawn(|| together(&docs));
        let walked = scope.spawn(|| together(&tree));
        let ran = scope.spawn(move || match hear.recv() {
            Ok(true) => together(&programs),
            // Either the compiler did not build or the spine stopped before it got that far, and
            // in both cases the failure is the spine's to report rather than eight more of it.
            _ => Ok(Vec::new()),
        });
        let head = in_order(&head);
        let ready = matches!(&head, Ok(done) if done.iter().all(Done::ok));
        // Before anything that could return early, because the lane on the other end is waiting to
        // hear and a gate that forgot to tell it would be a gate that never finishes. Whether the
        // message arrived is not worth asking: the only reader is a lane this scope has not joined
        // yet, so it is still there.
        let _ = told.send(ready);
        let built = head.and_then(|mut done| {
            if ready {
                done.extend(in_order(&tail)?);
            }
            Ok(done)
        });
        (documented.join(), walked.join(), ran.join(), built)
    });
    let joined = |what: &str, result: std::thread::Result<Result<Vec<Done>>>| match result {
        Ok(inner) => inner,
        Err(_) => Err(Error::Io(format!("the {what} lane stopped without saying why"))),
    };
    done.extend(joined("documentation", documented)?);
    done.extend(joined("tree", walked)?);
    done.extend(built?);
    done.extend(joined("program", ran)?);

    // The generated files last, because each of them starts `cargo run` and the spine has only
    // just let go of the lock. Only if everything that builds built, since a generator run through
    // a workspace that does not compile is a second failure about the first one.
    let stopped = done.iter().any(|one| one.verdict == Verdict::Failed);
    if !stopped {
        done.extend(together(&generated()?)?);
    }

    for one in &done {
        report(one);
    }
    let mut skipped: Vec<&str> = done
        .iter()
        .filter(|one| one.verdict == Verdict::Skipped)
        .map(|one| one.name.as_str())
        .collect();
    if stopped {
        skipped.push("anything that needed the build");
    }
    println!("{}", accounted(&skipped));
    println!("{}", spent(&done, began.elapsed().as_secs()));

    let problems: Vec<String> = done
        .iter()
        .filter(|one| one.verdict == Verdict::Failed)
        .map(|one| format!("{} failed, and said so in target/gate/{}.log", one.name, one.name))
        .collect();
    if problems.is_empty() {
        return Ok(());
    }
    Err(Error::Failed { task: "ci", problems })
}

#[cfg(test)]
mod tests {
    use super::{Done, Verdict, accounted, spent};

    /// Nothing about a check except what the report reads off it.
    fn done(name: &str, seconds: u64) -> Done {
        Done { name: name.to_owned(), seconds, verdict: Verdict::Passed, output: String::new() }
    }

    #[test]
    fn a_run_that_checked_everything_says_so() {
        // The point of the line. Silence here is what let the command read as complete.
        assert_eq!(accounted(&[]), "xtask: ci ran every check it has");
    }

    #[test]
    fn a_check_that_did_not_run_is_named() {
        let line = accounted(&["safety"]);
        assert_eq!(line, "xtask: ci did not run safety, each of which said why above");
    }

    #[test]
    fn every_check_that_did_not_run_is_named() {
        let line = accounted(&["safety", "accounting"]);
        assert!(line.contains("safety, accounting"), "{line}");
    }

    /// A check that could not run is not a check that failed, which is the whole reason the two
    /// have different exit codes and is what keeps a machine with no container off the hook.
    #[test]
    fn a_check_that_could_not_run_does_not_stop_the_gate() {
        let one = Done {
            name: "dso".to_owned(),
            seconds: 1,
            verdict: Verdict::Skipped,
            output: String::new(),
        };
        assert!(one.ok());
    }

    #[test]
    fn a_check_that_failed_stops_the_gate() {
        let one = Done {
            name: "clippy".to_owned(),
            seconds: 1,
            verdict: Verdict::Failed,
            output: String::new(),
        };
        assert!(!one.ok());
    }

    #[test]
    fn the_run_is_summed_up_by_its_slowest_checks_longest_first() {
        let line = spent(&[done("style", 1), done("doc", 248), done("test", 86)], 300);
        assert_eq!(line, "xtask: ci took 300s, the longest being doc 248s, test 86s, style 1s");
    }

    /// The whole run rather than the sum of the checks, because the checks overlap and a sum of
    /// them would be larger than the run and read as though the gate were slower than it is.
    #[test]
    fn the_time_reported_is_the_run_and_not_the_sum_of_what_ran_in_it() {
        let line = spent(&[done("doc", 248), done("test", 86)], 250);
        assert!(line.starts_with("xtask: ci took 250s"), "{line}");
    }

    #[test]
    fn a_report_of_nothing_still_names_the_run() {
        assert_eq!(spent(&[], 0), "xtask: ci took 0s, the longest being ");
    }
}
