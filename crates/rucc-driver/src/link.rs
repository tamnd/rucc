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
//! # What is not here yet
//!
//! Darwin and Windows. `ld64` wants a different line, a platform version load command and a
//! different set of default libraries, and `link.exe` wants another one again. Each arrives with
//! the target that needs it.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
/// # Errors
///
/// [`Error::Target`] for a platform there is no line for yet, which is every one but Linux.
pub fn line(
    target: Triple,
    opts: &LinkOptions,
    items: &[Item],
    output: &str,
) -> Result<Vec<String>, Error> {
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

    // The startup file the C library brings, which is what calls `main` and what passes it the
    // arguments. `Scrt1.o` rather than `crt1.o` when the result moves, because the two differ in
    // whether the reference to `main` in them is one a loader may relocate.
    if opts.wants_startfiles() {
        let first = if opts.shared {
            None
        } else if pie {
            Some("Scrt1.o")
        } else {
            Some("crt1.o")
        };
        for name in first.into_iter().chain(["crti.o"]) {
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

    #[test]
    fn a_runtime_directory_that_is_not_on_this_machine_is_not_offered() {
        let dirs = runtime_dirs(linux(), Some(Path::new("/definitely/not/a/sysroot")));
        assert!(dirs.is_empty(), "{dirs:?}");
    }
}
