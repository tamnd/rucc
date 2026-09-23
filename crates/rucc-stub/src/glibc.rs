//! glibc's description as this crate carries it, and the stubs a glibc link line is written from.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` sections 9.1, 9.2 and 9.9, and document 13 section
//! 13.1's "libc descriptions" row, which is these files.
//!
//! # What is carried
//!
//! Three blobs in the format of [`crate::blob`], one per library that has code in it on a current
//! glibc: `libc`, `libm` and `librt`. Each holds the eight architectures in [`ARCHITECTURES`], packed
//! from the `abilist` files of the newest glibc `tamnd/rucc-cross` pins, and since an `abilist` says
//! which release added every name, the newest one describes every older release too. They are
//! compiled into the binary, which is what makes a stub something the compiler writes rather than
//! something it has to fetch.
//!
//! `cargo xtask glibc-blob` writes them. It needs the abilists `bin/abilist` extracts, so it is run by
//! hand when the pinned glibc moves and the blobs are committed, which is the same arrangement the
//! checked in `docs/TARGETS.md` has with the table it is generated from.
//!
//! # What is written
//!
//! [`stubs`] is every file a glibc link line wants from this crate for one target: `libc.so`,
//! `libm.so` and `librt.so` out of the blobs, cut at the release the target asked for, and the empty
//! compatibility libraries of [`crate::compat`]. The file names are what `-l` opens and the
//! `SONAME`s inside them are what the loader will be asked for, which differ, and [`Library::file`]
//! and [`Library::soname`] are the two columns.
//!
//! Not written here: `libc_nonshared.a` and the start files, which are compiled code out of glibc's
//! own build and come with the fetched sysroot, and the loader's own library, whose `abilist` this
//! does not carry yet. A program that refers to a name only the loader exports, which in practice is
//! `__tls_get_addr` from a shared object's thread locals, gets an undefined symbol at link time.

use core::fmt;

use rucc_tuple::{Arch, DataModel, Env, Os, TargetTuple};

use crate::blob::{self, Blob};
use crate::compat::{Form, compat};
use crate::describe::{self, Library as Stub};

/// One glibc library this crate carries a description of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Library {
    /// The library's name, which is also the stem of its `abilist` and of its blob.
    pub name: &'static str,
    /// What `-l` opens, so what the stub is written as.
    pub file: &'static str,
    /// What the loader is told to find, so what goes into `DT_NEEDED` of a program linked against it.
    pub soname: &'static str,
    /// The packed description.
    blob: &'static [u8],
}

/// The three libraries, in the order a link line would name them.
pub const LIBRARIES: &[Library] = &[
    Library {
        name: "libc",
        file: "libc.so",
        soname: "libc.so.6",
        blob: include_bytes!("../glibc/libc.blob"),
    },
    Library {
        name: "libm",
        file: "libm.so",
        soname: "libm.so.6",
        blob: include_bytes!("../glibc/libm.blob"),
    },
    Library {
        name: "librt",
        file: "librt.so",
        soname: "librt.so.1",
        blob: include_bytes!("../glibc/librt.blob"),
    },
];

/// The architectures in every blob, spelled the way `bin/abilist` names its directories.
pub const ARCHITECTURES: &[&str] =
    &["x86_64", "x86", "aarch64", "arm", "riscv64", "powerpc64", "s390x", "loongarch64"];

impl Library {
    /// The description, read.
    ///
    /// # Errors
    ///
    /// A blob that does not read, which is a blob somebody committed without running the tests.
    pub fn description(&self) -> Result<Blob<'static>, blob::Error> {
        Blob::read(self.blob)
    }

    /// How many bytes the description is, for the size report.
    #[must_use]
    pub fn packed_len(&self) -> usize {
        self.blob.len()
    }
}

/// The section of the blobs a target reads, or [`None`] for a target glibc has no port for.
///
/// Little endian only for `powerpc64`, because the blob holds the little endian port and nothing
/// else, and 64-bit pointers only for `x86_64`, because x32 has its own `abilist` that is not here.
#[must_use]
pub fn architecture(target: TargetTuple) -> Option<&'static str> {
    if target.os() != Os::Linux || target.env() != Env::Gnu {
        return None;
    }
    match target.arch() {
        Arch::X86_64 if target.data_model() == DataModel::Lp64 => Some("x86_64"),
        Arch::X86 => Some("x86"),
        Arch::Aarch64 if target.is_little_endian() => Some("aarch64"),
        Arch::Arm if target.is_little_endian() => Some("arm"),
        Arch::Riscv64 => Some("riscv64"),
        Arch::PowerPc64 if target.is_little_endian() => Some("powerpc64"),
        Arch::S390x => Some("s390x"),
        Arch::LoongArch64 => Some("loongarch64"),
        _ => None,
    }
}

/// One file of a glibc sysroot that this crate writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// The name to write it under, which is what `-l` opens.
    pub name: String,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// Every file a glibc link line for `target` wants out of this crate, cut at glibc 2.`minor`.
///
/// The release is the caller's rather than read off the tuple here, because the answer for a tuple
/// that names none is the bundled release and which one that is belongs to the header tree rather
/// than to this crate.
///
/// # Errors
///
/// A target glibc has no port for, and whatever the writer refuses for a target, which today is
/// loongarch64's `e_flags`.
pub fn stubs(target: TargetTuple, minor: u32) -> Result<Vec<File>, Error> {
    let arch = architecture(target).ok_or(Error::NoPort { target })?;
    let ceiling = format!("GLIBC_2.{minor}");
    let mut out = Vec::new();
    for library in LIBRARIES {
        let exports = library
            .description()
            .and_then(|blob| blob.exports_at(arch, &ceiling))
            .map_err(|why| Error::Blob { library: library.name, why })?;
        let mut stub = Stub::new(library.soname);
        for symbol in exports.symbols {
            stub.export(symbol);
        }
        let bytes = crate::write(&stub, target)
            .map_err(|why| Error::Write { library: library.name, why })?;
        out.push(File { name: library.file.to_owned(), bytes });
    }
    for one in compat(target) {
        // Every glibc entry is a shared object, and an archive here would be a musl row reaching a
        // glibc target, which is a bug in the table rather than a file to write.
        debug_assert!(matches!(one.form, Form::Shared(_)), "{}", one.file);
        let bytes = one.bytes(target).map_err(|why| Error::Write { library: "compat", why })?;
        out.push(File { name: one.file, bytes });
    }
    Ok(out)
}

/// Why [`stubs`] wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The target is not one glibc has a port for, or not one these blobs describe.
    NoPort {
        /// The target.
        target: TargetTuple,
    },
    /// A blob did not read or has no section for the architecture.
    Blob {
        /// Which library's.
        library: &'static str,
        /// What the reader said.
        why: blob::Error,
    },
    /// The writer refused.
    Write {
        /// Which library it was writing.
        library: &'static str,
        /// What it said.
        why: describe::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoPort { target } => {
                write!(f, "{} is not a target glibc has a port for", target.to_canonical_string())
            }
            Error::Blob { library, why } => write!(f, "the description of {library}: {why}"),
            Error::Write { library, why } => write!(f, "the stub for {library}: {why}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(spelling: &str) -> TargetTuple {
        spelling.parse().expect("a tuple the table knows")
    }

    /// Every blob reads and has every architecture, which is what `pack-glibc` promises and what a
    /// hand edited or half written blob would break.
    #[test]
    fn every_blob_reads_and_describes_every_architecture() {
        for library in LIBRARIES {
            let blob =
                library.description().unwrap_or_else(|why| panic!("{}: {why}", library.name));
            let mut have: Vec<&str> = blob.architectures().collect();
            have.sort_unstable();
            let mut want = ARCHITECTURES.to_vec();
            want.sort_unstable();
            assert_eq!(have, want, "{}", library.name);
        }
    }

    /// The property section 9.2 calls the whole point: an older release gets the older definition.
    #[test]
    fn memcpy_is_the_old_one_before_glibc_2_14_and_the_new_one_after() {
        let libc = LIBRARIES[0].description().expect("libc reads");
        let default = |minor: u32| {
            let exports = libc.exports_at("x86_64", &format!("GLIBC_2.{minor}")).expect("x86_64");
            exports
                .symbols
                .into_iter()
                .find(|symbol| {
                    symbol.name == "memcpy" && symbol.version.as_ref().is_some_and(|v| v.default)
                })
                .and_then(|symbol| symbol.version)
                .map(|version| version.node)
        };
        assert_eq!(default(12).as_deref(), Some("GLIBC_2.2.5"));
        assert_eq!(default(28).as_deref(), Some("GLIBC_2.14"));
    }

    /// `sin` is in libm, which is the reason libm is carried rather than left empty.
    #[test]
    fn sin_is_in_libm_on_every_architecture() {
        let libm = LIBRARIES[1].description().expect("libm reads");
        for arch in ARCHITECTURES {
            let exports = libm.exports(arch).expect("every architecture");
            assert!(exports.symbols.iter().any(|symbol| symbol.name == "sin"), "{arch}");
        }
    }

    #[test]
    fn a_glibc_target_gets_three_real_stubs_and_the_empty_four() {
        let files = stubs(target("x86_64-linux-gnu"), 39).expect("x86_64 writes");
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "libc.so",
                "libm.so",
                "librt.so",
                "libpthread.so",
                "libdl.so",
                "libutil.so",
                "libanl.so"
            ]
        );
        // The same request twice is the same bytes, which is claim 5 of document 02.
        assert_eq!(files, stubs(target("x86_64-linux-gnu"), 39).expect("again"));
    }

    #[test]
    fn a_target_glibc_has_no_port_for_is_refused_by_name() {
        for spelling in ["x86_64-linux-musl", "x86_64-windows-gnu", "x86_64-linux-gnux32"] {
            let Ok(tuple) = spelling.parse::<TargetTuple>() else { continue };
            assert_eq!(architecture(tuple), None, "{spelling}");
            assert!(matches!(stubs(tuple, 39), Err(Error::NoPort { .. })), "{spelling}");
        }
    }
}
