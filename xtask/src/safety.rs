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

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result, root, staticlib};

/// The machine the suite is compiled for, which is the only one there is a back end for.
const TRIPLE: &str = "x86_64-unknown-linux-gnu";

/// The image the programs run in on a machine that is not an x86-64 Linux one.
///
/// A compiler image rather than a bare distribution, because what the container has to do is
/// assemble and link, and pinning the major version keeps a case that starts failing from being a
/// question about which `gcc` the machine pulled this morning.
const IMAGE: &str = "gcc:13";

/// The tier the suite is run at.
///
/// Milestone S1 is `detect` and nothing else. `enforce` and `kernel` get their own runs when the
/// milestones that define them arrive, and each will want its own column in the expectations
/// rather than a second pass over these.
const TIER: &str = "-fsafety=detect";

/// The optimization level the suite is run at.
///
/// None, because S1's whole point is that the checks are correct before anything tries to remove
/// them. Running the same cases at `-O2` is milestone S4's, and it is a different question: there
/// the interesting failure is a check that was eliminated when it should not have been, and the
/// expectations that answer it are the same files read a second time.
const LEVEL: &str = "-O0";

/// The line every report starts with, which is what says one happened at all.
const BANNER: &str = "rucc: memory safety violation";

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
struct Ran {
    /// Everything it wrote, on both streams.
    output: String,
    /// What it exited with, or nothing when it did not get as far as being linked.
    status: Option<i32>,
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
    let runner = Runner::find()?;
    println!("safety: {} programs, {TIER} {LEVEL}, {runner}", cases.len());

    let work = build(&cases)?;
    let ran = runner.run(&work)?;

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
        Ok(Self { name, path: path.to_path_buf(), row, verdict, gap, blocked })
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

/// Everything indented by two, so that a program's output in a problem reads as its output.
fn indent(text: &str) -> String {
    text.lines().map(|line| format!("      {line}\n")).collect()
}

/// Compiles every case and lays out the directory the runner is pointed at.
///
/// One directory holding the assembly for every case, the runtime archive, and the script that
/// builds and runs them. Nothing is written into it after this, and the runner mounts it read
/// only, which is what keeps a container from leaving files in the tree owned by somebody else.
fn build(cases: &[Case]) -> Result<PathBuf> {
    let work = root().join("target").join("safety");
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
    for case in cases {
        let out = Command::new(&rucc)
            .args(["-S", &format!("--target={TRIPLE}"), TIER, LEVEL, "-o"])
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
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "safety", problems });
    }

    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
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
const SCRIPT: &str = "\
#!/bin/sh
exec 2>/dev/null
out=/tmp/safety
mkdir -p \"$out\"
for source in *.s; do
    name=${source%.s}
    printf '<<<case %s>>>\\n' \"$name\"
    if gcc -no-pie \"$source\" safe-rt.a -o \"$out/$name\" >\"$out/$name.log\" 2>&1; then
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

/// How the programs get run.
#[derive(Debug)]
enum Runner {
    /// Straight, because this machine is the machine they are compiled for.
    Here,
    /// In a container, because it is not.
    Container,
}

impl std::fmt::Display for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Here => f.write_str("run here"),
            Self::Container => f.write_str("run in a container"),
        }
    }
}

impl Runner {
    /// Picks one, or says why there is not one.
    fn find() -> Result<Self> {
        let host = crate::host_triple()?;
        if host.starts_with("x86_64-") && host.contains("linux") {
            return Ok(Self::Here);
        }
        let up = Command::new("docker")
            .args(["version", "--format", "{{.Server.Os}}"])
            .output()
            .is_ok_and(|out| out.status.success());
        if up {
            return Ok(Self::Container);
        }
        Err(Error::Io(format!(
            "this suite runs {TRIPLE} programs and this machine is {host}, so it needs a \
             container to run them in and docker is not answering. Start it, or run the suite on \
             an x86-64 Linux machine."
        )))
    }

    /// Runs the script over the directory and reads back what each case did.
    fn run(&self, work: &Path) -> Result<BTreeMap<String, Ran>> {
        let out = match self {
            Self::Here => Command::new("sh").arg("run.sh").current_dir(work).output(),
            Self::Container => Command::new("docker")
                .args(["run", "--rm", "--platform", "linux/amd64", "-v"])
                .arg(format!("{}:/w:ro", work.display()))
                .args(["-w", "/w", IMAGE, "sh", "run.sh"])
                .output(),
        }
        .map_err(|e| Error::Io(format!("could not run the suite: {e}")))?;
        if !out.status.success() {
            return Err(Error::Io(format!(
                "the suite did not run: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(read(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// Splits what the script printed back into one entry per case.
fn read(text: &str) -> BTreeMap<String, Ran> {
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
}
