//! The distribution budget of `spec/cross-compile/13-distribution.md` section 13.1, measured.
//!
//! The argument document 02.4 makes against carrying LLVM is an argument about size, and an argument
//! about size obliges a number. Section 13.1 is that number, a row per component and a total, and it
//! says a regression against it is release blocking in the same way a benchmark regression is. There
//! was no command that would notice one.
//!
//! So the table is read out of the specification rather than copied into this file. The numbers in a
//! budget move as things are measured, twice already for the two header trees, and a second copy of
//! them here would be a second thing to update and the one that is wrong would be the one nobody
//! reads. What this file holds instead is how each row is measured, which is the part a document
//! cannot say.
//!
//! Every row has to be claimed. A row in the table that nothing here knows how to measure is a
//! failure, and so is an entry here that matches no row, which is the same rule `interpose` uses on
//! its two lists and it is here for the same reason: the check is worth having because the
//! specification is edited by people, and a row added to the table without a measurement is a budget
//! nobody is held to.
//!
//! Most rows are not produced in this repository yet, and they say so rather than passing. A payload
//! of zero bytes is under every budget ever written, so a check that treated absence as success would
//! go green for the whole of the work it exists to watch. The rows that are produced elsewhere name
//! where, because `tamnd/rucc-cross` measures the two header trees already and those numbers are what
//! the table now carries.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

use crate::{Error, Result, root};

/// How one row of the table is measured.
enum How {
    /// The release binary, built if it is not there.
    Binary,
    /// A directory of files in this repository, with what to say about where they end up.
    Tree { under: &'static str, note: &'static str },
    /// Produced somewhere, and not here. The string says where and is printed.
    Elsewhere(&'static str),
    /// Not produced anywhere yet. The string says what would produce it.
    Absent(&'static str),
    /// In the table and not in the base, so measured by nobody here. The linker and the two SDKs.
    OutOfBase(&'static str),
    /// The base total, which is the sum of the rows above it rather than a thing on disk.
    Total,
}

/// One row of section 13.1, and how to measure it.
///
/// The key is matched against the start of the component cell with the backticks and the bold markers
/// taken out, so a row whose wording gains a parenthesis still matches. Two keys where one is a prefix
/// of the other would be ambiguous, and the check below says so rather than picking one.
const ROWS: &[(&str, How)] = &[
    ("rucc itself", How::Binary),
    (
        "compiler headers",
        How::Tree {
            under: "crates/rucc-session/runtime/include",
            note: "compiled into the binary with include_str!, so these bytes are inside the row above",
        },
    ),
    (
        "libc descriptions",
        How::Absent(
            "the abilist blob of spec/cross-compile/09-libc-stubs.md is what would write it",
        ),
    ),
    ("glibc header tree", How::Elsewhere("bin/glibc-headers in tamnd/rucc-cross")),
    ("musl headers", How::Elsewhere("bin/sysroot in tamnd/rucc-cross")),
    ("Linux uapi headers", How::Elsewhere("bin/kernel-headers in tamnd/rucc-cross")),
    (
        "mingw-w64 headers",
        How::Absent(
            "nothing touches it, which is worth knowing about the largest row in the table",
        ),
    ),
    ("start files", How::Elsewhere("bin/glibc-startfiles in tamnd/rucc-cross")),
    (
        "librucc_builtins.a",
        How::Absent("cargo xtask builtins writes one target's archive and this row is every one"),
    ),
    ("base distribution", How::Total),
    ("linker (on demand)", How::OutOfBase("document 11.2 keeps it out of the binary")),
    ("Darwin SDK", How::OutOfBase("section 13.4 is why neither is distributed")),
];

/// Which binary the report is about.
///
/// A triple because the release workflow builds one per published target into its own directory, and
/// the row the binary is measured against is the same row for all of them. A check that could only
/// look at `target/release` would be a check of whichever of them the machine built last.
struct Wanted {
    /// Whether to build before measuring.
    build: bool,
    /// The target, where the caller named one, spelled the way `rustc` spells it.
    target: Option<String>,
}

/// What one row came to.
struct Measured {
    /// The component cell, as the table spells it.
    component: String,
    /// The budget in bytes, where the cell names one.
    budget: Option<u64>,
    /// The budget cell, for the report, because "15 to 40 MB" is worth printing as written.
    budget_text: String,
    /// What it weighs, where that is a question with an answer today.
    bytes: Option<u64>,
    /// Whether the row counts towards the base total.
    in_base: bool,
    /// What to print after the numbers.
    note: String,
}

/// Reads section 13.1, measures what can be measured, and holds each row to its budget.
///
/// `--no-build` measures what is already in `target`, for the release workflow, which has just built
/// the thing and should not build it twice. `--target <triple>` measures that target's binary, which
/// is the form the release workflow wants because each of its published targets is built in its own
/// job and the row is the same row for all of them.
///
/// # Errors
///
/// [`Error::Io`] when the specification cannot be read or its table cannot be found, and
/// [`Error::Failed`] with one problem per row that is over budget, per row nothing here claims and
/// per entry here that matches no row.
pub(crate) fn size(args: &[String]) -> Result<()> {
    let mut build = true;
    let mut target = None;
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--no-build" => build = false,
            "--target" => {
                at += 1;
                target = Some(
                    args.get(at)
                        .cloned()
                        .ok_or_else(|| Error::Io("--target wants a triple after it".to_owned()))?,
                );
            }
            other => match other.strip_prefix("--target=") {
                Some(triple) => target = Some(triple.to_owned()),
                None => return Err(Error::Io(format!("size: unknown argument `{other}`"))),
            },
        }
        at += 1;
    }
    let wanted = Wanted { build, target };

    let spec = root().join("spec/cross-compile/13-distribution.md");
    let text =
        fs::read_to_string(&spec).map_err(|e| Error::Io(format!("{}: {e}", spec.display())))?;
    let table = budget_table(&text)
        .ok_or_else(|| Error::Io(format!("{}: no table under section 13.1", spec.display())))?;

    let mut problems = Vec::new();
    let mut rows = Vec::new();
    let mut claimed = vec![false; ROWS.len()];
    for (component, budget_text) in table {
        let plain = plain(&component);
        let matches: Vec<usize> = ROWS
            .iter()
            .enumerate()
            .filter(|(_, (key, _))| plain.starts_with(key))
            .map(|(i, _)| i)
            .collect();
        let at = match matches.as_slice() {
            [one] => *one,
            [] => {
                problems.push(format!(
                    "section 13.1 has a row for `{plain}` and xtask/src/size.rs does not say how to \
                     measure it, so nothing holds it to its budget"
                ));
                continue;
            }
            several => {
                let keys: Vec<&str> = several.iter().map(|&i| ROWS[i].0).collect();
                problems.push(format!(
                    "the row `{plain}` matches {} keys in xtask/src/size.rs, which are {}",
                    several.len(),
                    keys.join(", ")
                ));
                continue;
            }
        };
        claimed[at] = true;
        rows.push(measure(&component, &budget_text, &ROWS[at].1, &wanted)?);
    }
    for (at, taken) in claimed.iter().enumerate() {
        if !taken {
            problems.push(format!(
                "xtask/src/size.rs claims to measure `{}` and section 13.1 has no such row",
                ROWS[at].0
            ));
        }
    }

    // The total is the sum of what is measured rather than of the budgets, so it is a floor on the
    // base and not the base. It still answers the question the row exists for, which is whether what
    // we ship today is inside the number we published, and it becomes the real answer as the payloads
    // above it start being produced here.
    let measured: u64 = rows.iter().filter(|r| r.in_base).filter_map(|r| r.bytes).sum();
    if let Some(total) = rows.iter_mut().find(|r| r.note == TOTAL_NOTE) {
        total.bytes = Some(measured);
    }

    print!("{}", report(&rows));

    for row in &rows {
        if let (Some(budget), Some(bytes)) = (row.budget, row.bytes) {
            if bytes > budget {
                problems.push(format!(
                    "{} is {} against a budget of {}",
                    row.component,
                    mb(bytes),
                    mb(budget)
                ));
            }
        }
    }

    if problems.is_empty() { Ok(()) } else { Err(Error::Failed { task: "size", problems }) }
}

/// What the total row's note says, which is also how the total is found again.
const TOTAL_NOTE: &str =
    "the measurement is the sum of the rows above that are produced here, so it is a floor on it";

/// Measures one row.
fn measure(component: &str, budget_text: &str, how: &How, wanted: &Wanted) -> Result<Measured> {
    let budget = bytes_in(budget_text);
    let (bytes, in_base, note) = match how {
        How::Binary => {
            let path = binary(wanted)?;
            let size = fs::metadata(&path)
                .map_err(|e| Error::Io(format!("{}: {e}", path.display())))?
                .len();
            let shown = path.strip_prefix(root()).unwrap_or(&path).to_path_buf();
            (Some(size), true, format!("{}, with one back end in it", shown.display()))
        }
        How::Tree { under, note } => {
            let dir = root().join(under);
            let (size, files) =
                tree(&dir).map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?;
            (Some(size), false, format!("{files} files in {under}, {note}"))
        }
        How::Elsewhere(where_) => (
            None,
            true,
            format!("produced by {where_}, so the number in the table is the measurement"),
        ),
        How::Absent(what) => (None, true, format!("not produced yet, and {what}")),
        How::OutOfBase(why) => (None, false, (*why).to_owned()),
        How::Total => (None, false, TOTAL_NOTE.to_owned()),
    };
    Ok(Measured {
        component: plain(component),
        budget,
        budget_text: plain(budget_text),
        bytes,
        in_base,
        note,
    })
}

/// The release binary, built first unless the caller said not to.
///
/// Built rather than required, because a check that depends on somebody having run the right command
/// first is a check that reports on whatever is lying in `target/`. `--no-build` is for the job that
/// has just built it and for a machine with no network.
fn binary(wanted: &Wanted) -> Result<PathBuf> {
    let mut path = root().join("target");
    if let Some(triple) = &wanted.target {
        path.push(triple);
    }
    path.push("release");
    path.push(if cfg!(windows) { "rucc.exe" } else { "rucc" });
    if wanted.build {
        let mut cargo = Command::new("cargo");
        cargo.args(["build", "-q", "--release", "-p", "rucc", "--bin", "rucc"]);
        if let Some(triple) = &wanted.target {
            cargo.args(["--target", triple]);
        }
        let status = cargo
            .current_dir(root())
            .status()
            .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
        if !status.success() {
            return Err(Error::Failed {
                task: "size",
                problems: vec![
                    "the release build failed, so there is nothing to measure".to_owned(),
                ],
            });
        }
    }
    if !path.is_file() {
        return Err(Error::Io(format!(
            "{} is not there. Run it without --no-build, or `cargo build --release`",
            path.display()
        )));
    }
    Ok(path)
}

/// The bytes and the file count under a directory, walked.
fn tree(dir: &Path) -> io::Result<(u64, usize)> {
    let mut bytes = 0;
    let mut files = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let (more, count) = tree(&entry.path())?;
            bytes += more;
            files += count;
        } else {
            bytes += entry.metadata()?.len();
            files += 1;
        }
    }
    Ok((bytes, files))
}

/// The component and budget cells of the table under section 13.1, in the order they appear.
///
/// The table is found by its heading and ends at the blank line after it, which is how every table in
/// the specification is written. The header row and the separator are dropped.
fn budget_table(text: &str) -> Option<Vec<(String, String)>> {
    let from = text.find("## 13.1")?;
    let mut rows = Vec::new();
    for line in text[from..].lines() {
        if !line.starts_with('|') {
            if rows.is_empty() {
                continue;
            }
            break;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 2 || cells[0] == "component" || cells[0].starts_with("---") {
            continue;
        }
        rows.push((cells[0].to_owned(), cells[1].to_owned()));
    }
    (!rows.is_empty()).then_some(rows)
}

/// A cell with the markdown taken out, so that a key matches what the sentence says.
fn plain(cell: &str) -> String {
    cell.replace("**", "").replace('`', "").trim().to_owned()
}

/// The first size in a budget cell, in bytes.
///
/// The first, because a cell says either one number or two, and where it says two the first is the
/// uncompressed one: the base row reads "≤ 60 MB uncompressed, ≤ 25 MB compressed", and what the rows
/// above it sum to is the uncompressed half. A cell with no number in it, which is the two SDKs, has
/// no budget rather than a budget of zero.
fn bytes_in(cell: &str) -> Option<u64> {
    let plain = plain(cell);
    let mut chars = plain.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if !c.is_ascii_digit() {
            continue;
        }
        let rest = &plain[at..];
        let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(rest.len());
        let number: f64 = rest[..end].parse().ok()?;
        let unit = rest[end..].trim_start();
        let scale = if unit.starts_with("KB") {
            1024.0
        } else if unit.starts_with("MB") {
            1024.0 * 1024.0
        } else {
            // A number that is not a size, which is a section reference or a document number. Skip
            // the digits and keep looking, so that "document 11.2" does not become a budget.
            while chars.peek().is_some_and(|&(next, _)| next < at + end) {
                chars.next();
            }
            continue;
        };
        #[expect(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "a budget in megabytes, and it is positive"
        )]
        return Some((number * scale) as u64);
    }
    None
}

/// A size, as the table writes them.
fn mb(bytes: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "megabytes to one decimal place")]
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb < 1.0 {
        #[expect(clippy::cast_precision_loss, reason = "kilobytes, rounded")]
        let kb = bytes as f64 / 1024.0;
        format!("{kb:.0} KB")
    } else {
        format!("{mb:.1} MB")
    }
}

/// The report, which is the table with a measurement beside each budget.
///
/// Two lines a row rather than a column per field, because the component cells are sentences and the
/// longest of them is sixty characters. A row nothing can be measured against still prints, since the
/// note is the whole of what those rows have to say.
fn report(rows: &[Measured]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "xtask: spec/cross-compile/13-distribution.md section 13.1, measured");
    for row in rows {
        let measured = match row.bytes {
            Some(bytes) => mb(bytes),
            None => "not measured".to_owned(),
        };
        let _ = writeln!(out, "  {}: {} budgeted, {measured}", row.component, row.budget_text);
        let _ = writeln!(out, "    {}", row.note);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_sizes_the_table_writes() {
        assert_eq!(bytes_in("**≤ 25 MB**"), Some(25 * 1024 * 1024));
        assert_eq!(bytes_in("~200 KB"), Some(200 * 1024));
        assert_eq!(bytes_in("~2 MB total"), Some(2 * 1024 * 1024));
        assert_eq!(bytes_in("5.3 MB"), Some(5_557_452));
        // The base row says two numbers and the first is the one the rows above it sum to.
        assert_eq!(
            bytes_in("**≤ 75 MB uncompressed, ≤ 30 MB compressed**"),
            Some(75 * 1024 * 1024)
        );
        // A range answers at its top, because the top is the number a budget means. The first
        // number in that cell has no unit after it, which is what makes it skippable.
        assert_eq!(bytes_in("15 to 40 MB"), Some(40 * 1024 * 1024));
        assert_eq!(bytes_in("**not distributed**"), None);
    }

    #[test]
    fn a_document_number_is_not_a_budget() {
        assert_eq!(bytes_in("document 11.2: separate, not in the binary"), None);
        assert_eq!(bytes_in("§13.4"), None);
    }

    #[test]
    fn every_row_of_the_real_table_is_claimed_once() {
        let text =
            fs::read_to_string(root().join("spec/cross-compile/13-distribution.md")).unwrap();
        let table = budget_table(&text).expect("section 13.1 has a table");
        // The table the check is written against, so that a row added to it fails here as well as in
        // the task, and a row taken out of it does too.
        assert_eq!(table.len(), ROWS.len());
        for (component, _) in &table {
            let plain = plain(component);
            let matched = ROWS.iter().filter(|(key, _)| plain.starts_with(key)).count();
            assert_eq!(matched, 1, "`{plain}` is claimed {matched} times");
        }
    }

    #[test]
    fn a_size_reads_the_way_the_table_writes_one() {
        assert_eq!(mb(25 * 1024 * 1024), "25.0 MB");
        assert_eq!(mb(34_273), "33 KB");
    }
}
