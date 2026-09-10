//! Where a target's headers and link inputs live, which directories are searched for them, and in
//! what order they reach the linker.
//!
//! Design: `spec/cross-compile/08-sysroots.md`, and section 8.5 for the rule this crate exists to
//! enforce.
//!
//! # The claim this crate is responsible for
//!
//! `spec/cross-compile/02-the-goal.md` claim 5 is byte identical output from different hosts for
//! the same target. It holds only because the search path rules make the libc header directory a
//! function of the target rather than of the machine, and that function is here.
//!
//! So every answer in this crate is derived from a [`TargetTuple`] and from paths the caller
//! supplies. Nothing reads an environment variable, nothing looks at `std::env::consts`, and
//! nothing touches the filesystem. A host directory can only enter through
//! [`Options::host_include`], which [`include_paths`] uses exactly once and only when the target
//! is the host, and that is checkable by reading one function.
//!
//! # What is in here
//!
//! [`Sysroot`] is the directory layout for one target: where its headers go, where its link inputs
//! go, and where the record of what they are goes. Its root is a function of a cache directory and
//! the tuple, which is what makes the tuple a cache key.
//!
//! [`include_paths`] is section 8.5 as an ordered list, with each entry saying which of the four
//! steps put it there. [`LinkLine`] is the start files, the libraries and the end files for a musl
//! link, in the order a linker needs them.
//!
//! [`Manifest`] is what a produced sysroot carries: every input with where it came from, its hash
//! and its licence. Two sysroots for the same target built on two hosts have the same manifest, and
//! comparing manifests is how that gets checked without comparing several thousand files.
//!
//! # What is not in here
//!
//! Nothing fetches. Downloading musl, verifying it and unpacking it is
//! `spec/cross-compile/13-distribution.md`, and it needs a cache, a provenance record and a network
//! policy that this crate deliberately has no opinion about. What this crate settles is where the
//! result goes and how it is searched, which is the part that has to be decided before anything is
//! worth downloading.
//!
//! ```
//! use rucc_sysroot::{Sysroot, LinkLine, LinkMode};
//! use rucc_tuple::TargetTuple;
//! use std::path::Path;
//!
//! let target: TargetTuple = "aarch64-linux-musl".parse().unwrap();
//! let sysroot = Sysroot::in_cache(Path::new("/cache"), target);
//!
//! // The cache key is the canonical spelling, so two hosts asking for the same target ask for
//! // the same directory.
//! assert_eq!(sysroot.cache_key(), "aarch64-linux-musl");
//! assert_eq!(sysroot.root(), Path::new("/cache/sysroots/aarch64-linux-musl"));
//!
//! // A static link needs three start files, and `crtn.o` goes after the libraries rather than
//! // with the other two.
//! let line = LinkLine::musl(&sysroot, LinkMode::Static);
//! assert_eq!(line.start.last().unwrap().file_name().unwrap(), "crti.o");
//! assert_eq!(line.end.first().unwrap().file_name().unwrap(), "crtn.o");
//! assert!(line.flags.iter().any(|flag| flag == "-static"));
//! ```

#![doc(html_root_url = "https://docs.rs/rucc-sysroot/0.10.16")]
// Every public item here is read by somebody bringing up a target, and an undocumented one is a
// question they have to answer by reading the body.
#![deny(missing_docs)]

pub mod layout;
pub mod link;
pub mod manifest;
pub mod search;

pub use layout::Sysroot;
pub use link::{LinkLine, LinkMode};
pub use manifest::{Input, Licence, Manifest, ManifestError};
pub use search::{Entry, Options, Origin, include_paths};

#[doc(inline)]
pub use rucc_tuple::TargetTuple;
