//! What this release pins: one sysroot artifact per target, by URL and by hash.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.2, which says every downloaded
//! artifact has a hash pinned in the rucc release, checked before use, with a mismatch being a hard
//! failure and no flag to get past it. Section 13.8 divides the work in three and the other two are
//! written: [`crate::fetch`] moves the bytes with a program the machine already has, and
//! [`crate::install`] decides whether what arrived is the right tree. This is the third, which is
//! the statement of what the right one is, and it is the half that makes the other two mean
//! anything.
//!
//! # Why the table is in the binary
//!
//! Because a hash that travels with the artifact is not a pin, and a hash in a file beside the
//! compiler is a hash whoever replaces the artifact can replace too. The release is the authority
//! for what an artifact of that release is, so the table is compiled into the release, which also
//! means an upgrade can change a URL without anything on the machine having to be told.
//!
//! It is a table rather than a computed URL for the same reason. A name built out of a version and
//! a tuple looks tidier and quietly says that every target's artifact is at a predictable address
//! forever, which is a promise about somebody else's file server. A row per target costs three
//! strings and says only what is true.
//!
//! # Why it is empty
//!
//! Because nothing has published a sysroot artifact yet. The producer is in `tamnd/rucc-cross`, per
//! document 08.7, and the table cannot honestly name a URL and a hash before there is a file at one
//! with the other. So [`PINNED`] has no rows today, every `--fetch` says so by name, and the test at
//! the bottom of this file is what the first row will be held to when it is added.

use std::path::{Path, PathBuf};

/// One artifact: the sysroot for one target, as this release pins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pinned {
    /// The target it is the sysroot for, in the spelling that names its directory under the cache.
    pub tuple: &'static str,
    /// Where to get it. Handed to a downloader as it stands, and nothing here builds it out of
    /// parts.
    pub url: &'static str,
    /// The sha256 of the archive, lowercase hex, which is what the bytes that arrive are held to.
    pub sha256: &'static str,
}

impl Pinned {
    /// The name to write the archive under, which is the last component of the URL.
    ///
    /// The URL's own name rather than one built out of the tuple, so that the file on disk is the
    /// file the server served and a person comparing the two is comparing names as well as bytes.
    #[must_use]
    pub fn file_name(&self) -> &'static str {
        self.url.rsplit('/').next().unwrap_or(self.url)
    }

    /// Where in the cache the archive is kept.
    ///
    /// Under the cache rather than in a temporary directory, because a machine with no downloader is
    /// told this exact path and a second `--fetch` carries on from the check, which is section 13.8's
    /// answer for a host that cannot reach the network at all. It is kept after the install for the
    /// same reason and for one more: a fetch of a target that is already installed then moves
    /// nothing and says so.
    #[must_use]
    pub fn archive_in(&self, cache: &Path) -> PathBuf {
        cache.join("downloads").join(self.file_name())
    }
}

/// Every artifact this release pins, in tuple order.
///
/// Empty, for the reason the module documentation gives. A row is three strings and the test below
/// says what they have to be.
pub const PINNED: &[Pinned] = &[];

/// The artifact this release pins for `tuple`, if it pins one.
///
/// The canonical spelling is what a row is named by, so the caller parses what the user wrote and
/// asks with the tuple's own text rather than with theirs.
#[must_use]
pub fn pinned_for(tuple: &str) -> Option<&'static Pinned> {
    look(PINNED, tuple)
}

/// Every target this release pins an artifact for, for a message that has to say what there is.
#[must_use]
pub fn pinned_targets() -> Vec<&'static str> {
    PINNED.iter().map(|what| what.tuple).collect()
}

/// The same lookup over a table that is passed in, so the cases are testable while [`PINNED`] has no
/// rows in it.
fn look<'a>(table: &'a [Pinned], tuple: &str) -> Option<&'a Pinned> {
    table.iter().find(|what| what.tuple == tuple)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rucc_tuple::TargetTuple;

    use super::*;

    /// A table with rows in it, which is what [`PINNED`] will look like.
    const TABLE: &[Pinned] = &[
        Pinned {
            tuple: "aarch64-linux-musl",
            url: "https://example.invalid/rucc-sysroot-aarch64-linux-musl.tar.gz",
            sha256: "1111111111111111111111111111111111111111111111111111111111111111",
        },
        Pinned {
            tuple: "x86_64-linux-musl",
            url: "https://example.invalid/rucc-sysroot-x86_64-linux-musl.tar.gz",
            sha256: "2222222222222222222222222222222222222222222222222222222222222222",
        },
    ];

    #[test]
    fn a_target_the_table_names_is_found_and_one_it_does_not_is_not() {
        let found = look(TABLE, "x86_64-linux-musl").expect("the table has that one");
        assert_eq!(found.sha256, TABLE[1].sha256);
        assert_eq!(look(TABLE, "riscv64-linux-gnu"), None);
    }

    /// A tuple that starts with one the table has is a different target and not a match.
    #[test]
    fn a_longer_tuple_is_not_the_row_it_begins_with() {
        assert_eq!(look(TABLE, "x86_64-linux-musl.1.2.5"), None);
        assert_eq!(look(TABLE, "x86_64-linux"), None);
    }

    #[test]
    fn the_archive_is_named_by_the_url_and_kept_under_the_cache() {
        let what = TABLE[0];
        assert_eq!(what.file_name(), "rucc-sysroot-aarch64-linux-musl.tar.gz");
        assert_eq!(
            what.archive_in(&PathBuf::from("/tmp/cache")),
            PathBuf::from("/tmp/cache/downloads/rucc-sysroot-aarch64-linux-musl.tar.gz")
        );
    }

    /// What every row of [`PINNED`] has to be, which passes today because there are none.
    ///
    /// Left as a test rather than as a comment above the table, because the day somebody adds a row
    /// is the day the rules stop being obvious, and a pasted hash with a capital letter in it or a
    /// tuple spelled the way the URL spells it would otherwise be found by a user.
    #[test]
    fn every_row_is_a_target_a_url_and_a_hash() {
        for what in PINNED {
            let tuple: TargetTuple =
                what.tuple.parse().unwrap_or_else(|why| panic!("{}: {why}", what.tuple));
            assert_eq!(
                tuple.to_canonical_string(),
                what.tuple,
                "a row is named by the canonical spelling, because that is what names the \
                 directory the tree is installed at"
            );
            assert!(what.url.starts_with("https://"), "{}: {}", what.tuple, what.url);
            // A query string or a fragment would make the file name something other than the last
            // component of the URL, which is the one thing the name is read out of.
            assert!(!what.url.contains('?') && !what.url.contains('#'), "{}", what.url);
            assert!(!what.file_name().is_empty(), "{} ends with a separator", what.url);
            assert_eq!(what.sha256.len(), 64, "{}: {}", what.tuple, what.sha256);
            assert!(
                what.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{}: {} is not lowercase hex, and the check compares text",
                what.tuple,
                what.sha256
            );
        }
        let mut sorted: Vec<&str> = pinned_targets();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, pinned_targets(), "the rows are in tuple order and each target once");
    }
}
