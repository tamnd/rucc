//! `-fopt-info`, which is the compiler saying what it did and what it nearly did.
//!
//! GCC's version is documented at `gcc/doc/invoke.texi:20403` and takes the keywords `optimized`,
//! `missed`, `note` and `all`. Section 42.2 of `spec/optimizer/42-measurement.md` picks out
//! `missed` as the one that earns the feature: it turns "this loop was not vectorized" from a
//! mystery into a sentence, and a compiler that reports only its successes cannot be tuned by
//! anybody outside it.
//!
//! What is printed here comes from the records the passes returned, so this module invents
//! nothing and cannot drift from what the passes actually did. Everything a pass wants said has
//! to be in its [`crate::Stats`], which is the same requirement that makes the pass manager
//! believe it changed something.
//!
//! # The format, and the position that is not in it
//!
//! One line per pass per function per event:
//!
//! ```text
//! a.c: f: optimized: integer instruction folded to a constant (3) [fold]
//! ```
//!
//! The file, the function, the kind, the event, how many times, and the pass whose `-fno-` flag
//! turns it off. GCC puts a line and a column after the file. This does not, because the IR does
//! not carry source positions yet, and a zero there would be a position rather than an admission
//! that there is not one. When the IR carries them this line grows a `:line:col` and the shape of
//! everything else stays as it is.

use std::fmt::Write as _;

use rucc_base::Interner;

use crate::pipeline::Report;
use crate::stats::Kind;

/// Which kinds of remark were asked for.
///
/// Empty is a real answer and means the flag was never given, which is why [`Wants::is_empty`]
/// exists and why the driver checks it rather than carrying an `Option` around.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wants {
    /// Whether rewrites are printed.
    optimized: bool,
    /// Whether the sites a pass gave up on are printed.
    missed: bool,
    /// Whether everything else is printed.
    note: bool,
}

impl Wants {
    /// Nothing, which is what a compilation with no `-fopt-info` on it asks for.
    #[must_use]
    pub const fn none() -> Self {
        Self { optimized: false, missed: false, note: false }
    }

    /// Every kind, which is what `-fopt-info-all` asks for.
    #[must_use]
    pub const fn all() -> Self {
        Self { optimized: true, missed: true, note: true }
    }

    /// Adds what one `-fopt-info` argument asked for.
    ///
    /// The keywords are joined by hyphens, the way GCC joins them, so
    /// `-fopt-info-missed-optimized` is two of them. A bare `-fopt-info` is spelled here as an
    /// empty argument and means `optimized`, which is the default GCC documents.
    ///
    /// Two of these flags add up rather than the second replacing the first, because a person who
    /// writes both wanted both.
    ///
    /// # Errors
    ///
    /// When a keyword is not one this compiler has. A misspelling that quietly printed nothing
    /// would look exactly like a compilation where no pass had anything to say, and telling those
    /// two apart is the whole reason somebody reached for this flag.
    pub fn add(&mut self, spec: &str) -> Result<(), String> {
        if spec.is_empty() {
            self.optimized = true;
            return Ok(());
        }
        for word in spec.split('-') {
            match word {
                "all" => *self = Self::all(),
                "optimized" => self.optimized = true,
                "missed" => self.missed = true,
                "note" => self.note = true,
                _ => {
                    return Err(format!(
                        "`{word}` is not a kind of remark this compiler makes, which are \
                         `optimized`, `missed`, `note` and `all`"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whether this kind is printed.
    #[must_use]
    pub const fn wants(self, kind: Kind) -> bool {
        match kind {
            Kind::Optimized => self.optimized,
            Kind::Missed => self.missed,
            Kind::Note => self.note,
        }
    }

    /// Whether nothing at all was asked for.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.optimized && !self.missed && !self.note
    }
}

/// Renders the remarks a run produced, as the lines `-fopt-info` prints.
///
/// In the order the passes ran, then the order the module holds its functions, then the order
/// each pass recorded its events. That is the order the work happened in, which is the order
/// somebody reading down the output is reconstructing.
///
/// A function a pass had nothing to say about produces no lines. The record for it still exists,
/// and `--print-pass-stats` is where a pass that fires on nothing becomes visible. Printing a
/// line per silent pass per function here would bury the ones that spoke.
#[must_use]
pub fn render(file: &str, report: &Report, names: &Interner, wants: Wants) -> String {
    let mut out = String::new();
    if wants.is_empty() {
        return out;
    }
    for remark in &report.remarks {
        let func = names.resolve(remark.func);
        for event in remark.stats.events() {
            if !wants.wants(event.kind) {
                continue;
            }
            let _ = writeln!(
                out,
                "{file}: {func}: {}: {} ({}) [{}]",
                event.kind, event.what, event.count, remark.pass
            );
        }
    }
    out
}

/// Renders what `--print-pass-stats` prints: every pass that ran, and its totals.
///
/// Every pass, including the ones that said nothing, and that is the difference between this and
/// [`render`]. A pass that fires zero times over a whole corpus is either dead code or a bug, and
/// there is no way to find out which from output that omits it.
#[must_use]
pub fn totals(report: &Report) -> String {
    let mut out = String::new();
    let mut seen: Vec<&'static str> = Vec::new();
    for remark in &report.remarks {
        if !seen.contains(&remark.pass) {
            seen.push(remark.pass);
        }
    }
    for pass in seen {
        let stats = report.totals(pass);
        if stats.is_empty() {
            let _ = writeln!(out, "{pass}: nothing");
            continue;
        }
        for event in stats.events() {
            let _ = writeln!(out, "{pass}: {}: {} ({})", event.kind, event.what, event.count);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::{Wants, render, totals};
    use crate::Stats;
    use crate::pipeline::{Remark, Report};
    use crate::stats::Kind;

    /// A report with one talkative pass and one silent one.
    fn report(names: &mut Interner) -> Report {
        let f = names.intern("f");
        let g = names.intern("g");
        let mut loud = Stats::new();
        loud.record(Kind::Optimized, "a rewrite", 3);
        loud.missed("a site it gave up on");
        loud.note("something an analysis found");
        Report {
            remarks: vec![
                Remark { pass: "fold", func: f, stats: loud },
                Remark { pass: "fold", func: g, stats: Stats::new() },
                Remark { pass: "dce", func: f, stats: Stats::new() },
                Remark { pass: "dce", func: g, stats: Stats::new() },
            ],
            ..Report::default()
        }
    }

    #[test]
    fn nothing_is_printed_when_nothing_was_asked_for() {
        let mut names = Interner::new();
        let report = report(&mut names);
        assert_eq!(render("a.c", &report, &names, Wants::none()), "");
    }

    #[test]
    fn a_bare_flag_asks_for_the_rewrites_and_nothing_else() {
        let mut wants = Wants::none();
        wants.add("").expect("the bare flag is always allowed");
        assert!(wants.wants(Kind::Optimized));
        assert!(!wants.wants(Kind::Missed));
        assert!(!wants.wants(Kind::Note));
    }

    #[test]
    fn keywords_are_joined_by_hyphens_and_two_flags_add_up() {
        let mut wants = Wants::none();
        wants.add("missed-note").expect("both of those exist");
        assert!(!wants.wants(Kind::Optimized));
        assert!(wants.wants(Kind::Missed));
        assert!(wants.wants(Kind::Note));
        wants.add("optimized").expect("that exists too");
        assert_eq!(wants, Wants::all(), "the second flag replaced the first");
    }

    #[test]
    fn a_keyword_that_does_not_exist_is_refused_rather_than_ignored() {
        let mut wants = Wants::none();
        let why = wants.add("vectorized").expect_err("no such kind");
        assert!(why.contains("`optimized`"), "{why}");
        let why = wants.add("missed-vectorized").expect_err("one bad word spoils the argument");
        assert!(why.contains("vectorized"), "{why}");
    }

    #[test]
    fn every_kind_asked_for_is_printed_with_its_count_its_function_and_its_pass() {
        let mut names = Interner::new();
        let report = report(&mut names);
        let text = render("a.c", &report, &names, Wants::all());
        assert_eq!(
            text,
            "a.c: f: optimized: a rewrite (3) [fold]\n\
             a.c: f: missed: a site it gave up on (1) [fold]\n\
             a.c: f: note: something an analysis found (1) [fold]\n"
        );
    }

    #[test]
    fn asking_for_the_misses_leaves_out_the_rewrites() {
        let mut names = Interner::new();
        let report = report(&mut names);
        let mut wants = Wants::none();
        wants.add("missed").expect("that exists");
        let text = render("a.c", &report, &names, wants);
        assert_eq!(text, "a.c: f: missed: a site it gave up on (1) [fold]\n");
    }

    #[test]
    fn the_totals_name_every_pass_that_ran_including_the_ones_that_said_nothing() {
        let mut names = Interner::new();
        let report = report(&mut names);
        let text = totals(&report);
        // `dce` ran over both functions and had nothing to say, and that is the line worth
        // having. A pass that fires on nothing is either dead code or a bug, and output that
        // leaves it out cannot tell anybody which.
        assert_eq!(
            text,
            "fold: optimized: a rewrite (3)\n\
             fold: missed: a site it gave up on (1)\n\
             fold: note: something an analysis found (1)\n\
             dce: nothing\n"
        );
    }
}
