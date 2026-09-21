//! What this compiler's line table says, held against what the system compiler's says for the same
//! source.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4 and the second box of tamnd/rucc#1558.
//!
//! `crates/rucc/tests/debug_line.rs` asks whether the sections are there and whether the paths in
//! them went through the prefix map. It cannot ask the question that matters, which is whether the
//! lines are the right lines, because the only way to answer that is to have a second compiler put
//! its answer beside ours and compare, and a compiler's own test suite is the wrong place to depend
//! on another compiler being installed.
//!
//! So this. It compiles the same source twice, reads both tables back with `readelf`, which is not
//! ours, and counts how far apart they are. `crates/rucc-stub` on s390x is why the reading is done
//! by something else: a writer checked by a reader written beside it is two halves holding the same
//! wrong belief and agreeing with each other.
//!
//! # Why it counts rather than compares
//!
//! The two compilers do not generate the same instructions, so there is no address in one that has
//! a counterpart in the other and nothing here can compare addresses directly. What can be compared
//! is what each table says about the same function, so everything below is per function: which line
//! its first byte belongs to, which lines it names at all, and whether any of them is a line outside
//! the function.
//!
//! Even that does not come out equal, and it should not. The system compiler at `-O0` builds a
//! frame in every function, so it always has prologue bytes to put the declaration over. This one
//! does not build a frame it has no use for, so in a leaf function the first byte really is the
//! first statement, and saying the declaration there would be wrong. That is why the claims below
//! are floors and not equalities.
//!
//! # Why the floors are where they are
//!
//! Each one is under what the SQLite amalgamation gives today, by enough that a compiler change
//! that moves one function does not turn the check red and not by enough to hide a pass that stops
//! recording lines. They are meant to be raised as the gaps in tamnd/rucc#1613 close, and the run
//! prints what it measured next to each floor so that raising one is reading a number rather than
//! guessing at it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, cost, indent, libraries, root};

/// One claim about the table, as a share of the functions both compilers wrote.
struct Claim {
    /// What it is called in the output.
    what: &'static str,
    /// The lowest share, in whole percent, that counts as passing.
    floor: u32,
    /// How many the share has to be out of before the floor means anything.
    ///
    /// Zero for a claim that is an absolute rather than a share, which holds over one function as
    /// firmly as over a thousand. The rest are shares that the two compilers are expected to
    /// disagree on some of, and a floor of eighty five percent over six functions is a claim about
    /// which six they were. The amalgamation is what gives a population large enough for the number
    /// to be about the compiler, so on a machine without it those floors are measured and printed
    /// and not held to, and the run says so rather than passing quietly.
    over: u32,
    /// What a number under the floor means, which is the half worth reading.
    why: &'static str,
}

/// What the tables have to agree on, and how far apart they are allowed to be.
const CLAIMS: [Claim; 4] = [
    Claim {
        what: "functions whose table starts at the first byte",
        floor: 100,
        over: 0,
        why: "a function whose table starts later has a hole at the front of it, and a program \
              counter in that hole gets no answer at all rather than a slightly early one, which \
              is the worse of the two for anybody reading a backtrace",
    },
    Claim {
        what: "lines named inside the function they were named for",
        floor: 99,
        over: 500,
        why: "a line outside the function is a line from somewhere else in the file, which is \
              what an expression that came out of a macro looks like when the span kept is the \
              macro's body rather than the place it was written",
    },
    Claim {
        what: "functions whose first byte is the same line both ways",
        floor: 85,
        over: 500,
        why: "the first byte of a function is either its declaration or its first statement, and \
              the two compilers differ there only where this one built no frame, so a fall means \
              something that does have a prologue stopped saying what it is for",
    },
    Claim {
        what: "lines the system compiler names that this one names too",
        floor: 80,
        over: 500,
        why: "a statement with no row is a statement no address resolves to, and a report that \
              lands in one comes back naming whichever line before it did get a row",
    },
];

/// The source that gives a population large enough for a share to be about the compiler.
const AMALGAMATION: &str = "sqlite3.c";

/// One function, as the two tables describe it.
#[derive(Debug, Default)]
struct Both {
    /// Where each of our rows is in the function, and which line it names.
    ours: Vec<(u64, u32)>,
    /// The same from the system compiler's table.
    theirs: Vec<(u64, u32)>,
}

/// How far apart the two tables were over one source.
#[derive(Debug, Default)]
struct Counts {
    /// Functions both compilers wrote, which is what every share below is out of.
    funcs: u32,
    /// Of those, the ones whose table has a row on the first byte.
    front: u32,
    /// The ones whose first byte belongs to the same line in both tables.
    first: u32,
    /// The ones where every line we name lies between the first and the last line the system
    /// compiler named for the same function.
    inside: u32,
    /// Lines the system compiler named, over every function.
    lines: u32,
    /// How many of those we named as well.
    shared: u32,
}

impl Counts {
    /// The five numbers the claims are made of, in the order [`CLAIMS`] is in.
    fn shares(&self) -> [(u32, u32); 4] {
        [
            (self.front, self.funcs),
            (self.inside, self.funcs),
            (self.first, self.funcs),
            (self.shared, self.lines),
        ]
    }

    /// Adds another source's numbers to these.
    fn add(&mut self, other: &Self) {
        self.funcs += other.funcs;
        self.front += other.front;
        self.first += other.first;
        self.inside += other.inside;
        self.lines += other.lines;
        self.shared += other.shared;
    }
}

/// Compiles each source both ways and holds the two tables against each other.
///
/// # Errors
///
/// [`Error::Failed`] when a share is under its floor, and [`Error::Io`] when the compiler will not
/// build or the reading cannot be run at all.
pub(crate) fn lines() -> Result<()> {
    let runner = Runner::find("the line table differential")?;
    let rucc = cost::compiler()?;
    let work = laid_out()?;
    let mut sources = vec![("shapes.c".to_owned(), root().join("tests").join("lines/shapes.c"))];
    // And the largest single translation unit anybody is likely to point this at, when it is on
    // the machine. It is not in the tree for the reason `xtask/src/libraries.rs` says none of them
    // are, and the numbers with it and without it are far enough apart that the run prints which
    // of the two it got.
    if let Some(dir) = libraries::found(&libraries::PROJECTS[0]) {
        sources.push((AMALGAMATION.to_owned(), dir.join(AMALGAMATION)));
    }

    let mut problems = Vec::new();
    let mut total = Counts::default();
    let mut measured = Vec::new();
    for (name, from) in &sources {
        std::fs::copy(from, work.join(name))
            .map_err(|e| Error::Io(format!("could not copy {}: {e}", from.display())))?;
        let object = format!("{}.o", name.trim_end_matches(".c"));
        ours(&rucc, &work, name, &object, &mut problems)?;
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "lines", problems });
    }

    std::fs::write(work.join("run.sh"), script(&sources))
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    let printed = runner.run(&work, "the line table differential")?;
    for (name, _) in &sources {
        let stem = name.trim_end_matches(".c");
        let Some(counts) = read(&printed, stem) else {
            problems.push(format!("{name}: the run printed nothing about it"));
            continue;
        };
        if counts.funcs == 0 {
            problems.push(format!("{name}: no function is in both tables"));
            continue;
        }
        total.add(&counts);
        measured.push((stem.to_owned(), counts));
    }
    if !problems.is_empty() {
        problems.push(format!("what the run printed:\n{}", indent(printed.trim_end())));
        return Err(Error::Failed { task: "lines", problems });
    }

    for (claim, (got, outof)) in CLAIMS.iter().zip(total.shares()) {
        let share = percent(got, outof);
        if outof >= claim.over && share < claim.floor {
            problems.push(format!(
                "{}: {got} of {outof}, which is {share} percent and the floor is {}. {}",
                claim.what, claim.floor, claim.why
            ));
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "lines", problems });
    }

    for (name, counts) in &measured {
        println!("lines: {name}, {} functions in both tables", counts.funcs);
    }
    for (claim, (got, outof)) in CLAIMS.iter().zip(total.shares()) {
        let held = if outof >= claim.over { "floor" } else { "not held to the floor" };
        println!("lines: {}: {got} of {outof}, {held} {}", claim.what, claim.floor);
    }
    if !sources.iter().any(|(name, _)| name == AMALGAMATION) {
        println!(
            "lines: {AMALGAMATION} is not on this machine, so three of the four are measured and \
             not held. Set {} to where it is.",
            libraries::PROJECTS[0].variable
        );
    }
    println!("lines: {} sources, {runner}", sources.len());
    Ok(())
}

/// A share in whole percent, rounded down, and a hundred for nothing out of nothing.
fn percent(got: u32, outof: u32) -> u32 {
    (got * 100).checked_div(outof).unwrap_or(100)
}

/// The directory the script is run over, emptied first.
fn laid_out() -> Result<PathBuf> {
    let work = root().join("target").join("lines");
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;
    Ok(work)
}

/// Compiles one source with this compiler, into the directory the script reads.
///
/// `-fno-function-sections` because a section apiece puts every function at address zero, and what
/// says which function a row belongs to here is the address it names against the symbol table.
fn ours(
    rucc: &Path,
    work: &Path,
    source: &str,
    object: &str,
    problems: &mut Vec<String>,
) -> Result<()> {
    let out = Command::new(rucc)
        .args([&format!("--target={TRIPLE}"), "-c", "-g", "-O0", "-fno-function-sections"])
        .arg("-o")
        .arg(work.join(object))
        .arg(work.join(source))
        .output()
        .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
    if !out.status.success() {
        problems.push(format!(
            "{source} did not compile\n{}",
            indent(String::from_utf8_lossy(&out.stderr).trim_end())
        ));
    }
    Ok(())
}

/// What the runner runs: the system compiler's build of each source, and both tables read back.
///
/// Everything it writes goes under `/tmp`, because the directory it is pointed at is mounted read
/// only. The markers are what the parsing keys on, and they carry the name of the source so that
/// one run over several sources comes back as several answers rather than one.
fn script(sources: &[(String, PathBuf)]) -> String {
    let mut sh = String::from("#!/bin/sh\nexec 2>/dev/null\n");
    for (name, _) in sources {
        let stem = name.trim_end_matches(".c");
        sh.push_str(&format!(
            "gcc -g -O0 -fno-function-sections -c {name} -o /tmp/{stem}-theirs.o || exit 1\n"
        ));
        sh.push_str(&format!("echo '@@ symbols {stem} ours'\nnm -S --defined-only {stem}.o\n"));
        sh.push_str(&format!(
            "echo '@@ rows {stem} ours'\nreadelf --debug-dump=decodedline {stem}.o\n"
        ));
        sh.push_str(&format!(
            "echo '@@ symbols {stem} theirs'\nnm -S --defined-only /tmp/{stem}-theirs.o\n"
        ));
        sh.push_str(&format!(
            "echo '@@ rows {stem} theirs'\nreadelf --debug-dump=decodedline /tmp/{stem}-theirs.o\n"
        ));
    }
    sh.push_str("echo '@@ end'\n");
    sh
}

/// Everything between one marker and the next.
fn chunk<'a>(printed: &'a str, marker: &str) -> Option<&'a str> {
    let at = printed.find(&format!("@@ {marker}\n"))? + marker.len() + 4;
    let rest = &printed[at..];
    Some(rest.find("@@ ").map_or(rest, |end| &rest[..end]))
}

/// One source's numbers, out of what the script printed.
fn read(printed: &str, stem: &str) -> Option<Counts> {
    let mut both: BTreeMap<String, Both> = BTreeMap::new();
    for side in ["ours", "theirs"] {
        let symbols = symbols(chunk(printed, &format!("symbols {stem} {side}"))?);
        for (at, line) in rows(chunk(printed, &format!("rows {stem} {side}"))?) {
            let Some((start, name)) = holding(&symbols, at) else {
                continue;
            };
            let entry = both.entry(name).or_default();
            let into = if side == "ours" { &mut entry.ours } else { &mut entry.theirs };
            into.push((at - start, line));
        }
    }
    let mut counts = Counts::default();
    for one in both.values() {
        if one.ours.is_empty() || one.theirs.is_empty() {
            continue;
        }
        counts.funcs += 1;
        if one.ours.iter().any(|(at, _)| *at == 0) {
            counts.front += 1;
        }
        if at_front(&one.ours) == at_front(&one.theirs) {
            counts.first += 1;
        }
        let ours: BTreeSet<u32> = one.ours.iter().map(|(_, line)| *line).collect();
        let theirs: BTreeSet<u32> = one.theirs.iter().map(|(_, line)| *line).collect();
        // Both ends of what the other compiler said the function covers, which is as close to the
        // extent of the function in the source as anything here can get without a parser.
        let lo = theirs.iter().next().copied().unwrap_or_default();
        let hi = theirs.iter().next_back().copied().unwrap_or_default();
        if ours.iter().all(|line| (lo..=hi).contains(line)) {
            counts.inside += 1;
        }
        counts.lines += u32::try_from(theirs.len()).unwrap_or(u32::MAX);
        counts.shared += u32::try_from(ours.intersection(&theirs).count()).unwrap_or(u32::MAX);
    }
    Some(counts)
}

/// The line the first byte of a function belongs to.
fn at_front(rows: &[(u64, u32)]) -> Option<u32> {
    rows.iter().min_by_key(|(at, _)| *at).map(|(_, line)| *line)
}

/// Every function in a `nm -S --defined-only` listing, as where it starts, how long it is, and what
/// it is called.
///
/// Sorted, because what reads it is a search for the one a byte is inside of.
fn symbols(text: &str) -> Vec<(u64, u64, String)> {
    let mut got: Vec<(u64, u64, String)> = text
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let at = u64::from_str_radix(parts.next()?, 16).ok()?;
            let size = u64::from_str_radix(parts.next()?, 16).ok()?;
            let kind = parts.next()?;
            let name = parts.next()?;
            // The two letters a function gets, one for a name every object can see and one for a
            // name only this object can. Anything else is data and has no line table of its own.
            (kind == "T" || kind == "t").then(|| (at, size, name.to_owned()))
        })
        .collect();
    got.sort();
    got
}

/// Which function a byte is inside of.
fn holding(symbols: &[(u64, u64, String)], at: u64) -> Option<(u64, String)> {
    let which = symbols.partition_point(|(start, _, _)| *start <= at).checked_sub(1)?;
    let (start, size, name) = &symbols[which];
    (at < start + size).then(|| (*start, name.clone()))
}

/// Every row in a `readelf --debug-dump=decodedline` listing, as an address and a line.
///
/// A row whose line is a dash ends a sequence and describes no instruction, so it is not one. The
/// header and the file names around them have no number in that column and fall out the same way.
fn rows(text: &str) -> Vec<(u64, u32)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let _name = parts.next()?;
            let which = parts.next()?.parse::<u32>().ok()?;
            Some((address(parts.next()?)?, which))
        })
        .collect()
}

/// An address out of that column, which readelf writes with a prefix except when it is zero.
///
/// Zero is the one that matters most here, since it is the first byte of the first function and the
/// claim about the front of a function is a claim about exactly that row.
fn address(said: &str) -> Option<u64> {
    match said.strip_prefix("0x") {
        Some(rest) => u64::from_str_radix(rest, 16).ok(),
        None => said.parse::<u64>().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symbol_listing_gives_up_its_functions_and_nothing_else() {
        let text = "0000000000000060 0000000000000012 T main\n\
                    0000000000000000 0000000000000004 r some.0\n\
                    0000000000000010 0000000000000050 t total\n\
                    0000000000000000 U printf\n";
        let got = symbols(text);
        assert_eq!(got, vec![(0x10, 0x50, "total".to_owned()), (0x60, 0x12, "main".to_owned()),]);
    }

    #[test]
    fn a_byte_is_inside_the_function_that_covers_it_and_no_other() {
        let got = symbols(
            "0000000000000000 0000000000000010 T a\n0000000000000020 0000000000000010 T b\n",
        );
        assert_eq!(holding(&got, 0), Some((0, "a".to_owned())));
        assert_eq!(holding(&got, 0xf), Some((0, "a".to_owned())));
        // The gap between the two, which is alignment padding and belongs to neither.
        assert_eq!(holding(&got, 0x10), None);
        assert_eq!(holding(&got, 0x25), Some((0x20, "b".to_owned())));
        assert_eq!(holding(&got, 0x30), None);
    }

    #[test]
    fn a_decoded_table_gives_up_its_rows_and_skips_the_ends_of_sequences() {
        let text = "\
File name                        Line number    Starting address    View    Stmt
g.c                                        3                   0               x
g.c                                        4                 0x5               x
g.c                                        -                 0x6
";
        // The first row's address is printed without the prefix, which is readelf saying zero, and
        // a row that names no line is the end of a sequence rather than an instruction.
        assert_eq!(rows(text), vec![(0, 3), (0x5, 4)]);
    }

    #[test]
    fn a_share_of_nothing_is_a_share_that_passes() {
        // A source with no function in it is not a failure, it is a source with nothing to say,
        // and the count of functions is what says so.
        assert_eq!(percent(0, 0), 100);
        assert_eq!(percent(1, 3), 33);
        assert_eq!(percent(3, 3), 100);
    }

    #[test]
    fn the_script_asks_for_both_tables_of_every_source() {
        let sources = vec![("one.c".to_owned(), PathBuf::from("/tmp/one.c"))];
        let sh = script(&sources);
        assert!(sh.contains("gcc -g -O0 -fno-function-sections -c one.c"));
        assert!(sh.contains("@@ symbols one ours"));
        assert!(sh.contains("@@ rows one theirs"));
    }
}
