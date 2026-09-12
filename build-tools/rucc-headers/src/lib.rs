//! Derives a multi-version libc header tree from real installed header sets.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.3, which calls the headers the hard half of
//! a sysroot and says why. A glibc `.so` is a few hundred kilobytes of generated stubs and nothing
//! equivalent exists for `/usr/include`, because headers are program text that has to be the same
//! text the target's libc was built against. Shipping a full copy per architecture per supported
//! release is several hundred megabytes against document 13's budget, so the per version
//! differences go inside the files as conditionals on `__GLIBC_MINOR__` and one tree serves every
//! release. Zig's `generic-glibc` is that artifact and `ziglang/universal-headers` is the technique;
//! document 01.2 records both, and records that the second of them has been unfinished for years.
//!
//! # What this is and is not
//!
//! It is the derivation, and nothing else. Given the headers that glibc installs for each release in
//! the support table, it writes one tree in which every release's text can still be read out, and it
//! checks that claim for every file before it writes anything. It does not fetch glibc, build it,
//! install its headers or decide which releases matter. Those are the producer's job and the
//! producer is `tamnd/rucc-cross`, for the reason section 8.3 gives: installing a libc means
//! fetching a libc, and a compiler checkout should not download one to run its tests.
//!
//! It is a build tool rather than a crate of the compiler because nothing in a compilation reads it.
//! The compiler's half of this is two lines in `rucc-sysroot`, the directory search order and
//! `bundled_glibc_minor`, and they are there already.
//!
//! # The four pieces
//!
//! - [`norm`] cuts a header into the pieces a conditional can go between, and says what makes two
//!   copies of one header the same. They are the same when their code is the same, because glibc
//!   moves a copyright year in every file every January and that is not a version difference.
//! - [`diff`] lines two releases of one header up.
//! - [`merge`] writes one file out of all of them, and reads it back to check it.
//! - [`tree`] does that for every file in every release and counts what happened.
//!
//! # How to run it
//!
//! ```text
//! cargo run -q -p rucc-headers -- --release 2.28=<dir> --release 2.44=<dir> --out <dir>
//! ```
//!
//! Each directory is the one a release's headers are under, so the one `usr/include` is in an
//! install ends at. The releases have to be given oldest first, and there have to be at least two,
//! because merging one release is copying it.

pub mod cond;
pub mod diff;
pub mod merge;
pub mod norm;
pub mod tree;
