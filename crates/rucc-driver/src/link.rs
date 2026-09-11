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
//! # What is not here yet
//!
//! Darwin, and Windows in Microsoft's ABI. `ld64` wants a platform version load command and a
//! different set of default libraries, and `lld-link` wants a `/`-style command line and an import
//! library set out of an SDK nobody may redistribute. Each arrives with the target that needs it,
//! and a cross link to either is refused by name rather than approximated. A mingw-w64 target does
//! have a line, because PE in that environment is written in the GNU style and the import libraries
//! for it are ours to produce.
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

use rucc_sysroot::layout::Sysroot;
use rucc_sysroot::{LinkMode, argv};
use rucc_target::{Arch, Env, Os, Triple};

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
    /// `-Wl,<arg>` and `-Xlinker <arg>`, in order, passed through untouched.
    pub passthrough: Vec<String>,
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
    /// `-fno-builtins-lib`, which leaves our own runtime off the line so that the machine's
    /// libgcc answers for everything instead.
    pub no_builtins_lib: bool,
    /// `-pg`, which changes the link as well as the code.
    ///
    /// The counts a profiled program keeps have to be started before `main` runs and written out
    /// after it returns, and what does both is a start file of its own. So a build that compiles
    /// with the flag and links without it produces a program that calls the hook on every function
    /// and never writes a profile.
    pub profile: bool,
}

impl LinkOptions {
    /// Whether the startup files go on the line.
    fn wants_startfiles(&self) -> bool {
        !self.no_stdlib && !self.no_startfiles
    }

    /// Whether the library the program was written against goes on the line.
    fn wants_defaultlibs(&self) -> bool {
        !self.no_stdlib && !self.no_defaultlibs
    }

    /// Whether the compiler's own runtime goes on the line.
    ///
    /// The same switch as the C library, because `-nodefaultlibs` in GCC means the compiler's
    /// runtime too, and a link that keeps `libgcc` while dropping `libc` is not a thing anyone
    /// asks for on purpose.
    fn wants_runtime(&self) -> bool {
        !self.no_stdlib && !self.no_defaultlibs
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
}

impl std::fmt::Display for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Item::File(path) => f.write_str(path),
            Item::Library(name) => write!(f, "-l{name}"),
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
                write!(f, "no linker was found; tried {}", tried.join(", "))
            }
            Error::Named { name } => {
                write!(f, "-fuse-ld={name} asks for a linker that is not on this machine")
            }
            Error::Target { triple } => {
                write!(f, "there is no link line for {triple} in this compiler yet")
            }
            Error::Cross { why } => f.write_str(why),
            Error::Sysroot { target, dir } => write!(
                f,
                "there is no sysroot for {target} at {dir}, so there is nothing to link it \
                 against. Pass --sysroot=<dir> to name a tree you have already, or see \
                 spec/cross-compile/13-distribution.md section 13.2 for the cache that will hold \
                 one"
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
    if cross_sysroot(target, opts).is_some() {
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
#[must_use]
pub fn cross_sysroot(target: Triple, opts: &LinkOptions) -> Option<Sysroot> {
    cross_for(target, opts, Triple::host())
}

/// The same answer with the host as a parameter, so that both branches are testable on one machine.
fn cross_for(target: Triple, opts: &LinkOptions, host: Option<Triple>) -> Option<Sysroot> {
    if opts.sysroot.is_some() || host == Some(target) {
        return None;
    }
    let cache = opts.cache.as_deref()?;
    Some(Sysroot::in_cache(cache, target.tuple()))
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
        })
        .collect();
    let output = PathBuf::from(output);
    let invocation = argv::Invocation {
        inputs: &inputs,
        output: Some(&output),
        mode: mode(opts),
        search: &opts.search,
        passthrough: &opts.passthrough,
        no_startfiles: !opts.wants_startfiles(),
        no_defaultlibs: !opts.wants_defaultlibs(),
        no_builtins_lib: opts.no_builtins_lib,
        export_dynamic: opts.export_dynamic,
        strip: opts.strip,
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
    let Some(sysroot) = cross_sysroot(target, opts) else { return Ok(()) };
    cross_line(target, opts, &[], "a.out", &sysroot)?;
    // The library directory rather than the root, because the root of a cache directory that has
    // been created and never populated is there and holds nothing. Section 11.6's rule is that
    // suitable is checked and not assumed, and this is the cheapest form of that.
    if !sysroot.lib().is_dir() {
        return Err(Error::Sysroot {
            target: target.tuple().to_canonical_string(),
            dir: sysroot.root().display().to_string(),
        });
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
    for name in &tried {
        if name.contains(std::path::MAIN_SEPARATOR) || name.contains('/') {
            let path = PathBuf::from(name);
            if path.is_file() {
                return Ok(Linker { name: name.clone(), path });
            }
            continue;
        }
        for dir in &opts.prefixes {
            let path = dir.join(name);
            if path.is_file() {
                return Ok(Linker { name: name.clone(), path });
            }
        }
        if let Some(path) = on_path(name) {
            return Ok(Linker { name: name.clone(), path });
        }
    }
    match &opts.use_ld {
        Some(name) => Err(Error::Named { name: name.clone() }),
        None => Err(Error::NoLinker { tried }),
    }
}

/// The first executable of that name on `PATH`.
///
/// Executability is checked rather than assumed, because a directory of that name on `PATH` is
/// not a thing to try to run and neither is a file nobody may execute.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| executable(p))
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
    if let Some(sysroot) = cross_sysroot(target, opts) {
        return cross_line(target, opts, items, output, &sysroot);
    }
    if target.os != Os::Linux {
        return Err(Error::Target { triple: target.to_string() });
    }
    let machine = emulation(target);
    let root = opts.sysroot.as_deref();
    let dirs = library_dirs(target, root);
    // Where a gcc on this machine keeps its own runtime, which is a different place from where
    // the C library keeps its own, and where our runtime is if it was built for this target.
    let runtime = runtime_dirs(target, root);
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
        }
    }
    // After the objects, because a static archive is searched for what is undefined at the point
    // it is reached and a library named before the object that needs it contributes nothing.
    args.extend(runtime_items(opts, &runtime, ours.as_deref()));

    if opts.wants_startfiles() {
        // The other end of `crtbegin`, and it goes before `crtn.o` for the same reason `crti.o`
        // goes before `crtbegin`: the four are two nested pairs and not four separate files.
        let end = if opts.shared || pie { "crtendS.o" } else { "crtend.o" };
        if let Some(path) = find_file(&runtime, end).or_else(|| find_file(&runtime, "crtend.o")) {
            args.push(path.display().to_string());
        }
        if let Some(path) = find_file(&dirs, "crtn.o") {
            args.push(path.display().to_string());
        }
    }

    // Last, so that anything the user said wins over anything decided above, which is what
    // `-Wl,` is for.
    args.extend(opts.passthrough.iter().cloned());
    Ok(args)
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
            let dir = under(sysroot, &format!("{base}/{name}"));
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            let mut versions: Vec<(Vec<u64>, PathBuf)> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .map(|p| (version_key(&p), p))
                .collect();
            // Descending, so the highest version is the first place `find_file` looks. Ties keep
            // the order the directory gave, which is arbitrary and does not matter because two
            // directories that sort the same hold the same version.
            versions.sort_by(|a, b| b.0.cmp(&a.0));
            found.extend(versions.into_iter().map(|(_, path)| path));
        }
    }
    found
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
    const NAME: &str = "librucc_builtins.a";
    let triple = target.to_string();
    let mut places: Vec<PathBuf> = Vec::new();
    for prefix in prefixes {
        places.push(prefix.join(&triple).join(NAME));
        places.push(prefix.join(NAME));
    }
    if let Some(dir) =
        std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        // An install: the compiler in `bin` and its runtime in `lib/rucc/<triple>`.
        if let Some(up) = dir.parent() {
            places.push(up.join("lib").join("rucc").join(&triple).join(NAME));
            // A build tree: the compiler in `target/release` and the runtime, which is built for
            // the target and not the host, in `target/<triple>/release`.
            for profile in ["release", "debug"] {
                places.push(up.join(&triple).join(profile).join(NAME));
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
    // A cross link searches one directory and it is the sysroot's, so this is that and not the
    // machine's. What `-print-search-dirs` says is what a build system pastes into a link line of its
    // own, and an answer that named `/usr/lib` for a target whose link line never goes near it would
    // be worse than no answer at all.
    if let Some(sysroot) = cross_sysroot(target, link) {
        dirs.push(sysroot.lib());
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

/// Runs the linker and waits for it.
///
/// # Errors
///
/// [`Error::Spawn`] when it could not be started, which is a machine problem, and
/// [`Error::Refused`] when it ran and said no, which is a program problem and one the linker has
/// already explained on its own error output.
pub fn run(linker: &Linker, args: &[String]) -> Result<(), Error> {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let status = Command::new(&linker.path).args(&args).status().map_err(|why| Error::Spawn {
        path: linker.path.display().to_string(),
        why: why.to_string(),
    })?;
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
    fn what_the_user_told_the_linker_comes_after_what_this_told_it() {
        let opts = LinkOptions {
            passthrough: vec!["--no-eh-frame-hdr".to_owned()],
            ..LinkOptions::default()
        };
        let args = line(linux(), &opts, &one("a.o"), "a.out").expect("a line");
        assert_eq!(args.last().map(String::as_str), Some("--no-eh-frame-hdr"));
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
        for triple in [
            Triple::new(Arch::X86_64, Os::Darwin, Env::Gnu),
            Triple::new(Arch::X86_64, Os::Windows, Env::Msvc),
        ] {
            let error = line(triple, &LinkOptions::default(), &one("a.o"), "a.out")
                .expect_err("no line for it");
            assert!(matches!(error, Error::Target { .. }), "{error:?}");
        }
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
    fn cached() -> LinkOptions {
        LinkOptions { cache: Some(PathBuf::from("/cache")), ..LinkOptions::default() }
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
        for target in [
            Triple::new(Arch::Aarch64, Os::Darwin, Env::None),
            Triple::new(Arch::X86_64, Os::Windows, Env::Msvc),
        ] {
            let error = cross_line(target, &cached(), &one("a.o"), "a.out", &a_sysroot(target))
                .expect_err("no line for that format");
            let Error::Cross { why } = &error else { panic!("{error:?}") };
            assert!(why.contains(&target.tuple().to_canonical_string()), "{why}");
        }
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
    fn profiling_a_cross_link_is_refused_because_the_startup_file_is_compiled_code() {
        let target = Triple::new(Arch::X86_64, Os::Linux, Env::Musl);
        let opts = LinkOptions { profile: true, ..cached() };
        let error = cross_line(target, &opts, &one("a.o"), "a.out", &a_sysroot(target))
            .expect_err("there is no gcrt1.o in a generated sysroot");
        let Error::Cross { why } = &error else { panic!("{error:?}") };
        assert!(why.contains("gcrt1.o"), "{why}");
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
        let Error::Sysroot { dir, .. } = &error else { panic!("{error:?}") };
        assert!(dir.ends_with("x86_64-none"), "{dir}");
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
