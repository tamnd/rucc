//! Times the four block routines against the C library's own.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8, whose last sentence about the block routines
//! is that the word at a time paths want a benchmark to hold them to rather than an opinion. This
//! is that benchmark, and it is a task of its own rather than part of `cargo xtask ci` for the
//! reason `bench` is: a timing on a machine that is also doing something else is not a check, and a
//! check that fails when somebody starts a build is a check people turn off.
//!
//! The shape is the one `builtins_diff` uses, because the hazards are the same ones.
//! `tests/builtins/bench.c` is compiled twice by the system compiler, once with the archive rucc
//! wrote on the link line and once without it so the four names resolve in the C library, and
//! neither program knows which side it got. Nothing is renamed, so what is timed is the archive a
//! person is handed rather than a copy of it with different symbols.
//!
//! # Why the C library and not the Rust reference
//!
//! The reference next door exists to answer whether the two implementations agree, and the
//! differential asks it twenty seven thousand times. It is no use as a denominator for a speed
//! number, because it is the same algorithm written again and two byte loops racing each other says
//! nothing. What a program actually gets when it is not freestanding is glibc's memcpy, which is
//! vector assembly picked at load time from what the machine reports, and that is a number no
//! portable C will reach. It is still the right denominator: it means the same thing on two
//! machines, which a microsecond count does not, and the distance to it is the honest description
//! of what these routines cost a freestanding program.
//!
//! # The guard, which runs the other way round from the differential's
//!
//! There it is a failure for the harness to reach a name outside the archive. Here that is a failure
//! on one side and the whole point on the other, so the script reports which of the four names each
//! program left undefined and the task holds the two sides to opposite answers: the archive side
//! must have reached none of them outside itself, and the library side must have reached all four.
//! A library side that resolved one of them somewhere else would be timing our routine against our
//! routine and reporting a ratio of one.
//!
//! The fortification is explicitly off for the same reason it is off next door. On a distribution
//! that turns `_FORTIFY_SOURCE` on by default the calls in the harness become calls to glibc's
//! `__memcpy_chk`, which is a different routine from the one the row is named after and is on the
//! wrong side of the link line besides.
//!
//! # What the rows are
//!
//! Four lengths at two shapes for each of five cases, where the fifth case is `memmove` over
//! overlapping ranges, which is the one path in these four routines that nothing else exercises.
//! The two shapes are the reason the table is not a list of lengths: a word at a time loop only
//! runs when the source and the destination reach a word boundary together, so the skewed rows are
//! the ones that still run a byte at a time and a report without them would claim an improvement
//! that half the calls in a program never see.

use std::path::{Path, PathBuf};

use crate::bench::{Stats, commit, host};
use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, root};

/// What this task is called, for the messages.
const TASK: &str = "builtins-bench";

/// The two sides, spelled the way the script prefixes their output.
const SIDES: [&str; 2] = ["ours", "libc"];

/// The four names, which are what both guards are about.
const NAMES: [&str; 4] = ["memcpy", "memmove", "memset", "memcmp"];

/// How many timings are taken by default, which is what section 16.2 asks of a benchmark here.
const RUNS: usize = 10;

/// Runs the benchmark and prints the report.
///
/// # Errors
///
/// [`Error::Io`] when the options are wrong or the archive will not build, and [`Error::Failed`]
/// when either side reached the wrong routines, when the two runs did not get through the same
/// cases, or when they disagree about what the copies produced.
pub(crate) fn builtins_bench(args: &[String]) -> Result<()> {
    let mut runs = RUNS;
    let mut csv = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--runs" => {
                let value = rest.next().ok_or_else(|| bad("--runs needs a count"))?;
                runs = value.parse().map_err(|_| bad("--runs needs a count"))?;
                if runs < 4 {
                    // Three quartiles out of three points is not a range, it is three points.
                    return Err(bad("--runs needs at least 4, or the quartiles are not quartiles"));
                }
            }
            "--csv" => csv = true,
            other => return Err(bad(format!("unknown option `{other}`"))),
        }
    }

    let work = build(runs)?;
    let runner = Runner::find("the block routine benchmark")?;
    let printed = runner.run(&work, "the block routine benchmark")?;
    let cases = read(&printed)?;
    if csv {
        print_csv(&cases);
    } else {
        print_report(&cases, runs, runner);
    }
    Ok(())
}

/// Lays out the directory the runner is pointed at: the archive, the harness, and the script.
fn build(runs: usize) -> Result<PathBuf> {
    let work = root().join("target").join(TASK);
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let ours = crate::builtins_archive(TRIPLE)?;
    copy(&ours, &work.join("librucc_builtins.a"))?;
    let harness = root().join("tests").join("builtins").join("bench.c");
    copy(&harness, &work.join("bench.c"))?;
    std::fs::write(work.join("run.sh"), SCRIPT.replace("@RUNS@", &runs.to_string()))
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// One file into the work directory, which is copied rather than read from the tree because the
/// runner mounts the directory read only and a container cannot reach anything outside it.
fn copy(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", from.display())))?;
    Ok(())
}

/// What the runner runs.
///
/// `-O2` rather than the `-O1` the differential uses, because this one is about time and a loop
/// nobody optimized is not the loop a program would have around a call. The archive is on one link
/// line and not the other, which is the whole difference between the two programs.
///
/// The two programs run alternately, one timing each, rather than one of them running to the end
/// and then the other. `xtask/src/cost.rs` settled that pattern for the same reason it is needed
/// here: a machine that gets slower halfway through should slow both sides of every ratio, and the
/// first version of this measured the rewrite against the byte loops on a machine that was busier
/// during one half than the other and reported a regression that was not there.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/builtins-bench
mkdir -p \"$out\"
flags=\"-O2 -fno-builtin -U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=0\"
gcc $flags -o \"$out/ours\" bench.c librucc_builtins.a || exit 1
gcc $flags -o \"$out/libc\" bench.c || exit 1
for side in ours libc; do
    nm -u \"$out/$side\" | awk -v side=$side '{ s=$NF; sub(/@.*/, \"\", s); \\
        if (s ~ /^mem(cpy|move|set|cmp)$/) print side \" reached \" s }'
done
nm -g --defined-only librucc_builtins.a | awk '$2 == \"T\" { print $3 }' > \"$out/names\"
for name in memcpy memmove memset memcmp; do
    grep -qx \"$name\" \"$out/names\" || echo \"ours missing $name\"
done
round=0
while [ $round -lt @RUNS@ ]; do
    round=$((round + 1))
    \"$out/ours\" 1 1 | sed 's/^/ours /'
    \"$out/libc\" 1 1 | sed 's/^/libc /'
done
";

/// One row of the report, which is one case timed on both sides.
struct Case {
    /// The routine, the length and the shape, as the harness spelled them.
    name: String,
    /// How many bytes one timing moved, which is the same on every row and is what makes the
    /// throughput columns comparable across lengths.
    bytes: u64,
    ours: Stats,
    libc: Stats,
}

impl Case {
    /// Megabytes a second, from the median of the timings.
    fn rate(bytes: u64, stats: &Stats) -> f64 {
        // The timings are nanoseconds, so bytes over nanoseconds is gigabytes a second and a
        // thousand of that is megabytes a second.
        bytes as f64 * 1000.0 / stats.median
    }

    /// How many times as long our routine took as the library's.
    fn ratio(&self) -> f64 {
        self.ours.median / self.libc.median
    }
}

/// What one side of the run said.
#[derive(Default)]
struct Side {
    /// One entry per case, in the order the cases first ran, with the timings from every round
    /// of that case appended to it.
    cases: Vec<(String, u64, Vec<f64>)>,
    /// What the copies came out as, one per round, which both sides have to agree about.
    checksums: Vec<String>,
    /// The names this side went looking for outside itself.
    reached: Vec<String>,
}

/// Turns what the two programs printed into rows, and refuses when the run does not hold together.
fn read(printed: &str) -> Result<Vec<Case>> {
    let mut sides: [Side; 2] = [Side::default(), Side::default()];
    let mut problems = Vec::new();
    for line in printed.lines() {
        let mut words = line.split_whitespace();
        let Some(side) = words.next() else { continue };
        let Some(at) = SIDES.iter().position(|s| *s == side) else { continue };
        match words.next() {
            Some("missing") => {
                let name = words.next().unwrap_or("?");
                problems.push(format!(
                    "the archive does not define {name}, so the link reached past it into the C \
                     library and both sides timed the same routine"
                ));
            }
            Some("case") => {
                let rest: Vec<&str> = words.collect();
                if rest.len() < 5 {
                    problems.push(format!("`{line}` is not a case"));
                    continue;
                }
                let name = rest[..3].join(" ");
                let bytes = rest[3].parse().unwrap_or(0);
                let times = rest[4..].iter().filter_map(|t| t.parse::<f64>().ok());
                match sides[at].cases.iter_mut().find(|(seen, _, _)| *seen == name) {
                    Some(case) => case.2.extend(times),
                    None => sides[at].cases.push((name, bytes, times.collect())),
                }
            }
            Some("checksum") => {
                if let Some(value) = words.next() {
                    sides[at].checksums.push(value.to_owned());
                }
            }
            Some("reached") => {
                if let Some(name) = words.next() {
                    sides[at].reached.push(name.to_owned());
                }
            }
            _ => {}
        }
    }

    // The two guards, which want opposite answers from the two sides. Ours reaching a name outside
    // itself means the archive on the link line was skipped, and the library side reaching none of
    // them means it was linked against ours after all, and either way the ratio below would be a
    // routine compared with itself.
    for name in &sides[0].reached {
        problems.push(format!(
            "the archive side calls {name} outside itself, so the routine on the link line was \
             not the routine that ran"
        ));
    }
    for name in NAMES {
        if !sides[1].reached.iter().any(|r| r == name) {
            problems.push(format!(
                "the library side does not call {name} outside itself, so it is not the C \
                 library's routine that ran"
            ));
        }
    }
    if sides[0].cases.len() != sides[1].cases.len() {
        problems.push(format!(
            "the two sides ran {} and {} cases, so there is nothing to compare row by row",
            sides[0].cases.len(),
            sides[1].cases.len()
        ));
    }
    if sides[0].checksums != sides[1].checksums {
        problems.push(format!(
            "the two sides produced different bytes: {:?} against {:?}. The differential is the \
             check for that, and this run is a speed number for a routine that is wrong",
            sides[0].checksums, sides[1].checksums
        ));
    }
    if sides[0].cases.is_empty() {
        problems.push("neither side printed a case, so nothing was timed".to_owned());
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: TASK, problems });
    }

    let mut cases = Vec::new();
    for (ours, libc) in sides[0].cases.iter().zip(&sides[1].cases) {
        if ours.0 != libc.0 {
            return Err(Error::Failed {
                task: TASK,
                problems: vec![format!("the two sides ran `{}` against `{}`", ours.0, libc.0)],
            });
        }
        let mut one = ours.2.clone();
        let mut two = libc.2.clone();
        cases.push(Case {
            name: ours.0.clone(),
            bytes: ours.1,
            ours: Stats::of(&mut one),
            libc: Stats::of(&mut two),
        });
    }
    Ok(cases)
}

/// Prints the human readable report.
fn print_report(cases: &[Case], runs: usize, runner: Runner) {
    let moved = cases.first().map_or(0, |c| c.bytes) as f64 / (1024.0 * 1024.0);
    println!(
        "block routines: {} cases, {runs} runs each, {moved:.1} MB moved per run",
        cases.len()
    );
    println!(
        "times are microseconds, rates are megabytes a second, ratio is ours over the library"
    );
    println!();
    println!(
        "  {:<28} {:>9} {:>7} {:>8}   {:>9} {:>7} {:>8}   {:>7}",
        "case", "ours", "IQR", "MB/s", "libc", "IQR", "MB/s", "ratio"
    );
    for case in cases {
        println!(
            "  {:<28} {:>9.1} {:>7.1} {:>8.0}   {:>9.1} {:>7.1} {:>8.0}   {:>6.1}x",
            case.name,
            case.ours.median / 1000.0,
            case.ours.iqr() / 1000.0,
            Case::rate(case.bytes, &case.ours),
            case.libc.median / 1000.0,
            case.libc.iqr() / 1000.0,
            Case::rate(case.bytes, &case.libc),
            case.ratio(),
        );
    }

    // The two numbers worth quoting out of forty rows, and the median rather than the mean for the
    // same reason every other number here is a median: one row on a machine that got interrupted
    // should not move the summary.
    let mut ratios: Vec<f64> = cases.iter().map(Case::ratio).collect();
    let middle = Stats::of(&mut ratios).median;
    let worst = cases.iter().max_by(|a, b| a.ratio().total_cmp(&b.ratio()));
    println!();
    println!("  the middle row takes {middle:.1} times the C library's time");
    if let Some(worst) = worst {
        println!("  the worst is {} at {:.1} times", worst.name, worst.ratio());
    }

    // A row whose interquartile range is a large part of its own median was measured on a machine
    // that was doing something else, and a comparison against another run of it means nothing.
    // Saying which rows those are is cheaper than somebody quoting one of them.
    let noisy: Vec<&str> = cases
        .iter()
        .filter(|c| c.ours.iqr() * 10.0 > c.ours.median || c.libc.iqr() * 10.0 > c.libc.median)
        .map(|c| c.name.as_str())
        .collect();
    if !noisy.is_empty() {
        println!();
        println!("  {} rows have an interquartile range past a tenth of their own", noisy.len());
        println!("  median, so this machine was busy and those rows are not comparable:");
        println!("  {}", noisy.join(", "));
    }
    if runner == Runner::Container {
        println!();
        println!("  these numbers were taken inside a container, on a machine that is not the one");
        println!("  the programs were compiled for, so they are a smoke test and not a benchmark");
    }
}

/// Prints one row per metric, which is what section 16.6 asks a nightly to write.
///
/// The same columns `cargo xtask bench --csv` writes, so that the two benchmarks land in one table.
/// The compiler column holds the side here, since what is being compared is two implementations of
/// a routine rather than two compilers.
fn print_csv(cases: &[Case]) {
    let commit = commit();
    let host = host();
    println!("commit,host,suite,benchmark,compiler,metric,value,iqr");
    for case in cases {
        let name = case.name.replace(' ', "-");
        for (side, stats) in [("ours", &case.ours), ("libc", &case.libc)] {
            println!(
                "{commit},{host},builtins,{name},{side},wall_ns,{:.1},{:.1}",
                stats.median,
                stats.iqr()
            );
            println!(
                "{commit},{host},builtins,{name},{side},mb_per_second,{:.1},",
                Case::rate(case.bytes, stats)
            );
        }
    }
}

fn bad(message: impl Into<String>) -> Error {
    Error::Io(format!("{TASK}: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a clean run prints, shortened to two cases.
    const CLEAN: &str = "\
libc reached memcpy
libc reached memmove
libc reached memset
libc reached memcmp
ours case memcpy 8 aligned 1048576 999000.0 1000000.0 1000000.0 1001000.0
ours case memcpy 64 aligned 1048576 800000.0 800000.0 800000.0 800000.0
ours checksum 77
ours sink 0
libc case memcpy 8 aligned 1048576 499000.0 500000.0 500000.0 501000.0
libc case memcpy 64 aligned 1048576 400000.0 400000.0 400000.0 400000.0
libc checksum 77
libc sink 0
";

    /// What the task said about a run it refused, for the tests that want the refusal.
    ///
    /// A helper rather than `expect_err`, because that wants the success type to be printable and
    /// a row holds five statistics nobody would ever want printed.
    fn refused(printed: &str) -> String {
        match read(printed) {
            Ok(cases) => {
                panic!("{} cases came out of a run that should have been refused", cases.len())
            }
            Err(problems) => format!("{problems}"),
        }
    }

    #[test]
    fn a_clean_run_becomes_rows() {
        let cases = read(CLEAN).expect("the run is clean");
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].name, "memcpy 8 aligned");
        assert_eq!(cases[0].bytes, 1_048_576);
        // Two distributions a factor of two apart, which is the ratio the last column shows.
        assert!((cases[0].ratio() - 2.0).abs() < 1e-9);
        // A mebibyte in a millisecond, which is 1048.576 megabytes a second, and getting the
        // thousand in that conversion wrong is the mistake worth a test.
        assert!((Case::rate(cases[0].bytes, &cases[0].ours) - 1048.576).abs() < 1e-3);
    }

    #[test]
    fn rounds_of_the_same_case_are_one_row() {
        // The two programs run alternately and each one prints every case every time, so a case
        // that came round twice is one row with both timings on it rather than two rows.
        let twice = format!(
            "{CLEAN}ours case memcpy 8 aligned 1048576 1002000.0\n\
             ours checksum 77\n\
             libc case memcpy 8 aligned 1048576 502000.0\n\
             libc checksum 77\n"
        );
        let cases = read(&twice).expect("the run is clean");
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].name, "memcpy 8 aligned");
        // Five timings on the row now, so the median moved up one place.
        assert!((cases[0].ours.max - 1_002_000.0).abs() < 1e-9);
        assert!((cases[0].ours.median - 1_000_000.0).abs() < 1e-9);
    }

    #[test]
    fn the_archive_side_reaching_a_name_outside_itself_is_a_failure() {
        // The fortification case, which is what this guard exists for: the harness called
        // something else and the archive on the link line was never reached.
        let printed =
            CLEAN.replace("libc reached memcpy", "libc reached memcpy\nours reached memcpy");
        assert!(refused(&printed).contains("the archive side calls memcpy"));
    }

    #[test]
    fn the_library_side_reaching_nothing_outside_itself_is_a_failure() {
        // The other half of the guard. A library side that resolved the four names somewhere else
        // is our own routine wearing the denominator's name.
        let printed = CLEAN.replace("libc reached memset\n", "");
        assert!(refused(&printed).contains("the library side does not call memset"));
    }

    #[test]
    fn a_name_the_archive_does_not_define_is_a_failure() {
        let printed = format!("{CLEAN}ours missing memmove\n");
        assert!(refused(&printed).contains("the archive does not define memmove"));
    }

    #[test]
    fn two_sides_that_copied_different_bytes_are_a_failure() {
        // A speed number for a routine that produces the wrong answer is worse than no number.
        let printed = CLEAN.replace("ours checksum 77", "ours checksum 78");
        assert!(refused(&printed).contains("different bytes"));
    }

    #[test]
    fn a_run_with_nothing_in_it_is_a_failure() {
        // The shape a script that died after the guards would leave, which must not read as a
        // benchmark that found nothing to say.
        assert!(refused("").contains("neither side printed a case"));
    }
}
