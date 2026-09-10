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
//! turns one into an ELF shared object for a target. A [`Symbol`] may carry a [`Version`], and
//! between them they cover both libcs: musl and the BSDs, which have no version nodes at all, and
//! glibc, which has them on nearly everything.
//!
//! [`compat()`] is section 9.9: the libraries a link line names that the libc does not have separately
//! any more. Build systems pass `-lm` and `-lpthread` whether or not there is anything in them, so the
//! files have to exist, and what they have to be differs between the two libcs in more than spelling.
//! glibc wants empty shared objects carrying the `SONAME` the loader will go looking for, and musl
//! wants empty archives, which record no dependency at all because there is no such file on a musl
//! system. The module is a list of names and forms rather than of contents, since what a library
//! exports is a question for its description.
//!
//! Version nodes are the glibc half and the hard one. glibc exports several implementations of one
//! name under different versions, `memcpy@GLIBC_2.2.5` beside `memcpy@GLIBC_2.14`, and a link picks
//! the highest node not exceeding the target's glibc version. That is why
//! `spec/cross-compile/03-target-model.md` puts `env_version` in the tuple and why
//! `x86_64-linux-gnu.2.28` is a different target from `x86_64-linux-gnu.2.39`. [`Symbol::at`] and
//! [`Symbol::behind`] say which definition of a name an unversioned reference takes and which ones
//! stay exported for programs that were linked against them, and [`write()`] turns that into real
//! `.gnu.version` and `.gnu.version_d` tables rather than versioned spellings of names.
//!
//! [`abilist`] is where a glibc description comes from, which is section 9.2. glibc checks in one file
//! per architecture naming every symbol and node it exports, and it is the file glibc itself treats
//! as the ABI rather than a description of one. What it does not record is the binding, since its
//! generator accepts a global and a weak symbol through the same guard and keeps neither letter, so
//! every symbol read from one comes back global. That is section 9.1's last row and it is the gap to
//! close next, because the stub cannot be right about a weak symbol until it comes from somewhere
//! else.
//!
//! [`blob`] is how that description is carried, which is the rest of section 9.2. The eight
//! `abilist` files for the architectures rucc targets are 22712 lines and 589828 bytes of text
//! between them, with 3000 distinct names, so a blob stores each name once and refers to it by index,
//! and the eight of them come to 72316 bytes against the megabyte document 13 budgets. It is also one
//! file for every glibc version rather than one per version, because a line says which node a name was
//! added at: what glibc 2.28 exported is what the current file says with the nodes above `GLIBC_2.28`
//! dropped, and dropping them is [`blob::Blob::exports_at`]. That is what makes document 13 section
//! 13.3's argument work, which is that stubs are generated rather than bundled because eight
//! architectures times a dozen glibc versions is a cross product nobody can ship.
//!
//! [`def`] is the Windows half, which is section 9.4, and it is the same trick with a different
//! vocabulary. mingw-w64 checks in a module definition file per system DLL, 2124 of them, naming
//! every export, and that file is where a Windows description comes from for the same reason an
//! `abilist` is where a glibc one comes from. There are no versions anywhere in it, which makes the
//! reading easy, and there are two things that have to be right instead: the i386 `__stdcall`
//! decoration, which is in the names as written, and whether an export is code or data, because a
//! program that reaches data through a thunk dereferences instructions. The container the names go
//! into is a COFF archive rather than an ELF file, so it is not [`elf`] with different constants and
//! it is not here yet.
//!
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
//!
//! // glibc, where one name has two implementations. `at` is the one a plain `memcpy` reference
//! // binds to and `behind` is the older one, which stays exported so that a program linked years
//! // ago keeps the behaviour it was built against.
//! let glibc: TargetTuple = "x86_64-linux-gnu".parse().unwrap();
//! let mut libc6 = Library::new("libc.so.6");
//! libc6
//!     .export(Symbol::function("memcpy").at("GLIBC_2.14"))
//!     .export(Symbol::function("memcpy").behind("GLIBC_2.2.5"));
//! assert!(rucc_stub::write(&libc6, glibc).is_ok());
//!
//! // Two definitions that both take unversioned references leave a plain `memcpy` with no single
//! // answer, so it is refused rather than written and resolved by whichever the linker saw first.
//! let mut both = Library::new("libc.so.6");
//! both
//!     .export(Symbol::function("memcpy").at("GLIBC_2.14"))
//!     .export(Symbol::function("memcpy").at("GLIBC_2.2.5"));
//! assert!(rucc_stub::write(&both, glibc).is_err());
//!
//! // The same thing read out of glibc's own file rather than written by hand. Four lines in the
//! // shape an `abilist` has, two of them two implementations of one name.
//! let text = "\
//! GLIBC_2.2.5 printf F
//! GLIBC_2.2.5 environ D 0x8
//! GLIBC_2.2.5 memcpy F
//! GLIBC_2.14 memcpy F
//! ";
//! let exports = rucc_stub::abilist::read(text).unwrap();
//! assert_eq!(exports.symbols.len(), 4);
//!
//! // The file does not say which `memcpy` a plain reference takes, because its generator strips the
//! // parentheses objdump marks the superseded definition with. The highest node is the answer.
//! let node = |symbol: &Symbol| symbol.version.as_ref().unwrap().node.clone();
//! let default = exports
//!     .symbols
//!     .iter()
//!     .find(|symbol| symbol.name == "memcpy" && symbol.version.as_ref().unwrap().default)
//!     .unwrap();
//! assert_eq!(node(default), "GLIBC_2.14");
//!
//! let mut read = Library::new("libc.so.6");
//! read.symbols = exports.symbols.clone();
//! assert!(rucc_stub::write(&read, glibc).is_ok());
//!
//! // Carried as section 9.2's blob, which is how one file covers every architecture. Packed here
//! // with one architecture in it for brevity; the point of the format is that a second one sharing
//! // these names costs an index each rather than a copy of the names.
//! let aarch64 = rucc_stub::abilist::read("GLIBC_2.17 printf F\nGLIBC_2.17 environ D 0x8\n").unwrap();
//! let packed = rucc_stub::blob::pack(&[("aarch64", &aarch64), ("x86_64", &exports)]).unwrap();
//! let carried = rucc_stub::blob::Blob::read(&packed).unwrap();
//! assert_eq!(carried.architectures().collect::<Vec<_>>(), ["aarch64", "x86_64"]);
//!
//! // The same symbols, in the blob's order rather than the file's, because a blob has no lines.
//! let mut there = carried.exports("x86_64").unwrap().symbols;
//! let mut back = exports.symbols.clone();
//! there.sort();
//! back.sort();
//! assert_eq!(there, back);
//!
//! // And a target on an older glibc gets what that glibc had: the `GLIBC_2.14` memcpy is above the
//! // line, so the `GLIBC_2.2.5` one is what a plain reference takes.
//! let old = carried.exports_at("x86_64", "GLIBC_2.12").unwrap();
//! assert_eq!(old.symbols.len(), 3);
//! let default = old
//!     .symbols
//!     .iter()
//!     .find(|symbol| symbol.name == "memcpy" && symbol.version.as_ref().unwrap().default)
//!     .unwrap();
//! assert_eq!(node(default), "GLIBC_2.2.5");
//!
//! // Windows, where the description is a module definition file. The `@8` is the i386 stdcall
//! // decoration and part of the name, the `@103` on its own is an ordinal, and `DATA` is the one
//! // distinction an import library cannot afford to lose.
//! let module = rucc_stub::def::read("\
//! LIBRARY \"KERNEL32.dll\"
//! EXPORTS
//! GetProcAddress@8
//! DnsGlobals DATA
//! ord_103 @103
//! ").unwrap();
//! assert_eq!(module.dll(), "KERNEL32.dll");
//! assert_eq!(module.exports[0].name, "GetProcAddress@8");
//! assert_eq!(module.exports[1].form, rucc_stub::def::Form::Data);
//! assert_eq!(module.exports[2].ordinal, Some(103));
//!
//! // An API set has no dot in its name, so the DLL name is the library name with a suffix. That is
//! // the rule both dlltools apply, and it is about a dot rather than about a known suffix.
//! let api = rucc_stub::def::read("LIBRARY api-ms-win-core-apiquery-l2-1-0\nEXPORTS\nf\n").unwrap();
//! assert_eq!(api.dll(), "api-ms-win-core-apiquery-l2-1-0.dll");
//! ```

#![doc(html_root_url = "https://docs.rs/rucc-stub/0.10.13")]
// Every public item here is read by somebody bringing up a target who has an ELF reference open
// beside it, so an undocumented one is a question they have to answer by reading the body.
#![deny(missing_docs)]

pub mod abilist;
pub mod blob;
pub mod compat;
pub mod def;
pub mod describe;
pub mod elf;

pub use compat::{Compat, Form, compat};
pub use describe::{Binding, Error, Kind, Library, Symbol, Version};
pub use elf::write;

#[doc(inline)]
pub use rucc_tuple::TargetTuple;

/// The milestone in `spec/cross-compile/15-plan.md` that fills this crate in.
pub const MILESTONE: &str = "M9";
