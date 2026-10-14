//! The compile-time budget of `spec/optimizer/42-measurement.md` section 42.7, measured.
//!
//! Design: tamnd/rucc#2972. Spec 16.4 holds `-O2` to one and a half times `clang -O2`, and section
//! 42.7 divides that time between seven phases with a share each. This task compiles a set of C
//! files with both compilers at `-O2`, sets the two times against each other, and divides rucc's
//! time between the phases out of what `-frucc-trace` wrote for each file. A phase over its share is
//! printed with the passes in it that took the most, which is the finding the section asks for.
//!
//! The files are the corpus at its pin unless `--programs` names a directory, and `--sqlite` adds
//! the SQLite amalgamation on its own line, because one translation unit of a quarter of a million
//! lines is a different question from four thousand small ones.
//!
//! The ratio is CPU time, the children's user and system time from `/proc/self/stat`, because the
//! machines this runs on are shared and wall time on a loaded machine measures the load. The phases
//! are what the trace says, which is wall time inside one process, so on a loaded machine the shares
//! are better than their absolute values. A file either compiler refuses is left out of both
//! totals, and the run says how many there were.
//!
//! The shares are read out of the table in section 42.7 rather than written down a second time here,
//! for the reason `size` gives about its own table.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Error, Result, corpus, libraries, root, target_dir};

/// What the trace calls each phase, in the order the table in section 42.7 lists them.
const PHASES: [&str; 7] =
    ["front", "analyses", "rewriting", "scalar", "selection", "allocation", "machine"];

/// How many passes to name for a phase that is over its share.
const NAMED: usize = 5;

/// What spec 16.4 allows `-O2` against `clang -O2`.
const ALLOWED: f64 = 1.5;

/// Measures the budget and prints it.
pub(crate) fn budget(args: &[String]) -> Result<()> {
    let mut programs: Option<PathBuf> = None;
    let mut sqlite = false;
    let mut clang = "clang".to_owned();
    let mut build = true;
    let mut repeats = 1;
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--programs" => {
                let value =
                    args.get(at + 1).ok_or_else(|| usage("--programs wants a directory"))?;
                programs = Some(PathBuf::from(value));
                at += 2;
            }
            "--sqlite" => {
                sqlite = true;
                at += 1;
            }
            "--clang" => {
                let value = args.get(at + 1).ok_or_else(|| usage("--clang wants a program"))?;
                clang = value.clone();
                at += 2;
            }
            "--no-build" => {
                build = false;
                at += 1;
            }
            "--repeats" => {
                let value = args.get(at + 1).ok_or_else(|| usage("--repeats wants a number"))?;
                repeats = value.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                    usage(&format!("--repeats wants a number above zero, not `{value}`"))
                })?;
                at += 2;
            }
            other => return Err(usage(&format!("`{other}` is not an option this task has"))),
        }
    }

    let shares = shares(&fs::read_to_string(root().join("spec/optimizer/42-measurement.md"))?)?;
    if !corpus::runnable(&clang) {
        return Err(Error::Io(format!("the budget is set against {clang}, and it is not here")));
    }
    let rucc = target_dir().join("release").join("rucc");
    if build {
        println!("xtask: building rucc");
        corpus::cargo(&root(), &["build", "--release", "-p", "rucc"])?;
    }
    if !rucc.exists() {
        return Err(Error::Io(format!(
            "{} does not exist, so there is nothing to time",
            rucc.display()
        )));
    }
    let (programs, named) = match programs {
        Some(dir) => {
            let named = dir.display().to_string();
            (dir, named)
        }
        None => {
            let (dir, rev) = corpus::pinned()?;
            (dir.join("programs"), format!("the corpus at {}", &rev[..12.min(rev.len())]))
        }
    };

    println!("xtask: {}", machine());
    println!("xtask: {}", first_line(&rucc.display().to_string()));
    println!("xtask: {}", first_line(&clang));
    if !cpu_time_is_there() {
        println!("xtask: no /proc/self/stat here, so the ratio is wall time and not CPU time");
    }
    let work = target_dir().join("budget");
    fs::create_dir_all(&work)?;

    let mut files = Vec::new();
    sources(&programs, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err(Error::Io(format!("{} holds no C files", programs.display())));
    }
    let measured = measure(&rucc, &clang, &files, repeats, &work)?;
    print!("{}", report(&named, &measured, &shares));

    if sqlite {
        let dir = libraries::found(&libraries::PROJECTS[0]).ok_or_else(|| {
            Error::Io(
                "no SQLite amalgamation found, set RUCC_SQLITE_AMALGAMATION to the directory \
                 holding sqlite3.c"
                    .to_owned(),
            )
        })?;
        let measured = measure(&rucc, &clang, &[dir.join("sqlite3.c")], repeats, &work)?;
        print!("{}", report("the SQLite amalgamation", &measured, &shares));
    }
    Ok(())
}

/// What one set of files came to.
#[derive(Debug, Default)]
struct Measured {
    /// Files both compilers compiled.
    files: usize,
    /// Files one of them refused, which are in neither total.
    refused: usize,
    /// The CPU time each compiler took over the files, the least of the repeats.
    rucc: Duration,
    clang: Duration,
    /// The trace's seven phases added up over every file and every repeat, in the order of
    /// [`PHASES`].
    phases: [Duration; 7],
    /// Each pass's time and the phase it is in, added up the same way.
    passes: Vec<(String, String, Duration)>,
    /// The driver's laps that are the front end, added up the same way.
    front: Vec<(String, Duration)>,
    /// The machine phase in the two parts the trace divides it into, added up the same way.
    machine: Vec<(String, Duration)>,
}

/// Compiles every file with both compilers, one file at a time and clang first, so that the load
/// on a shared machine falls on the two about evenly.
fn measure(
    rucc: &Path,
    clang: &str,
    files: &[PathBuf],
    repeats: u32,
    work: &Path,
) -> Result<Measured> {
    let trace = work.join("trace.jsonl");
    let object = work.join("out.o");
    let mut measured = Measured::default();
    let mut refused = vec![false; files.len()];
    for repeat in 0..repeats {
        let _ = fs::remove_file(&trace);
        let (mut rucc_time, mut clang_time) = (Duration::ZERO, Duration::ZERO);
        for (index, file) in files.iter().enumerate() {
            if refused[index] {
                continue;
            }
            let (ok, took) =
                timed(Command::new(clang).args(["-O2", "-c", "-w", "-o"]).arg(&object).arg(file));
            if !ok {
                refused[index] = true;
                continue;
            }
            let mut flag = std::ffi::OsString::from("-frucc-trace=");
            flag.push(&trace);
            let (ok, rucc_took) = timed(
                Command::new(rucc)
                    .args(["-O2", "-c", "-w"])
                    .arg(flag)
                    .arg("-o")
                    .arg(&object)
                    .arg(file),
            );
            if !ok {
                refused[index] = true;
                continue;
            }
            clang_time += took;
            rucc_time += rucc_took;
        }
        if repeat == 0 || rucc_time < measured.rucc {
            measured.rucc = rucc_time;
        }
        if repeat == 0 || clang_time < measured.clang {
            measured.clang = clang_time;
        }
        if trace.exists() {
            for line in fs::read_to_string(&trace)?.lines() {
                add(&mut measured, line);
            }
        }
    }
    measured.refused = refused.iter().filter(|it| **it).count();
    measured.files = files.len() - measured.refused;
    // A file refused on a later repeat was counted on the earlier ones. That has never happened,
    // and if it does the trace and the totals disagree by that file, which the count of refusals
    // will show.
    Ok(measured)
}

/// Adds one trace record to the totals, unless it is from a compile that failed.
fn add(measured: &mut Measured, line: &str) {
    if !line.contains("\"ok\":true") {
        return;
    }
    for (name, value) in object(line, "budget") {
        if let Some(at) = PHASES.iter().position(|phase| *phase == name) {
            measured.phases[at] += seconds(&value);
        }
    }
    let phase_of = object(line, "phase-of");
    for (pass, value) in object(line, "passes") {
        let phase = phase_of
            .iter()
            .find(|(it, _)| *it == pass)
            .map_or_else(|| "scalar".to_owned(), |(_, phase)| phase.clone());
        match measured.passes.iter_mut().find(|(it, _, _)| *it == pass) {
            Some((_, _, total)) => *total += seconds(&value),
            None => measured.passes.push((pass, phase, seconds(&value))),
        }
    }
    for (part, value) in object(line, "machine") {
        match measured.machine.iter_mut().find(|(it, _)| *it == part) {
            Some((_, total)) => *total += seconds(&value),
            None => measured.machine.push((part, seconds(&value))),
        }
    }
    for (lap, value) in object(line, "phases") {
        if lap == "optimize" || lap == "generate" {
            continue;
        }
        match measured.front.iter_mut().find(|(it, _)| *it == lap) {
            Some((_, total)) => *total += seconds(&value),
            None => measured.front.push((lap, seconds(&value))),
        }
    }
}

/// What one set of files came to, as the lines to print.
fn report(named: &str, measured: &Measured, shares: &[u32; 7]) -> String {
    let mut out = String::new();
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{named}: {} files, {} left out because a compiler refused them",
        measured.files, measured.refused
    );
    let times = ratio(measured.rucc, measured.clang);
    let _ = writeln!(
        out,
        "  rucc -O2 {:.2} s, clang -O2 {:.2} s, {times:.2}x where spec 16.4 allows {ALLOWED:.2}x{}",
        measured.rucc.as_secs_f64(),
        measured.clang.as_secs_f64(),
        if times > ALLOWED { ", over" } else { "" }
    );
    let total: Duration = measured.phases.iter().sum();
    let _ = writeln!(out, "  {:<12}{:>10}{:>8}{:>8}", "phase", "seconds", "share", "budget");
    let mut over = Vec::new();
    for (at, phase) in PHASES.iter().enumerate() {
        let share = 100.0 * ratio(measured.phases[at], total);
        let budget = f64::from(shares[at]);
        let mark = if share > budget {
            over.push(at);
            "  over"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {phase:<12}{:>10.3}{:>7.1}%{:>7}%{mark}",
            measured.phases[at].as_secs_f64(),
            share,
            shares[at]
        );
    }
    for at in over {
        let phase = PHASES[at];
        let mut named: Vec<(&str, Duration)> = if phase == "front" {
            measured.front.iter().map(|(lap, time)| (lap.as_str(), *time)).collect()
        } else if phase == "machine" {
            measured.machine.iter().map(|(part, time)| (part.as_str(), *time)).collect()
        } else {
            measured
                .passes
                .iter()
                .filter(|(_, it, _)| it == phase)
                .map(|(pass, _, time)| (pass.as_str(), *time))
                .collect()
        };
        if named.is_empty() {
            let _ =
                writeln!(out, "  {phase} is over and the trace has nothing finer to say about it");
            continue;
        }
        named.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let list: Vec<String> = named
            .iter()
            .take(NAMED)
            .map(|(name, time)| format!("{name} {:.3} s", time.as_secs_f64()))
            .collect();
        let _ = writeln!(out, "  {phase} is over, most of it in {}", list.join(", "));
    }
    out
}

/// The shares in the table in section 42.7, in its order, which is the order of [`PHASES`].
fn shares(spec: &str) -> Result<[u32; 7]> {
    let section = spec
        .split("\n## ")
        .find(|it| it.starts_with("42.7 "))
        .ok_or_else(|| Error::Io("spec 42 has no section 42.7".to_owned()))?;
    let mut shares = Vec::new();
    for line in section.lines() {
        let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
        let [_, share] = cells.as_slice() else { continue };
        if let Some(share) = share.strip_suffix('%') {
            if let Ok(share) = share.parse() {
                shares.push(share);
            }
        }
    }
    let shares: [u32; 7] = shares.try_into().map_err(|found: Vec<u32>| {
        Error::Io(format!(
            "the table in section 42.7 has {} shares where it had seven",
            found.len()
        ))
    })?;
    if shares.iter().sum::<u32>() != 100 {
        return Err(Error::Io("the shares in section 42.7 do not add up to 100".to_owned()));
    }
    Ok(shares)
}

/// The flat object under one key of a trace record, as names and the values written after them.
///
/// The trace is written by our own driver and every object this reads is flat, a name to a number
/// or a name to a name, so this reads exactly that much JSON and no more. A name with a quote in it
/// cannot happen, because pass, lap and phase names are ours.
fn object(line: &str, key: &str) -> Vec<(String, String)> {
    let Some(start) = line.find(&format!("\"{key}\":{{")) else {
        return Vec::new();
    };
    let body = &line[start + key.len() + 4..];
    let body = &body[..body.find('}').unwrap_or(body.len())];
    body.split(',')
        .filter_map(|pair| pair.split_once(':'))
        .map(|(name, value)| {
            (name.trim_matches('"').to_owned(), value.trim_matches('"').to_owned())
        })
        .collect()
}

/// Seconds as the trace writes them.
fn seconds(value: &str) -> Duration {
    value
        .parse()
        .ok()
        .filter(|it: &f64| it.is_finite() && *it >= 0.0)
        .map_or(Duration::ZERO, Duration::from_secs_f64)
}

/// One time over another, or zero when there is nothing to divide by.
fn ratio(top: Duration, bottom: Duration) -> f64 {
    if bottom.is_zero() { 0.0 } else { top.as_secs_f64() / bottom.as_secs_f64() }
}

/// Runs a compiler and says whether it worked and the CPU time it took.
fn timed(command: &mut Command) -> (bool, Duration) {
    let before = children();
    let started = Instant::now();
    let ok = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    let took = match (before, children()) {
        (Some(before), Some(after)) => after.saturating_sub(before),
        _ => started.elapsed(),
    };
    (ok, took)
}

/// The user and system time of every child this process has waited for, from `/proc/self/stat`.
///
/// The fields after the name in parentheses, counted from the state as the first, put the
/// children's user time 14th and their system time 15th, in ticks of a hundredth of a second,
/// which is what Linux reports there whatever its own tick is.
fn children() -> Option<Duration> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    let user: u64 = fields.get(13)?.parse().ok()?;
    let system: u64 = fields.get(14)?.parse().ok()?;
    Some(Duration::from_millis((user + system) * 10))
}

/// Whether [`children`] works here.
fn cpu_time_is_there() -> bool {
    children().is_some()
}

/// The processor and how many of them, so that a number posted from a run says where it came from.
fn machine() -> String {
    let model = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .find(|line| line.starts_with("model name"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, model)| model.trim().to_owned())
        })
        .unwrap_or_else(|| std::env::consts::ARCH.to_owned());
    let count = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    format!("{model}, {count} processors")
}

/// The first line of what a compiler says about its version.
fn first_line(program: &str) -> String {
    Command::new(program)
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8_lossy(&out.stdout).lines().next().map(str::to_owned))
        .unwrap_or_else(|| format!("{program} did not say its version"))
}

/// Every C file under a directory.
fn sources(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| Error::Io(format!("{}: {e}", dir.display())))? {
        let path = entry?.path();
        if path.is_dir() {
            sources(&path, files)?;
        } else if path.extension().is_some_and(|it| it == "c") {
            files.push(path);
        }
    }
    Ok(())
}

/// A mistake in how the task was asked for, with the shape of the answer attached.
fn usage(why: &str) -> Error {
    Error::Io(format!(
        "{why}\n\
         \n\
         usage: cargo xtask budget [--programs <dir>] [--sqlite] [--clang <program>] [--no-build]\n\
         \x20                         [--repeats <n>]\n\
         \n\
         Compiles the corpus at its pin, or the C files under --programs, with rucc and with clang\n\
         at -O2, and divides rucc's time between the phases of spec 42.7. --sqlite adds the SQLite\n\
         amalgamation, found the way `cargo xtask libraries` finds it.\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shares_are_read_out_of_the_specification() {
        let spec = fs::read_to_string(root().join("spec/optimizer/42-measurement.md")).unwrap();
        assert_eq!(shares(&spec).unwrap(), [25, 15, 15, 20, 8, 12, 5]);
    }

    #[test]
    fn a_trace_record_is_added_up_by_phase_and_by_pass() {
        let line = "{\"rucc\":\"0.1.0\",\"input\":\"a.c\",\"output\":\"a.o\",\"ok\":true,\
                    \"seconds\":0.010000,\"peak-kb\":null,\"phases\":{\"read\":0.001000,\
                    \"parse\":0.002000,\"optimize\":0.004000,\"generate\":0.003000},\
                    \"passes\":{\"egraph\":0.002000,\"gvn\":0.001000},\"fired\":{\"egraph\":{}},\
                    \"budget\":{\"front\":0.003000,\"analyses\":0.000500,\"rewriting\":0.002000,\
                    \"scalar\":0.001500,\"selection\":0.001000,\"allocation\":0.001500,\
                    \"machine\":0.000500},\"machine\":{\"machine passes\":0.000200,\
                    \"emission\":0.000300},\"phase-of\":{\"egraph\":\"rewriting\",\
                    \"gvn\":\"scalar\"}}";
        let mut measured = Measured::default();
        add(&mut measured, line);
        add(&mut measured, line);
        add(&mut measured, &line.replace("\"ok\":true", "\"ok\":false"));
        assert_eq!(measured.phases[0], Duration::from_millis(6));
        assert_eq!(measured.phases[2], Duration::from_millis(4));
        assert_eq!(
            measured.passes,
            vec![
                ("egraph".to_owned(), "rewriting".to_owned(), Duration::from_millis(4)),
                ("gvn".to_owned(), "scalar".to_owned(), Duration::from_millis(2)),
            ]
        );
        assert_eq!(
            measured.front,
            vec![
                ("read".to_owned(), Duration::from_millis(2)),
                ("parse".to_owned(), Duration::from_millis(4))
            ]
        );
        assert_eq!(
            measured.machine,
            vec![
                ("machine passes".to_owned(), Duration::from_micros(400)),
                ("emission".to_owned(), Duration::from_micros(600))
            ]
        );
    }

    #[test]
    fn a_phase_over_its_share_names_its_passes() {
        let mut measured = Measured {
            files: 2,
            rucc: Duration::from_secs(3),
            clang: Duration::from_secs(1),
            ..Measured::default()
        };
        measured.phases[2] = Duration::from_secs(9);
        measured.phases[0] = Duration::from_secs(1);
        measured.passes.push(("egraph".to_owned(), "rewriting".to_owned(), Duration::from_secs(8)));
        let out = report("two files", &measured, &[25, 15, 15, 20, 8, 12, 5]);
        assert!(out.contains("3.00x where spec 16.4 allows 1.50x, over"), "{out}");
        assert!(out.contains("rewriting is over, most of it in egraph 8.000 s"), "{out}");
        assert!(!out.contains("front is over"), "{out}");
    }

    #[test]
    fn the_machine_phase_names_its_two_parts() {
        let mut measured = Measured { files: 1, ..Measured::default() };
        measured.phases[6] = Duration::from_secs(5);
        measured.phases[0] = Duration::from_secs(5);
        measured.machine.push(("machine passes".to_owned(), Duration::from_secs(2)));
        measured.machine.push(("emission".to_owned(), Duration::from_secs(3)));
        let out = report("one file", &measured, &[25, 15, 15, 20, 8, 12, 5]);
        assert!(
            out.contains("machine is over, most of it in emission 3.000 s, machine passes 2.000 s"),
            "{out}"
        );
    }
}
