//! Every file of every release, merged into one tree, with the numbers that come out of doing it.
//!
//! The per file work is in `merge.rs`. What is here is the part that makes it a tree: the union of
//! the paths, the order they are done in, the writing, and the count of what happened. The count is
//! the point as much as the tree is, because `spec/cross-compile/08-sysroots.md` section 8.3 names
//! 210 files that change somewhere between 2.28 and 2.44 and this is what turns that survey into a
//! statement about an artifact: how many files needed a conditional, how many could not be cut
//! finer than the whole file, and what the tree weighs against the installs it came from.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cond::Releases;
use crate::merge::{self, Kind};

/// One installed header tree and the release it is.
#[derive(Debug, Clone)]
pub struct Input {
    /// The glibc minor version, so 28 for 2.28.
    pub minor: u32,
    /// The directory its headers are under, which is the one `usr/include` ends at.
    pub root: PathBuf,
}

/// What one file became.
#[derive(Debug, Clone)]
pub struct Record {
    /// Where it is in the tree.
    pub path: String,
    /// What had to be done to it.
    pub kind: Kind,
    /// How many conditionals of ours are in it.
    pub branches: usize,
    /// Whether some release does not have it.
    pub guarded: bool,
    /// How many releases have it.
    pub releases: usize,
}

/// What a merge of a whole tree did.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// How the releases are spelled, for the first line of the report.
    pub releases: Vec<String>,
    /// One entry per file in the merged tree.
    pub records: Vec<Record>,
    /// What the installs weigh, added up.
    pub bytes_in: u64,
    /// What the newest install weighs on its own, which is the honest thing to compare against:
    /// shipping one release is the alternative to merging.
    pub bytes_newest: u64,
    /// What the merged tree weighs.
    pub bytes_out: u64,
    /// Everything that is wrong, which has to be empty for the tree to be worth shipping.
    pub problems: Vec<String>,
}

impl Report {
    /// How many files ended up each way.
    pub fn counted(&self, kind: Kind) -> usize {
        self.records.iter().filter(|r| r.kind == kind).count()
    }

    /// The files with the most conditionals in them, worst first.
    pub fn busiest(&self, how_many: usize) -> Vec<&Record> {
        let mut sorted: Vec<&Record> = self.records.iter().filter(|r| r.branches > 0).collect();
        sorted.sort_by(|a, b| b.branches.cmp(&a.branches).then(a.path.cmp(&b.path)));
        sorted.truncate(how_many);
        sorted
    }

    /// The record as one line per file, for a reviewer who wants to see the whole list.
    pub fn listing(&self) -> String {
        let mut out = String::new();
        for record in &self.records {
            let kind = match record.kind {
                Kind::Same => "same",
                Kind::Conditional => "merged",
                Kind::PerRelease => "per-release",
            };
            let absent = if record.guarded { "guarded" } else { "everywhere" };
            out.push_str(&format!(
                "{}\t{kind}\t{}\t{absent}\t{}\n",
                record.path, record.branches, record.releases
            ));
        }
        out
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "headers: {} releases, {}", self.releases.len(), self.releases.join(" "))?;
        writeln!(
            f,
            "headers: {} files, {} with no conditional, {} merged, {} per release, {} guarded",
            self.records.len(),
            self.counted(Kind::Same),
            self.counted(Kind::Conditional),
            self.counted(Kind::PerRelease),
            self.records.iter().filter(|r| r.guarded).count(),
        )?;
        let branches: usize = self.records.iter().map(|r| r.branches).sum();
        writeln!(f, "headers: {branches} conditionals written")?;
        writeln!(
            f,
            "headers: {} KiB, against {} KiB for the newest install alone and {} KiB for all {}",
            self.bytes_out / 1024,
            self.bytes_newest / 1024,
            self.bytes_in / 1024,
            self.releases.len(),
        )?;
        for record in self.busiest(10) {
            writeln!(f, "headers: {} has {} conditionals", record.path, record.branches)?;
        }
        Ok(())
    }
}

/// Merges every input tree into one at `out`.
///
/// The output directory has to be empty or absent, because writing a merged tree over a tree that
/// is already there leaves whatever the last run wrote and nothing says which file came from which
/// run. The producer gets a fresh directory and renames it into place, the same way
/// `rucc_driver::install` does with a sysroot.
pub fn merge_trees(inputs: &[Input], out: &Path) -> Result<Report, String> {
    let releases = Releases::new(inputs.iter().map(|i| i.minor).collect())?;
    if out.exists() && fs::read_dir(out).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Err(format!("{} is not empty, and a merge writes a whole tree", out.display()));
    }
    let mut report = Report {
        releases: (0..releases.count()).map(|n| releases.spelled(n)).collect(),
        ..Report::default()
    };

    let mut paths: BTreeSet<String> = BTreeSet::new();
    let mut found: Vec<BTreeSet<String>> = Vec::new();
    for input in inputs {
        let mut one = BTreeSet::new();
        walk(&input.root, &input.root, &mut one)
            .map_err(|why| format!("{}: {why}", input.root.display()))?;
        paths.extend(one.iter().cloned());
        found.push(one);
    }
    if paths.is_empty() {
        return Err("none of the trees has a file in it".to_owned());
    }

    for path in &paths {
        let mut texts: Vec<Option<String>> = Vec::with_capacity(inputs.len());
        for (n, input) in inputs.iter().enumerate() {
            if !found[n].contains(path) {
                texts.push(None);
                continue;
            }
            let whole = input.root.join(path);
            let text =
                fs::read_to_string(&whole).map_err(|why| format!("{}: {why}", whole.display()))?;
            report.bytes_in += text.len() as u64;
            if n + 1 == inputs.len() {
                report.bytes_newest += text.len() as u64;
            }
            texts.push(Some(text));
        }
        let given: Vec<Option<&str>> = texts.iter().map(|t| t.as_deref()).collect();
        let merged = merge::one(&releases, path, &given)?;
        report.problems.extend(merged.problems.iter().cloned());
        report.records.push(Record {
            path: path.clone(),
            kind: merged.kind,
            branches: merged.branches,
            guarded: merged.guarded,
            releases: given.iter().filter(|t| t.is_some()).count(),
        });
        report.bytes_out += merged.text.len() as u64;
        let whole = out.join(path);
        if let Some(dir) = whole.parent() {
            fs::create_dir_all(dir).map_err(|why| format!("{}: {why}", dir.display()))?;
        }
        fs::write(&whole, &merged.text).map_err(|why| format!("{}: {why}", whole.display()))?;
    }
    Ok(report)
}

/// Every file under `dir`, named relative to `root`, with forward slashes.
///
/// Sorted, because the order files are merged in is the order the report lists them in and a report
/// that depends on what order a directory happens to be read in is a report two people cannot
/// compare.
fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) -> Result<(), String> {
    let listing = fs::read_dir(dir).map_err(|why| format!("{}: {why}", dir.display()))?;
    for entry in listing {
        let entry = entry.map_err(|why| why.to_string())?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|why| format!("{}: {why}", path.display()))?;
        if kind.is_dir() {
            walk(root, &path, out)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("{} is not under {}", path.display(), root.display()))?;
        let Some(name) = relative.to_str() else {
            return Err(format!("{} is not a name this can write down", relative.display()));
        };
        out.insert(name.replace('\\', "/"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory under the temporary directory, named after the test that wanted it.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rucc-headers-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    fn write(root: &Path, path: &str, text: &str) {
        let whole = root.join(path);
        fs::create_dir_all(whole.parent().expect("a parent")).expect("the directory");
        fs::write(whole, text).expect("the file");
    }

    #[test]
    fn two_trees_become_one_and_the_report_says_what_happened() {
        let dir = scratch("two-trees");
        let (old, new, out) = (dir.join("2.28"), dir.join("2.31"), dir.join("out"));
        write(&old, "stdio.h", "int puts (const char *);\n");
        write(&new, "stdio.h", "int puts (const char *);\nint newer (void);\n");
        write(&old, "sys/gone.h", "int gone (void);\n");
        write(&new, "sys/here.h", "int here (void);\n");
        write(&old, "same.h", "int same (void);\n");
        write(&new, "same.h", "int same (void);\n");

        let inputs = vec![Input { minor: 28, root: old }, Input { minor: 31, root: new.clone() }];
        let report = merge_trees(&inputs, &out).expect("a merge");
        assert_eq!(report.problems, Vec::<String>::new());
        assert_eq!(report.records.len(), 4);
        // Three files needed no conditional: the one nobody changed and the two only one release
        // has, which are wrapped in a guard and are otherwise a copy.
        assert_eq!(report.counted(Kind::Same), 3);
        assert_eq!(report.records.iter().filter(|r| r.guarded).count(), 2);

        // Every file of both trees is in the output, and the one nobody changed is a copy.
        assert_eq!(fs::read_to_string(out.join("same.h")).expect("written"), "int same (void);\n");
        let here = fs::read_to_string(out.join("sys/here.h")).expect("written");
        assert!(here.contains("#error"), "{here}");
        assert!(here.contains("int here (void);"), "{here}");
        assert_eq!(report.bytes_newest, newest_bytes(&new));
        let _ = fs::remove_dir_all(&dir);
    }

    /// What one install weighs, which the report has to agree with.
    fn newest_bytes(root: &Path) -> u64 {
        let mut paths = BTreeSet::new();
        walk(root, root, &mut paths).expect("a walk");
        paths.iter().map(|p| fs::metadata(root.join(p)).expect("there").len()).sum()
    }

    #[test]
    fn a_tree_already_there_is_not_written_over() {
        let dir = scratch("not-over");
        let (old, new, out) = (dir.join("2.28"), dir.join("2.31"), dir.join("out"));
        write(&old, "a.h", "int a (void);\n");
        write(&new, "a.h", "int a (void);\n");
        write(&out, "leftover.h", "from the last run\n");
        let inputs = vec![Input { minor: 28, root: old }, Input { minor: 31, root: new }];
        let why = merge_trees(&inputs, &out).expect_err("it is not empty");
        assert!(why.contains("is not empty"), "{why}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_release_is_not_a_merge() {
        let dir = scratch("one-release");
        let root = dir.join("2.28");
        write(&root, "a.h", "int a (void);\n");
        let why = merge_trees(&[Input { minor: 28, root }], &dir.join("out"))
            .expect_err("one release is a copy");
        assert!(why.contains("at least two releases"), "{why}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_listing_has_one_line_per_file() {
        let dir = scratch("listing");
        let (old, new, out) = (dir.join("2.28"), dir.join("2.31"), dir.join("out"));
        write(&old, "a.h", "int a (void);\n");
        write(&new, "a.h", "int a (void);\nint b (void);\n");
        let inputs = vec![Input { minor: 28, root: old }, Input { minor: 31, root: new }];
        let report = merge_trees(&inputs, &out).expect("a merge");
        assert_eq!(report.listing(), "a.h\tmerged\t1\teverywhere\t2\n");
        assert!(format!("{report}").contains("1 files, 0 with no conditional, 1 merged"));
        let _ = fs::remove_dir_all(&dir);
    }
}
