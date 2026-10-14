//! glibc's description as this crate carries it, and the stubs a glibc link line is written from.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` sections 9.1, 9.2 and 9.9, and document 13 section
//! 13.1's "libc descriptions" row, which is these files.
//!
//! # What is carried
//!
//! Four blobs in the format of [`crate::blob`], one per library that has code in it on a current
//! glibc: `libc`, `libm`, `librt` and `ld`, which is the loader. Each holds the eight architectures in [`ARCHITECTURES`], packed
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
//! `libm.so` and `librt.so` out of the blobs, cut at the release the target asked for, the loader,
//! and the empty compatibility libraries of [`crate::compat()`]. The file names are what `-l` opens
//! and the `SONAME`s inside them are what the loader will be asked for, which differ, and
//! [`Library::file`] and [`Library::soname`] are the two columns.
//!
//! # What an abilist cannot say
//!
//! glibc keeps some symbols only for programs that were linked against them in the past. Such a
//! symbol has a definition at a version node but no default definition, so a new link that names it
//! fails with an undefined reference. The `abilist` line for it is the same as the line for any other
//! symbol, so the blobs mark it as the default like the rest. `glibc/compat.txt` is the list of these
//! names, with the first release where each one has no default, and [`compat_only`] reads it.
//! `bin/glibc-compat` in `tamnd/rucc-cross` makes the list from real Debian libraries, one per
//! release and architecture. It is run by hand and the list is committed, the same as the blobs.
//!
//! The loader is written under its `SONAME`, which is a different name on each port, and `ld.so`
//! beside it is a linker script with the one line `INPUT ( AS_NEEDED ( <soname> ) )`. glibc's own
//! `libc.so` names the loader in the same way. A program needs it for `__tls_get_addr`, and on
//! AArch64, Arm, RISC-V and LoongArch for `__stack_chk_guard`, which the loader exports there and
//! not libc. A program that uses none of its names does not get it in
//! `DT_NEEDED`.
//!
//! Not written here: `libc_nonshared.a` and the start files, which are compiled code out of glibc's
//! own build and come with the fetched sysroot.

use core::fmt;
use std::collections::BTreeSet;

use rucc_tuple::{Abi, Arch, DataModel, Env, Os, TargetTuple};

use crate::blob::{self, Blob};
use crate::compat::{Form, compat};
use crate::describe::{self, Library as Stub, Symbol};

/// One glibc library this crate carries a description of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Library {
    /// The library's name, which is also the stem of its `abilist` and of its blob.
    pub name: &'static str,
    /// What `-l` opens, so what the stub is written as.
    pub file: &'static str,
    /// What the loader is told to find, so what goes into `DT_NEEDED` of a program linked against it.
    /// [`None`] for the loader, whose name is the target's, from [`loader`].
    pub soname: Option<&'static str>,
    /// The packed description.
    blob: &'static [u8],
}

/// The four libraries, in the order a link line would name them.
pub const LIBRARIES: &[Library] = &[
    Library {
        name: "libc",
        file: "libc.so",
        soname: Some("libc.so.6"),
        blob: include_bytes!("../glibc/libc.blob"),
    },
    Library {
        name: "libm",
        file: "libm.so",
        soname: Some("libm.so.6"),
        blob: include_bytes!("../glibc/libm.blob"),
    },
    Library {
        name: "librt",
        file: "librt.so",
        soname: Some("librt.so.1"),
        blob: include_bytes!("../glibc/librt.blob"),
    },
    Library { name: "ld", file: "ld.so", soname: None, blob: include_bytes!("../glibc/ld.blob") },
];

/// The names glibc keeps only for old programs, with the release each one stopped being a default.
///
/// Each line is `library name release [arch=release]...`. The release is a minor number, or a dash for
/// never. The release after the name is the one for every architecture that the line does not name
/// again.
const COMPAT: &str = include_str!("../glibc/compat.txt");

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

/// The `SONAME` of the loader for a target that [`architecture`] gives a section for.
///
/// It is the file name of the path in `rucc_sysroot::link::glibc_loader`, and the reasons for each
/// row are given there. The two have to agree, because the stub tells the linker what to put in
/// `DT_NEEDED` and the path is what the kernel opens.
#[must_use]
pub fn loader(target: TargetTuple) -> Option<&'static str> {
    let hard = matches!(target.resolved_abi(), Abi::DoubleFloat);
    let name = match architecture(target)? {
        "x86_64" => "ld-linux-x86-64.so.2",
        "x86" => "ld-linux.so.2",
        "aarch64" => "ld-linux-aarch64.so.1",
        "arm" if hard => "ld-linux-armhf.so.3",
        "arm" => "ld-linux.so.3",
        "riscv64" if hard => "ld-linux-riscv64-lp64d.so.1",
        "riscv64" => "ld-linux-riscv64-lp64.so.1",
        "powerpc64" => "ld64.so.2",
        "s390x" => "ld64.so.1",
        "loongarch64" if hard => "ld-linux-loongarch-lp64d.so.1",
        "loongarch64" => "ld-linux-loongarch-lp64s.so.1",
        _ => return None,
    };
    Some(name)
}

/// Takes the default away from every name that glibc 2.`minor` keeps only for old programs.
///
/// `library` is a [`Library::name`] and `arch` is one of [`ARCHITECTURES`]. Every definition of such a
/// name becomes superseded, so a reference that names the version node still binds and an unversioned
/// reference does not. That is what a real glibc does. Returns how many definitions changed.
pub fn compat_only(library: &str, arch: &str, minor: u32, symbols: &mut [Symbol]) -> usize {
    let mut names = BTreeSet::new();
    for line in COMPAT.lines() {
        let mut fields = line.split_whitespace();
        let (Some(lib), Some(name), Some(mut since)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if lib != library {
            continue;
        }
        for field in fields {
            if let Some((other, release)) = field.split_once('=') {
                if other == arch {
                    since = release;
                }
            }
        }
        if since.parse::<u32>().is_ok_and(|since| since <= minor) {
            names.insert(name);
        }
    }
    let mut changed = 0;
    for symbol in symbols {
        if !names.contains(symbol.name.as_str()) {
            continue;
        }
        if let Some(version) = symbol.version.as_mut() {
            if version.default {
                version.default = false;
                changed += 1;
            }
        }
    }
    changed
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
        let mut exports = library
            .description()
            .and_then(|blob| blob.exports_at(arch, &ceiling))
            .map_err(|why| Error::Blob { library: library.name, why })?;
        let soname = library.soname.or_else(|| loader(target)).ok_or(Error::NoPort { target })?;
        compat_only(library.name, arch, minor, &mut exports.symbols);
        let mut stub = Stub::new(soname);
        for symbol in exports.symbols {
            stub.export(symbol);
        }
        let bytes = crate::write(&stub, target)
            .map_err(|why| Error::Write { library: library.name, why })?;
        if library.soname.is_some() {
            out.push(File { name: library.file.to_owned(), bytes });
        } else {
            out.push(File { name: soname.to_owned(), bytes });
            let script = format!("INPUT ( AS_NEEDED ( {soname} ) )\n");
            out.push(File { name: library.file.to_owned(), bytes: script.into_bytes() });
        }
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

    /// Whether `name` has a default definition in `library` on `arch` at glibc 2.`minor`.
    fn has_default(library: usize, arch: &str, name: &str, minor: u32) -> bool {
        let blob = LIBRARIES[library].description().expect("the blob reads");
        let mut exports = blob.exports_at(arch, &format!("GLIBC_2.{minor}")).expect("the arch");
        compat_only(LIBRARIES[library].name, arch, minor, &mut exports.symbols);
        exports
            .symbols
            .iter()
            .any(|symbol| symbol.name == name && symbol.version.as_ref().is_some_and(|v| v.default))
    }

    /// glibc stopped offering the RPC functions to new programs. Debian built them as defaults up to
    /// its 2.30 package.
    #[test]
    fn xdr_opaque_has_a_default_before_glibc_2_31_and_none_after() {
        assert!(has_default(0, "x86_64", "xdr_opaque", 28));
        assert!(!has_default(0, "x86_64", "xdr_opaque", 31));
        assert!(!has_default(0, "aarch64", "xdr_opaque", 39));
    }

    /// The `__*_finite` entry points went in 2.31, and the old definitions stay behind for old
    /// programs. The ordinary math function is not touched.
    #[test]
    fn the_finite_math_names_lose_the_default_in_glibc_2_31() {
        assert!(has_default(1, "x86_64", "__exp_finite", 30));
        assert!(!has_default(1, "x86_64", "__exp_finite", 31));
        assert!(has_default(1, "x86_64", "exp", 44));
    }

    /// `__finitel` is the same function as `__finite` where `long double` is `double`, so only Arm
    /// keeps it for old programs. The list names the architectures that differ.
    #[test]
    fn a_name_can_be_compat_only_on_one_architecture() {
        assert!(!has_default(0, "arm", "__finitel", 44));
        assert!(has_default(0, "x86_64", "__finitel", 44));
    }

    /// Every line of the list reads, and names a library and architectures that the blobs have.
    #[test]
    fn every_line_of_the_compat_list_reads() {
        let release = |text: &str| text == "-" || text.parse::<u32>().is_ok();
        let mut lines = 0;
        for line in COMPAT.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert!(fields.len() >= 3, "{line}");
            assert!(LIBRARIES.iter().any(|library| library.name == fields[0]), "{line}");
            assert!(release(fields[2]), "{line}");
            for other in &fields[3..] {
                let (arch, since) = other.split_once('=').expect("arch=release");
                assert!(ARCHITECTURES.contains(&arch) && release(since), "{line}");
            }
            lines += 1;
        }
        assert!(lines > 400, "{lines}");
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
    fn a_glibc_target_gets_four_real_stubs_and_the_empty_four() {
        let files = stubs(target("x86_64-linux-gnu"), 39).expect("x86_64 writes");
        let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "libc.so",
                "libm.so",
                "librt.so",
                "ld-linux-x86-64.so.2",
                "ld.so",
                "libpthread.so",
                "libdl.so",
                "libutil.so",
                "libanl.so"
            ]
        );
        // The same request twice is the same bytes, which is claim 5 of document 02.
        assert_eq!(files, stubs(target("x86_64-linux-gnu"), 39).expect("again"));
    }

    /// The stack protector's guard is a variable in the loader on four ports and a field of the
    /// thread block on the other four, which have nothing to export. The script names the stub
    /// under its `SONAME`.
    #[test]
    fn the_loader_stub_has_the_guard_where_glibc_puts_it_there() {
        let ld = LIBRARIES[3].description().expect("ld reads");
        let has = |arch: &str, name: &str| {
            let exports = ld.exports_at(arch, "GLIBC_2.39").expect("every architecture");
            exports.symbols.iter().any(|symbol| symbol.name == name)
        };
        for arch in ARCHITECTURES {
            let guard = matches!(*arch, "aarch64" | "arm" | "riscv64" | "loongarch64");
            assert_eq!(has(arch, "__stack_chk_guard"), guard, "{arch}");
            // s390x calls its own `__tls_get_offset` and not the name every other port has.
            let tls = if *arch == "s390x" { "__tls_get_offset" } else { "__tls_get_addr" };
            assert!(has(arch, tls), "{arch}");
        }
        let files = stubs(target("aarch64-linux-gnu"), 39).expect("aarch64 writes");
        let script = files.iter().find(|file| file.name == "ld.so").expect("the script");
        assert_eq!(script.bytes, b"INPUT ( AS_NEEDED ( ld-linux-aarch64.so.1 ) )\n");
        assert!(files.iter().any(|file| file.name == "ld-linux-aarch64.so.1"));
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
