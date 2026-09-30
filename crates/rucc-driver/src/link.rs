//! Finding a linker and telling it what to link.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.9. There is no linker of our own before 1.0, so
//! this finds one on the machine and builds the command line it wants.
//!
//! The linker is invoked directly rather than through the system compiler driver. Going through
//! `cc` would be shorter to write and would borrow that compiler's idea of where everything is,
//! and it would also mean this compiler cannot link on a machine that has no other compiler on
//! it, which is most of the machines a compiler ends up on. It would also make `-###` output a
//! line that does not say what happens, since the interesting half would be inside the program
//! being spawned.
//!
//! # What is not decided here
//!
//! The startup files and the library directories are looked for rather than configured, for the
//! same reason `library` looks for the headers: gcc settles this when it is built because a gcc
//! is built for the machine it will run on, and this is one binary that runs wherever it is
//! copied. So the shape of the answer is a list of candidates per platform of which the ones
//! that exist are taken, and a cross build says where the rest is with `--sysroot`.
//!
//! # The compiler's own runtime
//!
//! `crtbegin`, `crtend` and the runtime libraries are found the same way, on the machine rather
//! than by configuration. Ours is `librucc_builtins.a`, looked for beside the compiler, and the
//! machine's `libgcc` goes on after it for the parts we have not written, which today is the
//! unwinder and its personality routine. The C library goes in front of both, so that on a target
//! that has one its `memcpy` is the one that answers rather than ours. `-fno-builtins-lib` leaves
//! ours off, for somebody who wants libgcc to answer for everything.
//!
//! On a static link the three archives go inside `--start-group`, because `libc.a` refers to the
//! unwinder and the unwinder refers back to `libc.a`, and a linker walking a list once resolves
//! whichever of the two it reaches first and leaves the other undefined. That circularity is the
//! whole reason `-static` failed before this, and it is issue #277.
//!
//! # Linking for a machine that is not this one
//!
//! Everything above describes a link against the machine running the compiler, and it is what runs
//! when the target is that machine. A target that is not is a different problem: there is no
//! `crt1.o` for it in `/usr/lib`, the `libc.so` there is the wrong architecture, and a line built
//! out of what is lying around either fails at the first input or, worse, links. So a cross link
//! does not look at this machine at all. It is built by [`rucc_sysroot::argv`] out of the target
//! and a sysroot under the cache directory, and `spec/cross-compile/11-linking.md` section 11.3 is
//! the design. [`cross_sysroot`] is the one place that decides which of the two it is.
//!
//! Two conditions keep that out of the way of everything that works today. The target has to differ
//! from the host, and `--sysroot` must not have been given: somebody who assembled a tree and named
//! it is asking for the line above with their own root in front of every path, which is what a
//! cross compile with a real distribution tree in it has always been.
//!
//! That second condition is also the escape hatch for a machine which has a distribution's own cross
//! files installed, where `/usr/lib/aarch64-linux-gnu` really does hold an AArch64 `crt1.o`.
//! `--sysroot=/` takes the line above, and then every directory it decides is that machine's again.
//!
//! # Windows in Microsoft's environment
//!
//! `lld-link` takes a line of its own, which [`rucc_sysroot::argv`] writes as it writes the others,
//! and the libraries on it come out of a tree nobody may redistribute. `--fetch` and
//! `--fetch-msvc-sdk` lay that tree out from Microsoft's own downloads once the person asking has
//! accepted Microsoft's licence, and say which `--sysroot` to pass, so the tree is always one
//! somebody named, and `msvc_sysroot` is the one place that says so. A mingw-w64 target links
//! against the cache like every other cross target, because PE in that environment is written in
//! the GNU style and the import libraries for it are ours.
//!
//! Darwin has a line of its own, [`darwin_line`], and it is the same line on a Mac and anywhere
//! else. Everything it links against is in the SDK, which is found the way the header search finds
//! it, so a machine that is not a Mac links for one exactly when somebody has named an SDK with
//! `-isysroot` or `SDKROOT` and there is an `ld64.lld` to hand it to.
//!
//! The headers are the other half of a cross compile and [`crate::library::header_dirs`] is where
//! they are decided. It asks [`cross_sysroot`] the same question this file asks it, which is the
//! point: a compile that took its libc from the sysroot and its declarations from this machine would
//! be wrong in the quietest way available, and one function answering for both is what stops that
//! being possible.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rucc_session::Compress;
use rucc_sysroot::layout::{Kernel, Sysroot};
use rucc_sysroot::{Chip, Crt, LinkMode, argv};
use rucc_target::{Arch, Env, Os, Triple};
use rucc_tuple::TargetTuple;

/// What the command line said about linking.
///
/// Kept apart from `Options` because none of it reaches the compilation. A flag here changes what
/// the linker is told and changes nothing about the object files handed to it, which is why `-lm`
/// on a `-c` line is a note rather than an error.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LinkOptions {
    /// `-fuse-ld=<name>`, which names a linker rather than a path to one.
    pub use_ld: Option<String>,
    /// `-L<dir>`, in order, because the linker takes the first library it finds.
    pub search: Vec<PathBuf>,
    /// `-B<prefix>`, which is where to look for the linker before looking on the path.
    pub prefixes: Vec<PathBuf>,
    /// `--sysroot=<dir>`, which prefixes the directories this looks in.
    pub sysroot: Option<PathBuf>,
    /// Where the generated sysroots are, which is [`crate::cache::dir`] on a real command line.
    ///
    /// [`None`] is a caller that was not given one, which outside a test is nothing, and then there
    /// is no cross link line and a foreign target is refused the way it was before there was one.
    /// It is a field rather than a call inside this module because a link line that read the
    /// environment could only be tested on a machine whose environment said the right thing.
    pub cache: Option<PathBuf>,
    /// Where a distribution's cross packages put the tree for another architecture, which is
    /// `/usr` on a real command line.
    ///
    /// Debian and Ubuntu install `libc6-dev-arm64-cross` and its friends as `/usr/<multiarch>/include`
    /// and `/usr/<multiarch>/lib`, and gcc's own files for that target under
    /// `/usr/lib/gcc-cross/<multiarch>`. [`distro_cross`] reads it. A field for the reason
    /// [`LinkOptions::cache`] is one, and [`None`] in a test is a machine with no such packages.
    pub usr: Option<PathBuf>,
    /// `-static`.
    pub is_static: bool,
    /// `-shared`.
    pub shared: bool,
    /// `-pie` or `-no-pie`, and the platform's default when neither was written.
    pub pie: Option<bool>,
    /// `-nostdlib`, which is `-nostartfiles` and `-nodefaultlibs` together.
    pub no_stdlib: bool,
    /// `-nostartfiles`.
    pub no_startfiles: bool,
    /// `-nodefaultlibs`.
    pub no_defaultlibs: bool,
    /// `-rdynamic`, which puts every symbol in the dynamic table so a program can look itself up.
    pub export_dynamic: bool,
    /// `-s`, which drops the symbol table.
    pub strip: bool,
    /// `-mwindows` rather than `-mconsole`, whichever came last. Only a Windows line reads it.
    pub gui: bool,
    /// `-municode`, which only a Windows line reads.
    pub unicode: bool,
    /// `-fms-runtime-lib=`, which is `/MT` or `/MD` and which only an MSVC line reads.
    pub crt: Crt,
    /// `-fno-builtins-lib`, which leaves our own runtime off the line so that the machine's
    /// libgcc answers for everything instead.
    pub no_builtins_lib: bool,
    /// The whole ten field target when `--target=` spelled one, which is where a pinned libc
    /// release is.
    ///
    /// [`None`] is a command line that named no target at all, and then there is nothing pinned and
    /// this machine is the target. A `Triple` has room for an architecture, an OS and an
    /// environment and nowhere to put a release, so the release arrives here instead of there, and
    /// [`cross_sysroot`] reads it for both of the things it decides: whether this is a cross link
    /// and which directory under the cache it is against.
    pub pinned: Option<TargetTuple>,
    /// `-pg`, which changes the link as well as the code.
    ///
    /// The counts a profiled program keeps have to be started before `main` runs and written out
    /// after it returns, and what does both is a start file of its own. So a build that compiles
    /// with the flag and links without it produces a program that calls the hook on every function
    /// and never writes a profile.
    pub profile: bool,
    /// Whether `-Ofast`, `-ffast-math` or `-funsafe-math-optimizations` was in force at the end
    /// of the command line, which links `crtfastmath.o` into anything that is not a shared object.
    ///
    /// That file is a constructor which sets flush to zero and denormals are zero before `main`,
    /// so the mode is the process's rather than the unit's, and it is the half of fast math the
    /// compiler cannot give from inside a function.
    pub fast_math: bool,
    /// `-mdaz-ftz` or `-mno-daz-ftz`, which decides the same file outright and for a shared
    /// object as well.
    pub daz_ftz: Option<bool>,
    /// `-r`, which joins objects into one bigger object rather than into something that runs.
    ///
    /// Kbuild builds every directory into a `built-in.o` this way. The result is still an input to a
    /// later link, so it takes no startup files, no libraries and nothing about how it will be
    /// loaded, which is what gcc leaves off the line when it sees the flag.
    pub relocatable: bool,
    /// The deployment target on an Apple platform, from `-mmacosx-version-min=` and its friends or
    /// from the tuple, which `ld64` is told in `-platform_version` and checks every object against.
    ///
    /// [`None`] is the platform's default, the same one the object writer puts in
    /// `LC_BUILD_VERSION`, so that the two agree when neither was told anything.
    pub os_version: Option<rucc_tuple::Version>,
    /// `-gz`, which the linker is told so that what it writes stays compressed.
    pub compress: Compress,
}

impl LinkOptions {
    /// Whether gcc's `crtfastmath.o` goes on the line, which is its end file spec on x86-64.
    fn wants_fastmath(&self) -> bool {
        self.daz_ftz.unwrap_or(self.fast_math && !self.shared)
    }

    /// Whether the startup files go on the line.
    fn wants_startfiles(&self) -> bool {
        !self.no_stdlib && !self.no_startfiles && !self.relocatable
    }

    /// Whether the library the program was written against goes on the line.
    fn wants_defaultlibs(&self) -> bool {
        !self.no_stdlib && !self.no_defaultlibs && !self.relocatable
    }

    /// Whether the compiler's own runtime goes on the line.
    ///
    /// The same switch as the C library, because `-nodefaultlibs` in GCC means the compiler's
    /// runtime too, and a link that keeps `libgcc` while dropping `libc` is not a thing anyone
    /// asks for on purpose.
    fn wants_runtime(&self) -> bool {
        !self.no_stdlib && !self.no_defaultlibs && !self.relocatable
    }
}

/// One item on the link line, in the order it was written, because link order is semantic.
///
/// A library named before the object that needs it is not found on a static link, which is the
/// oldest surprise in the toolchain and the reason this is one ordered list rather than a list of
/// files and a list of libraries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A file: an object this compilation produced, or one named on the command line.
    File(String),
    /// `-l<name>`, which the linker resolves against its search path.
    Library(String),
    /// One word from `-Wl,` or `-Xlinker`, handed to the linker where the user wrote it.
    ///
    /// Here rather than in a list of its own because a great many of the linker's options are a
    /// bracket around the files after them, and an option moved away from what it brackets means
    /// something else or nothing at all. `--whole-archive` says that every member of every archive
    /// named after it goes in whether anything referenced it or not, `--start-group` says that the
    /// archives after it are searched again until nothing more comes out, and `-Bstatic` says which
    /// half of a library that ships both is wanted. Collecting them and appending them to the end
    /// leaves each of those pointing at nothing.
    Linker(String),
}

impl std::fmt::Display for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Item::File(path) => f.write_str(path),
            Item::Library(name) => write!(f, "-l{name}"),
            Item::Linker(arg) => write!(f, "-Wl,{arg}"),
        }
    }
}

/// Why a link could not be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No linker was found, after looking everywhere there was to look.
    NoLinker {
        /// The names that were tried, in the order they were tried.
        tried: Vec<String>,
    },
    /// `-fuse-ld=` named one that is not on this machine.
    Named {
        /// What it named.
        name: String,
    },
    /// A target this does not know how to build a link line for.
    Target {
        /// The triple that was asked for.
        triple: String,
    },
    /// A cross link this scheme cannot produce, which [`rucc_sysroot::argv`] has explained.
    ///
    /// The reason is carried as a sentence rather than as a variant per cause, because the causes
    /// live in `rucc-sysroot` and a second enumeration here would be a second thing to keep in step
    /// with them. What this adds is that the sentence came from a link rather than from a
    /// compilation.
    Cross {
        /// Why, in full, ready to print.
        why: String,
    },
    /// The sysroot a cross link needs is not on this machine.
    Sysroot {
        /// The target that was asked for.
        target: String,
        /// Where its sysroot would be.
        dir: String,
        /// Whether this release pins an artifact for that target, which decides whether the message
        /// can name a command that would fix it.
        pinned: bool,
    },
    /// The linker was found and cannot do this target's link.
    ///
    /// Separate from [`Error::NoLinker`] because the linker is there and runs, and separate from
    /// [`Error::Refused`] because the refusal is ours rather than its own: this is the case the
    /// linker would not complain about at all.
    TooOld {
        /// What it was found as, which is what to look for when replacing it.
        name: String,
        /// The major version it reported.
        found: u32,
        /// The target whose link it cannot do.
        target: String,
    },
    /// The linker was found and could not be started.
    Spawn {
        /// Where it was.
        path: String,
        /// What the operating system said.
        why: String,
    },
    /// The linker ran and said no.
    Refused {
        /// What it exited with, or a description when it was killed instead.
        status: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoLinker { tried } => {
                write!(f, "no linker was found; tried {}", tried.join(", "))?;
                // Only when lld was one of the names, because that is the linker every cross
                // target here is linked with and the one there is a single answer for.
                if tried.iter().any(|name| is_lld(name)) {
                    write!(
                        f,
                        ". lld {LLD_EXPORTAS} or newer links for every target, and {}",
                        lld_advice()
                    )?;
                }
                Ok(())
            }
            Error::Named { name } => {
                write!(f, "-fuse-ld={name} asks for a linker that is not on this machine")
            }
            Error::Target { triple } => {
                write!(f, "there is no link line for {triple} in this compiler yet")
            }
            Error::Cross { why } => f.write_str(why),
            // Two sentences and the second one changes, because a person whose link just failed
            // wants the command that fixes it and there is only a command to name when this release
            // pins an artifact for that target. Section 13.8's rule is that a compile which is
            // missing a sysroot says what to run rather than running it, and this is where it says
            // it.
            Error::Sysroot { target, dir, pinned: true } => write!(
                f,
                "there is no sysroot for {target} at {dir}, so there is nothing to link it \
                 against. `rucc --fetch {target}` gets the one this release pins, or pass \
                 --sysroot=<dir> to name a tree you have already"
            ),
            Error::Sysroot { target, dir, pinned: false } => write!(
                f,
                "there is no sysroot for {target} at {dir}, so there is nothing to link it \
                 against, and this release pins none for it to fetch. Pass --sysroot=<dir> to name \
                 a tree you have already, or see spec/cross-compile/13-distribution.md section \
                 13.2 for the cache that will hold one"
            ),
            // The whole message, because the person reading it has a linker that works, a link that
            // succeeded on their last try, and no reason to suspect the thing that is wrong.
            Error::TooOld { name, found, target } => write!(
                f,
                "{name} is lld {found} and cannot link for {target}. mingw-w64 writes a few hundred \
                 of its aliases, `_crt_atexit == atexit` among them, as IMPORT_NAME_EXPORTAS \
                 records in its import libraries, which lld learned to read in {LLD_EXPORTAS}. An \
                 older one neither reads them nor says so: it writes an import by ordinal zero, the \
                 link succeeds, and the program dies at startup. No newer lld was found either. \
                 For lld {LLD_EXPORTAS} or newer, {}, or name one with -fuse-ld=",
                lld_advice()
            ),
            Error::Spawn { path, why } => write!(f, "could not run the linker at {path}: {why}"),
            Error::Refused { status } => write!(f, "the linker {status}"),
        }
    }
}

impl std::error::Error for Error {}

/// A linker, found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linker {
    /// The name it is known by, which is what `--print-config` reports.
    pub name: String,
    /// Where it is, which is what gets spawned.
    pub path: PathBuf,
}

/// The names to look for, in the order section 4.9 gives.
///
/// `mold` first because it is dramatically faster, and a compiler that is twice the speed of
/// another one while the link takes twelve seconds has not helped anybody. Then `lld`, then the
/// platform's own. Each is looked for under both the bare name and the `ld.` prefix, because a
/// distribution installs `mold` under its own name and `ld.mold` for exactly this lookup.
#[must_use]
pub fn order(target: Triple, opts: &LinkOptions) -> Vec<String> {
    if let Some(named) = &opts.use_ld {
        // A name rather than a path, so `-fuse-ld=mold` finds a `mold` that is not `ld.mold`.
        return vec![format!("ld.{named}"), named.clone()];
    }
    // Apple's `ld` and lld's Mach-O flavour, and nothing from the list below. mold and `ld.lld` write
    // ELF, and a Mac with Homebrew's llvm on its `PATH` has an `ld.lld` that would take the line and
    // fail on its first flag.
    if target.os == Os::Darwin {
        return vec!["ld".to_owned(), "ld64.lld".to_owned()];
    }
    // The two linkers that read Microsoft's line, on every host. `ld.lld` is the same program as
    // `lld-link`, and which line it reads is decided by the name it was started under, so the name
    // is the whole of the choice.
    if is_msvc(target) {
        return vec!["lld-link".to_owned(), "link.exe".to_owned()];
    }
    if cross_sysroot(target, opts).is_some() {
        let mut names = cross_order(target);
        // The mingw-w64 sysroot this release fetches has import libraries that GNU ld cannot link
        // against: the `==` aliases are IMPORT_NAME_EXPORTAS records, and binutils reports every
        // one of them as an undefined reference to a symbol like `__imp__fmode`. So on this path
        // lld is the only linker worth finding, and a machine without one is told how to get it
        // instead of being handed a page of undefined references.
        if (target.os, target.env) == (Os::Windows, Env::Gnu) {
            names.retain(|name| is_lld(name));
        }
        return names;
    }
    if distro_cross(target, opts).is_some() {
        return cross_order(target);
    }
    match target.os {
        Os::Windows => vec!["lld-link".to_owned(), "link.exe".to_owned()],
        _ => vec![
            "ld.mold".to_owned(),
            "mold".to_owned(),
            "ld.lld".to_owned(),
            "lld".to_owned(),
            "ld".to_owned(),
        ],
    }
}

/// Whether this is a Windows target in Microsoft's environment, which is linked by `lld-link`.
fn is_msvc(target: Triple) -> bool {
    (target.os, target.env) == (Os::Windows, Env::Msvc)
}

/// The tree an MSVC link is against, which is always the one `--sysroot` named.
///
/// Never the cache, because the cache holds what a release of this compiler pins and nothing for
/// this environment ever will be: `spec/cross-compile/13-distribution.md` section 13.4. What
/// `--fetch` lays out is under a directory named for the versions it fetched, and it ends
/// by printing the `--sysroot` to pass, so a link with no `--sysroot` is one where that has not
/// happened yet and the answer is to say what to run.
///
/// # Errors
///
/// [`Error::Cross`] when no `--sysroot` was given.
fn msvc_sysroot(target: Triple, opts: &LinkOptions) -> Result<Sysroot, Error> {
    let tuple = target_tuple(target, opts);
    match &opts.sysroot {
        Some(root) => Ok(Sysroot::at(root.clone(), tuple)),
        None => Err(Error::Cross {
            why: format!(
                "a link for {tuple} is against Microsoft's C runtime and the Windows SDK, which \
                 this compiler may not ship. `rucc --fetch {tuple}` gets them from Microsoft once \
                 you accept Microsoft's licence and prints the --sysroot=<dir> to pass",
                tuple = tuple.to_canonical_string()
            ),
        }),
    }
}

/// The names to look for when the target is not this machine.
///
/// A shorter list than the one above and a different one, because most of that list cannot do this.
/// `spec/cross-compile/11-linking.md` section 11.2 settles it: `ld.lld` is the ELF cross linker,
/// since one binary of it links for every architecture it was built with and that is all of them.
/// mold is off the list because it links for the host and `wild` likewise, which is why section 11.2
/// has them as `-fuse-ld=` choices for a native link rather than as defaults. The platform's own
/// `ld` is off it for the same reason: a distribution's `/usr/bin/ld` is built for one architecture,
/// and `-fuse-ld=` is still there for somebody whose is not.
///
/// A cross binutils under its prefixed name is last, because a machine that has
/// `aarch64-linux-gnu-ld` installed has it on purpose. The prefix is a distribution convention and
/// there are two of them: a Linux target is filed under its multiarch name and a mingw-w64 one under
/// `<arch>-w64-mingw32`, which is what every distribution's mingw packages install. `ld.lld` is the
/// same binary for both, because its MinGW mode is a mode of the one linker rather than a second one.
fn cross_order(target: Triple) -> Vec<String> {
    let mut names = vec!["ld.lld".to_owned(), "lld".to_owned()];
    match (target.os, target.env) {
        (Os::Linux, _) => names.push(format!("{}-ld", multiarch(target))),
        (Os::Windows, Env::Gnu) => names.push(format!("{}-w64-mingw32-ld", target.arch.as_str())),
        _ => {}
    }
    names
}

/// The sysroot a cross link would use, or [`None`] for a link against this machine.
///
/// The one place the two paths are told apart, so that the linker that is looked for and the line it
/// is handed cannot disagree about which kind of link this is.
///
/// Three conditions, and two of them are about leaving working configurations alone. A target that
/// is this machine is linked against this machine, which is what every native compile has always
/// done and what the directories under `/usr/lib` are for. A `--sysroot` the user wrote is taken as
/// the root of a tree they assembled, and the line above prefixes every path it decides with it,
/// which is what cross compiling against a real distribution tree has always meant here. The third
/// is that there has to be a cache directory to look in, which on a real command line there always
/// is.
///
/// An unknown host counts as different from every target. A machine this compiler cannot name is a
/// machine whose `/usr/lib` it should not be guessing at.
///
/// # A pinned release is a cross compile
///
/// The first of those three conditions is about the machine and not about the triple, and a target
/// that names a libc release is not this machine even when it is this architecture. Somebody on a
/// 2.44 box writing `--target=x86_64-linux-gnu.2.28` is asking for a binary that runs on a 2.28
/// machine, and handing them their own headers and their own libc gives them a binary that does not.
/// So the condition is the triple being the host *and* no release named, and what it costs is that a
/// pin equal to this machine's own release also stops using this machine's libc. That is not a loss:
/// the two should be the same text, and if they are not then this machine's copy is patched and the
/// bundled tree is the one the pin asked for. tamnd/rucc#956.
#[must_use]
pub fn cross_sysroot(target: Triple, opts: &LinkOptions) -> Option<Sysroot> {
    cross_for(target, opts, Triple::host())
}

/// The same answer with the host as a parameter, so that both branches are testable on one machine.
fn cross_for(target: Triple, opts: &LinkOptions, host: Option<Triple>) -> Option<Sysroot> {
    if opts.sysroot.is_some() {
        return None;
    }
    let tuple = target_tuple(target, opts);
    // Windows is left out because a Windows machine has no C library of its own to compile
    // against. On an x86-64 Windows box the default target is the host, and the only headers
    // it can have are the mingw-w64 tree `--fetch` put in the cache.
    if host == Some(target) && target.os != Os::Windows && tuple.env_version().is_none() {
        return None;
    }
    if distro_for(target, opts, host).is_some() {
        return None;
    }
    let cache = opts.cache.as_deref()?;
    Some(Sysroot::in_cache(cache, tuple))
}

/// A tree a distribution's cross packages installed for a Linux target that is not this machine.
///
/// What `apt install gcc-aarch64-linux-gnu` leaves behind: the C library's headers and files under
/// `/usr/aarch64-linux-gnu`, and gcc's `crtbegin.o` and `libgcc.a` for that target under
/// `/usr/lib/gcc-cross/aarch64-linux-gnu/<version>`. The library's `libc.so` script names its files
/// by their full path, so the tree is linked where it is and not under a `--sysroot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro {
    /// `/usr/<multiarch>`, which has `include` and `lib` under it.
    pub root: PathBuf,
    /// gcc's directories for the target, newest version first, and empty when only the library
    /// was installed.
    pub gcc: Vec<PathBuf>,
}

impl Distro {
    /// The C library's headers, and the kernel's, which the same packages put in the same place.
    #[must_use]
    pub fn include(&self) -> PathBuf {
        self.root.join("include")
    }

    /// The C library's startup files and libraries.
    #[must_use]
    pub fn lib(&self) -> PathBuf {
        self.root.join("lib")
    }
}

/// The distribution's tree for this target, when a cross compile should use it.
///
/// Only when nothing better was asked for or is there. A `--sysroot` is a tree somebody named, a
/// pinned release is a request for our tree cut at that release, and a sysroot already fetched into
/// the cache is the one this release pins, so each of those wins. What is left is a machine that
/// has the distribution's cross packages and nothing of ours, and there the packages are what a
/// prefixed gcc on the same machine would use, so they are what this uses too.
#[must_use]
pub fn distro_cross(target: Triple, opts: &LinkOptions) -> Option<Distro> {
    distro_for(target, opts, Triple::host())
}

/// The same answer with the host as a parameter, for the same reason as [`cross_for`].
fn distro_for(target: Triple, opts: &LinkOptions, host: Option<Triple>) -> Option<Distro> {
    if opts.sysroot.is_some() || target.os != Os::Linux || host == Some(target) {
        return None;
    }
    let tuple = target_tuple(target, opts);
    if tuple.env_version().is_some() {
        return None;
    }
    if opts.cache.as_deref().is_some_and(|cache| Sysroot::in_cache(cache, tuple).lib().is_dir()) {
        return None;
    }
    let usr = opts.usr.as_deref()?;
    let name = multiarch(target);
    let root = usr.join(&name);
    if !root.join("include").is_dir() || !root.join("lib").is_dir() {
        return None;
    }
    let gcc = newest_first(&usr.join("lib/gcc-cross").join(&name));
    Some(Distro { root, gcc })
}

/// The target as the model that has room for a release, which is what names the cache directory.
///
/// The pinned spelling when there is one, because `x86_64-linux-gnu` and `x86_64-linux-gnu.2.28` are
/// two sysroots and not one: the release is in the tuple for the reason
/// `spec/cross-compile/03-target-model.md` section 3.2 admits a field at all, which is that it
/// changes what is compiled. A command line that named no target, or one whose spelling the ten
/// field parser did not take, falls back to what the three field one did.
fn target_tuple(target: Triple, opts: &LinkOptions) -> TargetTuple {
    opts.pinned.unwrap_or_else(|| target.tuple())
}

/// The kernel headers that go with [`cross_sysroot`], for the targets that have any.
///
/// The same three conditions, asked through the same function, because the two halves of one
/// target's system headers have to be decided together or a compile could read glibc's `sys/stat.h`
/// against this machine's `asm/stat.h`. A `None` here on a Linux target where the sysroot is `Some`
/// means only one thing, which is that the cache has no kernel tree for that architecture, and the
/// directory is still named for the reason [`crate::library::header_dirs`] gives.
///
/// Not under the sysroot, because `linux/` and `asm-generic/` are the same nine megabytes for every
/// target that shares an architecture, and a copy per target is eight copies of one thing.
#[must_use]
pub fn cross_kernel(target: Triple, opts: &LinkOptions) -> Option<Kernel> {
    kernel_for(target, opts, Triple::host())
}

/// The same answer with the host as a parameter, for the same reason as [`cross_for`].
fn kernel_for(target: Triple, opts: &LinkOptions, host: Option<Triple>) -> Option<Kernel> {
    cross_for(target, opts, host)?;
    Kernel::for_target(opts.cache.as_deref()?, target.tuple())
}

/// How the result is linked, as the five cases a sysroot link line is written over.
///
/// Four booleans reach here and five cases leave, because static and position independent are not
/// independent of each other and the start file differs in four of the five. The default for `pie`
/// is the one the native line above uses, so that a command line that says neither gets the same
/// answer whichever path it takes.
fn mode(opts: &LinkOptions) -> LinkMode {
    let pie = opts.pie.unwrap_or(!opts.is_static && !opts.shared);
    if opts.shared {
        LinkMode::Shared
    } else if opts.is_static {
        if pie { LinkMode::StaticPie } else { LinkMode::Static }
    } else if pie {
        LinkMode::Dynamic
    } else {
        LinkMode::DynamicNoPie
    }
}

/// The line for a machine that is not this one, from the target and the sysroot and nothing else.
///
/// Everything this knows is already in `opts`, and all it does is say it in the shape
/// [`rucc_sysroot::argv`] is written over. There is deliberately no decision here: a second place
/// that decided what goes on a cross link line would be a second place to get it wrong, and the
/// recorded lines under `tests/link-lines` would stop describing what this compiler does.
fn cross_line(
    target: Triple,
    opts: &LinkOptions,
    items: &[Item],
    output: &str,
    sysroot: &Sysroot,
) -> Result<Vec<String>, Error> {
    if opts.profile {
        // `gcrt1.o` is a compiled object out of the C library's own sources, and a generated sysroot
        // has the names a libc exports rather than the bodies behind them. Said here rather than
        // left to the linker, because what the linker would say is that `main` is undefined.
        return Err(Error::Cross {
            why: format!(
                "-pg needs gcrt1.o, or gcrt2.o on Windows, the startup file that starts and stops \
                 the counting, and a generated sysroot for {target} does not have one. Profile on \
                 the host, or pass --sysroot=<dir> naming a tree that has it"
            ),
        });
    }
    let inputs: Vec<argv::Item> = items
        .iter()
        .map(|item| match item {
            Item::File(path) => argv::Item::File(PathBuf::from(path)),
            Item::Library(name) => argv::Item::Library(name.clone()),
            Item::Linker(arg) => argv::Item::Linker(arg.clone()),
        })
        .collect();
    let output = PathBuf::from(output);
    // Ours, from beside the compiler, because that is where `cargo xtask builtins` writes it and a
    // fetched sysroot will never hold it. The cross line used to name it inside the sysroot, which
    // is a file nothing puts there, so every cross link either failed at the linker or quietly ran
    // against somebody else's `libgcc` copied in under the name. tamnd/rucc#1514.
    let ours = builtins_archive(target, &opts.prefixes);
    if ours.is_none() && opts.wants_runtime() && !opts.no_builtins_lib {
        // Said here rather than left to the linker, which on a Windows target says `___chkstk_ms`
        // is undefined and names mingw-w64's objects as the callers, and on a musl one says
        // `__udivti3` is. Neither of those is a person's first guess at a missing archive.
        let tuple = target.tuple().to_canonical_string();
        return Err(Error::Cross {
            why: format!(
                "a cross link ends with librucc_builtins.a, this compiler's own runtime for \
                 {tuple}, and there is none beside the compiler or under a -B prefix. A sysroot \
                 does not carry it, because it is our output rather than the platform's. Build it \
                 with `cargo xtask builtins --target={tuple}`, or pass -fno-builtins-lib to link \
                 without it"
            ),
        });
    }
    let invocation = argv::Invocation {
        inputs: &inputs,
        output: Some(&output),
        mode: mode(opts),
        search: &opts.search,
        no_startfiles: !opts.wants_startfiles(),
        no_defaultlibs: !opts.wants_defaultlibs(),
        no_builtins_lib: opts.no_builtins_lib,
        builtins: ours.as_deref(),
        export_dynamic: opts.export_dynamic,
        strip: opts.strip,
        gui: opts.gui,
        unicode: opts.unicode,
        crt: opts.crt,
    };
    argv::argv(target.tuple(), sysroot, &invocation)
        .map_err(|why| Error::Cross { why: why.to_string() })
}

/// Whether this link can be run at all, asked before anything is compiled.
///
/// Two questions that have answers before the first object exists: whether there is a line for this
/// target and mode at all, and whether the sysroot it would read is on the machine. Both are worth a
/// second at the start rather than a message after a minute of compiling, which is the same reason
/// the linker itself is looked for first.
///
/// The line is built rather than inspected, with no inputs and a name nothing will be written to,
/// because the refusals belong to the one function that builds it. A link against this machine has
/// nothing to answer here: its directories are looked for as the line is built and a missing one is
/// simply a directory that is not offered.
///
/// # Errors
///
/// [`Error::Cross`] for a target or a mode that has no line, and [`Error::Sysroot`] when the sysroot
/// it would be linked against is not there.
pub fn preflight(target: Triple, opts: &LinkOptions) -> Result<(), Error> {
    if opts.relocatable {
        return relocatable_line(target, opts, &[], "a.out").map(drop);
    }
    // Whether there is an SDK, which is the one thing a Darwin link can be missing that is worth
    // saying before everything has been compiled.
    if target.os == Os::Darwin {
        return darwin_line(target, opts, &[], "a.out").map(drop);
    }
    if is_msvc(target) {
        return msvc_preflight(target, opts);
    }
    let Some(sysroot) = cross_sysroot(target, opts) else { return Ok(()) };
    // Whether there is a line for this target and mode at all, asked with our own runtime left off
    // it. Otherwise a target nothing here can link and a machine where nobody built the runtime
    // report the same thing, and the archive is the smaller of the two problems by a long way.
    let shape = LinkOptions { no_builtins_lib: true, ..opts.clone() };
    cross_line(target, &shape, &[], "a.out", &sysroot)?;
    // The library directory rather than the root, because the root of a cache directory that has
    // been created and never populated is there and holds nothing. Section 11.6's rule is that
    // suitable is checked and not assumed, and this is the cheapest form of that.
    if !sysroot.lib().is_dir() {
        let tuple = target_tuple(target, opts).to_canonical_string();
        return Err(Error::Sysroot {
            dir: sysroot.root().display().to_string(),
            pinned: rucc_sysroot::pinned_for_target(target_tuple(target, opts)).is_some(),
            target: tuple,
        });
    }
    // And now the whole line, which is the sysroot's files plus ours, so that a missing runtime is
    // said here rather than by the linker after everything has been compiled.
    cross_line(target, opts, &[], "a.out", &sysroot)?;
    Ok(())
}

/// [`preflight`] for Microsoft's environment, which is the same three questions about a tree
/// somebody named rather than one in the cache.
///
/// Whether the tree is one is asked of the CRT's directory for this architecture, because that is
/// the one every line needs and the one a tree fetched for another architecture does not have.
fn msvc_preflight(target: Triple, opts: &LinkOptions) -> Result<(), Error> {
    let sysroot = msvc_sysroot(target, opts)?;
    let shape = LinkOptions { no_builtins_lib: true, ..opts.clone() };
    cross_line(target, &shape, &[], "a.exe", &sysroot)?;
    let tuple = target_tuple(target, opts);
    if let Some(chip) = Chip::of(tuple) {
        let crt = sysroot.root().join("crt").join("lib").join(chip.in_tree());
        if !crt.is_dir() {
            return Err(Error::Cross {
                why: format!(
                    "{} has no {}, so it is not a tree for {tuple} to link against. `rucc \
                     --fetch {tuple}` lays one out and prints where",
                    sysroot.root().display(),
                    crt.display(),
                    tuple = tuple.to_canonical_string()
                ),
            });
        }
    }
    cross_line(target, opts, &[], "a.exe", &sysroot)?;
    Ok(())
}

/// Writes the stub libraries a glibc cross link reads, into [`Sysroot::stubs`].
///
/// `spec/cross-compile/09-libc-stubs.md` section 9.1: the stubs are generated on demand rather than
/// shipped, out of the description `rucc-stub` carries, cut at the release the tuple names or at
/// the bundled one when it names none. So there is nothing to fetch for them and nothing to go
/// stale, and a pin is a different directory rather than a different download.
///
/// A file is written only when its bytes differ from what is there, and then through a temporary
/// name and a rename, because two builds for one target run side by side all the time and a
/// linker must never read a half written `libc.so`. The bytes are the same on every host, so two
/// processes racing to write them race to write the same thing.
///
/// Nothing for a link against this machine, a `--sysroot` the user named, or a target that is not
/// glibc. A glibc release newer than the bundled tree was already refused when the headers were
/// chosen, so it is quietly nothing here too.
///
/// # Errors
///
/// [`Error::Cross`] when the stubs cannot be generated for this target or cannot be written.
pub fn write_stubs(target: Triple, opts: &LinkOptions) -> Result<(), Error> {
    let Some(sysroot) = cross_sysroot(target, opts) else { return Ok(()) };
    let tuple = sysroot.target();
    if rucc_stub::glibc::architecture(tuple).is_none() {
        return Ok(());
    }
    let Ok(Some(minor)) = rucc_sysroot::bundled_glibc_minor(tuple) else { return Ok(()) };
    let files = rucc_stub::glibc::stubs(tuple, minor).map_err(|why| Error::Cross {
        why: format!("the glibc stubs for {}: {why}", tuple.to_canonical_string()),
    })?;
    let dir = sysroot.stubs();
    let failed = |path: &Path, why: std::io::Error| Error::Cross {
        why: format!("{} cannot be written: {why}", path.display()),
    };
    fs::create_dir_all(dir).map_err(|why| failed(dir, why))?;
    for file in files {
        let path = dir.join(&file.name);
        if fs::read(&path).is_ok_and(|there| there == file.bytes) {
            continue;
        }
        let temporary = dir.join(format!(".{}.{}", file.name, std::process::id()));
        fs::write(&temporary, &file.bytes).map_err(|why| failed(&temporary, why))?;
        fs::rename(&temporary, &path).map_err(|why| {
            let _ = fs::remove_file(&temporary);
            failed(&path, why)
        })?;
    }
    Ok(())
}

/// The linker to use, looked for where a linker is.
///
/// `-B` prefixes first, since the point of one is to put a toolchain in front of the machine's,
/// then the path. A name that contains a separator is a path and is taken as one, which is what
/// gcc does with `-fuse-ld=/usr/bin/ld.gold` and what a build system relying on that expects.
///
/// # Errors
///
/// [`Error::Named`] when `-fuse-ld=` asked for one that is not here, and [`Error::NoLinker`] when
/// nothing was, which name the candidates so that the message says what was looked for.
pub fn find(target: Triple, opts: &LinkOptions) -> Result<Linker, Error> {
    let tried = order(target, opts);
    let places = lld_dirs(Path::new("/"), std::env::var_os("ProgramFiles").map(PathBuf::from));
    // The first linker [`suitable`] turned down, which is the answer when nothing after it is any
    // better. Ubuntu 24.04 has lld 18 on PATH and lld 19 under /usr/lib/llvm-19/bin once somebody
    // installs `lld-19`, and the second is the one to use without asking them to change PATH.
    let mut refused = None;
    for name in &tried {
        for path in linker_candidates(name, opts, &places) {
            let linker = Linker { name: name.clone(), path };
            match suitable(target, &linker) {
                Ok(()) => return Ok(linker),
                Err(why) => {
                    refused.get_or_insert(why);
                }
            }
        }
    }
    if let Some(why) = refused {
        return Err(why);
    }
    match &opts.use_ld {
        Some(name) => Err(Error::Named { name: name.clone() }),
        None => Err(Error::NoLinker { tried }),
    }
}

/// Every file a linker of this name could be, in the order they are asked.
///
/// A name with a separator in it is a path and is the only answer. Otherwise the `-B` prefixes,
/// then every directory on `PATH` rather than the first hit, then the places an lld is installed
/// without being put on `PATH`. All of them rather than the first, because the first may be an lld
/// that [`suitable`] turns down and a later one may not be.
fn linker_candidates(name: &str, opts: &LinkOptions, places: &[PathBuf]) -> Vec<PathBuf> {
    if name.contains(std::path::MAIN_SEPARATOR) || name.contains('/') {
        let path = PathBuf::from(name);
        return if path.is_file() { vec![path] } else { Vec::new() };
    }
    let pathext = pathext();
    let mut found: Vec<PathBuf> = opts
        .prefixes
        .iter()
        .flat_map(|dir| spellings(&dir.join(name), &pathext))
        .filter(|path| path.is_file())
        .collect();
    if let Some(path) = std::env::var_os("PATH") {
        found.extend(
            std::env::split_paths(&path)
                .flat_map(|dir| spellings(&dir.join(name), &pathext))
                .filter(|p| executable(p)),
        );
    }
    // The same spellings as on PATH, so `ld.lld` here and `ld.lld.exe` on Windows. Only `.exe`
    // was tried at first, which found nothing in /usr/lib/llvm-19/bin on the Linux machines this
    // search was written for.
    if is_lld(name) {
        for dir in places {
            for file in spellings(&dir.join(name), &pathext) {
                if executable(&file) && !found.contains(&file) {
                    found.push(file);
                }
            }
        }
    }
    found
}

/// The extensions a program name is tried with, from `PATHEXT` on Windows and none elsewhere.
///
/// `ld.lld` on Windows is `ld.lld.exe`, and looking for the bare name finds nothing, which is how
/// a Windows host with LLVM installed and on `PATH` used to be told there was no linker. `PATHEXT`
/// is what `cmd.exe` uses for the same question, and when it is unset the list is the one
/// Windows ships with.
fn pathext() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let list = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    list.split(';').filter(|ext| ext.starts_with('.')).map(str::to_ascii_lowercase).collect()
}

/// A path with each extension added, or as given when there are none or it already ends in one.
///
/// `ld.lld` ends in `.lld`, which is not an extension anybody runs, so the check is against the
/// list rather than against whether there is a dot in the name at all. The bare name is left out
/// when there is a list, because a file called `ld.lld` in a Git Bash directory on Windows is a
/// shell script that `CreateProcess` cannot start.
fn spellings(path: &Path, exts: &[String]) -> Vec<PathBuf> {
    let has = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| exts.iter().any(|e| e[1..].eq_ignore_ascii_case(ext)));
    if exts.is_empty() || has {
        return vec![path.to_path_buf()];
    }
    exts.iter()
        .map(|ext| {
            let mut name = path.as_os_str().to_owned();
            name.push(ext);
            PathBuf::from(name)
        })
        .collect()
}

/// Whether a name is one of the spellings lld is installed under.
fn is_lld(name: &str) -> bool {
    matches!(name, "ld.lld" | "ld64.lld" | "lld" | "lld-link")
}

/// Where lld is installed without being on `PATH`, newest first where a version is in the name.
///
/// Homebrew's `lld` and `llvm` formulae, whose `llvm` is keg-only and so never on `PATH` unless
/// somebody put it there. Debian and Ubuntu's `lld-<N>` packages, which put the real program in
/// `/usr/lib/llvm-<N>/bin` and only a versioned name in `/usr/bin`. Fedora's compatibility packages,
/// which do the same under `/usr/lib64/llvm<N>/bin`. And the LLVM installer for Windows, which puts
/// everything in `%ProgramFiles%\LLVM\bin` and leaves adding it to `PATH` as a checkbox that is off.
///
/// `root` is `/` except in the tests, which build the same tree somewhere they are allowed to.
fn lld_dirs(root: &Path, program_files: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        "opt/homebrew/opt/lld/bin",
        "opt/homebrew/opt/llvm/bin",
        "usr/local/opt/lld/bin",
        "usr/local/opt/llvm/bin",
        "home/linuxbrew/.linuxbrew/opt/lld/bin",
        "home/linuxbrew/.linuxbrew/opt/llvm/bin",
    ]
    .iter()
    .map(|dir| root.join(dir))
    .collect();
    for (parent, prefix) in [("usr/lib", "llvm-"), ("usr/lib64", "llvm")] {
        let mut versions: Vec<(u32, PathBuf)> = fs::read_dir(root.join(parent))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let version = name.to_str()?.strip_prefix(prefix)?.parse().ok()?;
                Some((version, entry.path().join("bin")))
            })
            .collect();
        versions.sort_by_key(|(version, _)| std::cmp::Reverse(*version));
        dirs.extend(versions.into_iter().map(|(_, dir)| dir));
    }
    if let Some(dir) = program_files {
        dirs.push(dir.join("LLVM").join("bin"));
    }
    dirs
}

/// Where to get an lld on the machine this compiler is running on, as the end of a sentence.
///
/// One per host rather than a list of every package manager, because the person reading it has one
/// machine and wants the one command for it. Each names a place [`lld_dirs`] looks, so that doing
/// what it says is enough and nobody has to edit `PATH` afterwards.
fn lld_advice() -> &'static str {
    if cfg!(target_os = "macos") {
        "`brew install lld` installs one, and rucc finds it in Homebrew's directory without a \
         change to PATH"
    } else if cfg!(windows) {
        "the LLVM installer from https://github.com/llvm/llvm-project/releases, or `winget install \
         LLVM.LLVM`, installs one in %ProgramFiles%\\LLVM\\bin, where rucc finds it without a \
         change to PATH"
    } else {
        "on Debian and Ubuntu `apt install lld-19` installs one in /usr/lib/llvm-19/bin and on \
         Fedora `dnf install lld` installs one on PATH, and rucc finds either without a change to \
         PATH"
    }
}

/// The first lld that reads `IMPORT_NAME_EXPORTAS`, which is what a windows-gnu link needs.
///
/// 18 does not read it and does not say so, so the number is not a convenience: below it the
/// answer is wrong rather than absent. tamnd/rucc#1515.
pub const LLD_EXPORTAS: u32 = 19;

/// Whether a found linker can do this target's link, asked before it is handed anything.
///
/// Section 11.6's rule is that suitable is checked and not assumed, and this is the one check that
/// cannot be made by looking at a file. A windows-gnu link reads import libraries that mingw-w64's
/// `==` aliases compiled into `IMPORT_NAME_EXPORTAS` records, which lld reads from
/// [`LLD_EXPORTAS`] on. An older lld writes an import by ordinal zero instead, without a warning
/// and with a successful exit, so nothing later in the toolchain has anything to notice: the
/// program is wrong at startup and the link that made it said nothing. Ubuntu 24.04 is the current
/// LTS and ships 18, so the machine this happens on is an ordinary one.
///
/// Every other target is left alone, and so is anything that is not an lld, because this is the one
/// version of the one linker that is known to answer wrongly rather than not at all.
///
/// A linker that will not run or whose version cannot be read is allowed through. What the check
/// can establish is that a specific old lld is here, and it should not turn every unusual linker
/// into a refusal on the strength of failing to recognise it.
///
/// # Errors
///
/// [`Error::TooOld`] when the linker is an lld older than [`LLD_EXPORTAS`] and the target is
/// windows-gnu.
pub fn suitable(target: Triple, linker: &Linker) -> Result<(), Error> {
    if (target.os, target.env) != (Os::Windows, Env::Gnu) {
        return Ok(());
    }
    let Some(found) = lld_major(&reported_version(&linker.path)) else { return Ok(()) };
    if found >= LLD_EXPORTAS {
        return Ok(());
    }
    Err(Error::TooOld {
        name: linker.name.clone(),
        found,
        target: target.tuple().to_canonical_string(),
    })
}

/// What `<linker> --version` prints, or an empty string when it will not say.
///
/// A linker that cannot be started is not this function's problem to report, because the link is
/// about to start it again and say so properly. What this returns for such a one is nothing to
/// read, which is the same as a linker that ran and said something unrecognisable.
fn reported_version(path: &Path) -> String {
    let Ok(out) = Command::new(path).arg("--version").output() else { return String::new() };
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The major version in an lld's `--version`, when the program that printed it was an lld.
///
/// What lld prints is `LLD 18.1.8 (compatible with GNU linkers)`, with a distribution's own prefix
/// in front of it often enough that the word is looked for rather than the line starting with it:
/// Ubuntu's says `Ubuntu LLD 18.1.3`. Binutils prints `GNU ld (GNU Binutils for Ubuntu) 2.42` and
/// mold prints its own name, and neither has the word, so both come back as [`None`] and are left
/// alone.
fn lld_major(text: &str) -> Option<u32> {
    let mut words = text.split_whitespace();
    words.find(|word| *word == "LLD")?;
    words.next()?.split('.').next()?.parse().ok()
}

/// Whether a path is a file this process could run.
#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Whether a path is a file this process could run.
///
/// Windows has no executable bit and decides by extension, and the names looked for above carry
/// theirs, so being a file is the whole of the question here.
#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.is_file()
}

/// What the linker is told, in order, not counting the linker itself.
///
/// Two lines and [`cross_sysroot`] picks which: the one above for this machine, and
/// [`rucc_sysroot::argv`]'s for any other. Nothing about the machine is read on the second path, so
/// `-###` prints the same line on every host and prints it whether the sysroot has been built or
/// not, which is what makes it worth printing.
///
/// # Errors
///
/// [`Error::Target`] for a platform there is no native line for yet, which is every one but Linux,
/// and [`Error::Cross`] for a cross link that cannot be produced at all.
pub fn line(
    target: Triple,
    opts: &LinkOptions,
    items: &[Item],
    output: &str,
) -> Result<Vec<String>, Error> {
    let mut args = line_for(target, opts, items, output)?;
    // `-gz` on a link, which gcc hands to the linker so the output keeps its debug sections
    // compressed too. Only an ELF linker has the option, and on the others the sections are left
    // as the objects had them, which is what the object writer does on those formats as well.
    if !matches!(target.os, Os::Darwin | Os::Windows) {
        match opts.compress {
            Compress::None => {}
            how => args.push(format!("--compress-debug-sections={how}")),
        }
    }
    Ok(args)
}

/// [`line`], before `-gz` is added to it.
fn line_for(
    target: Triple,
    opts: &LinkOptions,
    items: &[Item],
    output: &str,
) -> Result<Vec<String>, Error> {
    if opts.relocatable {
        return relocatable_line(target, opts, items, output);
    }
    if target.os == Os::Darwin {
        return darwin_line(target, opts, items, output);
    }
    if is_msvc(target) {
        return cross_line(target, opts, items, output, &msvc_sysroot(target, opts)?);
    }
    if let Some(sysroot) = cross_sysroot(target, opts) {
        return cross_line(target, opts, items, output, &sysroot);
    }
    if target.os != Os::Linux {
        return Err(Error::Target { triple: target.to_string() });
    }
    let machine = emulation(target);
    let root = opts.sysroot.as_deref();
    // A distribution's cross tree is linked by the same line, with its two directories in place of
    // this machine's, because it is laid out the way this machine's own library is.
    let distro = distro_cross(target, opts);
    let dirs = match &distro {
        Some(distro) => vec![distro.lib()],
        None => library_dirs(target, root),
    };
    // Where a gcc on this machine keeps its own runtime, which is a different place from where
    // the C library keeps its own, and where our runtime is if it was built for this target.
    let runtime = match distro {
        Some(distro) => distro.gcc,
        None => runtime_dirs(target, root),
    };
    let ours = if opts.no_builtins_lib { None } else { builtins_archive(target, &opts.prefixes) };
    let mut args = vec![
        "-o".to_owned(),
        output.to_owned(),
        // Which of the several formats one `ld` can write is meant. A linker built for more than
        // one machine guesses from its first input otherwise, and a link of no objects at all has
        // nothing to guess from.
        "-m".to_owned(),
        machine.to_owned(),
        // The table a program unwinds through, which a C program with no exceptions in it still
        // needs because `backtrace` and every crash handler read it.
        "--eh-frame-hdr".to_owned(),
        // The symbol hash a dynamic loader from this century reads. The old one is still written
        // alongside by default on some distributions, and asking for this one is what stops a link
        // from carrying a table nothing has needed since 2006.
        "--hash-style=gnu".to_owned(),
    ];

    let pie = opts.pie.unwrap_or(!opts.is_static && !opts.shared);
    if opts.shared {
        args.push("-shared".to_owned());
    } else if opts.is_static {
        args.push("-static".to_owned());
    } else if pie {
        args.push("-pie".to_owned());
    } else {
        args.push("-no-pie".to_owned());
    }
    if !opts.is_static && !opts.shared {
        args.push("-dynamic-linker".to_owned());
        args.push(target_path(root, loader(target)));
    }
    if opts.export_dynamic {
        args.push("--export-dynamic".to_owned());
    }
    if opts.strip {
        args.push("-s".to_owned());
    }

    if opts.wants_startfiles() {
        for name in startfile(opts, pie).into_iter().chain(["crti.o"]) {
            if let Some(path) = find_file(&dirs, name) {
                args.push(path.display().to_string());
            }
        }
        // The compiler's own startup file, which runs the static constructors. Three spellings
        // of the same thing, and which one is right is about how the code in it refers to
        // itself: `S` for a position independent result, `T` for a static one, plain for the
        // rest. Skipped when there is no gcc on the machine to take it from, because a program
        // with no constructor in it does not miss it.
        let begin = if opts.shared || pie {
            "crtbeginS.o"
        } else if opts.is_static {
            "crtbeginT.o"
        } else {
            "crtbegin.o"
        };
        if let Some(path) = find_file(&runtime, begin).or_else(|| find_file(&runtime, "crtbegin.o"))
        {
            args.push(path.display().to_string());
        }
    }

    for dir in &opts.search {
        args.push(format!("-L{}", dir.display()));
    }
    for dir in &dirs {
        args.push(format!("-L{}", dir.display()));
    }
    // Where `libgcc.a` and `libgcc_eh.a` are, which is not where the C library is. Nothing is
    // added when there is no gcc on the machine, and then the `-l` names below are left off too.
    for dir in &runtime {
        args.push(format!("-L{}", dir.display()));
    }

    for item in items {
        match item {
            Item::File(path) => args.push(path.clone()),
            Item::Library(name) => args.push(format!("-l{name}")),
            Item::Linker(arg) => args.push(arg.clone()),
        }
    }
    // After the objects, because a static archive is searched for what is undefined at the point
    // it is reached and a library named before the object that needs it contributes nothing.
    args.extend(runtime_items(opts, &runtime, ours.as_deref()));

    if opts.wants_startfiles() {
        // The other end of `crtbegin`, and it goes before `crtn.o` for the same reason `crti.o`
        // goes before `crtbegin`: the four are two nested pairs and not four separate files.
        // The fast math constructor, ahead of `crtend` where gcc puts it. Skipped when there is
        // no gcc to take it from, as `crtbegin` is.
        if opts.wants_fastmath() {
            if let Some(path) = find_file(&runtime, "crtfastmath.o") {
                args.push(path.display().to_string());
            }
        }
        let end = if opts.shared || pie { "crtendS.o" } else { "crtend.o" };
        if let Some(path) = find_file(&runtime, end).or_else(|| find_file(&runtime, "crtend.o")) {
            args.push(path.display().to_string());
        }
        if let Some(path) = find_file(&dirs, "crtn.o") {
            args.push(path.display().to_string());
        }
    }

    Ok(args)
}

/// The line for `-r`, which is the objects and the machine and nothing else.
///
/// No sysroot is read, because a relocatable link takes nothing from the C library, so this is the
/// same line for this machine and for any other Linux target. The `-L` directories the command
/// line gave are kept for a `-l` written on it, which the linker still resolves against archives.
fn relocatable_line(
    target: Triple,
    opts: &LinkOptions,
    items: &[Item],
    output: &str,
) -> Result<Vec<String>, Error> {
    if target.os == Os::Darwin {
        // `ld64` joins objects with `-r` too, and needs only to be told the architecture, since
        // nothing from the SDK goes into an object that is still going to be linked.
        let mut args = vec![
            "-r".to_owned(),
            "-arch".to_owned(),
            darwin_arch(target)?.to_owned(),
            "-o".to_owned(),
            output.to_owned(),
        ];
        for dir in &opts.search {
            args.push(format!("-L{}", dir.display()));
        }
        push_items(&mut args, items);
        return Ok(args);
    }
    if target.os != Os::Linux {
        return Err(Error::Target { triple: target.to_string() });
    }
    let mut args = vec![
        "-o".to_owned(),
        output.to_owned(),
        "-m".to_owned(),
        emulation(target).to_owned(),
        "-r".to_owned(),
    ];
    if opts.strip {
        args.push("-s".to_owned());
    }
    for dir in &opts.search {
        args.push(format!("-L{}", dir.display()));
    }
    for item in items {
        match item {
            Item::File(path) => args.push(path.clone()),
            Item::Library(name) => args.push(format!("-l{name}")),
            Item::Linker(arg) => args.push(arg.clone()),
        }
    }
    Ok(args)
}

/// The line `ld64` takes, which is Apple's `ld` or lld's Mach-O flavour.
///
/// Shorter than the ELF one, because the SDK carries everything a program starts with. There are no
/// start files to find: `libSystem` has the C library, the startup code and the runtime in it, and
/// `dyld` calls `main` itself. What the linker has to be told is the architecture, the platform with
/// the oldest release the program runs on and the SDK it was built against, and where the SDK is,
/// which `-syslibroot` puts in front of every library it looks for.
///
/// `-pie` and `-no-pie` are not passed on. Every arm64 program on a Mac is position independent and
/// `ld64` rejects `-no_pie` there, so the flag has nothing to change. `-static` is refused, because
/// Apple ships no static C library and a static executable is a kernel's business.
///
/// # Errors
///
/// [`Error::Cross`] when there is no SDK to link against, or for a static link, or an architecture
/// that has no Mach-O name.
pub fn darwin_line(
    target: Triple,
    opts: &LinkOptions,
    items: &[Item],
    output: &str,
) -> Result<Vec<String>, Error> {
    let arch = darwin_arch(target)?;
    if opts.is_static && !opts.shared {
        return Err(Error::Cross {
            why: "there is no static C library for Apple platforms, so -static cannot make a \
                  program for one"
                .to_owned(),
        });
    }
    let Some(sdk) = crate::library::sdk(opts.sysroot.as_deref()) else {
        return Err(Error::Cross {
            why: format!(
                "no SDK was found to link {} against. Install the command line tools with \
                 `xcode-select --install`, or name one with -isysroot <dir> or SDKROOT",
                target_tuple(target, opts).to_canonical_string()
            ),
        });
    };
    let tuple = target_tuple(target, opts);
    let (platform, default) = match (tuple.os(), tuple.env()) {
        (rucc_tuple::Os::IOs, rucc_tuple::Env::Simulator) => ("ios-simulator", "14.0"),
        (rucc_tuple::Os::IOs, rucc_tuple::Env::MacAbi) => ("mac-catalyst", "14.0"),
        (rucc_tuple::Os::IOs, _) => ("ios", "14.0"),
        _ => ("macos", "11.0"),
    };
    let minimum = opts
        .os_version
        .or_else(|| tuple.os_version())
        .map_or_else(|| default.to_owned(), |version| version.to_string());
    // What the SDK says it is, which `ld64` records so the loader can tell which behaviours the
    // program was built to expect. The deployment target when the SDK does not say, which is the
    // answer that asks for no behaviour newer than the program claims to run on.
    let version = sdk_version(&sdk).unwrap_or_else(|| minimum.clone());

    let mut args = vec![
        "-arch".to_owned(),
        arch.to_owned(),
        "-platform_version".to_owned(),
        platform.to_owned(),
        minimum,
        version,
        "-syslibroot".to_owned(),
        sdk.display().to_string(),
        "-o".to_owned(),
        output.to_owned(),
    ];
    if opts.shared {
        args.push("-dylib".to_owned());
    }
    if opts.export_dynamic {
        args.push("-export_dynamic".to_owned());
    }
    if opts.strip {
        // The debug map and the local symbols, which between them are what `-s` leaves out of an
        // ELF program. `ld64` has no one flag for it.
        args.push("-S".to_owned());
        args.push("-x".to_owned());
    }
    for dir in &opts.search {
        args.push(format!("-L{}", dir.display()));
    }
    push_items(&mut args, items);
    if opts.wants_runtime() && !opts.no_builtins_lib {
        if let Some(ours) = builtins_archive(target, &opts.prefixes) {
            args.push(ours.display().to_string());
        }
    }
    if opts.wants_defaultlibs() {
        args.push("-lSystem".to_owned());
    }
    Ok(args)
}

/// What `ld64` calls the architecture, which is not what the triple does.
fn darwin_arch(target: Triple) -> Result<&'static str, Error> {
    match target.arch {
        Arch::Aarch64 => Ok("arm64"),
        Arch::X86_64 => Ok("x86_64"),
        _ => Err(Error::Target { triple: target.to_string() }),
    }
}

/// The version an SDK says it is, from the `SDKSettings.json` every SDK since Xcode 7 has at its
/// root, or from its directory name, which is `MacOSX15.2.sdk` when it is not the unversioned link.
///
/// Read by hand rather than parsed, because the one field wanted is a quoted string at the top
/// level and a JSON parser would be a dependency for one line.
fn sdk_version(sdk: &Path) -> Option<String> {
    let valid = |text: &str| {
        !text.is_empty()
            && text
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    };
    if let Ok(text) = fs::read_to_string(sdk.join("SDKSettings.json")) {
        if let Some(at) = text.find("\"Version\"") {
            let rest = &text[at + "\"Version\"".len()..];
            let rest = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
            let version = &rest[..rest.find('"')?];
            if valid(version) {
                return Some(version.to_owned());
            }
        }
    }
    let name = sdk.file_name()?.to_str()?.strip_suffix(".sdk")?;
    let version = name.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    valid(version).then(|| version.to_owned())
}

/// The objects, libraries and linker words, in the order they were written.
fn push_items(args: &mut Vec<String>, items: &[Item]) {
    for item in items {
        match item {
            Item::File(path) => args.push(path.clone()),
            Item::Library(name) => args.push(format!("-l{name}")),
            Item::Linker(arg) => args.push(arg.clone()),
        }
    }
}

/// The startup file the C library brings, or `None` for a link that calls nothing.
///
/// This is what calls `main` and what passes it the arguments, so a shared object takes none of
/// them: nothing starts one and it has no `main` to be started at. `Scrt1.o` rather than `crt1.o`
/// when the result moves, because the two differ in whether the reference to `main` in them is one
/// a loader may relocate.
///
/// A profiled program gets a different one again, which does all of that and starts and stops the
/// counting around it. There are two of those rather than three: the one that relocates itself is
/// only needed by a static position independent link, and every other link takes the plain one,
/// which is what gcc does with the same flag.
fn startfile(opts: &LinkOptions, pie: bool) -> Option<&'static str> {
    if opts.shared {
        None
    } else if opts.profile {
        Some(if pie && opts.is_static { "grcrt1.o" } else { "gcrt1.o" })
    } else if pie {
        Some("Scrt1.o")
    } else {
        Some("crt1.o")
    }
}

/// The libraries the compiler's own runtime contributes, in the order the linker wants them.
///
/// The C library first, then ours, then the machine's `libgcc`. Order inside this list is not
/// about whether a symbol resolves, it is about which archive supplies one that more than one of
/// them defines, and the two places that happens both have a right answer.
///
/// `memcpy` and its three neighbours are in the C library on a hosted target and in ours only for
/// a freestanding one, which is what `spec/12-abi-and-runtime.md` section 12.8 says they are for.
/// glibc's are written in assembly per microarchitecture and ours is a word at a time loop, so a
/// link that took ours over glibc's would be slower at the one routine every program reaches.
///
/// The wide arithmetic is in ours and in `libgcc` both, and the two are ABI-identical on purpose,
/// so which one answers is not a correctness question. Ours comes first because it is ours, and
/// `-fno-builtins-lib` leaves it off for somebody who would rather it were not.
///
/// A static link puts the whole list inside `--start-group`. `libc.a` refers to `_Unwind_Resume`,
/// and the unwinder refers back into `libc.a`, so a linker walking the list once resolves
/// whichever it reaches first and reports the other as undefined. That is exactly the failure
/// issue #277 describes and the group is the fix for it.
///
/// A dynamic link needs no group, because the shared `libc` resolves its own references inside
/// itself. `libgcc_s` is asked for `--as-needed` there, the way gcc asks for it, so a program that
/// never unwinds does not acquire a dependency on it.
fn runtime_items(opts: &LinkOptions, runtime: &[PathBuf], ours: Option<&Path>) -> Vec<String> {
    let mut args = Vec::new();
    if !opts.wants_defaultlibs() && !opts.wants_runtime() {
        return args;
    }
    // Only when there is a gcc to take them from. On a machine without one the names would be an
    // error about a library that was never going to be there, and a program that needs neither
    // the unwinder nor a wide divide links and runs without them.
    let has_gcc = find_file(runtime, "libgcc.a").is_some();

    if opts.is_static {
        args.push("--start-group".to_owned());
    }
    if opts.wants_defaultlibs() {
        args.push("-lc".to_owned());
    }
    if opts.wants_runtime() {
        if let Some(path) = ours {
            args.push(path.display().to_string());
        }
        if has_gcc {
            args.push("-lgcc".to_owned());
            if opts.is_static {
                args.push("-lgcc_eh".to_owned());
            }
        }
    }
    if opts.is_static {
        args.push("--end-group".to_owned());
    } else if opts.wants_runtime() && has_gcc {
        // The shared half, and only if something still wants it after everything above.
        args.push("--as-needed".to_owned());
        args.push("-lgcc_s".to_owned());
        args.push("--no-as-needed".to_owned());
    }
    args
}

/// Where a gcc on this machine keeps `crtbegin.o`, `crtend.o` and `libgcc.a`, newest first.
///
/// This is not where the C library's files are. A distribution puts them under a directory named
/// for the gcc version, and there may be several, so the answer is every one that exists with the
/// highest version in front. Newest first because a newer `libgcc` is a superset of an older one
/// and because that is the one the C library on the same machine was built against.
#[must_use]
pub fn runtime_dirs(target: Triple, sysroot: Option<&Path>) -> Vec<PathBuf> {
    let libc = match target.env {
        Env::Musl => "musl",
        Env::None | Env::Gnu | Env::Msvc => "gnu",
    };
    let arch = target.arch.as_str();
    // The spellings the distributions use for the same triple. Debian and Ubuntu drop the vendor
    // field, the source builds and Arch keep `pc`, and Red Hat and SUSE write their own name in
    // it, so all of them are looked for and the ones that are there are taken.
    let names = [
        format!("{arch}-linux-{libc}"),
        format!("{arch}-pc-linux-{libc}"),
        format!("{arch}-redhat-linux"),
        format!("{arch}-suse-linux"),
        format!("{arch}-alpine-linux-{libc}"),
    ];
    let mut found = Vec::new();
    for base in ["/usr/lib/gcc", "/usr/lib64/gcc", "/usr/local/lib/gcc"] {
        for name in &names {
            found.extend(newest_first(&under(sysroot, &format!("{base}/{name}"))));
        }
    }
    found
}

/// The version directories under one of gcc's, highest version first.
fn newest_first(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut versions: Vec<(Vec<u64>, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .map(|p| (version_key(&p), p))
        .collect();
    // Descending, so the highest version is the first place `find_file` looks. Ties keep the order
    // the directory gave, which is arbitrary and does not matter because two directories that sort
    // the same hold the same version.
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, path)| path).collect()
}

/// A directory name read as a version, so that `13` sorts above `9` and `10.2` above `10`.
///
/// A name that is not a version at all sorts below every name that is, rather than being left
/// out, because a directory holding a `libgcc.a` is worth looking in whatever it is called.
fn version_key(dir: &Path) -> Vec<u64> {
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    name.split('.').map(|part| part.parse::<u64>().unwrap_or(0)).collect()
}

/// Our own runtime library for this target, if it was built.
///
/// Looked for beside the compiler rather than at a path decided when the compiler was built, for
/// the same reason everything else here is looked for: one binary runs wherever it is copied. A
/// `-B` prefix is asked first, because that is what a `-B` prefix is for.
#[must_use]
pub fn builtins_archive(target: Triple, prefixes: &[PathBuf]) -> Option<PathBuf> {
    // The name from the crate that puts it on a line, rather than a second spelling of it here,
    // which is what that constant asks of anybody who needs the name.
    const NAME: &str = rucc_sysroot::link::BUILTINS;
    // The four field triple first and then the tuple the sysroots are named by, because `cargo
    // xtask builtins --target=aarch64-linux-musl` names its directory after what it was given, and
    // that shorter spelling is the one people type.
    let spellings = [target.to_string(), target.tuple().to_string()];
    let mut places: Vec<PathBuf> = Vec::new();
    for prefix in prefixes {
        for triple in &spellings {
            places.push(prefix.join(triple).join(NAME));
        }
        places.push(prefix.join(NAME));
    }
    if let Some(dir) =
        std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        // A release archive: the compiler at the top of the unpacked directory and the runtime
        // beside it in `lib/rucc/<triple>`, which is how `.github/package.sh` lays one out.
        for triple in &spellings {
            places.push(dir.join("lib").join("rucc").join(triple).join(NAME));
        }
        if let Some(up) = dir.parent() {
            for triple in &spellings {
                // An install: the compiler in `bin` and its runtime in `lib/rucc/<triple>`.
                places.push(up.join("lib").join("rucc").join(triple).join(NAME));
                // A build tree: the compiler in `target/release` and the runtime, which is built
                // for the target and not the host, in `target/<triple>/release`.
                for profile in ["release", "debug"] {
                    places.push(up.join(triple).join(profile).join(NAME));
                }
            }
        }
        places.push(dir.join(NAME));
    }
    places.into_iter().find(|path| path.is_file())
}

/// Which output format this `ld` should write, in the name `ld` knows it by.
fn emulation(target: Triple) -> &'static str {
    match target.arch {
        Arch::X86_64 => "elf_x86_64",
        Arch::Aarch64 => "aarch64linux",
        Arch::Riscv64 => "elf64lriscv",
        Arch::X86 => "elf_i386",
    }
}

/// The program that starts a dynamically linked program, whose path is part of the file.
///
/// It is a per-target constant rather than something to look for, because the name is fixed by
/// the platform's ABI and a program naming a different one does not start.
fn loader(target: Triple) -> &'static str {
    match (target.arch, target.env) {
        (Arch::X86_64, Env::Musl) => "/lib/ld-musl-x86_64.so.1",
        (Arch::X86_64, _) => "/lib64/ld-linux-x86-64.so.2",
        (Arch::Aarch64, Env::Musl) => "/lib/ld-musl-aarch64.so.1",
        (Arch::Aarch64, _) => "/lib/ld-linux-aarch64.so.1",
        (Arch::Riscv64, Env::Musl) => "/lib/ld-musl-riscv64.so.1",
        (Arch::Riscv64, _) => "/lib/ld-linux-riscv64-lp64d.so.1",
        (Arch::X86, Env::Musl) => "/lib/ld-musl-i386.so.1",
        (Arch::X86, _) => "/lib/ld-linux.so.2",
    }
}

/// Where the library's own files might be, in search order.
///
/// The multiarch directory first for the reason it comes first in the header search: it is where
/// a distribution that can hold two architectures at once puts the one being asked for, and a
/// distribution that cannot simply does not have it. `lib64` after it, which is what the
/// distributions that split by word size use instead, and `lib` last, which is every other one.
#[must_use]
pub fn candidates(target: Triple, sysroot: Option<&Path>) -> Vec<PathBuf> {
    let multiarch = multiarch(target);
    [
        format!("/usr/lib/{multiarch}"),
        format!("/lib/{multiarch}"),
        "/usr/lib64".to_owned(),
        "/lib64".to_owned(),
        "/usr/lib".to_owned(),
        "/lib".to_owned(),
    ]
    .into_iter()
    .map(|dir| under(sysroot, &dir))
    .collect()
}

/// The name a distribution that holds two architectures at once files this target under.
///
/// `x86_64-linux-gnu` and its friends, which is what `gcc -print-multiarch` prints and what a
/// build system pastes into a path when it is looking for a library itself.
#[must_use]
pub fn multiarch(target: Triple) -> String {
    let libc = match target.env {
        Env::Musl => "musl",
        Env::None | Env::Gnu | Env::Msvc => "gnu",
    };
    format!("{}-linux-{libc}", target.arch.as_str())
}

/// The candidates that are there.
fn library_dirs(target: Triple, sysroot: Option<&Path>) -> Vec<PathBuf> {
    candidates(target, sysroot).into_iter().filter(|dir| dir.is_dir()).collect()
}

/// Where a library is looked for, in the order it is looked for in.
///
/// The command line first and the target's own after it, which is the order the linker is handed
/// and therefore the order `-print-search-dirs` has to print.
#[must_use]
pub fn search_dirs(link: &LinkOptions, target: Triple) -> Vec<PathBuf> {
    let mut dirs = link.search.clone();
    // A cross link searches the sysroot's directory and, for a libc that is a stub, the one its
    // stubs are written to, so this is those and not the machine's. What `-print-search-dirs` says is what a build
    // system pastes into a link line of its own, and an answer that named `/usr/lib` for a target
    // whose link line never goes near it would be worse than no answer at all.
    if let Some(sysroot) = cross_sysroot(target, link) {
        dirs.push(sysroot.lib());
        if rucc_sysroot::link::libc(sysroot.target()) == rucc_sysroot::link::Libc::Stub {
            dirs.push(sysroot.stubs().to_path_buf());
        }
        return dirs;
    }
    if let Some(distro) = distro_cross(target, link) {
        dirs.push(distro.lib());
        return dirs;
    }
    dirs.extend(candidates(target, link.sysroot.as_deref()));
    dirs
}

/// The full path of a file with that name, when one of the search directories holds it.
///
/// What `-print-file-name=` answers. GCC prints the name back unchanged when it finds nothing,
/// which is what makes the flag safe to paste into a link line either way.
#[must_use]
pub fn find_in_search(link: &LinkOptions, target: Triple, name: &str) -> Option<PathBuf> {
    find_file(&search_dirs(link, target), name)
}

/// The first of those directories holding a file of that name.
fn find_file(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().map(|dir| dir.join(name)).find(|path| path.is_file())
}

/// A path under the sysroot, when there is one.
fn under(sysroot: Option<&Path>, path: &str) -> PathBuf {
    match sysroot {
        // `strip_prefix` because joining an absolute path replaces the root rather than extending
        // it, which would make every entry the unprefixed one.
        Some(root) => root.join(path.strip_prefix('/').unwrap_or(path)),
        None => PathBuf::from(path),
    }
}

/// A path on the machine that will run the program, rather than on the one compiling it.
///
/// Written with the separator of the target and not of the host, which matters for the one path
/// that is not looked at here but stored in the file and read by something else later: the loader
/// a dynamic program names. A Windows host joining it would put a backslash in the middle of a
/// name that a Linux loader has to find, and the program would not start.
fn target_path(sysroot: Option<&Path>, path: &str) -> String {
    match sysroot {
        Some(root) => {
            let root = root.display().to_string();
            format!("{}/{}", root.trim_end_matches(['/', '\\']), path.trim_start_matches('/'))
        }
        None => path.to_owned(),
    }
}

/// The whole invocation as one line, quoted the way `-###` prints it.
#[must_use]
pub fn render(linker: &Linker, args: &[String]) -> String {
    let mut out = linker.path.display().to_string();
    for arg in args {
        out.push(' ');
        if arg.is_empty() || arg.contains(char::is_whitespace) {
            out.push('"');
            out.push_str(arg);
            out.push('"');
        } else {
            out.push_str(arg);
        }
    }
    out
}

/// How long a command line may be on Windows before the arguments go in a file instead.
///
/// `CreateProcess` takes 32767 characters, the program's own path and the quoting included, and
/// what is left for the arguments is not worth computing to the character.
const WINDOWS_LINE: usize = 30_000;

/// The same for every other host, where the limit is a megabyte or more shared with the
/// environment, and a link this long is Kbuild's `-r` of a whole subsystem.
const UNIX_LINE: usize = 256 * 1024;

/// Whether these arguments, joined with a space and a pair of quotes each, are over `limit`.
fn too_long(args: &[String], limit: usize) -> bool {
    args.iter().map(|arg| arg.len() + 3).sum::<usize>() > limit
}

/// The arguments that stay on the command line and the ones that go in the file.
///
/// `-m` and its value stay, because they are what makes `ld.lld` pick its MinGW driver over its
/// ELF one, and it is better not to depend on that choice looking inside the file.
fn split_for_file(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut front = Vec::new();
    let mut rest = Vec::with_capacity(args.len());
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        if arg == "-m" {
            front.push(arg.clone());
            front.extend(words.next().cloned());
        } else {
            rest.push(arg.clone());
        }
    }
    (front, rest)
}

/// Whether this linker reads a response file with Windows quoting, where a backslash is an
/// ordinary character unless it comes before a quote.
///
/// lld does on a Windows host, both its ELF driver and its MinGW one, and the MinGW one has no
/// option to say otherwise. `lld-link` is the same program and reads the same way, and Microsoft's
/// `link.exe` only runs on Windows and has never read any other. GNU ld reads the GNU way on every
/// host, MSYS2's included.
fn windows_quoting(linker: &Linker) -> bool {
    let file = linker.path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
    let microsoft = linker.name == "link.exe" || file.eq_ignore_ascii_case("link");
    cfg!(windows)
        && (is_lld(&linker.name)
            || file.starts_with("ld.lld")
            || file.starts_with("lld")
            || microsoft)
}

/// The words of a response file, one to a line, quoted so that the linker reads them back as
/// they were.
///
/// The GNU way is a backslash before every quote and every backslash. The Windows way leaves a
/// backslash alone unless a run of them ends at a quote, and then doubles the run and escapes the
/// quote, which is the rule `CommandLineToArgvW` reads with and so the rule LLVM reads with too.
fn response_text(args: &[String], windows: bool) -> String {
    let mut text = String::new();
    for arg in args {
        text.push('"');
        let mut slashes = 0;
        for c in arg.chars() {
            match c {
                '\\' if windows => slashes += 1,
                '"' if windows => {
                    text.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                    text.push('"');
                    slashes = 0;
                }
                _ if windows => {
                    text.extend(std::iter::repeat_n('\\', slashes));
                    text.push(c);
                    slashes = 0;
                }
                '"' | '\\' => {
                    text.push('\\');
                    text.push(c);
                }
                _ => text.push(c),
            }
        }
        text.extend(std::iter::repeat_n('\\', slashes * 2));
        text.push_str("\"\n");
    }
    text
}

/// Runs the linker and waits for it.
///
/// A line too long for the host goes to the linker as a response file, which is what a Windows
/// build of a large program needs: a few hundred objects in a deep directory are past what
/// `CreateProcess` takes.
///
/// # Errors
///
/// [`Error::Spawn`] when it could not be started, which is a machine problem, and
/// [`Error::Refused`] when it ran and said no, which is a program problem and one the linker has
/// already explained on its own error output.
pub fn run(linker: &Linker, args: &[String]) -> Result<(), Error> {
    let spawn = |why: std::io::Error| Error::Spawn {
        path: linker.path.display().to_string(),
        why: why.to_string(),
    };
    let mut command = Command::new(&linker.path);
    let mut written = None;
    if too_long(args, if cfg!(windows) { WINDOWS_LINE } else { UNIX_LINE }) {
        let (front, rest) = split_for_file(args);
        let path = std::env::temp_dir().join(format!("rucc-link-{}.rsp", std::process::id()));
        fs::write(&path, response_text(&rest, windows_quoting(linker))).map_err(spawn)?;
        let mut at = OsString::from("@");
        at.push(&path);
        command.args(front).arg(at);
        written = Some(path);
    } else {
        command.args(args.iter().map(OsString::from));
    }
    let status = command.status();
    if let Some(path) = written {
        let _ = fs::remove_file(path);
    }
    let status = status.map_err(spawn)?;
    if status.success() {
        return Ok(());
    }
    // Nothing is added to what the linker printed. It has already named the symbol or the file,
    // and a second message from here saying that linking failed would only push the first one
    // further up the screen.
    Err(Error::Refused {
        status: match status.code() {
            Some(code) => format!("exited with status {code}"),
            None => "was killed before it finished".to_owned(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux() -> Triple {
        Triple::new(Arch::X86_64, Os::Linux, Env::Gnu)
    }

    fn one(name: &str) -> Vec<Item> {
        vec![Item::File(name.to_owned())]
    }

    #[test]
    fn the_fast_one_is_looked_for_first_and_the_platforms_own_last() {
        let names = order(linux(), &LinkOptions::default());
        assert_eq!(names.first().map(String::as_str), Some("ld.mold"));
        assert_eq!(names.last().map(String::as_str), Some("ld"));
    }

    #[test]
    fn naming_one_is_the_whole_of_the_order() {
        let opts = LinkOptions { use_ld: Some("gold".to_owned()), ..LinkOptions::default() };
        assert_eq!(order(linux(), &opts), ["ld.gold", "gold"]);
    }

    #[test]
    fn a_dynamic_program_names_the_loader_that_will_start_it() {
        let args = line(linux(), &LinkOptions::default(), &one("a.o"), "a.out").expect("a line");
        let at = args.iter().position(|a| a == "-dynamic-linker").expect("the flag");
        assert!(args[at + 1].ends_with("/lib64/ld-linux-x86-64.so.2"), "{args:?}");
    }

    /// Kbuild's `built-in.o`, which is objects joined into an object and is linked again later.
    #[test]
    fn a_relocatable_link_is_the_objects_and_nothing_a_program_needs() {
        let opts = LinkOptions { relocatable: true, ..LinkOptions::default() };
        let args = line(linux(), &opts, &one("a.o"), "built-in.o").expect("a line");
        assert_eq!(args, ["-o", "built-in.o", "-m", "elf_x86_64", "-r", "a.o"]);
    }

    /// `-gz` reaches the linker as gcc passes it, so the linked file keeps its debug sections
    /// compressed, and a Mac link is not handed an option ld64 does not have.
    #[test]
    fn compressed_debug_sections_are_asked_of_an_elf_linker_only() {
        let zlib =
            LinkOptions { relocatable: true, compress: Compress::Zlib, ..LinkOptions::default() };
        let args = line(linux(), &zlib, &one("a.o"), "built-in.o").expect("a line");
        assert_eq!(args.last().map(String::as_str), Some("--compress-debug-sections=zlib"));
        let gnu = LinkOptions { compress: Compress::ZlibGnu, ..zlib.clone() };
        let args = line(linux(), &gnu, &one("a.o"), "built-in.o").expect("a line");
        assert_eq!(args.last().map(String::as_str), Some("--compress-debug-sections=zlib-gnu"));
        let zstd = LinkOptions { compress: Compress::Zstd, ..zlib.clone() };
        let args = line(linux(), &zstd, &one("a.o"), "built-in.o").expect("a line");
        assert_eq!(args.last().map(String::as_str), Some("--compress-debug-sections=zstd"));
        let args = line(mac(), &zlib, &one("a.o"), "b.o").expect("ld64 takes -r");
        assert!(args.iter().all(|arg| !arg.contains("compress")), "{args:?}");
    }

    #[test]
    fn a_static_program_names_no_loader_because_nothing_will_start_it() {
        let opts = LinkOptions { is_static: true, ..LinkOptions::default() };
        let args = line(linux(), &opts, &one("a.o"), "a.out").expect("a line");
        assert!(args.contains(&"-static".to_owned()), "{args:?}");
        assert!(!args.contains(&"-dynamic-linker".to_owned()), "{args:?}");
    }

    #[test]
    fn the_startup_file_of_a_program_that_moves_is_not_the_one_of_a_program_that_does_not() {
        let moving = LinkOptions { pie: Some(true), ..LinkOptions::default() };
        let fixed = LinkOptions { pie: Some(false), ..LinkOptions::default() };
        let named = |opts: &LinkOptions| {
            line(linux(), opts, &one("a.o"), "a.out")
                .expect("a line")
                .iter()
                .filter_map(|a| Path::new(a).file_name().map(|n| n.to_string_lossy().into_owned()))
                .find(|n| n.ends_with("crt1.o"))
        };
        // Only when the machine running this has them, which is what makes this two assertions
        // rather than one: a machine with no glibc development files has neither to find.
        if let Some(name) = named(&moving) {
            assert_eq!(name, "Scrt1.o");
            assert_eq!(named(&fixed).as_deref(), Some("crt1.o"));
        }
    }

    /// A profiled program is started by a startup file of its own.
    ///
    /// The counts it keeps have to be started before `main` runs and written out after it returns,
    /// and what does both is this file rather than anything the compiler wrote. So a build that
    /// compiles with the flag and links without it produces a program that calls the hook on every
    /// function and never writes a profile, which is the failure this is here to keep out.
    ///
    /// A shared object takes none of them either way, since nothing starts one.
    #[test]
    fn a_profiled_program_is_started_by_the_startup_file_that_counts() {
        let profile = LinkOptions { profile: true, ..LinkOptions::default() };
        assert_eq!(startfile(&profile, false), Some("gcrt1.o"));
        assert_eq!(startfile(&profile, true), Some("gcrt1.o"));
        let still = LinkOptions { is_static: true, ..profile.clone() };
        assert_eq!(startfile(&still, true), Some("grcrt1.o"));
        assert_eq!(startfile(&still, false), Some("gcrt1.o"));
        let shared = LinkOptions { shared: true, ..profile };
        assert_eq!(startfile(&shared, false), None);
    }

    /// And a program that is not profiled is started by the one it always was.
    #[test]
    fn a_program_that_is_not_profiled_is_started_by_the_usual_one() {
        let plain = LinkOptions::default();
        assert_eq!(startfile(&plain, false), Some("crt1.o"));
        assert_eq!(startfile(&plain, true), Some("Scrt1.o"));
    }

    #[test]
    fn asking_for_no_startup_files_leaves_out_both_ends_of_them() {
        let opts = LinkOptions { no_startfiles: true, ..LinkOptions::default() };
        let args = line(linux(), &opts, &one("a.o"), "a.out").expect("a line");
        assert!(!args.iter().any(|a| a.ends_with("crt1.o")), "{args:?}");
        assert!(!args.iter().any(|a| a.ends_with("crtn.o")), "{args:?}");
        // And still links against the library, because that is the other flag.
        assert!(args.contains(&"-lc".to_owned()), "{args:?}");
    }

    #[test]
    fn asking_for_no_library_at_all_leaves_out_the_startup_files_too() {
        let opts = LinkOptions { no_stdlib: true, ..LinkOptions::default() };
        let args = line(linux(), &opts, &one("a.o"), "a.out").expect("a line");
        assert!(!args.contains(&"-lc".to_owned()), "{args:?}");
        assert!(!args.iter().any(|a| a.ends_with("crt1.o")), "{args:?}");
    }

    #[test]
    fn the_library_comes_after_the_objects_that_need_it() {
        let items = vec![Item::File("a.o".to_owned()), Item::Library("m".to_owned())];
        let args = line(linux(), &LinkOptions::default(), &items, "a.out").expect("a line");
        let obj = args.iter().position(|a| a == "a.o").expect("the object");
        let m = args.iter().position(|a| a == "-lm").expect("the library");
        let c = args.iter().position(|a| a == "-lc").expect("the library");
        assert!(obj < m && m < c, "{args:?}");
    }

    #[test]
    fn what_the_user_told_the_linker_stays_where_the_user_wrote_it() {
        // The pair libtool writes around a set of convenience archives, which is what found this.
        // Both words are about the files between them, so a line that collects them and puts them
        // at the end has two options that do nothing and an archive whose members were all dropped.
        let items = vec![
            Item::File("a.o".to_owned()),
            Item::Linker("--whole-archive".to_owned()),
            Item::File("libaesni.a".to_owned()),
            Item::Linker("--no-whole-archive".to_owned()),
            Item::Library("m".to_owned()),
        ];
        let args = line(linux(), &LinkOptions::default(), &items, "a.out").expect("a line");
        let at = |what: &str| args.iter().position(|a| a == what).expect(what);
        assert!(at("a.o") < at("--whole-archive"), "{args:?}");
        assert!(at("--whole-archive") < at("libaesni.a"), "{args:?}");
        assert!(at("libaesni.a") < at("--no-whole-archive"), "{args:?}");
        assert!(at("--no-whole-archive") < at("-lm"), "{args:?}");
        assert!(at("-lm") < at("-lc"), "{args:?}");
    }

    #[test]
    fn a_sysroot_moves_every_path_this_decided_and_none_the_user_wrote() {
        let opts = LinkOptions {
            sysroot: Some(PathBuf::from("/nowhere-at-all")),
            search: vec![PathBuf::from("/opt/mine")],
            ..LinkOptions::default()
        };
        let args = line(linux(), &opts, &one("a.o"), "a.out").expect("a line");
        let at = args.iter().position(|a| a == "-dynamic-linker").expect("the flag");
        assert_eq!(args[at + 1], "/nowhere-at-all/lib64/ld-linux-x86-64.so.2");
        assert!(args.contains(&"-L/opt/mine".to_owned()), "{args:?}");
    }

    #[test]
    fn a_platform_with_no_link_line_is_said_so_rather_than_linked_wrongly() {
        let triple = Triple::new(Arch::X86_64, Os::None, Env::None);
        let error = line(triple, &LinkOptions::default(), &one("a.o"), "a.out")
            .expect_err("no line for it");
        assert!(matches!(error, Error::Target { .. }), "{error:?}");
    }

    /// A made up SDK with the one file the line reads out of it, so that nothing about this machine
    /// decides what the line says.
    fn an_sdk(name: &str, settings: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("rucc-sdk-{}-{}", std::process::id(), name.replace('.', "-")))
            .join(name);
        fs::create_dir_all(&dir).expect("a temporary directory");
        if let Some(settings) = settings {
            fs::write(dir.join("SDKSettings.json"), settings).expect("a settings file");
        }
        dir
    }

    fn mac() -> Triple {
        Triple::new(Arch::Aarch64, Os::Darwin, Env::None)
    }

    #[test]
    fn a_mac_link_names_the_platform_the_sdk_and_libsystem() {
        let sdk = an_sdk(
            "MacOSX.sdk",
            Some(
                "{\"CanonicalName\": \"macosx15.2\", \"Version\" : \"15.2\", \"MaximumDeploymentTarget\": \"15.2.99\"}",
            ),
        );
        let opts = LinkOptions {
            sysroot: Some(sdk.clone()),
            search: vec![PathBuf::from("/opt/mine")],
            no_builtins_lib: true,
            ..LinkOptions::default()
        };
        let items = vec![Item::File("a.o".to_owned()), Item::Library("m".to_owned())];
        let args = line(mac(), &opts, &items, "a.out").expect("a line");
        let sdk = sdk.display().to_string();
        let want: Vec<&str> = vec![
            "-arch",
            "arm64",
            "-platform_version",
            "macos",
            "11.0",
            "15.2",
            "-syslibroot",
            &sdk,
            "-o",
            "a.out",
            "-L/opt/mine",
            "a.o",
            "-lm",
            "-lSystem",
        ];
        assert_eq!(args, want);

        // The deployment target from the command line, and a shared library.
        let opts =
            LinkOptions { os_version: Some(rucc_tuple::Version::new(13, 4)), shared: true, ..opts };
        let args = line(mac(), &opts, &one("a.o"), "liba.dylib").expect("a line");
        assert_eq!(args[3..6], ["macos", "13.4", "15.2"]);
        assert!(args.contains(&"-dylib".to_owned()), "{args:?}");
        assert!(!args.iter().any(|arg| arg.contains("pie")), "{args:?}");
    }

    #[test]
    fn the_sdk_version_comes_from_its_name_when_it_has_no_settings() {
        let sdk = an_sdk("MacOSX14.5.sdk", None);
        let opts =
            LinkOptions { sysroot: Some(sdk), no_builtins_lib: true, ..LinkOptions::default() };
        let args = line(mac(), &opts, &one("a.o"), "a.out").expect("a line");
        assert_eq!(args[3..6], ["macos", "11.0", "14.5"]);
        // And the deployment target when neither says anything.
        let sdk = an_sdk("Somewhere", None);
        let opts = LinkOptions {
            sysroot: Some(sdk),
            os_version: Some(rucc_tuple::Version::major(12)),
            ..opts
        };
        let args = line(mac(), &opts, &one("a.o"), "a.out").expect("a line");
        assert_eq!(args[3..6], ["macos", "12", "12"]);
    }

    #[test]
    fn a_mac_link_looks_for_a_mach_o_linker_and_nothing_else() {
        assert_eq!(order(mac(), &LinkOptions::default()), ["ld", "ld64.lld"]);
    }

    #[test]
    fn a_static_mac_program_is_refused_and_a_relocatable_one_is_not() {
        let sdk = an_sdk("MacOSX.sdk", None);
        let opts = LinkOptions { sysroot: Some(sdk), is_static: true, ..LinkOptions::default() };
        let error = line(mac(), &opts, &one("a.o"), "a.out").expect_err("no static line");
        let Error::Cross { why } = &error else { panic!("{error:?}") };
        assert!(why.contains("-static"), "{why}");
        let args = relocatable_line(mac(), &LinkOptions::default(), &one("a.o"), "b.o")
            .expect("ld64 takes -r");
        assert_eq!(args, ["-r", "-arch", "arm64", "-o", "b.o", "a.o"]);
    }

    #[test]
    fn the_line_is_printed_the_way_it_would_be_typed() {
        let linker = Linker { name: "ld".to_owned(), path: PathBuf::from("/usr/bin/ld") };
        let args = ["-o".to_owned(), "a b".to_owned()];
        assert_eq!(render(&linker, &args), "/usr/bin/ld -o \"a b\"");
    }

    #[test]
    fn a_linker_that_is_not_there_is_said_by_name() {
        let opts = LinkOptions {
            use_ld: Some("a-linker-nobody-has".to_owned()),
            ..LinkOptions::default()
        };
        let error = find(linux(), &opts).expect_err("not on this machine");
        assert_eq!(error, Error::Named { name: "a-linker-nobody-has".to_owned() });
    }
    /// A directory with a `libgcc.a` in it, so a test can say what a machine with a gcc on it
    /// looks like without needing one.
    fn a_gcc_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rucc-link-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("a temporary directory");
        fs::write(dir.join("libgcc.a"), b"not really an archive").expect("a file in it");
        dir
    }

    #[test]
    fn the_c_library_supplies_the_block_routines_and_our_runtime_does_not_displace_them() {
        let gcc = a_gcc_dir("order");
        let ours = PathBuf::from("/somewhere/librucc_builtins.a");
        let args = runtime_items(&LinkOptions::default(), &[gcc], Some(&ours));
        let at_libc = args.iter().position(|a| a == "-lc").expect("libc");
        let at_ours = args.iter().position(|a| a.ends_with("librucc_builtins.a")).expect("ours");
        // glibc's `memcpy` is assembly per microarchitecture and ours is a word at a time loop,
        // so on a target that has one, its is the one that should answer.
        assert!(at_libc < at_ours, "{args:?}");
    }

    #[test]
    fn a_static_link_puts_them_in_a_group_because_two_of_them_refer_to_each_other() {
        let gcc = a_gcc_dir("group");
        let opts = LinkOptions { is_static: true, ..LinkOptions::default() };
        let args = runtime_items(&opts, &[gcc], None);
        assert_eq!(args.first().map(String::as_str), Some("--start-group"), "{args:?}");
        assert_eq!(args.last().map(String::as_str), Some("--end-group"), "{args:?}");
        // The unwinder, which is what `libc.a` refers to and what a static link fails on without
        // it. Issue #277.
        assert!(args.contains(&"-lgcc_eh".to_owned()), "{args:?}");
    }

    #[test]
    fn a_dynamic_link_needs_no_group_and_asks_for_the_shared_half_only_if_something_wants_it() {
        let gcc = a_gcc_dir("dynamic");
        let args = runtime_items(&LinkOptions::default(), &[gcc], None);
        assert!(!args.contains(&"--start-group".to_owned()), "{args:?}");
        assert!(!args.contains(&"-lgcc_eh".to_owned()), "{args:?}");
        let at = args.iter().position(|a| a == "-lgcc_s").expect("the shared half");
        assert_eq!(args[at - 1], "--as-needed", "{args:?}");
        assert_eq!(args[at + 1], "--no-as-needed", "{args:?}");
    }

    #[test]
    fn our_own_runtime_comes_before_the_machines_because_the_two_are_interchangeable() {
        let gcc = a_gcc_dir("ours");
        let ours = PathBuf::from("/somewhere/librucc_builtins.a");
        let args = runtime_items(&LinkOptions::default(), &[gcc], Some(&ours));
        let at_ours = args.iter().position(|a| a.ends_with("librucc_builtins.a")).expect("ours");
        let at_gcc = args.iter().position(|a| a == "-lgcc").expect("libgcc");
        assert!(at_ours < at_gcc, "{args:?}");
    }

    #[test]
    fn no_builtins_lib_leaves_ours_off_and_keeps_the_machines() {
        let gcc = a_gcc_dir("theirs");
        let opts = LinkOptions { no_builtins_lib: true, ..LinkOptions::default() };
        let args = line(linux(), &opts, &one("a.o"), "a.out").expect("a line");
        assert!(!args.iter().any(|a| a.ends_with("librucc_builtins.a")), "{args:?}");
        // And the machine's half is still decided the same way it was, from the directories
        // that are there, which on the machine running this test may be none.
        assert!(runtime_items(&opts, &[gcc], None).contains(&"-lgcc".to_owned()));
    }

    #[test]
    fn nodefaultlibs_leaves_the_whole_runtime_off_and_not_only_the_c_library() {
        let gcc = a_gcc_dir("none");
        let opts = LinkOptions { no_defaultlibs: true, ..LinkOptions::default() };
        assert!(runtime_items(&opts, &[gcc], None).is_empty());
    }

    #[test]
    fn a_machine_with_no_gcc_on_it_gets_no_names_for_libraries_that_are_not_there() {
        let empty = std::env::temp_dir().join("rucc-link-empty-not-a-gcc");
        let args = runtime_items(&LinkOptions::default(), &[empty], None);
        assert_eq!(args, ["-lc"], "{args:?}");
    }

    #[test]
    fn a_gcc_version_directory_is_read_as_a_version_and_not_as_a_word() {
        assert!(version_key(Path::new("/usr/lib/gcc/x/13")) > version_key(Path::new("/x/9")));
        assert!(version_key(Path::new("/x/10.2")) > version_key(Path::new("/x/10")));
        // Something that is not a version at all still sorts, and sorts below one that is.
        assert!(version_key(Path::new("/x/snapshot")) < version_key(Path::new("/x/1")));
    }

    /// A command line that has a cache to find generated sysroots in, which a real one always has.
    ///
    /// And a `-B` prefix with our runtime in it, because a cross link refuses without one and
    /// every machine that does this for real has the archive `cargo xtask builtins` wrote. What
    /// happens when it is missing is its own test below.
    /// The fast math startup file follows gcc's end file spec: the family puts it in anything
    /// that is not a shared object, and `-mdaz-ftz` decides it outright either way.
    #[test]
    fn the_fast_math_startup_file_is_wanted_where_gccs_spec_wants_it() {
        let fast = LinkOptions { fast_math: true, ..LinkOptions::default() };
        assert!(!LinkOptions::default().wants_fastmath());
        assert!(fast.wants_fastmath());
        assert!(!LinkOptions { shared: true, ..fast.clone() }.wants_fastmath());
        assert!(!LinkOptions { daz_ftz: Some(false), ..fast.clone() }.wants_fastmath());
        let forced = LinkOptions { shared: true, daz_ftz: Some(true), ..LinkOptions::default() };
        assert!(forced.wants_fastmath());
    }

    fn cached() -> LinkOptions {
        LinkOptions {
            cache: Some(PathBuf::from("/cache")),
            prefixes: vec![a_builtins_dir()],
            ..LinkOptions::default()
        }
    }

    /// A directory with our runtime archive in it, so that a test can say what a machine where the
    /// runtime was built looks like without building one.
    ///
    /// One directory for every test rather than one each, since none of them writes to it and the
    /// name of the file is the whole of what they read.
    fn a_builtins_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rucc-link-ours-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("a temporary directory");
        fs::write(dir.join("librucc_builtins.a"), b"not really an archive").expect("a file in it");
        dir
    }

    /// An archive in a directory named by the short tuple is found, which is where `cargo xtask
    /// builtins --target=aarch64-linux-musl` puts it.
    #[test]
    fn the_runtime_is_found_under_the_tuple_as_well_as_the_triple() {
        let dir = std::env::temp_dir().join(format!("rucc-link-tuple-{}", std::process::id()));
        let target: Triple = "aarch64-linux-musl".parse().expect("a triple");
        let under = dir.join(target.tuple().to_string());
        fs::create_dir_all(&under).expect("a temporary directory");
        fs::write(under.join("librucc_builtins.a"), b"not really an archive")
            .expect("a file in it");
        let found = builtins_archive(target, std::slice::from_ref(&dir));
        assert_eq!(found, Some(under.join("librucc_builtins.a")));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Where that cache would keep this target's sysroot.
    fn a_sysroot(target: Triple) -> Sysroot {
        Sysroot::in_cache(Path::new("/cache"), target.tuple())
    }

    /// A target that is not the machine running this test, whatever machine that is.
    ///
    /// A freestanding one, because [`Triple::host`] answers Linux, Darwin or Windows and never
    /// `Os::None`. Every other triple is somebody's host, so a test that wants the cross path out of
    /// [`line`] itself has to use this one and the rest go through [`cross_line`].
    fn foreign() -> Triple {
        Triple::new(Arch::X86_64, Os::None, Env::None)
    }

    #[test]
    fn a_cross_link_reads_the_targets_own_sysroot_and_nothing_of_this_machine() {
        let target = Triple::new(Arch::Aarch64, Os::Linux, Env::Musl);
        let sysroot = a_sysroot(target);
        // The paths as this host spells them, because what is being checked is which directory the
        // files are in and a Windows separator is a backslash.
        let root = sysroot.root().display().to_string();
        let lib = sysroot.lib();
        let args = cross_line(target, &cached(), &one("a.o"), "a.out", &sysroot).expect("a line");
        assert!(args.contains(&format!("--sysroot={root}")), "{args:?}");
        assert!(args.contains(&format!("-L{}", lib.display())), "{args:?}");
        assert!(args.contains(&lib.join("libc.a").display().to_string()), "{args:?}");
        let at = args.iter().position(|a| a == "-dynamic-linker").expect("the loader");
        assert_eq!(args[at + 1], "/lib/ld-musl-aarch64.so.1", "{args:?}");
        // The whole point of the other path not being taken: not one directory of this machine is
        // on the line, so the line is the same on every host and the recorded ones describe it.
        for arg in &args {
            assert!(!arg.contains("/usr/lib"), "{arg} in {args:?}");
            assert!(!arg.contains("/lib64"), "{arg} in {args:?}");
        }
    }

    #[test]
    fn a_freestanding_target_links_against_our_runtime_instead_of_being_refused() {
        let args = line(foreign(), &cached(), &one("a.o"), "a.out").expect("a line");
        assert!(args.iter().any(|a| a.ends_with("librucc_builtins.a")), "{args:?}");
        // No libc, because there is not one, and no start file either: what runs before `main` on a
        // freestanding target comes from whatever is being built.
        assert!(!args.iter().any(|a| a.ends_with("libc.a")), "{args:?}");
        assert!(!args.contains(&"-lc".to_owned()), "{args:?}");
        assert!(!args.iter().any(|a| a.ends_with("crt1.o")), "{args:?}");
    }

    /// And with nothing to find sysroots in it is refused, which is what it was before this.
    #[test]
    fn a_driver_with_no_cache_to_look_in_says_so_rather_than_guessing() {
        let error = line(foreign(), &LinkOptions::default(), &one("a.o"), "a.out")
            .expect_err("no line for it");
        assert!(matches!(error, Error::Target { .. }), "{error:?}");
    }

    #[test]
    fn a_static_link_against_a_libc_that_is_a_stub_is_refused_rather_than_attempted() {
        let target = Triple::new(Arch::X86_64, Os::Linux, Env::Gnu);
        let opts = LinkOptions { is_static: true, ..cached() };
        let error = cross_line(target, &opts, &one("a.o"), "a.out", &a_sysroot(target))
            .expect_err("there is no libc.a in a stub sysroot");
        let Error::Cross { why } = &error else { panic!("{error:?}") };
        // Because a stub carries the names a library exports and none of the bodies, which is
        // everything a dynamic link reads and nothing a static one does.
        assert!(why.contains("stub"), "{why}");
    }

    #[test]
    fn a_target_whose_linker_wants_a_different_line_is_refused_by_name() {
        let target = Triple::new(Arch::Aarch64, Os::Darwin, Env::None);
        let error = cross_line(target, &cached(), &one("a.o"), "a.out", &a_sysroot(target))
            .expect_err("no line for that format");
        let Error::Cross { why } = &error else { panic!("{error:?}") };
        assert!(why.contains(&target.tuple().to_canonical_string()), "{why}");
    }

    #[test]
    fn an_msvc_link_is_against_the_tree_sysroot_names_and_says_what_to_run_without_one() {
        let target = Triple::new(Arch::X86_64, Os::Windows, Env::Msvc);
        // Not the cache, even with one to look in, because nothing of ours is ever there for it.
        let error = line(target, &cached(), &one("a.obj"), "a.exe").expect_err("no tree named");
        let Error::Cross { why } = &error else { panic!("{error:?}") };
        assert!(why.contains("rucc --fetch x86_64-windows-msvc"), "{why}");

        let tree = PathBuf::from("/trees/msvc");
        let named = LinkOptions { sysroot: Some(tree.clone()), ..cached() };
        let args = line(target, &named, &one("a.obj"), "a.exe").expect("a line");
        let crt = format!("-libpath:{}", tree.join("crt/lib").join("x86_64").display());
        assert!(args.contains(&crt), "{args:?}");
        assert!(args.contains(&"-out:a.exe".to_owned()), "{args:?}");
        assert!(args.contains(&"libcmt.lib".to_owned()), "{args:?}");
        assert!(args.iter().any(|arg| arg.ends_with("librucc_builtins.a")), "{args:?}");
        let dll = LinkOptions { crt: Crt::Dll, ..named };
        let args = line(target, &dll, &one("a.obj"), "a.exe").expect("a line");
        assert!(args.contains(&"msvcrt.lib".to_owned()), "{args:?}");
        assert!(!args.contains(&"libcmt.lib".to_owned()), "{args:?}");

        // And the linker that reads that line, whether or not there is a cache.
        assert_eq!(order(target, &cached()).first().map(String::as_str), Some("lld-link"));
        assert_eq!(order(target, &LinkOptions::default())[0], "lld-link");
    }

    #[test]
    fn an_msvc_tree_without_the_crt_for_this_architecture_is_said_before_anything_is_compiled() {
        let target = Triple::new(Arch::X86_64, Os::Windows, Env::Msvc);
        let tree = std::env::temp_dir().join(format!("rucc-msvc-tree-{}", std::process::id()));
        fs::create_dir_all(tree.join("crt/lib/aarch64")).expect("a temporary directory");
        let named = LinkOptions { sysroot: Some(tree.clone()), ..cached() };
        let error = preflight(target, &named).expect_err("a tree for the other architecture");
        let Error::Cross { why } = &error else { panic!("{error:?}") };
        assert!(why.contains("rucc --fetch x86_64-windows-msvc"), "{why}");
        fs::create_dir_all(tree.join("crt/lib/x86_64")).expect("the right one");
        preflight(target, &named).expect("a tree for this one");
        let _ = fs::remove_dir_all(&tree);
    }

    #[test]
    fn a_mingw_target_links_and_looks_for_a_linker_that_can_write_a_pe_image() {
        let target = Triple::new(Arch::X86_64, Os::Windows, Env::Gnu);
        let args = cross_line(target, &cached(), &one("a.o"), "a.exe", &a_sysroot(target))
            .expect("a line for mingw-w64");
        let at = |flag: &str| args.iter().position(|arg| arg == flag).expect(flag);
        assert_eq!(args[at("-m") + 1], "i386pep");
        assert_eq!(args[at("--subsystem") + 1], "console");
        assert!(args.iter().any(|arg| arg.ends_with("libmsvcrt.a")), "{args:?}");
        // And the prefixed name a distribution files its mingw binutils under, which is not the
        // multiarch one.
        let names = cross_order(target);
        assert_eq!(names.first().map(String::as_str), Some("ld.lld"));
        assert!(names.contains(&"x86_64-w64-mingw32-ld".to_owned()), "{names:?}");
    }

    #[test]
    fn our_runtime_comes_from_beside_the_compiler_rather_than_from_inside_the_sysroot() {
        // The two halves of tamnd/rucc#1514. The line used to name it under the sysroot's `lib`,
        // where nothing ever put it: it is this compiler's output for the target and a sysroot
        // fetched from a release holds the platform's files and not ours. So the path on the line
        // is the one the driver found, and the only `librucc_builtins.a` on the line is that one.
        let target = Triple::new(Arch::X86_64, Os::Windows, Env::Gnu);
        let sysroot = a_sysroot(target);
        let opts = cached();
        let args = cross_line(target, &opts, &one("a.o"), "a.exe", &sysroot).expect("a line");
        let ours: Vec<&String> =
            args.iter().filter(|arg| arg.ends_with("librucc_builtins.a")).collect();
        assert_eq!(ours.len(), 1, "{args:?}");
        assert_eq!(ours[0], &opts.prefixes[0].join("librucc_builtins.a").display().to_string());
        assert!(!ours[0].starts_with(&sysroot.lib().display().to_string()), "{args:?}");
        // And it is still last, after everything that calls into it.
        assert_eq!(args.last(), Some(ours[0]), "{args:?}");
    }

    #[test]
    fn a_cross_link_with_no_runtime_to_find_says_which_command_writes_one() {
        // What the linker would say instead is that `___chkstk_ms` is undefined, referenced from
        // mingw-w64's own objects, which is tamnd/rucc#1513 and is nobody's first guess at a
        // missing archive.
        let target = Triple::new(Arch::X86_64, Os::Windows, Env::Gnu);
        let opts = LinkOptions { prefixes: Vec::new(), ..cached() };
        let error = cross_line(target, &opts, &one("a.o"), "a.exe", &a_sysroot(target))
            .expect_err("there is no runtime for it to find");
        let Error::Cross { why } = &error else { panic!("{error:?}") };
        assert!(why.contains("cargo xtask builtins"), "{why}");
        assert!(why.contains("-fno-builtins-lib"), "{why}");

        // And that flag is the way through it, for somebody who meant to link without ours.
        let without = LinkOptions { no_builtins_lib: true, ..opts };
        let args = cross_line(target, &without, &one("a.o"), "a.exe", &a_sysroot(target))
            .expect("a line without ours on it");
        assert!(!args.iter().any(|arg| arg.ends_with("librucc_builtins.a")), "{args:?}");
    }

    #[test]
    fn the_version_an_lld_prints_is_read_and_nothing_elses_is() {
        // What each of these programs actually prints, because the word being in the line is the
        // whole of how one is told from another.
        assert_eq!(lld_major("LLD 18.1.8 (compatible with GNU linkers)\n"), Some(18));
        assert_eq!(lld_major("Ubuntu LLD 18.1.3 (compatible with GNU linkers)\n"), Some(18));
        assert_eq!(lld_major("LLD 20.1.2 (compatible with GNU linkers)\n"), Some(20));

        // Binutils and mold do not have it, and neither of them has this problem, so the answer
        // for both is that this check has nothing to say about them.
        assert_eq!(lld_major("GNU ld (GNU Binutils for Ubuntu) 2.42\n"), None);
        assert_eq!(lld_major("mold 2.4.1 (compatible with GNU ld)\n"), None);
        assert_eq!(lld_major(""), None);
    }

    /// A program that prints `text` and exits, which is as much of a linker as this check reads.
    ///
    /// Named after what it says, so that two of them in one test are two files.
    #[cfg(unix)]
    fn a_linker_that_says(tag: &str, text: &str) -> Linker {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("rucc-link-ld-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("a temporary directory");
        let path = dir.join(format!("ld.lld-{tag}"));
        fs::write(&path, format!("#!/bin/sh\necho '{text}'\n")).expect("a script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("an executable one");
        Linker { name: "ld.lld".to_owned(), path }
    }

    #[test]
    #[cfg(unix)]
    fn an_lld_too_old_to_read_exportas_is_refused_for_windows_gnu_and_nowhere_else() {
        // The failure this replaces has no diagnostic at all: 18 writes an import by ordinal zero,
        // exits successfully, and the program dies at startup under wine. tamnd/rucc#1515.
        let windows = Triple::new(Arch::X86_64, Os::Windows, Env::Gnu);
        let old = a_linker_that_says("18", "LLD 18.1.8 (compatible with GNU linkers)");
        let error = suitable(windows, &old).expect_err("18 cannot link this");
        let Error::TooOld { name, found, target } = &error else { panic!("{error:?}") };
        assert_eq!((name.as_str(), *found, target.as_str()), ("ld.lld", 18, "x86_64-windows-gnu"));
        assert!(error.to_string().contains("IMPORT_NAME_EXPORTAS"), "{error}");

        // The same linker for a target whose import libraries have no such records in them, which
        // is every other target, since this is one encoding in one format.
        let linux = Triple::new(Arch::X86_64, Os::Linux, Env::Musl);
        assert_eq!(suitable(linux, &old), Ok(()));

        // And the first one that reads them.
        let new = a_linker_that_says("19", "LLD 19.1.0 (compatible with GNU linkers)");
        assert_eq!(suitable(windows, &new), Ok(()));
    }

    #[test]
    #[cfg(unix)]
    fn an_old_lld_first_in_line_is_passed_over_for_a_newer_one_behind_it() {
        // Ubuntu 24.04 after `apt install lld-19`: 18 is the one on PATH and 19 is somewhere else.
        // Two -B prefixes stand in for the two places, since the search asks them in order too. The
        // name is one nothing on this machine is called, so a real lld on PATH does not answer.
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("rucc-link-two-lld-{}", std::process::id()));
        let mut prefixes = Vec::new();
        for (dir, version) in [("old", "18.1.3"), ("new", "19.1.7")] {
            let dir = root.join(dir);
            fs::create_dir_all(&dir).expect("a temporary directory");
            let path = dir.join("ld.rucc-test-lld");
            fs::write(&path, format!("#!/bin/sh\necho 'Ubuntu LLD {version}'\n"))
                .expect("a script");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("executable");
            prefixes.push(dir);
        }
        let windows = Triple::new(Arch::X86_64, Os::Windows, Env::Gnu);
        let opts = LinkOptions {
            use_ld: Some("rucc-test-lld".to_owned()),
            prefixes,
            ..LinkOptions::default()
        };
        let found = find(windows, &opts).expect("the newer one");
        assert_eq!(found.path, root.join("new").join("ld.rucc-test-lld"));

        // With only the old one there, the answer is the refusal that names it.
        let opts = LinkOptions { prefixes: vec![root.join("old")], ..opts };
        let error = find(windows, &opts).expect_err("only 18 is here");
        assert!(matches!(error, Error::TooOld { found: 18, .. }), "{error:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_long_line_goes_in_a_response_file_that_reads_back_as_it_was() {
        let args: Vec<String> =
            ["-o", r"C:\Users\a b\out.exe", "-m", "i386pep", r#"say "hi""#, "", "plain.o"]
                .map(str::to_owned)
                .to_vec();
        let (front, rest) = split_for_file(&args);
        assert_eq!(front, ["-m", "i386pep"]);
        assert_eq!(
            crate::response_words(&response_text(&rest, false)),
            [&args[..2], &args[4..]].concat()
        );
        // And the Windows way, which leaves the backslashes in a path alone.
        assert_eq!(
            response_text(&rest, true),
            "\"-o\"\n\"C:\\Users\\a b\\out.exe\"\n\"say \\\"hi\\\"\"\n\"\"\n\"plain.o\"\n"
        );
        assert_eq!(response_text(&[r"C:\dir\".to_owned()], true), "\"C:\\dir\\\\\"\n");
        assert!(!too_long(&args, 1000));
        assert!(too_long(&vec!["x".repeat(100); 400], WINDOWS_LINE));
    }

    #[test]
    fn a_program_name_is_tried_with_each_pathext_extension_it_does_not_already_have() {
        let exts = vec![".com".to_owned(), ".exe".to_owned()];
        let dir = Path::new("bin");
        assert_eq!(
            spellings(&dir.join("ld.lld"), &exts),
            [dir.join("ld.lld.com"), dir.join("ld.lld.exe")]
        );
        assert_eq!(spellings(&dir.join("lld-link.EXE"), &exts), [dir.join("lld-link.EXE")]);
        assert_eq!(spellings(&dir.join("ld.lld"), &[]), [dir.join("ld.lld")]);
    }

    #[test]
    #[cfg(unix)]
    fn an_lld_off_path_is_found_under_its_own_name() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("rucc-link-off-path-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("a temporary directory");
        let lld = dir.join("ld.lld");
        fs::write(&lld, "#!/bin/sh\n").expect("a file");
        fs::set_permissions(&lld, fs::Permissions::from_mode(0o755)).expect("permissions");
        let found =
            linker_candidates("ld.lld", &LinkOptions::default(), std::slice::from_ref(&dir));
        let _ = fs::remove_dir_all(&dir);
        assert!(found.contains(&lld), "{found:?}");
    }

    #[test]
    fn lld_is_looked_for_where_package_managers_put_it_newest_version_first() {
        let root = std::env::temp_dir().join(format!("rucc-link-lld-dirs-{}", std::process::id()));
        for dir in ["usr/lib/llvm-18/bin", "usr/lib/llvm-19/bin", "usr/lib/llvm-9/bin", "usr/lib/x"]
        {
            fs::create_dir_all(root.join(dir)).expect("a temporary directory");
        }
        let dirs = lld_dirs(&root, Some(PathBuf::from("C:/Program Files")));
        let under_lib: Vec<_> =
            dirs.iter().filter(|dir| dir.starts_with(root.join("usr/lib"))).collect();
        assert_eq!(
            under_lib,
            [
                &root.join("usr/lib/llvm-19/bin"),
                &root.join("usr/lib/llvm-18/bin"),
                &root.join("usr/lib/llvm-9/bin")
            ]
        );
        assert!(dirs.contains(&root.join("opt/homebrew/opt/lld/bin")), "{dirs:?}");
        assert_eq!(dirs.last(), Some(&PathBuf::from("C:/Program Files").join("LLVM").join("bin")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_linker_says_where_to_get_lld_only_when_lld_was_looked_for() {
        let cross = Error::NoLinker { tried: vec!["ld.lld".to_owned(), "lld".to_owned()] };
        assert!(cross.to_string().contains("lld 19 or newer"), "{cross}");
        let native = Error::NoLinker { tried: vec!["ld".to_owned()] };
        assert_eq!(native.to_string(), "no linker was found; tried ld");
    }

    #[test]
    #[cfg(unix)]
    fn a_linker_that_will_not_say_what_it_is_is_left_alone() {
        // Every linker that is not an lld reaches this check too, and what it can establish is
        // that a specific old lld is here rather than that anything else is fit. Turning "I did
        // not recognise this" into a refusal would break machines this problem never touched.
        let windows = Triple::new(Arch::X86_64, Os::Windows, Env::Gnu);
        let quiet = a_linker_that_says("gnu", "GNU ld (GNU Binutils for Ubuntu) 2.42");
        assert_eq!(suitable(windows, &quiet), Ok(()));

        let missing = Linker { name: "ld.lld".to_owned(), path: PathBuf::from("/no/such/linker") };
        assert_eq!(suitable(windows, &missing), Ok(()));
    }

    #[test]
    fn profiling_a_cross_link_is_refused_because_the_startup_file_is_compiled_code() {
        let target = Triple::new(Arch::X86_64, Os::Linux, Env::Musl);
        let opts = LinkOptions { profile: true, ..cached() };
        let error = cross_line(target, &opts, &one("a.o"), "a.out", &a_sysroot(target))
            .expect_err("there is no gcrt1.o in a generated sysroot");
        let Error::Cross { why } = &error else { panic!("{error:?}") };
        assert!(why.contains("gcrt1.o"), "{why}");
    }

    #[test]
    fn a_distributions_cross_tree_is_used_when_there_is_no_sysroot_of_ours() {
        let usr = std::env::temp_dir().join(format!("rucc-link-usr-{}", std::process::id()));
        let root = usr.join("aarch64-linux-gnu");
        for dir in ["include", "lib"] {
            fs::create_dir_all(root.join(dir)).expect("a scratch tree");
        }
        for version in ["9", "13"] {
            fs::create_dir_all(usr.join("lib/gcc-cross/aarch64-linux-gnu").join(version))
                .expect("a scratch gcc");
        }
        let host = Triple::new(Arch::X86_64, Os::Linux, Env::Gnu);
        let arm = Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu);
        let opts = LinkOptions { usr: Some(usr.clone()), ..cached() };
        let distro = distro_for(arm, &opts, Some(host)).expect("the packages are there");
        assert_eq!(distro.include(), root.join("include"));
        assert_eq!(distro.lib(), root.join("lib"));
        assert_eq!(
            distro.gcc,
            [13, 9].map(|v| usr.join("lib/gcc-cross/aarch64-linux-gnu").join(v.to_string()))
        );
        // And then it is not a link against a sysroot of ours, which is what decides the line.
        assert!(cross_for(arm, &opts, Some(host)).is_none());
        // The host itself, a target with no tree, a named tree and a pinned release all leave it.
        assert!(distro_for(host, &opts, Some(host)).is_none());
        let riscv = Triple::new(Arch::Riscv64, Os::Linux, Env::Gnu);
        assert!(distro_for(riscv, &opts, Some(host)).is_none());
        let named = LinkOptions { sysroot: Some(PathBuf::from("/opt/root")), ..opts.clone() };
        assert!(distro_for(arm, &named, Some(host)).is_none());
        let pinned = LinkOptions {
            pinned: Some("aarch64-linux-gnu.2.28".parse::<TargetTuple>().expect("a release")),
            ..opts.clone()
        };
        assert!(distro_for(arm, &pinned, Some(host)).is_none());
        // A sysroot of ours in the cache wins over the packages, because it is the one pinned.
        let cache = usr.join("cache");
        fs::create_dir_all(Sysroot::in_cache(&cache, arm.tuple()).lib()).expect("a sysroot");
        let fetched = LinkOptions { cache: Some(cache), ..opts };
        assert!(distro_for(arm, &fetched, Some(host)).is_none());
        let _ = fs::remove_dir_all(&usr);
    }

    #[test]
    fn the_host_takes_the_host_line_and_a_tree_the_user_named_takes_it_too() {
        let host = Triple::new(Arch::X86_64, Os::Linux, Env::Gnu);
        let other = Triple::new(Arch::Riscv64, Os::Linux, Env::Musl);
        assert!(cross_for(host, &cached(), Some(host)).is_none());
        assert!(cross_for(other, &cached(), Some(host)).is_some());
        // A tree somebody assembled and named is what `--sysroot` has always meant here, and the
        // native line prefixes every path it decides with it.
        let named = LinkOptions { sysroot: Some(PathBuf::from("/opt/root")), ..cached() };
        assert!(cross_for(other, &named, Some(host)).is_none());
        // A host this compiler cannot name is a host whose directories it should not be guessing at.
        assert!(cross_for(other, &cached(), None).is_some());
    }

    #[test]
    fn a_windows_host_compiles_for_itself_against_the_fetched_tree() {
        // There is no `/usr/include` on Windows, so the host line would find no `stdio.h` at all.
        let host = Triple::new(Arch::X86_64, Os::Windows, Env::Gnu);
        let at =
            cross_for(host, &cached(), Some(host)).expect("the cache is the only tree there is");
        assert!(at.root().ends_with("x86_64-windows-gnu"), "{:?}", at.root());
    }

    #[test]
    fn a_pinned_release_on_this_machines_own_target_is_a_cross_compile() {
        // The case that used to be dropped on the floor. `--target=x86_64-linux-gnu.2.28` on an
        // x86-64 glibc machine read that machine's headers and linked that machine's libc, and the
        // release reached nothing, so what came out was a binary for whatever release the build
        // machine happened to have. A pin is the one thing a person writes to say otherwise.
        let host = Triple::new(Arch::X86_64, Os::Linux, Env::Gnu);
        let pinned = LinkOptions {
            pinned: Some(
                "x86_64-linux-gnu.2.28".parse::<TargetTuple>().expect("a spelling with a release"),
            ),
            ..cached()
        };
        let at = cross_for(host, &pinned, Some(host)).expect("a pin is a cross compile");
        // And against the release's own directory, because the release is in the cache key: a tree
        // produced for 2.28 and a tree produced for 2.44 are two trees and the path has to say which.
        assert!(at.root().ends_with("x86_64-linux-gnu.2.28"), "{:?}", at.root());
        // The release is the whole of the difference. The same command line without it is this
        // machine, which is what every native compile has always been.
        let bare = LinkOptions { pinned: None, ..cached() };
        assert!(cross_for(host, &bare, Some(host)).is_none());
    }

    #[test]
    fn a_glibc_cross_link_writes_its_stubs_beside_the_sysroot_once() {
        let cache = std::env::temp_dir().join(format!("rucc-link-stubs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&cache);
        let target = Triple::new(Arch::X86_64, Os::Linux, Env::Gnu);
        let pinned = LinkOptions {
            cache: Some(cache.clone()),
            pinned: Some("x86_64-linux-gnu.2.28".parse().expect("a spelling with a release")),
            ..LinkOptions::default()
        };
        write_stubs(target, &pinned).expect("x86_64 glibc has a description");
        let dir = cache.join("stubs").join("x86_64-linux-gnu.2.28");
        let libc = dir.join("libc.so");
        let bytes = fs::read(&libc).expect("libc.so was written");
        assert!(bytes.starts_with(b"\x7fELF"));
        assert!(dir.join("libm.so").is_file());
        // 2.28 is before the release that emptied libpthread, so there is no empty one to write.
        assert!(!dir.join("libpthread.so").exists());
        // The second time finds the same bytes and leaves the file alone, which is what keeps a
        // linker in another build from ever reading one that is being replaced.
        let before = fs::metadata(&libc).and_then(|m| m.modified()).expect("a time");
        write_stubs(target, &pinned).expect("again");
        let after = fs::metadata(&libc).and_then(|m| m.modified()).expect("a time");
        assert_eq!(before, after);
        // And nothing for a libc that is not glibc, whose sysroot has a real one in it.
        let musl = LinkOptions {
            cache: Some(cache.clone()),
            pinned: Some("x86_64-linux-musl".parse().expect("musl")),
            ..LinkOptions::default()
        };
        write_stubs(Triple::new(Arch::X86_64, Os::Linux, Env::Musl), &musl).expect("nothing");
        assert!(!cache.join("stubs").join("x86_64-linux-musl").exists());
        let _ = fs::remove_dir_all(&cache);
    }

    #[test]
    fn what_a_cross_link_searches_is_the_sysroot_and_not_this_machine() {
        let dirs = search_dirs(&cached(), foreign());
        // One directory, because that is what the line has, and the same one the line has, because
        // `-print-search-dirs` is what a build system reads to write a link line of its own.
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        assert!(dirs[0].starts_with("/cache"), "{dirs:?}");
        assert!(dirs[0].ends_with("lib"), "{dirs:?}");
        // And what the user wrote still comes first, the way it does on the line itself.
        let mine = LinkOptions { search: vec![PathBuf::from("/opt/mine")], ..cached() };
        assert_eq!(search_dirs(&mine, foreign())[0], PathBuf::from("/opt/mine"));
    }

    #[test]
    fn gnu_ld_is_not_looked_for_against_the_fetched_mingw_sysroot() {
        let windows = Triple::new(Arch::X86_64, Os::Windows, Env::Gnu);
        assert_eq!(order(windows, &cached()), ["ld.lld", "lld"]);
    }

    #[test]
    fn the_linker_looked_for_on_a_cross_link_is_one_that_can_cross() {
        let names = cross_order(Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu));
        assert_eq!(names.first().map(String::as_str), Some("ld.lld"));
        assert!(names.contains(&"aarch64-linux-gnu-ld".to_owned()), "{names:?}");
        // mold links for the machine it is running on, and so does a distribution's own `ld`, so
        // neither is a default here. `-fuse-ld=` is still there for somebody whose is different.
        assert!(!names.iter().any(|name| name.contains("mold")), "{names:?}");
        assert!(!names.contains(&"ld".to_owned()), "{names:?}");
        // And the lookup the driver really does for a target that is not this machine.
        assert_eq!(order(foreign(), &cached()), ["ld.lld", "lld"]);
    }

    #[test]
    fn the_four_flags_become_the_five_modes_they_describe() {
        let plain = LinkOptions::default();
        assert_eq!(mode(&plain), LinkMode::Dynamic);
        assert_eq!(
            mode(&LinkOptions { pie: Some(false), ..plain.clone() }),
            LinkMode::DynamicNoPie
        );
        assert_eq!(mode(&LinkOptions { is_static: true, ..plain.clone() }), LinkMode::Static);
        let both = LinkOptions { is_static: true, pie: Some(true), ..plain.clone() };
        assert_eq!(mode(&both), LinkMode::StaticPie);
        assert_eq!(mode(&LinkOptions { shared: true, ..plain }), LinkMode::Shared);
    }

    #[test]
    fn a_sysroot_that_has_not_been_built_is_named_before_anything_is_compiled() {
        let opts = LinkOptions {
            cache: Some(std::env::temp_dir().join("rucc-a-cache-nobody-filled")),
            ..LinkOptions::default()
        };
        let error = preflight(foreign(), &opts).expect_err("nothing has built one");
        let Error::Sysroot { dir, pinned, .. } = &error else { panic!("{error:?}") };
        assert!(dir.ends_with("x86_64-none"), "{dir}");
        // Nothing is pinned for that target, or for any target yet, so the message says that rather
        // than naming a command that would not work.
        assert!(!pinned, "nothing should be pinned for a bare metal target");
        let said = error.to_string();
        assert!(said.contains("pins none for it to fetch"), "{said}");
    }

    /// The other half of the same message, which is what a target this release does pin an artifact
    /// for is told. Built by hand rather than through `preflight`, because what is being checked is
    /// the message and not which targets `rucc_sysroot::artifact` happens to pin this release.
    #[test]
    fn a_sysroot_that_could_be_fetched_is_told_what_to_run() {
        let said = Error::Sysroot {
            target: "x86_64-linux-musl".to_owned(),
            dir: "/somewhere/sysroots/x86_64-linux-musl".to_owned(),
            pinned: true,
        }
        .to_string();
        assert!(said.contains("`rucc --fetch x86_64-linux-musl`"), "{said}");
        // And the other way out of it, because a person who has a tree already does not want a
        // download.
        assert!(said.contains("--sysroot=<dir>"), "{said}");
    }

    #[test]
    fn a_link_against_this_machine_has_nothing_to_check_before_it_starts() {
        // Its directories are looked for as the line is built, and one that is not there is simply
        // one that is not offered, so there is no question to answer early.
        assert!(preflight(linux(), &LinkOptions::default()).is_ok());
    }

    #[test]
    fn a_runtime_directory_that_is_not_on_this_machine_is_not_offered() {
        let dirs = runtime_dirs(linux(), Some(Path::new("/definitely/not/a/sysroot")));
        assert!(dirs.is_empty(), "{dirs:?}");
    }
}
