//! What a release brings onto a machine, per target, as one file an auditor can read.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.5, whose last line asks for the same
//! provenance as `-print-sysroot-provenance` gives, for every target, shipped beside the binary so
//! that it can be audited without running the compiler.
//!
//! # What it can honestly say today
//!
//! Not what a produced sysroot is made of, because no release ships one yet. [`crate::Manifest`] is
//! that record and it is written by the producer that writes the files, which means it exists on a
//! machine that has the tree rather than in a release that has not got one. What a release can say
//! is what it will bring: for every target in the table, whether the sysroot for it is in the archive
//! already, is pinned as a download with a URL and a hash, is behind a licence wall and therefore
//! will never be either, or is ours to ship and nobody has published it yet.
//!
//! That is four states and they are the four sentences a person gets from the compiler when they ask
//! for a target, which is the property worth having: the file says in advance what `--fetch` and a
//! cross compile will say, so somebody deciding whether this compiler can be used inside their
//! organization does not have to run it once per target to find out.
//!
//! # Why a file in the repository rather than something the release writes
//!
//! Because the release builds a binary per host and two of those hosts cannot run what they built,
//! so a file produced by running the compiler would be produced on three machines and absent on two.
//! Generated and committed instead, the way `docs/TARGETS.md` and `tests/link-lines` already are,
//! with `cargo xtask provenance --check` in CI holding it to the tables it came from. So the file
//! is auditable in the repository as well as in the archive, and a release whose file drifted from
//! its own tables does not get built, because the check runs before the tag is packaged.
//!
//! # Why there is no parser here
//!
//! Nothing of ours reads it. The sysroot manifest has [`crate::Manifest::parse`] because an install
//! checks a tree against the one inside it, and this file has no such consumer: it is written for
//! whoever is asking what the release contains, and they have `cut` and `grep`. The format is lines
//! of tab separated fields under a version so that writing the parser they do want is ten minutes.

use std::fmt::Write as _;

use rucc_tuple::{Os, TARGETS, TargetTuple};

use crate::artifact::Pinned;
use crate::manifest::Licence;
use crate::wall::Wall;

/// The first line, which says what the file is and which version of it this is.
const HEADER: &str = "rucc distribution manifest 1";

/// Where the trees that are not in the release come from, named once in the header rather than on
/// every line that has not got a URL yet.
const PRODUCER: &str = "https://github.com/tamnd/rucc-cross";

/// How the sysroot for one target reaches the machine that compiles for it.
///
/// Four answers, and the reason this is an enum rather than an optional URL is that three of them
/// have no URL and mean entirely different things. A target nobody has published a tree for yet gets
/// one in a later release. A target behind a licence wall does not, ever, and telling the two apart
/// is the whole of `spec/cross-compile/13-distribution.md` section 13.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    /// Nothing has to arrive. The compiler's own headers are inside the binary and a freestanding
    /// target links none of a library, so the archive is the whole of what this target needs.
    Included,
    /// A sysroot artifact this release pins, by URL and by hash.
    Pinned(&'static Pinned),
    /// Behind one of the two licence walls, so there is nothing to ship and nothing to fetch, and
    /// what a person does instead is name a path they already have a licence for.
    Wall(Wall),
    /// Ours to ship, and not yet published. A later release pins it.
    Unpublished,
}

impl Arrival {
    /// How the sysroot for this target arrives.
    #[must_use]
    pub fn of(target: TargetTuple) -> Arrival {
        if let Some(wall) = Wall::of(target) {
            return Arrival::Wall(wall);
        }
        if let Some(pinned) = crate::artifact::pinned_for(&target.to_canonical_string()) {
            return Arrival::Pinned(pinned);
        }
        // Freestanding is the one row that needs no sysroot at all rather than one nobody has built.
        // `spec/cross-compile/08-sysroots.md` section 8.2 gives it nine compiler headers and no link
        // inputs, and those headers are compiled into the binary.
        if target.os() == Os::None {
            return Arrival::Included;
        }
        Arrival::Unpublished
    }

    /// The word this state is spelled with in the file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Arrival::Included => "included",
            Arrival::Pinned(_) => "pinned",
            Arrival::Wall(_) => "wall",
            Arrival::Unpublished => "unpublished",
        }
    }
}

/// The whole file, for a release named by `version`.
///
/// The version is an argument rather than this crate's own, because the number that belongs in a
/// release manifest is the release's and a crate that read its own would be right only for as long
/// as nobody ever published one of these crates on its own.
///
/// Sorted by target, which is the order the table is already in and is checked below, because a file
/// two releases apart should diff as what changed between them.
#[must_use]
pub fn render(version: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{HEADER}");
    let _ = writeln!(out, "release\t{version}");
    let _ = writeln!(out, "producer\t{PRODUCER}");
    // The one payload there is. Everything else in the archive is this file, the licence, the
    // changelog and the readme, which are documents about the release rather than inputs to a
    // compile, and the compiler's own headers are inside the binary because they are compiled into
    // it with `include_str!`.
    let _ = writeln!(out, "payload\trucc\t{}", Licence::Apache2);
    let _ = writeln!(out, "targets\t{}", TARGETS.len());
    for entry in TARGETS {
        let target = match entry.parse() {
            Ok(target) => target,
            // A row that does not parse is a bug in the table that its own tests catch. This file
            // says so rather than leaving the row out, because a manifest with a target missing
            // reads as a release that has nothing to say about it.
            Err(_) => {
                let _ = writeln!(out, "target\t{}\tunparsed", entry.tuple);
                continue;
            }
        };
        let arrival = Arrival::of(target);
        let _ = write!(out, "target\t{}\t{}", entry.tuple, arrival.as_str());
        match arrival {
            Arrival::Pinned(pinned) => {
                let _ = write!(out, "\t{}\t{}", pinned.url, pinned.sha256);
            }
            Arrival::Wall(wall) => {
                let _ = write!(out, "\t{}", wall.under());
            }
            Arrival::Included | Arrival::Unpublished => {}
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::PINNED;

    /// The file for a release, split into lines, for a test to read.
    fn lines() -> Vec<String> {
        render("0.0.0").lines().map(str::to_owned).collect()
    }

    /// The fields of the one line about this target.
    fn row(tuple: &str) -> Vec<String> {
        let wanted = format!("target\t{tuple}\t");
        let text = render("0.0.0");
        let line = text
            .lines()
            .find(|line| line.starts_with(&wanted))
            .unwrap_or_else(|| panic!("no line for {tuple}"));
        line.split('\t').map(str::to_owned).collect()
    }

    #[test]
    fn the_header_says_what_the_file_is_and_which_release_it_describes() {
        let lines = lines();
        assert_eq!(lines[0], HEADER);
        assert_eq!(lines[1], "release\t0.0.0");
        assert_eq!(lines[2], format!("producer\t{PRODUCER}"));
        assert_eq!(lines[3], "payload\trucc\tapache-2.0");
    }

    /// Every target in the table, and a count above them so that a truncated file is not a shorter
    /// table.
    #[test]
    fn there_is_a_line_for_every_target_and_the_count_says_how_many() {
        let lines = lines();
        assert_eq!(lines[4], format!("targets\t{}", TARGETS.len()));
        let rows: Vec<&String> = lines.iter().filter(|line| line.starts_with("target\t")).collect();
        assert_eq!(rows.len(), TARGETS.len());
        for (row, entry) in rows.iter().zip(TARGETS) {
            assert!(row.starts_with(&format!("target\t{}\t", entry.tuple)), "{row}");
        }
    }

    #[test]
    fn a_target_behind_a_licence_wall_says_which_licence_put_it_there() {
        // The two walls and not one word for both, because a person who may use one of them cannot
        // necessarily use the other and the licence is what decides.
        assert_eq!(row("aarch64-macos"), ["target", "aarch64-macos", "wall", "apple-sdk"]);
        assert_eq!(
            row("x86_64-windows-msvc"),
            ["target", "x86_64-windows-msvc", "wall", "microsoft-sdk"]
        );
        // And the Windows target that is ours to ship is not behind either of them, which is the
        // distinction the file exists to carry.
        assert_eq!(row("x86_64-windows-gnu"), ["target", "x86_64-windows-gnu", "unpublished"]);
    }

    #[test]
    fn a_freestanding_target_needs_nothing_and_says_so_rather_than_saying_nobody_built_it() {
        assert_eq!(row("armv7m-none-eabi")[2], "included");
        assert_eq!(row("x86_64-linux-gnu")[2], "unpublished");
    }

    /// What a row looks like once the producer has published something, which is the state the file
    /// is written for and the one no row is in today.
    #[test]
    fn a_pinned_target_carries_the_url_and_the_hash_it_is_held_to() {
        static PINNED_ROW: Pinned = Pinned {
            tuple: "x86_64-linux-musl",
            url: "https://example.invalid/rucc-sysroot-x86_64-linux-musl.tar.gz",
            sha256: "3333333333333333333333333333333333333333333333333333333333333333",
        };
        let pinned = &PINNED_ROW;
        let arrival = Arrival::Pinned(pinned);
        assert_eq!(arrival.as_str(), "pinned");
        // The same three fields the table holds, in the order a person checking a download wants
        // them: what it is, where it came from, what it has to hash to.
        let mut line = format!("target\t{}\t{}", pinned.tuple, arrival.as_str());
        line.push_str(&format!("\t{}\t{}", pinned.url, pinned.sha256));
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 5);
        assert_eq!(fields[3], pinned.url);
        assert_eq!(fields[4], pinned.sha256);
    }

    /// Today's table has no rows, so every target that is ours to ship is unpublished and the file
    /// says that rather than going quiet.
    #[test]
    fn nothing_is_pinned_yet_and_the_file_does_not_pretend_otherwise() {
        assert!(PINNED.is_empty(), "a row landed, so this test is the one to update");
        let text = render("0.0.0");
        assert!(!text.contains("\tpinned"), "{text}");
        assert!(text.contains("\tunpublished\n"), "{text}");
    }

    #[test]
    fn every_line_has_a_kind_and_no_field_is_empty() {
        for line in lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            if line == HEADER {
                continue;
            }
            assert!(
                matches!(fields[0], "release" | "producer" | "payload" | "targets" | "target"),
                "{line}"
            );
            for field in fields {
                assert!(!field.is_empty(), "an empty field in `{line}`");
            }
        }
    }
}
