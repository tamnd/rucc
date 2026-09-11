//! What the monitor costs at run time.
//!
//! Design: `spec/safe-memory/13-performance.md` and `spec/safe-memory/16-milestones.md` milestone
//! S1, whose last exit criterion is that the unoptimized overhead is measured and written down as
//! the baseline every later claim improves on.
//!
//! Each program in `bench/safety` is compiled twice from the same source with the same flags, once
//! with the monitor off and once with it on, and both are run in the same loop on the same machine.
//! The report is a ratio per program, because section 13.4 rule 1 says a geomean may appear beside
//! the table and never instead of it, and rule 2 says the worst case is a headline rather than a
//! footnote.
//!
//! # What this measures and what it does not
//!
//! Wall clock only. Section 13.1 asks for cache misses, memory traffic, peak RSS, branch
//! mispredictions and spill counts alongside it, and says an instruction count is never the
//! headline because the predicted dominant cost is a second cache line per node that no
//! instruction count can see. None of those counters are available through the container this runs
//! in on a developer machine, and reading them on the native runner means `perf`, which needs a
//! permission a CI runner does not give. So the number here is the one anybody can reproduce, the
//! missing metrics are named rather than quietly skipped, and the row that would tell us most is
//! the linked list one, where the prediction says the wall clock will move for a reason the
//! instruction count would not explain.
//!
//! The spill counts are the one of those that is now measured, in [`crate::pressure`], and it gets
//! them out of the compiler rather than off the machine, which is why it needs no runner and no
//! permission.
//!
//! # Why `-O0` is the default and not the only choice
//!
//! Section 13.2 says the baseline for an overhead number is `rucc -O2` with safety off. That is the
//! right baseline for a claim about a tier's budget and S1's number is not one. S1 has no check
//! elimination in it at all, deliberately, and the milestone calls its own number the unoptimized
//! baseline for exactly that reason. So `-O0` on both sides is the default and stays the default,
//! because it isolates the monitor from the optimizer and it is the number every later claim is an
//! improvement on.
//!
//! The other number is S4's, where the question is how much of the monitor the elimination rules
//! take back, and asking it means running the same eight programs at `-O2` on both sides. That is
//! the same measurement with one flag changed, so the level is an argument rather than a second
//! task. Both sides always get the same level, which is the whole point of the ratio: a comparison
//! between an optimized program with the monitor off and an unoptimized one with it on would be a
//! measurement of the optimizer.
//!
//! # Why the emulated run is not a data point
//!
//! On a machine that is not x86-64 Linux the programs run in a container under emulation, which
//! changes the ratio between an instruction and a cache miss, which is the ratio this whole
//! document is about. That run is useful for checking the apparatus works and is worthless as a
//! measurement, so it says so in its own output rather than leaving somebody to notice.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, root, staticlib};

/// The optimization level used when the caller names none.
///
/// S1's number, which is the one every later claim improves on, so changing this would silently
/// move a published baseline rather than add a measurement beside it.
const LEVEL: &str = "-O0";

/// The levels this will run at.
///
/// A named list rather than anything the compiler is asked, because a level that the compiler
/// accepts and that means nothing here, `-Og` say, would come back as a table of ratios that look
/// fine and answer a question nobody asked. These four are the ones the performance document
/// mentions.
const LEVELS: &[&str] = &["-O0", "-O1", "-O2", "-Os"];

/// How many timings are taken and how many are thrown away first.
///
/// The same ten and three as `spec/16-performance.md` section 16.2, for the same reasons: ten is
/// enough for a median with quartiles either side of it, and the first runs of anything are a
/// measurement of a cold page cache rather than of the program.
const RUNS: usize = 10;
const WARMUPS: usize = 3;

/// One program, built both ways.
#[derive(Debug)]
pub(crate) struct Bench {
    /// The file name without its extension.
    pub(crate) name: String,
    /// The file itself.
    pub(crate) path: PathBuf,
}

/// What one build of one program did, over every timed run.
#[derive(Debug, Default)]
struct Times {
    /// Nanoseconds, one per run, in the order they happened.
    runs: Vec<u64>,
}

impl Times {
    /// The median, which is the number reported.
    fn median(&self) -> f64 {
        middle(&self.runs)
    }

    /// The interquartile range, which is what says whether a difference means anything.
    ///
    /// The quartiles are the medians of the two halves of the run in time order sorted, not of
    /// the two halves of the run in the order it happened, which would be two arbitrary subsets
    /// and can come out negative.
    fn spread(&self) -> f64 {
        let mut sorted = self.runs.clone();
        sorted.sort_unstable();
        let half = sorted.len() / 2;
        let low = middle(&sorted[..half]);
        let high = middle(&sorted[sorted.len() - half..]);
        high - low
    }
}

/// The median of a sorted-on-the-spot copy.
fn middle(runs: &[u64]) -> f64 {
    if runs.is_empty() {
        return 0.0;
    }
    let mut sorted = runs.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        #[expect(clippy::cast_precision_loss, reason = "nanoseconds, and these are milliseconds")]
        return (sorted[mid - 1] as f64 + sorted[mid] as f64) / 2.0;
    }
    #[expect(clippy::cast_precision_loss, reason = "nanoseconds, and these are milliseconds")]
    {
        sorted[mid] as f64
    }
}

/// Builds every benchmark both ways, runs them, and prints the table.
///
/// # Errors
///
/// [`Error::Io`] when a program will not compile, when there is no way to run an x86-64 Linux
/// program, or when a run produced no timings.
pub(crate) fn cost(args: &[String]) -> Result<()> {
    let level = level(args)?;
    let extra = extra(args);
    let benches = benches()?;
    let runner = Runner::find("this measurement")?;
    let named = if extra.is_empty() { String::new() } else { format!(" {}", extra.join(" ")) };
    println!("cost: {} programs, {level}{named} both sides, {runner}", benches.len());

    let work = build(&benches, level, &extra)?;
    let times = read(&runner.run(&work, "the benchmarks")?);

    let mut rows = Vec::new();
    for bench in &benches {
        let off = times.get(&format!("{}.off", bench.name)).ok_or_else(|| {
            Error::Io(format!("{} produced no timings with safety off", bench.name))
        })?;
        let on = times.get(&format!("{}.on", bench.name)).ok_or_else(|| {
            Error::Io(format!("{} produced no timings with safety on", bench.name))
        })?;
        rows.push((bench.name.clone(), off.median(), off.spread(), on.median(), on.spread()));
    }

    report(&rows, &runner, level);
    Ok(())
}

/// The `-f` flags the caller wants both sides compiled with, in the order they gave them.
///
/// Section 13.5 asks what each elimination source is worth on its own, and the only honest way to
/// ask that of a benchmark is to turn one off and run the same eight programs again. Both sides get
/// them, not just the side with the monitor on: a flag that changed the code on one side only would
/// make the ratio a measurement of the flag rather than of the monitor. The flags a run used are
/// printed above the table, because a table of ratios with no record of what produced it is the one
/// thing section 13.4 says a measurement must not be.
fn extra(args: &[String]) -> Vec<String> {
    args.iter().filter(|arg| !LEVELS.contains(&arg.as_str())).cloned().collect()
}

/// The level the caller asked for, or the default when they asked for nothing.
///
/// # Errors
///
/// [`Error::Io`] when more than one level is given or when the argument is not one of [`LEVELS`].
fn level(args: &[String]) -> Result<&'static str> {
    let mut found = None;
    for arg in args {
        if arg.starts_with("-f") {
            continue;
        }
        let Some(&known) = LEVELS.iter().find(|&&known| known == arg) else {
            return Err(Error::Io(format!(
                "cost takes an optimization level and `-f` flags, and `{arg}` is neither one of {} \
                 nor a flag",
                LEVELS.join(", ")
            )));
        };
        if found.is_some_and(|had| had != known) {
            return Err(Error::Io(
                "cost runs at one level, since both sides of a ratio have to have the same one"
                    .to_owned(),
            ));
        }
        found = Some(known);
    }
    Ok(found.unwrap_or(LEVEL))
}

/// Prints the table and the two summary numbers section 13.4 asks to see together.
fn report(rows: &[(String, f64, f64, f64, f64)], runner: &Runner, level: &str) {
    println!();
    println!("{:<32} {:>12} {:>12} {:>8}", "program", "safety off", "safety on", "ratio");
    let mut log = 0.0f64;
    let mut worst = ("", 0.0f64);
    for (name, off, off_spread, on, on_spread) in rows {
        let ratio = if *off > 0.0 { on / off } else { 0.0 };
        println!("{name:<32} {:>9.1} ms {:>9.1} ms {ratio:>7.2}x", off / 1e6, on / 1e6);
        println!(
            "{:<32} {:>9.1} ms {:>9.1} ms",
            "  interquartile range",
            off_spread / 1e6,
            on_spread / 1e6
        );
        log += ratio.ln();
        if ratio > worst.1 {
            worst = (name, ratio);
        }
    }
    #[expect(clippy::cast_precision_loss, reason = "a handful of benchmarks")]
    let geomean = (log / rows.len() as f64).exp();
    println!();
    println!(
        "cost: {geomean:.2}x geomean at {level}, {:.2}x worst case, which is {}",
        worst.1, worst.0
    );
    if level == LEVEL {
        println!(
            "cost: this is the unoptimized baseline, with no check elimination on either side. \
             How much of it the rules take back is the same task at -O2."
        );
    }
    println!(
        "cost: wall clock only. Section 13.1 also asks for cache misses, memory traffic, peak \
         RSS and branch mispredictions, and none of those are readable here. The spill counts \
         it asks for are `cargo xtask pressure`, which reads them out of the compiler."
    );
    if matches!(runner, Runner::Container) {
        println!(
            "cost: this ran under emulation, which moves the cost of a cache miss relative to the \
             cost of an instruction, and that ratio is what the number is about. Not a data point."
        );
    }
}

/// Every program on disk, in the order a directory listing gives them.
pub(crate) fn benches() -> Result<Vec<Bench>> {
    let dir = root().join("bench").join("safety");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| Error::Io(format!("could not read {}: {e}", dir.display())))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err(Error::Io(format!("{} has no programs in it", dir.display())));
    }
    paths
        .iter()
        .map(|path| {
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .ok_or_else(|| Error::Io(format!("{} has no name", path.display())))?;
            Ok(Bench { name, path: path.clone() })
        })
        .collect()
}

/// Compiles every program twice and lays out the directory the runner is pointed at.
fn build(benches: &[Bench], level: &str, extra: &[String]) -> Result<PathBuf> {
    let work = root().join("target").join("cost");
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let rucc = compiler()?;
    let archive = staticlib("rucc-safe-rt", TRIPLE)?;
    std::fs::copy(&archive, work.join("safe-rt.a"))
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", archive.display())))?;

    for bench in benches {
        for (suffix, tier) in [("off", "-fsafety=off"), ("on", "-fsafety=detect")] {
            let out = Command::new(&rucc)
                .args(["-S", &format!("--target={TRIPLE}"), tier, level])
                .args(extra)
                .arg("-o")
                .arg(work.join(format!("{}.{suffix}.s", bench.name)))
                .arg(&bench.path)
                .current_dir(root())
                .output()
                .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
            if !out.status.success() {
                return Err(Error::Io(format!(
                    "{}: did not compile with {tier}\n{}",
                    bench.name,
                    String::from_utf8_lossy(&out.stderr).trim_end()
                )));
            }
        }
    }

    std::fs::write(work.join("run.sh"), script())
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// Builds the compiler this tree describes and gives back the path to it.
pub(crate) fn compiler() -> Result<PathBuf> {
    let status = Command::new("cargo")
        .args(["build", "-q", "--release", "-p", "rucc"])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !status.success() {
        return Err(Error::Io("the compiler did not build".to_owned()));
    }
    Ok(root().join("target").join("release").join("rucc"))
}

/// The script that links each build and times it.
///
/// The runs are interleaved rather than grouped, so that a machine that gets slower halfway
/// through slows both sides of every ratio instead of one side of half of them. Everything it
/// writes goes under `/tmp`, which lets the work directory be mounted read only.
fn script() -> String {
    let mut sh = String::new();
    sh.push_str("#!/bin/sh\nexec 2>/dev/null\nout=/tmp/cost\nmkdir -p \"$out\"\n");
    sh.push_str("for source in *.s; do\n");
    sh.push_str("    name=${source%.s}\n");
    sh.push_str("    gcc -no-pie \"$source\" safe-rt.a -o \"$out/$name\" || exit 1\n");
    sh.push_str("done\n");
    let _ = write!(sh, "round=0\nwhile [ $round -lt {} ]; do\n", RUNS + WARMUPS);
    sh.push_str("    round=$((round + 1))\n");
    sh.push_str("    for source in *.s; do\n");
    sh.push_str("        name=${source%.s}\n");
    sh.push_str("        start=$(date +%s%N)\n");
    sh.push_str("        \"$out/$name\" || exit 1\n");
    sh.push_str("        end=$(date +%s%N)\n");
    sh.push_str(
        "        printf '<<<time %s %s %s>>>\\n' \"$name\" \"$round\" \"$((end - start))\"\n",
    );
    sh.push_str("    done\n");
    sh.push_str("done\n");
    sh
}

/// Reads the timings back out, dropping the warmups.
fn read(text: &str) -> BTreeMap<String, Times> {
    let mut times: BTreeMap<String, Times> = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("<<<time ") else { continue };
        let Some(rest) = rest.strip_suffix(">>>") else { continue };
        let mut fields = rest.split_whitespace();
        let (Some(name), Some(round), Some(nanos)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(round), Ok(nanos)) = (round.parse::<usize>(), nanos.parse::<u64>()) else {
            continue;
        };
        if round <= WARMUPS {
            continue;
        }
        times.entry(name.to_owned()).or_default().runs.push(nanos);
    }
    times
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warmups_are_not_part_of_the_measurement() {
        // Otherwise the first run of a program on a cold page cache lands in the median, and the
        // number moves depending on what else the machine did beforehand.
        let mut text = String::new();
        for round in 1..=WARMUPS {
            let _ = writeln!(text, "<<<time a.off {round} 1000000000>>>");
        }
        for round in WARMUPS + 1..=WARMUPS + RUNS {
            let _ = writeln!(text, "<<<time a.off {round} 5>>>");
        }
        let times = read(&text);
        let seen = times.get("a.off").expect("a program");
        assert_eq!(seen.runs.len(), RUNS);
        assert!((seen.median() - 5.0).abs() < f64::EPSILON, "{}", seen.median());
    }

    #[test]
    fn the_spread_is_the_range_the_middle_half_falls_in() {
        // A median on its own says nothing about whether two of them differ for a reason, which
        // is what `spec/16-performance.md` section 16.5 asks the interquartile range to answer.
        let steady = Times { runs: vec![100, 101, 102, 103, 104, 105, 106, 107] };
        let jumpy = Times { runs: vec![10, 20, 100, 103, 104, 180, 300, 900] };
        assert!(steady.spread() < jumpy.spread());
    }

    #[test]
    fn the_spread_does_not_depend_on_the_order_the_runs_arrived_in() {
        // The runs come in the order the machine produced them, and a slow first round followed by
        // a fast last one used to put the upper quartile below the lower one and print a negative
        // range.
        let rising = Times { runs: vec![10, 20, 30, 40, 50, 60, 70, 80] };
        let falling = Times { runs: vec![80, 70, 60, 50, 40, 30, 20, 10] };
        assert!(rising.spread() > 0.0, "{}", rising.spread());
        assert!((rising.spread() - falling.spread()).abs() < f64::EPSILON);
    }

    #[test]
    fn no_level_is_the_unoptimized_baseline() {
        // Which is S1's published number, so a caller who asks for nothing has to keep getting it.
        assert_eq!(level(&[]).expect("no argument is fine"), "-O0");
    }

    #[test]
    fn the_level_asked_for_is_the_level_used() {
        for &known in LEVELS {
            let asked = vec![known.to_owned()];
            assert_eq!(level(&asked).expect("a level on the list"), known);
        }
    }

    #[test]
    fn a_level_that_is_not_on_the_list_is_refused() {
        // Rather than passed through to the compiler, which would accept it and hand back a table
        // of ratios that answer a question nobody asked.
        let asked = vec!["-O3".to_owned()];
        let e = level(&asked).expect_err("not a level this runs at");
        assert!(format!("{e}").contains("-O3"), "{e}");
    }

    #[test]
    fn two_different_levels_are_refused() {
        // Both sides of a ratio get the same level, so there is nothing sensible to do with two.
        let asked = vec!["-O0".to_owned(), "-O2".to_owned()];
        assert!(level(&asked).is_err());
        // The same one twice says nothing contradictory, so it is allowed.
        let twice = vec!["-O2".to_owned(), "-O2".to_owned()];
        assert_eq!(level(&twice).expect("the same level twice"), "-O2");
    }

    #[test]
    fn a_flag_travels_past_the_level_and_lands_on_both_sides() {
        let asked = vec!["-O2".to_owned(), "-fno-hoist".to_owned(), "-fno-split".to_owned()];
        assert_eq!(level(&asked).expect("a level with flags beside it"), "-O2");
        assert_eq!(extra(&asked), ["-fno-hoist", "-fno-split"]);
        // And a run that names no flags builds what it always built.
        assert!(extra(&["-O2".to_owned()]).is_empty());
    }

    #[test]
    fn a_line_that_is_not_a_timing_is_ignored() {
        // The shell writes other things, and a linker warning in the middle of the output should
        // not become a benchmark called `ld:`.
        let times = read("ld: warning\n<<<time a.on 4 7>>>\nAborted\n");
        assert_eq!(times.len(), 1);
        assert!(times.contains_key("a.on"));
    }
}
