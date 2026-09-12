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
//! [`Kernel`] is the other half of a Linux target's headers. `linux/` and `asm/` are the system
//! call interface rather than the C library, 31 of glibc's installed headers and 3 of musl's
//! include one of them, and they are the same files for every target that shares an architecture.
//! So they sit in the cache rather than in a sysroot and a Linux target searches four directories.
//!
//! [`bundled_glibc_minor`] is the version half of the same tree. One tree serves every glibc
//! release, with the differences inside the files as `#if __GLIBC_MINOR__ >= n`, so the release is
//! what the target supplies and the compiler defines the macro. It answers [`None`] for every libc
//! that has no such macro and an error for a release newer than the tree, which is the one direction
//! that cannot be approximated.
//!
//! [`include_paths`] is section 8.5 as an ordered list, with each entry saying which of the four
//! steps put it there. [`LinkLine`] is the start files, the libraries and the end files, in the
//! order a linker needs them, for either libc.
//!
//! [`argv`] is `spec/cross-compile/11-linking.md` section 11.3: the whole linker command line as a
//! function of the target, the sysroot and what the user asked for. It is the same division as the
//! one above, one level up. [`LinkLine`] is what has to be linked, which is a fact about the target,
//! and [`argv`] is how that is spelled for a linker, which is a fact about the linker. Section 11.3
//! asks for a golden file per target and `tests/link-lines` is it, one file per target, regenerated
//! by `cargo xtask link-lines` and checked in CI.
//!
//! [`Manifest`] is what a produced sysroot carries: every input with where it came from, its hash
//! and its licence. Two sysroots for the same target built on two hosts have the same manifest, and
//! comparing manifests is how that gets checked without comparing several thousand files.
//! [`Manifest::digest`] is the same comparison in one line, which is what the cache layout of
//! `spec/cross-compile/13-distribution.md` section 13.2 wanted a hash in a directory's name for.
//!
//! [`sha256`] is how that digest is computed, and it is public because the digest is not the only
//! thing that needs it. An artifact a downloader just wrote is checked against the hash pinned in
//! the release before anything is unpacked, and the files that come out of it are checked against
//! the manifest inside it, which is section 13.8's division of a fetch into the transport and the
//! part that decides whether the result is correct.
//!
//! # What is not in here
//!
//! Nothing fetches. Downloading musl, verifying it and unpacking it is
//! `spec/cross-compile/13-distribution.md`, and the network policy it needs is that document's
//! section 13.8: the bytes are moved by a downloader the machine already has, and the hash check,
//! the manifest and the rename into place are ours. None of those three is in this crate either.
//! What this crate settles is where the result goes and how it is searched, which is the part that
//! has to be decided before anything is worth downloading.
//!
//! ```
//! use rucc_sysroot::{Sysroot, LinkLine, LinkMode, argv};
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
//!
//! // And the whole linker command line, which names nothing on this machine.
//! let options = argv::Invocation { mode: LinkMode::Static, ..Default::default() };
//! let line = argv::argv(target, &sysroot, &options).unwrap();
//! assert!(line.contains(&"-static".to_owned()));
//! assert!(line.contains(&"-m".to_owned()) && line.contains(&"aarch64linux".to_owned()));
//! ```

#![doc(html_root_url = "https://docs.rs/rucc-sysroot/0.10.25")]
// Every public item here is read by somebody bringing up a target, and an undocumented one is a
// question they have to answer by reading the body.
#![deny(missing_docs)]

pub mod argv;
pub mod layout;
pub mod link;
pub mod manifest;
pub mod search;
pub mod sha256;

pub use argv::{Invocation, Item, Unsupported};
pub use layout::{BUNDLED_GLIBC, GlibcSkew, Kernel, Sysroot, bundled_glibc_minor};
pub use link::{Libc, LinkLine, LinkMode, libc};
pub use manifest::{Input, Licence, Manifest, ManifestError, Provenance};
pub use search::{Entry, Options, Origin, include_paths};

#[doc(inline)]
pub use rucc_tuple::TargetTuple;
