//! The libc a linker reads, synthesized from a description of what it exports.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md`, and section 9.8 for this crate's three
//! correctness properties.
//!
//! # The one sentence this crate rests on
//!
//! At link time a shared library is only a list of symbol names, their types, their sizes and their
//! versions, and the code is irrelevant. So the linker's input can be synthesized from a
//! description, the description is small, and a cross compiler does not have to carry a copy of
//! every libc it targets. This is the technique `zig cc` is built on and section 9.1 is where rucc
//! adopts it.
//!
//! What it buys is the thing people actually cross compile for, which is building on a modern
//! machine and running on an old one. That is not a side effect of the scheme, it is the scheme,
//! and it is bought by getting symbol versions right.
//!
//! # What is here and what is not
//!
//! [`Library`] is a description: a `SONAME`, a `DT_NEEDED` chain and a list of [`Symbol`]. [`write()`]
//! turns one into an ELF shared object for a target. Between them they cover the unversioned case,
//! which is musl and the BSDs.
//!
//! [`compat()`] is section 9.9: the libraries a link line names that the libc does not have separately
//! any more. Build systems pass `-lm` and `-lpthread` whether or not there is anything in them, so the
//! files have to exist, and what they have to be differs between the two libcs in more than spelling.
//! glibc wants empty shared objects carrying the `SONAME` the loader will go looking for, and musl
//! wants empty archives, which record no dependency at all because there is no such file on a musl
//! system. The module is a list of names and forms rather than of contents, since what a library
//! exports is a question for its description.
//!
//! glibc's version nodes are not here yet and they are the hard half. glibc exports several
//! implementations of one name under different versions, `memcpy@GLIBC_2.2.5` beside
//! `memcpy@GLIBC_2.14`, and a link picks the highest node not exceeding the target's glibc version.
//! That is why `spec/cross-compile/03-target-model.md` puts `env_version` in the tuple and why
//! `x86_64-linux-gnu.2.28` is a different target from `x86_64-linux-gnu.2.39`. Adding it means real
//! `.gnu.version_d` records rather than versioned spellings of names, and it is tracked separately.
//!
//! The import libraries Windows wants are section 9.4, a different container, and also not here.
//! Darwin needs nothing from this crate at all: Apple ships `.tbd` files, which are section 9.1's
//! technique adopted by the platform vendor, so per section 9.7 they are consumed from the SDK
//! rather than generated.
//!
//! # Getting it wrong
//!
//! Section 9.1 has a table of seven things a stub must get exactly right, and the reason the table
//! is worth reading is that only five of the seven are loud. A symbol that should be there and is
//! not gives a link error, which is the good outcome. The two quiet ones are the symbol version,
//! which resolves to the wrong implementation or fails at load time with a message about a version
//! nobody has heard of, and the size of an object symbol, where a copy relocation copies the wrong
//! number of bytes into a program that linked without a word of complaint.
//!
//! The real instance is section 9.2's `_STAT_VER` story. zig's stubs and its headers disagreed
//! about a constant, and the result compiled, linked, and failed only on old glibc. The general
//! form, which is what shapes the testing here: any symbol whose correct use depends on a constant
//! defined in the headers has to be validated by the headers and the stub together rather than
//! separately, and there is no static substitute for running it.
//!
//! # What is checkable without a target machine
//!
//! Two of section 9.8's three properties, and they are what `tests/` holds.
//!
//! Determinism is the same description and the same tuple producing byte identical output on every
//! host, which is a direct component of claim 5 of `spec/cross-compile/02-the-goal.md`. The way it
//! is kept is that [`write()`] sorts the symbols itself and there is no hash map in the crate, so the
//! order a caller happened to assemble a description in cannot reach the bytes.
//!
//! Round trip is the written stub, parsed back, reproducing the description exactly. The test reads
//! the bytes by offset from the ELF specification rather than through anything in this crate, which
//! catches an offset computed one way and read back another.
//!
//! What it does not catch is the writer and the reader being wrong together, and that is not a
//! hypothetical. The first version of both used four byte `.hash` entries on every architecture, the
//! round trip agreed with itself, and `llvm-readelf` pointed out that 64-bit s390 uses eight. A test
//! whose reader was written by whoever wrote the writer checks the encoding and not the belief
//! behind it, so running a reader from outside the workspace over the output is worth more than it
//! looks, and it is why [`elf::write`] is kept down to what a reference actually says.
//!
//! The third property is comparison against a real distribution `libc.so`, which section 9.8 calls
//! the single highest value test in the document because it validates the description against
//! reality per architecture without executing anything. It needs a real library to compare with, so
//! it belongs to the glibc work rather than to the writer.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is tier 3:
//! its Rust API is explicitly unstable and will change without a major version bump. Depend on the
//! `rucc` binary's behaviour, not on this.
//!
//! ```
//! use rucc_stub::{Library, Symbol};
//! use rucc_tuple::TargetTuple;
//!
//! let target: TargetTuple = "x86_64-linux-musl".parse().unwrap();
//!
//! let mut libc = Library::new("libc.so");
//! libc.function("printf").function("malloc").object("environ", 8);
//!
//! let stub = rucc_stub::write(&libc, target).unwrap();
//! assert_eq!(&stub[..4], b"\x7fELF");
//!
//! // Nothing about the order the description was assembled in reaches the bytes, which is what
//! // claim 5 of spec/cross-compile/02-the-goal.md needs from this crate.
//! let mut backwards = Library::new("libc.so");
//! backwards.object("environ", 8).function("malloc").function("printf");
//! assert_eq!(rucc_stub::write(&backwards, target).unwrap(), stub);
//!
//! // A function with a size, or an object without one, is a description that was assembled
//! // wrongly, and it is worth hearing about while there is still somebody to tell.
//! let mut wrong = Library::new("libc.so");
//! wrong.export(Symbol { size: 4, ..Symbol::function("printf") });
//! assert!(rucc_stub::write(&wrong, target).is_err());
//! ```

#![doc(html_root_url = "https://docs.rs/rucc-stub/0.10.5")]
// Every public item here is read by somebody bringing up a target who has an ELF reference open
// beside it, so an undocumented one is a question they have to answer by reading the body.
#![deny(missing_docs)]

pub mod compat;
pub mod describe;
pub mod elf;

pub use compat::{Compat, Form, compat};
pub use describe::{Binding, Error, Kind, Library, Symbol};
pub use elf::write;

#[doc(inline)]
pub use rucc_tuple::TargetTuple;

/// The milestone in `spec/cross-compile/15-plan.md` that fills this crate in.
pub const MILESTONE: &str = "M9";
