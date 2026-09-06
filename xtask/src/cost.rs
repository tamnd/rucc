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
//! # Why the baseline is `-O0` rather than `-O2`
//!
//! Section 13.2 says the baseline for an overhead number is `rucc -O2` with safety off. That is the
//! right baseline for a claim about a tier's budget and this is not one. S1 has no check
//! elimination in it at all, deliberately, and the milestone calls its own number the unoptimized
//! baseline for exactly that reason. Both sides are `-O0` here, which isolates the monitor from the
//! optimizer, and the `-O2` comparison is S4's, where the interesting question is how much of this
//! the rules take back.
//!
//! # Why the emulated run is not a data point
//!
//! On a machine that is not x86-64 Linux the programs run in a container under emulation, which
//! changes the ratio between an instruction and a cache miss, which is the ratio this whole
//! document is about. That run is useful for checking the apparatus works and is worthless as a
//! measurement, so it says so in its own output rather than leaving somebody to notice.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result, root, staticlib};

/// The machine the programs are compiled for.
const TRIPLE: &str = "x86_64-unknown-linux-gnu";

/// The image they run in when this machine is not that one.
const IMAGE: &str = "gcc:13";

/// The optimization level, on both sides.
const LEVEL: &str = "-O0";

/// How many timings are taken and how many are thrown away first.
///
/// The same ten and three as `spec/16-performance.md` section 16.2, for the same reasons: ten is
/// enough for a median with quartiles either side of it, and the first runs of anything are a
/// measurement of a cold page cache rather than of the program.
const RUNS: usize = 10;
const WARMUPS: usize = 3;

/// One program, built both ways.
#[derive(Debug)]
struct Bench {
    /// The file name without its extension.
    name: String,
    /// The file itself.
    path: PathBuf,
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
pub(crate) fn cost() -> Result<()> {
    let benches = benches()?;
    let runner = Runner::find()?;
    println!("cost: {} programs, {LEVEL} both sides, {runner}", benches.len());

    let work = build(&benches)?;
    let times = runner.run(&work)?;

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

    report(&rows, &runner);
    Ok(())
}

/// Prints the table and the two summary numbers section 13.4 asks to see together.
fn report(rows: &[(String, f64, f64, f64, f64)], runner: &Runner) {
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
    println!("cost: {geomean:.2}x geomean, {:.2}x worst case, which is {}", worst.1, worst.0);
    println!(
        "cost: wall clock only. Section 13.1 also asks for cache misses, memory traffic, peak \
         RSS, branch mispredictions and spill counts, and none of those are readable here."
    );
    if matches!(runner, Runner::Container) {
        println!(
            "cost: this ran under emulation, which moves the cost of a cache miss relative to the \
             cost of an instruction, and that ratio is what the number is about. Not a data point."
        );
    }
}

/// Every program on disk, in the order a directory listing gives them.
fn benches() -> Result<Vec<Bench>> {
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
fn build(benches: &[Bench]) -> Result<PathBuf> {
    let work = root().join("target").join("cost");
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

    for bench in benches {
        for (suffix, tier) in [("off", "-fsafety=off"), ("on", "-fsafety=detect")] {
            let out = Command::new(&rucc)
                .args(["-S", &format!("--target={TRIPLE}"), tier, LEVEL, "-o"])
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

/// Where the programs are run.
#[derive(Debug)]
enum Runner {
    /// Straight, because this machine is the machine they are compiled for.
    Here,
    /// In a container, because it is not, which makes the numbers useless and the run still worth
    /// being able to do.
    Container,
}

impl std::fmt::Display for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Here => f.write_str("run on this machine"),
            Self::Container => f.write_str("run in a container, so not a data point"),
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
            "these programs are {TRIPLE} ones and this machine is {host}, so they need a \
             container to run in and docker is not answering. Start it, or measure on an x86-64 \
             Linux machine, which is the only place the number means anything anyway."
        )))
    }

    /// Runs the script over the directory and reads the timings back.
    fn run(&self, work: &Path) -> Result<BTreeMap<String, Times>> {
        let out = match self {
            Self::Here => Command::new("sh").arg("run.sh").current_dir(work).output(),
            Self::Container => Command::new("docker")
                .args(["run", "--rm", "--platform", "linux/amd64", "-v"])
                .arg(format!("{}:/w:ro", work.display()))
                .args(["-w", "/w", IMAGE, "sh", "run.sh"])
                .output(),
        }
        .map_err(|e| Error::Io(format!("could not run the benchmarks: {e}")))?;
        if !out.status.success() {
            return Err(Error::Io(format!(
                "the benchmarks did not run: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(read(&String::from_utf8_lossy(&out.stdout)))
    }
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
    fn a_line_that_is_not_a_timing_is_ignored() {
        // The shell writes other things, and a linker warning in the middle of the output should
        // not become a benchmark called `ld:`.
        let times = read("ld: warning\n<<<time a.on 4 7>>>\nAborted\n");
        assert_eq!(times.len(), 1);
        assert!(times.contains_key("a.on"));
    }
}
