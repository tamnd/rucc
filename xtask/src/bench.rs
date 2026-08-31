//! The throughput benchmark.
//!
//! Design: `spec/16-performance.md` sections 16.2 and 16.6.
//!
//! Section 16.5 is the part that decided the shape of this. A benchmark number without its
//! methodology is marketing, so a run here reports the median of ten timings with the
//! interquartile range next to it, never a single number and never a mean. The IQR is what
//! says whether a five percent difference between two commits means anything, and a mean over
//! ten runs on a machine that is also doing something else is a number that quietly moves
//! whenever the something else does.
//!
//! The workload is the floor: a file that includes three headers and defines `main`. Nothing
//! about it is clever, and that is the point. On a real build the floor is a surprisingly
//! large fraction of the total, because most translation units are small and most of what a
//! compiler does to them is read the same headers again. It is also the only workload the
//! compiler can run today, since there is no code generator yet, so what is measured is
//! preprocessing and nothing else and the report says so.
//!
//! The reference compiler is timed on the same file in the same loop, because an absolute
//! millisecond count means nothing across machines and a ratio means something on all of
//! them. It also keeps the benchmark honest in the direction that matters: it is easy to be
//! fast at preprocessing when you skip work the other compiler is doing, and the differential
//! in `tamnd/rucc-compat` is what says we are not.
//!
//! Where the headers come from is the one piece of setup worth explaining. rucc has no
//! built-in system include directories, deliberately, so the benchmark asks the reference
//! compiler where it looks and passes the answer through. That is the same thing the
//! differential harness does, and for the same reason: two compilers reading different
//! headers are not running the same workload, and a throughput win that came from reading a
//! smaller `stdio.h` is not a throughput win.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Error, Result, root};

/// How many timings are taken, and how many are thrown away first.
///
/// Ten is what section 16.2 asks for. Three warmups is enough to get the file into the page
/// cache and the branch predictors into steady state; the first run of anything on a cold
/// cache is a different measurement and mixing the two is how a benchmark ends up with an
/// interquartile range wider than the effect it is trying to detect.
const RUNS: usize = 10;
const WARMUPS: usize = 3;

/// The source of the floor workload.
///
/// Three headers and a `main` that does nothing. `stdio.h` pulls in most of what a C program
/// touches, and on glibc the three of them together are around thirty thousand lines after
/// preprocessing, essentially all of which is work no program asked for.
const FLOOR: &str = "\
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    return 0;
}
";

/// Runs the benchmark and prints the report.
pub(crate) fn bench(args: &[String]) -> Result<()> {
    let mut runs = RUNS;
    let mut reference = Some("cc".to_owned());
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
            "--against" => {
                let value = rest.next().ok_or_else(|| bad("--against needs a compiler"))?;
                reference = if value == "none" { None } else { Some(value.clone()) };
            }
            "--csv" => csv = true,
            other => return Err(bad(format!("unknown option `{other}`"))),
        }
    }

    let rucc = root().join("target/release/rucc");
    if !rucc.exists() {
        return Err(bad(format!(
            "{} does not exist, run `cargo build --release -p rucc` first",
            rucc.display()
        )));
    }
    let dir = std::env::temp_dir().join("rucc-bench");
    std::fs::create_dir_all(&dir)?;
    let source = dir.join("floor.c");
    std::fs::write(&source, FLOOR)?;

    // The include directories come from the reference compiler, or from the one named by
    // `--against` when that is a different compiler. With `--against none` there is nowhere
    // to ask, so the run is refused rather than measuring a compiler that fails instantly.
    let asked = reference.clone().unwrap_or_else(|| "cc".to_owned());
    let includes = system_includes(Path::new(&asked));
    if includes.is_empty() {
        return Err(bad(format!(
            "`{asked}` did not say where it looks for headers, so there is no workload to run"
        )));
    }

    let rss = RssProbe::detect();
    let mut results = Vec::new();
    let mut ours: Vec<String> = includes.clone();
    ours.extend(["-E".to_owned(), "-P".to_owned(), source.display().to_string()]);
    results.push(measure("rucc", &rucc, &ours, runs, &rss)?);
    if let Some(reference) = &reference {
        let theirs = vec!["-E".to_owned(), "-P".to_owned(), source.display().to_string()];
        results.push(measure(reference, Path::new(reference), &theirs, runs, &rss)?);
    }

    if csv {
        print_csv(&results);
    } else {
        print_report(&results, runs, &rss);
    }
    Ok(())
}

/// One workload run against one compiler.
struct Measured {
    name: String,
    times: Stats,
    /// Peak resident set size in bytes, when the platform would say.
    peak_rss: Option<u64>,
    /// How many bytes of preprocessed output came out.
    ///
    /// Reported next to the time on purpose. It is easy to be fast at preprocessing by not
    /// doing some of it, and a throughput ratio between two compilers that produced different
    /// amounts of output is not a throughput ratio. This does not replace the differential in
    /// `tamnd/rucc-compat`, which is what actually checks the output; it is the cheap signal
    /// that sits in the same report as the number it qualifies.
    output_bytes: u64,
    /// How many non-empty lines of preprocessed output came out.
    ///
    /// This, and not the byte count, is what decides whether the two compilers did the same
    /// work, because the byte count is not portable across vendors. On the macOS SDK the
    /// reference is clang and the headers are full of `__has_feature` queries that only clang
    /// answers yes to, so clang's declarations carry `_Nullable`, availability attributes and
    /// the long deprecation messages while rucc's carry none of them. That is thirteen
    /// kilobytes of attribute text on a forty six kilobyte output, and every declaration is
    /// present in both. Counting lines sees through it: a compiler that dropped a whole header
    /// loses hundreds of lines, and a compiler that spells attributes differently loses none.
    output_lines: u64,
}

/// Times one compiler `runs` times and summarises.
fn measure(
    name: &str,
    program: &Path,
    args: &[String],
    runs: usize,
    rss: &RssProbe,
) -> Result<Measured> {
    let run_once = || -> Result<Duration> {
        let started = Instant::now();
        let status = Command::new(program)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| bad(format!("could not run `{}`: {e}", program.display())))?;
        let elapsed = started.elapsed();
        if !status.success() {
            return Err(bad(format!(
                "`{}` failed on the workload, so there is nothing to time",
                program.display()
            )));
        }
        Ok(elapsed)
    };
    for _ in 0..WARMUPS {
        run_once()?;
    }
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        times.push(run_once()?.as_secs_f64() * 1000.0);
    }
    let output = Command::new(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .map_err(|e| bad(format!("could not run `{}`: {e}", program.display())))?;
    Ok(Measured {
        name: name.to_owned(),
        times: Stats::of(&mut times),
        peak_rss: rss.of(program, args),
        output_bytes: output.stdout.len() as u64,
        output_lines: nonblank_lines(&output.stdout),
    })
}

/// Whether two counts are more than a tenth of the larger one apart.
fn apart(a: u64, b: u64) -> bool {
    let (small, large) = if a < b { (a, b) } else { (b, a) };
    large > 0 && (large - small) * 10 > large
}

/// Counts the lines of `out` that hold something other than whitespace.
///
/// Blank lines are skipped because `-P` is not precise about them: clang emits a few and rucc
/// emits none, which is a difference worth nothing and worth not tripping over. Works on bytes
/// rather than text, since preprocessed output carries whatever the headers carried and is not
/// promised to be UTF-8.
fn nonblank_lines(out: &[u8]) -> u64 {
    out.split(|&b| b == b'\n').filter(|line| line.iter().any(|b| !b.is_ascii_whitespace())).count()
        as u64
}

/// The five numbers a timing distribution is reported as.
struct Stats {
    min: f64,
    q1: f64,
    median: f64,
    q3: f64,
    max: f64,
}

impl Stats {
    /// Sorts in place and takes the quartiles.
    ///
    /// Linear interpolation between the two neighbouring order statistics, which is what R and
    /// numpy do by default. Any of the nine definitions of a quartile would do here as long as
    /// it is written down, and this one is written down.
    fn of(values: &mut [f64]) -> Stats {
        values.sort_by(f64::total_cmp);
        Stats {
            min: values[0],
            q1: quantile(values, 0.25),
            median: quantile(values, 0.5),
            q3: quantile(values, 0.75),
            max: values[values.len() - 1],
        }
    }

    /// The interquartile range, which is the number that says whether a difference is real.
    fn iqr(&self) -> f64 {
        self.q3 - self.q1
    }
}

fn quantile(sorted: &[f64], p: f64) -> f64 {
    let at = (sorted.len() - 1) as f64 * p;
    let below = at.floor() as usize;
    let above = at.ceil() as usize;
    if below == above {
        return sorted[below];
    }
    sorted[below] + (sorted[above] - sorted[below]) * (at - below as f64)
}

/// Prints the human readable report.
fn print_report(results: &[Measured], runs: usize, rss: &RssProbe) {
    println!("floor: three system headers and an empty main, preprocess only");
    println!("{runs} runs after {WARMUPS} warmups, times in milliseconds");
    println!();
    for r in results {
        let t = &r.times;
        print!(
            "  {:<8} median {:>7.1}   IQR {:>7.1} to {:>7.1} ({:>5.1})   min {:>7.1}   max {:>7.1}",
            r.name,
            t.median,
            t.q1,
            t.q3,
            t.iqr(),
            t.min,
            t.max
        );
        match r.peak_rss {
            Some(bytes) => print!("   peak RSS {:>6.1} MB", bytes as f64 / (1024.0 * 1024.0)),
            None => print!("   peak RSS      ?   "),
        }
        println!(
            "   output {:>6.0} KB in {:>6} lines",
            r.output_bytes as f64 / 1024.0,
            r.output_lines
        );
    }
    if let ([ours], [theirs]) = (&results[..1], &results[1..]) {
        // The ratio is of the medians, because that is the statistic being reported. A ratio
        // of the minimums would flatter whichever compiler got the luckiest single run.
        let ratio = ours.times.median / theirs.times.median;
        println!();
        println!("  rucc is {ratio:.2}x the time of {} on this workload", theirs.name);
        // Two distributions whose interquartile ranges overlap have not been told apart by
        // this many runs, whatever the medians say. Saying so is cheaper than someone quoting
        // the ratio in a release note it does not support.
        if ours.times.q3 >= theirs.times.q1 && theirs.times.q3 >= ours.times.q1 {
            println!("  the two interquartile ranges overlap, so this run does not separate them");
        }
        // A tenth of the lines is well outside anything the known differences account for. If
        // the outputs are that far apart the ratio above is comparing two different amounts of
        // work, and the report has to say so in the same breath rather than leave the number
        // standing. The byte counts get a line of their own because on a cross vendor pairing
        // they run apart for a reason that is not skipped work, and a reader who sees the two
        // sizes side by side deserves to be told which of the two means anything.
        if apart(ours.output_lines, theirs.output_lines) {
            println!("  the outputs differ by more than a tenth of their lines, so the two are");
            println!("  not doing the same work and the ratio above does not mean anything");
        } else if apart(ours.output_bytes, theirs.output_bytes) {
            println!("  the outputs agree on lines but differ by more than a tenth in bytes,");
            println!("  which is what two compilers answering `__has_feature` differently to");
            println!("  the same headers looks like, and not a difference in work done");
        }
    }
    if rss.is_unavailable() {
        println!();
        println!("  peak RSS was not measured: no `/usr/bin/time` on this platform");
    }
}

/// Prints one row per metric, which is what section 16.6 asks a nightly to write.
fn print_csv(results: &[Measured]) {
    let commit = commit();
    let host = host();
    println!("commit,host,suite,benchmark,compiler,metric,value,iqr");
    for r in results {
        println!(
            "{commit},{host},throughput,floor,{},wall_ms,{:.3},{:.3}",
            r.name,
            r.times.median,
            r.times.iqr()
        );
        if let Some(bytes) = r.peak_rss {
            println!("{commit},{host},throughput,floor,{},peak_rss_bytes,{bytes},", r.name);
        }
        println!("{commit},{host},throughput,floor,{},output_bytes,{},", r.name, r.output_bytes);
        println!("{commit},{host},throughput,floor,{},output_lines,{},", r.name, r.output_lines);
    }
}

/// The commit the numbers belong to, or `unknown` outside a checkout.
fn commit() -> String {
    let out = Command::new("git").args(["rev-parse", "HEAD"]).output();
    match out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        _ => "unknown".to_owned(),
    }
}

/// The machine the numbers belong to. A benchmark row without one is not comparable to
/// anything, which is the failure section 16.5 is about.
fn host() -> String {
    let out = Command::new("uname").args(["-sm"]).output();
    match out {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().replace(' ', "-")
        }
        _ => "unknown".to_owned(),
    }
}

/// Where the reference compiler looks for headers, as `-isystem` flags.
///
/// Parsed out of `-E -v`, between the two lines GCC and clang both print around the list.
fn system_includes(cc: &Path) -> Vec<String> {
    let Ok(out) = Command::new(cc).args(["-E", "-v", "-x", "c", "/dev/null"]).output() else {
        return Vec::new();
    };
    search_list(&String::from_utf8_lossy(&out.stderr))
}

/// The `-isystem` flags named by a `-E -v` transcript.
fn search_list(text: &str) -> Vec<String> {
    let mut flags = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("#include <...> search starts here:") {
            inside = true;
            continue;
        }
        if line.starts_with("End of search list.") {
            break;
        }
        if inside {
            // clang writes ` /path (framework directory)` for the framework entries, which are
            // not include directories and must not be passed as ones.
            let entry = line.trim();
            if entry.is_empty() || entry.ends_with(')') {
                continue;
            }
            flags.push("-isystem".to_owned());
            flags.push(entry.to_owned());
        }
    }
    flags
}

/// How, if at all, this platform will report a child's peak memory.
///
/// `/usr/bin/time` is the only way to ask that does not need a dependency, and the two
/// implementations of it spell the flag and the answer differently. Probing once is cheaper
/// than guessing from the operating system name, and it gets the case where somebody has GNU
/// coreutils installed on a Mac right for free.
enum RssProbe {
    /// BSD `time -l`, which prints bytes.
    Bsd,
    /// GNU `time -v`, which prints kilobytes.
    Gnu,
    /// Neither works, so peak memory is not reported rather than guessed at.
    None,
}

impl RssProbe {
    fn detect() -> RssProbe {
        for (flag, probe) in [("-l", RssProbe::Bsd), ("-v", RssProbe::Gnu)] {
            let Ok(out) = Command::new("/usr/bin/time").args([flag, "true"]).output() else {
                continue;
            };
            if out.status.success() && probe.parse(&String::from_utf8_lossy(&out.stderr)).is_some()
            {
                return probe;
            }
        }
        RssProbe::None
    }

    fn is_unavailable(&self) -> bool {
        matches!(self, RssProbe::None)
    }

    /// Runs the workload once more under `time` and reads the peak off it.
    ///
    /// A separate run from the timed ones on purpose. `time` adds a process to the
    /// measurement, and a memory number does not need ten samples the way a time does: peak
    /// RSS on the same input is close to deterministic, which is exactly why it is worth
    /// reporting as a single number and time is not.
    fn of(&self, program: &Path, args: &[String]) -> Option<u64> {
        if self.is_unavailable() {
            return None;
        }
        let flag = match self {
            RssProbe::Bsd => "-l",
            RssProbe::Gnu => "-v",
            RssProbe::None => return None,
        };
        let out = Command::new("/usr/bin/time")
            .arg(flag)
            .arg(program)
            .args(args)
            .stdout(Stdio::null())
            .output()
            .ok()?;
        self.parse(&String::from_utf8_lossy(&out.stderr))
    }

    fn parse(&self, text: &str) -> Option<u64> {
        match self {
            RssProbe::Bsd => text.lines().find_map(|line| {
                let line = line.trim();
                let rest = line.strip_suffix("maximum resident set size")?;
                rest.trim().parse::<u64>().ok()
            }),
            RssProbe::Gnu => text.lines().find_map(|line| {
                let rest = line.trim().strip_prefix("Maximum resident set size (kbytes):")?;
                rest.trim().parse::<u64>().ok().map(|kb| kb * 1024)
            }),
            RssProbe::None => None,
        }
    }
}

fn bad(message: impl Into<String>) -> Error {
    Error::Io(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quartiles_interpolate_between_neighbours() {
        // The definition R and numpy use, written down here so that a change to it is a change
        // somebody has to make on purpose. Ten points from 1 to 10: the median is between the
        // fifth and the sixth, and the quartiles land a quarter of the way along.
        let mut ten: Vec<f64> = (1..=10).map(f64::from).collect();
        let stats = Stats::of(&mut ten);
        assert!((stats.median - 5.5).abs() < 1e-9);
        assert!((stats.q1 - 3.25).abs() < 1e-9);
        assert!((stats.q3 - 7.75).abs() < 1e-9);
        assert!((stats.iqr() - 4.5).abs() < 1e-9);
        assert!((stats.min - 1.0).abs() < 1e-9);
        assert!((stats.max - 10.0).abs() < 1e-9);
    }

    #[test]
    fn an_unsorted_input_is_sorted_first() {
        // The caller hands over timings in the order they were taken, and every statistic here
        // is an order statistic.
        let mut jumbled = vec![9.0, 1.0, 5.0, 3.0, 7.0];
        let stats = Stats::of(&mut jumbled);
        assert!((stats.median - 5.0).abs() < 1e-9);
        assert!((stats.min - 1.0).abs() < 1e-9);
        assert!((stats.max - 9.0).abs() < 1e-9);
    }

    #[test]
    fn a_peak_is_read_from_either_spelling_of_time() {
        // BSD prints bytes with the label after the number, GNU prints kilobytes with the
        // label before it, and getting the units wrong is a factor of a thousand in a report
        // that is meant to be about whether we use too much memory.
        let bsd = "        1.00 real         0.50 user\n     12582912  maximum resident set size\n";
        assert_eq!(RssProbe::Bsd.parse(bsd), Some(12_582_912));
        let gnu = "\tMaximum resident set size (kbytes): 12288\n\tMinor page faults: 3\n";
        assert_eq!(RssProbe::Gnu.parse(gnu), Some(12_288 * 1024));
        assert_eq!(RssProbe::None.parse(gnu), None);
    }

    #[test]
    fn framework_directories_are_not_include_directories() {
        // Apple's clang lists them in the same block, and passing one as `-isystem` gives a
        // directory with no headers in it and a workload that is not the one intended.
        // Anything after the end of the list is not part of it either, which is where the
        // compiler prints the command it would have run.
        let text = "\
ignore me
#include <...> search starts here:
 /usr/local/include
 /Library/Frameworks (framework directory)
 /usr/include
End of search list.
 /not/a/directory
";
        assert_eq!(
            search_list(text),
            vec![
                "-isystem".to_owned(),
                "/usr/local/include".to_owned(),
                "-isystem".to_owned(),
                "/usr/include".to_owned(),
            ]
        );
        // A transcript with no list in it produces no flags rather than a partial guess, and
        // the caller refuses to run instead of benchmarking a compiler that fails instantly.
        assert!(search_list("nothing here").is_empty());
    }

    #[test]
    fn attribute_text_moves_the_bytes_without_moving_the_lines() {
        // The shape the macOS SDK produces: same declarations, one of them carrying an
        // attribute the other compiler does not know how to ask for. The line counts have to
        // stay together or the report accuses the two of doing different work.
        let ours = b"int a(void);\nint b(void);\n";
        let theirs =
            b"int a(void) __attribute__((availability(macosx,introduced=10.10)));\nint b(void);\n";
        assert_eq!(nonblank_lines(ours), nonblank_lines(theirs));
        assert!(apart(ours.len() as u64, theirs.len() as u64));
        assert!(!apart(nonblank_lines(ours), nonblank_lines(theirs)));
    }

    #[test]
    fn a_dropped_header_moves_the_lines() {
        // The shape the check exists for. Whatever the attributes look like, output that is
        // missing a header is missing its lines, and that is the case worth shouting about.
        let full: Vec<u8> = "int f(void);\n".repeat(100).into_bytes();
        let short: Vec<u8> = "int f(void);\n".repeat(50).into_bytes();
        assert!(apart(nonblank_lines(&full), nonblank_lines(&short)));
    }

    #[test]
    fn blank_lines_are_not_counted_either_way() {
        // `-P` leaves clang emitting a few blank lines and rucc emitting none. Neither is
        // output and neither should register as a difference.
        assert_eq!(nonblank_lines(b"a\n\n\nb\n"), 2);
        assert_eq!(nonblank_lines(b"a\nb\n"), 2);
        assert_eq!(nonblank_lines(b"  \n\t\n"), 0);
        // No trailing newline still counts the last line.
        assert_eq!(nonblank_lines(b"a\nb"), 2);
    }
}
