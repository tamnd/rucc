//! What the monitor costs the register allocator.
//!
//! Design: `spec/safe-memory/13-performance.md` section 13.1, whose table of metrics has a row for
//! spill and fill counts, and section 5.2.1 of `spec/safe-memory/05-representation.md`, which is
//! where the risk is stated: a capability in flight is four words in registers, and four words per
//! live pointer is not affordable on x86-64 unless most of them never get materialized. Milestone
//! S4 in `spec/safe-memory/16-milestones.md` asks for the delta on the pointer heavy benchmarks,
//! which is what this reports.
//!
//! Each program in `bench/safety` is compiled twice from the same source with the same flags, once
//! with the monitor off and once with it on, and the compiler is asked for its allocator's numbers
//! both times with `-Zregister-pressure=`. The table is a per program delta, and the row that says
//! most is the reloads, because a value spilled once and read every time round a loop stores once
//! and reloads as often as the loop runs.
//!
//! # Why this runs at -O2 and `cargo xtask cost` runs at -O0
//!
//! The wall clock measurement is S1's and its whole point is to isolate the monitor from the
//! optimizer, so both sides of it are unoptimized. This is S4's, and the question S4 asks is how
//! much of the cost the rules take back, so both sides here are `-O2`. A pressure number at `-O0`
//! would be a measurement of a compiler nobody ships.
//!
//! # Why this needs no runner and `cargo xtask cost` does
//!
//! Nothing is executed. The numbers come out of the compiler while it is compiling, so this works
//! the same on a machine that cannot run an x86-64 Linux program, which is most developer machines
//! here. That is also why it fills a hole `cargo xtask cost` names in its own output: the counters
//! section 13.1 asks for beside the wall clock are not readable through a container, and this one
//! never needed to be read at run time in the first place.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cost::{Bench, benches, compiler};
use crate::runner::TRIPLE;
use crate::{Error, Result, root};

/// The optimization level, on both sides.
const LEVEL: &str = "-O2";

/// What allocating something cost, which is one function or a whole program.
#[derive(Debug, Default, Clone, Copy)]
struct Cost {
    /// Values that went to the stack.
    slots: u64,
    /// Writes into a slot.
    stores: u64,
    /// Reads out of a slot.
    reloads: u64,
}

impl Cost {
    /// Takes in another one.
    fn add(&mut self, other: Self) {
        self.slots += other.slots;
        self.stores += other.stores;
        self.reloads += other.reloads;
    }

    /// What the two counts a program pays at run time come to together.
    const fn traffic(self) -> u64 {
        self.stores + self.reloads
    }
}

/// One program, measured both ways.
#[derive(Debug)]
struct Row {
    /// The program.
    name: String,
    /// What it cost with the monitor off.
    off: Cost,
    /// What it cost with the monitor on.
    on: Cost,
    /// The function whose stack traffic grew the most, and by how much.
    ///
    /// The totals say whether the representation is affordable and this says where to look if it
    /// is not, which is the whole reason the listing is per function rather than one number.
    worst: Option<(String, u64)>,
}

/// Compiles every benchmark both ways and prints the table.
///
/// # Errors
///
/// [`Error::Io`] when a program will not compile, or when the compiler wrote a listing this could
/// not read back.
pub(crate) fn pressure() -> Result<()> {
    let benches = benches()?;
    println!("pressure: {} programs, {LEVEL} both sides, --target={TRIPLE}", benches.len());

    let rucc = compiler()?;
    let work = root().join("target").join("pressure");
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let mut rows = Vec::new();
    for bench in &benches {
        let off = measure(&rucc, &work, bench, "off", "-fsafety=off")?;
        let on = measure(&rucc, &work, bench, "on", "-fsafety=detect")?;
        rows.push(row(&bench.name, &off, &on));
    }
    report(&rows);
    Ok(())
}

/// Compiles one program one way and reads the listing back.
fn measure(
    rucc: &Path,
    work: &Path,
    bench: &Bench,
    suffix: &str,
    tier: &str,
) -> Result<BTreeMap<String, Cost>> {
    let listing = listing(work, &bench.name, suffix);
    let out = Command::new(rucc)
        .args(["-S", &format!("--target={TRIPLE}"), tier, LEVEL])
        .arg(format!("-Zregister-pressure={}", listing.display()))
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
    let text = std::fs::read_to_string(&listing)
        .map_err(|e| Error::Io(format!("could not read {}: {e}", listing.display())))?;
    let read = read(&text);
    if read.is_empty() {
        return Err(Error::Io(format!(
            "{}: the compiler wrote no register pressure for {tier}, so either it compiled no \
             function or the listing changed shape",
            bench.name
        )));
    }
    Ok(read)
}

/// Reads a listing back, one entry per function, ignoring the comment the totals are on.
fn read(text: &str) -> BTreeMap<String, Cost> {
    let mut costs = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(slots), Some(stores), Some(reloads), Some(name)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(slots), Ok(stores), Ok(reloads)) =
            (slots.parse::<u64>(), stores.parse::<u64>(), reloads.parse::<u64>())
        else {
            continue;
        };
        costs.insert(name.to_owned(), Cost { slots, stores, reloads });
    }
    costs
}

/// Adds up one program's functions and finds the one that grew the most.
///
/// A function the instrumented build has and the other does not counts as having grown by all of
/// it, since the check lowering does add functions and the traffic in one of them is traffic the
/// program did not pay before.
fn row(name: &str, off: &BTreeMap<String, Cost>, on: &BTreeMap<String, Cost>) -> Row {
    let mut row =
        Row { name: name.to_owned(), off: Cost::default(), on: Cost::default(), worst: None };
    for cost in off.values() {
        row.off.add(*cost);
    }
    for (func, cost) in on {
        row.on.add(*cost);
        let before = off.get(func).map_or(0, |cost| cost.traffic());
        let grew = cost.traffic().saturating_sub(before);
        if grew > 0 && row.worst.as_ref().is_none_or(|(_, most)| grew > *most) {
            row.worst = Some((func.clone(), grew));
        }
    }
    row
}

/// Prints the table and the summary.
///
/// The counts are absolute rather than a ratio, because a program that spills nothing either way
/// has no ratio and is the answer we want most: it says the capability never got materialized.
fn report(rows: &[Row]) {
    println!();
    println!(
        "{:<32} {:>18} {:>18} {:>10}",
        "program", "slots off / on", "traffic off / on", "delta"
    );
    let mut off = Cost::default();
    let mut on = Cost::default();
    for row in rows {
        println!(
            "{:<32} {:>8} / {:<7} {:>8} / {:<7} {:>+10}",
            row.name,
            row.off.slots,
            row.on.slots,
            row.off.traffic(),
            row.on.traffic(),
            i128::from(row.on.traffic()) - i128::from(row.off.traffic())
        );
        if let Some((func, grew)) = &row.worst {
            println!("{:<32} {func} by {grew}", "  most of it in");
        }
        off.add(row.off);
        on.add(row.on);
    }
    println!();
    println!(
        "pressure: {} slots and {} stack moves with the monitor off, {} and {} with it on",
        off.slots,
        off.traffic(),
        on.slots,
        on.traffic()
    );
    println!(
        "pressure: stores and reloads are counted apart in the listing and added together here, \
         because what a program pays is both of them and what a hot loop pays is mostly reloads."
    );
    println!(
        "pressure: this is the single pass allocator in `spec/10-backend.md` section 10.4, which \
         is the only one there is. A backtracking allocator would spill less on both sides."
    );
}

/// Where one build of one program writes its listing.
///
/// Named for the program and the side, because both builds of a program land in one directory and
/// a run that wrote one file would report the same numbers for both sides of every row.
fn listing(work: &Path, name: &str, suffix: &str) -> PathBuf {
    work.join(format!("{name}.{suffix}.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_totals_comment_is_not_read_back_as_a_function() {
        // It starts with a hash and holds four fields like every other line, so a reader that
        // only split on whitespace would count the whole file twice.
        let costs =
            read("# rucc register pressure: 1 functions, 2 slots, 3 stores, 4 reloads\n2 3 4 f\n");
        assert_eq!(costs.len(), 1);
        assert_eq!(costs["f"].slots, 2);
        assert_eq!(costs["f"].stores, 3);
        assert_eq!(costs["f"].reloads, 4);
    }

    #[test]
    fn a_function_only_the_instrumented_build_has_counts_as_all_new_traffic() {
        // The check lowering adds calls and can add functions, and traffic inside one of those is
        // traffic the uninstrumented program did not pay.
        let off = read("1 1 1 f\n");
        let on = read("1 1 1 f\n0 2 3 __rucc_check\n");
        let row = row("a", &off, &on);
        assert_eq!(row.off.traffic(), 2);
        assert_eq!(row.on.traffic(), 7);
        let (func, grew) = row.worst.expect("one function grew");
        assert_eq!(func, "__rucc_check");
        assert_eq!(grew, 5);
    }

    #[test]
    fn a_function_that_got_cheaper_is_not_reported_as_the_worst_one() {
        // Splitting a loop can take a check out of the hot half, and a build that spills less is
        // not a build with a regression in it to point at.
        let off = read("2 4 4 f\n");
        let on = read("1 1 1 f\n");
        let row = row("a", &off, &on);
        assert!(row.worst.is_none(), "{:?}", row.worst);
    }

    #[test]
    fn the_listing_a_run_writes_is_named_for_the_program_and_the_side() {
        // Two builds of the same program in one directory, so the names have to differ.
        let work = Path::new("/tmp");
        assert_ne!(listing(work, "list", "off"), listing(work, "list", "on"));
    }
}
