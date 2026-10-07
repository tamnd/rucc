//! The Juliet test suite, per row of the bug model and per tier.
//!
//! Design: `spec/safe-memory/14-verification.md` section 14.6 and `spec/safe-memory/03-bug-model.md`
//! section 3.7.
//!
//! Every row of document 03's matrix that has a CWE column runs the Juliet cases for that CWE at
//! every tier, and what comes out is three raw counts per row and per tier: the cases whose bad
//! half was refused, the ones whose bad half was not, and the ones whose good half was refused when
//! it should not have been. The missed cases are listed by test id rather than folded into a
//! percentage, because section 14.6 says a missed case is either a document 17 entry with a reason
//! or a bug, and nobody can tell which from a percentage.
//!
//! This is a measurement and not a gate. Juliet is synthetic and uniform in shape, and section 14.6
//! is plain that a tool can score perfectly on it and be useless, so the numbers are printed and
//! written out and the task only fails when it could not take them.
//!
//! # Why the sources are not in the tree
//!
//! For the reason `xtask/src/libraries.rs` gives about SQLite, and with more force: Juliet 1.3 is a
//! hundred and fifty megabytes of somebody else's C. It is NIST's, from the SARD downloads page, and
//! `RUCC_JULIET_SOURCE` points at the `C` directory the archive unpacks to. Without it the task says
//! where to get the archive and stops.
//!
//! # What a case is
//!
//! What Juliet's own Linux makefiles say it is. A file whose name ends in a number is a case on its
//! own, and files that end in the same number followed by `a`, `b` and so on are one case between
//! them. Files with `w32` or `wchar_t` in the name are left out, as the makefiles leave them out,
//! and so is everything in C++. The cases that read their input from a listening socket are left
//! out as well: they wait for a connection that nothing here makes, and are counted apart so that
//! the total still adds up.
//!
//! Each case is built once per tier, together with Juliet's own support files and a driver that
//! calls the bad function or the good one depending on its argument, and run twice. The support
//! files are compiled by rucc at the same tier as the case, because they are part of the program:
//! most cases pass the pointer they went wrong with to `printLine`, and a program whose printing was
//! built by somebody else is the mixed link, which is a different question from this one.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::runner::{Runner, TRIPLE};
use crate::safety::BANNER;
use crate::{Error, Result, cost, target_dir};

/// The variable that says where Juliet's `C` directory is.
const VARIABLE: &str = "RUCC_JULIET_SOURCE";

/// The file that says a directory is Juliet's `C` directory.
const MARKER: &str = "testcasesupport/std_testcase.h";

/// The tiers, in the order the report lists them.
const TIERS: [&str; 3] = ["detect", "enforce", "kernel"];

/// The level the cases are built at unless the command line says otherwise.
///
/// The one people ship at, for the reason `accounting` runs there: it is where every pass that could
/// take a check out has run.
const LEVEL: &str = "-O2";

/// How long one half of one case may run before it counts as having hung.
const SECONDS: u32 = 10;

/// One CWE that Juliet has cases for, and the rows of document 03 that name it.
struct Weakness {
    /// The CWE number, as the directory names start with it.
    cwe: u32,
    /// What it is, for the report.
    what: &'static str,
    /// The rows of document 03's matrix whose CWE column has this one in it.
    rows: &'static [&'static str],
}

/// Every CWE in document 03's matrix that Juliet 1.3 has C cases for.
///
/// One CWE is the column of more than one row more often than not, because Juliet sorts by what the
/// mistake is and the matrix sorts by what the monitor has to know to catch it. A heap overflow is
/// row S1 when it is a whole object and row S8 when it is a field, and Juliet does not say which, so
/// both rows carry the same cases and the report says so rather than inventing a split.
const WEAKNESSES: &[Weakness] = &[
    Weakness { cwe: 121, what: "stack based buffer overflow", rows: &["S2", "S8"] },
    Weakness { cwe: 122, what: "heap based buffer overflow", rows: &["S1", "S8"] },
    Weakness { cwe: 124, what: "buffer underwrite", rows: &["S8"] },
    Weakness { cwe: 126, what: "buffer overread", rows: &["S3", "S8"] },
    Weakness { cwe: 127, what: "buffer underread", rows: &["S8"] },
    Weakness { cwe: 401, what: "memory leak", rows: &["T9"] },
    Weakness { cwe: 415, what: "double free", rows: &["T2"] },
    Weakness { cwe: 416, what: "use after free", rows: &["T1", "T5", "T6", "C4"] },
    Weakness { cwe: 457, what: "use of uninitialized variable", rows: &["Y6", "Y7"] },
    Weakness { cwe: 476, what: "null pointer dereference", rows: &["S6"] },
    Weakness { cwe: 562, what: "return of stack variable address", rows: &["T4"] },
    Weakness { cwe: 590, what: "free of memory not on the heap", rows: &["T3"] },
    Weakness { cwe: 761, what: "free of a pointer not at the start", rows: &["T3"] },
    Weakness { cwe: 843, what: "type confusion", rows: &["Y1", "Y2", "Y3", "Y4", "Y5"] },
];

/// The CWEs the matrix names that Juliet 1.3 has no C cases for, so that their rows are not
/// mistaken for rows that passed.
const ABSENT: &[(u32, &str)] = &[(125, "S1"), (787, "S1, S4"), (908, "Y6"), (362, "C1, C2, C3")];

/// Cases where both halves do something else wrong before they reach the mistake under test.
///
/// Juliet's good halves are meant to be the same program with the mistake taken out, and now and
/// then they keep a different one, which the bad half has as well and reaches first. A report from
/// a case like that is the monitor being right about the other mistake, and it says nothing either
/// way about the one the case is filed under: counting the good half as a false positive would be
/// the suite's mistake charged to the compiler, and counting the bad half as detected would be
/// credit for a mistake the program never got to. So a case under one of these whose good half
/// reported is set aside whole. Each entry says what both halves do, so that it can be checked
/// against the source by anybody who doubts it, and a good half that reports and is not under one
/// of these is a false positive.
#[derive(Debug)]
struct Excuse {
    /// The test ids it covers start with one of these.
    prefixes: &'static [&'static str],
    /// And end with this flow variant, when the excuse is about one variant rather than a family.
    variant: Option<&'static str>,
    /// What both halves do wrong.
    why: &'static str,
}

impl Excuse {
    /// Whether the case with this id is one this excuse is about.
    fn covers(&self, id: &str) -> bool {
        self.prefixes.iter().any(|prefix| id.starts_with(prefix))
            && self.variant.is_none_or(|variant| id.ends_with(variant))
    }
}

/// Every case known to be wrong in its own right before it gets to its mistake.
const EXCUSES: &[Excuse] = &[
    Excuse {
        prefixes: &["CWE843_Type_Confusion__"],
        variant: None,
        why: "both halves point at a local declared in a block and read it after the block has \
              closed, which is a use after the end of its lifetime (row T4)",
    },
    Excuse {
        prefixes: &[
            "CWE121_Stack_Based_Buffer_Overflow__",
            "CWE124_Buffer_Underwrite__",
            "CWE126_Buffer_Overread__",
            "CWE127_Buffer_Underread__",
        ],
        variant: Some("_32"),
        why: "both halves read the pointer they are about to overwrite through a second pointer to \
              it before anything has written it, which is a read of a local nothing wrote (row Y6)",
    },
];

/// One Juliet case.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Case {
    /// The test id, which is the file name without its number's letter and without `.c`, and is
    /// what the bad and good functions are named after.
    id: String,
    /// The CWE it is filed under.
    cwe: u32,
    /// Its files, in order.
    files: Vec<PathBuf>,
}

/// What one half of one case did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    /// The monitor reported.
    Reported,
    /// It ran to the end and nothing was said.
    Silent,
    /// It stopped some other way, with this status, and nothing was said.
    Stopped(i32),
    /// It was still running when the time ran out.
    Hung,
}

/// What one case did at one tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// rucc could not compile it.
    Unbuilt,
    /// It compiled and did not link.
    Unlinked,
    /// It ran, and this is what each half did.
    Ran { bad: Half, good: Half },
}

/// The counts for one CWE or one row at one tier, with the ids behind them.
#[derive(Debug, Default)]
struct Tally {
    detected: usize,
    missed: Vec<String>,
    false_positive: Vec<String>,
    /// Cases set aside because both halves did something else wrong first, under the excuse that
    /// says what.
    excused: Vec<(&'static Excuse, Vec<String>)>,
    unbuilt: Vec<String>,
}

/// Builds every case at every tier, runs both halves of each, and reports per row and per tier.
///
/// The arguments are an optional level, `-O0` to `-O3`, and any number of CWE numbers to run only
/// those, for the run somebody makes while working on one row.
///
/// # Errors
///
/// [`Error::Io`] when the suite is not on this machine, a build failed outright, or the programs
/// could not be run. A case that was missed is a number in the report and not an error.
pub(crate) fn juliet(args: &[String]) -> Result<()> {
    let (level, only) = arguments(args)?;
    let Some(source) = found() else {
        println!(
            "juliet: no suite on this machine, so nothing was built. Unpack NIST's Juliet C/C++ \
             1.3 archive from the SARD downloads page and point {VARIABLE} at its C directory."
        );
        return Ok(());
    };
    let chosen: Vec<&Weakness> =
        WEAKNESSES.iter().filter(|w| only.is_empty() || only.contains(&w.cwe)).collect();
    let (cases, listening) = cases(&source, &chosen)?;
    let runner = Runner::find("this suite")?;
    println!(
        "juliet: {} cases under {}, {} more left out because they wait on a socket, {level}, {}, \
         {runner}",
        cases.len(),
        source.display(),
        listening,
        TIERS.join(" ")
    );

    let work = target_dir().join("juliet");
    let built = build(&cases, &source, &work, &level)?;
    let ran = read(&runner.run(&work, "the suite")?);

    let mut outcomes: BTreeMap<(&str, &str), Outcome> = BTreeMap::new();
    for case in &cases {
        for tier in TIERS {
            let outcome = if !built.contains(&(tier, case.id.as_str())) {
                Outcome::Unbuilt
            } else {
                ran.get(&(tier.to_owned(), case.id.clone())).copied().unwrap_or(Outcome::Unlinked)
            };
            outcomes.insert((tier, case.id.as_str()), outcome);
        }
    }

    let report = report(&chosen, &cases, &outcomes, &level);
    let file = work.join("report.txt");
    std::fs::write(&file, &report.full)
        .map_err(|e| Error::Io(format!("could not write {}: {e}", file.display())))?;
    print!("{}", report.summary);
    println!("juliet: every missed and false positive case is listed in {}", file.display());
    Ok(())
}

/// The level and the CWEs the command line asked for.
fn arguments(args: &[String]) -> Result<(String, Vec<u32>)> {
    let mut level = LEVEL.to_owned();
    let mut only = Vec::new();
    for arg in args {
        if matches!(arg.as_str(), "-O0" | "-O1" | "-O2" | "-O3") {
            level.clone_from(arg);
            continue;
        }
        let number = arg.trim_start_matches("CWE").trim_start_matches("cwe");
        match number.parse::<u32>() {
            Ok(cwe) if WEAKNESSES.iter().any(|w| w.cwe == cwe) => only.push(cwe),
            _ => {
                return Err(Error::Io(format!(
                    "juliet: `{arg}` is neither a level nor a CWE this suite has cases for"
                )));
            }
        }
    }
    Ok((level, only))
}

/// Where the suite is, if the variable names it.
///
/// Either the `C` directory or the directory the archive was unpacked into, since both are what
/// somebody would reasonably point at.
fn found() -> Option<PathBuf> {
    let said = PathBuf::from(std::env::var_os(VARIABLE)?);
    [said.clone(), said.join("C")].into_iter().find(|dir| dir.join(MARKER).is_file())
}

/// Every case for these CWEs, and how many were left out for waiting on a socket.
fn cases(source: &Path, chosen: &[&Weakness]) -> Result<(Vec<Case>, usize)> {
    let testcases = source.join("testcases");
    let entries = std::fs::read_dir(&testcases)
        .map_err(|e| Error::Io(format!("could not read {}: {e}", testcases.display())))?;
    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    dirs.sort();

    let mut all = Vec::new();
    let mut listening = 0;
    for weakness in chosen {
        let prefix = format!("CWE{}_", weakness.cwe);
        let Some(dir) = dirs.iter().find(|d| {
            d.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix))
        }) else {
            return Err(Error::Io(format!(
                "juliet: {} has no directory for CWE-{}, so it is not the 1.3 suite",
                testcases.display(),
                weakness.cwe
            )));
        };
        let mut files = Vec::new();
        walk(dir, &mut files)?;
        let mut grouped: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for file in files {
            let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Some(id) = case_id(name) {
                grouped.entry(id).or_default().push(file);
            }
        }
        for (id, mut files) in grouped {
            if id.contains("listen_socket") {
                listening += 1;
                continue;
            }
            files.sort();
            all.push(Case { id, cwe: weakness.cwe, files });
        }
    }
    Ok((all, listening))
}

/// Every file under a directory, at any depth.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Io(format!("could not read {}: {e}", dir.display())))?;
    for entry in entries {
        let path = entry.map_err(|e| Error::Io(e.to_string()))?.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// The test id a file belongs to, or nothing when the file is not a C case Juliet builds on Linux.
///
/// `..._01.c` is case `..._01`, and `..._51a.c` and `..._51b.c` are both case `..._51`.
fn case_id(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".c")?;
    if !stem.starts_with("CWE") || stem.contains("w32") || stem.contains("wchar_t") {
        return None;
    }
    let numbered = stem.trim_end_matches(|c: char| c.is_ascii_lowercase());
    if stem.len() - numbered.len() > 1 || !numbered.ends_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(numbered.to_owned())
}

/// Compiles the support files and every case at every tier, and writes the script that links and
/// runs them.
///
/// Says which cases compiled, by tier and id. A case that did not is not an error here: Juliet
/// compiles on Linux with GCC, so it is a compiler bug, and it is counted and listed as one.
fn build<'a>(
    cases: &'a [Case],
    source: &Path,
    work: &Path,
    level: &str,
) -> Result<std::collections::BTreeSet<(&'static str, &'a str)>> {
    if work.exists() {
        std::fs::remove_dir_all(work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;
    let rucc = cost::compiler()?;
    let archive = crate::staticlib("rucc-safe-rt", TRIPLE)?;
    std::fs::copy(&archive, work.join("safe-rt.a"))
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", archive.display())))?;
    std::fs::write(work.join("driver.c"), DRIVER)
        .map_err(|e| Error::Io(format!("could not write the driver: {e}")))?;

    let support = source.join("testcasesupport");
    let compile = |tier: &str, file: &Path, out: &Path| -> Result<Option<String>> {
        let out = Command::new(&rucc)
            .args(["-c", &format!("--target={TRIPLE}"), &format!("-fsafety={tier}"), level])
            .arg(crate::VERIFY)
            .arg("-I")
            .arg(&support)
            .arg("-o")
            .arg(out)
            .arg(file)
            .output()
            .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
        if out.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").to_owned()))
    };

    let mut problems = Vec::new();
    for tier in TIERS {
        let dir = work.join(tier).join("support");
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::Io(format!("could not make {}: {e}", dir.display())))?;
        for name in SUPPORT {
            let out = dir.join(name.replace(".c", ".o"));
            if let Some(said) = compile(tier, &support.join(name), &out)? {
                problems.push(format!("{tier}: {name} did not compile: {said}"));
            }
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "juliet", problems });
    }

    let jobs: Vec<(&'static str, &Case)> =
        TIERS.iter().flat_map(|tier| cases.iter().map(move |case| (*tier, case))).collect();
    let next = AtomicUsize::new(0);
    let built = Mutex::new(std::collections::BTreeSet::new());
    let failed = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(tier, case)) = jobs.get(at) else {
                        break;
                    };
                    let dir = work.join(tier).join(&case.id);
                    let mut said = None;
                    if let Err(e) = std::fs::create_dir_all(&dir) {
                        said = Some(e.to_string());
                    }
                    for (n, file) in case.files.iter().enumerate() {
                        if said.is_some() {
                            break;
                        }
                        said = match compile(tier, file, &dir.join(format!("{n}.o"))) {
                            Ok(said) => said,
                            Err(e) => Some(e.to_string()),
                        };
                    }
                    if let Some(said) = said {
                        failed.lock().expect("a worker panicked").push((tier, &case.id, said));
                        let _ = std::fs::remove_dir_all(&dir);
                    } else {
                        built.lock().expect("a worker panicked").insert((tier, case.id.as_str()));
                    }
                }
            });
        }
    });
    let built = built.into_inner().expect("a worker panicked");
    let failed = failed.into_inner().expect("a worker panicked");
    let mut list = String::new();
    for (tier, id) in &built {
        let _ = writeln!(list, "{tier} {id}");
    }
    std::fs::write(work.join("cases"), list)
        .map_err(|e| Error::Io(format!("could not write the case list: {e}")))?;
    let mut unbuilt = String::new();
    for (tier, id, said) in failed {
        let _ = writeln!(unbuilt, "{tier} {id}: {said}");
    }
    std::fs::write(work.join("unbuilt.txt"), unbuilt)
        .map_err(|e| Error::Io(format!("could not write the unbuilt list: {e}")))?;
    std::fs::write(work.join("one.sh"), one())
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(built)
}

/// The support files every case links against, from Juliet's own makefiles.
const SUPPORT: [&str; 2] = ["io.c", "std_thread.c"];

/// The driver, which picks a half by its first argument.
///
/// One driver for every case, with the two names it calls bound to the case's own functions when
/// the case is linked, so that it is compiled once rather than once per case. It is compiled by the
/// system compiler and is not instrumented, and it touches no memory a case owns.
const DRIVER: &str = "\
void juliet_bad(void);
void juliet_good(void);
int main(int argc, char **argv)
{
    if (argc > 1 && argv[1][0] == 'b')
        juliet_bad();
    else
        juliet_good();
    return 0;
}
";

/// The script that links and runs every case, as many at a time as the machine has processors.
///
/// Each case prints one line, and a line that short goes out in one write, so the lines from
/// different cases can interleave but never tear. Everything it makes goes under `/tmp`, so the
/// work directory can be mounted read only.
const SCRIPT: &str = "\
#!/bin/sh
here=$(pwd)
out=/tmp/rucc-${here##*/}
rm -rf \"$out\"
mkdir -p \"$out\"
gcc -c driver.c -o \"$out/driver.o\" || exit 1
export out
xargs -P \"$(nproc)\" -n 2 sh one.sh < cases
rm -rf \"$out\"
";

/// The number a half that reads one is given.
///
/// Many cases take the value that goes wrong from outside: a line on the standard input read with
/// `fgets` or `fscanf`, or the variable `ADD`. With nothing there the bad half keeps the value it
/// started with, which for most of them is one its own check turns away, and the case is a miss
/// nobody could have caught. So every half is given a number, and the number is the one that makes
/// the bad half go wrong. Ten is one past the ten element buffers the overflow and overread cases
/// index, and the CWE-761 cases walk their pointer along whatever they are given and then free it
/// from where it stopped. The good halves check what they read, which is the difference.
const HIGH: i32 = 10;

/// The CWEs whose cases go wrong on a negative number rather than a large one, which are the two
/// that write and read before the start of a buffer and check only the upper bound.
const LOW: &[u32] = &[124, 127];

/// The variable the environment sources read.
const ENVIRONMENT: &str = "ADD";

/// One case at one tier: link it, run each half, and say what each did.
///
/// A half reported when the banner is anywhere in what it wrote, whatever its status. Otherwise it
/// is silent when it exited zero, hung when `timeout` stopped it, and stopped with its status when
/// anything else did.
///
/// Each half is given [`HIGH`], or minus one under one of the [`LOW`] CWEs, on its standard input
/// and in [`ENVIRONMENT`].
fn one() -> String {
    format!(
        "\
#!/bin/sh
tier=$1
id=$2
program=\"$out/$tier.$id\"
if ! gcc -no-pie \"$tier/$id\"/*.o \"$tier\"/support/*.o \"$out/driver.o\" safe-rt.a \\
    -lpthread -lm -ldl -Wl,--defsym=juliet_bad=\"${{id}}_bad\" \\
    -Wl,--defsym=juliet_good=\"${{id}}_good\" -o \"$program\" >/dev/null 2>&1; then
    printf '<<<%s %s unlinked>>>\\n' \"$tier\" \"$id\"
    exit 0
fi
case $id in
{low}) input=-1 ;;
*) input={HIGH} ;;
esac
half() {{
    printf '%s\\n' \"$input\" | {ENVIRONMENT}=\"$input\" timeout {SECONDS} \"$program\" \"$1\" \\
        >\"$program.$1\" 2>&1
    status=$?
    if grep -q '{BANNER}' \"$program.$1\"; then
        echo reported
    elif [ $status -eq 0 ]; then
        echo silent
    elif [ $status -eq 124 ]; then
        echo hung
    else
        echo \"stopped$status\"
    fi
}}
bad=$(half bad)
good=$(half good)
rm -f \"$program\" \"$program.bad\" \"$program.good\"
printf '<<<%s %s %s %s>>>\\n' \"$tier\" \"$id\" \"$bad\" \"$good\"
",
        low = LOW.iter().map(|cwe| format!("CWE{cwe}_*")).collect::<Vec<_>>().join("|"),
    )
}

/// Reads what the script printed into an outcome per tier and case.
fn read(text: &str) -> BTreeMap<(String, String), Outcome> {
    let mut runs = BTreeMap::new();
    for line in text.lines() {
        let Some(inner) = line.strip_prefix("<<<").and_then(|l| l.strip_suffix(">>>")) else {
            continue;
        };
        let words: Vec<&str> = inner.split(' ').collect();
        let outcome = match words.as_slice() {
            [_, _, "unlinked"] => Outcome::Unlinked,
            [_, _, bad, good] => match (half(bad), half(good)) {
                (Some(bad), Some(good)) => Outcome::Ran { bad, good },
                _ => continue,
            },
            _ => continue,
        };
        runs.insert((words[0].to_owned(), words[1].to_owned()), outcome);
    }
    runs
}

/// One half's word from the script.
fn half(word: &str) -> Option<Half> {
    match word {
        "reported" => Some(Half::Reported),
        "silent" => Some(Half::Silent),
        "hung" => Some(Half::Hung),
        _ => word.strip_prefix("stopped")?.parse().ok().map(Half::Stopped),
    }
}

/// What the task prints and what it writes out.
struct Report {
    /// The counts, per row and per tier, for the terminal.
    summary: String,
    /// The counts again and every id behind them, for the file.
    full: String,
}

/// Counts the outcomes per CWE and per row, and lists the ids.
///
/// A case is detected when its bad half reported and missed when it did not, whatever else it did:
/// a bad half the hardware stopped is a case the monitor missed and the processor caught. A case
/// is a false positive when its good half reported, which can be true of a detected case as well,
/// so the three counts are not meant to add up to the total. A case under an [`Excuse`] whose good
/// half reported is set aside and is in none of the three, and so is a case that did not compile
/// or link, and both are listed apart.
fn report(
    chosen: &[&Weakness],
    cases: &[Case],
    outcomes: &BTreeMap<(&str, &str), Outcome>,
    level: &str,
) -> Report {
    let mut tallies: BTreeMap<(u32, &str), Tally> = BTreeMap::new();
    for case in cases {
        for tier in TIERS {
            let tally = tallies.entry((case.cwe, tier)).or_default();
            match outcomes.get(&(tier, case.id.as_str())) {
                Some(Outcome::Ran { bad, good }) => {
                    let excuse = EXCUSES.iter().find(|e| e.covers(&case.id));
                    if let (Half::Reported, Some(excuse)) = (good, excuse) {
                        match tally.excused.iter_mut().find(|(e, _)| std::ptr::eq(*e, excuse)) {
                            Some((_, ids)) => ids.push(case.id.clone()),
                            None => tally.excused.push((excuse, vec![case.id.clone()])),
                        }
                        continue;
                    }
                    if *bad == Half::Reported {
                        tally.detected += 1;
                    } else {
                        tally.missed.push(case.id.clone());
                    }
                    if *good == Half::Reported {
                        tally.false_positive.push(case.id.clone());
                    }
                }
                Some(Outcome::Unbuilt) => tally.unbuilt.push(format!("{} (compile)", case.id)),
                Some(Outcome::Unlinked) | None => {
                    tally.unbuilt.push(format!("{} (link)", case.id));
                }
            }
        }
    }
    let total = |cwe: u32| cases.iter().filter(|c| c.cwe == cwe).count();

    let mut summary = String::new();
    let _ = writeln!(
        summary,
        "{:<5}{:<16}{:<9}{:>6}{:>10}{:>8}{:>8}{:>11}{:>11}",
        "cwe", "rows", "tier", "cases", "detected", "missed", "false+", "set aside", "not built"
    );
    for weakness in chosen {
        let rows = weakness.rows.join(" ");
        let cases = total(weakness.cwe);
        for tier in TIERS {
            let Some(tally) = tallies.get(&(weakness.cwe, tier)) else {
                continue;
            };
            let _ = writeln!(
                summary,
                "{:<5}{rows:<16}{tier:<9}{cases:>6}{:>10}{:>8}{:>8}{:>11}{:>11}",
                weakness.cwe,
                tally.detected,
                tally.missed.len(),
                tally.false_positive.len(),
                tally.excused.iter().map(|(_, ids)| ids.len()).sum::<usize>(),
                tally.unbuilt.len()
            );
        }
    }
    for weakness in chosen {
        for tier in TIERS {
            let Some(tally) = tallies.get(&(weakness.cwe, tier)) else {
                continue;
            };
            for (excuse, ids) in &tally.excused {
                let _ = writeln!(
                    summary,
                    "CWE-{} {tier}: {} cases set aside, {}",
                    weakness.cwe,
                    ids.len(),
                    excuse.why
                );
            }
        }
    }
    for (cwe, rows) in ABSENT {
        let _ = writeln!(summary, "CWE-{cwe} ({rows}): Juliet 1.3 has no C cases for it");
    }

    let mut full = format!("Juliet C/C++ 1.3, C cases only, {level}\n\n{summary}");
    for weakness in chosen {
        let _ = writeln!(
            full,
            "\nCWE-{} {}, rows {}, {} cases",
            weakness.cwe,
            weakness.what,
            weakness.rows.join(" "),
            total(weakness.cwe)
        );
        for tier in TIERS {
            let Some(tally) = tallies.get(&(weakness.cwe, tier)) else {
                continue;
            };
            let excused = tally.excused.iter().flat_map(|(_, ids)| ids.iter().cloned()).collect();
            for (what, ids) in [
                ("missed", &tally.missed),
                ("false positive", &tally.false_positive),
                ("set aside", &excused),
                ("not built", &tally.unbuilt),
            ] {
                if ids.is_empty() {
                    continue;
                }
                let _ = writeln!(full, "  {tier} {what} ({}):", ids.len());
                for id in ids {
                    let _ = writeln!(full, "    {id}");
                }
            }
        }
    }
    Report { summary, full }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numbered_file_is_a_case_and_lettered_files_share_one() {
        assert_eq!(
            case_id("CWE416_Use_After_Free__malloc_free_char_01.c").as_deref(),
            Some("CWE416_Use_After_Free__malloc_free_char_01")
        );
        assert_eq!(
            case_id("CWE121_Stack_Based_Buffer_Overflow__CWE129_fgets_51a.c"),
            case_id("CWE121_Stack_Based_Buffer_Overflow__CWE129_fgets_51b.c")
        );
        assert_eq!(
            case_id("CWE121_Stack_Based_Buffer_Overflow__CWE129_fgets_51b.c").as_deref(),
            Some("CWE121_Stack_Based_Buffer_Overflow__CWE129_fgets_51")
        );
    }

    #[test]
    fn what_the_linux_makefiles_leave_out_is_left_out() {
        assert_eq!(case_id("CWE416_Use_After_Free__malloc_free_wchar_t_01.c"), None);
        assert_eq!(case_id("CWE590_Free_Memory_Not_on_Heap__free_char_alloca_w32_01.c"), None);
        assert_eq!(case_id("CWE416_Use_After_Free__malloc_free_char_43.cpp"), None);
        assert_eq!(case_id("main_linux.c"), None);
        assert_eq!(case_id("CWE416.bat"), None);
    }

    #[test]
    fn the_script_line_reads_back_as_the_outcome() {
        let ran = read(
            "noise\n<<<detect CWE1_x_01 reported silent>>>\n<<<kernel CWE1_x_01 \
             stopped139 hung>>>\n<<<enforce CWE1_x_01 unlinked>>>\n",
        );
        let at = |tier: &str| ran.get(&(tier.to_owned(), "CWE1_x_01".to_owned())).copied();
        assert_eq!(at("detect"), Some(Outcome::Ran { bad: Half::Reported, good: Half::Silent }));
        assert_eq!(at("kernel"), Some(Outcome::Ran { bad: Half::Stopped(139), good: Half::Hung }));
        assert_eq!(at("enforce"), Some(Outcome::Unlinked));
    }

    #[test]
    fn the_script_looks_for_the_banner_the_runtime_prints() {
        let one = one();
        assert!(one.contains(&format!("grep -q '{BANNER}'")));
        assert!(one.contains("--defsym=juliet_bad=\"${id}_bad\""));
        assert!(one.contains("printf '<<<%s %s unlinked>>>\\n'"));
    }

    #[test]
    fn a_half_is_given_the_number_that_makes_its_bad_half_go_wrong() {
        let one = one();
        assert!(one.contains("CWE124_*|CWE127_*) input=-1 ;;\n*) input=10 ;;"));
        assert!(one.contains("printf '%s\\n' \"$input\" | ADD=\"$input\" timeout 10 "));
    }

    #[test]
    fn a_case_missed_by_one_tier_is_counted_and_named_under_that_tier() {
        let case = Case { id: "CWE416_a_01".to_owned(), cwe: 416, files: Vec::new() };
        let weakness = WEAKNESSES.iter().find(|w| w.cwe == 416).expect("416 is in the table");
        let mut outcomes = BTreeMap::new();
        outcomes.insert(
            ("detect", "CWE416_a_01"),
            Outcome::Ran { bad: Half::Reported, good: Half::Silent },
        );
        outcomes.insert(
            ("enforce", "CWE416_a_01"),
            Outcome::Ran { bad: Half::Stopped(139), good: Half::Reported },
        );
        outcomes.insert(("kernel", "CWE416_a_01"), Outcome::Unbuilt);
        let report = report(&[weakness], &[case], &outcomes, "-O2");
        assert!(report.summary.contains(
            "416  T1 T5 T6 C4     detect        1         1       0       0          0          0"
        ));
        assert!(report.summary.contains(
            "416  T1 T5 T6 C4     enforce       1         0       1       1          0          0"
        ));
        assert!(report.summary.contains(
            "416  T1 T5 T6 C4     kernel        1         0       0       0          0          1"
        ));
        assert!(report.full.contains("  enforce missed (1):\n    CWE416_a_01\n"));
        assert!(report.full.contains("  kernel not built (1):\n    CWE416_a_01 (compile)\n"));
    }

    #[test]
    fn a_case_wrong_before_its_mistake_is_set_aside_and_not_counted_either_way() {
        let excused =
            Case { id: "CWE843_Type_Confusion__char_01".to_owned(), cwe: 843, files: Vec::new() };
        let other =
            Case { id: "CWE843_Type_Confusion__short_02".to_owned(), cwe: 843, files: Vec::new() };
        let weakness = WEAKNESSES.iter().find(|w| w.cwe == 843).expect("843 is in the table");
        let mut outcomes = BTreeMap::new();
        for tier in TIERS {
            outcomes.insert(
                (tier, excused.id.as_str()),
                Outcome::Ran { bad: Half::Reported, good: Half::Reported },
            );
            // The excuse is about what the good half did, so one whose good half was quiet is
            // counted like any other case.
            outcomes.insert(
                (tier, other.id.as_str()),
                Outcome::Ran { bad: Half::Reported, good: Half::Silent },
            );
        }
        let report = report(&[weakness], &[excused.clone(), other.clone()], &outcomes, "-O2");
        assert!(report.summary.contains("detect        2         1       0       0          1"));
        assert!(report.summary.contains("CWE-843 detect: 1 cases set aside, both halves point"));
        assert!(
            report.full.contains("  detect set aside (1):\n    CWE843_Type_Confusion__char_01\n")
        );
    }

    #[test]
    fn an_excuse_about_one_variant_covers_that_variant_only() {
        let excuse = EXCUSES.iter().find(|e| e.variant == Some("_32")).expect("there is one");
        assert!(excuse.covers("CWE121_Stack_Based_Buffer_Overflow__CWE805_char_declare_loop_32"));
        assert!(!excuse.covers("CWE121_Stack_Based_Buffer_Overflow__CWE805_char_declare_loop_31"));
        assert!(!excuse.covers("CWE122_Heap_Based_Buffer_Overflow__c_CWE805_char_loop_32"));
    }
}
