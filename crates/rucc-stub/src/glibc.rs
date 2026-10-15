//! glibc's description as this crate carries it, and the stubs a glibc link line is written from.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` sections 9.1, 9.2 and 9.9, and document 13 section
//! 13.1's "libc descriptions" row, which is these files.
//!
//! # What is carried
//!
//! Five blobs in the format of [`crate::blob`], one per library that has code in it on a current
//! glibc: `libc`, `libm`, `librt`, `ld`, which is the loader, and `libresolv`. Each holds the eight architectures in [`ARCHITECTURES`], packed
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
//! `libresolv.so`, and the empty compatibility libraries of [`crate::compat()`]. The file names are what `-l` opens
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
//! # What moved
//!
//! The blobs are from glibc 2.44, and glibc moved names between its libraries before that. In 2.34
//! the contents of `libpthread`, `libdl`, `libutil` and `libanl` went into `libc`, and 2.44's `libc`
//! abilist lists those names at their old nodes, as `pthread_create@GLIBC_2.2.5`. A `libc` stub cut
//! at 2.28 would then export `pthread_create`, and the program would ask the real 2.28 `libc.so.6`
//! for a name it does not have. `glibc/moved.txt` gives, for each such definition, the release it
//! moved in and the library it was in before, and [`moved`] gives it back to that library. So a
//! target below 2.34 gets real stubs for the four libraries, with the definitions they had then.
//! The resolver functions that moved into `libc` in 2.34 go back to `libresolv` in the same way.
//! `bin/glibc-compat moved` in `tamnd/rucc-cross` makes the list from the same Debian libraries as
//! `glibc/compat.txt`. That is tamnd/rucc#3666.
//!
//! # Which node is the default
//!
//! The rule of [`describe`] takes the highest node of a name as its default. For 16 names that is
//! wrong. On x86, `pread`, `open64` and six more have the default at `GLIBC_2.1`, and the `GLIBC_2.2`
//! definition is the one libpthread had before 2.34. On s390x, `setjmp`, `longjmp` and six more have
//! the default at `GLIBC_2.2` or `GLIBC_2.11`, and a later node keeps an old definition.
//! `glibc/defaults.txt` gives the default node of each, and [`default_nodes`] sets it.
//! `bin/glibc-compat defaults` in `tamnd/rucc-cross` makes the list. That is tamnd/rucc#3660.
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
use crate::compat::{Form, GLIBC, MERGED, compat};
use crate::describe::{self, Library as Stub, Symbol, defaults};

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

/// The five libraries. The first four are in the order a link line would name them, and `libresolv`
/// is last because only a program that asks for it with `-lresolv` gets it.
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
    Library {
        name: "libresolv",
        file: "libresolv.so",
        soname: Some("libresolv.so.2"),
        blob: include_bytes!("../glibc/libresolv.blob"),
    },
];

/// The names glibc keeps only for old programs, with the release each one stopped being a default.
///
/// Each line is `library name release [arch=release]...`. The release is a minor number, or a dash for
/// never. The release after the name is the one for every architecture that the line does not name
/// again.
const COMPAT: &str = include_str!("../glibc/compat.txt");

/// The definitions that moved into a library after the release a target asked for.
///
/// Each line is `library name origin release [arch=release]...`. Before the release, the library had
/// no definition of the name, and the origin library had it. The origin is a dash when no library
/// had it. A name written as `name@node` is about the definition at that node alone, because the
/// library already had the name at another node. The release after the origin is the one for every
/// architecture that the line does not name again, and a dash means the definition did not move.
const MOVED: &str = include_str!("../glibc/moved.txt");

/// The names whose default definition is not at their highest node.
///
/// Each line is `library name arch node`. The node is the default on that architecture in every
/// measured release that defines the name at it.
const DEFAULTS: &str = include_str!("../glibc/defaults.txt");

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

/// Makes the node that [`DEFAULTS`] gives the default of its name, where that is not the highest node.
///
/// `library` is a [`Library::name`] and `arch` is one of [`ARCHITECTURES`]. The rule of [`describe`]
/// has already made the highest node the default, so this runs after it. A name that has no
/// definition at the node in `symbols` is left as it is. Returns how many definitions changed.
pub fn default_nodes(library: &str, arch: &str, symbols: &mut [Symbol]) -> usize {
    let mut changed = 0;
    for line in DEFAULTS.lines().filter(|line| !line.starts_with('#')) {
        let mut fields = line.split_whitespace();
        let (Some(lib), Some(name), Some(on), Some(node)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if lib != library || on != arch {
            continue;
        }
        let has = symbols.iter().any(|symbol| {
            symbol.name == name
                && symbol.version.as_ref().is_some_and(|version| version.node == node)
        });
        if !has {
            continue;
        }
        for symbol in symbols.iter_mut().filter(|symbol| symbol.name == name) {
            if let Some(version) = symbol.version.as_mut() {
                let default = version.node == node;
                if version.default != default {
                    version.default = default;
                    changed += 1;
                }
            }
        }
    }
    changed
}

/// Gives each definition that moved into a library after glibc 2.`minor` back to the library it came
/// from.
///
/// `sets` holds a [`Library::name`] and the definitions of each library, cut at the release. A
/// definition that [`MOVED`] says was not in its library yet goes out of that library, and into its
/// origin when the origin is in `sets`. The highest node of a name can change, so the caller runs the
/// rule for the defaults again after this. Returns how many definitions went out.
pub fn moved(arch: &str, minor: u32, sets: &mut [(&str, Vec<Symbol>)]) -> usize {
    let mut count = 0;
    for line in MOVED.lines().filter(|line| !line.starts_with('#')) {
        let mut fields = line.split_whitespace();
        let (Some(library), Some(name), Some(origin), Some(mut since)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        for field in fields {
            if let Some((other, release)) = field.split_once('=') {
                if other == arch {
                    since = release;
                }
            }
        }
        if !since.parse::<u32>().is_ok_and(|since| minor < since) {
            continue;
        }
        let (name, node) = match name.split_once('@') {
            Some((name, node)) => (name, Some(node)),
            None => (name, None),
        };
        let Some(from) = sets.iter().position(|(one, _)| *one == library) else {
            continue;
        };
        let mut gone = Vec::new();
        sets[from].1.retain(|symbol| {
            let hit = symbol.name == name
                && node.is_none_or(|node| {
                    symbol.version.as_ref().is_some_and(|version| version.node == node)
                });
            if hit {
                gone.push(symbol.clone());
            }
            !hit
        });
        count += gone.len();
        let Some(to) = sets.iter().position(|(one, _)| *one == origin) else {
            continue;
        };
        for symbol in gone {
            let node = symbol.version.as_ref().map(|version| &version.node);
            let have = sets[to].1.iter().any(|one| {
                one.name == symbol.name && one.version.as_ref().map(|version| &version.node) == node
            });
            if !have {
                sets[to].1.push(symbol);
            }
        }
    }
    count
}

/// Every library's definitions on `arch` at glibc 2.`minor`, by [`Library::name`].
///
/// The libraries of [`LIBRARIES`] come first, in their order. Before 2.34 the four libraries
/// that glibc then emptied into `libc` come after them, with what [`moved`] gave back. Each name has
/// its default from the highest node, or from [`default_nodes`] for the few names where glibc keeps
/// an older one. Then [`compat_only`] takes the default away where glibc keeps the name only for old
/// programs.
///
/// # Errors
///
/// A blob that does not read or has no section for `arch`, and a library that has two definitions
/// of a name at one node after the moves.
pub fn exports(arch: &str, minor: u32) -> Result<Vec<(&'static str, Vec<Symbol>)>, Error> {
    let ceiling = format!("GLIBC_2.{minor}");
    let mut sets = Vec::new();
    for library in LIBRARIES {
        let exports = library
            .description()
            .and_then(|blob| blob.exports_at(arch, &ceiling))
            .map_err(|why| Error::Blob { library: library.name, why })?;
        sets.push((library.name, exports.symbols));
    }
    if MERGED.minor_part().is_some_and(|merged| minor < merged) {
        for &(file, _) in GLIBC {
            sets.push((file.strip_suffix(".so").unwrap_or(file), Vec::new()));
        }
    }
    moved(arch, minor, &mut sets);
    for (library, symbols) in &mut sets {
        let library: &'static str = library;
        defaults(symbols).map_err(|_| Error::Moved { library })?;
        default_nodes(library, arch, symbols);
        compat_only(library, arch, minor, symbols);
    }
    Ok(sets)
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
    let mut out = Vec::new();
    for (index, (name, symbols)) in exports(arch, minor)?.into_iter().enumerate() {
        let (file, soname) = match LIBRARIES.get(index) {
            Some(library) => (library.file, library.soname),
            None => match GLIBC.get(index - LIBRARIES.len()) {
                Some(&(file, soname)) => (file, Some(soname)),
                None => continue,
            },
        };
        let soname = soname.or_else(|| loader(target)).ok_or(Error::NoPort { target })?;
        let mut stub = Stub::new(soname);
        for symbol in symbols {
            stub.export(symbol);
        }
        let bytes =
            crate::write(&stub, target).map_err(|why| Error::Write { library: name, why })?;
        if LIBRARIES.get(index).is_none_or(|library| library.soname.is_some()) {
            out.push(File { name: file.to_owned(), bytes });
        } else {
            out.push(File { name: soname.to_owned(), bytes });
            let script = format!("INPUT ( AS_NEEDED ( {soname} ) )\n");
            out.push(File { name: file.to_owned(), bytes: script.into_bytes() });
        }
    }
    for one in compat(target) {
        // A target whose tuple and release disagree about 2.34 has the real one already.
        if out.iter().any(|file| file.name == one.file) {
            continue;
        }
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
    /// A library has two definitions of a name at one node after [`moved`] gave definitions back.
    Moved {
        /// Which library.
        library: &'static str,
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
            Error::Moved { library } => {
                write!(f, "the stub for {library} has a name twice at one node after the moves")
            }
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

    /// A library this module writes a stub for, from the blobs or from what moved.
    fn known(name: &str) -> bool {
        LIBRARIES.iter().any(|library| library.name == name)
            || GLIBC.iter().any(|(file, _)| file.strip_suffix(".so") == Some(name))
    }

    /// Every node of `name` in `library`, lowest first, and whether it is the default.
    fn nodes(sets: &[(&str, Vec<Symbol>)], library: &str, name: &str) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = sets
            .iter()
            .filter(|(one, _)| *one == library)
            .flat_map(|(_, symbols)| symbols)
            .filter(|symbol| symbol.name == name)
            .filter_map(|symbol| symbol.version.clone())
            .map(|version| (version.node, version.default))
            .collect();
        out.sort_by(|one, two| describe::by_version(&one.0, &two.0));
        out
    }

    fn at(list: &[(&str, bool)]) -> Vec<(String, bool)> {
        list.iter().map(|&(node, default)| (node.to_owned(), default)).collect()
    }

    /// glibc 2.34 moved libpthread and libdl into libc. Before it, the real libc has no
    /// `pthread_create`, and a program that takes it from libc does not load. tamnd/rucc#3666.
    #[test]
    fn pthread_create_is_in_libpthread_before_glibc_2_34_and_in_libc_after() {
        let old = exports("x86_64", 28).expect("x86_64");
        assert_eq!(nodes(&old, "libc", "pthread_create"), at(&[]));
        assert_eq!(nodes(&old, "libpthread", "pthread_create"), at(&[("GLIBC_2.2.5", true)]));
        assert_eq!(nodes(&old, "libc", "dlopen"), at(&[]));
        assert_eq!(nodes(&old, "libdl", "dlopen"), at(&[("GLIBC_2.2.5", true)]));
        let new = exports("x86_64", 34).expect("x86_64");
        let both = at(&[("GLIBC_2.2.5", false), ("GLIBC_2.34", true)]);
        assert_eq!(nodes(&new, "libc", "pthread_create"), both);
        assert!(new.iter().all(|(library, _)| *library != "libpthread"));
    }

    /// Before 2.34 the x86 libc has `open64` at `GLIBC_2.1` alone, and the `GLIBC_2.2` definition is
    /// in libpthread. Only that one definition moves.
    #[test]
    fn one_node_of_open64_moves_on_x86() {
        let old = exports("x86", 28).expect("x86");
        assert_eq!(nodes(&old, "libc", "open64"), at(&[("GLIBC_2.1", true)]));
        assert_eq!(nodes(&old, "libpthread", "open64"), at(&[("GLIBC_2.2", true)]));
    }

    /// Before 2.31 the old `clock_gettime` is in librt and libc has only the `GLIBC_2.17` one.
    #[test]
    fn the_old_clock_gettime_is_in_librt_before_glibc_2_31() {
        let old = exports("x86_64", 28).expect("x86_64");
        assert_eq!(nodes(&old, "libc", "clock_gettime"), at(&[("GLIBC_2.17", true)]));
        assert_eq!(nodes(&old, "librt", "clock_gettime"), at(&[("GLIBC_2.2.5", true)]));
        let new = exports("x86_64", 31).expect("x86_64");
        let both = at(&[("GLIBC_2.2.5", false), ("GLIBC_2.17", true)]);
        assert_eq!(nodes(&new, "libc", "clock_gettime"), both);
    }

    /// Every line of the list reads, and names libraries and architectures that are known.
    #[test]
    fn every_line_of_the_moved_list_reads() {
        let release = |text: &str| text == "-" || text.parse::<u32>().is_ok();
        let mut lines = 0;
        for line in MOVED.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert!(fields.len() >= 4, "{line}");
            assert!(known(fields[0]), "{line}");
            assert!(fields[2] == "-" || known(fields[2]), "{line}");
            assert!(release(fields[3]), "{line}");
            for other in &fields[4..] {
                let (arch, since) = other.split_once('=').expect("arch=release");
                assert!(ARCHITECTURES.contains(&arch) && release(since), "{line}");
            }
            lines += 1;
        }
        assert!(lines > 200, "{lines}");
    }

    /// On x86 `pread` and `open64` have the default at `GLIBC_2.1`, and the `GLIBC_2.2` definition is
    /// the one that was in libpthread. tamnd/rucc#3660.
    #[test]
    fn pread_on_x86_has_the_default_at_glibc_2_1() {
        let new = exports("x86", 39).expect("x86");
        let old_first = at(&[("GLIBC_2.1", true), ("GLIBC_2.2", false)]);
        assert_eq!(nodes(&new, "libc", "pread"), old_first);
        assert_eq!(nodes(&new, "libc", "open64"), old_first);
        let before = exports("x86", 28).expect("x86");
        assert_eq!(nodes(&before, "libc", "pread"), at(&[("GLIBC_2.1", true)]));
        assert_eq!(nodes(&before, "libpthread", "pread"), at(&[("GLIBC_2.2", true)]));
        let wide = exports("x86_64", 39).expect("x86_64");
        assert_eq!(nodes(&wide, "libc", "pread"), at(&[("GLIBC_2.2.5", true)]));
    }

    /// On s390x `setjmp` has the default at `GLIBC_2.2`, and `GLIBC_2.19` keeps an old definition.
    #[test]
    fn setjmp_on_s390x_has_the_default_at_glibc_2_2() {
        for minor in [28, 39] {
            let sets = exports("s390x", minor).expect("s390x");
            assert_eq!(
                nodes(&sets, "libc", "setjmp"),
                at(&[("GLIBC_2.2", true), ("GLIBC_2.19", false)]),
                "2.{minor}"
            );
            let chk = nodes(&sets, "libc", "__longjmp_chk");
            let default: Vec<&str> =
                chk.iter().filter(|(_, default)| *default).map(|(node, _)| node.as_str()).collect();
            assert_eq!(default, ["GLIBC_2.11"], "2.{minor}");
        }
    }

    /// Every line of the list reads, and names a library and an architecture that are known.
    #[test]
    fn every_line_of_the_defaults_list_reads() {
        let mut lines = 0;
        for line in DEFAULTS.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 4, "{line}");
            assert!(known(fields[0]), "{line}");
            assert!(ARCHITECTURES.contains(&fields[2]), "{line}");
            assert!(fields[3].starts_with("GLIBC_2."), "{line}");
            lines += 1;
        }
        assert!(lines >= 16, "{lines}");
    }

    /// Every line of the list reads, and names a library and architectures that the blobs have.
    #[test]
    fn every_line_of_the_compat_list_reads() {
        let release = |text: &str| text == "-" || text.parse::<u32>().is_ok();
        let mut lines = 0;
        for line in COMPAT.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert!(fields.len() >= 3, "{line}");
            assert!(known(fields[0]), "{line}");
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
    fn a_glibc_target_gets_five_real_stubs_and_the_empty_four() {
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
                "libresolv.so",
                "libpthread.so",
                "libdl.so",
                "libutil.so",
                "libanl.so"
            ]
        );
        // The same request twice is the same bytes, which is claim 5 of document 02.
        assert_eq!(files, stubs(target("x86_64-linux-gnu"), 39).expect("again"));
    }

    /// Before 2.34 the same four files are there, written from what moved rather than empty.
    #[test]
    fn a_target_before_glibc_2_34_gets_the_four_with_their_names() {
        let old = stubs(target("x86_64-linux-gnu.2.28"), 28).expect("x86_64 writes");
        let new = stubs(target("x86_64-linux-gnu"), 39).expect("x86_64 writes");
        let names = |files: &[File]| files.iter().map(|file| file.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&old), names(&new));
        let size = |files: &[File], name: &str| {
            files.iter().find(|file| file.name == name).map_or(0, |file| file.bytes.len())
        };
        assert!(size(&old, "libpthread.so") > size(&new, "libpthread.so"));
        assert!(size(&old, "libdl.so") > size(&new, "libdl.so"));
    }

    /// `res_query` and `ns_initparse` are in libresolv at every release. In 2.34 the x86 `res_query`
    /// and the `__dn_expand` family moved into libc, so before 2.34 libresolv has them and libc does
    /// not. tamnd/rucc#3676.
    #[test]
    fn the_resolver_is_in_libresolv_before_glibc_2_34() {
        let old = exports("x86", 28).expect("x86");
        assert_eq!(nodes(&old, "libc", "__dn_expand"), at(&[]));
        assert_eq!(nodes(&old, "libresolv", "__dn_expand"), at(&[("GLIBC_2.2", true)]));
        assert!(!nodes(&old, "libresolv", "ns_initparse").is_empty());
        assert!(!nodes(&old, "libresolv", "res_query").is_empty());
        assert!(nodes(&old, "libc", "res_query").is_empty());
        let new = exports("x86", 39).expect("x86");
        assert!(!nodes(&new, "libc", "__dn_expand").is_empty());
        assert!(!nodes(&new, "libresolv", "ns_initparse").is_empty());
        for arch in ARCHITECTURES {
            let sets = exports(arch, 39).expect("every architecture");
            assert!(!nodes(&sets, "libresolv", "ns_initparse").is_empty(), "{arch}");
        }
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
