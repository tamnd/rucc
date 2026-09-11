//! The memory safety suite.
//!
//! Design: `spec/safe-memory/03-bug-model.md` section 3.7 and `spec/safe-memory/16-milestones.md`
//! milestone S1.
//!
//! Every other check in this repository asks what the compiler produced. This one asks what the
//! program did, because a memory safety monitor is only worth what it catches when it is running,
//! and a check that is emitted, lowered, linked and then silently never reached looks identical
//! from the outside to one that works. So each case here is a whole C program with a verdict
//! written at the top of it, and the suite compiles it, links it against the runtime, runs it, and
//! holds what came out to what the file said would.
//!
//! # Why this is not a `#[test]`
//!
//! Because it needs a machine that the compiler emits code for. The only back end is x86-64, and a
//! developer on an arm mac cannot run what it produces, so a test in the workspace would either
//! fail there or skip there, and a suite that skips is a suite nobody notices has stopped running.
//! An `xtask` is a thing somebody asks for, and asking for it on a machine that cannot do it gets
//! a sentence explaining what to install rather than a green tick.
//!
//! On an x86-64 Linux machine, which is what CI is, the programs run directly. Anywhere else they
//! run in a container, which is one `docker run` for the whole suite rather than one per case.
//!
//! # Why the programs declare their own `malloc`
//!
//! rucc has no built-in system include directories, so a case that said `#include <stdlib.h>`
//! would be testing whichever headers the machine happens to have. Four declarations at the top of
//! a case are the same four declarations everywhere.
//!
//! # The mixed link
//!
//! A case can say `links: name`, and it is then built against `tests/safety/lib/name.c` compiled
//! into a shared object by the system compiler. That library is never built by rucc and is never
//! instrumented, which is the entire point of it: document 10 section 10.7 says the configuration
//! that matters in practice is an instrumented program against libraries nobody rebuilt, and the
//! only way to show that works is to link against one.
//!
//! A case can also say `summary: text`, and it is then compiled a second time with
//! `--emit=safety-summary` and the report has to contain that text. It is what holds the build's
//! account of what it trusts to the same program the run is checking, so the two cannot drift.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::indent;
use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, root, staticlib};

/// The tier the suite is run at.
///
/// Milestone S1 is `detect` and nothing else. `enforce` and `kernel` get their own runs when the
/// milestones that define them arrive, and each will want its own column in the expectations
/// rather than a second pass over these.
pub(crate) const TIER: &str = "-fsafety=detect";

/// The optimization level the suite is run at.
///
/// None, because S1's whole point is that the checks are correct before anything tries to remove
/// them. `accounting` is where they are run at `-O2`, and it is a different question: there the
/// interesting failure is a check that was eliminated when it should not have been, and the
/// expectations that answer it are these same files read a second time.
pub(crate) const LEVEL: &str = "-O0";

/// The optimization level the differential accounting runs at.
///
/// The one people ship at, and the one where every pass that could take a check out has run.
pub(crate) const OPTIMIZED: &str = "-O2";

/// What turns the elimination off without turning anything else off.
///
/// Build A of section 14.3 is "all checks inserted, no elimination at all", and it has to be the
/// same build in every other respect or a divergence is not evidence about elimination. Dropping
/// to `-O0` would change the code the checks are in as well, so the passes are disabled by name and
/// the rest of the pipeline runs exactly as it does in build B.
///
/// Every pass that takes a check out belongs here. A pass left off the list is a pass whose
/// removals are in both builds, and a comparison of two builds that both did the thing has nothing
/// to say about whether the thing was right.
pub(crate) const NO_ELIMINATION: &[&str] = &["-fdisable-discharge", "-fdisable-hoist"];

/// The line every report starts with, which is what says one happened at all.
pub(crate) const BANNER: &str = "rucc: memory safety violation";

/// What a case says should happen to it.
#[derive(Debug)]
enum Verdict {
    /// A report, naming this judgement, and every one of these substrings in it.
    Refuse { judgement: u8, says: Vec<String> },
    /// Nothing at all, and an exit status of zero.
    Allow,
}

/// One program and what it expects.
#[derive(Debug)]
struct Case {
    /// The file name without its extension, which is what a message names.
    name: String,
    /// The file itself.
    path: PathBuf,
    /// Which row of `spec/safe-memory/03-bug-model.md` this is, or which idiom of section 3.5.
    row: String,
    /// What should happen.
    verdict: Verdict,
    /// The issue that will make this case pass, for a row nothing catches yet.
    ///
    /// A case with one of these is run backwards: the refusal it describes must *not* happen, and
    /// the suite fails when it starts happening. That is section 15.7's rule about not deleting a
    /// test to make CI green, applied to a test that has not started passing rather than one that
    /// has stopped.
    gap: Option<String>,
    /// Flags this one case is compiled with on top of the tier and the level.
    ///
    /// For a check the command line turns on, which is every check that would refuse a program C
    /// permits. A suite that only ever compiled one way could not hold both halves of a flag like
    /// that to anything, and a second suite run for every flag would cost a build each.
    flags: Vec<String>,
    /// Libraries in `tests/safety/lib` this case is linked against, uninstrumented.
    ///
    /// Document 10 section 10.7's mixed link, which is the configuration that matters most in
    /// practice and the one milestone S2 exits on. Each name is built by the system compiler into
    /// a shared object, with nothing said to it about the monitor, and the case is linked against
    /// the result. A case with none of these is an ordinary program and links against the runtime
    /// alone.
    links: Vec<String>,
    /// A substring `--emit=safety-summary` has to print for this case.
    ///
    /// Section 10.2's artifact, held to something. A case that links against a library nobody
    /// instrumented should be able to say so in its own summary, and asserting on the summary is
    /// how that stops being a claim.
    summary: Vec<String>,
    /// The issue that will let this case compile at all, for a construct the compiler cannot
    /// lower yet.
    ///
    /// Run backwards like a gap, one step earlier: the compilation must fail, and the suite fails
    /// when it starts succeeding. A program that cannot be built is not evidence about the
    /// monitor, so a blocked case is not counted as a row covered, but writing the expectation
    /// down now is what stops the row from being forgotten between here and the day it builds.
    blocked: Option<String>,
}

/// What one program actually did.
#[derive(Debug)]
pub(crate) struct Ran {
    /// Everything it wrote, on both streams.
    pub(crate) output: String,
    /// What it exited with, or nothing when it did not get as far as being linked.
    pub(crate) status: Option<i32>,
}

/// Runs every program in `tests/safety` and holds each to the verdict written in it.
///
/// # Errors
///
/// [`Error::Failed`] with one line per case that did not do what it said, and [`Error::Io`] when
/// the suite could not be run at all, which is a missing target library or no way to run an
/// x86-64 Linux program.
pub(crate) fn safety() -> Result<()> {
    let cases = cases()?;
    let runner = Runner::find("this suite")?;
    println!("safety: {} programs, {TIER} {LEVEL}, {runner}", cases.len());

    let plan = Plan { level: LEVEL, without: &[], dir: "safety", summaries: true };
    let work = build(&cases, &plan)?;
    let ran = read(&runner.run(&work, "the suite")?);

    let mut problems = Vec::new();
    let mut refused = 0;
    let mut silent = 0;
    let mut gaps = 0;
    let mut blocked = 0;
    for case in &cases {
        if case.blocked.is_some() {
            blocked += 1;
            continue;
        }
        let Some(ran) = ran.get(&case.name) else {
            problems.push(format!("{}: did not run", case.name));
            continue;
        };
        match case.judge(ran) {
            Err(problem) => problems.push(problem),
            Ok(()) if case.gap.is_some() => gaps += 1,
            Ok(()) => match case.verdict {
                Verdict::Refuse { .. } => refused += 1,
                Verdict::Allow => silent += 1,
            },
        }
    }
    let counted = coverage(&cases);
    println!(
        "safety: {refused} refused, {silent} silent, {gaps} known gaps, {blocked} not yet \
         buildable, {counted} rows covered"
    );
    if problems.is_empty() {
        return Ok(());
    }
    Err(Error::Failed { task: "safety", problems })
}

/// Runs every program twice at `-O2`, once with the elimination and once without, and holds the
/// two runs to each other.
///
/// Design: `spec/safe-memory/14-verification.md` section 14.3, which calls this the highest value
/// test in the specification. Section 14.2's first gap is that the analyses feeding the removal
/// rules are asserted rather than verified: a rule can be proved correct and still remove a
/// necessary check, because what the rule is applied to is a context an ordinary dataflow walk
/// worked out. Nothing else in this project can observe that. A proof cannot, because the proof is
/// about the rule. The suite at `-O0` cannot, because nothing has been removed there.
///
/// So the two builds are compared instead of being judged separately. Build A has every check the
/// instrumentation inserted, build B is the ordinary optimized build, and a program that reports
/// in A and not in B is a check that elimination took out and that would have fired. The
/// specification writes the assertion as `reports(A)` being a subset of `reports(B)` and says to
/// investigate a difference in either direction, so both directions are reported here.
///
/// Both builds are also held to the case's own verdict, which is the same expectations read at a
/// level they were never read at before. A divergence and a wrong verdict are different findings
/// and are counted apart: the first says elimination is unsound, the second says the case does not
/// do at `-O2` what it does at `-O0`, which could be either half of the compiler.
///
/// # Errors
///
/// [`Error::Failed`] with one line per divergence and one per wrong verdict, and [`Error::Io`]
/// when the suite could not be run at all.
pub(crate) fn accounting() -> Result<()> {
    let cases = cases()?;
    let runner = Runner::find("this suite")?;
    println!("accounting: {} programs, {TIER} {OPTIMIZED}, twice, {runner}", cases.len());

    let all =
        Plan { level: OPTIMIZED, without: NO_ELIMINATION, dir: "checks-all", summaries: false };
    let cut = Plan { level: OPTIMIZED, without: &[], dir: "checks-cut", summaries: false };
    let ran_all = read(&runner.run(&build(&cases, &all)?, "the suite")?);
    let ran_cut = read(&runner.run(&build(&cases, &cut)?, "the suite")?);

    let mut problems = Vec::new();
    let mut compared = 0;
    let mut divergences = 0;
    let mut blocked = 0;
    for case in &cases {
        if case.blocked.is_some() {
            blocked += 1;
            continue;
        }
        let (Some(a), Some(b)) = (ran_all.get(&case.name), ran_cut.get(&case.name)) else {
            problems.push(format!("{}: did not run in both builds", case.name));
            continue;
        };
        compared += 1;
        if let Err(problem) = diverged(&case.name, a, b) {
            problems.push(problem);
            divergences += 1;
            // The verdicts are not worth asking about once the two builds disagree. Whichever of
            // them is wrong, the divergence is the finding and two more lines about the same case
            // would bury it.
            continue;
        }
        // Only one of the two, because they agree. Which one does not matter and holding both
        // would say the same thing twice.
        if let Err(problem) = case.judge(b) {
            problems.push(format!("at {OPTIMIZED}, {problem}"));
        }
    }

    println!(
        "accounting: {compared} programs compared, {divergences} divergences, {blocked} not yet \
         buildable"
    );
    if problems.is_empty() {
        return Ok(());
    }
    Err(Error::Failed { task: "accounting", problems })
}

/// Whether the two builds of one program did the same thing.
///
/// What is compared is whether a report happened and which judgement it named. The specification
/// keys reports by class, source location and dynamic occurrence, and two of those three are not
/// available: nothing fills the `pc` field of a descriptor in yet, so there is no source location,
/// and the runs are made with abort semantics as section 14.3 asks, so there is at most one report
/// and the occurrence index is always the first. The judgement is what is left, and it is enough
/// to catch the failure this exists for, which is a report in A that is not in B at all.
fn diverged(name: &str, all: &Ran, cut: &Ran) -> std::result::Result<(), String> {
    let (spoke_all, spoke_cut) = (all.output.contains(BANNER), cut.output.contains(BANNER));
    match (spoke_all, spoke_cut) {
        (true, false) => Err(format!(
            "{name}: reported with the elimination off and said nothing with it on, so a check \
             that would have fired was removed. This is an unsound elimination.\n{}",
            indent(&all.output)
        )),
        (false, true) => Err(format!(
            "{name}: said nothing with the elimination off and reported with it on, which is \
             backwards and means the optimized build changed what the program does.\n{}",
            indent(&cut.output)
        )),
        (true, true) => {
            let judgement = |ran: &Ran| {
                ran.output
                    .split_once("judgement J")
                    .and_then(|(_, rest)| rest.split_once(','))
                    .map(|(number, _)| number.to_owned())
            };
            let (was, now) = (judgement(all), judgement(cut));
            if was == now {
                return Ok(());
            }
            Err(format!(
                "{name}: refused for J{} with the elimination off and J{} with it on\n{}",
                was.unwrap_or_else(|| "?".to_owned()),
                now.unwrap_or_else(|| "?".to_owned()),
                indent(&cut.output)
            ))
        }
        (false, false) => Ok(()),
    }
}

/// Every case on disk, in the order a directory listing gives them.
fn cases() -> Result<Vec<Case>> {
    let dir = root().join("tests").join("safety");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| Error::Io(format!("could not read {}: {e}", dir.display())))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err(Error::Io(format!("{} has no cases in it", dir.display())));
    }
    paths.iter().map(|path| Case::read(path)).collect()
}

impl Case {
    /// Reads one case and the directives at the top of it.
    fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Io(format!("could not read {}: {e}", path.display())))?;
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .ok_or_else(|| Error::Io(format!("{} has no name", path.display())))?;

        let mut row = None;
        let mut judgement = None;
        let mut says = Vec::new();
        let mut allow = false;
        let mut flags = Vec::new();
        let mut links = Vec::new();
        let mut summary = Vec::new();
        let mut gap = None;
        let mut blocked = None;
        for line in directives(&text) {
            let (key, value) = match line.split_once(':') {
                Some((key, value)) => (key.trim(), value.trim()),
                None => (line.trim(), ""),
            };
            match key {
                "row" => row = Some(value.to_owned()),
                "refuse" => {
                    let number = value.strip_prefix('J').unwrap_or(value);
                    judgement = Some(number.parse::<u8>().map_err(|_| {
                        Error::Io(format!("{name}: `refuse: {value}` is not a judgement"))
                    })?);
                }
                "says" => says.push(value.to_owned()),
                "flags" => {
                    for flag in value.split_whitespace() {
                        if !flag.starts_with('-') {
                            return Err(Error::Io(format!(
                                "{name}: `{flag}` is not a flag, and this directive is not a way \
                                 to hand the compiler another file"
                            )));
                        }
                        flags.push(flag.to_owned());
                    }
                }
                "links" => links.push(value.to_owned()),
                "summary" => summary.push(value.to_owned()),
                "allow" => allow = true,
                "gap" => gap = Some(value.to_owned()),
                "blocked" => blocked = Some(value.to_owned()),
                other => {
                    return Err(Error::Io(format!("{name}: `{other}` is not a directive")));
                }
            }
        }

        let Some(row) = row else {
            return Err(Error::Io(format!("{name}: no `row:`, so nothing says what it is for")));
        };
        let verdict = match (judgement, allow) {
            (Some(judgement), false) => Verdict::Refuse { judgement, says },
            (None, true) if says.is_empty() => Verdict::Allow,
            (None, true) => {
                return Err(Error::Io(format!("{name}: `allow` and `says` in the same case")));
            }
            (Some(_), true) => {
                return Err(Error::Io(format!("{name}: both `refuse` and `allow`")));
            }
            (None, false) => {
                return Err(Error::Io(format!("{name}: neither `refuse` nor `allow`")));
            }
        };
        if gap.is_some() && matches!(verdict, Verdict::Allow) {
            return Err(Error::Io(format!(
                "{name}: a `gap` on a case that expects nothing to happen says nothing"
            )));
        }
        if gap.is_some() && blocked.is_some() {
            return Err(Error::Io(format!(
                "{name}: `blocked` already says nothing happens, so the `gap` adds nothing"
            )));
        }
        Ok(Self {
            name,
            path: path.to_path_buf(),
            row,
            verdict,
            flags,
            links,
            summary,
            gap,
            blocked,
        })
    }

    /// Whether what the program did is what the case said it would.
    fn judge(&self, ran: &Ran) -> std::result::Result<(), String> {
        let name = &self.name;
        let Some(status) = ran.status else {
            return Err(format!("{name}: did not link\n{}", indent(&ran.output)));
        };
        let reported = ran.output.contains(BANNER);
        match &self.verdict {
            // A gap is the refusal below, run backwards. The status is not looked at, because a
            // row nothing catches yet is a program doing whatever undefined behaviour does, and
            // one of the things it does is crash. Being caught by the hardware is not being
            // caught by us, and this suite is about us.
            Verdict::Refuse { .. } if self.gap.is_some() => {
                if reported {
                    let gap = self.gap.as_deref().unwrap_or("");
                    return Err(format!(
                        "{name}: refused, and {gap} says it should not be yet. If the milestone \
                         that closes this landed, take the `gap` line out.\n{}",
                        indent(&ran.output)
                    ));
                }
                Ok(())
            }
            Verdict::Refuse { judgement, says } => {
                if !reported {
                    return Err(format!(
                        "{name}: no report, and it exited {status}\n{}",
                        indent(&ran.output)
                    ));
                }
                let wanted = format!("judgement J{judgement},");
                if !ran.output.contains(&wanted) {
                    return Err(format!(
                        "{name}: refused, but not for `{wanted}`\n{}",
                        indent(&ran.output)
                    ));
                }
                for want in says {
                    if !ran.output.contains(want.as_str()) {
                        return Err(format!(
                            "{name}: the report does not say `{want}`\n{}",
                            indent(&ran.output)
                        ));
                    }
                }
                // A refusal that let the program carry on is a refusal that did not refuse.
                if status == 0 {
                    return Err(format!("{name}: reported and then exited 0"));
                }
                Ok(())
            }
            Verdict::Allow => {
                if reported {
                    return Err(format!(
                        "{name}: a false positive against a program doing nothing wrong\n{}",
                        indent(&ran.output)
                    ));
                }
                if status != 0 {
                    return Err(format!(
                        "{name}: exited {status} without a report, so something else went \
                         wrong\n{}",
                        indent(&ran.output)
                    ));
                }
                Ok(())
            }
        }
    }
}

/// The directive lines at the top of a case.
///
/// The old kind of comment and only before the first line of code, which is the same rule
/// `tests/accept` uses and for the same reason: a directive that can appear anywhere is a
/// directive somebody eventually writes inside a string.
fn directives(text: &str) -> Vec<&str> {
    text.lines()
        .take_while(|line| line.trim().is_empty() || line.trim_start().starts_with("/*"))
        .filter_map(|line| line.trim().strip_prefix("/*")?.strip_suffix("*/"))
        .map(str::trim)
        .filter(|line| is_directive(line))
        .collect()
}

/// Whether one comment at the top of a case is a directive or a sentence about the case.
///
/// A directive is a bare lower case word, on its own or with a colon after it. Everything else is
/// prose, and a case is allowed prose at the top because why a program is in this suite is worth
/// saying beside it rather than in a file somewhere else.
fn is_directive(line: &str) -> bool {
    let word = line.split_once(':').map_or(line, |(key, _)| key);
    !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase())
}

/// How many distinct rows the suite has a case for that it can actually run.
///
/// A blocked case does not count. It says what the answer should be, which is worth having
/// written down, but a program the compiler cannot build is not evidence about the monitor and
/// counting it would make the coverage number say more than it knows.
fn coverage(cases: &[Case]) -> usize {
    let mut rows: Vec<&str> =
        cases.iter().filter(|case| case.blocked.is_none()).map(|case| case.row.as_str()).collect();
    rows.sort_unstable();
    rows.dedup();
    rows.len()
}

/// One way of building the suite.
///
/// The plain run and each half of the differential accounting are the same compilation with two
/// things changed, so they are two values of this rather than two copies of [`build`].
#[derive(Debug)]
struct Plan {
    /// The optimization level.
    level: &'static str,
    /// The passes to turn off, which is how build A of section 14.3 is made.
    without: &'static [&'static str],
    /// The directory under `target` this build lays itself out in.
    ///
    /// Two builds of the same cases have to be able to exist at once, because the whole point is
    /// running both and comparing, so the name is per plan rather than fixed.
    dir: &'static str,
    /// Whether to hold each case's `summary:` lines to `--emit=safety-summary`.
    ///
    /// Once is enough. It is an assertion about what the compiler says it trusts, which is the
    /// same at both levels, and asking for it twice would double the compilations for nothing.
    summaries: bool,
}

/// Compiles every case and lays out the directory the runner is pointed at.
///
/// One directory holding the assembly for every case, the runtime archive, and the script that
/// builds and runs them. Nothing is written into it after this, and the runner mounts it read
/// only, which is what keeps a container from leaving files in the tree owned by somebody else.
fn build(cases: &[Case], plan: &Plan) -> Result<PathBuf> {
    let work = root().join("target").join(plan.dir);
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
    let archive = staticlib("rucc-safe-rt", TRIPLE)?;
    std::fs::copy(&archive, work.join("safe-rt.a"))
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", archive.display())))?;

    let mut problems = Vec::new();
    problems.extend(libraries(cases, &work)?);
    for case in cases {
        let mut compile = Command::new(&rucc);
        compile.args(["-S", &format!("--target={TRIPLE}"), TIER, plan.level]);
        compile.args(plan.without);
        compile.args(&case.flags);
        let out = compile
            .arg("-o")
            .arg(work.join(format!("{}.s", case.name)))
            .arg(&case.path)
            .current_dir(root())
            .output()
            .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
        match (&case.blocked, out.status.success()) {
            (None, false) => problems.push(format!(
                "{}: did not compile\n{}",
                case.name,
                indent(String::from_utf8_lossy(&out.stderr).trim_end())
            )),
            (Some(blocked), true) => problems.push(format!(
                "{}: compiled, and {blocked} says it should not be able to yet. Take the \
                 `blocked` line out and let the case run.",
                case.name
            )),
            // A blocked case that did not compile leaves no assembly behind, so the script never
            // sees it and it is not run.
            (Some(_), false) | (None, true) => {}
        }
        if case.blocked.is_some() {
            continue;
        }
        if !case.links.is_empty() {
            std::fs::write(work.join(format!("{}.links", case.name)), case.links.join(" "))
                .map_err(|e| Error::Io(format!("could not write {}'s links: {e}", case.name)))?;
        }
        if plan.summaries {
            problems.extend(summarised(&rucc, case, &work)?);
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "safety", problems });
    }

    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// Lays out the sources of the uninstrumented libraries, and says which cases named one that is
/// not there.
///
/// Copied rather than mounted from the tree, because the runner is given one directory and the
/// container mounts it read only. They are compiled inside that runner rather than here, by the
/// system compiler, which is the whole point: a library rucc built would be an instrumented library
/// and this is document 10 section 10.7's mixed link.
fn libraries(cases: &[Case], work: &Path) -> Result<Vec<String>> {
    let dir = root().join("tests").join("safety").join("lib");
    let into = work.join("lib");
    std::fs::create_dir_all(&into)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", into.display())))?;

    let mut problems = Vec::new();
    for case in cases {
        for name in &case.links {
            let source = dir.join(format!("{name}.c"));
            if !source.exists() {
                problems.push(format!(
                    "{}: `links: {name}` and there is no {}",
                    case.name,
                    source.display()
                ));
                continue;
            }
            std::fs::copy(&source, into.join(format!("{name}.c")))
                .map_err(|e| Error::Io(format!("could not copy {}: {e}", source.display())))?;
        }
    }
    Ok(problems)
}

/// Holds a case's `--emit=safety-summary` to whatever its `summary:` lines asked for.
///
/// Run as a second compilation rather than read off the first, because the two emit different
/// things and a driver that produced both at once would be answering a question nobody asked.
fn summarised(rucc: &Path, case: &Case, work: &Path) -> Result<Vec<String>> {
    if case.summary.is_empty() {
        return Ok(Vec::new());
    }
    let path = work.join(format!("{}.safety.json", case.name));
    let out = Command::new(rucc)
        .args(["--emit=safety-summary", &format!("--target={TRIPLE}"), TIER, LEVEL])
        .args(&case.flags)
        .arg("-o")
        .arg(&path)
        .arg(&case.path)
        .current_dir(root())
        .output()
        .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
    if !out.status.success() {
        return Ok(vec![format!(
            "{}: no summary\n{}",
            case.name,
            indent(String::from_utf8_lossy(&out.stderr).trim_end())
        )]);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Error::Io(format!("could not read {}: {e}", path.display())))?;
    Ok(case
        .summary
        .iter()
        .filter(|want| !text.contains(want.as_str()))
        .map(|want| format!("{}: the summary does not say `{want}`\n{}", case.name, indent(&text)))
        .collect())
}

/// The script that assembles, links and runs each case.
///
/// It writes nothing into the directory it is given. Everything it produces goes under `/tmp`,
/// which means the directory can be mounted read only and a container running as root cannot leave
/// anything behind in the tree.
///
/// `-no-pie` because the back end emits the small code model and not the position independent one.
/// Making that a driver flag is the linker's part of document 11 and is not this suite's to
/// decide.
///
/// The shell's own stderr is thrown away, since the only thing on it is chatter about programs
/// that did what they were supposed to do. A shell also writes a note when a program dies on a
/// signal, which ends up in that program's block, and that is left alone: it is true, it is short,
/// and it only shows up in a message about a case that has already failed.
///
/// Anything under `lib` is built first, by the system compiler, into a shared object nothing
/// instrumented. That is deliberate and is the point of document 10 section 10.7: the library a
/// case links against has to be one this project did not build, or the mixed link is not being
/// tested. A case says which of them it wants in a `.links` file beside its assembly.
pub(crate) const SCRIPT: &str = "\
#!/bin/sh
exec 2>/dev/null
out=/tmp/safety
mkdir -p \"$out\"
for source in lib/*.c; do
    [ -f \"$source\" ] || continue
    name=${source#lib/}
    name=${name%.c}
    gcc -shared -fPIC \"$source\" -o \"$out/lib$name.so\" >\"$out/lib$name.log\" 2>&1
done
for source in *.s; do
    name=${source%.s}
    printf '<<<case %s>>>\\n' \"$name\"
    libs=
    if [ -f \"$name.links\" ]; then
        for each in $(cat \"$name.links\"); do
            libs=\"$libs -l$each\"
        done
        libs=\"-L$out -Wl,-rpath,$out$libs\"
    fi
    if gcc -no-pie \"$source\" safe-rt.a $libs -o \"$out/$name\" >\"$out/$name.log\" 2>&1; then
        \"$out/$name\" >\"$out/$name.out\" 2>&1
        status=$?
        cat \"$out/$name.out\"
        printf '<<<status %s>>>\\n' \"$status\"
    else
        cat \"$out/$name.log\"
        printf '<<<status nolink>>>\\n'
    fi
done
";

/// Splits what the script printed back into one entry per case.
pub(crate) fn read(text: &str) -> BTreeMap<String, Ran> {
    let mut runs = BTreeMap::new();
    let mut name = String::new();
    let mut output = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("<<<case ").and_then(|l| l.strip_suffix(">>>")) {
            name = rest.to_owned();
            output.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("<<<status ").and_then(|l| l.strip_suffix(">>>")) {
            let status = rest.parse::<i32>().ok();
            runs.insert(
                std::mem::take(&mut name),
                Ran { output: std::mem::take(&mut output), status },
            );
            continue;
        }
        let _ = writeln!(output, "{line}");
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directive_is_read_off_the_top_of_a_case_and_nowhere_else() {
        // The second block is after code, so it is a comment about the code and not a directive.
        // Reading it as one would let a case quietly change its own expectations halfway down.
        let text = "/* row: T1 */\n/* refuse: J1 */\nint main(void) { return 0; }\n/* allow */\n";
        assert_eq!(directives(text), ["row: T1", "refuse: J1"]);
    }

    #[test]
    fn a_case_that_says_neither_what_it_wants_nor_why_is_refused() {
        // Every case names a row, because the count of rows covered is what the milestone is
        // measured by and a case that belongs to no row does not move it.
        let dir = std::env::temp_dir().join("rucc-safety-directives");
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let path = dir.join("a-case.c");
        std::fs::write(&path, "/* refuse: J1 */\nint main(void) { return 0; }\n").expect("write");
        let said = Case::read(&path).expect_err("no row").to_string();
        assert!(said.contains("no `row:`"), "{said}");

        std::fs::write(&path, "/* row: T1 */\nint main(void) { return 0; }\n").expect("write");
        let said = Case::read(&path).expect_err("no verdict").to_string();
        assert!(said.contains("neither `refuse` nor `allow`"), "{said}");
    }

    #[test]
    fn what_the_script_printed_comes_back_one_entry_per_case() {
        let text = "<<<case one>>>\nrucc: memory safety violation\n<<<status 134>>>\n\
                    <<<case two>>>\n<<<status 0>>>\n<<<case three>>>\nld: no\n<<<status nolink>>>\n";
        let runs = read(text);
        assert_eq!(runs["one"].status, Some(134));
        assert!(runs["one"].output.contains(BANNER));
        assert_eq!(runs["two"].status, Some(0));
        assert_eq!(runs["two"].output, "");
        // A case that did not link has no status, and that is a different failure from a case
        // that ran and did the wrong thing.
        assert_eq!(runs["three"].status, None);
    }

    /// A case with the given directives, for the judging tests.
    ///
    /// The name is the test's own, because these run at the same time and a shared file is two
    /// tests reading each other's directives.
    fn case(name: &str, directives: &str) -> Case {
        let dir = std::env::temp_dir().join("rucc-safety-judging");
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let path = dir.join(format!("{name}.c"));
        std::fs::write(&path, format!("{directives}int main(void) {{ return 0; }}\n"))
            .expect("write");
        Case::read(&path).expect("a case")
    }

    #[test]
    fn a_refusal_has_to_be_the_refusal_the_case_asked_for() {
        // Otherwise a program that is refused for the wrong reason counts as a pass, which is how
        // a suite ends up reporting coverage of a row nothing actually catches.
        let one =
            case("a-refusal", "/* row: T1 */\n/* refuse: J1 */\n/* says: has been freed */\n");
        let report = |text: &str| Ran { output: text.to_owned(), status: Some(134) };

        assert!(
            one.judge(&report("rucc: memory safety violation\njudgement J1, x\nhas been freed"))
                .is_ok()
        );
        assert!(one.judge(&report("rucc: memory safety violation\njudgement J6, x")).is_err());
        assert!(one.judge(&report("rucc: memory safety violation\njudgement J1, x")).is_err());
        assert!(one.judge(&Ran { output: String::new(), status: Some(0) }).is_err());
        // Reported and carried on anyway, which would mean the access the check refused went
        // ahead.
        assert!(
            one.judge(&Ran {
                output: "rucc: memory safety violation\njudgement J1, x\nhas been freed".to_owned(),
                status: Some(0),
            })
            .is_err()
        );
    }

    #[test]
    fn a_case_that_expects_nothing_fails_on_a_report_and_on_a_crash() {
        // The false positive idioms of section 3.5. Both halves matter: a report means the monitor
        // rejected a correct program, and a non-zero status with no report means the program did
        // something else wrong and the case is not testing what it says it is.
        let quiet = case("a-quiet-case", "/* row: 3.5 one past the end */\n/* allow */\n");
        assert!(quiet.judge(&Ran { output: String::new(), status: Some(0) }).is_ok());
        assert!(quiet.judge(&Ran { output: BANNER.to_owned(), status: Some(134) }).is_err());
        assert!(quiet.judge(&Ran { output: String::new(), status: Some(139) }).is_err());
    }

    #[test]
    fn a_case_can_ask_for_the_flags_it_is_compiled_with_and_cannot_ask_for_a_file() {
        let asked = case(
            "a-flagged-case",
            "/* row: S4 */\n/* flags: -fsafety-subobject */\n/* refuse: J1 */\n",
        );
        assert_eq!(asked.flags, ["-fsafety-subobject"]);
        assert!(case("a-plain-case", "/* row: S4 */\n/* allow */\n").flags.is_empty());

        // The directive hands words to the compiler, so a word that is not a flag is another input
        // file, and a case that could add one of those could compile something nobody reviewed.
        let dir = std::env::temp_dir().join("rucc-safety-flags");
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let path = dir.join("a-sneaky-case.c");
        std::fs::write(&path, "/* row: S4 */\n/* flags: other.c */\n/* allow */\n").expect("write");
        let said = Case::read(&path).expect_err("not a flag").to_string();
        assert!(said.contains("is not a flag"), "{said}");
    }

    #[test]
    fn a_gap_is_the_same_expectation_run_backwards() {
        // Section 15.7's rule about not deleting a test to make CI green, for a test that has not
        // started passing. The day the row is caught, this fails and asks for the line to come
        // out.
        let gap = case("a-gap", "/* row: S2 */\n/* refuse: J1 */\n/* gap: #428 */\n");
        assert!(gap.judge(&Ran { output: String::new(), status: Some(0) }).is_ok());
        // Crashing without a report is still not being caught by us.
        assert!(gap.judge(&Ran { output: String::new(), status: Some(139) }).is_ok());
        let said = gap
            .judge(&Ran { output: BANNER.to_owned(), status: Some(134) })
            .expect_err("the gap closed");
        assert!(said.contains("#428"), "{said}");
    }

    #[test]
    fn a_blocked_case_keeps_its_verdict_and_is_left_out_of_the_coverage_count() {
        // The verdict is written down now so that the day the construct lowers, the case runs
        // against an expectation somebody wrote before they knew what the compiler would do.
        let blocked = case(
            "a-blocked-case",
            "/* row: 3.5 variable length arrays */\n/* allow */\n\
                  /* blocked: #291 */\n",
        );
        assert!(blocked.blocked.is_some());
        assert!(matches!(blocked.verdict, Verdict::Allow));
        assert_eq!(coverage(std::slice::from_ref(&blocked)), 0);
        let runs = case("a-running-case", "/* row: S1 */\n/* allow */\n");
        assert_eq!(coverage(&[blocked, runs]), 1);
    }

    #[test]
    fn a_case_can_name_a_library_and_a_line_its_summary_has_to_have() {
        // Both directives carry text with punctuation in it, and the summary one carries a colon
        // of its own, so the thing worth pinning down is that the parser stops at the first one
        // and hands the rest over whole.
        let mixed = case(
            "a-mixed-link",
            "/* row: 10.7 the mixed link */\n/* allow */\n/* links: notes */\n\
             /* summary: \"crossings\": { \"entered\": 1, \"returned\": 1 } */\n",
        );
        assert_eq!(mixed.links, vec!["notes".to_owned()]);
        assert_eq!(mixed.summary, vec!["\"crossings\": { \"entered\": 1, \"returned\": 1 }"]);
    }

    #[test]
    fn a_library_a_case_names_has_to_exist() {
        // A typo in a `links:` line would otherwise turn into a link error inside the runner, which
        // arrives as a wall of linker output about an undefined symbol rather than a sentence
        // saying which file is missing.
        let case = case("a-missing-library", "/* row: S2 */\n/* allow */\n/* links: nothing */\n");
        let work = std::env::temp_dir().join("rucc-safety-libraries");
        let problems =
            libraries(std::slice::from_ref(&case), &work).expect("the directory is writable");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("nothing.c"), "{}", problems[0]);
    }

    #[test]
    fn a_blocked_case_may_not_also_be_a_gap() {
        // Both say the same thing, which is that nothing is expected to happen, and a case with
        // two issue numbers on it leaves nobody sure which one closing it should make it run.
        let dir = std::env::temp_dir().join("rucc-safety-judging");
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let path = dir.join("a-blocked-gap.c");
        std::fs::write(
            &path,
            "/* row: S2 */\n/* refuse: J1 */\n/* gap: #428 */\n/* blocked: #291 */\n\
             int main(void) { return 0; }\n",
        )
        .expect("write");
        let said = Case::read(&path).expect_err("both at once");
        assert!(format!("{said}").contains("adds nothing"), "{said}");
    }

    /// A run that said the given thing, for the accounting tests.
    fn ran(output: &str) -> Ran {
        Ran { output: output.to_owned(), status: Some(if output.is_empty() { 0 } else { 134 }) }
    }

    /// What the runtime prints, near enough for a comparison that only reads two parts of it.
    fn reported(judgement: u8) -> String {
        format!("{BANNER}\n  judgement J{judgement}, an access the capability does not cover\n")
    }

    #[test]
    fn two_builds_that_both_said_nothing_have_not_diverged() {
        assert!(diverged("quiet", &ran(""), &ran("")).is_ok());
    }

    #[test]
    fn two_builds_that_refused_for_the_same_reason_have_not_diverged() {
        let (all, cut) = (ran(&reported(1)), ran(&reported(1)));
        assert!(diverged("agreed", &all, &cut).is_ok());
    }

    #[test]
    fn a_report_that_only_the_unoptimized_build_makes_is_an_unsound_elimination() {
        // This is the finding the whole task exists for: a check fired with elimination off and
        // did not fire with it on, so the pass took out a check that had something to say.
        let said = diverged("lost", &ran(&reported(1)), &ran("")).expect_err("a divergence");
        assert!(said.contains("unsound elimination"), "{said}");
        assert!(said.contains("lost"), "{said}");
    }

    #[test]
    fn a_report_that_only_the_optimized_build_makes_is_reported_the_other_way_round() {
        // Elimination can only take checks away, so this is not elimination being unsound. It is
        // some other part of `-O2` changing what the program does, and saying so points at it.
        let said = diverged("gained", &ran(""), &ran(&reported(1))).expect_err("a divergence");
        assert!(said.contains("backwards"), "{said}");
    }

    #[test]
    fn two_builds_that_refused_for_different_reasons_have_diverged() {
        // Both refused, so nothing was lost, but they disagree about what was wrong and one of
        // the two answers is not the one the case asked for.
        let (all, cut) = (ran(&reported(1)), ran(&reported(2)));
        let said = diverged("disagreed", &all, &cut).expect_err("a divergence");
        assert!(said.contains("J1"), "{said}");
        assert!(said.contains("J2"), "{said}");
    }
}
