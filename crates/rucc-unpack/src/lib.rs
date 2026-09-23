//! The container formats a Windows SDK arrives in, read.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4. Layer rank 0, see
//! `spec/18-package-layout.md` section 18.2.
//!
//! # Why this crate exists
//!
//! Because the headers and libraries an MSVC target needs are four formats deep. `rucc-sysroot`
//! reads Microsoft's manifests and says which files a compiler needs, the driver downloads
//! them, and what lands on disk is vsix archives and MSIs. A vsix is a zip. An MSI is a compound
//! file holding a small relational database, and the database says which cabinet each of its files
//! is in and what it is called there. A cabinet is its own container again, and its contents are
//! compressed with MSZIP. So getting from a download to a `crt/include` directory is a zip reader, a
//! compound file reader, a table reader and a cabinet reader, and one decompressor under the two
//! ends of that.
//!
//! # Why it is ours rather than a dependency
//!
//! `spec/18-package-layout.md` section 18.3 is the budget, and the sysroot fetch is the precedent it
//! was written from: that one shells out to `curl` and `tar`, which are programs every host in the
//! support table already has, and so it costs nothing. The same question asked here has a different
//! answer. No host has a program that reads a compound file, the SDK's cabinets are named by opaque
//! hashes so the MSI is the only thing that can say which cabinet holds which header, and a
//! cross compiler that needs a package manager run before it can target Windows is not a cross
//! compiler. So these readers are ours, the same as the `ar` writer next door at rank 0 is.
//!
//! What that costs is bounded in a way that a dependency is not. These are finished formats. Deflate
//! was finished in 1996, the cabinet format in 1997, and compound files predate both. Nothing here
//! will need to be kept current.
//!
//! # What is here
//!
//! [`inflate`] is the decompressor, and both containers are built on it: a zip's ordinary method is
//! deflate and a cabinet's ordinary method is MSZIP, which is deflate with two bytes in front of
//! each block. [`zip`] reads the container a vsix is and [`cab`] the one an MSI keeps its bytes in.
//! [`cfb`] reads the compound file an MSI is, which is to say it gets the streams out of one by
//! name, and [`msi`] reads the tables in those streams, which is what says which cabinet holds
//! which header and what the cabinet calls it. All four readers are here. What is not here is the
//! driver that puts the four together and lays a sysroot out, which is not this crate's business.
//!
//! Reading only. Nothing in this compiler writes a zip or a cabinet, and if something ever does it
//! will not be this crate's business, the same way `rucc-archive` writes the one container a linker
//! reads and does not read it back.
//!
//! [`inflate`]: mod@inflate

#![doc(html_root_url = "https://docs.rs/rucc-unpack/0.11.1")]

pub mod cab;
pub mod cfb;
pub mod inflate;
pub mod msi;
pub mod zip;

use std::path::{Component, Path, PathBuf};

pub use cab::{Cab, CabError};
pub use cfb::{Cfb, CfbError};
pub use inflate::{InflateError, inflate, inflate_into};
pub use msi::{Msi, MsiError, Payload, Table};
pub use zip::{Member, Zip, ZipError};

/// Where a name out of an archive is allowed to be written, under `root`.
///
/// [`None`] for a name that would land anywhere else, which is the only answer that is safe to give.
/// A name in an archive is not a path, it is a string somebody else chose, and the three ways it can
/// leave the directory it was supposed to go in are an absolute path, a `..` that climbs out, and a
/// Windows drive or UNC prefix. Every one of them has been a real vulnerability in a real unpacker,
/// and the reason to answer the question here rather than where a file gets written is that there
/// will be three callers of this and one of them would get it wrong.
///
/// Backslashes count as separators. A cabinet spells its paths with them, so a name that means a
/// subdirectory on Windows would be one long file name here, and a `..\..` would climb out on a host
/// that took the backslash literally only after somebody had already copied the archive to Windows.
///
/// ```
/// # use std::path::Path;
/// # use rucc_unpack::under;
/// let root = Path::new("/tmp/sdk");
/// assert_eq!(under(root, "include/stdio.h"), Some(root.join("include/stdio.h")));
/// assert_eq!(under(root, r"include\um\windows.h"), Some(root.join("include/um/windows.h")));
/// assert_eq!(under(root, "../../etc/passwd"), None);
/// assert_eq!(under(root, "/etc/passwd"), None);
/// ```
#[must_use]
pub fn under(root: &Path, name: &str) -> Option<PathBuf> {
    // A drive letter or a UNC prefix is only a prefix component on a Windows host, so on every other
    // host it would pass through as an ordinary name with a colon in it. Checked by hand for that
    // reason, rather than left to the platform's own idea of what a path is.
    if name.starts_with('/') || name.starts_with('\\') || name.get(1..2) == Some(":") {
        return None;
    }
    let mut out = root.to_path_buf();
    let mut deep = 0usize;
    for part in name.split(['/', '\\']) {
        match Path::new(part).components().next() {
            // An empty part is a doubled separator or a trailing one, and a single dot is the
            // directory it is already in. Neither moves anywhere, so neither is worth refusing.
            None | Some(Component::CurDir) => {}
            Some(Component::Normal(part)) => {
                out.push(part);
                deep += 1;
            }
            // Everything else is a way out: `..`, a root, or a prefix. A `..` that stays inside is
            // harmless, but nothing produces one on purpose, and a name that needs it is a name
            // worth looking at rather than quietly resolving.
            _ => return None,
        }
    }
    (deep > 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::under;
    use std::path::Path;

    #[test]
    fn an_ordinary_name_lands_under_the_root() {
        let root = Path::new("/tmp/sdk");
        assert_eq!(under(root, "stdio.h"), Some(root.join("stdio.h")));
        assert_eq!(under(root, "ucrt/stdio.h"), Some(root.join("ucrt/stdio.h")));
        // Which is how a cabinet spells the same thing.
        assert_eq!(under(root, r"ucrt\stdio.h"), Some(root.join("ucrt/stdio.h")));
        // A doubled separator and a trailing dot are noise rather than an escape.
        assert_eq!(under(root, "ucrt//stdio.h"), Some(root.join("ucrt/stdio.h")));
        assert_eq!(under(root, "./ucrt/./stdio.h"), Some(root.join("ucrt/stdio.h")));
    }

    #[test]
    fn a_name_that_climbs_out_is_refused_however_it_is_spelled() {
        let root = Path::new("/tmp/sdk");
        assert_eq!(under(root, ".."), None);
        assert_eq!(under(root, "../etc/passwd"), None);
        assert_eq!(under(root, r"..\etc\passwd"), None);
        assert_eq!(under(root, "ucrt/../../etc/passwd"), None);
        // And one that stays inside on the arithmetic is refused too, because nothing writes one on
        // purpose and the cost of being wrong here is somebody else's file.
        assert_eq!(under(root, "ucrt/../stdio.h"), None);
    }

    #[test]
    fn an_absolute_name_is_refused_in_both_spellings_on_every_host() {
        let root = Path::new("/tmp/sdk");
        assert_eq!(under(root, "/etc/passwd"), None);
        assert_eq!(under(root, r"\Windows\System32\kernel32.dll"), None);
        // A drive letter and a UNC path are only prefixes on Windows, so they are caught by hand
        // and the answer is the same wherever this runs.
        assert_eq!(under(root, r"C:\Windows\System32\kernel32.dll"), None);
        assert_eq!(under(root, r"\\server\share\thing"), None);
    }

    #[test]
    fn a_name_that_names_nothing_is_refused() {
        let root = Path::new("/tmp/sdk");
        assert_eq!(under(root, ""), None);
        assert_eq!(under(root, "."), None);
        // A directory entry in a zip, which is a name with nothing after the last separator. It
        // names a directory rather than a file, so a caller that wants to make it asks for the
        // parent of something instead.
        assert_eq!(under(root, "ucrt/"), Some(root.join("ucrt")));
        assert_eq!(under(root, "/"), None);
    }
}
