//! Which glibc release a tree named with `--sysroot` is, and whether it is the one that was asked
//! for.
//!
//! Design: `spec/cross-compile/08-sysroots.md` sections 8.4 and 8.5.
//!
//! A release in the target is the one thing a person writes a pin to say, and section 8.5 is
//! explicit about what it buys them: `--target=x86_64-linux-gnu.2.28` reads the bundled tree for
//! 2.28 rather than this machine's headers, because a binary built against a newer libc does not
//! run where they asked for. A tree named with `--sysroot` replaces that tree, which #907 settled,
//! and from that point the pin decides nothing at all: the headers come from the named tree, the
//! link inputs come from the named tree, and the release in the target is a number nothing reads.
//!
//! That is the `_STAT_VER` class of section 8.4 arriving from the direction #925 describes. The
//! two sides came from two places and nobody said they had to match, the program compiles, it
//! links, and a function whose behaviour is selected by a versioning constant in a header takes the
//! wrong branch at run time. The difference here is that the mismatch is visible before anything is
//! compiled, because both halves of it are on the command line and in a file on the disk.
//!
//! So this compares them. The release in the target against the release the tree says it is, read
//! out of `features.h` with the preprocessor rather than off a directory name, because a version
//! read off a path is a version read off a convention nobody promised. `/opt/sysroots/glibc-2.28`
//! is a directory somebody named after what they believed was in it.
//!
//! Three things the issue asked to be settled, and what they are here.
//!
//! When it runs. Once, where the header directories are chosen, which is once for a whole command
//! line rather than once per file: `rucc a.c b.c` resolves its search path one time and compiles
//! two translation units against it. It is not in the link, because the thing that goes wrong goes
//! wrong at compilation and a build that stops at `-c` never reaches a link to be warned by. What
//! it costs is one probe of five or six small headers, and only on a command line that both named a
//! tree and pinned a release, which is the only shape that can be wrong this way.
//!
//! What it does when they differ. A warning, because the combination is one a person can mean. A
//! tree that is 2.28 and a pin that says 2.28 is the case this is silent about, and a tree the user
//! assembled for a release they know about is theirs to decide on. What a warning buys is that the
//! pin having no effect is said out loud once instead of being discovered in the field.
//!
//! Whether it applies to musl. It does not, and not as a special case: there is no `__GLIBC__` on
//! musl to read, so the probe answers nothing and nothing is compared. The condition on the target
//! being a glibc one is there so that the probe is not run at all rather than run and thrown away.

use std::path::PathBuf;

use rucc_base::Interner;
use rucc_lex::PpTokenKind;
use rucc_pp::{Context, Predef, Preprocessor, Tok};
use rucc_session::{FileSystem, Options, Session};
use rucc_target::Triple;
use rucc_tuple::{Env, Os, TargetTuple, Version};

use crate::preprocess::OsFileSystem;

/// The file the answer is in, which is the file every glibc program already reads it out of.
///
/// `__GLIBC__` and `__GLIBC_MINOR__` are defined here and `__GLIBC_PREREQ` next to them, so a tree
/// that answers this probe is a tree that answers every version test a program makes.
const PROBE: &str = "<glibc version probe>";

/// The word the two numbers come after, so that the answer is found in the output rather than
/// assumed to be all of it. The tree's own headers are read on the way and a stray token out of one
/// of them would otherwise be the thing that got parsed.
const MARKER: &str = "__rucc_glibc_release";

/// The probe itself.
///
/// Guarded on both macros rather than one, because a tree with half of the pair in it is a tree
/// this cannot say a version for and guessing the other half from the one that is there would be
/// the convention this exists to avoid.
const SOURCE: &[u8] = b"#include <features.h>\n\
#if defined __GLIBC__ && defined __GLIBC_MINOR__\n\
__rucc_glibc_release __GLIBC__ __GLIBC_MINOR__\n\
#endif\n";

/// The warning for a named tree that is not the release the target asked for, or [`None`].
///
/// `dirs` is the system header search path this compile resolved, which is where the tree's own
/// directories are by the time anything can be read out of them. It is passed rather than
/// recomputed so that the probe reads the directories the compilation will read and not a second
/// opinion about where they are.
pub(crate) fn skew(
    target: Triple,
    pinned: Option<TargetTuple>,
    dirs: &[PathBuf],
) -> Option<String> {
    skew_in(target, pinned, dirs, &OsFileSystem::new())
}

/// The same answer against a given file system, which is what makes it testable.
fn skew_in(
    target: Triple,
    pinned: Option<TargetTuple>,
    dirs: &[PathBuf],
    fs: &dyn FileSystem,
) -> Option<String> {
    let tuple = pinned.unwrap_or_else(|| target.tuple());
    if tuple.os() != Os::Linux || tuple.env() != Env::Gnu {
        return None;
    }
    // The pin, as two components. A tuple holds three and a glibc release is two, so a spelling
    // this cannot read as a release is a spelling that pinned nothing, which is the same reading
    // `rucc_sysroot::bundled_glibc_minor` gives it.
    let asked = tuple.env_version()?;
    let asked = Version::new(asked.major_part(), asked.minor_part()?);
    let tree = release_in(target, dirs, fs)?;
    if tree == asked {
        return None;
    }
    Some(format!(
        "the target asks for glibc {asked} and the tree named with --sysroot is glibc {tree}, and \
         the tree is what is compiled and linked against, so the release in the target decides \
         nothing here. Drop it from --target, or name a tree that is the release it asks for"
    ))
}

/// What the headers on `dirs` say their release is, or [`None`] when they do not say.
///
/// [`None`] covers every way this can decline to have an opinion and they are all the same answer:
/// a tree with no `features.h` in it, a tree that is not glibc, a directory that is not there, and
/// a `features.h` that could not be preprocessed because the rest of the tree is missing. None of
/// those is reported here. A tree that cannot be read is a tree the compilation is about to fail
/// on with a diagnostic that says which file it wanted, and a probe that got in front of that with
/// a worse version of the same message would be the wrong thing twice.
fn release_in(target: Triple, dirs: &[PathBuf], fs: &dyn FileSystem) -> Option<Version> {
    if dirs.is_empty() {
        return None;
    }
    let mut opts = Options::new(target);
    for dir in dirs {
        opts.search.push_system(dir.clone());
    }
    let mut sess = Session::new(opts.clone());
    let main = sess.sources.add(PROBE, SOURCE.to_vec()).ok()?;
    let mut pp = Preprocessor::new();
    let predef = Predef::for_options(&opts);
    let tokens = {
        let mut cx = Context::new(&mut sess.interner, &mut sess.sources, fs, &opts.search);
        cx.lex = rucc_lex::Options::for_dialect(opts.std, opts.gnu_extensions);
        pp.predefine(&sess.target, &predef, &mut cx).ok()?;
        pp.run(main, &mut cx)
    };
    read_release(&tokens, &sess.interner)
}

/// The two numbers after the marker, as a version.
fn read_release(tokens: &[Tok], interner: &Interner) -> Option<Version> {
    let spelled = |tok: &Tok| tok.value.map(|sym| interner.resolve(sym));
    let mut rest = tokens
        .iter()
        .skip_while(|tok| tok.kind != PpTokenKind::Ident || spelled(tok) != Some(MARKER));
    rest.next()?;
    let mut number = || {
        let tok = rest.next()?;
        if tok.kind != PpTokenKind::Number {
            return None;
        }
        spelled(tok)?.parse::<u32>().ok()
    };
    let major = number()?;
    let minor = number()?;
    Some(Version::new(major, minor))
}

#[cfg(test)]
mod tests {
    use rucc_session::MemoryFileSystem;

    use super::*;

    /// The three field target, built rather than parsed, because what the probe wants out of it is
    /// the predefined macro set and not a spelling.
    fn gnu() -> Triple {
        Triple::new(rucc_target::Arch::X86_64, rucc_target::Os::Linux, rucc_target::Env::Gnu)
    }

    fn musl() -> Triple {
        Triple::new(rucc_target::Arch::X86_64, rucc_target::Os::Linux, rucc_target::Env::Musl)
    }

    fn pin(spelling: &str) -> TargetTuple {
        spelling.parse::<TargetTuple>().expect("a tuple this compiler knows")
    }

    /// A tree with a `features.h` in it that says it is `major.minor`, in the shape glibc's own
    /// file has: the two macros defined next to each other and `__GLIBC_PREREQ` reading both.
    fn tree(major: u32, minor: u32) -> (MemoryFileSystem, Vec<PathBuf>) {
        let mut fs = MemoryFileSystem::new();
        let text = format!(
            "#ifndef _FEATURES_H\n#define _FEATURES_H 1\n#define __GLIBC__ {major}\n#define \
             __GLIBC_MINOR__ {minor}\n#define __GLIBC_PREREQ(maj, min) ((__GLIBC__ << 16) + \
             __GLIBC_MINOR__ >= ((maj) << 16) + (min))\n#endif\n"
        );
        fs.insert("/tree/usr/include/features.h", text.into_bytes());
        (fs, vec![PathBuf::from("/tree/usr/include")])
    }

    #[test]
    fn the_release_is_read_out_of_the_headers_rather_than_off_the_directory() {
        let (fs, dirs) = tree(2, 28);
        assert_eq!(release_in(gnu(), &dirs, &fs), Some(Version::new(2, 28)));
    }

    #[test]
    fn a_tree_that_is_the_release_the_target_asked_for_says_nothing() {
        let (fs, dirs) = tree(2, 28);
        let pinned = pin("x86_64-linux-gnu.2.28");
        assert_eq!(skew_in(gnu(), Some(pinned), &dirs, &fs), None);
    }

    #[test]
    fn a_tree_that_is_a_different_release_names_both_of_them() {
        let (fs, dirs) = tree(2, 39);
        let pinned = pin("x86_64-linux-gnu.2.28");
        let said = skew_in(gnu(), Some(pinned), &dirs, &fs).expect("a warning");
        assert!(said.contains("glibc 2.28"), "{said}");
        assert!(said.contains("glibc 2.39"), "{said}");
    }

    #[test]
    fn a_target_that_pinned_no_release_has_nothing_to_compare() {
        let (fs, dirs) = tree(2, 39);
        assert_eq!(skew_in(gnu(), None, &dirs, &fs), None);
    }

    #[test]
    fn a_musl_target_is_not_probed_because_there_is_no_macro_to_read() {
        let (fs, dirs) = tree(2, 39);
        let pinned = pin("x86_64-linux-musl");
        assert_eq!(skew_in(musl(), Some(pinned), &dirs, &fs), None);
    }

    #[test]
    fn a_tree_with_no_libc_headers_in_it_answers_nothing_and_is_not_an_error() {
        let fs = MemoryFileSystem::new();
        let dirs = vec![PathBuf::from("/deps/include")];
        assert_eq!(release_in(gnu(), &dirs, &fs), None);
        let pinned = pin("x86_64-linux-gnu.2.28");
        assert_eq!(skew_in(gnu(), Some(pinned), &dirs, &fs), None);
    }

    #[test]
    fn a_features_header_that_defines_only_half_the_pair_is_not_a_version() {
        let mut fs = MemoryFileSystem::new();
        fs.insert("/tree/usr/include/features.h", b"#define __GLIBC__ 2\n".to_vec());
        let dirs = vec![PathBuf::from("/tree/usr/include")];
        assert_eq!(release_in(gnu(), &dirs, &fs), None);
    }

    /// The rest of the tree missing is the ordinary state of a half assembled sysroot, and the
    /// release is still in the file that was read. The compilation will fail on the missing header
    /// with a message that names it, which is the right place for that to be said.
    #[test]
    fn a_header_the_tree_does_not_have_does_not_stop_the_release_being_read() {
        let mut fs = MemoryFileSystem::new();
        fs.insert(
            "/tree/usr/include/features.h",
            b"#define __GLIBC__ 2\n#define __GLIBC_MINOR__ 34\n#include <gnu/stubs.h>\n".to_vec(),
        );
        let dirs = vec![PathBuf::from("/tree/usr/include")];
        assert_eq!(release_in(gnu(), &dirs, &fs), Some(Version::new(2, 34)));
    }

    #[test]
    fn nothing_on_the_search_path_is_nothing_to_probe() {
        let fs = MemoryFileSystem::new();
        assert_eq!(release_in(gnu(), &[], &fs), None);
    }
}
