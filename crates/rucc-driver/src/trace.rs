//! What `-frucc-trace=<file>` writes: one line of JSON per file compiled, saying where the time
//! went.
//!
//! The reader is a build harness, not a person. `rucc-postgres` runs every compile of a Postgres
//! build through a shim and wants to know, for each translation unit, which phase and which
//! optimizer pass took the time, so that a file that got slow names its cause without anybody
//! rebuilding it under a profiler. `cargo xtask cost` already collects the same numbers for our
//! own benchmarks, and this is those numbers on the command line.
//!
//! One line per file, appended with a single write, so that a parallel make with many compilers
//! writing to one trace file gets whole lines in some order rather than lines cut into each other.

use std::fmt::Write as _;
use std::io::Write as _;
use std::time::{Duration, Instant};

use rucc_opt::Stats;

/// Where the time went in one compile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timing {
    /// Each phase that ran, in the order it ran. A compile that stopped early has fewer.
    pub phases: Vec<(&'static str, Duration)>,
    /// Each optimizer pass across the whole module, in the order the passes first ran.
    pub passes: Vec<(&'static str, Duration)>,
    /// What each of those passes said across the whole module, in the same order, a pass that
    /// said nothing included.
    pub fired: Vec<(&'static str, Stats)>,
}

/// What the optimizer hands back about its passes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Passes {
    /// How long each pass took, in the order the passes first ran.
    pub time: Vec<(&'static str, Duration)>,
    /// What each pass said, in the same order.
    pub fired: Vec<(&'static str, Stats)>,
}

/// Times phases one after another, each from where the one before it stopped.
#[derive(Debug)]
pub struct Clock {
    last: Instant,
    timing: Timing,
}

impl Clock {
    /// Starts the clock.
    #[must_use]
    pub fn start() -> Clock {
        Clock { last: Instant::now(), timing: Timing::default() }
    }

    /// Records a phase as everything since the last one.
    pub fn lap(&mut self, phase: &'static str) {
        let now = Instant::now();
        self.timing.phases.push((phase, now - self.last));
        self.last = now;
    }

    /// Runs `f` and records it as a phase, with whatever came before it since the last lap.
    pub fn time<T>(&mut self, phase: &'static str, f: impl FnOnce() -> T) -> T {
        let out = f();
        self.lap(phase);
        out
    }

    /// Hands over what the optimizer said about its passes.
    pub fn passes(&mut self, passes: Passes) {
        self.timing.passes = passes.time;
        self.timing.fired = passes.fired;
    }

    /// What was recorded.
    #[must_use]
    pub fn finish(self) -> Timing {
        self.timing
    }
}

/// What one line of the trace says about one file.
#[derive(Debug)]
pub struct Record<'a> {
    /// The input as the command line named it.
    pub input: &'a str,
    /// Where the output went, or `-` for standard output.
    pub output: &'a str,
    /// Whether the compile succeeded.
    pub ok: bool,
    /// The wall time for the whole file, which is a little more than the phases added up.
    pub total: Duration,
    /// The phases and passes.
    pub timing: &'a Timing,
}

impl Record<'_> {
    /// The line, with its newline.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = write!(
            out,
            "{{\"rucc\":\"{}\",\"input\":{},\"output\":{},\"ok\":{},\"seconds\":{}",
            env!("CARGO_PKG_VERSION"),
            quoted(self.input),
            quoted(self.output),
            self.ok,
            seconds(self.total)
        );
        match peak_kb() {
            Some(kb) => {
                let _ = write!(out, ",\"peak-kb\":{kb}");
            }
            None => out.push_str(",\"peak-kb\":null"),
        }
        out.push_str(",\"phases\":");
        object(&mut out, &self.timing.phases);
        out.push_str(",\"passes\":");
        object(&mut out, &self.timing.passes);
        out.push_str(",\"fired\":");
        fired(&mut out, &self.timing.fired);
        out.push_str("}\n");
        out
    }
}

/// Appends one record to the trace file, creating it if it is not there.
///
/// # Errors
///
/// The message to print, naming the file.
pub fn append(path: &str, record: &Record<'_>) -> Result<(), String> {
    let line = record.render();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|e| format!("{path}: {e}"))
}

/// Seconds with microseconds, which is finer than anything here is worth measuring.
fn seconds(time: Duration) -> String {
    format!("{:.6}", time.as_secs_f64())
}

/// A list of names and times as one JSON object, in the order given.
fn object(out: &mut String, times: &[(&'static str, Duration)]) {
    out.push('{');
    for (index, (name, time)) in times.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(out, "{}:{}", quoted(name), seconds(*time));
    }
    out.push('}');
}

/// What each pass said as one JSON object, a pass to an object of its events, each event named by
/// its kind and its text the way `-fopt-info` names it and given its count. A pass that said
/// nothing is an empty object, which is how a count over many files finds the passes that never
/// fired.
fn fired(out: &mut String, passes: &[(&'static str, Stats)]) {
    out.push('{');
    for (index, (pass, stats)) in passes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(out, "{}:{{", quoted(pass));
        for (at, event) in stats.events().iter().enumerate() {
            if at > 0 {
                out.push(',');
            }
            let what = format!("{}: {}", event.kind, event.what);
            let _ = write!(out, "{}:{}", quoted(&what), event.count);
        }
        out.push('}');
    }
    out.push('}');
}

/// A JSON string. Paths are the only text in here that can hold anything unusual.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if u32::from(ch) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// The most memory the process has held so far, in kilobytes, where the system says.
///
/// Linux keeps it in `/proc/self/status` as `VmHWM`. Other systems want a call into the C
/// library, which the driver does not link, so the trace says `null` there and the shim that
/// runs the compiler measures it from outside. A compiler usually compiles one file, so for most
/// lines this is that file's peak. With several files on one command line it is the peak up to
/// the end of this one.
fn peak_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    line["VmHWM:".len()..].trim().trim_end_matches("kB").trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_one_line_of_json_with_the_phases_in_order() {
        let timing = Timing {
            phases: vec![
                ("preprocess", Duration::from_millis(3)),
                ("parse", Duration::from_micros(1500)),
            ],
            passes: vec![("simplify", Duration::from_micros(20))],
            fired: Vec::new(),
        };
        let line = Record {
            input: "src/a \"b\".c",
            output: "a.o",
            ok: true,
            total: Duration::from_millis(5),
            timing: &timing,
        }
        .render();
        assert!(line.ends_with("}\n"));
        assert_eq!(line.lines().count(), 1);
        assert!(line.contains("\"input\":\"src/a \\\"b\\\".c\""));
        assert!(line.contains("\"seconds\":0.005000"));
        assert!(line.contains("\"phases\":{\"preprocess\":0.003000,\"parse\":0.001500}"));
        assert!(line.contains("\"passes\":{\"simplify\":0.000020}"));
        assert!(line.contains("\"fired\":{}"));
    }

    #[test]
    fn a_pass_that_said_nothing_is_written_as_well_as_one_that_did() {
        let mut folded = Stats::new();
        folded.record(rucc_opt::stats::Kind::Optimized, "instruction folded to a constant", 3);
        folded.missed("division by a value that might be zero");
        let timing =
            Timing { fired: vec![("fold", folded), ("dce", Stats::new())], ..Timing::default() };
        let line = Record {
            input: "a.c",
            output: "a.o",
            ok: true,
            total: Duration::ZERO,
            timing: &timing,
        }
        .render();
        assert!(
            line.contains(
                "\"fired\":{\"fold\":{\"optimized: instruction folded to a constant\":3,\
                 \"missed: division by a value that might be zero\":1},\"dce\":{}}"
            ),
            "{line}"
        );
    }

    #[test]
    fn control_characters_in_a_name_are_escaped() {
        assert_eq!(quoted("a\tb\u{1}"), "\"a\\tb\\u0001\"");
    }

    #[test]
    fn records_are_appended_to_the_file() {
        let dir = std::env::temp_dir().join(format!("rucc-trace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let path = path.to_str().unwrap();
        let timing = Timing::default();
        let record = Record {
            input: "a.c",
            output: "a.o",
            ok: true,
            total: Duration::ZERO,
            timing: &timing,
        };
        append(path, &record).unwrap();
        append(path, &record).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(text.lines().count(), 2);
    }
}
