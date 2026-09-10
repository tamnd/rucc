//! The libraries a link line names that the libc does not have separately any more.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` section 9.9.
//!
//! Autoconf scripts, CMake modules and hand written makefiles pass `-lm` and `-lpthread` whether or
//! not there is anything in them, and a missing file is a link error rather than a shrug. So a
//! sysroot has to contain a file for every name a link line might reasonably ask for, even when the
//! code moved into `libc` years ago. This module is the list of those names per target, and the list
//! is a policy rather than a description: what a library exports is a question for its `abilist`,
//! and what files have to exist at all is this question.
//!
//! Two things here are easy to get wrong and both are worth stating before the list.
//!
//! The first is that the name the linker opens is not the name that ends up in `DT_NEEDED`. `-lpthread`
//! makes the linker look for `libpthread.so`, and what goes into the program is the `SONAME` found
//! inside it, which is `libpthread.so.0`. A distribution has both, one a symlink to the other. A
//! generated sysroot needs the one the linker opens, carrying the `SONAME` of the one the loader
//! wants, and writing the file under the `SONAME` leaves `-lpthread` with nothing to find.
//!
//! The second is that empty means something different on glibc and on musl, and not just in spelling.
//! musl never had these libraries and ships empty archives, so a link that names one pulls in no
//! members and records no dependency. glibc did have them, kept the shared objects for binary
//! compatibility, and a link that names one still records a `DT_NEEDED` on a file that exists on the
//! target. An empty archive where an empty shared object belongs gives a program with no dependency
//! it needed, and an empty shared object where an archive belongs gives a program asking the loader
//! for a file musl has never shipped.

use rucc_tuple::{Env, TargetTuple, Version};

use crate::describe::{Error, Library};

/// The glibc release that emptied four of its libraries into `libc.so.6`.
///
/// glibc's own announcement for it names `libpthread`, `libdl`, `libutil` and `libanl`, and those
/// four are the ones below. `librt` moved in the same direction over several releases and `libm`
/// never moved at all, which is why neither is here. See [`compat`].
const MERGED: Version = Version::new(2, 34);

/// The four glibc libraries that are empty from [`MERGED`] onwards, and the `SONAME` each carries.
///
/// The left column is what `-l` opens and the right is what the loader is told to find. They differ,
/// and the numbers on the right are not derivable from anything: they are the versions glibc has
/// carried since before the merge and kept across it, because keeping them is the entire point of
/// leaving the files behind.
const GLIBC: &[(&str, &str)] = &[
    ("libpthread.so", "libpthread.so.0"),
    ("libdl.so", "libdl.so.2"),
    ("libutil.so", "libutil.so.1"),
    ("libanl.so", "libanl.so.1"),
];

/// The libraries musl ships as archives with nothing in them.
///
/// This is musl's `EMPTY_LIB_NAMES` and it is a fact about musl's build rather than a judgement of
/// ours. `libm` is in it, which is the row that makes the difference between the two libcs concrete:
/// the same `-lm` finds real code on glibc and an empty archive on musl, because musl puts the math
/// in `libc.a` and glibc does not.
const MUSL: &[&str] = &["m", "rt", "pthread", "crypt", "util", "xnet", "resolv", "dl"];

/// An archive with no members, which is the entire file.
///
/// Eight bytes, the magic and nothing after it. This is what `ar` produces when given no members and
/// what musl's build writes for each of its empty libraries, and every linker reads it as an archive
/// that contributes nothing rather than as a file it cannot parse.
pub const EMPTY_ARCHIVE: &[u8] = b"!<arch>\n";

/// What sort of file an empty library is, which is not the same question as what it is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Form {
    /// An ELF shared object exporting nothing, carrying this `SONAME`.
    ///
    /// A link against it records a `DT_NEEDED` on the `SONAME`, which is the behaviour glibc wants:
    /// the file is empty here and present on the target, and a program that names it loads it.
    Shared(String),
    /// An archive with no members, whose contents are [`EMPTY_ARCHIVE`].
    ///
    /// A link against it records nothing at all, which is the behaviour musl wants, since there is
    /// no such file on a musl system for a loader to go looking for.
    Archive,
}

/// One file a sysroot has to contain even though there is nothing in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compat {
    /// The name the linker opens when it resolves the `-l` that asks for this.
    pub file: String,
    /// What kind of file it is.
    pub form: Form,
}

impl Compat {
    /// The contents of the file, ready to write.
    pub fn bytes(&self, target: TargetTuple) -> Result<Vec<u8>, Error> {
        match &self.form {
            Form::Shared(soname) => crate::elf::write(&Library::new(soname), target),
            Form::Archive => Ok(EMPTY_ARCHIVE.to_vec()),
        }
    }
}

/// Every empty library a sysroot for this target has to contain.
///
/// Empty is the whole of the claim. A library that still has code in it is not here, because what it
/// exports has to come from a description rather than from a list of names in this file, and a list
/// of names is all this is.
///
/// # glibc
///
/// `libpthread`, `libdl`, `libutil` and `libanl`, from 2.34 onwards. A tuple with no version pinned is
/// treated as current, since the unpinned spelling is the one people write when they mean the machine
/// in front of them, and a target pinned below 2.34 gets nothing from here because on those releases
/// the libraries are real and their contents come from `abilist` like everything else.
///
/// `librt` and `libm` are deliberately absent, and both for the same reason rather than out of doubt
/// that the files are needed. `libm` on glibc is real code and always has been: `sin` lives in
/// `libm.so.6` today. `librt` has been moving into `libc` across several releases, so whether it is
/// empty at a given version is a question with a different answer per version, which is exactly the
/// kind of question `abilist` answers and a constant in this file does not. Both files do have to
/// exist in a glibc sysroot, and both will, produced from their description rather than from here.
///
/// `libcrypt` is also absent and is not glibc's any more. It left for libxcrypt rather than moving
/// into `libc`, so on a modern distribution `libcrypt.so.1` is somebody else's library that happens
/// to sit in the same directory.
///
/// # musl
///
/// `m`, `rt`, `pthread`, `crypt`, `util`, `xnet`, `resolv` and `dl`, as archives, at every version,
/// because musl has no versions in this sense and never shipped any of them.
///
/// # Everything else
///
/// Nothing, and for three different reasons. On the BSDs and illumos these libraries are real and
/// separate. On Android the NDK ships the set it ships, and section 9.6 is clear that the description
/// there is consumed rather than invented, so `-lpthread` on Android is a question for the driver's
/// link line rather than for a file we generate. Darwin and Windows want a different container
/// entirely and nothing in this crate answers for them.
pub fn compat(target: TargetTuple) -> Vec<Compat> {
    match target.env() {
        Env::Gnu => {
            if !target.env_version().is_none_or(|version| version.at_least(MERGED)) {
                return Vec::new();
            }
            GLIBC
                .iter()
                .map(|(file, soname)| Compat {
                    file: (*file).to_owned(),
                    form: Form::Shared((*soname).to_owned()),
                })
                .collect()
        }
        Env::Musl => MUSL
            .iter()
            .map(|name| Compat { file: format!("lib{name}.a"), form: Form::Archive })
            .collect(),
        Env::None | Env::Msvc | Env::Android | Env::Simulator | Env::MacAbi => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use rucc_tuple::TargetTuple;

    use super::{Compat, EMPTY_ARCHIVE, Form, compat};

    fn target(spelling: &str) -> TargetTuple {
        spelling.parse().expect("a tuple the table knows")
    }

    fn files(spelling: &str) -> Vec<String> {
        compat(target(spelling)).into_iter().map(|one| one.file).collect()
    }

    #[test]
    fn the_name_a_link_opens_is_not_the_name_the_loader_is_given() {
        // The mistake this exists to prevent. Writing the file under its SONAME leaves `-lpthread`
        // with nothing to open, and the link fails on a file that is sitting right there.
        let all = compat(target("x86_64-linux-gnu"));
        let pthread = all.iter().find(|one| one.file == "libpthread.so").expect("libpthread");
        assert_eq!(pthread.form, Form::Shared("libpthread.so.0".to_owned()));
        assert!(!all.iter().any(|one| one.file == "libpthread.so.0"));
    }

    #[test]
    fn glibc_gets_the_four_its_own_announcement_names() {
        assert_eq!(
            files("x86_64-linux-gnu"),
            ["libpthread.so", "libdl.so", "libutil.so", "libanl.so"]
        );
    }

    #[test]
    fn glibc_does_not_get_an_empty_libm_because_libm_is_real() {
        // The row that is wrong in the obvious reading of section 9.9. `sin` is in `libm.so.6` on
        // every glibc there has ever been, so an empty one here is a link failure for any program
        // that does arithmetic, and `librt` is left out for the related reason that whether it is
        // empty depends on the version and so belongs in a description.
        for spelling in ["x86_64-linux-gnu", "aarch64-linux-gnu", "s390x-linux-gnu"] {
            let files = files(spelling);
            assert!(!files.contains(&"libm.so".to_owned()), "{spelling}");
            assert!(!files.contains(&"librt.so".to_owned()), "{spelling}");
        }
    }

    #[test]
    fn a_glibc_older_than_the_merge_gets_nothing_because_the_libraries_are_real_there() {
        assert!(files("x86_64-linux-gnu.2.28").is_empty());
        assert!(files("x86_64-linux-gnu.2.17").is_empty());
        // The release that did it counts as having done it.
        assert_eq!(files("x86_64-linux-gnu.2.34").len(), 4);
        assert_eq!(files("x86_64-linux-gnu.2.39").len(), 4);
    }

    #[test]
    fn an_unpinned_glibc_is_treated_as_a_current_one() {
        // Somebody who writes the tuple without a version means the machine in front of them, and
        // that machine has had the merged layout since 2021.
        assert_eq!(files("x86_64-linux-gnu"), files("x86_64-linux-gnu.2.39"));
    }

    #[test]
    fn musl_gets_archives_and_one_of_them_is_libm() {
        let all = compat(target("x86_64-linux-musl"));
        assert_eq!(all.len(), 8);
        assert!(all.iter().all(|one| one.form == Form::Archive));
        assert!(all.iter().any(|one| one.file == "libm.a"));
        // The same flag, the two libcs, two different answers. On glibc `-lm` finds real code and
        // on musl it finds nothing, and both are correct for their libc.
        assert!(!files("x86_64-linux-gnu").contains(&"libm.so".to_owned()));
    }

    #[test]
    fn an_empty_archive_is_the_magic_and_nothing_else() {
        assert_eq!(EMPTY_ARCHIVE, b"!<arch>\n");
        let one = Compat { file: "libm.a".to_owned(), form: Form::Archive };
        assert_eq!(one.bytes(target("x86_64-linux-musl")).expect("eight bytes"), EMPTY_ARCHIVE);
    }

    #[test]
    fn an_empty_shared_library_is_a_library_with_the_right_soname_in_it() {
        let one = compat(target("aarch64-linux-gnu")).into_iter().next().expect("libpthread");
        let bytes = one.bytes(target("aarch64-linux-gnu")).expect("a stub");
        assert_eq!(&bytes[..4], b"\x7fELF");
        // The SONAME is in there as a string, which is the minimum the loader needs from it. That
        // it is in `.dynstr` and pointed at by `DT_SONAME` is `tests/roundtrip.rs`'s business.
        let found = bytes.windows(15).any(|window| window == b"libpthread.so.0");
        assert!(found, "the soname is not in the file");
    }

    #[test]
    fn the_targets_that_want_nothing_from_here_get_nothing() {
        for spelling in ["x86_64-freebsd", "x86_64-illumos", "aarch64-linux-android", "x86_64-none"]
        {
            assert!(files(spelling).is_empty(), "{spelling}");
        }
    }

    #[test]
    fn every_file_named_here_can_actually_be_produced() {
        // A list of names that cannot be turned into files is a list that fails the first time
        // somebody builds a sysroot out of it rather than the first time somebody reads it.
        for spelling in
            ["x86_64-linux-gnu", "aarch64-linux-gnu", "i686-linux-gnu", "armv7-linux-musleabihf"]
        {
            let target = target(spelling);
            for one in compat(target) {
                assert!(one.bytes(target).is_ok(), "{spelling} {}", one.file);
            }
        }
    }
}
