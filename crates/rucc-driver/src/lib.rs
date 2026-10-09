//! The driver: command line parsing, the phase graph, job scheduling and the linker
//! invocation.
//!
//! Design: `spec/04-driver-and-cli.md`. Layer rank 13, see `spec/18-package-layout.md`.
//!
//! This is the only crate that is allowed to know the process exists. It reads the command
//! line, touches the file system, spawns the linker and writes to the terminal, and it hands
//! everything below it a [`Session`]. The binary crate is a `main` that calls
//! [`run`] and nothing else, so that the whole driver is reachable from a test.
//!
//! # Status
//!
//! `--help`, `--version` and `--print-config` are real, which is the `M0` exit criterion in
//! `spec/17-milestones.md`. The phase graph is real and `-###` prints it, and the scheduler
//! that will run it is real and tested.
//!
//! Two phases run. `-E` reads the file, runs phase 4 over it and writes the result, to `-o` or
//! to standard output. `--emit=tast` carries on through phase 7, the parse and the checking,
//! and writes the typed tree. The flags those two read are real with them, which is `-D`, `-U`,
//! `-I`, `-I-`, `-iquote`, `-isystem`, `-idirafter`, `-iprefix`, `-iwithprefix`,
//! `-iwithprefixbefore`, `-include`, `-imacros`, `--sysroot=`, `-isysroot`, `-P`, `-std=`,
//! `-fgnuc-version=`, `-fgnu-as-version=`, `-ansi`, `-ffreestanding`, `-fno-builtin`,
//! `-fno-builtin-<name>`, `-fgnu89-inline`, `-pedantic` and `-Werror`.
//! The phases after them still say they are not implemented.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-driver/0.29.4")]

pub mod assemble;
pub mod cache;
pub mod compile;
pub mod deps;
pub mod dlltool;
pub mod fetch;
mod glibc;
pub mod host;
pub mod install;
mod kbuild;
pub mod library;
pub mod link;
pub mod lto;
mod map;
pub mod msvc;
mod notice;
pub mod phase;
pub mod preprocess;
pub mod schedule;
mod shapes;
mod specs;
pub mod trace;
mod warnings;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use rucc_codegen::coverage::{self, Fired};
use rucc_codegen::lowering::Lowerings;
use rucc_codegen::pressure::Pressure;
use rucc_pp::Dependency;
use rucc_session::{
    Control, Dumps, EmitKind, Hook, Math, Options, Pic, PrefixMap, Preinclude, Protector,
    SaveTemps, Session, Std, Wrapping, runtime,
};
use rucc_sysroot::{Manifest, Sysroot};
use rucc_target::{ObjectFormat, Triple};
use rucc_tuple::TargetTuple;

use crate::link::LinkOptions;

pub use crate::assemble::assemble;
pub use crate::compile::{Artifact, Compiled, Temps, compile, compile_ir};
pub use crate::phase::{ArchiveJob, Input, InputKind, Job, LinkJob, Output, Phase, Plan, Role};
pub use crate::preprocess::{OsFileSystem, Preprocessed, preprocess};
pub use crate::schedule::Jobs;

/// The compiler's version, taken from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Print usage and exit successfully.
    Help,
    /// Print one line and exit successfully, which is what the `-dump` and `-print` family do.
    ///
    /// A build system asks these before it compiles anything, and what it does with the answer
    /// is paste it into a path or into another command line, so each one is a single line with
    /// no decoration around it.
    Print(String),
    /// Print the `-v` banner on standard error and exit successfully, which is `-v` with no input.
    Verbose(String),
    /// Print the resolved configuration and exit successfully.
    PrintConfig(Box<Options>),
    /// Print the passes the level will run and exit successfully.
    PrintPipeline(Box<Options>),
    /// Print every constant `--param` can set, with the value it has after the ones the command
    /// line set, and exit successfully.
    PrintParams(Vec<String>),
    /// Print the phase plan and the link line and exit successfully, which is `-###`.
    PrintPlan {
        /// The resolved options, which is what says what the link line is for.
        opts: Box<Options>,
        /// What to do to each input, and in what order.
        plan: Box<Plan>,
        /// What the command line said about linking.
        link: Box<LinkOptions>,
    },
    /// `--fetch <tuple>`, which gets the sysroot this release pins for a target and installs it.
    ///
    /// The only action in this compiler that may run another program to move bytes onto the
    /// machine, which is `spec/cross-compile/13-distribution.md` section 13.8's rule rather than a
    /// property of how this happens to be written: a compilation has no branch that reaches it.
    Fetch {
        /// The artifact, from the table in [`rucc_sysroot::artifact`]. Resolved here rather than where the
        /// work happens, so that a target nothing is pinned for is a refusal from the parser like
        /// every other thing a command line can ask for and not have.
        what: &'static rucc_sysroot::Pinned,
        /// The target, which names the directory under the cache the tree is installed at and is
        /// checked against the record inside the artifact.
        target: TargetTuple,
        /// Where the cache is, read where everything else that needs it reads it.
        cache: PathBuf,
    },
    /// `--fetch-msvc-sdk <tuple>`, which gets what is behind Microsoft's licence wall.
    ///
    /// The other action that may run another program to move bytes onto the machine, and the only
    /// one that asks a person to accept somebody else's licence first.
    /// `spec/cross-compile/13-distribution.md` section 13.4 is why no release pins an artifact
    /// for these, and nothing about this may ever happen because a compile wanted it to. `--fetch`
    /// of an MSVC target is this action too, starting from the build this release pins rather than
    /// from the one Microsoft's channel names today.
    FetchMsvcSdk {
        /// The target, which says which architecture's CRT library package is wanted.
        target: TargetTuple,
        /// Whether `--accept-licence` was on the command line. Without it the licence and the list
        /// are printed and nothing is downloaded, which is the whole of what the flag is for.
        accepted: bool,
        /// Where the cache is, read where everything else that needs it reads it.
        cache: PathBuf,
        /// Whether the documents at the top of the chain are [`rucc_sysroot::PINNED_BUILD`]'s,
        /// which is `--fetch`, rather than the current channel's, which is `--fetch-msvc-sdk`.
        pinned: bool,
    },
    /// Compile the given inputs.
    Compile {
        /// The resolved options.
        opts: Box<Options>,
        /// What to do to each input, and in what order.
        plan: Box<Plan>,
        /// What the command line said about linking.
        link: Box<LinkOptions>,
        /// How many translation units to compile at once.
        jobs: Jobs,
        /// Whether `-v` asked for the plan to be printed while it runs.
        verbose: bool,
        /// What is worth saying about the command line before anything is compiled, printed as
        /// warnings and once for the whole run rather than once per file.
        ///
        /// These are not diagnostics. A diagnostic is about a piece of source and has a span to
        /// point at, and these are about the way two flags were combined, so there is nothing to
        /// point at and nowhere below the driver that knows both halves. `-w` does not reach them
        /// for the same reason it does not reach a refusal from the parser.
        notes: Vec<String>,
    },
}

/// Why a command line was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    /// The message, lowercase and without a trailing period, in the same shape as any other
    /// diagnostic.
    pub message: String,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

fn err(message: impl Into<String>) -> CliError {
    CliError { message: message.into() }
}

/// The two halves of one prefix mapping flag's argument, where `flag` includes its trailing `=`.
///
/// The split is at the last `=` in what follows the flag, not the first, which is gcc's rule and
/// the only one that lets a directory whose name contains an `=` be the old half. It also means
/// `-fmacro-prefix-map=a=b=c` rewrites `a=b` to `c` rather than `a` to `b=c`, which looks like a
/// trap until you notice the alternative traps the far more common case.
fn rewrite<'a>(arg: &'a str, flag: &str) -> Result<(&'a str, &'a str), CliError> {
    let rest = &arg[flag.len()..];
    PrefixMap::split(rest).ok_or_else(|| {
        let flag = flag.trim_end_matches('=');
        err(format!(
            "`{rest}` is not a rewrite for `{flag}`, which is an old prefix, an `=` and a new one"
        ))
    })
}

/// A question the command line asked instead of asking for a compilation.
///
/// These are answered after the loop rather than where they are read, because every one of them
/// is about the target or about the library search and the last word on both is the end of the
/// command line.
enum Query {
    /// `-dumpmachine`, the triple.
    Machine,
    /// `-dumpversion`, the major number of the GCC release this compiler claims to be.
    Version,
    /// `-dumpfullversion`, the same release in all three numbers.
    FullVersion,
    /// `-print-multiarch`, the directory name a distribution files this target under.
    Multiarch,
    /// `-print-multi-os-directory`, where the libraries are from GCC's own directory.
    MultiOsDirectory,
    /// `-print-search-dirs`, in the three lines GCC prints.
    SearchDirs,
    /// `-print-sysroot`, the root the headers and the libraries are read under.
    Sysroot,
    /// `-print-sysroot-provenance`, what is in that root and where each of it came from.
    SysrootProvenance,
    /// `-print-sysroot-digest`, the one number that names all of it.
    SysrootDigest,
    /// `-print-file-name=<name>`, the full path of a library file.
    FileName(String),
    /// `-print-prog-name=<name>`, the full path of a program.
    ProgName(String),
    /// `-print-libgcc-file-name`, which is `-print-file-name=libgcc.a` under another spelling.
    Libgcc,
}

/// Usage text.
///
/// Deliberately short. `spec/04-driver-and-cli.md` puts the full flag reference in the
/// manual page, because a `--help` nobody can read in one screen is a `--help` nobody reads.
pub const USAGE: &str = "\
rucc, an optimizing C compiler

usage: rucc [options] file...

options:
  -c                     compile and assemble, do not link
  -S                     compile only, emit assembly
  -E                     preprocess only
  -o <file>              write output to <file>, or to standard output for -
  -D <name>[=<value>], -U <name>      define or undefine a macro, in command line order
  -I <dir>               add <dir> to the include search path
  -iquote -isystem -idirafter <dir>   the other chains, -nostdinc drops ours
  -I-, -iprefix <p>, -iwithprefix[before] <dir>   the older spellings of those
  -include <file>, -imacros <file>    read <file> first, the second for its macros only
  --sysroot=<dir>        look for the library's headers under <dir>, -isysroot too
  -P, -dM                with -E: leave out the markers, or dump the macros
  -M -MM -MD -MMD        write a make rule for the source, the last two compile as well
  -MF <file> -MT <t> -MQ <t> -MP -MG   where the rule goes, what it builds, targets with no recipe, missing headers
  -std=<dialect>, -trigraphs   c89 through c2y and the gnu spellings, trigraphs in any of them
  -fgnuc-version=<v> -fgnu-as-version=<v> -fms-compatibility-version=<v>   claim GCC, gas or MSVC
  -x <lang>              treat later inputs as <lang>, or none to stop
  -O<level>              optimize: 0, 1, 2, 3, s, z, fast
  -fsafety=<tier>        check memory safety: off, detect, enforce, kernel
  -f[no-]sanitize=<what>   the negative is taken, the positive is refused by name
  -f[no-]safety-subobject   a write has to stay inside the member it names
  -f[no-]safety-restrict    two restrict pointers of one block may not meet
  -f<pass> -fno-<pass> -fdump-ir=<what> -fopt-info[-<kind>][=FILE]
  -fpass-fuel=<pass>=<n>, -fpass-fuel-global=<n>   stop a pass, or all of them, after n
  -fdisable-<pass>[=<funcs>], -fenable-<pass>[=<funcs>]   run a pass on some functions only
  -g -g0 -gdwarf-5, -fno-omit-frame-pointer, -m[no-]omit-leaf-frame-pointer, -mno-red-zone   debug info, frame pointer, red zone
  -gz[=none|zlib|zlib-gnu] -gno-split-dwarf -g[no-]record-gcc-switches   compress the debug sections, zlib when bare, the flags in DW_AT_producer
  -flto[=auto|jobserver|<n>] -fno-lto -ffat-lto-objects   keep the module in the object, not read at link time yet
  -fprofile-use[=<path>] -fprofile-dir=<dir> --coverage   read, and counted for gcov
  -f[no-]stack-protector[-strong|-all|-explicit], -f[no-]stack-clash-protection, -fcf-protection=<edges>, -fhardened
  -ffunction-sections -fdata-sections, -fno-plt   a section per function or variable, for --gc-sections, calls through the GOT
  -fvisibility=<what>    default, hidden, internal or protected, when nothing in the source said
  -l<name>, -L <dir>, -B <dir>, --gcc-toolchain=<dir>, -specs=<file>   a library, where to look for one, our tools, the GCC, the flags of a dpkg or Red Hat spec file
  -fPIC -fpic -fPIE -fpie, -pipe, -mtls-dialect=   what it does anyway, and -f[no-]common as the target's cc
  -f[no-]strict-aliasing, -f[no-]delete-null-pointer-checks   what it assumes anyway
  -static -shared -pie -no-pie -nostdlib -nostartfiles -nodefaultlibs -rdynamic -s   how to link
  -Wl,<arg> -Xlinker <arg> -fuse-ld=<name>, -Wa,<arg> -Xassembler <arg>   the linker, the assembler
  -Werror -pedantic -pedantic-errors -w -W[no-]system-headers   how much to say, and how fatal
  -m64 -march= -mtune= -mcpu= -mabi= -mcmodel=   what machine to generate for
  -m[no-]<feature> -mexec-model=<model>   a wasm feature, and command or reactor on wasm
  -pg -p, -mfentry -mno-fentry   call a profiler on the way in, and where that call goes
  -fpatchable-function-entry=<n>[,<m>]   room at the top of every function to patch later
  -fwrapv, -fwrapv-pointer, -fno-strict-overflow, -ftrapv   overflow wraps, or stops the program
  -f[no-]exceptions, -f[no-]non-call-exceptions   let an exception unwind through the code
  -f[no-]signed-char, -f[no-]unsigned-char, -f[no-]short-enums   change the ABI
  -ffp-contract=<how>    fuse a multiply and an addition: fast, on or off
  -f[no-]fast-math and each of its members, -f[no-]rounding-math, -fexcess-precision=<how>
  -ffile-prefix-map=<old>=<new>   rewrite that front of every path we put in the output
  -fmacro-prefix-map= -fdebug-prefix-map= -fprofile-prefix-map=   the same, one output each
  -pthread               build for more than one thread, and link the library for it
  -dumpmachine -dumpversion -print-multiarch -print-multi-os-directory -print-search-dirs   what this compiler is
  -print-file-name=<name> -print-prog-name=<name> -print-libgcc-file-name   where a file or a program is
  -print-sysroot         the root the headers and the libraries are read under
  -print-sysroot-provenance   every input under it, where it came from and its licence
  -print-sysroot-digest   the sha256 of that record, which names the whole sysroot in one line
  --fetch <tuple>        get the sysroot this release pins for <tuple> and install it in the cache
  --fetch-msvc-sdk <tuple>   the same for *-windows-msvc, from Microsoft's current build
  --offline              never download anything, which a compilation never does anyway
  --dlltool <args>       write an import library from a .def, as dlltool; --dlltool --help says how
  -j[n]                  compile n translation units at once, default all
  -v, -###               print each phase as it runs, or without running any
  -save-temps[=cwd|obj], -fstack-usage, -time   keep the .i and .s, write a .su, time each step
  --target=<triple>      generate code for <triple>, as the names <triple>-rucc and <triple>-gcc do
  --emit=<kind>          exe, obj, archive, asm, preprocessed, tast, ir, mir-final,
                         wasm-tree, safety-summary, type-granules
  --print-config, --print-pipeline, --print-params   print the setup, the passes or the thresholds
  --param <name>=<value> set one of the optimizer's thresholds for this compilation
  -h, --help, --version  print this message or the version, and exit

See spec/04-driver-and-cli.md for the full flag reference.
";

/// The flag naming a directory for one of the include chains that an argument starts with.
fn search_flag(arg: &str) -> Option<&'static str> {
    ["-iquote", "-isystem", "-idirafter"].into_iter().find(|flag| arg.starts_with(flag))
}

/// The argument of a flag that may be joined to it or may be the next word.
///
/// `-DFOO` and `-D FOO` are the same thing, and `at` is where the flag's own letters end.
fn joined_or_next(
    arg: &str,
    at: usize,
    args: &[String],
    i: &mut usize,
) -> Result<String, CliError> {
    if arg.len() > at {
        return Ok(arg[at..].to_owned());
    }
    let next = args.get(*i).ok_or_else(|| err(format!("{arg} requires an argument")))?;
    *i += 1;
    Ok(next.clone())
}

/// The smallest boundary a function is put on when the command line asked for no alignment at all.
///
/// Eight bytes, which is what gcc 16 gives `-fno-align-functions` on x86-64 and is a boundary every
/// target this compiler has is happy with. It is not zero: a function still has to start somewhere
/// an instruction may start, and the flag asks for the target's minimum rather than for none.
const MIN_FUNC_ALIGN: u32 = 8;

/// Where `-mindirect-branch=` or `-mfunction-return=` sends the branch, which is one of gcc's four
/// answers: left alone, a thunk linked in, a thunk the unit carries a copy of, or the thunk's
/// instructions written in place.
fn thunked(arg: &str, flag: &str) -> Result<rucc_target::Thunk, CliError> {
    match &arg[flag.len()..] {
        "keep" => Ok(rucc_target::Thunk::Keep),
        "thunk-extern" => Ok(rucc_target::Thunk::Extern),
        "thunk" => Ok(rucc_target::Thunk::Comdat),
        "thunk-inline" => Ok(rucc_target::Thunk::Inline),
        other => Err(err(format!(
            "`{other}` is not a way to write the branch, which is keep, thunk, thunk-inline or \
             thunk-extern"
        ))),
    }
}

/// The number of the AArch64 vector register `-ffixed-` names, in any of the spellings gcc takes
/// for it, from `b0` to `v31`.
fn fixed_vector(arg: &str) -> Option<u8> {
    let name = arg.strip_prefix("-ffixed-")?;
    let number = name.strip_prefix(['b', 'h', 's', 'd', 'q', 'v'])?;
    if number.len() > 1 && number.starts_with('0') {
        return None;
    }
    number.parse::<u8>().ok().filter(|&number| number < 32)
}

/// What `-falign-functions=N` asks for, as a power of two, or `None` for the target's own answer.
///
/// Zero and one both mean the default, which is gcc's reading of them, and everything else is
/// rounded up to the next power of two, which is also gcc's: `-falign-functions=3` puts a function
/// on a four byte boundary rather than being refused. Gives back `Err` shaped as an outer `None`
/// only when the text is not a number, since that is the one thing gcc will not read either. A
/// number larger than any alignment makes sense at is clamped rather than refused, for the same
/// reason: this is a preference about speed and a build that wrote a silly one still deserves to
/// compile.
fn function_alignment(text: &str) -> Option<Option<u32>> {
    // gcc takes `N:M:N2:M2`, where everything after the first number is about how far it is willing
    // to go to reach the boundary. Only the boundary is answerable here, so the rest is read to
    // check that it is numbers and then dropped.
    let mut parts = text.split(':');
    let first = parts.next()?;
    if parts.any(|part| part.parse::<u64>().is_err()) {
        return None;
    }
    let want: u64 = first.parse().ok()?;
    if want <= 1 {
        return Some(None);
    }
    let bytes = want.min(1 << 16).next_power_of_two();
    Some(Some(u32::try_from(bytes).ok()?))
}

/// Every name that may follow `-fsanitize=`, which is gcc 16's list and three of this compiler's
/// own.
///
/// The three are on it because `spec/07-types-and-semantics.md` section 7.7 already promises them:
/// each undefined behaviour this compiler exploits is listed there with the check that detects it,
/// and `alias`, `restrict` and `memory` are checks gcc has no spelling for. gcc refuses `memory`
/// outright, since the sanitizer of that name is clang's. A name being here means it is a name
/// rather than a typo, and nothing more than that: every one of them is refused after the loop,
/// because none of them is implemented.
///
/// `all` is deliberately absent. gcc takes it only in the negative, so it is handled where each of
/// those two spellings is read rather than by being on this list.
const SANITIZERS: [&str; 35] = [
    "address",
    "kernel-address",
    "hwaddress",
    "kernel-hwaddress",
    "pointer-compare",
    "pointer-subtract",
    "thread",
    "leak",
    "undefined",
    "shift",
    "shift-base",
    "shift-exponent",
    "integer-divide-by-zero",
    "unreachable",
    "vla-bound",
    "null",
    "return",
    "signed-integer-overflow",
    "bounds",
    "bounds-strict",
    "alignment",
    "object-size",
    "float-divide-by-zero",
    "float-cast-overflow",
    "nonnull-attribute",
    "returns-nonnull-attribute",
    "bool",
    "enum",
    "vptr",
    "pointer-overflow",
    "builtin",
    // AArch64's, which the arm64 kernel asks for with `CONFIG_SHADOW_CALL_STACK`.
    "shadow-call-stack",
    "alias",
    "restrict",
    "memory",
];

/// The command line with every `@file` replaced by the words in the file, the way gcc does it.
///
/// Meson writes the link of a large target this way, so that a command line holding a thousand
/// objects stays under the limit the system puts on one. Postgres's `postgres` executable is the
/// one link in its tree that meson writes as `@postgres.rsp`, and before this the name went to
/// the linker as it was. GNU ld reads response files itself, so it opened the file and found
/// `-Wl,--as-needed` in it, which is a driver flag it has never heard of.
///
/// The rules are libiberty's `expandargv`, since that is what gcc and every other GNU tool read
/// these files with. Words are split on white space, a single or a double quote keeps white
/// space in a word until the matching quote, and a backslash makes the character after it an
/// ordinary one, inside quotes as well as outside. A word the file gives that starts with `@` is
/// read as a response file in turn. A name that cannot be opened is left on the command line as
/// it was, which is what gcc does and which is how a file really called `@x.c` still reaches the
/// loop, where it is refused as an unknown input rather than swallowed. The depth is capped so a
/// file that names itself is an error and not a hang.
fn response_files(args: &[String]) -> Result<Vec<String>, CliError> {
    const DEEPEST: usize = 64;
    fn expand(args: &[String], depth: usize, out: &mut Vec<String>) -> Result<(), CliError> {
        for arg in args {
            let Some(name) = arg.strip_prefix('@') else {
                out.push(arg.clone());
                continue;
            };
            let Ok(text) = std::fs::read_to_string(name) else {
                out.push(arg.clone());
                continue;
            };
            if depth == DEEPEST {
                return Err(err(format!("response file '{name}' is nested too deeply")));
            }
            expand(&response_words(&text), depth + 1, out)?;
        }
        Ok(())
    }
    if !args.iter().any(|arg| arg.starts_with('@')) {
        return Ok(args.to_vec());
    }
    let mut out = Vec::with_capacity(args.len());
    expand(args, 0, &mut out)?;
    Ok(out)
}

/// The words of one response file, split the way libiberty's `buildargv` splits them.
fn response_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // Whether a word has begun, which is not the same as `word` having something in it: `''` is
    // an empty word of its own and has to reach the command line as one.
    let mut begun = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
                begun = true;
            }
            _ if quote == Some(c) => quote = None,
            _ if quote.is_some() => word.push(c),
            '\'' | '"' => {
                quote = Some(c);
                begun = true;
            }
            _ if c.is_whitespace() => {
                if begun {
                    words.push(std::mem::take(&mut word));
                    begun = false;
                }
            }
            _ => {
                word.push(c);
                begun = true;
            }
        }
    }
    if begun {
        words.push(word);
    }
    words
}

/// The command line with every `-Wp,` this compiler understands spelled as its own flags.
///
/// The preprocessor is inside this compiler, so what a build hands it through `-Wp,` has to be
/// read here. Kbuild is the reason: every object in the Linux kernel and in busybox is compiled
/// with `-Wp,-MD,dir/.name.o.d`, which is cpp's spelling of `-MD -MF dir/.name.o.d`. cpp's `-MD`
/// and `-MMD` take the file as their next word where the driver's do not, and the rest are the
/// same flags in both. A `-Wp,` holding anything else is left as it was so the loop refuses it,
/// because dropping part of what a build asked the preprocessor for would be the silent kind of
/// wrong.
fn preprocessor_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        let Some(list) = arg.strip_prefix("-Wp,") else {
            out.push(arg.clone());
            continue;
        };
        let words: Vec<&str> = list.split(',').collect();
        let mut spelled = Vec::new();
        let mut i = 0;
        let understood = loop {
            let Some(&word) = words.get(i) else {
                break true;
            };
            i += 1;
            match word {
                "-MD" | "-MMD" | "-MF" | "-MT" | "-MQ" => {
                    let Some(&value) = words.get(i) else {
                        break false;
                    };
                    i += 1;
                    if word == "-MD" || word == "-MMD" {
                        spelled.extend([word.to_owned(), "-MF".to_owned()]);
                    } else {
                        spelled.push(word.to_owned());
                    }
                    spelled.push(value.to_owned());
                }
                "-MP" => spelled.push(word.to_owned()),
                _ if word.len() > 2
                    && (word.starts_with("-D")
                        || word.starts_with("-U")
                        || word.starts_with("-I")) =>
                {
                    spelled.push(word.to_owned());
                }
                _ => break false,
            }
        };
        if understood {
            out.extend(spelled);
        } else {
            out.push(arg.clone());
        }
    }
    out
}

/// The WebAssembly feature that `-m<feature>` or `-mno-<feature>` names, and whether it turns it
/// on. Nothing for a flag that names no feature.
fn wasm_feature(arg: &str) -> Option<(rucc_target::wasm::Feature, bool)> {
    let name = arg.strip_prefix("-m")?;
    match name.strip_prefix("no-") {
        Some(name) => rucc_target::wasm::Feature::named(name).map(|f| (f, false)),
        None => rucc_target::wasm::Feature::named(name).map(|f| (f, true)),
    }
}

/// The extension a `-m` flag names and whether it turns it on, when it names one.
///
/// `-mno-` is the off form of every one of them, which is also how gcc spells it. A flag that is
/// not an extension, `-mno-red-zone` say, is `None` and is left to the rest of the parser.
fn isa_name(arg: &str) -> Option<(&str, rucc_target::Feature, bool)> {
    let rest = arg.strip_prefix("-m")?;
    let (name, on) = match rest.strip_prefix("no-") {
        Some(name) => (name, false),
        None => (rest, true),
    };
    let known =
        if on { rucc_target::Feature::named(name) } else { rucc_target::Feature::named_off(name) };
    known.map(|feature| (name, feature, on))
}

/// The extensions the machine running the compiler has, which is what `-march=native` means.
///
/// Asked of the processor with `cpuid`, through the standard library, and only when the compiler
/// is running on an x86-64 at all. Anywhere else there is no processor to ask about an x86-64 one,
/// and the driver refuses `-march=native` before it gets here, as a cross gcc does. The list is the
/// extensions whose names are stable in the standard library at this workspace's minimum Rust
/// version, which covers everything [`rucc_target::Feature::honoured`] says yes to and a good deal
/// that it does not.
fn native_isa() -> rucc_target::Isa {
    let base = rucc_target::Isa::baseline();
    #[cfg(target_arch = "x86_64")]
    {
        let mut isa = rucc_target::Choices::new();
        macro_rules! asked {
            ($($detected:tt => $name:literal),* $(,)?) => {
                $(if std::arch::is_x86_feature_detected!($detected) {
                    isa.read($name).expect("a name gcc knows");
                })*
            };
        }
        asked! {
            "sse3" => "sse3",
            "ssse3" => "ssse3",
            "sse4.1" => "sse4.1",
            "sse4.2" => "sse4.2",
            "sse4a" => "sse4a",
            "popcnt" => "popcnt",
            "avx" => "avx",
            "avx2" => "avx2",
            "fma" => "fma",
            "f16c" => "f16c",
            "bmi1" => "bmi",
            "bmi2" => "bmi2",
            "lzcnt" => "lzcnt",
            "xsave" => "xsave",
            "aes" => "aes",
            "pclmulqdq" => "pclmul",
            "sha" => "sha",
            "cmpxchg16b" => "cx16",
            "adx" => "adx",
            "rdrand" => "rdrnd",
            "rdseed" => "rdseed",
        }
        isa.over(base)
    }
    #[cfg(not(target_arch = "x86_64"))]
    base
}

/// The AArch64 extensions the machine running the compiler has, which is what `-march=native`
/// means there.
///
/// Only the CRC32 extension, which is the one [`rucc_target::Isa::aarch64_march`] reads, asked
/// of the processor through the standard library. On any other machine there is nothing to ask,
/// and the driver refuses `-march=native` before it gets here.
fn native_aarch64() -> rucc_target::Isa {
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("crc") {
            return rucc_target::Isa::aarch64_march("armv8-a+crc");
        }
    }
    rucc_target::Isa::NONE
}

/// The target that the options start from: the host, or on a host that is not a target, the last
/// `--target=` or the target of `--fetch`.
///
/// A host that is not a target is rucc running as WebAssembly, until rucc has a wasm backend. The
/// loop reads `--target=` again and refuses a value that does not parse, so a bad value here only
/// has to give an error that is not wrong.
fn starting_target(host: Option<Triple>, args: &[String]) -> Result<Triple, CliError> {
    if let Some(host) = host {
        return Ok(host);
    }
    let fetched = args.iter().enumerate().rev().find_map(|(i, arg)| match arg.as_str() {
        "--fetch" => args.get(i + 1).map(String::as_str),
        _ => arg.strip_prefix("--fetch="),
    });
    let named = args.iter().rev().find_map(|arg| arg.strip_prefix("--target=")).or(fetched);
    match named {
        Some(named) => named.parse().or_else(|e| {
            named.parse().ok().and_then(Triple::from_tuple).ok_or_else(|| err(format!("{e}")))
        }),
        None if host::WASM => Err(err(
            "rucc running as WebAssembly has no default target yet, so give one with --target=",
        )),
        None => Err(err("this host is not a supported target and no --target was given")),
    }
}

/// The flags that `DW_AT_producer` records, in the order of the command line.
///
/// This is the gcc rule in a short form. gcc records the flags that change the code: `-f`, `-m`,
/// `-O`, `-g`, `-std=` and `-ansi`. It does not record a flag that names a path or a macro, a
/// warning, a flag for the output or the link, or a flag for the diagnostics. A path in the
/// producer would make two builds in two directories give different objects.
fn switches(args: &[String]) -> Vec<String> {
    // The flags that take the next word as their value. The value is skipped with the flag.
    const SEPARATE: &str = "-o -I -D -U -include -imacros -idirafter -iprefix -iwithprefix \
        -iwithprefixbefore -isystem -iquote -isysroot -imultilib -MF -MT -MQ -x -Xlinker \
        -Xassembler -Xpreprocessor -L -l -u -T -z -B -e -aux-info --sysroot -arch -target";
    // The `-f` flags that name a path or that are about the diagnostics, as prefixes.
    const NOT_RECORDED: &str = "-ffile-prefix-map= -fdebug-prefix-map= -fmacro-prefix-map= \
        -fprofile-prefix-map= -fdiagnostics- -fno-diagnostics- -fmessage-length= -fmax-errors= \
        -fdump- -fopt-info -fuse-ld= -fcolor-diagnostics -fno-color-diagnostics";
    let mut out = Vec::new();
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        let arg = arg.as_str();
        if SEPARATE.split_whitespace().any(|flag| flag == arg) {
            words.next();
            continue;
        }
        let code = arg.starts_with("-f") || arg.starts_with("-m") || arg.starts_with("-O");
        // `-gz` is a driver flag in gcc, so the compiler proper does not see it and does not record it.
        let debug = arg.starts_with("-g")
            && !arg.ends_with("record-gcc-switches")
            && !arg.starts_with("-gz");
        let dialect = arg.starts_with("-std=") || arg == "-ansi";
        if (code || debug || dialect)
            && !NOT_RECORDED.split_whitespace().any(|skip| arg.starts_with(skip))
        {
            out.push(arg.to_owned());
        }
    }
    out
}

/// The name of the macro that `-D` defines: what comes before the `=` and before the `(` of a
/// function-like macro.
fn macro_name(define: &str) -> &str {
    let name = define.split('=').next().unwrap_or_default();
    name.split('(').next().unwrap_or_default()
}

/// Where the configuration files are: `<prefix>/lib/rucc` beside the binary in `<prefix>/bin`,
/// and then `/etc/rucc`.
fn config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(prefix) = std::env::current_exe().ok().and_then(|exe| {
        exe.parent().and_then(std::path::Path::parent).map(std::path::Path::to_path_buf)
    }) {
        dirs.push(prefix.join("lib").join("rucc"));
    }
    if !cfg!(windows) {
        dirs.push(PathBuf::from("/etc/rucc"));
    }
    dirs
}

/// The command line with the flags of the configuration files in front of it, and the files.
///
/// A distribution sets its defaults in `<row>.cfg`, for example `/etc/rucc/x86_64-linux-gnu.cfg`
/// with the hardening flags that its GCC turns on. The row is the one of `--target=`, or the
/// default row. Each file holds flags, and `#` starts a comment that goes to the end of the line.
/// The flags come before the command line, so the command line can turn each one off. A later
/// file comes after an earlier one, so `/etc` wins over the install. `--no-default-config` reads
/// no file, as in clang. Section 10.3 of the Linux plan.
///
/// # Errors
///
/// A file that is there and cannot be read.
fn with_config(
    args: Vec<String>,
    dirs: &[PathBuf],
) -> Result<(Vec<String>, Vec<String>), CliError> {
    if args.iter().any(|arg| arg == "--no-default-config") {
        return Ok((args, Vec::new()));
    }
    let named = args.iter().rev().find_map(|arg| arg.strip_prefix("--target="));
    let target = match named {
        Some(named) => named.parse::<Triple>().ok(),
        None => Triple::host(),
    };
    let Some(target) = target else { return Ok((args, Vec::new())) };
    let row = target.tuple().to_canonical_string();
    let mut words = Vec::new();
    let mut files = Vec::new();
    for dir in dirs {
        let path = dir.join(format!("{row}.cfg"));
        if !path.is_file() {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| err(format!("{}: {e}", path.display())))?;
        words.extend(text.lines().flat_map(|line| {
            let flags = line.split('#').next().unwrap_or_default();
            flags.split_whitespace().map(str::to_owned).collect::<Vec<_>>()
        }));
        files.push(path.display().to_string());
    }
    words.extend(args);
    Ok((words, files))
}

/// Parses a command line, without the program name.
///
/// # Errors
///
/// Returns the message to print when the arguments do not name a compilation this compiler
/// can attempt.
pub fn parse_args(args: &[String]) -> Result<Action, CliError> {
    let expanded = preprocessor_args(&response_files(args)?);
    let (expanded, configs) = with_config(expanded, &config_dirs())?;
    let expanded = specs::expand(expanded).map_err(err)?;
    let args = expanded.as_slice();
    let host = match starting_target(Triple::host(), args) {
        Ok(target) => target,
        // Help names no target, so it does not need one.
        Err(_) if args.iter().any(|arg| arg == "-h" || arg == "--help") => return Ok(Action::Help),
        Err(why) => return Err(why),
    };
    let mut opts = Options::new(host);
    // Where the compiler is running, which is what `DW_AT_comp_dir` is and what a debugger joins a
    // relative file name onto. Asked here rather than where the debug sections are written, because
    // this is the one layer that is allowed to look at the process it is in, and because a command
    // line that compiles four files should give the same answer for all four.
    opts.working_dir = std::env::current_dir().ok().map(|dir| dir.to_string_lossy().into_owned());
    opts.config_files = configs;
    // The last of the two flags wins, and gcc records the flags when neither is given.
    let record = args
        .iter()
        .rev()
        .find_map(|arg| match arg.as_str() {
            "-grecord-gcc-switches" => Some(true),
            "-gno-record-gcc-switches" => Some(false),
            _ => None,
        })
        .unwrap_or(true);
    if record {
        opts.switches = switches(args);
    }
    let mut inputs: Vec<Input> = Vec::new();
    let mut print_config = false;
    let mut print_pipeline = false;
    let mut print_params = false;
    let mut print_plan = false;
    let mut verbose = false;
    let mut jobs = Jobs::default();
    let mut nostdinc = false;
    let mut sysroot: Option<PathBuf> = None;
    // What the command line is worth warning about, filled in after the loop rather than during it,
    // because every question of this kind is about two flags and the last word on both of them is
    // the end of the loop.
    let mut notes: Vec<String> = Vec::new();
    // The whole ten field target, kept beside the three field one because `--target=` can pin a
    // libc version and `Triple` has nowhere to put it. It decides `__GLIBC_MINOR__` and nothing
    // else today, and `None` is a command line that named no target, which is this machine.
    let mut pinned: Option<TargetTuple> = None;
    // `-m64`, `-m32` or `-m16`, the last of them on the line.
    let mut word: Option<&str> = None;
    // What `-mregparm=` last said, weighed after the loop against the machine the word size and
    // `--target=` settle on.
    let mut regparm: Option<&str> = None;
    let mut min_version: Option<rucc_tuple::Version> = None;
    // The first flag that only an Apple linker understands, for the refusal after the loop when the
    // target is not Apple, and what `-arch` asked for, which is checked against the target there.
    let mut apple_only: Option<String> = None;
    let mut arches: Vec<String> = Vec::new();
    // What the `-fpic` family and the `-fpie` family last said, if anything, kept apart because gcc
    // keeps them apart. A positive spelling of either clears the other, since gcc's option table
    // chains the four so that the last one written wins, and a negative one only speaks for its
    // own family. The answer is worked out after the loop.
    let mut pic: Option<bool> = None;
    let mut cmodel = rucc_target::CodeModel::Small;
    let mut pie: Option<bool> = None;
    let mut output = None;
    let mut link = LinkOptions::default();
    // Whether `-static-pie` was written, which is the one way to ask for a static program that
    // moves itself. `-static` with `-pie` is not that, and is weighed after the loop.
    let mut static_pie = false;
    // `-fhardened`, which is weighed after the loop against each flag the line names.
    let mut hardened = false;
    let mut query: Option<Query> = None;
    // `--version`, answered after the loop because the banner names the GCC release claimed and
    // `-fgnuc-version=` may come after it.
    let mut version = false;
    // Whether `-std=` or `-ansi` said what the dialect is. When neither did, a claimed GCC release
    // decides it after the loop, the way that release's own default did.
    let mut std_given = false;
    // Each flag that came with a later gcc than some claim could be, with that release and what
    // gcc says to it. Weighed after the loop, because `-fgnuc-version=` may come after them.
    // `spec/04-driver-and-cli.md` section 4.12.
    let mut newer: Vec<(u32, String)> = Vec::new();
    // What `--fetch` named, and whether `--offline` forbade it. Both are weighed after the loop
    // because either can be written after the other.
    let mut fetch: Option<String> = None;
    // The other fetch, kept apart from the one above because they are different commands with
    // different rules, and weighed after the loop for the same reason that one is.
    let mut fetch_msvc: Option<String> = None;
    let mut accepted = false;
    let mut offline = false;
    let mut threads = false;
    // Which sanitizers are still asked for by the end of the command line. Accumulated across the
    // loop rather than answered where it was read, because `-fno-sanitize=` turns one off and a
    // build that asks for a check and then takes it back has asked for nothing. What happens to a
    // set that is not empty is decided after the loop.
    let mut sanitizers: Vec<&str> = Vec::new();
    // The `-ffast-math` family in the order it was written, replayed after the loop on top of
    // what `-Ofast` implies. gcc applies a level's defaults before any flag and the flags in order
    // after that, so `-fno-fast-math -Ofast` is not fast math, and only a replay can say so.
    let mut math_flags: Vec<&str> = Vec::new();
    let mut ofast = false;
    // `-mdaz-ftz` and `-mno-daz-ftz`, which decide the startup file directly and outrank the
    // family on that one question.
    let mut daz_ftz: Option<bool> = None;
    // The instruction set extensions the `-m` flags named, in order, and the processor `-march`
    // named last. Both are weighed after the loop, because a processor supplies only what no flag
    // spoke for whichever order they came in, and because `--target=` may come after either and
    // decide that neither means anything. See `rucc_target::isa`.
    let mut isa = rucc_target::Choices::new();
    let mut isa_flag: Option<&str> = None;
    let mut isa_on: Option<&str> = None;
    let mut march: Option<&str> = None;
    // What `-fexceptions` and `-fno-exceptions` last said, if either was written. It is kept apart
    // from the field because `-fnon-call-exceptions` turns exceptions on only when neither was,
    // which is gcc's rule and is why `-fno-exceptions -fnon-call-exceptions` defines no
    // `__EXCEPTIONS` whichever order the two come in.
    let mut exceptions: Option<bool> = None;
    // `-x` applies to inputs that come after it and stays in effect until the next one, which
    // is why it is tracked across the loop rather than attached to a single argument.
    let mut forced: Option<InputKind> = None;
    // What `-iprefix` last said, stuck on the front of every later `-iwithprefix`. It applies to
    // the flags after it and not the ones before, so a command line may set it more than once.
    // GCC's default is its own installed header directory with the last component taken off,
    // which is a path a cross compiler's build system knows and passes; there is no equivalent
    // here, so with no `-iprefix` the prefix is nothing and `-iwithprefix` names a directory
    // outright.
    let mut iprefix = String::new();
    // Every word handed to the assembler with `-Wa,` or `-Xassembler`, beside the argument it came
    // from so that a refusal can name both.
    let mut asm_words: Vec<(String, String)> = Vec::new();

    // The architecture the kernel's flags are answered for, which is the last `--target=` on the
    // line wherever it is written, so that `-mno-outline-atomics --target=aarch64-linux-gnu` is
    // read for AArch64. A target that does not parse is left for the loop to refuse.
    let arch = args
        .iter()
        .rev()
        .find_map(|arg| arg.strip_prefix("--target="))
        .and_then(|target| target.parse::<Triple>().ok())
        .map_or(opts.target.arch, |target| target.arch);
    // Either x86, since `-m32` on an x86-64 target only changes the machine once the loop is done.
    let x86 = matches!(arch, rucc_target::Arch::X86_64 | rucc_target::Arch::X86);
    let aarch64 = arch == rucc_target::Arch::Aarch64;
    // The WebAssembly features. `-mcpu=` names a set, and `-m<feature>` and `-mno-<feature>` change
    // one feature. They are weighed after the loop, where the set comes first and the flags come
    // after it in the order written, whatever the order of `-mcpu=` and the flags. That is clang's
    // rule, and `rucc_target::wasm::resolve` has the rest of it.
    let wasm_row = arch == rucc_target::Arch::Wasm32;
    let mut wasm_cpu = rucc_target::wasm::Cpu::default();
    let mut wasm_flags: Vec<(rucc_target::wasm::Feature, bool, &str)> = Vec::new();
    // `-fmin-function-alignment=`, which is weighed after the loop against what
    // `-falign-functions` said, in whichever order the two came.
    let mut min_function_align: Option<u32> = None;
    // The register files the kernel keeps out of every function, which are weighed after the
    // loop: the vector registers are off once the extensions they belong to are, whichever flag
    // took those away, whichever flag took them, and in whichever order they came.
    let mut general_regs_only = false;
    let mut x87 = true;
    let mut fp_ret_in_387 = true;
    // Where the stack protector's canary is, which is put together after the loop because the
    // four flags that say it may come in any order and gcc lets the later one win for each.
    let mut guard_global = false;
    let mut guard_reg: Option<rucc_target::Segment> = None;
    let mut guard_offset: Option<i32> = None;
    let mut guard_symbol: Option<&str> = None;
    // The AArch64 spellings, which name a system register rather than a segment. `sysreg` with
    // `sp_el0` is the copy an arm64 kernel keeps in each task.
    let mut guard_task = false;
    let mut guard_sp_el0 = false;
    // `-mpreferred-stack-boundary=`, weighed once the machine is settled.
    let mut boundary: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        match arg {
            "-h" | "--help" => return Ok(Action::Help),
            // The flags the kernel's build passes that nothing below answers, and a few that
            // something below would answer without saying which issue is about them. First, so
            // that the table's answer is the one given. See `kbuild`.
            // Where a structure of one, two, four or eight bytes comes back, which only i386
            // System V has a choice about. The kernel builds every 32 bit unit with the first.
            "-freg-struct-return" => opts.reg_struct_return = true,
            "-fpcc-struct-return" => opts.reg_struct_return = false,
            // A vector register kept out of the allocator on AArch64, which the kernel's AEGIS
            // code asks for so that the S-box its `asm` loads into `v16` to `v31` stays there.
            _ if aarch64 && fixed_vector(arg).is_some() => {
                let Some(number) = fixed_vector(arg) else { continue };
                opts.fixed_vectors |= 1 << number;
            }
            _ if kbuild::row(arg, arch).is_some() => {
                let Some(row) = kbuild::row(arg, arch) else { continue };
                if let kbuild::Answer::Refused(why, issue) = row.answer {
                    return Err(err(kbuild::refusal(arg, why, issue)));
                }
                if row.since > 0 {
                    newer.push((row.since, format!("unknown option `{arg}`")));
                }
                // A row with something to remember: `x18` is the static chain of a nested
                // function, which the lowering refuses to build once it has been promised away.
                if arg == "-ffixed-x18" {
                    opts.fixed_x18 = true;
                }
                // And whether a load or store may be at any address, which the last of the two
                // spellings says.
                if let Some(strict) = arg.strip_suffix("strict-align") {
                    opts.strict_align = strict == "-m";
                }
            }
            "--version" => version = true,
            // The sysroot fetch, which is weighed after the loop rather than acted on here, because
            // `--offline` written after it has to be able to forbid it. Both spellings, since a
            // flag that takes a tuple gets written both ways and neither is a guess at what the
            // other meant.
            "--fetch" => {
                let value = args
                    .get(i)
                    .ok_or_else(|| err("--fetch requires the target to get a sysroot for"))?;
                i += 1;
                fetch = Some(value.clone());
            }
            _ if arg.starts_with("--fetch=") => {
                fetch = Some(arg["--fetch=".len()..].to_owned());
            }
            // The other fetch, which is section 13.4's. Same two spellings for the same reason,
            // and weighed after the loop so that `--offline` and `--accept-licence` written after
            // it are read whichever order somebody put them in.
            "--fetch-msvc-sdk" => {
                let value = args.get(i).ok_or_else(|| {
                    err("--fetch-msvc-sdk requires the target to get the SDK for")
                })?;
                i += 1;
                fetch_msvc = Some(value.clone());
            }
            _ if arg.starts_with("--fetch-msvc-sdk=") => {
                fetch_msvc = Some(arg["--fetch-msvc-sdk=".len()..].to_owned());
            }
            // Both spellings of the word, because the compiler's own prose uses one of them and
            // most of the people typing this will reach for the other, and being told that a flag
            // is not a flag over the letter in the middle of it is a puzzle rather than a message.
            "--accept-licence" | "--accept-license" => accepted = true,
            // Accepted on any command line and only ever read by the fetch, because an ordinary
            // compile downloads nothing with or without it. So this flag takes nothing away today,
            // which is the property section 13.2 asks for rather than an omission: a build that
            // passes it is saying what it expects of this compiler, and what it expects is already
            // true.
            "--offline" => offline = true,
            // Anywhere but first it would be a compiler command line with a dlltool one inside it,
            // and there is no reading of that which is not a guess.
            "--dlltool" => {
                return Err(err(
                    "--dlltool has to be the first argument, since everything after it is a \
                     dlltool command line rather than a compiler one",
                ));
            }
            "--print-config" => print_config = true,
            "--print-pipeline" => print_pipeline = true,
            "--print-params" => print_params = true,
            // gcc's spelling, both with the setting as the next argument and after an `=`. The
            // name is checked here rather than when the passes read it, so that a misspelled one
            // is an error and not a run that measured the default and said it measured something
            // else.
            "--param" => {
                let spec = args.get(i).ok_or_else(|| err("--param requires name=value"))?.clone();
                i += 1;
                rucc_cost::heuristics::Param::new(&spec).map_err(|e| err(e.to_string()))?;
                opts.params.push(spec);
            }
            _ if arg.starts_with("--param=") => {
                let spec = &arg["--param=".len()..];
                rucc_cost::heuristics::Param::new(spec).map_err(|e| err(e.to_string()))?;
                opts.params.push(spec.to_owned());
            }
            "-###" => print_plan = true,
            "-v" => verbose = true,
            // The files a compilation goes through, kept rather than thrown away. The bare
            // spelling means `=obj` and not `=cwd`, which is not what the manual says and is what
            // gcc 16 does; `SaveTemps::Object` carries the measurement.
            "-save-temps" => opts.save_temps = SaveTemps::Object,
            _ if arg.starts_with("-save-temps=") => {
                opts.save_temps = arg["-save-temps=".len()..].parse().map_err(err)?;
            }
            // A `.su` beside every file compiled, one line per function saying how much stack it
            // takes. Where the file goes is the plan's business, see `Job::stack_usage`.
            "-fstack-usage" => opts.stack_usage = true,
            "-fno-stack-usage" => opts.stack_usage = false,
            // How long each step took. A misspelling of this is worth rejecting rather than
            // ignoring, since a run that says nothing looks like a compilation that took no time.
            "-time" => opts.time = true,
            "-c" => opts.emit = EmitKind::Object,
            "-S" => opts.emit = EmitKind::Asm,
            "-E" => opts.emit = EmitKind::Preprocessed,
            "-fsyntax-only" => opts.emit = EmitKind::SyntaxOnly,
            "-g" => opts.debug_info = true,
            // GCC's own levels of how much debug information to write. Zero is none and every
            // other number is some, and this compiler has one amount, so the numbers above zero
            // all mean the same thing here. `-ggdb` is the same flag asking for whatever the
            // debugger on the machine prefers, which is what we emit anyway.
            "-g0" => opts.debug_info = false,
            "-g1" | "-g2" | "-g3" | "-ggdb" | "-ggdb1" | "-ggdb2" | "-ggdb3" => {
                opts.debug_info = true;
            }
            // The version of DWARF to write. We write 5 by default and 4 when asked, which is what a
            // kernel with `CONFIG_DEBUG_INFO_DWARF4` asks for. Like gcc, naming a version also turns
            // debug information on. Any other version is refused rather than handed over as a file
            // the build's tools cannot read.
            "-gdwarf" | "-gdwarf-5" => {
                opts.debug_info = true;
                opts.dwarf_version = 5;
            }
            "-gdwarf-4" => {
                opts.debug_info = true;
                opts.dwarf_version = 4;
            }
            _ if arg.starts_with("-gdwarf-") => {
                return Err(err(format!(
                    "{arg}: this compiler writes DWARF 4 and 5 and no other version, see \
                     spec/11-debug-info.md"
                )));
            }
            // Whether the debug information goes in a file of its own beside the object. gcc
            // writes that `.dwo` whether or not it found anything to put in it, which means a
            // build system that declares the file as an output gets one and a make rule that
            // depends on it fires. Refused for that reason rather than taken: section 4.1 takes a
            // flag that changes nothing and refuses one that changes what is produced, and a file
            // that does not appear is the plainest change of that kind there is. The negative
            // spelling is taken, because putting it all in the object is what happens anyway.
            "-gno-split-dwarf" => {}
            // Read before the loop, see `switches`.
            "-grecord-gcc-switches" | "-gno-record-gcc-switches" => {}
            "-gsplit-dwarf" => {
                return Err(err(format!(
                    "{arg}: this compiler writes no separate `.dwo` file, and a build that \
                     expects one beside each object would wait for a file that never arrives, \
                     see spec/11-debug-info.md"
                )));
            }
            // How the debug sections are compressed. Bare `-gz` means `zlib`, as it does in gcc, and
            // a name that is not a method at all is refused here as a typo.
            "-gz" => {
                opts.compress = rucc_session::Compress::Zlib;
                link.compress = opts.compress;
            }
            _ if arg.starts_with("-gz=") => {
                let how = &arg["-gz=".len()..];
                opts.compress = how.parse().map_err(|()| {
                    err(format!(
                        "`{how}` is not a way to compress debug sections, which is none, zlib, \
                         zlib-gnu or zstd"
                    ))
                })?;
                link.compress = opts.compress;
            }
            "-Werror" => opts.warnings_are_errors = true,
            // Takes back an earlier `-Werror`, the way gcc reads it, which is how a build that has
            // `-Werror` in flags it does not own gets its warnings back as warnings.
            "-Wno-error" => opts.warnings_are_errors = false,
            // Nothing that is not fatal is said at all. Read at the one place a diagnostic goes
            // through rather than here, so that a warning `-w` dropped is not counted either.
            "-w" => opts.warnings = false,
            // Off by default, the way gcc has it off. A header that came with the machine is not
            // one the person compiling can change, so a warning about it is noise, and under
            // `-Werror` it is a build that stops on a line nobody in the project wrote. Somebody
            // porting a header does want to hear all of it, which is what the flag is for.
            "-Wsystem-headers" => opts.system_header_warnings = true,
            "-Wno-system-headers" => opts.system_header_warnings = false,
            // The diagnostics the standard requires, as errors, and no others. See
            // `rucc_diag::Named::pedantic_errors` for why it is not `-Werror`.
            "-pedantic-errors" => {
                opts.pedantic = true;
                opts.named_warnings.pedantic_errors();
            }
            "-P" => opts.line_markers = false,
            // Keep comments in the output of `-E`. Taken and not acted on: every comment still
            // becomes a space. What asks for it in practice is the kernel's vDSO linker script,
            // and ld reads the script the same with its comments gone.
            "-C" | "-CC" => {}
            // The dependency family, which section 4.4 calls required because every build system
            // that generates its own makefiles asks for it. The two that end in `D` write a file
            // beside the object and let the compilation happen, and the two that do not write to
            // standard output and stop after it. Nothing here turns the system headers back on
            // once a flag has turned them off, which is GCC's behaviour and is why `-MM -M` is
            // `-MM`: the flag asking for fewer of them is the one with something to say.
            "-M" => {
                opts.deps.emit = true;
                opts.deps.instead_of_compiling = true;
            }
            "-MM" => {
                opts.deps.emit = true;
                opts.deps.instead_of_compiling = true;
                opts.deps.system_headers = false;
            }
            "-MD" => opts.deps.emit = true,
            "-MMD" => {
                opts.deps.emit = true;
                opts.deps.system_headers = false;
            }
            "-MP" => opts.deps.phony = true,
            "-MG" => opts.deps.generated = true,
            // These three take a word and only in the separated form, which is how GCC spells
            // them and how every build system writes them.
            "-MF" | "-MT" | "-MQ" => {
                let value =
                    args.get(i).ok_or_else(|| err(format!("{arg} requires an argument")))?;
                i += 1;
                match arg {
                    "-MF" => opts.deps.file = Some(value.clone()),
                    // The whole of the difference between the two. `-MT` is for a build that has
                    // already escaped what it is passing, and `-MQ` is for one that has a name
                    // and wants it to arrive as that name.
                    "-MT" => opts.deps.targets.push(value.clone()),
                    _ => opts.deps.targets.push(deps::escaped(value)),
                }
            }
            // The questions a build system asks before it compiles anything. Answered after the
            // loop, because each one is about the target or the library search and the command
            // line has not finished saying what those are.
            // Of the three `-dump` questions GCC answers the first and stops, so `-dumpfullversion
            // -dumpversion`, which is how a script asks for the whole version from a GCC old
            // enough not to know the first flag, gets the whole version from a new one too.
            "-dumpversion" | "-dumpfullversion" | "-dumpmachine"
                if matches!(query, Some(Query::Machine | Query::Version | Query::FullVersion)) => {}
            "-dumpmachine" => query = Some(Query::Machine),
            // Both answer with the GCC release in `__GNUC__` rather than our own version, because
            // what asks is a build script deciding which GCC it is talking to, and `0.11` reads as
            // a GCC too old to have anything. The first one is the whole version before GCC 7 and
            // only the major number from 7 on, see `GnucVersion::dumpversion`.
            "-dumpversion" => query = Some(Query::Version),
            "-dumpfullversion" => query = Some(Query::FullVersion),
            "-print-multiarch" => query = Some(Query::Multiarch),
            "-print-multi-os-directory" => query = Some(Query::MultiOsDirectory),
            "-print-search-dirs" => query = Some(Query::SearchDirs),
            "-print-sysroot" => query = Some(Query::Sysroot),
            // Both spellings, because this one is ours rather than GCC's and our own documents
            // write it both ways: section 13.5 of `spec/cross-compile/13-distribution.md` gives it
            // two dashes like the other flags we invented, and document 12's table gives it one
            // like the `-print-` family it sits in. A person who reads either and types what it
            // says is right, so neither is refused.
            "-print-sysroot-provenance" | "--print-sysroot-provenance" => {
                query = Some(Query::SysrootProvenance);
            }
            "-print-sysroot-digest" | "--print-sysroot-digest" => {
                query = Some(Query::SysrootDigest);
            }
            "-print-libgcc-file-name" => query = Some(Query::Libgcc),
            _ if arg.starts_with("-print-file-name=") => {
                query = Some(Query::FileName(arg["-print-file-name=".len()..].to_owned()));
            }
            _ if arg.starts_with("-print-prog-name=") => {
                query = Some(Query::ProgName(arg["-print-prog-name=".len()..].to_owned()));
            }
            // A program built to run in more than one thread. On every platform this compiler
            // targets that is a macro the library's headers read and one more library on the
            // link line, and the library is added after the loop so that it lands after the
            // objects that refer to it.
            "-pthread" | "-pthreads" => {
                opts.defines.push("_REENTRANT".to_owned());
                threads = true;
            }
            "-ansi" => {
                opts.std = Std::C89;
                opts.gnu_extensions = false;
                std_given = true;
            }
            // Replaces trigraphs in any mode. gcc has no flag to turn them off in a mode that
            // has them, and neither does this.
            "-trigraphs" => opts.trigraphs = true,
            // `-Wpedantic` is the same flag under the name the `-W` family gives it, which is
            // the spelling a build system that groups its warning flags tends to write.
            "-pedantic" | "-Wpedantic" => opts.pedantic = true,
            // Both directions, because a build that needs this for one directory turns it back
            // off for the next one rather than leaving it on for the whole tree.
            "-fpermissive" => opts.permissive = true,
            "-fno-permissive" => opts.permissive = false,
            "-ffreestanding" => opts.hosted = false,
            "-fhosted" => opts.hosted = true,
            "-fno-builtin" => opts.builtins = false,
            "-fbuiltin" => opts.builtins = true,
            // The C89 dialects are under GNU's reading whatever this says, so turning it off
            // there is turning off something the dialect asked for, which is accepted and does
            // nothing. gcc refuses that command line, and there is nothing it could have meant.
            "-fgnu89-inline" => opts.gnu89_inline = true,
            "-fno-gnu89-inline" => opts.gnu89_inline = false,
            // Which trailing arrays are flexible. The bare spelling is the strictest level, as it
            // is in gcc, and the kernel passes `=3`. All three came in gcc 13, and the kernel
            // probes for them, so a persona before that has to say what gcc 12 says.
            "-fstrict-flex-arrays" => {
                opts.strict_flex_arrays = 3;
                newer.push((13, format!("unknown option `{arg}`")));
            }
            "-fno-strict-flex-arrays" => {
                opts.strict_flex_arrays = 0;
                newer.push((13, format!("unknown option `{arg}`")));
            }
            _ if arg.starts_with("-fstrict-flex-arrays=") => {
                newer.push((13, format!("unknown option `{arg}`")));
                let text = &arg["-fstrict-flex-arrays=".len()..];
                let level: u8 = text
                    .parse()
                    .map_err(|_| err(format!("{arg}: the level has to be a number from 0 to 3")))?;
                if level > 3 {
                    return Err(err(format!("{arg}: the level has to be a number from 0 to 3")));
                }
                opts.strict_flex_arrays = level;
            }
            // Both directions of each, because a build system that wants one of these usually
            // writes it beside the flag that turns it back off for one directory.
            "-fno-omit-frame-pointer" => opts.frame_pointer = Some(true),
            "-fomit-frame-pointer" => opts.frame_pointer = Some(false),
            // Ubuntu, Fedora and Arch pass the negative with `-fno-omit-frame-pointer`, so that
            // `perf` can walk each stack.
            "-mno-omit-leaf-frame-pointer" => opts.leaf_frame_pointer = true,
            "-momit-leaf-frame-pointer" => opts.leaf_frame_pointer = false,
            // Both directions again, for the same reason, and a third answer for a command line
            // that wrote neither: see `reorder_blocks` in `rucc_session`.
            "-freorder-blocks" => opts.reorder_blocks = Some(true),
            "-fno-reorder-blocks" => opts.reorder_blocks = Some(false),
            "-freorder-blocks-and-partition" => opts.partition_blocks = Some(true),
            "-fno-reorder-blocks-and-partition" => opts.partition_blocks = Some(false),
            "-freorder-functions" => opts.reorder_functions = Some(true),
            "-fno-reorder-functions" => opts.reorder_functions = Some(false),
            "-ftoplevel-reorder" => opts.toplevel_reorder = Some(true),
            "-fno-toplevel-reorder" => opts.toplevel_reorder = Some(false),
            // gcc's name for the scheduler that runs after the registers are handed out, which is
            // the only one rucc has: see `schedule_insns` in `rucc_session`. gcc also takes
            // `-fschedule-insns` for the pass before allocation, and reading that one as this would
            // be a flag that says a pass ran when none did, so it is dropped with gcc's other pass
            // names further down.
            "-fschedule-insns2" => opts.schedule_insns = Some(true),
            "-fno-schedule-insns2" => opts.schedule_insns = Some(false),
            // A call in tail position as a jump: see `sibling_calls` in `rucc_session`.
            "-foptimize-sibling-calls" => opts.sibling_calls = Some(true),
            "-fno-optimize-sibling-calls" => opts.sibling_calls = Some(false),
            "-mno-red-zone" => opts.red_zone = false,
            "-mred-zone" => opts.red_zone = true,
            // Five flags rather than one with an argument, which is how gcc spells them and how
            // every build line writes them. Last one wins, because a package build puts
            // `-fstack-protector-strong` in its global flags and a directory that cannot have one
            // turns it back off on the line after.
            "-fno-stack-protector"
            | "-fno-stack-protector-all"
            | "-fno-stack-protector-strong"
            | "-fno-stack-protector-explicit" => {
                opts.protector = Protector::None;
            }
            "-fstack-protector" => opts.protector = Protector::Buffers,
            "-fstack-protector-strong" => opts.protector = Protector::Strong,
            "-fstack-protector-all" => opts.protector = Protector::All,
            "-fstack-protector-explicit" => opts.protector = Protector::Explicit,
            // The other half of what a hardened build asks for, and it is a question about the
            // frame rather than about the function, so it is a switch rather than a level.
            "-fstack-clash-protection" => opts.stack_clash = true,
            "-fno-stack-clash-protection" => opts.stack_clash = false,
            // Arch and Fedora build with this. The call reads the address from the GOT.
            "-fno-plt" => opts.plt = false,
            "-fplt" => opts.plt = true,
            // GCC 14's set of the flags above and a few more. See `harden`.
            "-fhardened" => hardened = true,
            "-fno-hardened" => hardened = false,
            // The third of them, and the one that is a question with an argument rather than a
            // family of spellings, because what it asks about is which of the two edges of a
            // control flow transfer is checked. Bare is both of them, which is what gcc does.
            "-fcf-protection" => opts.control = Control::Full,
            "-fno-cf-protection" => opts.control = Control::None,
            // Two spellings of the same request, which is what gcc has as well. `-p` was the older
            // profiler and `-pg` the one that also recorded who called whom, and on every platform
            // this compiler targets there is now one hook and both ask for it.
            "-pg" | "-p" => {
                opts.profile = true;
                link.profile = true;
            }
            // Accepted on their own and doing nothing on their own, which is gcc's behaviour: they
            // say where the call goes and a command line that asked for no call has nowhere to put
            // one. That matters because a build system that sets `-mfentry` globally and `-pg` per
            // directory is a build system that would otherwise fail on every other directory.
            "-mfentry" => opts.hook = Hook::Early,
            "-mno-fentry" => opts.hook = Hook::Late,
            // Only on x86, as with gcc, where they are the i386 back end's and unknown to the
            // others. That back end is both widths, so a 32 bit kernel asks for them too. Taken on
            // their own like `-mfentry`, and doing nothing without `-pg`.
            "-mrecord-mcount" | "-mno-record-mcount" if x86 => {
                opts.record_mcount = arg == "-mrecord-mcount";
            }
            "-mnop-mcount" | "-mno-nop-mcount" if x86 => {
                opts.nop_mcount = arg == "-mnop-mcount";
            }
            // The hook the call goes to and the section it is listed in, in place of the target's
            // and `__mcount_loc`, on x86 where the others are. A function's own `fentry_name` and
            // `fentry_section` win over them. Empty is the default again.
            _ if x86 && arg.starts_with("-mfentry-name=") => {
                let name = &arg["-mfentry-name=".len()..];
                opts.fentry_name = (!name.is_empty()).then(|| name.to_owned());
            }
            _ if x86 && arg.starts_with("-mfentry-section=") => {
                let section = &arg["-mfentry-section=".len()..];
                opts.fentry_section = (!section.is_empty()).then(|| section.to_owned());
            }
            // GCC drops its own include directory along with the system ones, because its
            // headers are half of a pair with the library's and half a pair is worse than
            // none. A build that passes this is supplying the whole set itself.
            "-nostdinc" => nostdinc = true,
            "-o" => {
                output = Some(args.get(i).ok_or_else(|| err("-o requires an argument"))?.clone());
                i += 1;
            }
            // What the files kept beside an output are named after, which is `-save-temps` and
            // `-fstack-usage` so far. gcc takes each of the three in the separated form only, and
            // its driver passes them to every compilation it runs, so a build that copied a
            // command line out of gcc's `-v` has them. See `phase::aux_base` for what they do.
            "-dumpbase" | "-dumpbase-ext" | "-dumpdir" => {
                let value =
                    args.get(i).ok_or_else(|| err(format!("{arg} requires an argument")))?.clone();
                i += 1;
                match arg {
                    "-dumpbase" => opts.dump_base = Some(value),
                    "-dumpbase-ext" => opts.dump_base_ext = Some(value),
                    _ => opts.dump_dir = Some(value),
                }
            }
            // Apple's spelling of `--sysroot`, and the one its own build systems pass. The
            // two mean the same thing here: the configured directories are under there rather
            // than under the root.
            "-isysroot" => {
                let dir = args.get(i).ok_or_else(|| err("-isysroot requires an argument"))?;
                i += 1;
                sysroot = Some(PathBuf::from(dir));
            }
            // A directory joined or separate, as gcc takes them. The kernel's ptrace selftests
            // write `-iquote../../../../include/uapi`.
            _ if search_flag(arg).is_some() => {
                let flag = search_flag(arg).unwrap_or_default();
                let dir = joined_or_next(arg, flag.len(), args, &mut i)?;
                match flag {
                    "-iquote" => opts.search.push_quote(dir),
                    "-isystem" => opts.search.push_system(dir),
                    _ => opts.search.push_after(dir),
                }
            }
            "-iprefix" => {
                iprefix = args.get(i).ok_or_else(|| err("-iprefix requires an argument"))?.clone();
                i += 1;
            }
            // Where GCC puts these is not where its manual says it puts them, and this is the
            // measured answer rather than the documented one: `-iwithprefix` lands in the
            // `-isystem` slot and not the `-idirafter` slot, and `-iwithprefixbefore` lands in
            // the `-I` slot. A cross build that uses them is relying on the behaviour, since
            // that is the compiler it was developed against.
            "-iwithprefix" | "-iwithprefixbefore" => {
                let dir = args.get(i).ok_or_else(|| err(format!("{arg} requires an argument")))?;
                i += 1;
                let dir = format!("{iprefix}{dir}");
                if arg == "-iwithprefix" {
                    opts.search.push_system(dir);
                } else {
                    opts.search.push_bracket(dir);
                }
            }
            "-include" | "-imacros" => {
                let name = args.get(i).ok_or_else(|| err(format!("{arg} requires an argument")))?;
                i += 1;
                opts.preincludes
                    .push(Preinclude { name: name.clone(), macros_only: arg == "-imacros" });
            }
            // The flag `-iquote` was introduced to replace, still passed by build systems old
            // enough to predate the replacement. It is not a directory: it says that every `-I`
            // so far is for quoted includes only, and that a quoted include stops looking next
            // to the file that wrote it.
            "-I-" => opts.search.split_quote_chain(),
            // `-x c` and `-xc`, both of which gcc takes. busybox and toybox probe the compiler
            // with the joined one.
            _ if arg.starts_with("-x") => {
                let lang = joined_or_next(arg, 2, args, &mut i)?;
                forced = if lang == "none" {
                    None
                } else {
                    Some(InputKind::from_x_arg(&lang).map_err(|e| err(format!("{e}")))?)
                };
            }
            // Not a GCC flag. spec/03-architecture.md section 3.5 compiles several
            // translation units in one process rather than making the build system fork, and
            // section 3.8's determinism check compares `-j1` against `-j16`, so the knob has
            // to exist and has to be spelled the way `make` spells it.
            // `-DFOO`, `-D FOO` and the same for `-U` and `-I`. Both forms are in wide use
            // and a build system may produce either, so both are read here rather than
            // being normalised by whatever generated the command line.
            // GCC reads -D and -U in command line order, so the last one for a name decides. A
            // makefile that writes `-U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=2` after the flags of a
            // distribution gets level 2. So each flag takes back an earlier flag of the other
            // kind for the same name.
            _ if arg.starts_with("-D") => {
                let value = joined_or_next(arg, 2, args, &mut i)?;
                let name = macro_name(&value).to_owned();
                opts.undefines.retain(|undefine| *undefine != name);
                opts.defines.push(value);
            }
            _ if arg.starts_with("-U") => {
                let value = joined_or_next(arg, 2, args, &mut i)?;
                opts.defines.retain(|define| macro_name(define) != value);
                opts.undefines.push(value);
            }
            _ if arg.starts_with("-I") => {
                let dir = joined_or_next(arg, 2, args, &mut i)?;
                opts.search.push_bracket(dir);
            }
            _ if arg.starts_with("-std=") => {
                let name = &arg["-std=".len()..];
                let (std, gnu) = Std::from_flag(name)
                    .ok_or_else(|| err(format!("unknown dialect `{name}`, see --help")))?;
                opts.std = std;
                opts.gnu_extensions = gnu;
                std_given = true;
            }
            // Section 4.5. The claim decides which half of glibc's `sys/cdefs.h` we are
            // handed, so a differential run that does not set it is comparing two compilers
            // that believe they are different compilers.
            // GCC packs these into one flag, so `-dDI` is two of them. Letters in the family
            // that we have not written yet are accepted and ignored, because a dump is a
            // debugging aid and a build that asks for one should still compile. A letter
            // outside the family falls through to the unknown option error, which is what
            // keeps `-dumpversion` from being read as a dump of nothing.
            _ if Dumps::is_family(arg) => {
                opts.dumps.add(&arg[2..]);
            }
            // One name at a time, which is what a build that means its own `memcpy` and the
            // library's everything else writes. The name is not checked against a list, because
            // the flag is about what the program means by a name and a program is allowed to mean
            // something by a name this compiler has never heard of.
            _ if arg.starts_with("-fno-builtin-") => {
                opts.no_builtin.push(arg["-fno-builtin-".len()..].to_owned());
            }
            // The assembler this compiler stands in for, which is what `-Wa,--version` names.
            _ if arg.starts_with("-fgnu-as-version=") => {
                let v = &arg["-fgnu-as-version=".len()..];
                opts.gnu_as = v.parse().map_err(|why| err(format!("-fgnu-as-version=: {why}")))?;
            }
            _ if arg.starts_with("-fgnuc-version=") => {
                let v = &arg["-fgnuc-version=".len()..];
                opts.gnuc = v.parse().map_err(err)?;
                opts.gnuc_given = true;
            }
            // The MSVC release an MSVC row claims, which is `_MSC_VER` and nothing else here.
            _ if arg.starts_with("-fms-compatibility-version=") => {
                let v = &arg["-fms-compatibility-version=".len()..];
                opts.msc = v.parse().map_err(err)?;
            }
            // Which of Microsoft's C runtimes an MSVC row links against, in clang's spelling of
            // `cl.exe`'s `/MT` and `/MD`. The headers are told through `_DLL` and the link through
            // the libraries it names, so it is both a compile flag and a link one. The two debug
            // runtimes are refused by name rather than taken as the release ones, since what they
            // link is a different set of libraries and a program that asked for one wants its
            // checks.
            _ if arg.starts_with("-fms-runtime-lib=") => {
                let dll = match &arg["-fms-runtime-lib=".len()..] {
                    "static" => false,
                    "dll" => true,
                    debug @ ("static_dbg" | "dll_dbg") => {
                        return Err(err(format!(
                            "-fms-runtime-lib={debug} asks for Microsoft's debug C runtime, which \
                             this compiler does not link against yet. static and dll are the two \
                             it has"
                        )));
                    }
                    other => {
                        return Err(err(format!(
                            "-fms-runtime-lib= takes static or dll, and `{other}` is neither"
                        )));
                    }
                };
                opts.ms_dll_runtime = dll;
                link.crt = if dll { rucc_sysroot::Crt::Dll } else { rucc_sysroot::Crt::Static };
            }
            // Apple's clang spelling for GNU's nested functions, which are always on in GNU C and
            // here, so both forms are taken and dropped. See spec/13-gnu-compat.md section 13.3.
            "-fnested-functions" | "-fno-nested-functions" => {}
            // Which of the two links the output is for, which is a real difference and not a
            // description of what happens anyway. Everything here is position independent either
            // way, and what these decide is whether a name may be one another object defines or
            // replaces, because a link that produces an executable puts every name in the same
            // program and a link that produces a shared library does not.
            //
            // It matters that they are accepted at all, whatever they then do. Every autoconf and
            // cmake build puts `-fPIC` on the compile line, so a compiler that rejects it cannot
            // be the `CC` of a project that has a configure script, whatever else it can do. That
            // is how this was found: building SQLite's test fixture stopped on it.
            "-fPIC" | "-fpic" => {
                pic = Some(true);
                pie = None;
            }
            // Not a synonym of the pair above, which is what they were treated as until #756. The
            // library is the expensive answer and gcc makes it the one that has to be asked for,
            // so this is also what nothing at all means.
            "-fPIE" | "-fpie" => {
                pie = Some(true);
                pic = None;
            }
            // A different question from the pair above, and the one every distribution build of a
            // shared library answers. `-fPIC` decides how an address is reached, and this decides
            // whether the optimizer may believe a body it can see, because an exported name is one
            // the dynamic linker may find another definition of first. On by default, which is
            // gcc's arrangement and is the honest answer, and off is a promise the build makes and
            // nothing checks.
            "-fsemantic-interposition" => opts.interposition = true,
            "-fno-semantic-interposition" => opts.interposition = false,
            // Two requests rather than one, and the same table answers both, so what decides is
            // whether either of them is standing. gcc arranges it the same way: the asynchronous
            // one is the default here and it implies the other, and a line that asks for a table
            // and against an asynchronous one gets a table.
            "-fasynchronous-unwind-tables" => opts.async_unwind_tables = true,
            "-fno-asynchronous-unwind-tables" => opts.async_unwind_tables = false,
            "-funwind-tables" => opts.unwind_tables = true,
            "-fno-unwind-tables" => opts.unwind_tables = false,
            // The other direction, which is a request rather than a description: the output is
            // linked where it runs, as a kernel and a `-no-pie` executable are, and nothing is to
            // be reached through a global offset table. The capital spellings are gcc's too, and
            // `-fno-PIE` is the one every x86 kernel from 4.9 on writes. What the pair of them
            // comes to is worked out after the loop, because gcc keeps the two questions apart and
            // `-fPIC -fno-pie` is still a library. See tamnd/rucc#2276.
            "-fno-pic" | "-fno-PIC" => pic = Some(false),
            "-fno-pie" | "-fno-PIE" => pie = Some(false),
            // A section per function and a section per variable, which is what makes
            // `--gc-sections` able to drop anything: a linker can leave out a section nothing
            // reaches and cannot leave out half of one. Both directions are taken, and the off
            // one is the default rather than a refusal, since a build that writes it is asking
            // for what happens anyway.
            "-ffunction-sections" => opts.function_sections = true,
            "-fno-function-sections" => opts.function_sections = false,
            "-fdata-sections" => opts.data_sections = true,
            "-fno-data-sections" => opts.data_sections = false,
            // Whether a file scope declaration with no initializer is offered to the linker as a
            // common symbol for it to merge, or written into `.bss` as an ordinary defined one.
            // Unwritten, the target answers, which is on for Darwin and off everywhere else.
            "-fcommon" => opts.common = Some(true),
            "-fno-common" => opts.common = Some(false),
            // Whether a variable the program initialized to zero may go in `.bss` rather than in
            // `.data` with its zeros written out. One with no initializer goes there either way.
            "-fzero-initialized-in-bss" => opts.zero_initialized_in_bss = true,
            "-fno-zero-initialized-in-bss" => opts.zero_initialized_in_bss = false,
            // What overflows rather than being undefined. Every one of these takes something away
            // from the optimizer rather than asking it to do anything, which is why the negative
            // spellings are the interesting ones and the positive spellings are the default.
            //
            // `-fno-strict-overflow` is both of the others, which is gcc's own reading of it: its
            // help text for `-fstrict-overflow` says "negated as -fwrapv -fwrapv-pointer". So it is
            // written here as the pair rather than kept as a third thing to test everywhere.
            //
            // `-ftrapv` is the exception and is the one that asks for something. It is the other
            // answer to the question `-fwrapv` answers, so the two cannot both hold and each clears
            // the other, which makes the last one on the command line the one that counts. That is
            // gcc 16's behaviour and was measured rather than read: `-ftrapv -fwrapv` emits no
            // checked calls and `-fwrapv -ftrapv` emits them. The positive spelling of the pointer
            // question is left alone by both, because neither has anything to say about it.
            "-fwrapv" => {
                opts.wrapping.signed = true;
                opts.wrapping.trap = false;
            }
            "-fno-wrapv" => opts.wrapping.signed = false,
            "-fwrapv-pointer" => opts.wrapping.pointer = true,
            "-fno-wrapv-pointer" => opts.wrapping.pointer = false,
            "-fno-strict-overflow" => opts.wrapping = Wrapping::ALL,
            // Which does not clear the checked one, because gcc does not: `-ftrapv
            // -fstrict-overflow` still emits the calls. It says what is assumed and not what
            // happens.
            "-fstrict-overflow" => {
                opts.wrapping.signed = false;
                opts.wrapping.pointer = false;
            }
            "-ftrapv" => {
                opts.wrapping.trap = true;
                opts.wrapping.signed = false;
            }
            "-fno-trapv" => opts.wrapping.trap = false,
            // The two flags that say what a plain `char` is, which is one question with two
            // spellings each: gcc reads `-fno-signed-char` as `-funsigned-char` and
            // `-fno-unsigned-char` as `-fsigned-char`, so there are four ways to write two
            // answers and the last one written wins. Nothing is set until one of them is given,
            // because the target's own ABI is the answer otherwise and it is not the same answer
            // everywhere: x86-64 and Apple's arm64 are signed, Linux's arm64 is not.
            "-fsigned-char" | "-fno-unsigned-char" => opts.char_signed = Some(true),
            "-funsigned-char" | "-fno-signed-char" => opts.char_signed = Some(false),
            // And the size of an enumeration, which is the other thing in this group that changes
            // the ABI rather than the code.
            "-fshort-enums" => opts.short_enums = true,
            "-fno-short-enums" => opts.short_enums = false,
            // And Microsoft's reading of an anonymous member, which changes the layout of every
            // record that writes a tag on one. Nothing is set until one of them is given, because
            // the target is the answer otherwise: gcc's mingw build has this on and its Linux
            // build has it off.
            "-fms-extensions" => opts.ms_extensions = Some(true),
            "-fno-ms-extensions" => opts.ms_extensions = Some(false),
            // Both directions of this one are recorded, and what they decide is whether lowering
            // names the type each access goes through. Turning it off is the front end leaving the
            // name off rather than a pass being told to ignore one it can see, which is one
            // condition in one place, and it is the reading that survives link time optimization:
            // a unit built with the flag off keeps its own answer when its bodies end up in a
            // module beside bodies that were not.
            //
            // Nothing in the pipeline reads those names yet. Layer 3 of the alias analysis does
            // and is tested, and no pass at any level asks the alias analysis anything today, so
            // no program compiles differently for having passed this. The flag is wired anyway,
            // because the change that makes a pass ask is not the change anybody will remember to
            // wire it in, and a flag that is taken and dropped once the names mean something is
            // the miscompilation `spec/04-driver-and-cli.md` section 4.1 warns about in as many
            // words.
            "-fstrict-aliasing" => opts.strict_aliasing = true,
            "-fno-strict-aliasing" => opts.strict_aliasing = false,
            // The same shape of answer for the same reason, and the flag the kernel writes beside
            // the one above it.
            //
            // Nothing here concludes that a pointer is not null from the fact that it was
            // dereferenced. There is no such conclusion to draw from, because no pass records one:
            // a load says where it read and nothing else, and a comparison against null is an
            // ordinary comparison of two values the optimizer has no fact about. So a function
            // that reads through a pointer and then tests it keeps the test, which is what the
            // kernel wants and what `-fno-delete-null-pointer-checks` asks for, and what gcc has
            // to be asked for because it draws the conclusion by default.
            //
            // `-fdelete-null-pointer-checks` is the request to draw it, and it goes the way
            // `-fstrict-aliasing` does: assuming less than was asked for costs speed and not
            // correctness, and `-O2` implies it, so refusing it would stop builds for nothing.
            "-fdelete-null-pointer-checks" | "-fno-delete-null-pointer-checks" => {}
            // The floating point group. Each of these has a restrictive spelling and a permissive
            // one. The restrictive ones, `-frounding-math` and `-ftrapping-math`, say that the
            // rounding mode may have been changed and that an exception raised by an operation may
            // be looked at. An operation on floating constants is folded the way gcc folds it,
            // in the default rounding mode and only when it raises nothing worse than an inexact
            // answer, so `-ftrapping-math`, the default, keeps a division by zero and an overflow
            // as code that runs, and `-frounding-math` keeps an inexact answer as code that runs.
            // Both are taken with the rest of the family below.
            //
            // `-fno-trapping-math` changes one more answer, because there is one conversion this
            // compiler does not fold and gcc folds under it, and the two answers differ. Converting a constant floating value to an integer type it does not fit in
            // is undefined behaviour rather than a value: left to the hardware it is one
            // instruction and the answer is the integer indefinite value, and folded it is the
            // nearest end of the integer's range. Both compilers leave it to the instruction by
            // default and gcc folds it under this flag, so a program built with it and compiled
            // without it gets a different number rather than a slower one. `-ftrapping-math` is
            // gcc's default, so a build spelling it out is asking for what it already has.
            //
            // The rest of the family goes with it, `-ffast-math` included, and all of them are
            // taken now. Each is a licence rather than a request and none of them is taken, so
            // the code does not change. What does change is the macros gcc
            // defines for each licence, which a header reads, and the startup file `-ffast-math`
            // links, which puts the hardware in flush to zero mode. Both are done after the loop,
            // because the family is a set of switches over the same fields and the last word on
            // each of them is the end of the command line.
            "-ftrapping-math"
            | "-fno-trapping-math"
            | "-frounding-math"
            | "-fno-rounding-math"
            | "-ffast-math"
            | "-fno-fast-math"
            | "-funsafe-math-optimizations"
            | "-fno-unsafe-math-optimizations"
            | "-fmath-errno"
            | "-fno-math-errno"
            | "-ffinite-math-only"
            | "-fno-finite-math-only"
            | "-fsigned-zeros"
            | "-fno-signed-zeros"
            | "-freciprocal-math"
            | "-fno-reciprocal-math"
            | "-fassociative-math"
            | "-fno-associative-math" => math_flags.push(arg),
            // Whether the startup file that sets flush to zero is linked, asked directly. gcc
            // links it for a shared object too when this is written, which the family does not.
            "-mdaz-ftz" => daz_ftz = Some(true),
            "-mno-daz-ftz" => daz_ftz = Some(false),
            // About temporary files rather than about code. There is nothing between the phases of
            // one compilation here to write to a file in the first place.
            "-pipe" => {}
            // Read before the loop, by `with_config`.
            "--no-default-config" => {}
            // Preprocess the input, which a C compile always does. GCC has it for Fortran, and
            // meson writes it when it asks a compiler for its predefined macros.
            "-cpp" => {}
            // Nothing here writes colour, so all of these are the same answer, and it is the answer
            // that costs nothing: the diagnostics come out plain either way and no build depends on
            // an escape sequence being there. Taken rather than refused because cmake writes
            // `-fdiagnostics-color=always` on every compile line when the generator is ninja, which
            // makes this the second most common flag after `-fPIC` to stop a build over a question
            // about how the text looks.
            "-fdiagnostics-color" | "-fno-diagnostics-color" => {}
            _ if arg.starts_with("-fdiagnostics-color=") => {}
            // The link flags. None of them changes the compilation, which is why they are
            // collected apart from `opts` and why `-lm` on a `-c` line is a note rather than an
            // error: it is a thing said to a linker that is not going to run.
            "-static" => link.is_static = true,
            // A static program that moves itself to wherever it is loaded, which is both at once.
            // The kernel's exec selftests build their load address checks with it.
            "-static-pie" => {
                link.is_static = true;
                link.pie = Some(true);
                static_pie = true;
            }
            "-shared" => link.shared = true,
            "-r" => link.relocatable = true,
            "-pie" => link.pie = Some(true),
            "-no-pie" | "-nopie" => link.pie = Some(false),
            "-nostdlib" => link.no_stdlib = true,
            "-nostartfiles" => link.no_startfiles = true,
            "-nodefaultlibs" => link.no_defaultlibs = true,
            "-fno-builtins-lib" => link.no_builtins_lib = true,
            "-fbuiltins-lib" => link.no_builtins_lib = false,
            "-rdynamic" | "-export-dynamic" => link.export_dynamic = true,
            // Apple's two kinds of loadable file. `-dynamiclib` is a library other links name,
            // which is what `-shared` is on a Mac as well, and `-bundle` is a file a program opens
            // with `dlopen`, which is how Postgres links every module there. tamnd/rucc#2010.
            "-dynamiclib" => {
                link.shared = true;
                apple_only.get_or_insert_with(|| arg.to_owned());
            }
            "-bundle" => {
                link.bundle = true;
                apple_only.get_or_insert_with(|| arg.to_owned());
            }
            // The rest of what clang's Darwin driver takes for `ld64`, `-bundle_loader` and
            // `-install_name` among them, kept in order with its argument and turned into the
            // linker's words by the link line, which is where the kind of file is known.
            _ if link::APPLE_FLAGS.iter().any(|(flag, _)| *flag == arg) => {
                let takes = link::APPLE_FLAGS.iter().any(|(flag, takes)| *flag == arg && *takes);
                let value = if takes {
                    let next =
                        args.get(i).ok_or_else(|| err(format!("{arg} requires an argument")))?;
                    i += 1;
                    Some(next.clone())
                } else {
                    None
                };
                link.apple.push((arg.to_owned(), value));
                apple_only.get_or_insert_with(|| arg.to_owned());
            }
            // A run path, which clang takes on the compiler line for every target and hands to the
            // linker where it was written. `ld64`, GNU ld, lld and mold all read `-rpath <dir>`.
            "-rpath" => {
                let next = args.get(i).ok_or_else(|| err("-rpath requires an argument"))?;
                i += 1;
                inputs.push(Input::linker("-rpath"));
                inputs.push(Input::linker(next));
            }
            // The architecture on Apple's spelling, which is one more way to say what the target
            // already says. Checked against it after the loop, since `--target=` may come later,
            // and more than one is a universal binary, which is one compile per architecture.
            "-arch" => {
                let next = args.get(i).ok_or_else(|| err("-arch requires an argument"))?;
                i += 1;
                arches.push(next.clone());
            }
            "-s" => link.strip = true,
            // mingw-w64's three. `-mwindows` and `-mconsole` pick the subsystem, last one wins,
            // and `-municode` picks the start file and tells the headers through `UNICODE`, which is
            // what gcc's spec does with it. All three are taken and ignored for other targets, as gcc
            // built for mingw is the only gcc that knows them and a Makefile written for it is what
            // passes them.
            "-mwindows" => link.gui = true,
            "-mconsole" => link.gui = false,
            "-municode" => {
                link.unicode = true;
                opts.defines.push("UNICODE".to_owned());
            }
            // Into the ordered input list rather than a list of its own, because a great many of
            // the linker's options are a bracket around the files after them and an option that
            // lost its place among them says nothing. `--whole-archive` is the one that found this.
            "-Xlinker" => {
                let next = args.get(i).ok_or_else(|| err("-Xlinker requires an argument"))?;
                i += 1;
                inputs.push(Input::linker(next));
            }
            _ if arg.starts_with("-Wl,") => {
                // Commas separate arguments rather than being part of one, which is what makes
                // `-Wl,-rpath,/opt/lib` two words to the linker and one word here.
                inputs.extend(arg["-Wl,".len()..].split(',').map(Input::linker));
            }
            _ if arg.starts_with("-fuse-ld=") => {
                link.use_ld = Some(arg["-fuse-ld=".len()..].to_owned());
            }
            _ if arg.starts_with("-l") && arg.len() > 2 => {
                inputs.push(Input::library(&arg[2..]));
            }
            "-l" => {
                let next = args.get(i).ok_or_else(|| err("-l requires an argument"))?;
                i += 1;
                inputs.push(Input::library(next));
            }
            _ if arg.starts_with("-L") => {
                link.search.push(PathBuf::from(joined_or_next(arg, 2, args, &mut i)?));
            }
            _ if arg.starts_with("-B") => {
                link.prefixes.push(PathBuf::from(joined_or_next(arg, 2, args, &mut i)?));
            }
            _ if arg.starts_with("-j") => {
                jobs = Jobs::parse(&arg[2..]).map_err(err)?;
            }
            _ if arg.starts_with("--sysroot=") => {
                sysroot = Some(PathBuf::from(&arg["--sysroot=".len()..]));
            }
            // clang's flag for the GCC a link takes `crtbegin.o` and `libgcc.a` from.
            _ if arg.starts_with("--gcc-toolchain=") => {
                link.gcc_toolchain = Some(PathBuf::from(&arg["--gcc-toolchain=".len()..]));
            }
            _ if arg.starts_with("--target=") => {
                let t = &arg["--target=".len()..];
                // The same string again, as the model that has room for a libc version. A spelling
                // the three field parser took and this one does not is not an error, because the
                // one that decides what is compiled has already accepted it and the only thing
                // lost is a version nobody asked for.
                pinned = t.parse().ok();
                // The other way round is a deployment target the three field parser has no room
                // for, `aarch64-macos.13`, and the triple is the one the tuple narrows to.
                opts.target = match t.parse() {
                    Ok(triple) => triple,
                    Err(e) => {
                        pinned.and_then(Triple::from_tuple).ok_or_else(|| err(format!("{e}")))?
                    }
                };
            }
            _ if arg.starts_with("--emit=") => {
                let k = &arg["--emit=".len()..];
                opts.emit = k
                    .parse()
                    .map_err(|()| err(format!("unknown --emit kind `{k}`, see --help")))?;
            }
            // A bare `-O` is `-O1`, which is what GCC has and what a hand written makefile tends
            // to write. `-Og` is GCC's level for a build somebody is going to step through, and
            // it is `-O1` with the transformations that move code around left out; this compiler
            // has no such level yet, so it is the nearest one and `--print-pipeline` says what
            // that came to rather than the flag pretending otherwise.
            "-O" | "-Og" => {
                opts.opt_level = rucc_session::OptLevel::O1;
                ofast = false;
            }
            // The union of `-O3` and `-ffast-math`. The second half is a default rather than a
            // flag, which is why it is remembered here and applied after the loop: a later level
            // takes it back, and so does a `-fno-fast-math` written on either side of it.
            "-Ofast" => {
                opts.opt_level = rucc_session::OptLevel::O3;
                ofast = true;
            }
            _ if arg.starts_with("-O") => {
                ofast = false;
                opts.opt_level = arg[2..]
                    .parse()
                    .map_err(|()| err(format!("unknown optimization level `{arg}`")))?;
            }
            // How far a multiply and an addition may be fused into one rounding. Before the
            // optimizer's `-f` family below for the reason the ones under it are, and kept rather
            // than dropped because it is the one flag in its group this compiler could act on: it
            // rides into the IR as an attribute on each function with a body, so the day the code
            // generator forms an `fma` it already knows which functions were given permission.
            // Nothing forms one today, under any value of this and under any `-march=`.
            _ if arg.starts_with("-ffp-contract=") => {
                let how = &arg["-ffp-contract=".len()..];
                opts.fp_contract = how.parse().map_err(|()| {
                    err(format!("`{how}` is not a contraction, which is fast, on or off"))
                })?;
            }
            // How much of an expression may be computed wider than it was written. The values are
            // gcc's and so is the refusal of anything else, and none of the three changes anything
            // here: an operation is computed in the type C says it is on every target this compiler
            // has a back end for, so `__FLT_EVAL_METHOD__` is 0 and `standard` is already what
            // happens. `fast` and `16` are permission to be wider, which is a licence this takes
            // and does not use, the same way the two above are. The flag is worth taking because
            // glibc's headers and a good deal of configure output write it, and because the answer
            // it asks about is one this compiler can state rather than guess at: there is no x87
            // target here, which is the machine the whole question was invented for.
            // Whether a local and a spilled value that are never both wanted may be the same bytes
            // of the frame. gcc's three values, and two of them mean the same thing here: what rucc
            // shares is a local whose address provably never leaves the function, or one declared
            // in a block whose address stops meaning anything when the block is left, which is no
            // wider than `named_vars` and narrower still than `all`, so both of them get it. `none`
            // is the one that changes anything, and it is the flag a program that reads a local
            // through a pointer it kept past the end of the block writes.
            _ if arg.starts_with("-fstack-reuse=") => {
                let how = &arg["-fstack-reuse=".len()..];
                opts.stack_reuse = match how {
                    "all" | "named_vars" => Some(true),
                    "none" => Some(false),
                    _ => {
                        return Err(err(format!(
                            "`{how}` is not a stack reuse, which is all, named_vars or none"
                        )));
                    }
                };
            }
            _ if arg.starts_with("-fexcess-precision=") => {
                let how = &arg["-fexcess-precision=".len()..];
                if !matches!(how, "16" | "fast" | "standard") {
                    return Err(err(format!(
                        "`{how}` is not an excess precision, which is 16, fast or standard"
                    )));
                }
            }
            // Which front of a path is rewritten before it reaches the output, which is how a
            // build gets the same bytes out of two different directories. The four spellings are
            // one flag each into three lists, and `-ffile-prefix-map=` is the three of them at
            // once. Only the macro list does anything today, because `__FILE__` is the only place
            // a path reaches the output: there is no DWARF and no profile data yet, so the other
            // two are recorded for the work that will read them. The argument splits at the last
            // `=` rather than the first, which is gcc's rule and is what lets a directory with an
            // `=` in its name be the old half.
            _ if arg.starts_with("-fmacro-prefix-map=") => {
                let (old, new) = rewrite(arg, "-fmacro-prefix-map=")?;
                opts.prefix_map.macros.push(old, new);
            }
            _ if arg.starts_with("-fdebug-prefix-map=") => {
                let (old, new) = rewrite(arg, "-fdebug-prefix-map=")?;
                opts.prefix_map.debug.push(old, new);
            }
            _ if arg.starts_with("-fprofile-prefix-map=") => {
                let (old, new) = rewrite(arg, "-fprofile-prefix-map=")?;
                opts.prefix_map.profile.push(old, new);
            }
            _ if arg.starts_with("-ffile-prefix-map=") => {
                let (old, new) = rewrite(arg, "-ffile-prefix-map=")?;
                opts.prefix_map.macros.push(old, new);
                opts.prefix_map.debug.push(old, new);
                opts.prefix_map.profile.push(old, new);
            }
            // A whole optimization rather than a flag, and the family is taken rather than
            // refused because of what ignoring it does. The link does not read the module an
            // object keeps yet (see `crate::lto`), so a build that asks for it gets a program
            // that is correct and slower than it could have been,
            // which is what section 4.1 means by a hint about speed and what every compilation at
            // `-O0` already is. The objects settle the rest of the argument: gcc's `-flto` object
            // holds the bytecode and no machine code at all, and every object here holds the code,
            // which is exactly what `-ffat-lto-objects` asks gcc for. So a build passing `-flto`
            // to this compiler gets objects that are more usable than the ones it asked for rather
            // than different ones. Every value is still checked against gcc's, because somebody
            // who wrote `-flto=thin` meant clang and had better hear about it here.
            "-flto" => opts.lto.requested = true,
            "-fno-lto" => opts.lto.requested = false,
            _ if arg.starts_with("-flto=") => {
                let how = &arg["-flto=".len()..];
                opts.lto.jobs = how.parse().map_err(|()| {
                    err(format!(
                        "`{how}` is not a number of link time jobs, which is auto, jobserver or a \
                         count above zero"
                    ))
                })?;
                opts.lto.requested = true;
            }
            _ if arg.starts_with("-flto-partition=") => {
                let how = &arg["-flto-partition=".len()..];
                opts.lto.partition = how.parse().map_err(|()| {
                    err(format!(
                        "`{how}` is not a partitioning model, which is balanced, 1to1, one, max \
                         or none"
                    ))
                })?;
            }
            _ if arg.starts_with("-flto-compression-level=") => {
                let how = &arg["-flto-compression-level=".len()..];
                let level =
                    how.parse::<u8>().ok().filter(|level| *level <= 19).ok_or_else(|| {
                        err(format!("`{how}` is not a compression level, 0 to 19"))
                    })?;
                opts.lto.compression = Some(level);
            }
            // Whether the object keeps its machine code as well as the bytecode. It always does
            // here, so the first of these describes what happens and the second asks for an object
            // with less in it, which is a smaller file and not a different program, so both are
            // taken.
            "-ffat-lto-objects" | "-fno-fat-lto-objects" => {}
            // Whether the linker is handed a plugin that does the link time work. The design in
            // `spec/09-optimizer.md` has this driver doing that work itself and never loading a
            // plugin into anybody, so neither answer is a question it has to hold.
            "-fuse-linker-plugin" | "-fno-use-linker-plugin" => {}
            // Reading a profile back. Taken for the reason the family above it is: nothing here
            // reads one, so a build that asks gets the program it would have got anyway, and gcc
            // itself produces a byte for byte identical object from `-fprofile-use` when there are
            // no counts beside the file. The path is recorded for the pass that will read it. The
            // warning gcc prints when it looked and found nothing is deliberately not copied,
            // because nothing here looks, and a warning about a file that was never opened would
            // fire on the builds that have a perfectly good profile as well as on the ones that
            // do not.
            "-fprofile-use" => opts.profile_data.requested = true,
            "-fno-profile-use" => opts.profile_data.requested = false,
            _ if arg.starts_with("-fprofile-use=") => {
                opts.profile_data.path = Some(arg["-fprofile-use=".len()..].to_string());
                opts.profile_data.requested = true;
            }
            _ if arg.starts_with("-fprofile-dir=") => {
                opts.profile_data.dir = Some(arg["-fprofile-dir=".len()..].to_string());
            }
            "-fprofile-abs-path" => opts.profile_data.absolute = true,
            "-fno-profile-abs-path" => opts.profile_data.absolute = false,
            "-fprofile-correction" => opts.profile_data.correction = true,
            "-fno-profile-correction" => opts.profile_data.correction = false,
            "-fprofile-partial-training" => opts.profile_data.partial_training = true,
            "-fno-profile-partial-training" => opts.profile_data.partial_training = false,
            // Arc counters in every function and a record that hands them to `__gcov_init`, which
            // is what the kernel's `GCOV_PROFILE` builds with. `-lgcov` goes on the link, as gcc
            // puts it there. See `rucc_opt::coverage`.
            "-fprofile-arcs" => {
                opts.profile_data.arcs = true;
                link.gcov = true;
            }
            "-fno-profile-arcs" => {
                opts.profile_data.arcs = false;
                link.gcov = false;
            }
            // The graph the counters are on, written to a `.gcno` beside the object for gcov to
            // read the `.gcda` against. `--coverage` is both of these, and `-lgcov` on the link.
            "-ftest-coverage" => opts.profile_data.notes = true,
            "-fno-test-coverage" => opts.profile_data.notes = false,
            "--coverage" => {
                opts.profile_data.arcs = true;
                opts.profile_data.notes = true;
                link.gcov = true;
            }
            // The rest of the writing half, which is refused rather than taken and is the same line
            // `-gsplit-dwarf` falls on the far side of. Ignoring these means a file a build declared
            // as an output never appears: `-fprofile-generate` adds value counters to the arcs and
            // the other two count conditions and paths, and a two stage build that got none of
            // them would go on to optimize against counts that are not there and report coverage of
            // nothing, with nothing along the way saying so.
            "-fcondition-coverage" | "-fpath-coverage" | "-fprofile-generate" => {
                return Err(err(format!(
                    "{arg}: this compiler does not instrument for profiling, and a build that \
                     expects the counts a run of the instrumented program writes would optimize \
                     against nothing on its second pass, see spec/04-driver-and-cli.md"
                )));
            }
            _ if arg.starts_with("-fprofile-generate=") => {
                return Err(err(format!(
                    "{arg}: this compiler does not instrument for profiling, and a build that \
                     expects the counts a run of the instrumented program writes would optimize \
                     against nothing on its second pass, see spec/04-driver-and-cli.md"
                )));
            }
            // The rest of the family describes instrumentation that is refused above, so what is
            // left to do with them is check them and drop them. They are checked because a
            // misspelling in a distribution's flags is worth finding here rather than on the day
            // the instrumentation lands, and dropped because there is nothing for an answer about
            // how a counter is written to be an answer about.
            _ if arg.starts_with("-fprofile-update=") => {
                let how = &arg["-fprofile-update=".len()..];
                if !matches!(how, "single" | "atomic" | "prefer-atomic") {
                    return Err(err(format!(
                        "`{how}` is not a profile update method, which is single, atomic or \
                         prefer-atomic"
                    )));
                }
            }
            _ if arg.starts_with("-fprofile-reproducible=") => {
                let how = &arg["-fprofile-reproducible=".len()..];
                if !matches!(how, "serial" | "parallel-runs" | "multithreaded") {
                    return Err(err(format!(
                        "`{how}` is not a profile reproducibility method, which is serial, \
                         parallel-runs or multithreaded"
                    )));
                }
            }
            "-fprofile-values" | "-fno-profile-values" | "-fprofile-info-section" => {}
            "-fno-profile-generate" => {}
            _ if arg.starts_with("-fprofile-filter-files=")
                || arg.starts_with("-fprofile-exclude-files=")
                || arg.starts_with("-fprofile-note=") => {}
            // What every name gets when nothing in the source said, which the attribute in the
            // source overrides rather than the other way round. Before the optimizer's `-f`
            // family below for the reason the tier below it is.
            _ if arg.starts_with("-fvisibility=") => {
                let seen = &arg["-fvisibility=".len()..];
                opts.visibility = seen.parse().map_err(|()| {
                    err(format!(
                        "`{seen}` is not a visibility, which is default, hidden, internal or \
                         protected"
                    ))
                })?;
            }
            // Which edges of a control flow transfer are checked. Before the optimizer's `-f`
            // family below for the reason the two above it are, and last of the three so that the
            // bare spelling and the negative one are matched exactly rather than by this.
            _ if arg.starts_with("-fcf-protection=") => {
                let edges = &arg["-fcf-protection=".len()..];
                opts.control = edges.parse().map_err(|()| {
                    err(format!(
                        "`{edges}` is not a control flow protection, which is full, branch, \
                         return, none or check"
                    ))
                })?;
            }
            // How much room every function opens with for something to be written over later.
            // Before the optimizer's `-f` family below for the reason the ones above it are.
            _ if arg.starts_with("-fpatchable-function-entry=") => {
                let room = &arg["-fpatchable-function-entry=".len()..];
                opts.patchable = room.parse().map_err(|()| {
                    err(format!(
                        "`{room}` is not an amount of room to reserve, which is a number of bytes                          and then, after a comma, how many of them go in front of the function's                          own label"
                    ))
                })?;
            }
            // The memory safety monitor, from section 15.4 of
            // `spec/safe-memory/15-integration.md`. Before the optimizer's `-f` family below,
            // because a pass that took the name `safety=detect` would otherwise be handed the
            // flag, and the tier is not a pass.
            _ if arg.starts_with("-fsafety=") => {
                let tier = &arg["-fsafety=".len()..];
                opts.safety = tier.parse().map_err(|()| {
                    err(format!(
                        "`{tier}` is not a safety tier, which is off, detect, enforce or kernel"
                    ))
                })?;
            }
            // Whether padding participates, from section 9.3 of document 09. Spelled out rather
            // than folded into the tier because it is a departure somebody who has read that
            // section makes, and the two defaults it describes are a property of what is being
            // built rather than of how much checking is wanted.
            _ if arg.starts_with("-fsafety-init=") => {
                let mode = &arg["-fsafety-init=".len()..];
                opts.padding = mode.parse().map_err(|()| {
                    err(format!("`{mode}` is not a padding mode, which is padding or nopadding"))
                })?;
            }
            // Row S4, from section 9.4 of document 09. A bare flag with no value, because the
            // strict form of that section needs a member id the front end does not name yet and
            // accepting the spelling for it would be accepting a promise this build cannot keep.
            // Before `-fno-` is looked at below, for the reason the tier is.
            "-fsafety-subobject" => opts.subobject = rucc_session::Subobject::Members,
            "-fno-safety-subobject" => opts.subobject = rucc_session::Subobject::Off,
            _ if arg.starts_with("-fsafety-subobject=") => {
                let form = &arg["-fsafety-subobject=".len()..];
                return Err(err(format!(
                    "`{form}` is not a form of -fsafety-subobject. The flag takes no value, and \
                     the strict form of section 9.4 is tamnd/rucc#967"
                )));
            }
            // Row Y8, from section 9.6 of document 09. A bare flag with no value, for the reason
            // the one above has none: there is one form of this check and a spelling that suggested
            // otherwise would be promising something. Before `-fno-` is looked at below, the same
            // way.
            "-fsafety-restrict" => opts.promise = rucc_session::Promise::Blocks,
            "-fno-safety-restrict" => opts.promise = rucc_session::Promise::Off,
            _ if arg.starts_with("-fsafety-restrict=") => {
                let form = &arg["-fsafety-restrict=".len()..];
                return Err(err(format!(
                    "`{form}` is not a form of -fsafety-restrict. The flag takes no value."
                )));
            }
            // Row T9, from section 8.7 of document 08. A bare flag for the reason the one above
            // is: there is one sweep, and it runs at exit.
            "-fsafety-leaks" => opts.leaks = rucc_session::Leaks::Exit,
            "-fno-safety-leaks" => opts.leaks = rucc_session::Leaks::Off,
            _ if arg.starts_with("-fsafety-leaks=") => {
                let form = &arg["-fsafety-leaks=".len()..];
                return Err(err(format!(
                    "`{form}` is not a form of -fsafety-leaks. The flag takes no value."
                )));
            }
            // Section 9.5's races, which take a value because the section gives them three modes
            // and the difference between two of them is which classes get reported rather than how
            // much is recorded. `-fno-` is the same as `=off` and is spelled out here for the same
            // reason the two above spell theirs out.
            _ if arg.starts_with("-fsafety-races=") => {
                let mode = &arg["-fsafety-races=".len()..];
                opts.races = mode.parse().map_err(|()| {
                    err(format!("`{mode}` is not a race mode, which is off, metadata or pointer"))
                })?;
            }
            "-fno-safety-races" => opts.races = rucc_session::Races::Off,
            // The sanitizers of document 12, which are checks at run time rather than a way of
            // generating the same program. Each name is held to gcc 16's list, and what is still
            // asked for by the end of the line is answered after the loop, so that a command line
            // which turns one on and then off again is a command line that asked for nothing.
            //
            // Before the optimizer's `-f` family below, for the reason the tier above it is.
            _ if arg.starts_with("-fsanitize=") => {
                for one in arg["-fsanitize=".len()..].split(',') {
                    if one == "all" {
                        // gcc takes `all` only in the negative, because turning every check on at
                        // once includes checks that contradict each other.
                        return Err(err(
                            "`-fsanitize=all` is not a gcc option, only `-fno-sanitize=all` is",
                        ));
                    }
                    if !SANITIZERS.contains(&one) {
                        return Err(err(format!(
                            "`{one}` is not a sanitizer, see spec/04-driver-and-cli.md section 4.7"
                        )));
                    }
                    if !sanitizers.contains(&one) {
                        sanitizers.push(one);
                    }
                }
            }
            _ if arg.starts_with("-fno-sanitize=") => {
                for one in arg["-fno-sanitize=".len()..].split(',') {
                    if one == "all" {
                        sanitizers.clear();
                        continue;
                    }
                    if !SANITIZERS.contains(&one) {
                        return Err(err(format!(
                            "`{one}` is not a sanitizer, see spec/04-driver-and-cli.md section 4.7"
                        )));
                    }
                    sanitizers.retain(|asked| *asked != one);
                }
            }
            // What a check does when it fires, and where the records about the checked objects go.
            // Each of them is an answer about the sanitizers refused after the loop, so there is
            // nothing left for them to change here. The names are still held to the list, because
            // a misspelling in a build's flags is worth finding when the compiler reads it.
            _ if arg.starts_with("-fsanitize-recover=")
                || arg.starts_with("-fno-sanitize-recover=")
                || arg.starts_with("-fsanitize-trap=")
                || arg.starts_with("-fno-sanitize-trap=") =>
            {
                // The guard above matched on a spelling that has an `=` in it, so the tail is
                // whatever follows the first one.
                let how = arg.split_once('=').map_or("", |(_, rest)| rest);
                for one in how.split(',') {
                    if one != "all" && !SANITIZERS.contains(&one) {
                        return Err(err(format!(
                            "`{one}` is not a sanitizer, see spec/04-driver-and-cli.md section 4.7"
                        )));
                    }
                }
            }
            "-fsanitize-undefined-trap-on-error"
            | "-fsanitize-address-use-after-scope"
            | "-fno-sanitize-address-use-after-scope" => {}
            _ if arg.starts_with("-fsanitize-sections=") => {}
            // Counting which edges a run reached, which is how a fuzzer knows an input was worth
            // keeping. Refused rather than dropped, because a fuzzer whose calls into
            // `__sanitizer_cov_*` were never generated runs blind and reports coverage of nothing,
            // and there is no point in the campaign where that announces itself.
            _ if arg.starts_with("-fsanitize-coverage=") => {
                let how = &arg["-fsanitize-coverage=".len()..];
                for one in how.split(',') {
                    if !matches!(one, "trace-pc" | "trace-cmp") {
                        return Err(err(format!(
                            "`{one}` is not a coverage instrumentation, which is trace-pc or \
                             trace-cmp"
                        )));
                    }
                }
                return Err(err(format!(
                    "{arg}: this compiler generates no coverage callbacks, and a fuzzer built \
                     with it would run without any feedback at all, see \
                     spec/04-driver-and-cli.md section 4.7"
                )));
            }
            // The optimizer's own flags, from section 9.10 of `spec/09-optimizer.md`. These come
            // after every `-f` the rest of the compiler answers to, so a pass can never take a
            // name that already means something else on the command line.
            _ if arg.starts_with("-fpass-fuel=") => {
                let (name, count) = arg["-fpass-fuel=".len()..]
                    .split_once('=')
                    .ok_or_else(|| err("-fpass-fuel= is spelled <pass>=<count>"))?;
                if rucc_opt::pass::find(name).is_none() {
                    return Err(err(format!(
                        "`{name}` is not a pass this compiler has, see --print-pipeline"
                    )));
                }
                let count: u32 = count
                    .parse()
                    .map_err(|_| err(format!("`{count}` is not a number of transformations")))?;
                opts.pass_fuel.push((name.to_owned(), count));
            }
            _ if arg.starts_with("-fpass-fuel-global=") => {
                let count = &arg["-fpass-fuel-global=".len()..];
                let count: u32 = count
                    .parse()
                    .map_err(|_| err(format!("`{count}` is not a number of transformations")))?;
                opts.pass_fuel_global = Some(count);
            }
            _ if arg.starts_with("-frucc-trace=") => {
                let path = &arg["-frucc-trace=".len()..];
                if path.is_empty() {
                    return Err(err("-frucc-trace= needs a file to write to"));
                }
                opts.trace = Some(path.to_owned());
            }
            // Everything from `-fopt-info` to the end of the argument, which is optional
            // keywords joined by hyphens and an optional `=<file>`. Checked here rather than
            // where the remarks are printed, because by then the compilation somebody wanted
            // to hear about is over.
            _ if arg == "-fopt-info"
                || arg.starts_with("-fopt-info=")
                || arg.starts_with("-fopt-info-") =>
            {
                let rest = &arg["-fopt-info".len()..];
                let (kinds, file) = match rest.split_once('=') {
                    Some((kinds, file)) => (kinds, Some(file)),
                    None => (rest, None),
                };
                let kinds = kinds.strip_prefix('-').unwrap_or(kinds);
                rucc_opt::Wants::none().add(kinds).map_err(err)?;
                opts.opt_info.push(kinds.to_owned());
                if let Some(file) = file {
                    if file.is_empty() {
                        return Err(err("-fopt-info= was given no file to write to"));
                    }
                    opts.opt_info_file = Some(file.to_owned());
                }
            }
            _ if arg.starts_with("-fdump-ir=") => {
                // Checked here rather than where the dumps are taken, because the compilation
                // that would have been dumped is over by then.
                let spec = &arg["-fdump-ir=".len()..];
                rucc_opt::Dumps::default().add(spec).map_err(err)?;
                opts.dump_ir.push(spec.to_owned());
            }
            // Before the bare `-f<pass>` below, because a pass called `enable-something` would
            // otherwise take the flag away from the gate. Checked here rather than where the
            // pipeline reads it, for the reason that applies to all of these: a misspelled pass
            // name that quietly gated nothing looks exactly like a pass that is not the guilty
            // one, and a bisection would carry on past the thing it was looking for.
            _ if arg.starts_with("-fdisable-") || arg.starts_with("-fenable-") => {
                let on = arg.starts_with("-fenable-");
                let spec = &arg[if on { "-fenable-".len() } else { "-fdisable-".len() }..];
                rucc_opt::Gates::default().add(on, spec).map_err(err)?;
                opts.pass_gates.push((on, spec.to_owned()));
            }
            // gcc's spelling for a pass this compiler has under a shorter name. It goes above the
            // two arms below rather than into the pile of gcc pass names further down, because the
            // pass is here: dropping the flag would leave a build that asked for unrolling without
            // it, and refusing it stops the build outright, which is what libtommath's makefile
            // ran into. `-funroll-all-loops` is deliberately not in here: gcc's is the one that
            // unrolls without a trip count, which is a different and usually worse thing.
            "-funroll-loops" => opts.passes.push(("unroll".to_owned(), true)),
            "-fno-unroll-loops" => opts.passes.push(("unroll".to_owned(), false)),
            // Here rather than through the two arms below, because what this names is not a
            // `rucc_opt::Pass`. Section 34.6's propagation is a module at a time and everything in
            // the pass list is one function at a time. `-fipa-cp-clone` is deliberately not here:
            // gcc turns that one on at `-O3` and it is in the list of what M4 does not build.
            "-fipa-cp" => opts.passes.push((rucc_opt::ipcp::NAME.to_owned(), true)),
            "-fno-ipa-cp" => opts.passes.push((rucc_opt::ipcp::NAME.to_owned(), false)),
            // The other half of the same section, here for the same reason, and `-fipa-sra` in gcc
            // is the aggregate splitting as well as the parameter removal. Asking for it gets the
            // half that is built.
            "-fipa-sra" => opts.passes.push((rucc_opt::ipasra::NAME.to_owned(), true)),
            "-fno-ipa-sra" => opts.passes.push((rucc_opt::ipasra::NAME.to_owned(), false)),
            // Which functions never come back, which gcc keeps in its pure and const discovery.
            "-fipa-pure-const" => opts.passes.push((rucc_opt::noreturn::NAME.to_owned(), true)),
            "-fno-ipa-pure-const" => opts.passes.push((rucc_opt::noreturn::NAME.to_owned(), false)),
            // The range every caller passes, which is a module at a time for the same reason.
            "-fipa-vrp" => opts.passes.push((rucc_opt::ipvrp::NAME.to_owned(), true)),
            "-fno-ipa-vrp" => opts.passes.push((rucc_opt::ipvrp::NAME.to_owned(), false)),
            // And the printf family fold, which is a module at a time for the same reason and so is
            // not a `rucc_opt::Pass` either. gcc has no flag of its own for this one, since
            // `-fno-builtin` already turns it off along with everything else the standard names
            // mean. This spelling is for taking one thing away during a bisection without taking
            // the rest of section 20.1 away with it.
            "-flibcall" => opts.passes.push((rucc_opt::libcall::NAME.to_owned(), true)),
            "-fno-libcall" => opts.passes.push((rucc_opt::libcall::NAME.to_owned(), false)),
            _ if arg.strip_prefix("-fno-").is_some_and(|n| rucc_opt::pass::find(n).is_some()) => {
                opts.passes.push((arg["-fno-".len()..].to_owned(), false));
            }
            _ if arg.strip_prefix("-f").is_some_and(|n| rucc_opt::pass::find(n).is_some()) => {
                opts.passes.push((arg["-f".len()..].to_owned(), true));
            }
            // The flags that name a pass of gcc's own. They arrive from the torture suite, where a
            // program reduced from a miscompilation usually names the pass that miscompiled it on
            // its `dg-options` line, and they arrive from hand written build files for the same
            // reason. Section 4.1 sorts a flag by what the output would be without it, and by that
            // rule these are one pile: a flag that turns one of gcc's passes on or off is asking
            // for a compiler that does not exist here, and the program it is attached to is a
            // correctness test that passes either way. Turning on a pass we do not have costs
            // speed, turning off a pass we do not have costs nothing, and neither changes what the
            // program computes.
            //
            // rucc's own pass names are matched above this, so `-fno-dce` turns off the dce this
            // compiler has rather than landing here, and the day one of these names becomes a pass
            // here it stops being taken and dropped without anybody editing this list.
            //
            // Two of them are prefixes rather than names, which is the one place this file takes a
            // family instead of a flag. gcc files its gimple passes under `-ftree-` and its
            // interprocedural passes under `-fipa-`, both namespaces are pass selection and
            // nothing else, and there is no member of either that changes the meaning of a program
            // that was already correct. The rest are written out one at a time, because they live
            // in the flat `-f` namespace where the neighbours do change meanings.
            _ if arg.starts_with("-ftree-") || arg.starts_with("-fno-tree-") => {}
            _ if arg.starts_with("-fipa-") || arg.starts_with("-fno-ipa-") => {}
            "-fexpensive-optimizations" | "-fno-expensive-optimizations" => {}
            "-fmodulo-sched" | "-fno-modulo-sched" => {}
            "-fvect-cost-model" | "-fno-vect-cost-model" => {}
            _ if arg.starts_with("-fvect-cost-model=") || arg.starts_with("-fsimd-cost-model=") => {
            }
            "-fearly-inlining" | "-fno-early-inlining" => {}
            // The kernel's crypto directory turns both off for the table heavy ciphers, where gcc's
            // scheduler and hoisting pass blow up the register pressure.
            "-fschedule-insns" | "-fno-schedule-insns" => {}
            "-fcode-hoisting" | "-fno-code-hoisting" => {}
            // gcc's global common subexpression pass, which the kernel's BPF interpreter turns off
            // because it undoes the computed goto dispatch the interpreter is written around.
            "-fgcse" | "-fno-gcse" => {}
            // gcc's tail duplication pass, which `builtins.exp` in the torture suite turns off for
            // every program in that directory.
            "-ftracer" | "-fno-tracer" => {}
            // The scheduler's own knobs, which serpent asks for with `-fsched-pressure`. Every
            // name under `-fsched-` and `-fsched2-` tunes that pass and nothing else, so the family
            // is taken whole like the two above.
            _ if ["-fsched-", "-fno-sched-", "-fsched2-", "-fno-sched2-"]
                .iter()
                .any(|family| arg.starts_with(family)) => {}
            // The one of the family that does reach the optimizer, since the step it names is built:
            // `-fno-inline` stops a function declared `inline` from being inlined and leaves
            // `always_inline` alone, which is what it does in gcc.
            "-finline" => opts.passes.push((rucc_opt::inline::NAME.to_owned(), true)),
            "-fno-inline" => opts.passes.push((rucc_opt::inline::NAME.to_owned(), false)),
            // The called once half of the same step, on its own, which leaves the `inline` hint and
            // `always_inline` as they are. tamnd/rucc#1966.
            "-finline-functions-called-once" => {
                opts.passes.push((rucc_opt::inline::ONCE.to_owned(), true));
            }
            "-fno-inline-functions-called-once" => {
                opts.passes.push((rucc_opt::inline::ONCE.to_owned(), false));
            }
            // And the half that takes a small function nobody declared `inline`, which gcc has on
            // from `-O2`.
            "-finline-small-functions" => {
                opts.passes.push((rucc_opt::inline::SMALL.to_owned(), true));
            }
            "-fno-inline-small-functions" => {
                opts.passes.push((rucc_opt::inline::SMALL.to_owned(), false));
            }
            "-finline-functions" | "-fno-inline-functions" => {}
            "-foptimize-strlen" | "-fno-optimize-strlen" => {}
            "-fira-share-spill-slots" | "-fno-ira-share-spill-slots" => {}
            // Where a function starts, which is a thing this compiler already decides and so is a
            // request it can answer rather than one it has to drop. The bare form asks for the
            // target's default and the default here is the sixteen bytes gcc also gives, so it
            // says nothing; a number is a floor under every function that did not ask for more
            // itself; and the negative form asks for the smallest boundary the target has. gcc 16
            // rounds a number that is not a power of two up rather than refusing it, which is what
            // `=3` giving `.p2align 2` on x86-64 means, so this rounds too.
            "-falign-functions" => opts.align_functions = None,
            "-fno-align-functions" => opts.align_functions = Some(MIN_FUNC_ALIGN),
            _ if arg.starts_with("-falign-functions=") => {
                opts.align_functions = function_alignment(&arg["-falign-functions=".len()..])
                    .ok_or_else(|| {
                        err(format!("{arg}: the alignment has to be a number of bytes"))
                    })?;
            }
            // The boundary the stack pointer is kept on at a call, as a power of two, which the
            // x86-64 kernel sets to 3 because interrupt entry leaves its stack on eight bytes, and
            // the i386 kernel sets to 2. The range depends on the machine, and `-m32` only settles
            // that after the loop, so it is checked there. Only on x86, where gcc has the flag.
            _ if x86 && arg.starts_with("-mpreferred-stack-boundary=") => boundary = Some(arg),
            // The x87 stack, which is where `long double` is on x86-64 and every float is on i386.
            // The kernel turns it off along with the vector registers, with `-mno-80387` on x86-64
            // and `-msoft-float`, the older spelling, on i386. Where a `float` is returned when
            // there is no x87 is only a question once there is none, so `-mno-fp-ret-in-387` is
            // taken then.
            "-mno-80387" | "-msoft-float" if x86 => x87 = false,
            "-m80387" | "-mhard-float" if x86 => x87 = true,
            "-mno-fp-ret-in-387" if x86 => fp_ret_in_387 = false,
            "-mfp-ret-in-387" if x86 => fp_ret_in_387 = true,
            // Nothing but the general purpose registers, which on x86-64 is the vector
            // extensions and the x87 stack all turned off at once, and on AArch64 is the FP and
            // SIMD registers. A function with a `float` in it is then refused, as gcc refuses it.
            "-mgeneral-regs-only"
                if matches!(arch, rucc_target::Arch::X86_64 | rucc_target::Arch::Aarch64) =>
            {
                general_regs_only = true;
                if arch == rucc_target::Arch::X86_64 {
                    isa.read("no-mmx").map_err(|_| err("-mno-mmx is a name gcc knows"))?;
                    isa.read("no-sse").map_err(|_| err("-mno-sse is a name gcc knows"))?;
                    x87 = false;
                }
            }
            // Where the canary is read from. gcc's default on x86-64 is `%fs:40`, where glibc keeps
            // it, and a kernel moves it into its own per CPU block behind `%gs`, at offset 40 up to
            // 6.12 and at the symbol `__ref_stack_chk_guard` from 6.13 on. `global` is a plain
            // variable named `__stack_chk_guard`, and then the other three are not read. On i386
            // the default is `%gs:20`, and an SMP kernel reads `%fs:__stack_chk_guard`, which is
            // its per CPU copy.
            // On AArch64 the default is the plain global, and `sysreg` is a distance past what a
            // system register holds, which only `sp_el0` is ever asked for.
            "-mstack-protector-guard=tls" if x86 => {
                guard_global = false;
            }
            "-mstack-protector-guard=global" if x86 || aarch64 => {
                guard_global = true;
                guard_task = false;
            }
            "-mstack-protector-guard=sysreg" if aarch64 => {
                guard_global = false;
                guard_task = true;
            }
            _ if aarch64 && arg.starts_with("-mstack-protector-guard-reg=") => {
                if &arg["-mstack-protector-guard-reg=".len()..] != "sp_el0" {
                    return Err(err(format!("{arg}: the register is sp_el0")));
                }
                guard_sp_el0 = true;
            }
            _ if x86 && arg.starts_with("-mstack-protector-guard-reg=") => {
                guard_reg = Some(match &arg["-mstack-protector-guard-reg=".len()..] {
                    "fs" => rucc_target::Segment::Fs,
                    "gs" => rucc_target::Segment::Gs,
                    _ => return Err(err(format!("{arg}: the register is fs or gs"))),
                });
            }
            _ if (x86 || aarch64) && arg.starts_with("-mstack-protector-guard-offset=") => {
                let text = &arg["-mstack-protector-guard-offset=".len()..];
                // gcc reads it as C reads a number, so `0x28` is 40 too.
                let (sign, digits) = text.strip_prefix('-').map_or((1, text), |rest| (-1, rest));
                let number = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
                    Some(hex) => i64::from_str_radix(hex, 16),
                    None => digits.parse::<i64>(),
                };
                let offset = number.ok().and_then(|n| i32::try_from(sign * n).ok());
                guard_offset = Some(offset.ok_or_else(|| {
                    err(format!("{arg}: the offset is a number that fits in 32 bits"))
                })?);
            }
            _ if x86 && arg.starts_with("-mstack-protector-guard-symbol=") => {
                let name = &arg["-mstack-protector-guard-symbol=".len()..];
                if name.is_empty() {
                    return Err(err(format!("{arg}: the symbol has a name")));
                }
                guard_symbol = Some(name);
            }
            // Every one of gcc's choices, on the two back ends that clear registers. The four
            // without `-gpr` in them clear the vector registers and the x87 stack as well.
            _ if arg.starts_with("-fzero-call-used-regs=") => {
                let choice = &arg["-fzero-call-used-regs=".len()..];
                opts.zero_regs = match choice {
                    "skip" => None,
                    "used-gpr" => Some((false, false, false)),
                    "used-gpr-arg" => Some((false, true, false)),
                    "all-gpr" => Some((true, false, false)),
                    "all-gpr-arg" => Some((true, true, false)),
                    "used" => Some((false, false, true)),
                    "used-arg" => Some((false, true, true)),
                    "all" => Some((true, false, true)),
                    "all-arg" => Some((true, true, true)),
                    _ => {
                        return Err(err(format!(
                            "{arg}: the choices are skip, used-gpr, used-gpr-arg, all-gpr, \
                             all-gpr-arg, used, used-arg, all and all-arg"
                        )));
                    }
                };
                let here = matches!(
                    arch,
                    rucc_target::Arch::X86_64 | rucc_target::Arch::X86 | rucc_target::Arch::Aarch64
                );
                if opts.zero_regs.is_some() && !here {
                    return Err(err(format!(
                        "{arg}: registers are only cleared on return on x86-64, i386 and AArch64"
                    )));
                }
            }
            // A floor under every function that `-falign-functions` cannot lower, which is gcc's
            // difference between the two: the kernel passes this one because ftrace and the call
            // padding it writes need every function on the boundary, the cold ones included. It is
            // put together with `-falign-functions` after the loop, since either may come last.
            // It came in gcc 14, and the kernel's CC_HAS_MIN_FUNCTION_ALIGNMENT asks.
            _ if arg.starts_with("-fmin-function-alignment=") => {
                newer.push((14, format!("unknown option `{arg}`")));
                let text = &arg["-fmin-function-alignment=".len()..];
                let bytes =
                    function_alignment(text).filter(|_| !text.contains(':')).ok_or_else(|| {
                        err(format!("{arg}: the alignment has to be a number of bytes"))
                    })?;
                min_function_align = bytes;
            }
            // `wchar_t` as a 16 bit unsigned type, which is what the kernel's EFI stub and its
            // UCS-2 strings want and what Windows has anyway. It changes what `L""` holds, what
            // `__WCHAR_TYPE__` says and so the ABI of any function that takes one, which is why
            // it is a flag the whole program has to agree on, as it is in gcc.
            "-fshort-wchar" => opts.short_wchar = true,
            "-fno-short-wchar" => opts.short_wchar = false,
            "-fconserve-stack" => opts.conserve_stack = true,
            "-fno-conserve-stack" => opts.conserve_stack = false,
            // The speculation hardening the kernel builds with when its mitigations are
            // configured, x86-64 only as in gcc. Each is last one wins. See `rucc_codegen::thunks`,
            // tamnd/rucc#2280 and tamnd/rucc#2326.
            _ if arch == rucc_target::Arch::X86_64 && arg.starts_with("-mindirect-branch=") => {
                opts.speculation.indirect = thunked(arg, "-mindirect-branch=")?;
            }
            _ if arch == rucc_target::Arch::X86_64 && arg.starts_with("-mfunction-return=") => {
                opts.speculation.returns = thunked(arg, "-mfunction-return=")?;
            }
            "-mindirect-branch-cs-prefix" if arch == rucc_target::Arch::X86_64 => {
                opts.speculation.padded = true;
            }
            "-mno-indirect-branch-cs-prefix" if arch == rucc_target::Arch::X86_64 => {
                opts.speculation.padded = false;
            }
            // Which functions sign their return address and whether indirect branches land on
            // `bti`. AArch64 only, as in gcc, and the older flag says the first half alone.
            _ if arch == rucc_target::Arch::Aarch64 && arg.starts_with("-mbranch-protection=") => {
                let value = &arg["-mbranch-protection=".len()..];
                opts.branch_protection =
                    rucc_target::BranchProtection::parse(value).map_err(err)?;
            }
            _ if arch == rucc_target::Arch::Aarch64
                && arg.starts_with("-msign-return-address=") =>
            {
                let value = &arg["-msign-return-address=".len()..];
                opts.branch_protection.sign = rucc_target::SignReturn::parse(value).map_err(err)?;
            }
            // Only a function that says `cf_check` opens with a landing pad, which is how a
            // program that knows every function it takes the address of keeps the rest from being
            // somewhere an indirect branch may land. x86 only, as in gcc.
            "-mmanual-endbr" if x86 => opts.manual_endbr = true,
            "-mno-manual-endbr" if x86 => opts.manual_endbr = false,
            _ if arch == rucc_target::Arch::X86_64 && arg.starts_with("-mharden-sls=") => {
                let (after_return, after_jump) = match &arg["-mharden-sls=".len()..] {
                    "none" => (false, false),
                    "return" => (true, false),
                    "indirect-jmp" => (false, true),
                    "all" => (true, true),
                    other => {
                        return Err(err(format!(
                            "`{other}` is not a place to stop straight line speculation, which \
                             is none, return, indirect-jmp or all"
                        )));
                    }
                };
                opts.speculation.after_return = after_return;
                opts.speculation.after_jump = after_jump;
            }
            // How a thread-local variable in one of the two dynamic models is reached: through
            // `__tls_get_addr`, or through a TLS descriptor. Fedora passes `gnu2` on each x86-64
            // compile. This compiler writes the initial exec sequence for each thread-local
            // variable (`thread_address` in the code generator), and the dialect does not change
            // that sequence, as in gcc with `-ftls-model=initial-exec`. So the value is checked and
            // there is nothing more to do until the dynamic models come (#1104).
            _ if arg.starts_with("-mtls-dialect=")
                && (x86 || arch == rucc_target::Arch::Aarch64) =>
            {
                let value = &arg["-mtls-dialect=".len()..];
                let choices: &[&str] = if x86 { &["gnu", "gnu2"] } else { &["desc", "trad"] };
                if !choices.contains(&value) {
                    return Err(err(format!(
                        "bad value `{value}` for `-mtls-dialect=`, which is {} on this target",
                        choices.join(" or ")
                    )));
                }
            }
            // Whether a `switch` may become a table, which the kernel turns off beside the thunks
            // because a jump through a table is an indirect branch that goes through no thunk.
            "-fjump-tables" => opts.jump_tables = true,
            "-fno-jump-tables" => opts.jump_tables = false,
            // What a local with no initializer holds, which the kernel asks for under
            // `CONFIG_INIT_STACK_ALL_ZERO` and `CONFIG_INIT_STACK_ALL_PATTERN`.
            _ if arg.starts_with("-ftrivial-auto-var-init=") => {
                opts.auto_var_init = match &arg["-ftrivial-auto-var-init=".len()..] {
                    "uninitialized" => None,
                    "zero" => Some(0),
                    "pattern" => Some(0xfe),
                    _ => {
                        return Err(err(format!(
                            "{arg}: the choices are uninitialized, zero and pattern"
                        )));
                    }
                };
            }
            // The head of every hot loop, which is padded when this is asked for so that a loop that
            // fits in a 64 byte line does not cross one. Both directions of the plain form are
            // answered. A number is taken and says nothing, because the boundary here is the
            // line's and a build that names another is asking for speed rather than for a
            // different program.
            "-falign-loops" => opts.align_loops = Some(true),
            "-fno-align-loops" => opts.align_loops = Some(false),
            // The other two of the family, which are about padding in front of any label and in
            // front of a label only a jump reaches. This compiler writes neither, and what they
            // ask for is speed: a label on a boundary computes what a label off one computes. So
            // they are taken and dropped for the reason `-march=` is, and the numbered form of
            // the loop flag with them.
            _ if arg.starts_with("-falign-labels")
                || arg.starts_with("-falign-loops=")
                || arg.starts_with("-falign-jumps")
                || arg.starts_with("-fno-align-labels")
                || arg.starts_with("-fno-align-jumps") => {}
            // The charset flags are not in that pile, because an encoding is a statement about
            // what the bytes of the source mean rather than about how fast the output is. The
            // preprocessor reads UTF-8 and has no converter, so the one name that describes what
            // already happens is taken and every other name is refused. Spelled without regard to
            // case and with both of the spellings iconv answers to, since a build writes whichever
            // one its author typed.
            _ if arg.starts_with("-finput-charset=") => {
                let name = &arg["-finput-charset=".len()..];
                if !name.eq_ignore_ascii_case("utf-8") && !name.eq_ignore_ascii_case("utf8") {
                    return Err(err(format!(
                        "-finput-charset={name}: the preprocessor reads UTF-8 and has no \
                         converter, so a file in another encoding would be read as though it were \
                         UTF-8 rather than converted",
                    )));
                }
            }
            // What C has of exceptions, which is a `cleanup` handler an unwind has to run and the
            // `__EXCEPTIONS` that tells a header so. The walk is what turns down the handler it has
            // no landing pad for, so a unit with none of them is taken whole.
            "-fexceptions" => exceptions = Some(true),
            "-fno-exceptions" => exceptions = Some(false),
            "-fnon-call-exceptions" => opts.non_call_exceptions = true,
            "-fno-non-call-exceptions" => opts.non_call_exceptions = false,
            // Whether an instruction that could raise one may still be deleted when nothing uses
            // what it computes. Nothing here keeps a dead one, and neither does gcc in a C unit
            // with no handler around it, so both spellings describe the code as it is.
            "-fdelete-dead-exceptions" | "-fno-delete-dead-exceptions" => {}
            "-finstrument-functions" => opts.instrument_functions = true,
            "-fno-instrument-functions" => opts.instrument_functions = false,
            // The unstable options, spelled the way rustc spells them and carrying the same
            // promise, which is none: one of these may change or go away in any release. They are
            // measurements and debugging aids rather than things a build asks for, which is why
            // none of them is in the usage text and all of them are in section 4.11 of
            // `spec/04-driver-and-cli.md`.
            "-Zverify-each" => opts.verify_each = true,
            _ if arg.starts_with("-Zrule-coverage=") => {
                let file = &arg["-Zrule-coverage=".len()..];
                if file.is_empty() {
                    return Err(err("-Zrule-coverage= needs a file to write to"));
                }
                opts.rule_coverage = Some(file.to_owned());
            }
            _ if arg.starts_with("-Zcycle-accurate-model=") => {
                let value = &arg["-Zcycle-accurate-model=".len()..];
                opts.cycle_accurate_model = match value {
                    "yes" | "1" => Some(true),
                    "no" | "0" => Some(false),
                    _ => {
                        return Err(err("-Zcycle-accurate-model= takes yes or no"));
                    }
                };
            }
            _ if arg.starts_with("-Zregalloc=") => {
                opts.backtracking = match &arg["-Zregalloc=".len()..] {
                    "backtracking" => Some(true),
                    "single" => Some(false),
                    _ => return Err(err("-Zregalloc= takes backtracking or single")),
                };
            }
            _ if arg.starts_with("-Zrewriter=") => {
                let name = &arg["-Zrewriter=".len()..];
                if rucc_opt::pipeline::Rewriter::from_name(name).is_none() {
                    return Err(err("-Zrewriter= takes default, consed, classical or egraph"));
                }
                opts.rewriter = name.to_string();
            }
            _ if arg.starts_with("-Zswitch=") => {
                let shape = &arg["-Zswitch=".len()..];
                if rucc_codegen::switch::Force::named(shape).is_none() {
                    return Err(err("-Zswitch= takes table, tree or walk"));
                }
                opts.switch_shape = Some(shape.to_owned());
            }
            _ if arg.starts_with("-Zlowering=") => {
                let file = &arg["-Zlowering=".len()..];
                if file.is_empty() {
                    return Err(err("-Zlowering= needs a file to write to"));
                }
                opts.lowering_dump = Some(file.to_owned());
            }
            _ if arg.starts_with("-Zregister-pressure=") => {
                let file = &arg["-Zregister-pressure=".len()..];
                if file.is_empty() {
                    return Err(err("-Zregister-pressure= needs a file to write to"));
                }
                opts.register_pressure = Some(file.to_owned());
            }
            _ if arg.starts_with("-Z") => {
                return Err(err(format!(
                    "`{arg}` is not an unstable option this compiler has, see \
                     spec/04-driver-and-cli.md section 4.11 for the ones it does"
                )));
            }
            // The word size, which is a statement about the target and is taken as one. It is
            // weighed after the loop, because `--target=` may come after it and the last one of
            // each is the one that counts.
            "-m64" | "-m32" | "-m16" | "-mx32" => word = Some(arg),
            // The WebAssembly set and features, which only a wasm row has. clang refuses an unknown
            // set with these words, and lists the names after them.
            _ if wasm_row && arg.starts_with("-mcpu=") => {
                let name = &arg["-mcpu=".len()..];
                wasm_cpu = rucc_target::wasm::Cpu::named(name).ok_or_else(|| {
                    err(format!(
                        "unknown target CPU '{name}', the names for wasm32 are mvp, generic, \
                         lime1 and bleeding-edge"
                    ))
                })?;
            }
            _ if wasm_row && wasm_feature(arg).is_some() => {
                let Some((feature, on)) = wasm_feature(arg) else { continue };
                wasm_flags.push((feature, on, arg));
            }
            // wasm has no processor family, and clang refuses `-march=` there. The message names the
            // flag that does choose what the unit may use.
            _ if wasm_row && arg.starts_with("-march=") => {
                return Err(err(format!(
                    "{arg}: wasm32 has no -march=, and -mcpu= chooses the features"
                )));
            }
            // A command or a reactor, which only the link reads. clang refuses it on a line that
            // does not link, and rucc takes it on any wasm line, because a build that gives it to
            // every command is still clear.
            _ if arg.starts_with("-mexec-model=") => {
                if !wasm_row {
                    return Err(err(format!("{arg}: the execution model is for wasm32 only")));
                }
                link.reactor = match &arg["-mexec-model=".len()..] {
                    "command" => false,
                    "reactor" => true,
                    other => {
                        return Err(err(format!(
                            "invalid argument '{other}' to -mexec-model=, the models are command \
                             and reactor"
                        )));
                    }
                };
            }
            // How many words of each function's arguments go in registers on 32 bit x86, which the
            // kernel builds every 32 bit unit with. See `rucc_target::TargetInfo::with_regparm`.
            _ if arg.starts_with("-mregparm=") => regparm = Some(arg),
            // One extension of the x86-64 instruction set, on or off, which is `-msse4.2` and its
            // relatives. Only the ones this compiler has the intrinsics for may be turned on for a
            // whole unit, because what turning one on does here is define the macro, and a macro
            // is a promise to a header that the names behind it exist. Turning one off is taken
            // for any name gcc knows, since nothing is promised by it. That includes the baseline,
            // which is what the kernel does to keep the vector registers out: a unit without SSE2
            // has nowhere to put a `double`, and a function that uses one is refused, the way gcc
            // refuses it.
            _ if isa_name(arg).is_some() => {
                let Some((_, feature, on)) = isa_name(arg) else { continue };
                if on && !feature.honoured() {
                    return Err(err(format!(
                        "{arg}: this compiler has no intrinsics for {} yet, so it cannot build a \
                         whole unit for it",
                        feature.name()
                    )));
                }
                isa.read(&arg["-m".len()..]).map_err(|_| err(format!("unknown option `{arg}`")))?;
                isa_flag.get_or_insert(arg);
                if on && !matches!(arg, "-msse" | "-msse2") {
                    isa_on.get_or_insert(arg);
                }
            }
            // Which processor in the family to build for. What it decides is the extensions of
            // the instruction set the unit may assume, which is the macros, on x86-64 and, for
            // the CRC32 extension alone, on AArch64; see `rucc_target::isa`. A processor it has
            // no list for is built for as the baseline, which is a program that could have been
            // faster rather than a program that is wrong, and the same goes for every other
            // target's processors. `-mtune=` says what to schedule for and changes nothing a
            // program can see.
            _ if arg.starts_with("-march=") => march = Some(&arg["-march=".len()..]),
            _ if arg.starts_with("-mtune=") || arg.starts_with("-mcpu=") => {}
            // The calling convention, which is not safe to ignore. Taken when it names the one
            // the target already uses and refused otherwise.
            _ if arg.starts_with("-mabi=") => {
                let want = &arg["-mabi=".len()..];
                let have = match opts.target.arch {
                    rucc_target::Arch::X86_64 | rucc_target::Arch::X86 => "sysv",
                    rucc_target::Arch::Aarch64 => "lp64",
                    rucc_target::Arch::Riscv64 => "lp64d",
                    // The C ABI of tool-conventions, which clang calls the MVP ABI.
                    rucc_target::Arch::Wasm32 => "mvp",
                };
                if want == "experimental-mv" && have == "mvp" {
                    return Err(err(format!(
                        "{arg}: the multivalue C ABI is not supported, and wasm32 uses the C ABI \
                         of tool-conventions, which clang calls mvp"
                    )));
                }
                if want != have {
                    return Err(err(format!(
                        "{arg}: {} uses the {have} convention and this compiler has no other",
                        opts.target
                    )));
                }
            }
            // How far apart the pieces of the program may be, and where. The small model is every
            // hosted program's default. The kernel model is the top 2 GiB of the address space,
            // which is where every x86-64 Linux kernel is linked, and a build that asks for it and
            // does not get it links and then does not run. Which machine and which link it is
            // checked against after the loop, since `--target=` may come after it.
            // tamnd/rucc#2275.
            "-mcmodel=small" => cmodel = rucc_target::CodeModel::Small,
            "-mcmodel=kernel" => cmodel = rucc_target::CodeModel::Kernel,
            "-mcmodel=tiny" => cmodel = rucc_target::CodeModel::Tiny,
            // clang's spellings of the deployment target, which it takes over a version in the
            // tuple. gcc on a Mac takes the first. A target that is not Apple ignores it, as
            // clang does, so a makefile that always passes it still builds for Linux.
            _ if arg.starts_with("-mmacosx-version-min=")
                || arg.starts_with("-mmacos-version-min=") =>
            {
                let text = &arg[arg.find('=').map_or(arg.len(), |i| i + 1)..];
                let version = rucc_tuple::Version::parse(text)
                    .ok_or_else(|| err(format!("`{text}` in `{arg}` is not a version")))?;
                min_version = Some(version);
            }
            _ if arg.starts_with("-mcmodel=") => {
                return Err(err(format!(
                    "{arg}: this compiler emits the small, kernel and tiny code models and no other, \
                     see spec/04-driver-and-cli.md section 4.3"
                )));
            }
            // GCC prints its spec strings. This compiler has none, and the questions a script
            // reads them for have their own flags.
            "-dumpspecs" => {
                return Err(err(
                    "-dumpspecs: this compiler has no spec strings to print. Use -dumpmachine, \
                     -dumpversion, -print-search-dirs or -v for what a script reads from them",
                ));
            }
            // What a build hands the assembler. gcc splits `-Wa,` at every comma and passes
            // `-Xassembler`'s word whole, and both are kept in order and read after the loop,
            // because whether `--64` is true depends on a `--target=` that may come later. See
            // `assembler_words` for which of them this compiler takes.
            _ if arg.starts_with("-Wa,") => {
                for word in arg["-Wa,".len()..].split(',') {
                    asm_words.push((word.to_owned(), arg.to_owned()));
                }
            }
            "-Xassembler" => {
                let word = args.get(i).ok_or_else(|| err("-Xassembler requires an argument"))?;
                i += 1;
                asm_words.push((word.clone(), format!("-Xassembler {word}")));
            }
            // Arguments meant for a separate preprocessor, which this compiler does not have: it is
            // inside it and does not read a command line. The `-Wp,` ones this compiler understands
            // were turned into its own flags before the loop, so one that reaches here is one it
            // does not, and it is refused rather than dropped, because a build that asked the
            // preprocessor for something and was silently not given it has been told something
            // untrue.
            _ if arg.starts_with("-Wp,") => {
                return Err(err(format!(
                    "`{arg}` is an argument for a separate preprocessor, and the preprocessor is \
                     inside this compiler rather than a program it runs"
                )));
            }
            "-Xpreprocessor" => {
                return Err(err(format!(
                    "{arg} hands an argument to a separate preprocessor, and the preprocessor is \
                     inside this compiler rather than a program it runs"
                )));
            }
            // Everything else in the `-W` family. `spec/04-driver-and-cli.md` section 4.1 has
            // this one as a rule about build systems rather than about warnings: autoconf and
            // meson find out whether a warning flag exists by passing it and looking at the exit
            // status, so the answer has to be gcc's. A name gcc knows is accepted, and one it does
            // not is refused, the way gcc refuses clang's names. `-Wno-` of a name nobody knows is
            // accepted, because gcc accepts it too, but `-Werror=` and `-Wno-error=` of one are
            // not. What they say about a warning this compiler gives is recorded by name, so
            // `-Wno-pointer-sign` quiets the warning gcc files under that name. The format checks are
            // off until `-Wformat` or `-Wall` asks for them, as in gcc. `-Wall` turns on nothing
            // else yet, which #485 is about.
            _ if arg.starts_with("-W") => {
                let name = &arg["-W".len()..];
                opts.named_warnings.flag(name);
                let named = name.strip_prefix("error=").or_else(|| name.strip_prefix("no-error="));
                if let Some(named) = named {
                    if !warnings::known(named) {
                        return Err(err(format!("`{arg}`: no option `-W{named}`")));
                    }
                    if warnings::since(named) > 0 {
                        let why = format!("`{arg}`: no option `-W{named}`");
                        newer.push((warnings::since(named), why));
                    }
                } else if !name.is_empty() && !name.starts_with("no-") {
                    if !warnings::known(name) {
                        return Err(err(format!("unknown option `{arg}`")));
                    }
                    if warnings::since(name) > 0 {
                        newer.push((warnings::since(name), format!("unknown option `{arg}`")));
                    }
                }
            }
            // Whether the object says what made it, in `.comment`, as gcc's does.
            "-fident" => opts.ident = true,
            "-fno-ident" => opts.ident = false,
            // Flags that name something this compiler does not do and would not do differently
            // if it did. The first two are about a way of ordering the compilation that has
            // been GCC's only way for twenty years. `-mthreads` is mingw's, and what it links is
            // `libmingwthrd.a`, which mingw-w64 keeps as an empty archive because its CRT does the
            // thread cleanup for every program. Section 4.1 asks for the list to be short and for
            // adding to it to be deliberate, which is why it is written out here.
            "-funit-at-a-time"
            | "-fno-unit-at-a-time"
            | "-shared-libgcc"
            | "-static-libgcc"
            | "-mthreads"
            | "-fpch-deps"
            | "-fno-pch-deps" => {}
            _ if arg.starts_with('-') && arg.len() > 1 => {
                // Silently ignoring an unknown flag is how a build ends up not doing what
                // its author asked. spec/13-gnu-compat.md section 13.4 makes this an error
                // for the flags that change code generation, and the safe default until the
                // flag table is populated is to reject everything we do not know.
                return Err(err(format!("unknown option `{arg}`")));
            }
            _ => inputs.push(Input { path: arg.to_owned(), forced, role: Role::File }),
        }
    }

    // gcc takes `-static` over `-pie`, in either order: the start file is `crt1.o` and the linker
    // is not told `-pie`, so the program is static and loaded where it was linked. Linux 5.15's
    // exec selftests link their load address checks with `-pie -static` and a 2 MiB page, and a
    // static PIE there is placed by a kernel that does not align it to that page yet.
    if link.is_static && !static_pie && link.pie == Some(true) {
        link.pie = Some(false);
    }

    // The word size against the target. On x86 the other size is the other machine, as it is for
    // gcc built for either: `-m32` on x86-64 is i686 and `-m64` on i686 is x86-64, with the same
    // operating system and runtime. `-m16` is the 32 bit machine assembled as `.code16gcc`, which
    // is how the kernel builds its real mode code. Anywhere else the other size is a target this
    // compiler does not have, which it is told rather than being given the wrong one.
    if let Some(word) = word {
        use rucc_target::Arch;
        let arch = match (word, opts.target.arch) {
            ("-m64", Arch::X86_64 | Arch::X86) => Some(Arch::X86_64),
            ("-m32" | "-m16", Arch::X86_64 | Arch::X86) => Some(Arch::X86),
            _ => None,
        };
        match arch {
            Some(arch) => {
                if arch != opts.target.arch {
                    opts.target.arch = arch;
                    // A tuple that names the other machine no longer describes this one.
                    pinned = None;
                }
                opts.sixteen = word == "-m16";
            }
            None if word == "-m16" => {
                return Err(err(format!("-m16 is for x86, and {} is not", opts.target)));
            }
            None => {
                let want = if word == "-m64" { 64 } else { 32 };
                let have = rucc_target::TargetInfo::new(opts.target).pointer_width;
                if have != want {
                    return Err(err(format!(
                        "{word} asks for a {want} bit target and {} is {have} bit, use \
                         --target= to name the one you mean",
                        opts.target
                    )));
                }
            }
        }
    }

    // The stack boundary against the machine. gcc's range is 3 to 12 on x86-64, with the vector
    // registers or without them, and 2 to 12 on i386, where the psABI only promises four bytes.
    // The kernel's display code is built with `-msse` and a boundary of 3, and a frame with a
    // sixteen byte vector in it is then aligned by the prologue, the same as a local that asks for
    // more than the boundary.
    if let Some(arg) = boundary {
        let least = if opts.target.arch == rucc_target::Arch::X86 { 2 } else { 3 };
        let text = &arg["-mpreferred-stack-boundary=".len()..];
        let power = text.parse::<u32>().ok().filter(|power| (least..=12).contains(power));
        let power = power.ok_or_else(|| {
            err(format!("{arg}: the boundary is a power of two between {least} and 12"))
        })?;
        opts.stack_boundary = Some(1 << power);
    }

    // The WebAssembly features, settled now that the target is. `-matomics` is refused, because
    // no wasm row has shared memory (decision D10 of the WebAssembly plan, #2863), and `-pthread`
    // is a warning for the same reason. wasm32-wasip3 has cooperative threads, and clang builds for
    // it as if `-pthread` was given, so it refuses a flag that turns off what those threads need,
    // and so does this.
    if opts.target.arch == rucc_target::Arch::Wasm32 {
        use rucc_target::wasm::{self, Feature};
        use rucc_target::{Os, Preview};
        let wasip3 = opts.target.os == Os::Wasi(Preview::P3);
        // The short name, `wasm32-wasip1`, for the messages.
        let row = opts.target.tuple();
        let last = |feature: Feature| wasm_flags.iter().rev().find(|(f, _, _)| *f == feature);
        if let Some((_, true, arg)) = last(Feature::Atomics) {
            return Err(err(format!(
                "{arg}: {row} has no shared memory, and rucc has no wasm row that has it"
            )));
        }
        if wasip3 {
            for feature in [Feature::BulkMemory, Feature::MutableGlobals, Feature::SignExt] {
                if let Some((_, false, arg)) = last(feature) {
                    return Err(err(format!("{arg}: {row} needs {feature} for its threads")));
                }
            }
        }
        let flags: Vec<(Feature, bool)> = wasm_flags.iter().map(|&(f, on, _)| (f, on)).collect();
        opts.wasm = wasm::resolve(wasm_cpu, &flags);
        if wasip3 {
            opts.wasm = opts.wasm.union(wasm::required(Preview::P3));
        }
        if threads && !wasip3 {
            notes.push(format!("-pthread has no effect on {row}: the row has one thread"));
        }
    }

    // The registers against the machine, settled now that the machine is. gcc takes 0 to 3 on 32
    // bit x86, says the flag does nothing on x86-64, and has no such flag anywhere else.
    if let Some(arg) = regparm {
        use rucc_target::Arch;
        let count = &arg["-mregparm=".len()..];
        match opts.target.arch {
            Arch::X86 => {
                let registers = count
                    .parse::<u8>()
                    .ok()
                    .filter(|&registers| registers <= 3)
                    .ok_or_else(|| err(format!("{arg} is not between 0 and 3")))?;
                if rucc_target::TargetInfo::new(opts.target).with_regparm(registers).is_none() {
                    return Err(err(format!("{arg} is not supported on {}", opts.target)));
                }
                opts.regparm = registers;
            }
            Arch::X86_64 => notes.push("-mregparm is ignored in 64 bit mode".to_owned()),
            _ => return Err(err(format!("{arg} is for x86, and {} is not", opts.target))),
        }
    }

    // The floor `-fmin-function-alignment=` put under every function, which `-falign-functions`
    // may raise and not lower. With no `-falign-functions` the target's own boundary stands unless
    // the floor is above it.
    if let Some(floor) = min_function_align {
        let have = opts.align_functions.unwrap_or(rucc_object::FUNC_ALIGN);
        if floor > have {
            opts.align_functions = Some(floor);
        }
    }

    // The fetch, before anything that resolves a compilation, because `--fetch` does not describe
    // one. It is here rather than in the loop so that `--offline` can forbid it whichever order the
    // two were written in, and it is before the refusals below so that a command line asking for a
    // sysroot is not told about a sanitizer.
    if let Some(named) = fetch {
        if fetch_msvc.is_some() {
            return Err(err(
                "--fetch and --fetch-msvc-sdk are two different commands and this command line \
                 asked for both. --fetch gets what this release pins by URL and by hash, which for \
                 a *-windows-msvc target is one Visual Studio build, and --fetch-msvc-sdk gets the \
                 build Microsoft's channel names today. Run whichever one you meant",
            ));
        }
        return fetch_action(&named, offline, accepted, &inputs);
    }
    if let Some(named) = fetch_msvc {
        return fetch_msvc_action(&named, offline, accepted, &inputs);
    }
    if accepted {
        return Err(err(
            "--accept-licence says that Microsoft's Visual Studio Build Tools licence is accepted, \
             and nothing on this command line asked for anything that licence covers. \
             --fetch <tuple> or --fetch-msvc-sdk <tuple> for a *-windows-msvc target is the \
             command it belongs to, and an ordinary compile downloads nothing with it or \
             without it",
        ));
    }

    // Last, so that it lands after every `-isystem` the command line gave. That is GCC's
    // order: a directory the user names outranks the compiler's own, and the compiler's own
    // outranks the library's. It is pushed after the loop rather than before it because
    // `SearchPath` appends within a group and the position is what the order is.
    // The same directory the headers were looked for under, because a sysroot is a statement
    // about a whole installation and not about half of one.
    // After the loop, because `-fno-sanitize=` can take back what an earlier flag asked for and a
    // command line that turns a check on and off again has asked for nothing. What is left is
    // refused rather than dropped, and it is the one place in this parser where the reason is not
    // that the output would differ. A sanitizer is a promise that the program is watched while it
    // runs, so a build that asks for one and is quietly given a program with no checks in it does
    // not get a slower program or a bigger file, it gets a test suite that passes for the wrong
    // reason. `-fsafety=` is the checking this compiler does have, and the message says so, because
    // somebody reaching for `-fsanitize=address` wants the nearest thing rather than a list of
    // options.
    if let Some(first) = sanitizers.first() {
        return Err(err(format!(
            "-fsanitize={first}: this compiler has no sanitizer instrumentation, and a build that \
             asked for one and got none would run its tests unchecked, see \
             spec/04-driver-and-cli.md section 4.7. `-fsafety=detect` is the memory checking this \
             compiler does have"
        )));
    }
    // The fast math family, replayed in order on top of what `-Ofast` implies. The startup file is
    // gcc's spec rather than the fields: it is linked when `-Ofast`, `-ffast-math` or
    // `-funsafe-math-optimizations` is still in force at the end of the line, whatever a later
    // member took back, and `-mdaz-ftz` decides it outright.
    // wasm32 starts with `-fno-math-errno`, as clang does there. wasi-libc's maths functions do
    // not set `errno`, so a promise that they do would only stop the code from being inlined.
    let mut math = Math { errno: opts.target.arch != rucc_target::Arch::Wasm32, ..Math::default() };
    let mut trapping = if ofast { math.set_fast(true) } else { true };
    let mut rounding = false;
    for flag in &math_flags {
        match *flag {
            "-ftrapping-math" => trapping = true,
            "-fno-trapping-math" => trapping = false,
            "-frounding-math" => rounding = true,
            "-fno-rounding-math" => rounding = false,
            // `-ffast-math` says the rounding mode is the default one and `-fno-fast-math` says
            // nothing about it, as in gcc's `set_fast_math_flags`.
            "-ffast-math" => {
                trapping = math.set_fast(true);
                rounding = false;
            }
            "-fno-fast-math" => trapping = math.set_fast(false),
            "-funsafe-math-optimizations" => trapping = math.set_unsafe(true),
            "-fno-unsafe-math-optimizations" => trapping = math.set_unsafe(false),
            "-fmath-errno" => math.errno = true,
            "-fno-math-errno" => math.errno = false,
            "-ffinite-math-only" => math.finite_only = true,
            "-fno-finite-math-only" => math.finite_only = false,
            "-fsigned-zeros" => math.signed_zeros = true,
            "-fno-signed-zeros" => math.signed_zeros = false,
            "-freciprocal-math" => math.reciprocal = true,
            "-fno-reciprocal-math" => math.reciprocal = false,
            "-fassociative-math" => math.associative = true,
            "-fno-associative-math" => math.associative = false,
            _ => unreachable!("{flag} is not in the family"),
        }
    }
    opts.trapping_math = trapping;
    opts.rounding_math = rounding;
    opts.math = math;
    let last = |on: &str, off: &str| {
        math_flags.iter().rev().find(|f| **f == on || **f == off).is_some_and(|f| *f == on)
    };
    link.fast_math = ofast
        || last("-ffast-math", "-fno-fast-math")
        || last("-funsafe-math-optimizations", "-fno-unsafe-math-optimizations");
    link.daz_ftz = daz_ftz;
    // The extensions, now that the target is known. On x86-64 the processor supplies whatever no
    // flag said. On AArch64 `-march=` alone says them, with its `+crc` and the rest, and nowhere
    // else is there any to have. Off x86 a flag naming one of its extensions is gcc's unknown
    // option too, so it is refused the same way it would have been had it not looked like an x86
    // flag. On i386 every one of them is already off, which is what the kernel's `-mno-sse` and
    // the rest ask for, and turning one on is refused, since there is no code here that uses it.
    // `-msse` and `-msse2` are the exception, because they ask for what the i386 backend does
    // anyway: it builds for the Pentium 4 and does `float` and `double` arithmetic in SSE2, and the
    // macros say so (see `rucc_pp::predef`). The kernel turns both back on for its floating point
    // units after `-mno-sse`, 69 of them in amdgpu's display code for i386 and `test_fpu_impl.c`.
    //
    // `-march=native` is the machine running the compiler, so it means something only when that
    // machine is the target's. Anywhere else gcc is a cross compiler, which has no way to ask the
    // processor and refuses the value, and the kernel's `CONFIG_CC_HAS_MARCH_NATIVE` is that
    // answer.
    let host = if cfg!(target_arch = "x86_64") {
        Some(rucc_target::Arch::X86_64)
    } else if cfg!(target_arch = "aarch64") {
        Some(rucc_target::Arch::Aarch64)
    } else {
        None
    };
    if march.is_some_and(|name| name.split('+').next() == Some("native"))
        && host != Some(opts.target.arch)
        && matches!(opts.target.arch, rucc_target::Arch::X86_64 | rucc_target::Arch::Aarch64)
    {
        return Err(err(
            "bad value `native` for `-march=`: the machine running the compiler is not the \
             target's, so there is no processor to ask",
        ));
    }
    match opts.target.arch {
        rucc_target::Arch::X86_64 => {
            let base = match march {
                Some("native") => native_isa(),
                Some(name) => {
                    rucc_target::Isa::level(name).unwrap_or_else(rucc_target::Isa::baseline)
                }
                None => rucc_target::Isa::baseline(),
            };
            opts.isa = isa.over(base);
        }
        rucc_target::Arch::Aarch64
        | rucc_target::Arch::Riscv64
        | rucc_target::Arch::X86
        | rucc_target::Arch::Wasm32 => {
            if opts.target.arch == rucc_target::Arch::X86 {
                if let Some(flag) = isa_on {
                    return Err(err(format!(
                        "{flag}: this compiler builds i386 code with no extensions to the \
                         instruction set, so one can only be turned off"
                    )));
                }
            } else if let Some(flag) = isa_flag {
                return Err(err(format!("unknown option `{flag}`")));
            }
            opts.isa = match (opts.target.arch, march) {
                (rucc_target::Arch::Aarch64, Some(name)) => match name.strip_prefix("native") {
                    Some(modifiers) => native_aarch64().aarch64_modifiers(modifiers),
                    None => rucc_target::Isa::aarch64_march(name),
                },
                _ => rucc_target::Isa::NONE,
            };
            // The processors gcc gives no `cmov`, which came with the Pentium Pro. A kernel built
            // for one of them is one that machine has to be able to run.
            if opts.target.arch == rucc_target::Arch::X86 {
                opts.cmov = !march.is_some_and(|name| {
                    matches!(
                        name,
                        "i386"
                            | "i486"
                            | "i586"
                            | "pentium"
                            | "pentium-mmx"
                            | "lakemont"
                            | "k6"
                            | "k6-2"
                            | "k6-3"
                            | "winchip-c6"
                            | "winchip2"
                            | "c3"
                    )
                });
            }
        }
    }
    // The register files, now that the extensions are known. On x86-64 the vector registers are
    // there for as long as SSE2 is, since that is where a `double` is kept, and on AArch64 until
    // `-mgeneral-regs-only` says otherwise.
    let sse = |name| rucc_target::Feature::named(name).is_some_and(|it| opts.isa.has(it));
    opts.vector = match opts.target.arch {
        rucc_target::Arch::X86_64 => sse("sse2"),
        _ => !general_regs_only,
    };
    opts.x87 = x87;
    opts.x87_return = fp_ret_in_387;
    opts.exceptions = exceptions.unwrap_or(opts.non_call_exceptions);
    // After the loop, since whether `--64` or `-march=` is true of the target is a question about
    // the last `--target=`.
    let assembler = assembler_words(&asm_words, opts.target)?;
    opts.asm_fatal_warnings = assembler.fatal_warnings;
    opts.asm_noexecstack = assembler.noexecstack;
    opts.asm_keep_slots = assembler.keep_slots;
    // gcc's `finish_options`, read as a table. An executable is what nothing at all means, and
    // what `-fpie` means whatever the other family said, so `-fPIE -fno-pic` is still position
    // independent. A library needs `-fpic` and no `-fpie` after it, and anything else that said
    // no is the position dependent answer: `-fno-pie` alone, `-fno-pic` alone, or both.
    opts.pic = match (pie, pic) {
        (Some(true), _) | (None, None) => Pic::Executable,
        (_, Some(true)) => Pic::Library,
        _ => Pic::Absolute,
    };
    // The kernel model promises every address is a 32 bit number sign extended, which is only
    // true of code linked where it runs, so it is refused beside anything position independent,
    // the default included, with the words gcc uses. A distribution's gcc builds position
    // independent executables by default and says the same about `-mcmodel=kernel` on its own.
    if cmodel == rucc_target::CodeModel::Kernel {
        if opts.target.arch != rucc_target::Arch::X86_64
            || opts.target.object_format() != ObjectFormat::Elf
        {
            return Err(err(format!(
                "-mcmodel=kernel: {} has no kernel code model, which is x86-64 ELF's",
                opts.target
            )));
        }
        if opts.pic != Pic::Absolute {
            return Err(err(
                "code model kernel does not support PIC mode: add -fno-pie or -fno-pic".to_owned(),
            ));
        }
    }
    // The tiny model is AArch64's, and gcc for any other machine refuses it.
    if cmodel == rucc_target::CodeModel::Tiny
        && (opts.target.arch != rucc_target::Arch::Aarch64
            || opts.target.object_format() != ObjectFormat::Elf)
    {
        return Err(err(format!(
            "-mcmodel=tiny: {} has no tiny code model, which is AArch64 ELF's",
            opts.target
        )));
    }
    // The nop is written where a direct call would be, and a position independent call to the hook
    // goes through the procedure linkage table, so gcc refuses the pair, with or without `-pg`, and
    // so does this, in its words.
    if opts.nop_mcount && opts.pic != Pic::Absolute {
        return Err(err("'-mnop-mcount' is not implemented for '-fPIC'".to_owned()));
    }
    opts.code_model = cmodel;
    // The canary's place, when a flag moved it or the code model did, once both it and the
    // position independence are settled. gcc reads it through `%gs`
    // rather than `%fs` under `-mcmodel=kernel`, which is what `gcc-x86_64-has-stack-protector.sh`
    // looks for. Under `-fPIC` a symbol is reached through the global offset table like any
    // other, which is also what gcc writes.
    let kernel = opts.code_model == rucc_target::CodeModel::Kernel;
    let moved =
        guard_global || guard_reg.is_some() || guard_offset.is_some() || guard_symbol.is_some();
    let i386 = opts.target.arch == rucc_target::Arch::X86;
    // A global guard in i386 position independent code is reached through `%ebx`, which the check
    // in the epilogue runs after putting back. Nothing builds that, the kernel included, so it is
    // refused rather than written wrong. A symbol read through a segment is not: gcc writes its
    // address into the instruction whatever the position independence, and the kernel's probe
    // for `%fs` runs with the compiler's default, which is PIE.
    if i386 && opts.pic != Pic::Absolute && guard_global {
        return Err(err(
            "-mstack-protector-guard=global is not supported in i386 position independent code"
                .to_owned(),
        ));
    }
    if (moved || kernel) && opts.target.arch == rucc_target::Arch::X86_64 || moved && i386 {
        let table = opts.pic == Pic::Library;
        // The local copy in i386 position independent code, for the reason given below.
        let local = i386 && opts.pic != Pic::Absolute;
        let fail = if local { "__stack_chk_fail_local" } else { "__stack_chk_fail" };
        let reg = if kernel || i386 { rucc_target::Segment::Gs } else { rucc_target::Segment::Fs };
        let reg = guard_reg.unwrap_or(reg);
        opts.guard = Some(if guard_global {
            rucc_target::Guard { fail, ..rucc_target::Guard::global("__stack_chk_guard", table) }
        } else if let Some(name) = guard_symbol {
            let symbol = Some(&*Box::leak(name.to_owned().into_boxed_str()));
            let table = table && !i386;
            rucc_target::Guard { segment: Some(reg), symbol, table, system: false, at: 0, fail }
        } else {
            let at = guard_offset.unwrap_or(if i386 { 20 } else { 40 });
            rucc_target::Guard { fail, ..rucc_target::Guard::in_segment(reg, at) }
        });
    }
    // AArch64 reads `__stack_chk_guard` through the global offset table in code that may be position
    // independent, since the C library defines it, and from its own page otherwise. A kernel asks
    // for its copy in the task, which gcc wants all three flags for, and the offset is one `ldr`
    // can carry. A guard elsewhere than an ELF target is refused where the protector is.
    let elf = opts.target.object_format() == ObjectFormat::Elf;
    if opts.target.arch == rucc_target::Arch::Aarch64 && elf {
        opts.guard = Some(if guard_task {
            let Some(at) = guard_offset.filter(|_| guard_sp_el0) else {
                return Err(err("both -mstack-protector-guard-offset and \
                     -mstack-protector-guard-reg must be used with -mstack-protector-guard=sysreg"
                    .to_owned()));
            };
            if !(0..=32760).contains(&at) || at % 8 != 0 {
                return Err(err(format!(
                    "-mstack-protector-guard-offset={at}: the offset is a multiple of 8 from 0 \
                     to 32760"
                )));
            }
            rucc_target::Guard::in_task(at)
        } else if guard_sp_el0 || guard_offset.is_some() {
            return Err(err(
                "-mstack-protector-guard-reg and -mstack-protector-guard-offset are used with \
                 -mstack-protector-guard=sysreg"
                    .to_owned(),
            ));
        } else {
            rucc_target::Guard::global("__stack_chk_guard", opts.pic != Pic::Absolute)
        });
    }
    // i386 position independent code calls the hidden copy of the failure routine that the C
    // library's static half carries, as gcc does: a call to the shared one would go through the
    // procedure linkage table, which wants the global offset table's address in `%ebx`, and the
    // check runs in the epilogue after `%ebx` has been put back. See tamnd/rucc#2247.
    if opts.guard.is_none()
        && opts.target.arch == rucc_target::Arch::X86
        && opts.target.object_format() == ObjectFormat::Elf
        && opts.pic != Pic::Absolute
    {
        let guard = rucc_target::Guard::in_segment(rucc_target::Segment::Gs, 20);
        opts.guard = Some(rucc_target::Guard { fail: "__stack_chk_fail_local", ..guard });
    }
    link.sysroot = sysroot.clone();
    // Where a sysroot for a target that is not this machine would be. Read once, here, rather than
    // inside the link line, because a link line that read the environment could only be tested on a
    // machine whose environment said the right thing, and the link line is the last thing that
    // touches a binary. `spec/cross-compile/13-distribution.md` section 13.2 owns the answer.
    link.cache = Some(cache::dir());
    // And where a distribution's cross packages would have put a tree for the target, which is only
    // read when the target is not this machine and there is no sysroot of ours for it.
    link.usr = Some(link::usr());
    // And the ten field spelling of the target, because the release on it decides two things the
    // three field one cannot say: whether a target that is this architecture is still a cross
    // compile, and which directory under the cache it is against. After the loop because the last
    // `--target=` on the command line is the one that counts.
    link.pinned = pinned;
    // The deployment target, from the flag if there was one and from the tuple otherwise. Only an
    // Apple platform has one: anywhere else a version on the tuple is a libc or a preview number.
    if opts.target.os == rucc_target::Os::Darwin {
        opts.os_version = min_version.or_else(|| pinned.and_then(TargetTuple::os_version));
        link.os_version = opts.os_version;
    }
    // The Apple linker flags on a target whose linker has never heard of them, which clang refuses
    // with the same words rather than letting GNU ld say something less clear about it.
    if opts.target.os != rucc_target::Os::Darwin {
        let first = apple_only.as_deref().or_else(|| arches.first().map(|_| "-arch"));
        if let Some(flag) = first {
            return Err(err(format!("unsupported option '{flag}' for target '{}'", opts.target)));
        }
    }
    for arch in &arches {
        let named = match arch.as_str() {
            "arm64" => Some(rucc_target::Arch::Aarch64),
            "x86_64" => Some(rucc_target::Arch::X86_64),
            _ => None,
        };
        if named != Some(opts.target.arch) {
            return Err(err(format!(
                "-arch {arch}: the target is {}, and this compiler builds for one architecture \
                 at a time, so name the one wanted with --target= and build once for each",
                opts.target
            )));
        }
    }
    // After the loop rather than where `-pthread` was read, so that it lands after the objects
    // that refer to it. A static link takes the definitions it needs from a library when it
    // reaches it and not afterwards, so a library before the objects is a library that answers
    // nothing.
    if threads {
        inputs.push(Input::library("pthread"));
    }
    // The dialect a claimed GCC release compiled when nothing said which, per
    // `spec/04-driver-and-cli.md` section 4.6. Only when the claim was written, so the default
    // claim leaves the default dialect alone, and not on an MSVC row, where the claim is
    // `__GNUC__` and nothing else, as it is in clang.
    // A flag the claimed release did not have yet is an unknown option, as it is to that gcc.
    // Only when the claim was written, since the default claim is the newest release.
    if opts.gnuc_given {
        if let Some((_, why)) = newer.into_iter().find(|(since, _)| opts.gnuc.major < *since) {
            return Err(err(why));
        }
    }
    if !std_given && opts.gnuc_given && opts.target.env != rucc_target::Env::Msvc {
        opts.std = opts.gnuc.default_std();
        opts.gnu_extensions = true;
    }
    if let Some(query) = query {
        return Ok(Action::Print(answer(&query, &opts, &link)?));
    }
    if version {
        return Ok(Action::Print(banner(&opts)));
    }
    // `gcc -v` with no file prints its banner and stops, and build systems ask it this way to find
    // out which compiler they have. A `-l` or a `-Wl,` word is not a file to compile.
    if verbose && !inputs.iter().any(|input| input.role == Role::File) {
        return Ok(Action::Verbose(verbose_banner(&opts)));
    }
    // `-M` and `-MM` produce the rule and nothing else, so the run stops after phase 4 whatever
    // else the command line asked for. Read here rather than where the flag was, because a `-c`
    // written after it has to lose and the loop cannot know that until it has ended. The output
    // file is where the rule goes rather than where an object would have gone, and the last
    // phase being the preprocessor is what makes that true without a second rule for it.
    if opts.deps.instead_of_compiling {
        opts.emit = EmitKind::Preprocessed;
    }
    // A missing header is not an error only when the run stops at the rule. A compile cannot
    // continue without the header. GCC gives the same error.
    if opts.deps.generated && !opts.deps.instead_of_compiling {
        return Err(err("-MG may only be used with -M or -MM"));
    }
    if !nostdinc {
        opts.search.push_system(runtime::DIR);
        // And the library's after ours, which is the other half of the same order. They go on
        // here rather than at the point `--target=` or `--sysroot=` was read because either
        // one changes the answer and the last word on both is the end of the loop.
        //
        // Which library's is the question `link::cross_sysroot` answers, and it is asked here so
        // that the headers and the libraries come from the same place. A target that is this
        // machine reads this machine's headers, and a target that is not reads the ones in the
        // sysroot for it rather than the ones next door.
        let cross = link::cross_sysroot(opts.target, &link);
        let kernel = link::cross_kernel(opts.target, &link);
        let distro = link::distro_cross(opts.target, &link);
        // And the version of those headers, which only the bundled tree has an answer for. A host
        // glibc and a tree the user named both define `__GLIBC_MINOR__` in their own `features.h`,
        // and a second definition with a different value is a warning on every file, so the
        // condition is the same one that chose the directories.
        if cross.is_some() {
            let target = pinned.unwrap_or_else(|| opts.target.tuple());
            opts.glibc_minor = rucc_sysroot::bundled_glibc_minor(target).map_err(|skew| {
                err(format!(
                    "{skew}; pin a release the tree has, or name a tree that has that one \
                     with --sysroot"
                ))
            })?;
        }
        let system = library::header_dirs(
            opts.target,
            sysroot.as_deref(),
            cross.as_ref(),
            kernel.as_ref(),
            distro.as_ref(),
        );
        // The two licence walls of `spec/cross-compile/13-distribution.md` section 13.4, which are
        // the only way step 3 comes back with nothing on a hosted target. Section 8.6 asks for the
        // answer to name the licence and the lawful ways to get what is behind it, rather than
        // leaving a person with an `#include` that failed as though a directory had gone missing.
        //
        // It is left on the search path instead of refused here, because a program that includes
        // none of the library needs none of the SDK and section 8.6 is explicit that targeting the
        // platform has to keep working. So the reason waits until an include has actually failed,
        // which is the only moment it helps and the only moment it is true.
        //
        // The condition is that step 3 found nothing at all, so an `SDKROOT`, an `INCLUDE` or a mac
        // with Xcode on it all pass through untouched, and `-nostdinc` never reaches this block. A
        // `--sysroot` or `-isysroot` passes through as well, even when the tree it names turns out to
        // be empty or absent: somebody who wrote a path has already answered the question this
        // message asks, and answering it again over the top of a mistyped directory would hide the
        // mistake behind a licence notice.
        if system.is_empty() && sysroot.is_none() {
            let tuple = pinned.unwrap_or_else(|| opts.target.tuple());
            if let Some(wall) = rucc_sysroot::Wall::of(tuple) {
                opts.search.explain_missing_system(wall.no_headers(&tuple.to_canonical_string()));
            }
        }
        // And whether the tree somebody named is the release they asked for, which is the one
        // question left once the directories are settled and the only place both halves of it are
        // known. Only for a named tree, because that is the case where the release in the target
        // stops deciding anything, and `crate::glibc` is where the rest of the reasoning is.
        if sysroot.is_some() {
            notes.extend(glibc::skew(opts.target, pinned, &system));
        }
        for dir in system {
            opts.search.push_system(dir);
        }
    }
    // Once, here, rather than as each directory is pushed. A `-I` that names a system
    // directory has to lose to the system entry and the system entry is added last, so the
    // question cannot be answered until the whole path is known.
    opts.search.remove_duplicates();

    // The target has to be resolved before the configuration is printed, so this check comes
    // after the loop rather than at the point `--print-config` was seen.
    if print_config {
        return Ok(Action::PrintConfig(Box::new(opts)));
    }
    if print_pipeline {
        return Ok(Action::PrintPipeline(Box::new(opts)));
    }
    if print_params {
        return Ok(Action::PrintParams(opts.params));
    }
    if hardened {
        harden(&mut opts, &mut link, args, &mut notes)?;
    }
    let plan = Plan::new(&opts, &inputs, output.as_deref()).map_err(|e| err(e.message))?;
    if print_plan {
        return Ok(Action::PrintPlan {
            opts: Box::new(opts),
            plan: Box::new(plan),
            link: Box::new(link),
        });
    }
    // gas answers `--version` and stops without reading its input, and gcc passes its exit status
    // on, so a compiler asked this writes its one line and nothing else. After the plan, so that a
    // command line gcc would refuse before it ran the assembler is refused here too.
    if assembler.version {
        return Ok(Action::Print(gas_banner(opts.gnu_as)));
    }
    Ok(Action::Compile {
        opts: Box::new(opts),
        plan: Box::new(plan),
        link: Box::new(link),
        jobs,
        verbose,
        notes,
    })
}

/// `-fhardened`, as GCC 14 and later do it.
///
/// The flag turns on each item of this list that the command line does not name: `_FORTIFY_SOURCE=3`
/// at `-O1` and above, `_GLIBCXX_ASSERTIONS`, `-ftrivial-auto-var-init=zero`,
/// `-fstack-protector-strong`, `-fstack-clash-protection`, `-fcf-protection=full` on x86, and
/// `-z now` and `-z relro` on the link. PIE is the default here already. An item that the line names
/// stays as the line says, and a warning says so, with the words that gcc uses. `-Wno-hardened`
/// stops the warnings. gcc supports the flag only on GNU/Linux, and so does rucc.
fn harden(
    opts: &mut Options,
    link: &mut LinkOptions,
    args: &[String],
    notes: &mut Vec<String>,
) -> Result<(), CliError> {
    use rucc_target::{Arch, Env, Os};
    if opts.target.os != Os::Linux || opts.target.env != Env::Gnu {
        return Err(err(format!("-fhardened is not supported for {}", opts.target)));
    }
    let warn = args.iter().rev().find_map(|arg| match arg.as_str() {
        "-Whardened" => Some(true),
        "-Wno-hardened" => Some(false),
        _ => None,
    });
    let mut note = |text: String| {
        if warn.unwrap_or(true) {
            notes.push(format!("{text} [-Whardened]"));
        }
    };
    let named = |prefixes: &[&str]| {
        args.iter().any(|arg| prefixes.iter().any(|prefix| arg.starts_with(prefix)))
    };
    let given =
        |list: &[String], name: &str| list.iter().any(|item| item.split('=').next() == Some(name));
    let skipped = |what: &str| {
        format!(
            "'{what}' is not enabled by -fhardened because it was specified on the command line"
        )
    };

    if given(&opts.defines, "_FORTIFY_SOURCE") || given(&opts.undefines, "_FORTIFY_SOURCE") {
        note(
            "'_FORTIFY_SOURCE' is not enabled by -fhardened because it was specified in -D or -U"
                .to_owned(),
        );
    } else if opts.opt_level == rucc_session::OptLevel::O0 {
        note(
            "'_FORTIFY_SOURCE' is not enabled by -fhardened because optimizations are turned off"
                .to_owned(),
        );
    } else {
        opts.defines.push("_FORTIFY_SOURCE=3".to_owned());
    }
    if !given(&opts.defines, "_GLIBCXX_ASSERTIONS")
        && !given(&opts.undefines, "_GLIBCXX_ASSERTIONS")
    {
        opts.defines.push("_GLIBCXX_ASSERTIONS".to_owned());
    }
    if named(&["-ftrivial-auto-var-init="]) {
        note(skipped("-ftrivial-auto-var-init=zero"));
    } else {
        opts.auto_var_init = Some(0);
    }
    if named(&["-fstack-protector", "-fno-stack-protector"]) {
        note(skipped("-fstack-protector-strong"));
    } else {
        opts.protector = Protector::Strong;
    }
    // gcc says nothing when the line names this one.
    if !named(&["-fstack-clash-protection", "-fno-stack-clash-protection"]) {
        opts.stack_clash = true;
    }
    if matches!(opts.target.arch, Arch::X86_64 | Arch::X86) {
        if named(&["-fcf-protection", "-fno-cf-protection"]) {
            note(skipped("-fcf-protection=full"));
        } else {
            opts.control = Control::Full;
        }
    }

    // The link. gcc leaves out `-z now` and `-z relro` when the line asks for a link that is not
    // a PIE, or asks the linker for lazy binding or for no relro.
    let mut words = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if let Some(list) = arg.strip_prefix("-Wl,") {
            words.extend(list.split(','));
        } else if arg == "-Xlinker" {
            words.extend(args.get(i + 1).map(String::as_str));
        }
    }
    let weaker = words.iter().any(|word| matches!(*word, "-zlazy" | "-znorelro"))
        || words.windows(2).any(|pair| pair[0] == "-z" && matches!(pair[1], "lazy" | "norelro"));
    if link.pie == Some(false) || link.is_static || link.shared || link.relocatable || weaker {
        if opts.emit == EmitKind::Executable {
            note(
                "linker hardening options not enabled by -fhardened because other link options \
                 were specified on the command line"
                    .to_owned(),
            );
        }
    } else {
        link.hardened = true;
    }
    Ok(())
}

/// What `--fetch <tuple>` asked for, or why it is not a thing that can be done.
///
/// The lookup happens here rather than at the point the bytes would move, so that a target this
/// release pins nothing for is a refusal from the parser and the only code that runs a downloader is
/// code that already knows what it is getting.
///
/// # Errors
///
/// [`CliError`] when `--offline` forbade it, when there are input files as well, when the tuple is
/// not a target this compiler knows, when its sysroot is behind Apple's licence wall, and when this
/// release pins no artifact for it.
fn fetch_action(
    named: &str,
    offline: bool,
    accepted: bool,
    inputs: &[Input],
) -> Result<Action, CliError> {
    // Not a precedence question. Section 13.2 says `--offline` forbids a fetch entirely, so a
    // command line that writes both has asked for two opposite things and the answer is to say so
    // rather than to pick one of them.
    if offline {
        return Err(err(
            "--fetch asks for a download and --offline forbids every download, so this command \
             line asks for two opposite things. Drop one of them: --offline is how a build says it \
             will not reach the network, and --fetch is one of the two things in this compiler \
             that reaches it",
        ));
    }
    if let Some(first) = inputs.first() {
        return Err(err(format!(
            "--fetch gets a sysroot and compiles nothing, so `{}` on the same command line is an \
             input that nothing would read",
            first.path
        )));
    }
    let target: TargetTuple = named
        .parse()
        .map_err(|why| err(format!("--fetch {named}: {why}, so there is no sysroot to get")))?;
    // The canonical spelling, because that is what a row is named by and what the directory under
    // the cache is called, and a person is free to write a tuple the long way round.
    let tuple = target.to_canonical_string();
    // Microsoft's side of the wall has something to fetch after all, which is the files its own
    // installer would fetch, from the build this release pins and only once the licence has been
    // accepted. Nothing of it is ours and nothing of it comes from us, which is why it is the other
    // action with a flag on it rather than a row in the table.
    if rucc_sysroot::Wall::of(target) == Some(rucc_sysroot::Wall::Microsoft) {
        return Ok(Action::FetchMsvcSdk { target, accepted, cache: cache::dir(), pinned: true });
    }
    // Before the table is consulted, because a target behind a licence wall is not a row that has not
    // been written yet. Section 13.4 is that no release pins one of these ever, so the message says
    // the licence and the two lawful ways rather than naming the producer that will publish the rest.
    if let Some(wall) = rucc_sysroot::Wall::of(target) {
        return Err(err(format!("--fetch {tuple}: {}", wall.no_fetch(&tuple))));
    }
    let Some(what) = rucc_sysroot::pinned_for_target(target) else {
        return Err(err(unpinned(&tuple)));
    };
    Ok(Action::Fetch { what, target, cache: cache::dir() })
}

/// What `--fetch-msvc-sdk <tuple>` asks for, weighed the same way the fetch above is.
///
/// The target is resolved here rather than where the work happens, so that a tuple this compiler
/// does not know and a target that is not behind Microsoft's wall are refusals from the parser like
/// every other thing a command line can ask for and not have. Whether the licence was accepted is
/// carried rather than acted on, because what it changes is what the command does and not whether
/// the command line made sense.
///
/// # Errors
///
/// [`CliError`] when `--offline` forbade it, when there are input files as well, and when the tuple
/// is not a target this compiler knows.
fn fetch_msvc_action(
    named: &str,
    offline: bool,
    accepted: bool,
    inputs: &[Input],
) -> Result<Action, CliError> {
    if offline {
        return Err(err(
            "--fetch-msvc-sdk asks for a download and --offline forbids every download, so this \
             command line asks for two opposite things. Drop one of them: --offline is how a build \
             says it will not reach the network",
        ));
    }
    if let Some(first) = inputs.first() {
        return Err(err(format!(
            "--fetch-msvc-sdk gets an SDK and compiles nothing, so `{}` on the same command line \
             is an input that nothing would read",
            first.path
        )));
    }
    let target: TargetTuple = named.parse().map_err(|why| {
        err(format!("--fetch-msvc-sdk {named}: {why}, so there is no SDK to get"))
    })?;
    Ok(Action::FetchMsvcSdk { target, accepted, cache: cache::dir(), pinned: false })
}

/// Why there is nothing to fetch for a target, which is a different sentence when the table is
/// empty.
///
/// A release that pins nothing and a release that pins eleven targets and not this one are two
/// situations, and a message that did not tell them apart would send somebody looking for a typo in
/// their tuple when the answer is that this work is not finished.
fn unpinned(tuple: &str) -> String {
    let pinned = rucc_sysroot::pinned_targets();
    if pinned.is_empty() {
        return format!(
            "this release pins no sysroot for {tuple}, and it pins none for any target yet. A \
             sysroot is built and published by the producer in tamnd/rucc-cross, per \
             spec/cross-compile/13-distribution.md section 13.8, and a release of this compiler \
             names one by URL and by hash afterwards. Until then, pass --sysroot=<dir> to compile \
             against a tree you have already"
        );
    }
    format!(
        "this release pins no sysroot for {tuple}. What it pins is {}. Pass --sysroot=<dir> to \
         compile against a tree you have already",
        pinned.join(", ")
    )
}

/// Gets the artifact and installs it, saying what each step did.
///
/// The steps are section 13.8's and so are the messages: the transport is somebody else's program
/// and the check is ours, so a person reading this wants to know which downloader ran, that the
/// bytes matched, how many files the record named and where the tree ended up. A fetch of something
/// that is already there says that instead and moves nothing.
///
/// A Linux target is two artifacts, its own sysroot and the kernel header tree every Linux target
/// shares, and `kernel` is the second one when the target reads it. It is fetched after the sysroot
/// and by the same two steps, so a machine that has fetched one Linux target already has it and a
/// second target's fetch says so and moves nothing.
fn fetch_sysroot(
    what: &rucc_sysroot::Pinned,
    kernel: Option<&rucc_sysroot::Pinned>,
    target: TargetTuple,
    cache: &std::path::Path,
) -> i32 {
    let tuple = target.to_canonical_string();
    let say = |line: &str| {
        let _ = writeln!(host::stdout(), "rucc: {tuple}: {line}");
    };
    if let Err(why) = bring(what, cache, &say) {
        return complain(why);
    }
    let archive = what.archive_in(cache);
    match install::install(&archive, what.sha256, target, cache) {
        Ok(done) => report(&done, "sysroot", &say),
        Err(why) => return complain(why),
    }
    let Some(kernel) = kernel else { return 0 };
    if let Err(why) = bring(kernel, cache, &say) {
        return complain(why);
    }
    match install::install_kernel(&kernel.archive_in(cache), kernel.sha256, cache) {
        Ok(done) => {
            report(&done, "kernel header tree", &say);
            0
        }
        Err(why) => complain(why),
    }
}

/// The download half of a fetch, for one artifact.
fn bring(
    what: &rucc_sysroot::Pinned,
    cache: &std::path::Path,
    say: &impl Fn(&str),
) -> Result<(), CliError> {
    let archive = what.archive_in(cache);
    match fetch::fetch(what.url, what.sha256, &archive)? {
        fetch::Fetched::AlreadyThere => {
            say(&format!("{} is already here and matches the hash", archive.display()));
        }
        fetch::Fetched::Downloaded(by) => {
            say(&format!("downloaded {} with {}", what.url, by.program()));
        }
    }
    Ok(())
}

/// What an install did, in the words a person reading a fetch wants.
fn report(done: &install::Installed, what: &str, say: &impl Fn(&str)) {
    match &done.before {
        install::Before::Nothing => {
            say(&format!("{} files installed at {}", done.files, done.root.display()));
        }
        install::Before::TheSame => {
            say(&format!(
                "the same {what} is already at {}, so nothing moved",
                done.root.display()
            ));
        }
        install::Before::Different(was) => {
            say(&format!(
                "{} files installed at {}, over a tree whose record digested to {was}",
                done.files,
                done.root.display()
            ));
        }
    }
    if !done.folded.is_empty() {
        say(&format!(
            "this filesystem does not tell case apart, so {} the file of a name that differs \
             only in case, and an include of it reads that file: {}",
            if done.folded.len() == 1 {
                "one header was written over by"
            } else {
                "these headers were written over by"
            },
            done.folded.join(", ")
        ));
    }
    say(&format!("the {what}'s record digests to {}", done.digest));
}

/// What one of the `-dump` and `-print` flags prints.
///
/// GCC prints the name back unchanged when it cannot find the file a `-print` flag asked about,
/// which is what makes the answer safe to paste into a link line whether or not the file is
/// there, and this does the same.
fn answer(query: &Query, opts: &Options, link: &LinkOptions) -> Result<String, CliError> {
    let found = |name: &str| {
        link::find_in_search(link, opts.target, name)
            .map_or_else(|| name.to_owned(), |path| path.display().to_string())
    };
    Ok(match query {
        Query::Machine => opts.target.to_string(),
        Query::Version => opts.gnuc.dumpversion(),
        Query::FullVersion => opts.gnuc.to_string(),
        Query::Multiarch => link::multiarch(opts.target),
        // libtool asks this to find the library directory next to the one it was given.
        Query::MultiOsDirectory => {
            link::multi_os_directory(opts.target, link.sysroot.as_deref()).to_owned()
        }
        // The three lines GCC prints, in its order and with its punctuation, because what reads
        // them is a script written against that shape. There is no installation directory to
        // report: this compiler is one binary that works wherever it is copied, and the headers
        // it ships are inside it, so `install` is where the binary is and nothing is under it.
        Query::SearchDirs => {
            let here = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                .unwrap_or_default();
            let list = |dirs: &[PathBuf]| {
                dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(":")
            };
            let libraries = link::search_dirs(link, opts.target);
            format!(
                "install: {}\nprograms: ={}\nlibraries: ={}",
                here.display(),
                list(&link.prefixes),
                list(&libraries)
            )
        }
        // The root the rest of the answers are under, which a build system asks for when it wants
        // to find a file itself rather than ask for one by name, and which is the first thing to
        // look at when a cross build read a header nobody expected. A native compile has no
        // sysroot and the answer is the empty line, which is what GCC prints when it was
        // configured without one. `--sysroot` wins over ours because it wins everywhere else.
        Query::Sysroot => {
            sysroot_root(opts, link).map(|root| root.display().to_string()).unwrap_or_default()
        }
        // Section 13.5 of `spec/cross-compile/13-distribution.md`: for every input that is not this
        // compiler's own code, what it is, where it was got, its hash, its licence and whether it
        // was bundled, generated or fetched. What is printed is the manifest the sysroot already
        // carries rather than a second format saying the same things, because the three uses 13.5
        // gives for this are a licence notice, a reproducibility check and a security audit, and all
        // three are somebody else parsing it. One format is one parser to write.
        // Read and rendered rather than copied out, so that what comes back is the format this
        // build understands. The last newline comes off because whatever prints an answer adds
        // one, the way it does for every other query here. Keeping it would put a blank line at
        // the end of the one answer that is a file somebody diffs against the file it came from.
        Query::SysrootProvenance => match sysroot_manifest(opts, link)? {
            Some(manifest) => manifest.render().trim_end_matches('\n').to_string(),
            None => String::new(),
        },
        // Section 13.2 of the same document, which asks for the hash of a cache directory's
        // contents in the directory's name. A name cannot carry one, because the path has to be
        // computable before anything has been read, by the producer about to write the files and by
        // the compiler about to read them, and neither has the contents when it asks. So the number
        // is here instead, and it is the sha256 of the record rather than of a walk of the tree,
        // which means `sha256sum` over the manifest answers the same thing.
        Query::SysrootDigest => match sysroot_manifest(opts, link)? {
            Some(manifest) => manifest.digest(),
            None => String::new(),
        },
        // GCC answers `plugin` with the directory its plugin headers are under, and the kernel
        // turns `GCC_PLUGINS` on when `include/plugin-version.h` is there. This compiler loads no
        // GCC plugin, so the answer is the bare word, which is what GCC prints for a file it does
        // not have, and nothing a search directory holds is allowed to change that.
        Query::FileName(name) if name == "plugin" => name.clone(),
        // GCC answers `include` with the directory its own `<stdarg.h>` and the rest are in, and a
        // kernel before 5.15 passes that to `-isystem` after `-nostdinc`. Ours are in the binary,
        // so they are written out to the cache for this, and the bare word is the answer only
        // when the cache cannot be written, as GCC's is for a file it does not have.
        Query::FileName(name) if name == "include" => {
            runtime::materialized(&cache::dir(), host::id())
                .map_or_else(|_| name.clone(), |dir| dir.display().to_string())
        }
        Query::FileName(name) => found(name),
        // The name GCC gives the library of routines a compiler's output calls that the C
        // library does not have. The native link uses the one in the directory of the newest GCC,
        // so that is the full path. With no GCC on the machine, the answer is the name itself,
        // which is what GCC prints when it cannot find one either.
        Query::Libgcc => found("libgcc.a"),
        // A program rather than a library: the linker and the archiver are the ones a build asks
        // about, and this compiler finds them on the path or under `-B` rather than shipping
        // them, so the name back is the honest answer unless a `-B` prefix holds one.
        Query::ProgName(name) => link
            .prefixes
            .iter()
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
            .map_or_else(|| name.clone(), |path| path.display().to_string()),
    })
}

/// The root every sysroot answer is about.
///
/// One function rather than a copy in each, because the other flags exist to say what is inside the
/// tree this one names, and two answers that disagreed about which tree that is would be a
/// difference nobody would think to look for. `--sysroot` wins over ours because it wins everywhere
/// else.
fn sysroot_root(opts: &Options, link: &LinkOptions) -> Option<PathBuf> {
    link.sysroot
        .clone()
        .or_else(|| link::cross_sysroot(opts.target, link).map(|at| at.root().to_path_buf()))
}

/// The record of the sysroot this command line reads, when there is one to read.
///
/// [`None`] covers two cases that both print nothing, and they are different things. A compile for
/// this machine has no sysroot at all, and a tree somebody laid out themselves and pointed
/// `--sysroot` at carries no manifest, so nothing here knows where any of it came from. Saying
/// nothing is the only honest answer to either, and a reader can tell it from a manifest with no
/// inputs in it because that one still has its header lines.
///
/// # Errors
///
/// A manifest this build cannot parse, and anything else that went wrong reading the file. Passing a
/// record we could not read on to whoever asked would make their parser the one that finds the
/// problem, and every use section 13.5 gives for these two flags is somebody else reading the
/// output.
fn sysroot_manifest(opts: &Options, link: &LinkOptions) -> Result<Option<Manifest>, CliError> {
    let Some(root) = sysroot_root(opts, link) else {
        return Ok(None);
    };
    let path = Sysroot::at(root, opts.target.tuple()).manifest_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => Manifest::parse(&text)
            .map(Some)
            .map_err(|why| err(format!("{}: {why}", path.display()))),
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(why) => Err(err(format!("{}: {why}", path.display()))),
    }
}

/// The passes the command line added and removed, in the order it did.
///
/// `-f<pass>` and `-fno-<pass>` as they were given, after what `-foptimize-sibling-calls` and its
/// `-fno-` form say about the pass that makes a loop of a call a function makes to itself. gcc
/// puts its `tailr` pass under that flag, and so does this. The flag goes first, so a pass named on
/// its own has the last word. It adds no pass at `-O0`, where gcc runs none.
pub(crate) fn toggles(opts: &Options) -> Vec<(String, bool)> {
    let mut toggles = Vec::with_capacity(opts.passes.len() + 1);
    if let Some(on) = opts.sibling_calls {
        if !on || opts.opt_level != rucc_session::OptLevel::O0 {
            toggles.push((rucc_opt::tailrec::NAME.to_owned(), on));
        }
    }
    toggles.extend(opts.passes.iter().cloned());
    toggles
}

/// Renders the passes this level will run, in order, with what each one does.
///
/// The level is the whole of the answer unless a `-f` flag edited it, which is section 9.1 of
/// `spec/09-optimizer.md`: a level is a list somebody wrote down rather than something that
/// emerges from which flags happen to be set, and this is how that list is read.
#[must_use]
pub fn print_pipeline(opts: &Options) -> String {
    let mut settings = rucc_opt::Options::for_level(opts.opt_level);
    settings.toggles = toggles(opts);
    settings.global_fuel = opts.pass_fuel_global;
    for (on, spec) in &opts.pass_gates {
        // Every spelling was checked while the arguments were parsed, so there is nothing here
        // this can refuse, and a listing is not the place to report it if there were.
        let _ = settings.gates.add(*on, spec);
    }
    rucc_opt::pipeline::print(&settings)
}

/// Renders the resolved configuration.
///
/// One `key: value` per line, sorted by nothing in particular but fixed in order, because
/// this output is diffed across hosts in CI and a reordering would read as a change.
#[must_use]
pub fn print_config(opts: &Options) -> String {
    let sess = Session::new(opts.clone());
    let t = &sess.target;
    let mut out = String::new();
    let _ = writeln!(out, "version: {VERSION}");
    // The three field triple the driver was given rather than the ten field tuple it widens to,
    // because this output is what a build system reads to find out what it asked for. The tuple is
    // the compiler's model of the machine and this line is a receipt for a command line.
    let _ = writeln!(out, "target: {}", opts.target);
    let _ = writeln!(out, "arch: {}", opts.target.arch.as_str());
    let _ = writeln!(out, "os: {}", opts.target.os.as_str());
    let _ = writeln!(out, "env: {}", opts.target.env.as_str());
    let _ = writeln!(out, "object-format: {}", t.object_format.as_str());
    let _ = writeln!(out, "pointer-width: {}", t.pointer_width);
    let _ = writeln!(out, "long-width: {}", t.long_width);
    let _ = writeln!(out, "long-double-width: {}", t.long_double_width);
    let _ = writeln!(out, "endian: {}", if t.little_endian { "little" } else { "big" });
    let _ = writeln!(out, "char-signed: {}", t.char_is_signed);
    let _ = writeln!(out, "va-list: {}", t.va_list.map_or("none", |list| list.as_str()));
    // The register file as a count per class, which is enough to tell a target whose registers
    // are described from one whose are not without printing sixteen names nobody asked for.
    let regs: Vec<String> = t
        .regs
        .classes()
        .map(|(class, info)| format!("{} {}", info.name, t.regs.len(class)))
        .collect();
    let _ = writeln!(
        out,
        "registers: {}",
        if regs.is_empty() { "none".to_string() } else { regs.join(", ") }
    );
    // What the schedule was chosen with, which is a sentence rather than a name on purpose: two
    // runs of a benchmark that disagree are usually two models and not two compilers.
    let _ = writeln!(out, "timing-model: {}", t.timing.map_or("none", |timing| timing.model));
    let _ = writeln!(out, "opt-level: {}", sess.opts.opt_level);
    let _ = writeln!(out, "safety: {}", sess.opts.safety);
    let _ = writeln!(out, "emit: {}", sess.opts.emit.as_str());
    let _ = writeln!(out, "debug-info: {}", sess.opts.debug_info);
    let _ = writeln!(out, "frame-pointer: {}", sess.opts.keeps_frame_pointer());
    let _ = writeln!(out, "red-zone: {}", sess.opts.red_zone);
    let _ = writeln!(out, "stack-protector: {}", sess.opts.protector);
    let _ = writeln!(out, "stack-clash-protection: {}", sess.opts.stack_clash);
    let _ = writeln!(out, "cf-protection: {}", sess.opts.control);
    let _ = writeln!(out, "patchable-function-entry: {}", sess.opts.patchable);
    let _ = writeln!(out, "profile: {}", sess.opts.profile);
    let _ = writeln!(out, "profile-hook: {}", sess.opts.hook);
    // Last because it is the one key with more than one line under it, and the only one
    // whose value is a property of the machine rather than of the command line.
    for dir in sess.opts.search.dirs() {
        let system = if dir.is_system { " (system)" } else { "" };
        let _ = writeln!(out, "include: {}{system}", dir.path.display());
    }
    out
}

/// The output name the make target is taken from, which is the `-o` argument or nothing.
///
/// A run that stops at the preprocessor has not named an object, whatever its `-o` says: under
/// `-E` that argument is the preprocessed text and under `-M` it is the rule itself, and neither
/// is a file `make` would rebuild by running this rule. GCC agrees and falls back to the source
/// name in both, which is why a `-MD -E -o out.i` writes `out.d` holding a rule for `a.o`. From
/// `-S` on the argument does name what the rule builds, and it is used as written.
fn deps_target_output<'a>(opts: &Options, plan: &'a Plan) -> Option<&'a str> {
    if opts.emit == EmitKind::Preprocessed { None } else { plan.output.as_deref() }
}

/// Writes to a path the command line named rather than one the plan derived, where `-` is
/// standard output.
fn write_named(path: &str, bytes: &[u8]) -> Result<(), String> {
    if path == "-" {
        return write_out(&Output::Stdout, bytes);
    }
    write_out(&Output::File(path.to_owned()), bytes)
}

/// Writes the make rule for one input, and reports whether it got there.
///
/// A rule with no file of its own goes where the compilation it replaced would have written,
/// which is what makes the usual makefile recipe work: `rucc -M $< -o $@` leaves the rule in
/// `$@`, and the same line with the `-o` left off puts it on standard output.
fn write_deps(
    opts: &Options,
    plan: &Plan,
    job: &Job,
    found: &[Dependency],
    stderr: &mut impl std::io::Write,
) -> bool {
    let targets = if opts.deps.targets.is_empty() {
        vec![deps::default_target(&job.input, deps_target_output(opts, plan))]
    } else {
        opts.deps.targets.clone()
    };
    let rule = deps::rule(&opts.deps, &targets, &job.input, found);
    // The file, on the other hand, is named after the `-o` in every mode that still has one to
    // spend, which is every mode except the two that spend it on the rule.
    let wrote = match deps::default_file(&opts.deps, &job.input, plan.output.as_deref()) {
        // A `-MF` on a run that had nowhere else to put the rule leaves the file the `-o`
        // named empty rather than absent, because a makefile that named it as a target of its
        // own is a makefile that will look for it.
        Some(path) => write_named(&path, rule.as_bytes()).and_then(|()| {
            if opts.deps.instead_of_compiling { write_out(&job.output, b"") } else { Ok(()) }
        }),
        None => write_out(&job.output, rule.as_bytes()),
    };
    if let Err(e) = wrote {
        let _ = writeln!(stderr, "rucc: error: {e}");
        return false;
    }
    true
}

/// Runs phase 4 over every input that has one, and writes what came out.
///
/// One input that fails does not stop the others. A build that reports every file it could
/// not preprocess in one run is worth more than one that stops at the first, and the exit
/// status is still a failure either way.
fn preprocess_all(opts: &Options, plan: &Plan) -> i32 {
    let fs = OsFileSystem::new();
    let mut stderr = host::stderr();
    let mut failed = false;
    for job in &plan.jobs {
        if !job.phases.first().is_some_and(|p| *p == Phase::Preprocess) {
            // An input that is already preprocessed, or an object file. GCC passes these
            // through untouched, and the plan has already said so in its notes.
            continue;
        }
        let started = std::time::Instant::now();
        let assembly = job.kind == InputKind::AssemblerWithCpp;
        let result = preprocess(opts, &job.input, assembly, &fs);
        if opts.time {
            say_time(&job.input, started.elapsed(), &mut stderr);
        }
        for message in &result.messages {
            let _ = writeln!(stderr, "{message}");
        }
        if result.failed() {
            failed = true;
            continue;
        }
        if opts.deps.emit {
            failed |= !write_deps(opts, plan, job, &result.deps, &mut stderr);
            // `-M` and `-MM` asked for the rule instead of the text, so there is nothing else
            // to write. The other two asked for both and fall through to the text below.
            if opts.deps.instead_of_compiling {
                continue;
            }
        }
        if let Err(e) = write_out(&job.output, result.text.as_bytes()) {
            let _ = writeln!(stderr, "rucc: error: {e}");
            failed = true;
        }
    }
    i32::from(failed)
}

/// Whether this job is a file of assembly that has to be assembled and that nothing here assembles.
///
/// The phases rather than the kind, because there are two kinds of assembly input and one of them
/// is preprocessed first, and because an object file also has no compile phase and is not this: it
/// has no phases at all and goes to the linker as it is. A `.s` on a `-c` line has exactly
/// [`Phase::Assemble`] left, and a `.S` has the preprocessor in front of it, and neither has
/// anything the front end can do.
fn needs_an_assembler(job: &Job) -> bool {
    job.phases.contains(&Phase::Assemble) && !job.phases.contains(&Phase::Compile)
}

/// Whether the preprocessor runs over it on the way in, which is the whole difference between the
/// two kinds of assembly input.
fn assembly_wants_cpp(job: &Job) -> bool {
    job.phases.contains(&Phase::Preprocess)
}

/// Runs the front end over every input that has a compile phase, and writes what came out.
///
/// The same rule as [`preprocess_all`]: one input that fails does not stop the others, and the
/// exit status is a failure either way. An input that is already assembly or an object has no
/// compile phase and is passed over here, which the plan has already said in its notes.
fn compile_all(opts: &Options, plan: &Plan) -> i32 {
    let fs = OsFileSystem::new();
    let mut stderr = host::stderr();
    let mut failed = false;
    let (mut remarks, ok) = Remarks::new(opts.opt_info_file.as_ref(), &mut stderr);
    failed |= !ok;
    let mut fired = Fired::new();
    let mut pressure = Pressure::new();
    let mut lowerings = Lowerings::new();
    for job in &plan.jobs {
        if !job.phases.contains(&Phase::Compile) && !needs_an_assembler(job) {
            continue;
        }
        // An input of IR is read back rather than compiled, since the C it came from is not
        // here any more. A file of assembly does not go through the front end at all and is
        // read by the assembler instead. Everything after this is the same for all three, so
        // the paths meet again at the messages and the file the result is written to.
        let started = std::time::Instant::now();
        let result = if needs_an_assembler(job) {
            assemble(opts, &job.input, assembly_wants_cpp(job), &fs)
        } else if job.kind == InputKind::Ir {
            compile_ir(opts, &job.input, &fs)
        } else {
            compile(&counted(opts, job), &job.input, &fs)
        };
        if opts.time {
            say_time(&job.input, started.elapsed(), &mut stderr);
        }
        failed |= !write_trace(opts, job, started, &result, &mut stderr);
        fired.merge(&result.fired);
        pressure.merge(&result.pressure);
        lowerings.merge(&result.lowerings);
        failed |= !write_dumps(&job.input, &result.dumps, &mut stderr);
        failed |= !remarks.write(&result.remarks, &mut stderr);
        for message in &result.messages {
            let _ = writeln!(stderr, "{message}");
        }
        // Before the failure below, because a compilation that stopped in the back end is exactly
        // the one whose preprocessed source somebody wants to look at.
        failed |= !write_temps(job, &result.temps, &mut stderr);
        // Before it as well, because gcc leaves an empty report for a file that did not compile
        // and a build that looks for one beside every object should find one.
        failed |= !write_stack_usage(job, &result.stack_usage, &mut stderr);
        failed |= !write_note(job, &result.note, &mut stderr);
        if result.failed() {
            failed = true;
            // gcc and clang leave the rule behind for a file that got through the preprocessor and
            // then failed, and kbuild reads it after a command it expects to fail.
            if opts.deps.emit && result.preprocessed {
                let _ = write_deps(opts, plan, job, &result.deps, &mut stderr);
            }
            continue;
        }
        // `-MD` and `-MMD` write the rule beside the object and let the compilation happen, so
        // this is the one path where both files come out of the same run. An input of IR has no
        // dependencies to report and produces an empty list, which produces a rule naming only
        // itself, and that is the honest answer rather than a missing file.
        if opts.deps.emit {
            failed |= !write_deps(opts, plan, job, &result.deps, &mut stderr);
        }
        if let Err(e) = write_out(&job.output, result.artifact.bytes()) {
            let _ = writeln!(stderr, "rucc: error: {e}");
            failed = true;
        }
    }
    failed |= !write_coverage(opts, &fired, &mut stderr);
    failed |= !write_pressure(opts, &pressure, &mut stderr);
    failed |= !write_lowering(opts, &lowerings, &mut stderr);
    i32::from(failed)
}

/// A directory for the object files only the link step ever sees, removed when it goes away.
///
/// `-c` writes its object where the user can see it and linking does not, which is the whole of
/// the difference: a `rucc a.c b.c` leaves an executable behind and nothing else, the same as
/// every other compiler. Removing them on drop rather than at the end of a function is so that a
/// link that failed leaves nothing behind either.
struct Scratch {
    /// Where the objects go.
    dir: PathBuf,
}

impl Scratch {
    /// Makes one, under whatever the platform calls its temporary directory.
    ///
    /// The name carries the process id so that two compilers running at once do not share a
    /// directory, which they would otherwise do the moment two of them compiled a file of the
    /// same name.
    fn new() -> Result<Scratch, String> {
        let dir = host::temp_dir().join(format!("rucc-{}", host::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Scratch { dir })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The link line the plan describes, for `-###`.
///
/// The names in it are the hints the plan carries rather than the temporaries a real compilation
/// would choose, because `-###` prints the line without having compiled anything and so has
/// nothing to point at. That also makes the printed line readable rather than naming a directory
/// that only exists while a compilation is running.
fn link_line(opts: &Options, link: &LinkOptions, job: &LinkJob) -> Result<String, link::Error> {
    let linker = link::find(opts.target, link)?;
    let args = link::line(opts.target, link, &job.inputs, &job.output)?;
    Ok(link::render(&linker, &args))
}

/// Compiles everything, then links it.
///
/// The objects go in a directory that is removed afterwards, which is why this is not
/// [`compile_all`] followed by a link: the plan says an object feeding the linker is temporary
/// and does not say where, because where is a question that only has an answer once something is
/// running.
fn link_all(opts: &Options, plan: &Plan, link: &LinkOptions, verbose: bool) -> i32 {
    let Some(job) = &plan.link else {
        // Every path into here comes from a plan whose last phase is the link, and such a plan
        // has a link job. Saying so is cheaper than an unwrap that would have to be explained.
        let mut stderr = host::stderr();
        let _ = writeln!(stderr, "rucc: error: there is nothing to link");
        return 1;
    };
    // Before anything is compiled, because a linker that is not on the machine is worth knowing
    // about in the second it takes to look rather than after the compilation.
    // And before that, whether this link has a line at all and whether what it reads is on the
    // machine. Both are answerable now, and a target whose sysroot has not been built is worth
    // saying so about before the compilation rather than after it.
    // First of all, whether there is code for this target. A target with no back end has no
    // runtime either, so the preflight would name the missing runtime and a command that fails.
    let compiles = plan.jobs.iter().any(|job| job.phases.contains(&Phase::Compile));
    if let Some(why) = compiles.then(|| compile::no_back_end(opts.target)).flatten() {
        return complain(why);
    }
    if let Err(why) = link::preflight(opts.target, link) {
        return complain(why);
    }
    let linker = match link::find(opts.target, link) {
        Ok(linker) => linker,
        Err(why) => return complain(why),
    };
    // Whether the one that was found can do this link is asked inside the search, which moves on
    // past an lld that is too old to a newer one somewhere else and refuses only when there is none.
    // The glibc stubs, which are the one part of a cross sysroot written here rather than fetched.
    // Before compiling for the same reason as the rest, and never for `-###`, which writes nothing.
    if let Err(why) = link::write_stubs(opts.target, link) {
        return complain(why);
    }

    let scratch = match Scratch::new() {
        Ok(scratch) => scratch,
        Err(why) => return complain(format!("could not make a place for the object files: {why}")),
    };

    let fs = OsFileSystem::new();
    let mut failed = false;
    // One per job, in job order, which is what lets the link line below be rebuilt with the real
    // paths in it: every job contributes exactly one file to the line and does so in this order.
    let mut produced: Vec<String> = Vec::with_capacity(plan.jobs.len());
    let mut fired = Fired::new();
    let mut pressure = Pressure::new();
    let mut lowerings = Lowerings::new();
    {
        let mut stderr = host::stderr();
        let (mut remarks, ok) = Remarks::new(opts.opt_info_file.as_ref(), &mut stderr);
        failed |= !ok;
        for (at, job) in plan.jobs.iter().enumerate() {
            let out = match &job.output {
                Output::Temporary(hint) => {
                    // The index because two inputs in different directories can have the same
                    // name, and the two objects of `rucc a/x.c b/x.c` must not be one file.
                    scratch.dir.join(format!("{at}-{hint}")).display().to_string()
                }
                Output::File(path) => path.clone(),
                // A job feeding the linker never writes to standard output, since the plan gives
                // it a temporary. This is here so that the match is total rather than a panic.
                Output::Stdout => continue,
            };
            produced.push(out.clone());
            if !job.phases.contains(&Phase::Compile) && !needs_an_assembler(job) {
                continue;
            }
            let started = std::time::Instant::now();
            let result = if needs_an_assembler(job) {
                assemble(opts, &job.input, assembly_wants_cpp(job), &fs)
            } else if job.kind == InputKind::Ir {
                compile_ir(opts, &job.input, &fs)
            } else {
                compile(&counted(opts, job), &job.input, &fs)
            };
            if opts.time {
                say_time(&job.input, started.elapsed(), &mut stderr);
            }
            failed |= !write_trace(opts, job, started, &result, &mut stderr);
            fired.merge(&result.fired);
            pressure.merge(&result.pressure);
            lowerings.merge(&result.lowerings);
            failed |= !write_dumps(&job.input, &result.dumps, &mut stderr);
            failed |= !remarks.write(&result.remarks, &mut stderr);
            for message in &result.messages {
                let _ = writeln!(stderr, "{message}");
            }
            failed |= !write_temps(job, &result.temps, &mut stderr);
            failed |= !write_stack_usage(job, &result.stack_usage, &mut stderr);
            failed |= !write_note(job, &result.note, &mut stderr);
            if result.failed() {
                failed = true;
                // gcc and clang leave the rule behind for a file that got through the preprocessor and
                // then failed, and kbuild reads it after a command it expects to fail.
                if opts.deps.emit && result.preprocessed {
                    let _ = write_deps(opts, plan, job, &result.deps, &mut stderr);
                }
                continue;
            }
            // A `-MD` on a command line that links writes the rule next to the executable and
            // names the executable as its target, since that is the file this source builds
            // here. The object it went through is in a temporary directory and is gone by the
            // time `make` reads any of this.
            if opts.deps.emit {
                failed |= !write_deps(opts, plan, job, &result.deps, &mut stderr);
            }
            if !matches!(result.artifact, Artifact::Object { .. }) {
                // Worth saying rather than writing whatever it is and letting the linker read it.
                // An empty file is a valid empty linker script, so a link handed one gets as far
                // as reporting every symbol of this file undefined, which is a page of messages
                // about something that went wrong here.
                let _ = writeln!(
                    stderr,
                    "rucc: internal error: {}: no object file was produced for the link",
                    job.input
                );
                failed = true;
                continue;
            }
            if let Err(e) = std::fs::write(&out, result.artifact.bytes()) {
                let _ = writeln!(stderr, "rucc: error: {out}: {e}");
                failed = true;
            }
        }
        failed |= !write_coverage(opts, &fired, &mut stderr);
        failed |= !write_pressure(opts, &pressure, &mut stderr);
        failed |= !write_lowering(opts, &lowerings, &mut stderr);
        failed |= !write_lowering(opts, &lowerings, &mut stderr);
    }
    if failed {
        // Nothing is linked from a compilation that did not finish. A linker run over the objects
        // that did compile would report every function of the file that did not as undefined,
        // which is a page of messages about a mistake already reported once.
        return 1;
    }

    // The items in command line order with the temporaries filled in. A library and a word for the
    // linker contribute no job and pass through, and every file item takes the next job's real
    // output, which is what keeps whatever was written between two objects between them here.
    let mut outputs = produced.into_iter();
    let mut items = Vec::with_capacity(job.inputs.len());
    for item in &job.inputs {
        match item {
            link::Item::Library(name) => items.push(link::Item::Library(name.clone())),
            link::Item::Linker(arg) => items.push(link::Item::Linker(arg.clone())),
            link::Item::File(_) => match outputs.next() {
                Some(path) => items.push(link::Item::File(path)),
                None => return complain("the plan asks the linker for a file nothing produced"),
            },
        }
    }

    // What `-flto` asks of the link: the objects that kept their module give way to one object
    // made from all of them together. One that cannot be made is said, and the objects are linked
    // as they are, which is the program without the work across files and nothing worse.
    if opts.lto.requested {
        let started = std::time::Instant::now();
        let joined = lto::link(opts, link.shared, &items, &scratch.dir);
        let mut stderr = host::stderr();
        match joined {
            Ok(Some(joined)) => items = joined,
            Ok(None) => {}
            Err(why) => {
                let _ = writeln!(
                    stderr,
                    "rucc: warning: -flto: {why}, so the objects are linked as they are"
                );
            }
        }
        if opts.time {
            say_time("lto", started.elapsed(), &mut stderr);
        }
    }

    let args = match link::line(opts.target, link, &items, &job.output) {
        Ok(args) => args,
        Err(why) => return complain(why),
    };
    if verbose {
        let mut stderr = host::stderr();
        let _ = writeln!(stderr, "{}", link::render(&linker, &args));
    }
    let started = std::time::Instant::now();
    let ran = link::run(&linker, &args);
    if opts.time {
        // The one step of a compilation that really is another program, so this line is the same
        // measurement gcc's is and names the linker the way gcc names `collect2`.
        let mut stderr = host::stderr();
        say_time(&linker.name, started.elapsed(), &mut stderr);
    }
    match ran {
        Ok(()) => 0,
        // The linker has already said what was wrong on its own error output, and repeating that
        // linking failed would only push its message further up the screen. A note after it is
        // different, when the link went ahead without our runtime, since that is the cause the
        // linker cannot see.
        Err(link::Error::Refused { .. }) => {
            if let Some(note) = link::missing_builtins(opts.target, link) {
                let mut stderr = host::stderr();
                let _ = writeln!(stderr, "rucc: note: {note}");
            }
            1
        }
        Err(why) => complain(why),
    }
}

/// Compiles everything and writes the objects into one static library.
///
/// No temporary directory and no second program. The objects never reach the file system at all:
/// they go from the compiler into the archive writer, which is both faster than writing a directory
/// of files for an `ar` to read back and the reason the symbol index can be written at all. A
/// member's index entries are the names the object writer says it wrote, and the only thing that
/// knows those is the run that wrote it.
///
/// `-save-temps` is the exception. It asked for the objects to be kept, the plan gave them names a
/// person can find, and they are written there as well as put in the archive.
fn archive_all(opts: &Options, plan: &Plan) -> i32 {
    let Some(job) = &plan.archive else {
        // Every path into here comes from a plan whose last phase is the archive, and such a plan
        // has an archive job. Saying so is cheaper than an unwrap that would have to be explained.
        return complain("there is nothing to put in an archive");
    };
    // Before anything is compiled, because a format this has no container for is worth knowing
    // about in the second it takes to look rather than after the whole compilation.
    let flavour = match opts.target.object_format() {
        ObjectFormat::Elf => rucc_archive::Flavour::Gnu,
        ObjectFormat::Coff => rucc_archive::Flavour::Coff,
        ObjectFormat::MachO => rucc_archive::Flavour::Bsd,
        // Wasm has no archive format of its own. wasm-ld reads the GNU one with its symbol index,
        // which is what `llvm-ar` writes for wasm members too.
        ObjectFormat::Wasm => rucc_archive::Flavour::Gnu,
    };

    let fs = OsFileSystem::new();
    let mut failed = false;
    let mut members: Vec<rucc_archive::Member> = Vec::with_capacity(plan.jobs.len());
    let mut names = job.members.iter();
    let mut fired = Fired::new();
    let mut pressure = Pressure::new();
    let mut lowerings = Lowerings::new();
    {
        let mut stderr = host::stderr();
        let (mut remarks, ok) = Remarks::new(opts.opt_info_file.as_ref(), &mut stderr);
        failed |= !ok;
        for plan_job in &plan.jobs {
            // What the plan called this member. The two lists are walked together rather than the
            // name being worked out again here, so that what `-###` printed and what goes in the
            // file cannot come apart.
            let Some(member) = names.next() else {
                return complain("the plan asks the archive for a member nothing produced");
            };
            if !plan_job.phases.contains(&Phase::Compile) && !needs_an_assembler(plan_job) {
                // Neither something to compile nor something to assemble, so there is nothing to
                // put in, and an archive quietly missing a member is worse than a message.
                let _ = writeln!(
                    &mut stderr,
                    "rucc: error: {}: this compiler makes an archive out of what it compiles, and \
                     there is nothing here for it to do",
                    plan_job.input
                );
                failed = true;
                continue;
            }
            let started = std::time::Instant::now();
            let result = if needs_an_assembler(plan_job) {
                assemble(opts, &plan_job.input, assembly_wants_cpp(plan_job), &fs)
            } else if plan_job.kind == InputKind::Ir {
                compile_ir(opts, &plan_job.input, &fs)
            } else {
                compile(&counted(opts, plan_job), &plan_job.input, &fs)
            };
            if opts.time {
                say_time(&plan_job.input, started.elapsed(), &mut stderr);
            }
            failed |= !write_trace(opts, plan_job, started, &result, &mut stderr);
            fired.merge(&result.fired);
            pressure.merge(&result.pressure);
            lowerings.merge(&result.lowerings);
            failed |= !write_dumps(&plan_job.input, &result.dumps, &mut stderr);
            failed |= !remarks.write(&result.remarks, &mut stderr);
            for message in &result.messages {
                let _ = writeln!(stderr, "{message}");
            }
            failed |= !write_temps(plan_job, &result.temps, &mut stderr);
            failed |= !write_stack_usage(plan_job, &result.stack_usage, &mut stderr);
            failed |= !write_note(plan_job, &result.note, &mut stderr);
            if result.failed() {
                failed = true;
                // gcc and clang leave the rule behind for a file that got through the preprocessor and
                // then failed, and kbuild reads it after a command it expects to fail.
                if opts.deps.emit && result.preprocessed {
                    let _ = write_deps(opts, plan, plan_job, &result.deps, &mut stderr);
                }
                continue;
            }
            if opts.deps.emit {
                failed |= !write_deps(opts, plan, plan_job, &result.deps, &mut stderr);
            }
            let Artifact::Object { bytes, defines } = result.artifact else {
                let _ = writeln!(
                    stderr,
                    "rucc: internal error: {}: no object file was produced for the archive",
                    plan_job.input
                );
                failed = true;
                continue;
            };
            // Under `-save-temps` the plan gave the object a name a person can find, so it is
            // written there too. Otherwise it is only ever a member and never a file.
            if let Output::File(path) = &plan_job.output {
                if let Err(e) = std::fs::write(path, &bytes) {
                    let _ = writeln!(stderr, "rucc: error: {path}: {e}");
                    failed = true;
                }
            }
            members.push(rucc_archive::Member { name: member.clone(), body: bytes, defines });
        }
        failed |= !write_coverage(opts, &fired, &mut stderr);
        failed |= !write_pressure(opts, &pressure, &mut stderr);
        failed |= !write_lowering(opts, &lowerings, &mut stderr);
        failed |= !write_lowering(opts, &lowerings, &mut stderr);
    }
    if failed {
        // Nothing is written from a compilation that did not finish, for the reason the link gives:
        // an archive missing the file that failed is one a link reports every name of as undefined,
        // which is a page of messages about a mistake already reported once.
        return 1;
    }

    let bytes = match rucc_archive::write(flavour, &members) {
        Ok(bytes) => bytes,
        // Every one of these is a bug here rather than a program's mistake: the names came from the
        // object writer and the bodies came from this process.
        Err(why) => return complain(format!("the archive could not be written: {why}")),
    };
    match std::fs::write(&job.output, &bytes) {
        Ok(()) => 0,
        Err(e) => complain(format!("{}: {e}", job.output)),
    }
}

/// Prints one driver level message and gives back the exit status that goes with it.
fn complain(why: impl std::fmt::Display) -> i32 {
    let mut stderr = host::stderr();
    let _ = writeln!(stderr, "rucc: error: {why}");
    1
}

/// Writes what `-Zrule-coverage=FILE` asked for, and says whether it could.
///
/// Once for the whole command line rather than once per input, because the question is which
/// lowering rules this run of the compiler reached and a file per input would leave the reader
/// unioning files to find out something one process already knew.
///
/// A file that could not be written is a failure and not a warning. What asks for this is a
/// measurement run, and a measurement that quietly did not happen is worse than one that stopped.
fn write_coverage(opts: &Options, fired: &Fired, stderr: &mut impl std::io::Write) -> bool {
    let Some(path) = &opts.rule_coverage else { return true };
    let Some(table) = coverage::table(opts.target.arch) else {
        let _ = writeln!(
            stderr,
            "rucc: error: there are no lowering rules for {} yet, so there is no coverage of them \
             to report",
            opts.target
        );
        return false;
    };
    match std::fs::write(path, fired.listing(table)) {
        Ok(()) => true,
        Err(e) => {
            let _ = writeln!(stderr, "rucc: error: {path}: {e}");
            false
        }
    }
}

/// Writes what `-Zregister-pressure=FILE` asked for, and says whether it could.
///
/// Once for the whole command line, for the reason [`write_coverage`] gives, and a file that could
/// not be written is a failure for the reason it gives too. There is no equivalent of the missing
/// rule table here, since every target this compiles for has an allocator, and a run that reached
/// no back end at all writes an empty listing rather than nothing: a measurement of a build that
/// produced no code is still an answer and it is the honest one.
fn write_pressure(opts: &Options, pressure: &Pressure, stderr: &mut impl std::io::Write) -> bool {
    let Some(path) = &opts.register_pressure else { return true };
    match std::fs::write(path, pressure.listing()) {
        Ok(()) => true,
        Err(e) => {
            let _ = writeln!(stderr, "rucc: error: {path}: {e}");
            false
        }
    }
}

/// Writes what `-Zlowering=FILE` asked for, and says whether it could.
///
/// Once for the whole command line, for the reason [`write_coverage`] gives, and a file that could
/// not be written is a failure for the reason it gives too. A run that reached no back end writes
/// an empty listing rather than nothing, the way [`write_pressure`] does and for the same reason.
fn write_lowering(opts: &Options, lowerings: &Lowerings, stderr: &mut impl std::io::Write) -> bool {
    let Some(path) = &opts.lowering_dump else { return true };
    match std::fs::write(path, lowerings.listing()) {
        Ok(()) => true,
        Err(e) => {
            let _ = writeln!(stderr, "rucc: error: {path}: {e}");
            false
        }
    }
}

/// Where the `-fopt-info` remarks go, and how much of the run has already gone there.
///
/// Standard error by default, and one file for the whole run when `-fopt-info=<file>` named one.
/// A file rather than the diagnostic stream is what a harness wants: the corpus in
/// `tamnd/rucc-corpus` matches a rejection against what the compiler said on standard error, and
/// a few thousand remarks mixed into that would bury it.
struct Remarks {
    /// The file, if there is one.
    file: Option<String>,
    /// Whether anything has been written to it yet, which decides between truncating and
    /// appending. One file holds the whole run rather than the last input in it.
    started: bool,
}

impl Remarks {
    /// Prepares the destination, emptying the file if there is one.
    ///
    /// Emptied here rather than at the first remark, because a run where no pass had anything to
    /// say should leave an empty file and not yesterday's. An absent file and an empty one are
    /// different facts and something reading this will act on the difference.
    fn new(file: Option<&String>, stderr: &mut impl std::io::Write) -> (Self, bool) {
        let mut ok = true;
        if let Some(path) = file {
            if let Err(e) = std::fs::write(path, "") {
                let _ = writeln!(stderr, "rucc: error: {path}: {e}");
                ok = false;
            }
        }
        (Self { file: file.cloned(), started: false }, ok)
    }

    /// Writes one input's remarks, and says whether that worked.
    ///
    /// A file that cannot be written is a failure and not a warning, for the reason
    /// [`write_dumps`] gives: remarks that quietly did not arrive look exactly like a compilation
    /// where nothing happened.
    fn write(&mut self, text: &str, stderr: &mut impl std::io::Write) -> bool {
        if text.is_empty() {
            return true;
        }
        let Some(path) = &self.file else {
            let _ = write!(stderr, "{text}");
            return true;
        };
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .append(self.started)
            .truncate(!self.started)
            .create(true)
            .open(path);
        self.started = true;
        let result =
            opened.and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes()));
        if let Err(e) = result {
            let _ = writeln!(stderr, "rucc: error: {path}: {e}");
            return false;
        }
        true
    }
}

/// Writes what `-fdump-ir=` asked to see, one file per dump.
///
/// The name is the input file with the dump's own name and `.ir` after it, so a directory listing
/// after a run is the passes in the order they ran, per input. They go in the working directory
/// rather than beside the output, because a dump is something a person asked for at a prompt and
/// the working directory is where that person is.
///
/// A file that could not be written is a failure and not a warning, for the reason
/// [`write_coverage`] gives: what asked for this is somebody debugging a pass, and a dump that
/// quietly did not happen looks exactly like a pass that did not run.
fn write_dumps(input: &str, dumps: &[rucc_opt::Dump], stderr: &mut impl std::io::Write) -> bool {
    let stem = std::path::Path::new(input)
        .file_name()
        .map_or_else(|| input.to_owned(), |name| name.to_string_lossy().into_owned());
    let mut ok = true;
    for dump in dumps {
        let path = format!("{stem}.{}.ir", dump.name);
        if let Err(e) = std::fs::write(&path, &dump.text) {
            let _ = writeln!(stderr, "rucc: error: {path}: {e}");
            ok = false;
        }
    }
    ok
}

/// Writes the files `-save-temps` kept, which is nothing at all unless it was given.
///
/// A file that could not be written is a failure rather than a warning, for the reason
/// [`write_dumps`] gives: somebody asked for these by name, and one that quietly did not happen
/// looks like a compilation that never went through that step.
fn write_temps(job: &Job, temps: &Temps, stderr: &mut impl std::io::Write) -> bool {
    let mut ok = true;
    let kept = [(job.saved_text(), &temps.preprocessed), (job.saved_asm(), &temps.assembly)];
    for (path, text) in kept {
        // A step the compilation did not reach has nothing to keep, and a job that is not keeping
        // that step has nowhere to put it. Either way there is no file here.
        let (Some(path), Some(text)) = (path, text) else { continue };
        if let Err(e) = std::fs::write(&path, text) {
            let _ = writeln!(stderr, "rucc: error: {path}: {e}");
            ok = false;
        }
    }
    ok
}

/// Writes the `.su` file `-fstack-usage` asked for, where the plan said it goes.
///
/// Written even when it is empty, because gcc writes an empty `.su` for a file with no functions,
/// for `-fsyntax-only` and for a file that did not compile, and a tool that looks for one beside
/// every object should find one.
/// The options one job compiles with, which are the line's own unless `-fprofile-arcs` gave the
/// job a `.gcda` to name.
///
/// The name goes into the object, so it is made absolute here the way gcc makes it: against the
/// working directory, or with `-fprofile-dir=` the whole path with its slashes turned to `#` under
/// that directory, so that two objects of one name in different places keep apart.
fn counted<'a>(opts: &'a Options, job: &Job) -> std::borrow::Cow<'a, Options> {
    let Some(counts) = &job.counts else { return std::borrow::Cow::Borrowed(opts) };
    let cwd = std::env::current_dir().unwrap_or_default();
    let full = cwd.join(counts);
    let path = match &opts.profile_data.dir {
        Some(dir) => {
            let dir = cwd.join(dir);
            dir.join(folded(&full.display().to_string())).display().to_string()
        }
        None => full.display().to_string(),
    };
    let mut opts = opts.clone();
    opts.profile_data.counts = Some(path);
    std::borrow::Cow::Owned(opts)
}

/// A whole path folded into one name for `-fprofile-dir=`, with every separator turned to `#`.
///
/// On Windows that is the backslash too, and the colon after the drive letter is turned to `~`,
/// which is how gcc spells it on a DOS file system. Without that the name is still absolute, and
/// joining it to the directory gives back the name alone.
fn folded(path: &str) -> String {
    if cfg!(windows) {
        path.replace(['/', '\\'], "#").replacen(':', "~", 1)
    } else {
        path.replace('/', "#")
    }
}

/// Writes the `.gcno` file `-ftest-coverage` asked for, unless the compilation stopped before
/// there was a graph to describe, which gcc leaves no file for either.
fn write_note(job: &Job, bytes: &[u8], stderr: &mut impl std::io::Write) -> bool {
    let Some(path) = job.note.as_ref().filter(|_| !bytes.is_empty()) else { return true };
    if let Err(e) = std::fs::write(path, bytes) {
        let _ = writeln!(stderr, "rucc: error: {path}: {e}");
        return false;
    }
    true
}

fn write_stack_usage(job: &Job, text: &str, stderr: &mut impl std::io::Write) -> bool {
    let Some(path) = &job.stack_usage else { return true };
    if let Err(e) = std::fs::write(path, text) {
        let _ = writeln!(stderr, "rucc: error: {path}: {e}");
        return false;
    }
    true
}

/// Appends the file's line to the `-frucc-trace` file, when there is one.
///
/// Returns whether that went well, and says why on standard error when it did not.
fn write_trace(
    opts: &Options,
    job: &Job,
    started: std::time::Instant,
    result: &Compiled,
    stderr: &mut impl std::io::Write,
) -> bool {
    let Some(path) = &opts.trace else {
        return true;
    };
    let output = match &job.output {
        Output::Stdout => "-",
        Output::File(path) | Output::Temporary(path) => path,
    };
    let record = trace::Record {
        input: &job.input,
        output,
        ok: !result.failed(),
        total: started.elapsed(),
        timing: &result.timing,
    };
    match trace::append(path, &record) {
        Ok(()) => true,
        Err(e) => {
            let _ = writeln!(stderr, "rucc: error: {e}");
            false
        }
    }
}

/// One line of `-time`, which is what a step was called and how long it took.
///
/// GCC's two numbers are the user and the system time of a subprocess it ran. This compiler runs
/// no subprocess for anything but the link, so what is measured here is the wall clock of the
/// step and the second column is always zero. The shape of the line is kept because a person
/// reading it next to gcc's should not have to work out which column is which.
fn say_time(name: &str, took: std::time::Duration, stderr: &mut impl std::io::Write) {
    let _ = writeln!(stderr, "# {name} {:.2} {:.2}", took.as_secs_f64(), 0.0);
}

/// Writes one job's result where the plan said it goes.
///
/// # Errors
///
/// Returns the message to print, which names the file when there is one, because "permission
/// denied" on its own does not say which file was refused.
fn write_out(output: &Output, bytes: &[u8]) -> Result<(), String> {
    match output {
        Output::Stdout => {
            let mut stdout = host::stdout();
            stdout.write_all(bytes).map_err(|e| format!("writing to standard output: {e}"))
        }
        Output::File(path) | Output::Temporary(path) => {
            std::fs::write(path, bytes).map_err(|e| format!("{path}: {e}"))
        }
    }
}

/// The target a program name asks for, the way `aarch64-linux-gnu-gcc` is gcc for that target.
///
/// `program` is the path the compiler was started as. The name without its directory and without a
/// trailing `.exe` has to end in `-rucc`, `-gcc`, `-gcc-<version>` or `-cc`, and what comes before
/// that has to read as a target, or there is no answer and the name means nothing. A link named
/// `my-rucc` or `ccache-gcc` is therefore just rucc and not an error.
///
/// The gcc spellings are there for cross builds that put a prefix in front of `gcc`, which is how
/// the Linux kernel's `CROSS_COMPILE=aarch64-linux-gnu-` reaches the compiler, and Debian installs
/// the same compiler again as `aarch64-linux-gnu-gcc-14`. The prefix is read with the target model
/// that knows every architecture a triple can name, not only the ones rucc generates code for, so
/// `i686-linux-gnu-gcc` implies `--target=i686-linux-gnu` and that is refused by name. Compiling
/// for the host instead would hand a 32 bit build a 64 bit object with no word said about it.
pub fn target_from_program(program: &str) -> Option<String> {
    let name = program.rsplit(['/', '\\']).next()?;
    let name = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name);
    let triple = name
        .strip_suffix("-rucc")
        .or_else(|| name.strip_suffix("-gcc"))
        .or_else(|| name.strip_suffix("-cc"))
        .or_else(|| {
            let (front, version) = name.rsplit_once('-')?;
            let versioned = !version.is_empty()
                && version.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && version.as_bytes()[0].is_ascii_digit();
            front.strip_suffix("-gcc").filter(|_| versioned)
        })?;
    triple.parse::<TargetTuple>().ok()?;
    Some(triple.to_owned())
}

/// [`run`] for a compiler started as `program`, which is `argv[0]`.
///
/// A target taken from the name goes in front of `args`, so a `--target=` written on the command
/// line comes later and wins, which is what gcc and clang do with a prefixed name.
///
/// A name ending in `dlltool`, or `--dlltool` as the first argument, is [`dlltool::run`] instead,
/// which writes an import library and compiles nothing.
pub fn run_as(program: &str, args: &[String]) -> i32 {
    if dlltool::is_dlltool(program) {
        return dlltool::run(program, args);
    }
    if args.first().is_some_and(|first| first == "--dlltool") {
        return dlltool::run(program, &args[1..]);
    }
    match target_from_program(program) {
        Some(triple) => {
            let mut all = Vec::with_capacity(args.len() + 1);
            all.push(format!("--target={triple}"));
            all.extend_from_slice(args);
            run(&all)
        }
        None => run(args),
    }
}

/// What `--version` prints.
///
/// Without a claimed GCC release the first line is ours and is the one every harness we have
/// reads. With `-fgnuc-version=` it has the shape of GCC's, `gcc (<build>) <version>`, with this
/// compiler named in the brackets where a distribution names its build. That is for the builds
/// that tell GCC from other compilers by this line: the Linux kernel from 4.18 to 5.11 sets
/// `CC_IS_GCC` from `grep gcc` on it, and every kernel copies it into `CONFIG_CC_VERSION_TEXT`. A
/// build that asks for the claim gets it in the banner too, and a build that did not ask still
/// sees `rucc`. The second line is for build systems that decide what kind of compiler they have
/// by reading this text. Meson takes the GNU path only when it finds "Free Software Foundation"
/// here, and otherwise stops with "Unknown compiler" before it has asked a single question. Past
/// that point meson reads the version from `__GNUC__` and asks the preprocessor everything else,
/// so the line decides the path and nothing more. It says what is true, that rucc speaks the
/// dialect of that GCC release. `spec/04-driver-and-cli.md` section 4.5 has the rest.
fn banner(opts: &Options) -> String {
    let gnuc = opts.gnuc;
    let first = if opts.gnuc_given {
        format!("gcc (rucc {VERSION}, GNU C persona {gnuc}) {gnuc}")
    } else {
        format!("rucc {VERSION}")
    };
    format!(
        "{first}\nA C compiler for the GNU C dialect of GCC {} from the Free Software Foundation.\nThis is free software under the Apache License 2.0. There is NO warranty.",
        gnuc.major
    )
}

/// What `-v` prints first, on standard error, in the shape of `gcc -v`.
///
/// zlib, libtool and OpenSSL read this text to decide if the compiler is GCC. They look for the
/// word `gcc`, and some read the `Target:` and `Thread model:` lines. So the first line names this
/// compiler and the GCC release that `__GNUC__` claims, and the lines after it are the ones that
/// GCC and clang both print. tamnd/rucc#3277.
fn verbose_banner(opts: &Options) -> String {
    let installed = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_default();
    let threads = if opts.target.arch.is_wasm() { "single" } else { "posix" };
    let mut text = format!(
        "rucc version {VERSION} (gcc version {} compatible)\nTarget: {}\nThread model: {threads}\nInstalledDir: {}\n",
        opts.gnuc,
        opts.target.tuple().to_canonical_string(),
        installed.display()
    );
    // The line clang prints for each file it read flags from.
    text.extend(opts.config_files.iter().map(|file| format!("Configuration file: {file}\n")));
    text
}

/// The first line of what gas prints for `--version`, which is all of what this compiler prints.
///
/// gas writes `GNU assembler (GNU Binutils) 2.44` and then a copyright and a licence, and what
/// reads it is the Linux kernel's `scripts/as-version.sh`, which takes the first line, wants its
/// first two words to be `GNU assembler` and takes the last word as the version. The part in
/// brackets is where a distribution names its build, so it is where this names itself. One line
/// rather than gas's six, since the rest is gas's licence and not ours.
fn gas_banner(version: rucc_session::GasVersion) -> String {
    format!("GNU assembler (rucc {VERSION} integrated) {version}")
}

/// What the words a build handed the assembler come to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Assembler {
    /// `--version`, which prints the assembler's banner instead of assembling anything.
    version: bool,
    /// `--fatal-warnings`, which makes the assembler's one warning an error.
    fatal_warnings: bool,
    /// `--noexecstack`, which marks the stack of a file of assembly as not executable.
    noexecstack: bool,
    /// `-mrelax-relocations=no`, which keeps the linker from rewriting a read of a slot of the
    /// global offset table.
    keep_slots: bool,
}

/// The words of every `-Wa,` and `-Xassembler`, each one honored, taken because it describes what
/// the assembler inside this compiler already does, or refused by name.
///
/// There is no separate assembler here, but builds pass these as if there were, and the Linux
/// kernel passes a good many. Refusing the ones that are not on this list matters as much as taking
/// the ones that are: kbuild's `as-option` and `cc-option` find out whether an assembler takes an
/// option by passing it and looking at the exit status, so an option taken and ignored is a
/// feature switched on that the output does not have. Each word is paired with the argument it came
/// from, so that the refusal names both. `spec/04-driver-and-cli.md` section 4.9 has the list and
/// why each entry is on it.
fn assembler_words(words: &[(String, String)], target: Triple) -> Result<Assembler, CliError> {
    let mut out = Assembler::default();
    let i386 = target.arch == rucc_target::Arch::X86;
    let x86 = i386 || target.arch == rucc_target::Arch::X86_64;
    let aarch64 = target.arch == rucc_target::Arch::Aarch64;
    let mut words = words.iter();
    while let Some((word, from)) = words.next() {
        let refuse = |why: &str| err(format!("`{word}` in `{from}`: {why}"));
        match word.as_str() {
            "--version" => out.version = true,
            "--fatal-warnings" => out.fatal_warnings = true,
            // The marker that says the stack is not executable, for a file of assembly that did not
            // write one. An object compiled from C has it anyway. Mach-O and COFF have no marker
            // and a stack that is not executable unless the link says otherwise.
            "--noexecstack" => out.noexecstack = true,
            // The note gas writes on x86 with the instruction sets and features a file used. This
            // compiler never writes it, so asking for it not to be written asks for what happens.
            "-mx86-used-note=no" if x86 => {}
            // Whether the linker may rewrite an instruction that reads a slot of the global offset
            // table, which gas says with the relocation it picks. The kernel turns it off for the
            // decompressor, which is linked without anything to relax against.
            "-mrelax-relocations=no" if x86 => out.keep_slots = true,
            "-mrelax-relocations=yes" if x86 => out.keep_slots = false,
            // The word size gas assembles for, which is the machine the target already is. The
            // other one is a different target, which `-m32` or `-m64` is how to ask for.
            "--64" if x86 && !i386 => {}
            "--32" if i386 => {}
            "--32" | "--64" if x86 => {
                return Err(refuse(
                    "the assembler inside this compiler assembles for the target, so the other \
                     word size is `-m32` or `-m64` on the compiler's own command line",
                ));
            }
            // The processor gas picks the padding between instructions for. Every name here is
            // padded with instructions that every one of them runs, which on i386 are gas's own
            // `generic32` ones and on x86-64 its long nops, so a name it knows changes nothing.
            // The i386 kernel passes `generic32` so that padding stays valid on a 486.
            _ if x86
                && word.strip_prefix("-mtune=").is_some_and(|name| {
                    matches!(name, "generic32" | "generic64" | "i386" | "i486" | "i586" | "i686")
                }) => {}
            // The data model gas assembles for on AArch64, where LP64 is the only one this compiler
            // has.
            "-mabi=lp64" if aarch64 => {}
            // A directory for `.include` and `.incbin`. The assembler reads no file but its input,
            // and refuses both directives, so a place to look for one changes nothing.
            "-I" => {
                if words.next().is_none() {
                    return Err(refuse("-I requires a directory"));
                }
            }
            _ if word.starts_with("-I") => {}
            // The architecture gas takes instructions from. The kernel passes `armv8.4-a` or
            // `armv8.5-a` on AArch64 so that the assembler takes instructions the compiler must not
            // generate, and this assembler takes every instruction it can encode whatever
            // architecture is named, so a name it knows is taken. On x86 gas's names are processors
            // rather than the compiler's levels and they narrow what it takes, which this assembler
            // cannot do.
            _ if word.starts_with("-march=") => {
                let name = &word["-march=".len()..];
                let arch = name.split_once('+').map_or(name, |(arch, _)| arch);
                if !aarch64 || rucc_target::Isa::aarch64_arch(arch).is_none() {
                    return Err(refuse(
                        "the assembler inside this compiler takes every instruction it can encode \
                         and cannot be narrowed to a processor, and on AArch64 it takes the \
                         armv8 and armv9 architecture names",
                    ));
                }
            }
            // Line tables for a file of assembly, which gas writes from the source lines when the
            // file has no `.loc` of its own. This assembler writes no debug information for a file
            // of assembly at all, so the option is refused rather than taken and not done.
            _ if word.starts_with("-gdwarf") || word.starts_with("--gdwarf") || word == "-g" => {
                return Err(refuse(
                    "the assembler inside this compiler writes no debug information for a file of \
                     assembly, so it cannot write the line table this asks for",
                ));
            }
            _ => {
                return Err(refuse(
                    "the assembler is inside this compiler, and this is not one of the options it \
                     takes, see spec/04-driver-and-cli.md section 4.9",
                ));
            }
        }
    }
    Ok(out)
}

/// Writes what a printing action answers to stdout. A reader that stops early closes the pipe, as
/// kbuild's `$(CC) --version | head -n 1` does, and that is not a failure: `print!` would panic
/// there and leave a backtrace in the build log.
fn emit(text: &str) {
    let _ = host::stdout().write_all(text.as_bytes());
}

/// Sets what `--param` said, before anything reads it.
///
/// The values are the process's and not the compilation's, since the passes that read them run on
/// as many threads as `-j` asks for, so a command line with no `--param` leaves every one where the
/// last command line in this process put it. The binary runs one command line, and the tests that
/// set one run the binary.
fn set_params(params: &[String]) {
    for spec in params {
        match rucc_cost::heuristics::Param::new(spec) {
            Ok(param) => param.set(),
            Err(_) => unreachable!("--param {spec} was checked when the arguments were read"),
        }
    }
}

/// Runs the driver and returns the process exit code.
///
/// `args` excludes the program name. Output goes to `stdout` and errors to `stderr`, which
/// is the one place in the compiler that is true.
pub fn run(args: &[String]) -> i32 {
    match parse_args(args) {
        Ok(Action::Help) => {
            emit(USAGE);
            0
        }
        Ok(Action::Print(line)) => {
            emit(&format!("{line}\n"));
            0
        }
        Ok(Action::Verbose(text)) => {
            let _ = write!(host::stderr(), "{text}");
            0
        }
        Ok(Action::PrintConfig(opts)) => {
            emit(&print_config(&opts));
            0
        }
        Ok(Action::PrintPipeline(opts)) => {
            emit(&print_pipeline(&opts));
            0
        }
        Ok(Action::PrintParams(params)) => {
            set_params(&params);
            emit(&rucc_cost::heuristics::listing());
            0
        }
        Ok(Action::PrintPlan { opts, plan, link }) => {
            emit(&plan.render());
            // The line as it would be typed, which is the half of `-###` that section 4.3 says
            // arrives with the link. It is printed even when the linker is not on this machine,
            // because what a build wants from `-###` is what the compiler would do.
            if let Some(job) = &plan.link {
                match link_line(&opts, &link, job) {
                    Ok(line) => emit(&format!("{line}\n")),
                    Err(why) => {
                        let mut stderr = host::stderr();
                        let _ = writeln!(stderr, "rucc: error: {why}");
                        return 1;
                    }
                }
            }
            0
        }
        Ok(Action::Fetch { what, target, cache }) => {
            let kernel = rucc_sysroot::Kernel::for_target(&cache, target)
                .map(|_| &rucc_sysroot::KERNEL_HEADERS);
            fetch_sysroot(what, kernel, target, &cache)
        }
        Ok(Action::FetchMsvcSdk { target, accepted, cache, pinned }) => msvc::fetch_msvc_sdk(
            target,
            accepted,
            &cache,
            pinned.then_some(&rucc_sysroot::PINNED_BUILD),
        ),
        Ok(Action::Compile { opts, plan, link, jobs, verbose, notes }) => {
            set_params(&opts.params);
            {
                let mut stderr = host::stderr();
                // Before the plan rather than after it, because a note is about the command line
                // and the plan is what the command line was read as, so the reader wants the two
                // in that order.
                for note in &notes {
                    let _ = writeln!(stderr, "rucc: warning: {note}");
                }
                if verbose {
                    let _ = write!(stderr, "{}", verbose_banner(&opts));
                    let _ = write!(stderr, "{}", plan.render());
                    let _ = writeln!(stderr, "workers: {}", jobs.count());
                    // What `gcc -v` says about headers, because meson and cmake read it to find the
                    // system directories.
                    let _ = write!(stderr, "{}", opts.search.render_gcc());
                }
            }
            if opts.emit == EmitKind::Preprocessed {
                return preprocess_all(&opts, &plan);
            }
            if opts.emit == EmitKind::Archive {
                return archive_all(&opts, &plan);
            }
            if opts.emit != EmitKind::Executable {
                return compile_all(&opts, &plan);
            }
            if let Some(why) = unlinkable(&opts) {
                let _ = writeln!(host::stderr(), "rucc: error: {why}");
                return 1;
            }
            link_all(&opts, &plan, &link, verbose)
        }
        Err(e) => {
            let mut stderr = host::stderr();
            let _ = writeln!(stderr, "rucc: error: {e}");
            let _ = writeln!(stderr, "rucc: note: run `rucc --help` for usage");
            1
        }
    }
}

/// Why a link that was asked for cannot be made, when that is known before anything is compiled.
///
/// The checked modes need `runtime/rucc-safe-rt` in the program, and that runtime is written
/// against Unix: shadow memory through `mmap`, reports through a signal handler, and the maps read
/// out of `/proc`. There is no Windows build of it, so a Windows program compiled with one used to
/// fail at the link with a page of undefined `__rucc_check_` names. Saying so before the link
/// is kinder until the port is done. Only the link is refused: an object or a listing built
/// with the checks in is still what was asked for, and is what a test of the instrumentation reads.
fn unlinkable(opts: &Options) -> Option<String> {
    (opts.safety.instruments() && opts.target.os == rucc_target::Os::Windows).then(|| {
        format!(
            "-fsafety={}: the checked modes are not available on a Windows target yet, because \
             the runtime they need has not been ported to Windows. -c still builds the object",
            opts.safety
        )
    })
}

#[cfg(test)]
mod tests {
    use rucc_session::{
        Compress, Contract, GnucVersion, IncludeForm, LtoJobs, OptLevel, Partition, Patchable,
        Visibility,
    };

    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    /// A target to write down where the host would otherwise decide, for the tests whose answer
    /// would be a different one on a different machine.
    ///
    /// Most of the tests here never name a target, which is right, because most of what the driver
    /// does with a command line is the same wherever it runs and a test that pinned one would be
    /// saying so in every case for the sake of the two that need it. The two that need it are the
    /// ones whose answer comes off the target rather than off the command line: the name an object
    /// gets, which is `a.o` here and `a.obj` on Windows, and whether Microsoft's reading of a
    /// nameless member is on, which is off here and on there. Both are the compiler being right, and
    /// a test that leaves the target to the host is asking a question with two correct answers.
    const LINUX: &str = "--target=x86_64-unknown-linux-gnu";

    #[test]
    fn a_response_file_is_split_the_way_libiberty_splits_one() {
        let words = response_words("-Wl,--as-needed  'a b' \"c d\"\ne\\ f '' \"it's\" g\\\\h\n");
        assert_eq!(words, ["-Wl,--as-needed", "a b", "c d", "e f", "", "it's", "g\\h"]);
        assert!(response_words(" \n\t").is_empty());
    }

    #[test]
    fn a_response_file_on_the_command_line_is_read_in_its_place() {
        let dir = std::env::temp_dir().join(format!("rucc-rsp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let inner = dir.join("inner.rsp");
        std::fs::write(&inner, "-lm\n").unwrap();
        let outer = dir.join("outer.rsp");
        // Backslashes doubled, because the file is split the way libiberty splits one and a
        // Windows path is full of them.
        let named = inner.display().to_string().replace('\\', "\\\\");
        std::fs::write(&outer, format!("-o 'my prog' -Wl,--as-needed @{named}\n")).unwrap();
        let line = args(&["x.o", &format!("@{}", outer.display()), "@no-such-file"]);
        assert_eq!(
            response_files(&line).unwrap(),
            args(&["x.o", "-o", "my prog", "-Wl,--as-needed", "-lm", "@no-such-file"])
        );
        let itself = dir.join("itself.rsp");
        let named = itself.display().to_string().replace('\\', "\\\\");
        std::fs::write(&itself, format!("@{named}")).unwrap();
        let looped = response_files(&args(&[&format!("@{}", itself.display())]));
        assert!(looped.is_err(), "a file that names itself should be refused");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dlltool_mode_is_asked_for_first_or_by_the_program_name() {
        let dir = std::env::temp_dir().join(format!("rucc-dlltool-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let def = dir.join("w.def");
        std::fs::write(&def, "LIBRARY w.dll\nEXPORTS\nw\n").unwrap();
        let def = def.display().to_string();
        let first = dir.join("first.a").display().to_string();
        let named = dir.join("named.a").display().to_string();
        let line = args(&["--dlltool", "-m", "i386:x86-64", "-d", &def, "-l", &first]);
        assert_eq!(run_as("rucc", &line), 0);
        // The prefix says the machine, as it does for a prefixed GNU dlltool.
        let line = args(&["-d", &def, "-l", &named]);
        assert_eq!(run_as("/opt/bin/x86_64-w64-mingw32-dlltool", &line), 0);
        assert_eq!(std::fs::read(&first).unwrap(), std::fs::read(&named).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();

        let later = parse_args(&args(&["x.c", "--dlltool", "-d", "x.def"])).unwrap_err();
        assert!(later.to_string().contains("first argument"), "{later}");
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        assert_eq!(parse_args(&args(&["-c", "--help", "x.c"])).unwrap(), Action::Help);
        let banner = printed(&["--version"]);
        assert!(banner.starts_with("rucc "), "{banner}");
        assert_eq!(printed(&["-c", "--version", "x.c"]), banner);
    }

    fn compile(s: &[&str]) -> (Box<Options>, Box<Plan>) {
        match parse_args(&args(s)).expect("expected a compilation") {
            Action::Compile { opts, plan, .. } => (opts, plan),
            other => panic!("expected a compilation, got {other:?}"),
        }
    }

    fn linking(s: &[&str]) -> (Box<LinkOptions>, Box<Plan>) {
        match parse_args(&args(s)).expect("expected a compilation") {
            Action::Compile { link, plan, .. } => (link, plan),
            other => panic!("expected a compilation, got {other:?}"),
        }
    }

    fn notes(s: &[&str]) -> Vec<String> {
        match parse_args(&args(s)).expect("expected a compilation") {
            Action::Compile { notes, .. } => notes,
            other => panic!("expected a compilation, got {other:?}"),
        }
    }

    /// The ordinary command line has nothing to say about itself, which is the property that makes
    /// a note worth reading when there is one.
    #[test]
    fn a_command_line_with_nothing_wrong_with_it_carries_no_notes() {
        assert_eq!(notes(&["-c", "a.c"]), Vec::<String>::new());
    }

    /// `-g` for Windows is kept, and says nothing, now that the COFF writer has DWARF sections.
    #[test]
    fn debug_information_for_coff_is_kept() {
        assert_eq!(
            notes(&["--target=x86_64-windows-gnu", "-g", "-c", "a.c"]),
            Vec::<String>::new()
        );
        let (opts, _) = compile(&["--target=x86_64-windows-gnu", "-g", "-c", "a.c"]);
        assert!(opts.debug_info);
    }

    /// A directory that is not there contributes nothing to the search path, so there is no tree to
    /// read a release out of and nothing to compare the pin against. Said as a test because this is
    /// the shape a hermetic machine takes: the probe reads the disk and every other machine has a
    /// different disk, so what can be asserted here is the silence.
    #[test]
    fn a_named_tree_that_is_not_on_the_machine_is_not_a_release_mismatch() {
        let said =
            notes(&["--target=x86_64-linux-gnu.2.28", "--sysroot=/nowhere-at-all", "-c", "a.c"]);
        assert_eq!(said, Vec::<String>::new());
    }

    #[test]
    fn collects_inputs_and_flags() {
        let (opts, plan) = compile(&["-c", "-O2", "-g", "a.c", "b.c"]);
        let paths: Vec<&str> = plan.jobs.iter().map(|j| j.input.as_str()).collect();
        assert_eq!(paths, vec!["a.c", "b.c"]);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.emit, EmitKind::Object);
        assert!(opts.debug_info);
    }

    /// The unstable options, which are spelled apart from everything else on purpose: what is
    /// under `-Z` promises nothing, and a build that reaches for one should have had to say so.
    #[test]
    fn an_unstable_option_is_taken_and_one_that_does_not_exist_is_refused() {
        let (opts, _) = compile(&["-c", "-Zrule-coverage=/tmp/rules.cov", "a.c"]);
        assert_eq!(opts.rule_coverage.as_deref(), Some("/tmp/rules.cov"));

        let (plain, _) = compile(&["-c", "a.c"]);
        assert_eq!(plain.rule_coverage, None, "nothing is measured unless it was asked for");

        assert!(parse_args(&args(&["-Zrule-coverage=", "a.c"])).is_err(), "a file with no name");
        let unknown = parse_args(&args(&["-Zwhat", "a.c"])).expect_err("there is no such option");
        assert!(unknown.message.contains("4.11"), "{}", unknown.message);
    }

    /// The other measurement written to a file, which reads the same way and fails the same way.
    #[test]
    fn where_the_register_pressure_goes_is_asked_for_the_same_way() {
        let (opts, _) = compile(&["-c", "-O2", "-Zregister-pressure=/tmp/spills.txt", "a.c"]);
        assert_eq!(opts.register_pressure.as_deref(), Some("/tmp/spills.txt"));

        let (plain, _) = compile(&["-c", "a.c"]);
        assert_eq!(plain.register_pressure, None, "nothing is measured unless it was asked for");

        assert!(parse_args(&args(&["-Zregister-pressure=", "a.c"])).is_err(), "no file named");
    }

    /// Which rewriter the optimizer runs, asked for by name.
    #[test]
    fn the_rewriter_is_asked_for_by_name() {
        let (opts, _) = compile(&["-c", "-O2", "-Zrewriter=consed", "a.c"]);
        assert_eq!(opts.rewriter, "consed");
        let (opts, _) = compile(&["-c", "-O2", "-Zrewriter=classical", "a.c"]);
        assert_eq!(opts.rewriter, "classical");
        let (plain, _) = compile(&["-c", "-O2", "a.c"]);
        assert_eq!(plain.rewriter, "", "the level decides unless it was asked for");
        let (opts, _) = compile(&["-c", "-O2", "-Zrewriter=egraph", "a.c"]);
        assert_eq!(opts.rewriter, "egraph");
        assert!(parse_args(&args(&["-Zrewriter=saturated", "a.c"])).is_err(), "not one there is");
    }

    /// Which register allocator runs, asked for by name, and left to the level when it is not.
    #[test]
    fn the_register_allocator_is_asked_for_by_name() {
        let (opts, _) = compile(&["-c", "-O2", "-Zregalloc=backtracking", "a.c"]);
        assert_eq!(opts.backtracking, Some(true));
        let (opts, _) = compile(&["-c", "-O2", "-Zregalloc=single", "a.c"]);
        assert_eq!(opts.backtracking, Some(false));
        let (plain, _) = compile(&["-c", "-O2", "a.c"]);
        assert_eq!(plain.backtracking, None, "the level decides unless it was asked for");
        assert!(parse_args(&args(&["-Zregalloc=graph", "a.c"])).is_err(), "not an allocator");
    }

    /// The third one, which says what the pre-selection lowering group did.
    #[test]
    fn a_switch_shape_is_forced_by_name_and_only_by_one_it_has() {
        let (opts, _) = compile(&["-c", "-O2", "-Zswitch=walk", "a.c"]);
        assert_eq!(opts.switch_shape.as_deref(), Some("walk"));
        let (plain, _) = compile(&["-c", "-O2", "a.c"]);
        assert_eq!(plain.switch_shape, None, "nothing is forced unless it was asked for");
        assert!(parse_args(&args(&["-Zswitch=bit-test", "a.c"])).is_err(), "not a shape it forces");
    }

    #[test]
    fn where_the_lowering_dump_goes_is_asked_for_the_same_way() {
        let (opts, _) = compile(&["-c", "-O2", "-Zlowering=/tmp/lowering.txt", "a.c"]);
        assert_eq!(opts.lowering_dump.as_deref(), Some("/tmp/lowering.txt"));

        let (plain, _) = compile(&["-c", "a.c"]);
        assert_eq!(plain.lowering_dump, None, "nothing is dumped unless it was asked for");

        assert!(parse_args(&args(&["-Zlowering=", "a.c"])).is_err(), "no file named");
    }

    /// Scheduling, which has the three way answer every optimization flag has: on, off, and
    /// nothing said, which is whatever the optimization level asks for. The name is gcc's, and
    /// gcc's has a two in it because gcc has a scheduler before allocation and one after and this
    /// is the one after.
    #[test]
    fn scheduling_can_be_turned_on_and_off_and_left_to_the_optimization_level() {
        let (on, _) = compile(&["-c", "-O0", "-fschedule-insns2", "a.c"]);
        assert_eq!(on.schedule_insns, Some(true));

        let (off, _) = compile(&["-c", "-O2", "-fno-schedule-insns2", "a.c"]);
        assert_eq!(off.schedule_insns, Some(false));

        let (quiet, _) = compile(&["-c", "-O2", "a.c"]);
        assert_eq!(quiet.schedule_insns, None, "nothing said, so the level decides");
        assert!(quiet.opt_level.schedules(), "and at this level the level says yes");

        let (none, _) = compile(&["-c", "a.c"]);
        assert!(!none.opt_level.schedules(), "at no optimization it says no");

        let (before, _) = compile(&["-c", "-O2", "-fno-schedule-insns", "a.c"]);
        assert_eq!(before.schedule_insns, None, "the scheduler before allocation is not this one");
    }

    /// Tail calls, which gcc spells as sibling calls and turns on at `-O2` and `-Os`.
    #[test]
    fn sibling_calls_can_be_turned_on_and_off_and_left_to_the_optimization_level() {
        let (on, _) = compile(&["-c", "-O1", "-foptimize-sibling-calls", "a.c"]);
        assert_eq!(on.sibling_calls, Some(true));

        let (off, _) = compile(&["-c", "-O2", "-fno-optimize-sibling-calls", "a.c"]);
        assert_eq!(off.sibling_calls, Some(false));

        let (quiet, _) = compile(&["-c", "-Os", "a.c"]);
        assert_eq!(quiet.sibling_calls, None, "nothing said, so the level decides");
        assert!(quiet.opt_level.sibling_calls(), "and at this level the level says yes");

        let (one, _) = compile(&["-c", "-O1", "a.c"]);
        assert!(!one.opt_level.sibling_calls(), "gcc leaves them off at -O1");
    }

    /// gcc's `tailr` is under the same flag, so the pass that makes a loop of a call a function
    /// makes to itself goes on and off with it, and a pass named on its own has the last word.
    #[test]
    fn the_sibling_calls_flag_turns_the_tail_recursion_pass_on_and_off() {
        let name = rucc_opt::tailrec::NAME;
        let listed = |args: &[&str]| print_pipeline(&compile(args).0).contains(name);
        assert!(listed(&["-c", "-O2", "a.c"]), "the level has it");
        assert!(!listed(&["-c", "-O2", "-fno-optimize-sibling-calls", "a.c"]));
        assert!(listed(&["-c", "-O1", "-foptimize-sibling-calls", "a.c"]));
        assert!(!listed(&["-c", "-O0", "-foptimize-sibling-calls", "a.c"]), "gcc runs none at -O0");
        let named = ["-c", "-O2", "-fno-optimize-sibling-calls", "-ftail-recursion", "a.c"];
        assert!(listed(&named), "the pass named on its own wins");
    }

    /// Whether the timing model is worth holding an instruction back over, which is a `-Z` because
    /// it is a question about a target's description rather than about the program being compiled.
    #[test]
    fn whether_the_timing_model_is_cycle_accurate_can_be_overridden() {
        let (yes, _) = compile(&["-c", "-O2", "-Zcycle-accurate-model=yes", "a.c"]);
        assert_eq!(yes.cycle_accurate_model, Some(true));

        let (no, _) = compile(&["-c", "-O2", "-Zcycle-accurate-model=no", "a.c"]);
        assert_eq!(no.cycle_accurate_model, Some(false));

        let (plain, _) = compile(&["-c", "-O2", "a.c"]);
        assert_eq!(plain.cycle_accurate_model, None, "the target's own answer stands");

        let bad = parse_args(&args(&["-Zcycle-accurate-model=maybe", "a.c"]))
            .expect_err("it takes yes or no");
        assert!(bad.message.contains("yes or no"), "{}", bad.message);
    }

    #[test]
    fn a_bare_dash_o_means_o1_the_way_gcc_reads_it() {
        let (opts, _) = compile(&["-O", "a.c"]);
        assert_eq!(opts.opt_level, OptLevel::O1);
    }

    #[test]
    fn dash_x_applies_to_later_inputs_only_and_none_stops_it() {
        let (_, plan) = compile(&["a.o", "-x", "c", "b.txt", "-x", "none", "c.o"]);
        assert_eq!(plan.jobs[0].kind, InputKind::LinkerInput);
        assert_eq!(plan.jobs[1].kind, InputKind::C);
        assert_eq!(plan.jobs[2].kind, InputKind::LinkerInput);
    }

    #[test]
    fn dash_x_can_be_joined_to_its_language() {
        let (_, plan) = compile(&["a.o", "-xc", "b.txt", "-xnone", "c.o"]);
        assert_eq!(plan.jobs[0].kind, InputKind::LinkerInput);
        assert_eq!(plan.jobs[1].kind, InputKind::C);
        assert_eq!(plan.jobs[2].kind, InputKind::LinkerInput);
    }

    #[test]
    fn dash_j_reaches_the_scheduler_and_defaults_to_the_machine() {
        let (_, _, jobs) = match parse_args(&args(&["-j4", "a.c"])).unwrap() {
            Action::Compile { opts, plan, jobs, .. } => (opts, plan, jobs),
            other => panic!("expected a compilation, got {other:?}"),
        };
        assert_eq!(jobs.count(), 4);

        let default = match parse_args(&args(&["a.c"])).unwrap() {
            Action::Compile { jobs, .. } => jobs,
            other => panic!("expected a compilation, got {other:?}"),
        };
        assert_eq!(default, Jobs::available());
        assert!(parse_args(&args(&["-j0", "a.c"])).is_err());
    }

    #[test]
    fn triple_hash_prints_the_plan_and_runs_nothing() {
        let a = parse_args(&args(&["-###", "-c", "a.c"])).unwrap();
        let Action::PrintPlan { plan, .. } = a else { panic!("expected a plan dump") };
        assert!(plan.render().contains("a.c: preprocess, compile, assemble -> a.o"));
    }

    #[test]
    fn the_flag_that_keeps_the_intermediate_files_has_three_spellings_and_two_meanings() {
        // The bare one is `=obj` and not `=cwd`. gcc's manual says the opposite and gcc 16 does
        // this, and following the compiler is what makes a build that reads either of them find
        // the files where they are.
        assert_eq!(compile(&["-c", "-save-temps", "a.c"]).0.save_temps, SaveTemps::Object);
        assert_eq!(compile(&["-c", "-save-temps=obj", "a.c"]).0.save_temps, SaveTemps::Object);
        assert_eq!(compile(&["-c", "-save-temps=cwd", "a.c"]).0.save_temps, SaveTemps::Cwd);
        assert_eq!(compile(&["-c", "a.c"]).0.save_temps, SaveTemps::No);
        // The last one on the line decides, the way it does for every other flag with an
        // argument, and a keyword that is neither is fatal rather than ignored: a run that kept
        // nothing and said nothing looks exactly like one where the files were not produced.
        let (opts, _) = compile(&["-c", "-save-temps", "-save-temps=cwd", "a.c"]);
        assert_eq!(opts.save_temps, SaveTemps::Cwd);
        let e = parse_args(&args(&["-c", "-save-temps=nowhere", "a.c"])).unwrap_err();
        assert!(e.message.contains("accepted: cwd, obj"), "{}", e.message);
    }

    #[test]
    fn the_flag_that_times_each_step_reaches_the_options_and_changes_nothing_else() {
        let (opts, plan) = compile(&["-c", "-time", "a.c"]);
        let (plain, without) = compile(&["-c", "a.c"]);
        assert!(opts.time);
        assert!(!plain.time);
        // Against the same line without the flag rather than against a spelling of the object's
        // name, since what the object is called is the host's business and this is not about that.
        assert_eq!(plan.jobs[0].output, without.jobs[0].output);
    }

    #[test]
    fn dash_x_names_what_it_accepts_when_it_does_not_know_a_language() {
        let e = parse_args(&args(&["-x", "fortran", "a.c"])).unwrap_err();
        assert!(e.message.contains("assembler-with-cpp"), "{}", e.message);
    }

    /// What `--fetch` says for a target this release pins nothing for, which today is every target
    /// but the three windows-gnu ones, the four musl ones, the eight glibc ones and the three WASI
    /// ones.
    #[test]
    fn a_fetch_of_a_target_nothing_is_pinned_for_says_so_rather_than_reaching_the_network() {
        let e = parse_args(&args(&["--fetch", "x86_64-linux-gnux32"])).unwrap_err();
        assert!(e.message.contains("pins no sysroot for x86_64-linux-gnux32"), "{}", e.message);
        // And what it does pin, because a release with some rows in the table and a release with
        // none are two situations and the second sentence is what tells them apart.
        assert!(e.message.contains("x86_64-windows-gnu"), "{}", e.message);
        // The joined spelling is the same flag.
        let joined = parse_args(&args(&["--fetch=x86_64-linux-gnux32"])).unwrap_err();
        assert_eq!(joined, e);
    }

    /// Each WASI row fetches its own archive, which `bin/wasi-sysroot` in `tamnd/rucc-cross` takes
    /// out of the wasi-sysroot of wasi-sdk 34. tamnd/rucc#2863.
    #[test]
    fn a_fetch_of_a_wasi_row_gets_the_archive_of_its_preview() {
        for tuple in ["wasm32-wasip1", "wasm32-wasip2", "wasm32-wasip3"] {
            match parse_args(&args(&["--fetch", tuple])) {
                Ok(Action::Fetch { what, target, .. }) => {
                    assert_eq!(what.tuple, tuple);
                    assert_eq!(target.to_canonical_string(), tuple);
                    assert_eq!(what.file_name(), format!("rucc-sysroot-{tuple}.tar.gz"));
                }
                other => panic!("{tuple}: {other:?}"),
            }
        }
        // wasm32-none has no C library, so there is nothing to fetch for it.
        let e = parse_args(&args(&["--fetch", "wasm32-none"])).unwrap_err();
        assert!(e.message.contains("pins no sysroot for wasm32-none"), "{}", e.message);
    }

    /// The two targets a release will never pin, which is a different answer from the one above.
    ///
    /// Section 13.4. A person who reads "this release pins no sysroot yet" waits for a release that
    /// does, and no release of this compiler can ship either of these. An Apple target gets the
    /// licence and what to do instead, and a Microsoft one gets Microsoft's own files from
    /// Microsoft, which is the one lawful download either wall has behind it.
    #[test]
    fn a_fetch_of_a_target_behind_a_licence_wall_says_so_rather_than_saying_not_yet() {
        let e = parse_args(&args(&["--fetch", "aarch64-macos"])).unwrap_err();
        assert!(e.message.contains("Xcode licence"), "{}", e.message);
        assert!(e.message.contains("there never will be"), "{}", e.message);
        assert!(!e.message.contains("tamnd/rucc-cross"), "{}", e.message);

        // Microsoft's side has a download behind it, which is Microsoft's own files from the build
        // this release pins, and the licence still has to be accepted for anything to move.
        let action = parse_args(&args(&["--fetch", "x86_64-windows-msvc"])).expect("pinned build");
        let Action::FetchMsvcSdk { target, accepted, pinned, .. } = action else {
            panic!("{action:?}")
        };
        assert_eq!(target.to_canonical_string(), "x86_64-windows-msvc");
        assert!(pinned);
        assert!(!accepted);
        let action = parse_args(&args(&["--fetch=aarch64-windows-msvc", "--accept-licence"]))
            .expect("pinned build");
        let Action::FetchMsvcSdk { accepted, pinned, .. } = action else { panic!("{action:?}") };
        assert!(accepted && pinned);
        // And the mingw-w64 target next to it is ours to ship and published, so the same flag has
        // something to get rather than a licence to explain.
        let action = parse_args(&args(&["--fetch", "x86_64-windows-gnu"])).expect("it is pinned");
        let Action::Fetch { what, .. } = action else { panic!("{action:?}") };
        assert_eq!(what.tuple, "x86_64-windows-gnu");
    }

    #[test]
    fn the_other_fetch_takes_a_target_behind_microsofts_wall_and_carries_the_acceptance() {
        // Both spellings of the flag, because a flag that takes a tuple gets written both ways.
        for line in [
            vec!["--fetch-msvc-sdk", "x86_64-windows-msvc"],
            vec!["--fetch-msvc-sdk=x86_64-windows-msvc"],
        ] {
            let action = parse_args(&args(&line)).expect("that is a target behind the wall");
            let Action::FetchMsvcSdk { target, accepted, pinned, .. } = action else {
                panic!("{action:?}")
            };
            assert_eq!(target.to_canonical_string(), "x86_64-windows-msvc");
            // Nothing on the line accepted anything, so nothing did.
            assert!(!accepted);
            // This one follows Microsoft's channel to whatever it names today.
            assert!(!pinned);
        }

        // And both spellings of the word, because the prose here uses one and most of the people
        // typing this will reach for the other.
        for word in ["--accept-licence", "--accept-license"] {
            let action = parse_args(&args(&["--fetch-msvc-sdk", "aarch64-windows-msvc", word]))
                .expect("that is a target behind the wall");
            let Action::FetchMsvcSdk { target, accepted, .. } = action else {
                panic!("{action:?}")
            };
            assert_eq!(target.to_canonical_string(), "aarch64-windows-msvc");
            assert!(accepted, "{word} should have been read");
        }
    }

    #[test]
    fn the_other_fetch_refuses_the_command_lines_that_do_not_mean_anything() {
        // A tuple is what it gets, so a flag with nothing after it is not a command.
        let e = parse_args(&args(&["--fetch-msvc-sdk"])).unwrap_err();
        assert!(e.message.contains("requires the target"), "{}", e.message);
        let e = parse_args(&args(&["--fetch-msvc-sdk", "not-a-target"])).unwrap_err();
        assert!(e.message.contains("there is no SDK to get"), "{}", e.message);

        // `--offline` forbids every download and this one asks for one, whichever order they came
        // in, which is the same answer `--fetch` gives.
        for line in [
            vec!["--offline", "--fetch-msvc-sdk", "x86_64-windows-msvc"],
            vec!["--fetch-msvc-sdk", "x86_64-windows-msvc", "--offline"],
        ] {
            let e = parse_args(&args(&line)).unwrap_err();
            assert!(e.message.contains("two opposite things"), "{}", e.message);
        }

        // It gets an SDK and compiles nothing, so a file on the same line would be read by nothing.
        let e = parse_args(&args(&["--fetch-msvc-sdk", "x86_64-windows-msvc", "a.c"])).unwrap_err();
        assert!(e.message.contains("compiles nothing"), "{}", e.message);

        // The two fetches are two commands and a line that asked for both asked for neither.
        let e = parse_args(&args(&[
            "--fetch",
            "x86_64-windows-gnu",
            "--fetch-msvc-sdk",
            "x86_64-windows-msvc",
        ]))
        .unwrap_err();
        assert!(e.message.contains("two different commands"), "{}", e.message);

        // And an acceptance with nothing to accept for is a command line that says something about
        // a licence no part of it goes near.
        let e = parse_args(&args(&["--accept-licence", "-c", "a.c"])).unwrap_err();
        assert!(e.message.contains("--fetch-msvc-sdk <tuple> for a"), "{}", e.message);
    }

    /// An Apple target on a machine with no SDK, which is section 8.6's other host.
    ///
    /// Not run on a mac, where the SDK this is about is installed and the compile is the ordinary one
    /// that uses it. What the reason says is asserted in `rucc_sysroot::wall` and where it is printed
    /// is asserted in `rucc-pp`, so what is left here is that the driver works it out and leaves it
    /// where the preprocessor will find it, and that neither way past the wall leaves one behind.
    #[test]
    fn an_apple_target_with_no_sdk_anywhere_carries_the_licence_rather_than_a_missing_directory() {
        if cfg!(target_os = "macos") || std::env::var_os("SDKROOT").is_some() {
            return;
        }
        let (opts, _) = compile(&["--target=aarch64-macos", "-c", "a.c"]);
        let why = opts.search.missing_system().expect("the wall is the reason there are none");
        assert!(why.contains("aarch64-macos needs a macOS SDK"), "{why}");
        assert!(why.contains("Xcode licence"), "{why}");
        assert!(why.contains("-isysroot"), "{why}");

        // A program that includes none of the library needs none of the SDK, which is what section
        // 8.6 means by being able to target the platform without one, so there is nothing to explain.
        let (opts, _) = compile(&["--target=aarch64-macos", "-nostdinc", "-c", "a.c"]);
        assert_eq!(opts.search.missing_system(), None);
        // And naming a path is the other way through, whether or not the path is there: a mistyped
        // directory is a mistake to report on its own terms rather than a licence to explain.
        let (opts, _) = compile(&["--target=aarch64-macos", "-isysroot", "/opt/sdk", "-c", "a.c"]);
        assert_eq!(opts.search.missing_system(), None);
    }

    /// The same wall on the compile side of an MSVC target, where the way past it is a tuple.
    ///
    /// Not run on Windows, for the same reason the one above is not run on a mac: the wall stands in
    /// front of an SDK this machine does not have, and a Windows machine is the kind that does. The
    /// driver asks `vswhere` where Visual Studio is and takes the newest kit under it, so on a box
    /// with the build tools installed there are headers, no wall and nothing here to be about.
    /// `INCLUDE` is the other way a machine has one and is the other half of the guard, since a
    /// person can set that anywhere while Visual Studio is only found on the platform it runs on.
    #[test]
    fn an_msvc_target_with_no_sdk_named_says_which_environment_needs_nothing_installed() {
        if cfg!(target_os = "windows") || std::env::var_os("INCLUDE").is_some() {
            return;
        }
        let (opts, _) = compile(&["--target=x86_64-windows-msvc", "-c", "a.c"]);
        let why = opts.search.missing_system().expect("the wall is the reason there are none");
        assert!(why.contains("the Windows SDK and its universal CRT"), "{why}");
        assert!(why.contains("mingw-w64"), "{why}");
        // And the mingw-w64 target has its headers from us, so nothing is missing to explain.
        let (opts, _) = compile(&["--target=x86_64-windows-gnu", "-c", "a.c"]);
        assert_eq!(opts.search.missing_system(), None);
    }

    #[test]
    fn a_fetch_with_no_target_and_a_fetch_of_a_tuple_that_is_not_one_both_say_which() {
        let e = parse_args(&args(&["--fetch"])).unwrap_err();
        assert!(e.message.contains("--fetch requires"), "{}", e.message);
        let e = parse_args(&args(&["--fetch", "sparc64-solaris-gnu"])).unwrap_err();
        assert!(e.message.contains("--fetch sparc64-solaris-gnu"), "{}", e.message);
        assert!(e.message.contains("no sysroot to get"), "{}", e.message);
    }

    /// Both flags on one line ask for opposite things, in either order.
    #[test]
    fn a_fetch_and_offline_together_is_a_refusal_whichever_way_round_they_are_written() {
        for line in [
            vec!["--offline", "--fetch", "x86_64-linux-musl"],
            vec!["--fetch", "x86_64-linux-musl", "--offline"],
        ] {
            let e = parse_args(&args(&line)).unwrap_err();
            assert!(e.message.contains("two opposite things"), "{}", e.message);
        }
    }

    #[test]
    fn a_fetch_does_not_compile_anything_and_says_so_when_it_is_handed_a_file() {
        let e = parse_args(&args(&["--fetch", "x86_64-linux-musl", "a.c"])).unwrap_err();
        assert!(e.message.contains("compiles nothing"), "{}", e.message);
        assert!(e.message.contains("a.c"), "{}", e.message);
    }

    /// `--offline` on its own is accepted and changes nothing, because an ordinary compile
    /// downloads nothing with or without it. A build that passes it everywhere is the case this is
    /// for, and it must not lose the compilation it was passed beside.
    #[test]
    fn offline_on_a_compilation_is_the_same_compilation() {
        let (opts, plan) = compile(&["-c", "--offline", "a.c"]);
        let (plain, without) = compile(&["-c", "a.c"]);
        assert_eq!(opts.target, plain.target);
        assert_eq!(plan.jobs.len(), without.jobs.len());
        assert_eq!(plan.jobs[0].output, without.jobs[0].output);
    }

    #[test]
    fn a_deployment_target_comes_from_the_tuple_or_from_the_flag() {
        let version = |v: &str| rucc_tuple::Version::parse(v);
        let (opts, _) = compile(&["--target=aarch64-macos.13", "-c", "a.c"]);
        assert_eq!(opts.target, "aarch64-apple-darwin".parse().unwrap());
        assert_eq!(opts.os_version, version("13"));
        // The flag wins over the tuple, as it does under clang, and either spelling of it works.
        let (opts, _) =
            compile(&["--target=aarch64-macos.13", "-mmacosx-version-min=14.2", "-c", "a.c"]);
        assert_eq!(opts.os_version, version("14.2"));
        let (opts, _) = compile(&["--target=x86_64-macos", "-mmacos-version-min=12", "-c", "a.c"]);
        assert_eq!(opts.os_version, version("12"));
        // Nothing said leaves the platform's default to the target description.
        let (opts, _) = compile(&["--target=aarch64-macos", "-c", "a.c"]);
        assert_eq!(opts.os_version, None);
        // A Linux build that always passes the flag is not an Apple build because of it.
        let (opts, _) =
            compile(&["--target=aarch64-linux-gnu", "-mmacosx-version-min=13", "-c", "a.c"]);
        assert_eq!(opts.os_version, None);
        let e = parse_args(&args(&["-mmacosx-version-min=thirteen", "a.c"])).unwrap_err();
        assert!(e.message.contains("is not a version"), "{}", e.message);
    }

    #[test]
    fn an_unknown_flag_is_an_error_rather_than_a_shrug() {
        let e = parse_args(&args(&["-fno-such-thing", "a.c"])).unwrap_err();
        assert!(e.message.contains("unknown option"), "{}", e.message);
    }

    /// `-fpermissive` and the flag that turns it back off, which a build writes beside it when
    /// one directory needs the older rules and the rest of the tree does not.
    #[test]
    fn permissive_reads_in_both_directions_and_the_last_one_wins() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.permissive, "off unless it is asked for");

        let (opts, _) = compile(&["-c", "-fpermissive", "a.c"]);
        assert!(opts.permissive);

        let (opts, _) = compile(&["-c", "-fpermissive", "-fno-permissive", "a.c"]);
        assert!(!opts.permissive);
    }

    #[test]
    fn asking_for_nested_functions_is_taken() {
        assert!(parse_args(&args(&["-fnested-functions", "a.c"])).is_ok());
        assert!(parse_args(&args(&["-fno-nested-functions", "a.c"])).is_ok());
    }

    #[test]
    fn the_flag_every_configure_script_writes_is_taken() {
        // All four spellings, because a build writes whichever one its macros picked and a
        // compiler that takes three of them is a compiler that fails on the fourth.
        for flag in ["-fPIC", "-fpic", "-fPIE", "-fpie"] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.emit, EmitKind::Object, "{flag}");
        }
    }

    #[test]
    fn a_table_is_written_unless_the_build_says_nothing_will_walk_it() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(opts.unwinds(), "the default is off");
        let (opts, _) = compile(&["-c", "-fno-asynchronous-unwind-tables", "a.c"]);
        assert!(!opts.unwinds(), "the build was not taken at its word");
        let (opts, _) = compile(&[
            "-c",
            "-fno-asynchronous-unwind-tables",
            "-fasynchronous-unwind-tables",
            "a.c",
        ]);
        assert!(opts.unwinds(), "the last flag did not win");
        // The weaker request, which the same table answers, so a line that asks for a table and
        // against an asynchronous one gets one. That is gcc's arrangement and it turns up when a
        // build turns the asynchronous one off globally and a directory asks for a table back.
        let (opts, _) =
            compile(&["-c", "-fno-asynchronous-unwind-tables", "-funwind-tables", "a.c"]);
        assert!(opts.unwinds(), "the weaker request was dropped");
        let (opts, _) = compile(&["-c", "-fno-unwind-tables", "a.c"]);
        assert!(opts.unwinds(), "the weaker negative turned off the stronger request");
        let (opts, _) =
            compile(&["-c", "-fno-unwind-tables", "-fno-asynchronous-unwind-tables", "a.c"]);
        assert!(!opts.unwinds(), "both were turned off and one stayed on");
    }

    #[test]
    fn the_flags_that_describe_what_this_compiler_already_does_are_taken() {
        // Every one of these is on a real build line somewhere and every one of them was an
        // unknown option. What they have in common is that the answer rucc gives is the answer
        // they ask for, so there is nothing to implement and nothing to refuse.
        for flag in [
            "-fstrict-aliasing",
            "-fno-strict-aliasing",
            "-fdelete-null-pointer-checks",
            "-fno-delete-null-pointer-checks",
            "-frounding-math",
            "-fno-rounding-math",
            "-fexcess-precision=standard",
            "-fexcess-precision=fast",
            "-fexcess-precision=16",
            "-pipe",
            "-cpp",
            "-fdiagnostics-color",
            "-fno-diagnostics-color",
            "-fdiagnostics-color=always",
            "-fdiagnostics-color=never",
            "-fdiagnostics-color=auto",
        ] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.emit, EmitKind::Object, "{flag}");
        }
    }

    #[test]
    fn whether_an_exception_is_looked_at_is_kept_and_defaults_to_gccs_answer() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(opts.trapping_math, "the default was not gcc's");
        let (opts, _) = compile(&["-c", "-fno-trapping-math", "a.c"]);
        assert!(!opts.trapping_math);
        let (opts, _) = compile(&["-c", "-ftrapping-math", "a.c"]);
        assert!(opts.trapping_math, "spelling out the default turned it off");
        // The last one written wins, which is how a build line that inherits a flag from one
        // place and overrides it in another is read.
        let (opts, _) = compile(&["-c", "-fno-trapping-math", "-ftrapping-math", "a.c"]);
        assert!(opts.trapping_math);
    }

    #[test]
    fn whether_the_rounding_mode_may_change_is_kept_and_fast_math_takes_it_back() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.rounding_math, "the default was not gcc's");
        let (opts, _) = compile(&["-c", "-frounding-math", "a.c"]);
        assert!(opts.rounding_math);
        let (opts, _) = compile(&["-c", "-frounding-math", "-fno-rounding-math", "a.c"]);
        assert!(!opts.rounding_math);
        let (opts, _) = compile(&["-c", "-frounding-math", "-ffast-math", "a.c"]);
        assert!(!opts.rounding_math, "fast math did not turn it off");
        let (opts, _) = compile(&["-c", "-ffast-math", "-frounding-math", "a.c"]);
        assert!(opts.rounding_math);
        let (opts, _) = compile(&["-c", "-frounding-math", "-fno-fast-math", "a.c"]);
        assert!(opts.rounding_math, "-fno-fast-math is not a word about the rounding mode");
    }

    /// The flags a torture program writes on its own `dg-options` line, which is where most of
    /// these come from: a program reduced from a miscompilation names the pass that miscompiled
    /// it. Eighteen programs in the suite stopped on the driver before anything read them, and
    /// tamnd/rucc#1019 is the list.
    #[test]
    fn no_inline_turns_off_the_inlining_of_a_function_declared_inline() {
        let (opts, _) = compile(&["-c", "-O2", "-fno-inline", "a.c"]);
        assert_eq!(opts.passes, [(rucc_opt::inline::NAME.to_owned(), false)]);
    }

    #[test]
    fn inlining_a_function_called_once_is_turned_off_and_on_by_its_own_flag() {
        for level in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Oz", "-Og"] {
            let (opts, _) = compile(&["-c", level, "-fno-inline-functions-called-once", "a.c"]);
            assert_eq!(opts.passes, [(rucc_opt::inline::ONCE.to_owned(), false)], "{level}");
            let (opts, _) = compile(&["-c", level, "-finline-functions-called-once", "a.c"]);
            assert_eq!(opts.passes, [(rucc_opt::inline::ONCE.to_owned(), true)], "{level}");
        }
    }

    #[test]
    fn the_flags_that_name_a_pass_of_gccs_own_are_taken_and_dropped() {
        for flag in [
            "-fno-tree-ccp",
            "-fno-tree-dominator-opts",
            "-fno-tree-vrp",
            "-fno-tree-bit-ccp",
            "-fno-tree-coalesce-vars",
            "-ftree-vectorize",
            "-ftree-loop-distribution",
            "-fipa-pta",
            "-fmodulo-sched",
            "-fno-vect-cost-model",
            "-fvect-cost-model=unlimited",
            "-fsimd-cost-model=cheap",
            "-fexpensive-optimizations",
            "-fno-early-inlining",
            "-finline-functions",
            "-foptimize-strlen",
            "-fno-ira-share-spill-slots",
            "-fno-schedule-insns",
            "-fschedule-insns",
            "-fno-code-hoisting",
            "-fno-gcse",
            "-fgcse",
            "-fno-tracer",
            "-ftracer",
            "-fsched-pressure",
            "-fno-sched-interblock",
            "-fsched-stalled-insns=2",
            "-fsched2-use-superblocks",
        ] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.emit, EmitKind::Object, "{flag}");
            assert!(opts.passes.is_empty(), "{flag} named a pass of gcc's and not one of ours");
        }
    }

    /// The two namespaces are taken whole, so a name neither this test nor gcc 16 has heard of
    /// goes the same way as the ones above rather than stopping a build on the day gcc adds it.
    #[test]
    fn a_pass_name_in_either_family_is_taken_whether_or_not_it_is_one_gcc_has() {
        for flag in ["-ftree-no-such-pass", "-fno-ipa-no-such-pass"] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.emit, EmitKind::Object, "{flag}");
        }
    }

    /// A pass this compiler has keeps its flag, since the arms that read the registry are above
    /// the family arms. `dce` is the one both compilers have a name for, and `execute/pr97421-2.c`
    /// is the program that writes it.
    #[test]
    fn a_pass_name_this_compiler_has_is_still_read_as_a_pass() {
        let (opts, _) = compile(&["-c", "-fno-dce", "a.c"]);
        assert_eq!(opts.passes, vec![("dce".to_owned(), false)]);
    }

    /// gcc's name for the unroller reaches the unroller, in both directions. libtommath puts
    /// `-funroll-loops` in `CFLAGS` unconditionally, and before this it was an unknown option and
    /// the build stopped on its first file.
    #[test]
    fn the_gcc_spelling_of_the_unroller_turns_the_unroller_on_and_off() {
        let (opts, _) = compile(&["-c", "-funroll-loops", "a.c"]);
        assert_eq!(opts.passes, vec![("unroll".to_owned(), true)]);
        let (opts, _) = compile(&["-c", "-fno-unroll-loops", "a.c"]);
        assert_eq!(opts.passes, vec![("unroll".to_owned(), false)]);
    }

    /// The three transformations that are a module at a time are named by a flag as well, even
    /// though none of them is a `rucc_opt::Pass` and so none is reached by the generic arms.
    ///
    /// A bisection over a miscompilation turns one thing off at a time, and a transformation with
    /// no spelling of its own cannot be the one turned off.
    #[test]
    fn the_transformations_that_are_not_passes_are_still_named_by_a_flag() {
        let (opts, _) = compile(&["-c", "-fno-ipa-cp", "-fipa-sra", "-fno-libcall", "a.c"]);
        assert_eq!(
            opts.passes,
            vec![
                (rucc_opt::ipcp::NAME.to_owned(), false),
                (rucc_opt::ipasra::NAME.to_owned(), true),
                (rucc_opt::libcall::NAME.to_owned(), false),
            ]
        );
        let (opts, _) = compile(&["-c", "-flibcall", "a.c"]);
        assert_eq!(opts.passes, vec![(rucc_opt::libcall::NAME.to_owned(), true)]);
    }

    /// Where a function starts is a question this compiler answers, so the flag that asks about it
    /// is answered rather than dropped. femtolisp's Makefile writes the bare form on every compile
    /// of the project, and before this it was an unknown option and the build stopped on its first
    /// file. The numbers are gcc 16's, read off `-S` on x86-64: nothing and the bare form both
    /// give `.p2align 4`, `=32` gives 5, `=3` gives 2, and the negative form gives `.align 8`.
    #[test]
    fn the_alignment_of_a_function_is_a_request_this_compiler_can_answer() {
        let (opts, _) = compile(&["-c", "-falign-functions", "a.c"]);
        assert_eq!(opts.align_functions, None, "the bare form asks for the default");

        let (opts, _) = compile(&["-c", "-falign-functions=32", "a.c"]);
        assert_eq!(opts.align_functions, Some(32));

        let (opts, _) = compile(&["-c", "-falign-functions=3", "a.c"]);
        assert_eq!(opts.align_functions, Some(4), "rounded up rather than refused");

        let (opts, _) = compile(&["-c", "-falign-functions=32:8", "a.c"]);
        assert_eq!(opts.align_functions, Some(32), "the boundary is the answerable half");

        for flag in ["-falign-functions=0", "-falign-functions=1"] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.align_functions, None, "{flag} means the default");
        }

        let (opts, _) = compile(&["-c", "-fno-align-functions", "a.c"]);
        assert_eq!(opts.align_functions, Some(8), "the smallest boundary the target has");

        // The last one on the line wins, which is how gcc reads a repeated flag.
        let (opts, _) = compile(&["-c", "-falign-functions=32", "-falign-functions", "a.c"]);
        assert_eq!(opts.align_functions, None);

        let e = parse_args(&args(&["-c", "-falign-functions=big", "a.c"])).unwrap_err();
        assert!(e.message.contains("number of bytes"), "{}", e.message);
    }

    /// The other three of the family are about padding inside a body, so none of them is about
    /// where a function starts. Every spelling of each, since a build writes whichever one its
    /// author typed.
    #[test]
    fn the_alignment_flags_about_the_inside_of_a_body_are_taken_and_say_nothing() {
        for flag in [
            "-falign-labels",
            "-falign-loops",
            "-falign-jumps",
            "-falign-loops=16",
            "-falign-labels=32",
            "-fno-align-loops",
            "-fno-align-labels",
            "-fno-align-jumps",
        ] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.emit, EmitKind::Object, "{flag}");
            assert_eq!(opts.align_functions, None, "{flag} is not about where a function starts");
        }
    }

    /// The loop flag in either direction is an answer, and a command line that wrote neither
    /// leaves the level to decide.
    #[test]
    fn the_loop_alignment_flag_is_answered_both_ways() {
        assert_eq!(compile(&["-c", "-O2", "a.c"]).0.align_loops, None);
        assert_eq!(compile(&["-c", "-O0", "-falign-loops", "a.c"]).0.align_loops, Some(true));
        assert_eq!(compile(&["-c", "-O2", "-fno-align-loops", "a.c"]).0.align_loops, Some(false));
        assert_eq!(compile(&["-c", "-falign-loops=32", "a.c"]).0.align_loops, None, "a number");
    }

    /// The encoding of the source is not a question about speed, so the one name that describes
    /// what the preprocessor does is taken and every other name is refused.
    #[test]
    fn the_input_charset_is_taken_when_it_names_the_one_that_is_read() {
        for flag in ["-finput-charset=utf-8", "-finput-charset=UTF-8", "-finput-charset=utf8"] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.emit, EmitKind::Object, "{flag}");
        }

        let e = parse_args(&args(&["-c", "-finput-charset=latin1", "a.c"])).unwrap_err();
        assert!(e.message.contains("latin1"), "{}", e.message);
        assert!(e.message.contains("UTF-8"), "what is read is worth saying: {}", e.message);
    }

    /// `-fnon-call-exceptions` turns exceptions on unless `-fexceptions` or `-fno-exceptions` was
    /// written, and the one written wins whichever side of it it is on, which is gcc 16's reading.
    #[test]
    fn exceptions_are_on_when_asked_for_and_non_call_ones_ask_unless_told_not_to() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.exceptions && !opts.non_call_exceptions, "gcc's default for C is off");
        let (opts, _) = compile(&["-c", "-fexceptions", "a.c"]);
        assert!(opts.exceptions && !opts.non_call_exceptions);
        let (opts, _) = compile(&["-c", "-fexceptions", "-fno-exceptions", "a.c"]);
        assert!(!opts.exceptions);
        let (opts, _) = compile(&["-c", "-fnon-call-exceptions", "a.c"]);
        assert!(opts.exceptions && opts.non_call_exceptions);
        for line in [
            ["-fno-exceptions", "-fnon-call-exceptions"],
            ["-fnon-call-exceptions", "-fno-exceptions"],
        ] {
            let (opts, _) = compile(&["-c", line[0], line[1], "a.c"]);
            assert!(!opts.exceptions && opts.non_call_exceptions, "{line:?}");
        }
        let (opts, _) =
            compile(&["-c", "-fnon-call-exceptions", "-fno-non-call-exceptions", "a.c"]);
        assert!(!opts.exceptions && !opts.non_call_exceptions);
        let (opts, _) = compile(&["-c", "-fno-delete-dead-exceptions", "a.c"]);
        assert_eq!(opts.emit, EmitKind::Object);
    }

    /// `-ffast-math` used to be refused beside it and is the family it names now, with each
    /// member settable on its own and the last word on each winning, which is gcc's reading.
    #[test]
    fn fast_math_is_the_family_it_names_and_the_last_word_on_each_member_wins() {
        let both = |line: &[&str]| {
            let (opts, _) = compile(&[&["-c"], line, &["a.c"]].concat());
            let (link, _) = linking(&[line, &["a.c"]].concat());
            (opts, link)
        };
        let (opts, link) = both(&[]);
        assert_eq!(opts.math, Math::default());
        assert!(opts.trapping_math);
        assert!(!link.fast_math);

        let (opts, link) = both(&["-ffast-math"]);
        assert!(opts.math.fast(opts.trapping_math), "{:?}", opts.math);
        assert!(!opts.trapping_math, "fast math turns trapping off");
        assert!(link.fast_math, "and it links the startup file");

        // Taking one member back leaves the rest, and the whole is not fast math any more.
        let (opts, link) = both(&["-ffast-math", "-fno-finite-math-only"]);
        assert!(!opts.math.finite_only);
        assert!(!opts.math.errno && !opts.math.signed_zeros && opts.math.reciprocal);
        assert!(!opts.math.fast(opts.trapping_math));
        assert!(link.fast_math, "gcc's spec reads the flag and not the fields");

        let (opts, _) = both(&["-ffast-math", "-ftrapping-math"]);
        assert!(opts.trapping_math);
        assert!(!opts.math.fast(opts.trapping_math));
        assert!(!opts.math.associative(opts.trapping_math));

        let (opts, link) = both(&["-ffast-math", "-fno-fast-math"]);
        assert_eq!(opts.math, Math::default());
        assert!(opts.trapping_math);
        assert!(!link.fast_math);

        // A member written alone is only that member.
        let (opts, link) = both(&["-fno-math-errno"]);
        assert_eq!(opts.math, Math { errno: false, ..Math::default() });
        assert!(opts.math.iec_559(opts.trapping_math), "errno is not an IEC 60559 question");
        assert!(!link.fast_math);

        let (opts, link) = both(&["-funsafe-math-optimizations"]);
        assert!(opts.math.unsafe_math && opts.math.associative(opts.trapping_math));
        assert!(opts.math.errno && !opts.math.finite_only);
        assert!(link.fast_math);
    }

    /// `-Ofast` is `-O3` with fast math as a default, which a later level and a
    /// `-fno-fast-math` on either side of it both take back.
    #[test]
    fn ofast_is_o3_with_fast_math_as_a_default_a_flag_can_take_back() {
        let both = |line: &[&str]| {
            let (opts, _) = compile(&[&["-c"], line, &["a.c"]].concat());
            let (link, _) = linking(&[line, &["a.c"]].concat());
            (opts, link)
        };
        let (opts, link) = both(&["-Ofast"]);
        assert_eq!(opts.opt_level, OptLevel::O3);
        assert!(opts.math.fast(opts.trapping_math));
        assert!(link.fast_math);

        for line in [&["-Ofast", "-O2"][..], &["-fno-fast-math", "-Ofast"]] {
            let (opts, _) = both(line);
            assert!(!opts.math.fast(opts.trapping_math), "{line:?}");
        }

        let (_, link) = both(&["-Ofast", "-mno-daz-ftz"]);
        assert_eq!(link.daz_ftz, Some(false));
    }

    /// `-finstrument-functions` used to be refused beside those two, and it is taken now that the
    /// hooks are called. The last of it and its negative is the one that counts, as with any pair.
    #[test]
    fn instrument_functions_is_taken_and_the_last_of_the_pair_wins() {
        let (opts, _) = compile(&["-c", "-finstrument-functions", "a.c"]);
        assert!(opts.instrument_functions);
        let (opts, _) =
            compile(&["-c", "-finstrument-functions", "-fno-instrument-functions", "a.c"]);
        assert!(!opts.instrument_functions);
    }

    #[test]
    fn a_tentative_definition_is_common_on_darwin_unless_told_otherwise() {
        // Two files each writing `int g;` link under `-fcommon` and do not without it, and Apple's
        // clang has it on where every other compiler rucc stands in for has it off.
        let (opts, _) = compile(&[LINUX, "-c", "a.c"]);
        assert!(!Session::new(*opts).common());

        let (opts, _) = compile(&["-c", "--target=aarch64-apple-darwin", "a.c"]);
        assert!(Session::new(*opts).common());

        let (opts, _) = compile(&[LINUX, "-c", "-fcommon", "a.c"]);
        assert!(Session::new(*opts).common());

        let (opts, _) =
            compile(&["-c", "--target=aarch64-apple-darwin", "-fcommon", "-fno-common", "a.c"]);
        assert!(!Session::new(*opts).common());
    }

    #[test]
    fn position_dependent_code_is_taken_in_every_spelling_gcc_has() {
        for flag in ["-fno-pic", "-fno-PIC", "-fno-pie", "-fno-PIE"] {
            assert_eq!(compile(&[flag, "a.c"]).0.pic, Pic::Absolute, "{flag}");
        }
    }

    #[test]
    fn the_kernel_code_model_is_taken_beside_position_dependent_code_on_x86_64_elf() {
        let (opts, _) = compile(&[LINUX, "-mcmodel=kernel", "-fno-PIE", "-c", "a.c"]);
        assert_eq!(opts.code_model, rucc_target::CodeModel::Kernel);
        // In either order, since the target and the link are settled after the loop.
        let (opts, _) = compile(&["-fno-pic", "-mcmodel=kernel", LINUX, "-c", "a.c"]);
        assert_eq!(opts.code_model, rucc_target::CodeModel::Kernel);
        // And the last one on the line counts.
        let (opts, _) = compile(&[LINUX, "-mcmodel=kernel", "-mcmodel=small", "-c", "a.c"]);
        assert_eq!(opts.code_model, rucc_target::CodeModel::Small);
        assert_eq!(compile(&[LINUX, "-c", "a.c"]).0.code_model, rucc_target::CodeModel::Small);
    }

    #[test]
    fn the_kernel_code_model_is_refused_where_it_cannot_be_true() {
        // gcc's words, and the default is a position independent executable here as it is on a
        // distribution's gcc.
        for line in [&[LINUX, "-mcmodel=kernel"][..], &[LINUX, "-mcmodel=kernel", "-fPIC"]] {
            let mut line = line.to_vec();
            line.extend(["-c", "a.c"]);
            let message = refused(&line);
            assert!(message.contains("code model kernel does not support PIC mode"), "{message}");
        }
        let arm =
            refused(&["--target=aarch64-linux-gnu", "-mcmodel=kernel", "-fno-pic", "-c", "a.c"]);
        assert!(arm.contains("no kernel code model"), "{arm}");
    }

    #[test]
    fn the_pic_and_pie_families_are_settled_the_way_gcc_settles_them() {
        let pic = |line: &[&str]| {
            let mut line = line.to_vec();
            line.push("a.c");
            compile(&line).0.pic
        };
        assert_eq!(pic(&[]), Pic::Executable);
        assert_eq!(pic(&["-fPIC"]), Pic::Library);
        assert_eq!(pic(&["-fpie"]), Pic::Executable);
        // A no only speaks for its own family, so a library asked for stays one.
        assert_eq!(pic(&["-fPIC", "-fno-pie"]), Pic::Library);
        assert_eq!(pic(&["-fPIE", "-fno-pic"]), Pic::Executable);
        // And the last yes wins over a no before it, in either family.
        assert_eq!(pic(&["-fno-pic", "-fPIC"]), Pic::Library);
        assert_eq!(pic(&["-fno-pie", "-fpie"]), Pic::Executable);
        assert_eq!(pic(&["-fPIC", "-fno-pic"]), Pic::Absolute);
        assert_eq!(pic(&["-fpie", "-fno-pie"]), Pic::Absolute);
        assert_eq!(pic(&["-fno-pie", "-fno-pic"]), Pic::Absolute);
        // A later yes in one family clears the other, as gcc's chain of negatives does.
        assert_eq!(pic(&["-fPIE", "-fPIC"]), Pic::Library);
        assert_eq!(pic(&["-fPIC", "-fPIE"]), Pic::Executable);
    }

    #[test]
    fn a_program_name_with_a_known_target_in_front_of_rucc_picks_that_target() {
        let t = |p: &str| target_from_program(p);
        assert_eq!(t("aarch64-linux-gnu-rucc").as_deref(), Some("aarch64-linux-gnu"));
        assert_eq!(t("/usr/bin/riscv64-linux-musl-rucc").as_deref(), Some("riscv64-linux-musl"));
        assert_eq!(t(r"C:\bin\x86_64-windows-gnu-rucc.exe").as_deref(), Some("x86_64-windows-gnu"));
        assert_eq!(t("rucc"), None);
        assert_eq!(t("/usr/local/bin/rucc"), None);
        assert_eq!(t("my-rucc"), None);
        assert_eq!(t("sparc64-linux-gnu-rucc"), None);
    }

    #[test]
    fn a_cross_gcc_name_picks_the_target_in_front_of_it() {
        let t = |p: &str| target_from_program(p);
        assert_eq!(t("aarch64-linux-gnu-gcc").as_deref(), Some("aarch64-linux-gnu"));
        assert_eq!(t("/usr/bin/x86_64-linux-gnu-gcc").as_deref(), Some("x86_64-linux-gnu"));
        assert_eq!(t("i686-linux-gnu-gcc").as_deref(), Some("i686-linux-gnu"));
        assert_eq!(t("aarch64-linux-gnu-gcc-14").as_deref(), Some("aarch64-linux-gnu"));
        assert_eq!(t("x86_64-linux-gnu-gcc-14.2").as_deref(), Some("x86_64-linux-gnu"));
        assert_eq!(t("riscv64-linux-gnu-cc").as_deref(), Some("riscv64-linux-gnu"));
        assert_eq!(t(r"C:\bin\x86_64-w64-mingw32-gcc.exe").as_deref(), Some("x86_64-w64-mingw32"));
        assert_eq!(t("gcc"), None);
        assert_eq!(t("ccache-gcc"), None);
        assert_eq!(t("x86_64-linux-gnu-gcc-ar"), None);
        assert_eq!(t("x86_64-linux-gnu-gcc-"), None);
        assert_eq!(t("sparc64-linux-gnu-gcc"), None);
    }

    #[test]
    fn a_cross_gcc_name_compiles_for_its_target_and_a_written_target_still_wins() {
        let target = |program: &str, line: &[&str]| {
            let mut all: Vec<String> = target_from_program(program)
                .map(|triple| format!("--target={triple}"))
                .into_iter()
                .collect();
            all.extend(args(line));
            match parse_args(&all) {
                Ok(Action::Compile { opts, .. }) => Ok(opts.target),
                Ok(_) => panic!("expected a compile"),
                Err(e) => Err(e.message),
            }
        };
        let aarch64: Triple = "aarch64-unknown-linux-gnu".parse().unwrap();
        let x86_64: Triple = "x86_64-unknown-linux-gnu".parse().unwrap();
        assert_eq!(target("aarch64-linux-gnu-gcc", &["-c", "a.c"]), Ok(aarch64));
        assert_eq!(target("x86_64-linux-gnu-gcc", &["-c", "a.c"]), Ok(x86_64));
        assert_eq!(
            target("aarch64-linux-gnu-gcc", &["--target=x86_64-linux-gnu", "-c", "a.c"]),
            Ok(x86_64)
        );
        let i686: Triple = "i686-unknown-linux-gnu".parse().unwrap();
        assert_eq!(target("i686-linux-gnu-gcc", &["-c", "a.c"]), Ok(i686));
    }

    #[test]
    fn a_host_that_is_not_a_target_starts_from_the_last_target_named() {
        let start = |line: &[&str]| starting_target(None, &args(line));
        let musl: Triple = "x86_64-linux-musl".parse().unwrap();
        let line = ["--target=aarch64-linux-gnu", "-c", "--target=x86_64-linux-musl", "a.c"];
        assert_eq!(start(&line), Ok(musl));
        assert_eq!(start(&["--target=aarch64-macos.13"]).unwrap().arch, rucc_target::Arch::Aarch64);
        assert_eq!(start(&["--fetch", "x86_64-linux-musl"]), Ok(musl));
        assert_eq!(start(&["--fetch=x86_64-linux-musl"]), Ok(musl));
        assert!(start(&["--target=sparc64-linux-gnu"]).unwrap_err().message.contains("sparc64"));
        let e = start(&["-c", "a.c"]).unwrap_err();
        assert!(e.message.contains("no --target was given"), "{}", e.message);
        let riscv: Triple = "riscv64-linux-gnu".parse().unwrap();
        assert_eq!(starting_target(Some(riscv), &args(&["--target=x86_64-linux-musl"])), Ok(riscv));
    }

    #[test]
    fn an_unsupported_target_names_itself() {
        let e = parse_args(&args(&["--target=sparc64-linux-gnu", "a.c"])).unwrap_err();
        assert!(e.message.contains("sparc64"), "{}", e.message);
    }

    #[test]
    fn no_inputs_is_an_error_but_print_config_needs_none() {
        assert!(parse_args(&args(&[])).is_err());
        assert!(matches!(parse_args(&args(&["--print-config"])), Ok(Action::PrintConfig(_))));
    }

    #[test]
    fn print_config_reports_the_target_it_was_given_not_the_host() {
        let a = parse_args(&args(&["--print-config", "--target=riscv64-linux-musl"])).unwrap();
        let Action::PrintConfig(opts) = a else { panic!("expected a configuration dump") };
        let text = print_config(&opts);
        assert!(text.contains("target: riscv64-unknown-linux-musl"), "{text}");
        assert!(text.contains("char-signed: false"), "{text}");
        assert!(text.contains("object-format: elf"), "{text}");
        assert!(text.contains("va-list: void-pointer"), "{text}");
        // RISC-V has a register file and this compiler has not written it down yet, and the
        // dump says which of those two it is rather than leaving the line out.
        assert!(text.contains("registers: none"), "{text}");
        assert!(text.contains("timing-model: none"), "{text}");
    }

    /// The model the schedule was chosen with, which is a receipt anybody comparing two runs of a
    /// benchmark needs: two numbers that disagree are usually two models and not two compilers.
    #[test]
    fn print_config_names_the_model_the_schedule_was_chosen_with() {
        let opts = Options::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let text = print_config(&opts);
        let line = text.lines().find(|l| l.starts_with("timing-model:")).expect("the model");
        assert!(line.contains("Skylake"), "{line}");
        assert!(line.contains("published"), "a sentence saying where it came from: {line}");
    }

    #[test]
    fn print_config_has_one_key_per_line_and_a_fixed_order() {
        let opts = Options::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let text = print_config(&opts);
        let keys: Vec<&str> =
            text.lines().map(|l| l.split(':').next().unwrap_or_default()).collect();
        assert_eq!(keys[0], "version");
        assert_eq!(keys[1], "target");
        assert_eq!(keys.len(), 26);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn the_safety_tier_is_read_off_the_command_line_and_a_wrong_one_is_refused() {
        let (opts, _) = compile(&["a.c"]);
        assert_eq!(opts.safety, rucc_session::Safety::Off);

        for (flag, tier) in [
            ("-fsafety=detect", rucc_session::Safety::Detect),
            ("-fsafety=enforce", rucc_session::Safety::Enforce),
            ("-fsafety=kernel", rucc_session::Safety::Kernel),
            ("-fsafety=off", rucc_session::Safety::Off),
        ] {
            let (opts, _) = compile(&[flag, "a.c"]);
            assert_eq!(opts.safety, tier, "{flag}");
        }

        // The last one wins, the way every other repeated flag on this command line does.
        let (opts, _) = compile(&["-fsafety=enforce", "-fsafety=off", "a.c"]);
        assert_eq!(opts.safety, rucc_session::Safety::Off);

        // A misspelled tier is refused rather than ignored. Silently compiling without the
        // monitor a build asked for is the one failure mode this feature cannot have.
        let e = parse_args(&args(&["-fsafety=on", "a.c"])).unwrap_err();
        assert!(e.message.contains("is not a safety tier"), "{}", e.message);
        assert!(parse_args(&args(&["-fsafety", "a.c"])).is_err());
    }

    /// A checked mode on a Windows target is refused at the link, with a message that says why,
    /// rather than left to fail there on names the runtime would have defined. The object is
    /// still built, and every other target links as before.
    #[test]
    fn a_checked_mode_on_windows_is_refused_at_the_link_and_nowhere_else() {
        let (opts, _) = compile(&["--target=x86_64-windows-gnu", "-fsafety=detect", "a.c"]);
        let why = unlinkable(&opts).expect("a refusal");
        assert!(why.contains("not available on a Windows target"), "{why}");
        let (opts, _) = compile(&["--target=x86_64-windows-gnu", "-fsafety=off", "a.c"]);
        assert_eq!(unlinkable(&opts), None);
        let (opts, _) = compile(&["--target=x86_64-linux-gnu", "-fsafety=detect", "a.c"]);
        assert_eq!(unlinkable(&opts), None);
    }

    /// A wasm target links, so nothing about it is refused before the compilation. What a row
    /// cannot link yet, a component or a shared library, is said by the line itself.
    #[test]
    fn a_wasm_target_is_not_refused_before_the_link() {
        for target in ["wasm32-wasip1", "wasm32-wasip2", "wasm32-unknown-unknown"] {
            let (opts, _) = compile(&[&format!("--target={target}"), "a.c"]);
            assert_eq!(unlinkable(&opts), None, "{target}");
        }
    }

    #[test]
    fn the_padding_mode_is_read_off_the_command_line_and_a_wrong_one_is_refused() {
        // The default is the one section 9.3 of document 09 gives library code, which is that
        // padding does not participate, so a record filled a member at a time is not reported.
        let (opts, _) = compile(&["a.c"]);
        assert_eq!(opts.padding, rucc_session::Padding::Ignored);

        let (opts, _) = compile(&["-fsafety=detect", "-fsafety-init=padding", "a.c"]);
        assert_eq!(opts.padding, rucc_session::Padding::Tracked);

        let (opts, _) = compile(&["-fsafety-init=padding", "-fsafety-init=nopadding", "a.c"]);
        assert_eq!(opts.padding, rucc_session::Padding::Ignored);

        // The tier is still a tier. A flag whose name starts the same way must not be eaten by
        // the one above it, which is the thing worth pinning about a pair of names like these.
        let (opts, _) = compile(&["-fsafety-init=padding", "a.c"]);
        assert_eq!(opts.safety, rucc_session::Safety::Off);

        let e = parse_args(&args(&["-fsafety-init=some", "a.c"])).unwrap_err();
        assert!(e.message.contains("is not a padding mode"), "{}", e.message);
    }

    #[test]
    fn whether_a_write_has_to_stay_inside_its_member_is_read_off_the_command_line() {
        // Off by default, because a store to allocated storage sets its effective type and C 6.5
        // lets a program reuse a buffer as something else. Row S4 is a build opting out of that.
        let (opts, _) = compile(&["a.c"]);
        assert_eq!(opts.subobject, rucc_session::Subobject::Off);

        let (opts, _) = compile(&["-fsafety=detect", "-fsafety-subobject", "a.c"]);
        assert_eq!(opts.subobject, rucc_session::Subobject::Members);

        let (opts, _) = compile(&["-fsafety-subobject", "-fno-safety-subobject", "a.c"]);
        assert_eq!(opts.subobject, rucc_session::Subobject::Off);

        // It takes no value. The form that would take one is the strict reading of section 9.4,
        // which is not written yet, so say so rather than accept a spelling that does nothing.
        let e = parse_args(&args(&["-fsafety-subobject=strict", "a.c"])).unwrap_err();
        assert!(e.message.contains("tamnd/rucc#967"), "{}", e.message);
    }

    #[test]
    fn whether_two_restrict_pointers_may_meet_is_read_off_the_command_line() {
        // Off by default, because the record a block keeps is the union of what each pointer
        // reached, so two pointers striding through one array without landing on the same byte are
        // reported and by the letter of the standard those are different objects. Row Y8 is a build
        // deciding it would rather know.
        let (opts, _) = compile(&["a.c"]);
        assert_eq!(opts.promise, rucc_session::Promise::Off);

        let (opts, _) = compile(&["-fsafety=detect", "-fsafety-restrict", "a.c"]);
        assert_eq!(opts.promise, rucc_session::Promise::Blocks);

        let (opts, _) = compile(&["-fsafety-restrict", "-fno-safety-restrict", "a.c"]);
        assert_eq!(opts.promise, rucc_session::Promise::Off);

        // The tier is still a tier, which is the thing worth pinning about a pair of names where
        // one is the front of the other.
        let (opts, _) = compile(&["-fsafety-restrict", "a.c"]);
        assert_eq!(opts.safety, rucc_session::Safety::Off);

        let e = parse_args(&args(&["-fsafety-restrict=blocks", "a.c"])).unwrap_err();
        assert!(e.message.contains("takes no value"), "{}", e.message);
    }

    #[test]
    fn safety_leaks_is_a_bare_flag_and_is_off_unless_asked_for() {
        let (opts, _) = compile(&["a.c"]);
        assert_eq!(opts.leaks, rucc_session::Leaks::Off);

        let (opts, _) = compile(&["-fsafety=detect", "-fsafety-leaks", "a.c"]);
        assert_eq!(opts.leaks, rucc_session::Leaks::Exit);

        let (opts, _) = compile(&["-fsafety-leaks", "-fno-safety-leaks", "a.c"]);
        assert_eq!(opts.leaks, rucc_session::Leaks::Off);

        let e = parse_args(&args(&["-fsafety-leaks=exit", "a.c"])).unwrap_err();
        assert!(e.message.contains("takes no value"), "{}", e.message);
    }

    #[test]
    fn safety_races_takes_a_mode_and_defaults_to_watching_nothing() {
        // Three modes rather than a bare flag, because section 9.5 gives two answers that record
        // the same thing and report different classes, so a flag with no value could not say which
        // was wanted. Off by default for the reason on `rucc_session::Races`, which is not a cost
        // argument: this is the one plane where an edge nobody interposed costs a false report.
        let (opts, _) = compile(&["a.c"]);
        assert_eq!(opts.races, rucc_session::Races::Off);

        let (opts, _) = compile(&["-fsafety-races=metadata", "a.c"]);
        assert_eq!(opts.races, rucc_session::Races::Metadata);

        let (opts, _) = compile(&["-fsafety-races=pointer", "a.c"]);
        assert_eq!(opts.races, rucc_session::Races::Pointer);

        // Last one wins, as it does for every other mode flag here.
        let (opts, _) = compile(&["-fsafety-races=pointer", "-fno-safety-races", "a.c"]);
        assert_eq!(opts.races, rucc_session::Races::Off);

        let e = parse_args(&args(&["-fsafety-races=all", "a.c"])).unwrap_err();
        assert!(e.message.contains("off, metadata or pointer"), "{}", e.message);
    }

    #[test]
    fn print_pipeline_answers_with_the_passes_the_level_asked_for() {
        let a = parse_args(&args(&["--print-pipeline", "-O2"])).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        let text = print_pipeline(&opts);
        assert!(text.starts_with("level: -O2\n"), "{text}");
        assert!(text.contains("fold"), "{text}");

        let a = parse_args(&args(&["--print-pipeline"])).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        // Two passes run at `-O0` and neither is an optimization. The first moves what
        // `__builtin_expect` said onto the branch and takes the instruction away, so that nothing
        // past the optimizer has to know the instruction exists. The second removes code nothing
        // reaches. See issue 359.
        assert!(print_pipeline(&opts).contains("1: expect,"), "{}", print_pipeline(&opts));
        assert!(print_pipeline(&opts).contains("2: simplify-cfg,"), "{}", print_pipeline(&opts));

        let a = parse_args(&args(&["--print-pipeline", "-fno-simplify-cfg"])).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        // The second turns off and the first does not, because nothing below the optimizer lowers
        // what it removes, so `-fno-expect` is a compile that stops rather than one that runs.
        let text = print_pipeline(&opts);
        assert!(text.contains("1: expect,"), "{text}");
        assert!(!text.contains("simplify-cfg"), "{text}");
    }

    #[test]
    fn print_pipeline_takes_the_toggles_into_account() {
        let a = parse_args(&args(&["--print-pipeline", "-O2", "-fno-fold"])).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        let text = print_pipeline(&opts);
        // The one that was named is gone and the rest of the level is not, which is the whole
        // of what a toggle promises.
        assert!(!text.contains("fold"), "{text}");
        assert!(text.contains("dce"), "{text}");

        // Every pass the compiler has, named off. Built from the registry rather than written
        // out, so a pass added later is turned off here too and this keeps testing the thing it
        // is about, which is that the toggles can empty a level down to the passes that are not
        // optional. Those are named, because a listing that is all of them is a level nobody
        // emptied and the assertion would pass while saying nothing.
        let mut off = vec!["--print-pipeline".to_owned(), "-O2".to_owned()];
        off.extend(rucc_opt::PASSES.iter().map(|p| format!("-fno-{}", p.name())));
        let spelled: Vec<&str> = off.iter().map(String::as_str).collect();
        let a = parse_args(&args(&spelled)).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        let text = print_pipeline(&opts);
        let left: Vec<&str> =
            rucc_opt::PASSES.iter().filter(|p| p.required()).map(|p| p.name()).collect();
        assert_eq!(left, vec!["expect", "constant-p"], "{text}");
        for (at, name) in left.iter().enumerate() {
            assert!(text.contains(&format!("{}: {name},", at + 1)), "{text}");
        }
        assert!(!text.contains("dce"), "{text}");
    }

    #[test]
    fn print_pipeline_says_when_a_budget_will_stop_the_run_short() {
        let a = parse_args(&args(&["--print-pipeline", "-O2"])).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        assert!(!print_pipeline(&opts).contains("global fuel"));

        let a = parse_args(&args(&["--print-pipeline", "-O2", "-fpass-fuel-global=4"])).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        let text = print_pipeline(&opts);
        // Because the listing is the answer to what this compilation will do, and a run that
        // stops after four rewrites is not doing what the level says it does.
        assert!(text.contains("global fuel: 4"), "{text}");
    }

    /// A pass is turned on and off by its own name, and the order the flags were given in is
    /// kept, because the last spelling of a name is the one that decides.
    #[test]
    fn a_pass_is_named_by_dash_f_and_unnamed_by_dash_f_no() {
        let (opts, _) = compile(&["-c", "-O0", "-ffold", "-fno-fold", "-ffold", "a.c"]);
        assert_eq!(
            opts.passes,
            [("fold".to_owned(), true), ("fold".to_owned(), false), ("fold".to_owned(), true)]
        );

        let e = parse_args(&args(&["-fno-such-pass", "a.c"])).unwrap_err();
        assert!(e.message.contains("unknown option"), "{}", e.message);
    }

    #[test]
    fn pass_fuel_names_a_pass_and_a_count_and_refuses_anything_else() {
        let (opts, _) = compile(&["-c", "-O2", "-fpass-fuel=fold=3", "a.c"]);
        assert_eq!(opts.pass_fuel, [("fold".to_owned(), 3)]);

        let e = parse_args(&args(&["-fpass-fuel=fold", "a.c"])).unwrap_err();
        assert!(e.message.contains("<pass>=<count>"), "{}", e.message);
        let e = parse_args(&args(&["-fpass-fuel=nosuch=3", "a.c"])).unwrap_err();
        assert!(e.message.contains("--print-pipeline"), "{}", e.message);
        let e = parse_args(&args(&["-fpass-fuel=fold=lots", "a.c"])).unwrap_err();
        assert!(e.message.contains("not a number"), "{}", e.message);
    }

    #[test]
    fn global_pass_fuel_is_a_count_on_its_own_and_defaults_to_no_limit() {
        let (opts, _) = compile(&["-c", "-O2", "a.c"]);
        assert_eq!(opts.pass_fuel_global, None);

        let (opts, _) = compile(&["-c", "-O2", "-fpass-fuel-global=12", "a.c"]);
        assert_eq!(opts.pass_fuel_global, Some(12));
        // And it is not the per pass flag with a longer name, so neither spelling swallows the
        // other.
        assert!(opts.pass_fuel.is_empty());

        let e = parse_args(&args(&["-fpass-fuel-global=lots", "a.c"])).unwrap_err();
        assert!(e.message.contains("not a number"), "{}", e.message);
    }

    #[test]
    fn the_trace_file_is_taken_from_the_flag_and_an_empty_one_is_refused() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.trace, None);
        let (opts, _) = compile(&["-c", "-frucc-trace=/tmp/compile.jsonl", "a.c"]);
        assert_eq!(opts.trace.as_deref(), Some("/tmp/compile.jsonl"));
        let e = parse_args(&args(&["-frucc-trace=", "a.c"])).unwrap_err();
        assert!(e.message.contains("needs a file"), "{}", e.message);
    }

    #[test]
    fn a_gate_names_a_pass_and_optionally_the_functions_it_covers() {
        let (opts, _) = compile(&["-c", "-O2", "-fdisable-fold", "-fenable-fold=2-4,main", "a.c"]);
        assert_eq!(
            opts.pass_gates,
            [(false, "fold".to_owned()), (true, "fold=2-4,main".to_owned())],
            "the order is what decides, so it has to survive the parse"
        );

        let e = parse_args(&args(&["-fdisable-nosuch", "a.c"])).unwrap_err();
        assert!(e.message.contains("--print-pipeline"), "{}", e.message);
        let e = parse_args(&args(&["-fenable-fold=9-2", "a.c"])).unwrap_err();
        assert!(e.message.contains("ends before it starts"), "{}", e.message);
        let e = parse_args(&args(&["-fdisable-fold=", "a.c"])).unwrap_err();
        assert!(e.message.contains("is empty"), "{}", e.message);
    }

    #[test]
    fn the_pipeline_listing_says_which_passes_a_gate_touched() {
        let (opts, _) = compile(&["-c", "-O2", "-fdisable-fold=main", "a.c"]);
        let text = print_pipeline(&opts);
        assert!(text.contains("fold, "), "{text}");
        assert!(text.contains("[off for main]"), "{text}");
    }

    /// The spelling is checked while the arguments are read, because a dump that names a pass
    /// this compiler does not have is a typo, and a typo found after the compilation has run is
    /// found too late to be any use.
    #[test]
    fn a_dump_is_checked_when_it_is_asked_for_rather_than_when_it_is_taken() {
        let (opts, _) = compile(&["-c", "-O2", "-fdump-ir=all", "-fdump-ir=after-fold", "a.c"]);
        assert_eq!(opts.dump_ir, ["all", "after-fold"]);

        let e = parse_args(&args(&["-fdump-ir=after-nosuch", "a.c"])).unwrap_err();
        assert!(e.message.contains("nosuch"), "{}", e.message);
        assert!(parse_args(&args(&["-fdump-ir=sideways-fold", "a.c"])).is_err());
    }

    /// Every spelling `-fopt-info` takes, and the one it does not.
    ///
    /// The keywords are checked here for the same reason a dump's pass name is: a person who
    /// misspelled one gets no output, and no output is also what a compilation where nothing
    /// happened looks like. Telling those two apart is the entire reason to reach for this flag.
    #[test]
    fn opt_info_takes_kinds_and_a_file_and_refuses_a_kind_it_does_not_have() {
        let (opts, _) = compile(&["-c", "-O2", "-fopt-info", "a.c"]);
        assert_eq!(opts.opt_info, [""], "a bare flag asks for the rewrites");
        assert_eq!(opts.opt_info_file, None, "and goes to standard error");

        let (opts, _) = compile(&["-c", "-O2", "-fopt-info-missed-note", "a.c"]);
        assert_eq!(opts.opt_info, ["missed-note"]);

        // Two flags add up rather than the second replacing the first, and the file is the last
        // one that named a file, which is how GCC treats both.
        let (opts, _) =
            compile(&["-c", "-O2", "-fopt-info-missed=one.txt", "-fopt-info-all=two.txt", "a.c"]);
        assert_eq!(opts.opt_info, ["missed", "all"]);
        assert_eq!(opts.opt_info_file.as_deref(), Some("two.txt"));

        let e = parse_args(&args(&["-fopt-info-vectorized", "a.c"])).unwrap_err();
        assert!(e.message.contains("vectorized"), "{}", e.message);
        assert!(e.message.contains("`missed`"), "{}", e.message);
        let e = parse_args(&args(&["-fopt-info-missed=", "a.c"])).unwrap_err();
        assert!(e.message.contains("no file"), "{}", e.message);
    }

    #[test]
    fn verify_each_is_unstable_and_off_unless_it_was_asked_for() {
        let (opts, _) = compile(&["-c", "-Zverify-each", "a.c"]);
        assert!(opts.verify_each);
        assert!(!USAGE.contains("verify-each"), "an unstable option stays out of the usage text");
    }

    #[test]
    fn dash_o_needs_an_argument() {
        let e = parse_args(&args(&["a.c", "-o"])).unwrap_err();
        assert_eq!(e.message, "-o requires an argument");
    }

    #[test]
    fn dash_d_and_dash_u_are_read_joined_or_separated_and_keep_their_order() {
        let (opts, _) = compile(&["-DFOO=1", "-D", "BAR", "-UBAZ", "-U", "QUX", "a.c"]);
        assert_eq!(opts.defines, ["FOO=1", "BAR"]);
        assert_eq!(opts.undefines, ["BAZ", "QUX"]);
    }

    #[test]
    fn the_last_dash_d_or_dash_u_for_a_name_decides() {
        let line = ["-D_FORTIFY_SOURCE=3", "-U_FORTIFY_SOURCE", "-D_FORTIFY_SOURCE=2", "a.c"];
        let (opts, _) = compile(&line);
        assert_eq!(opts.defines, ["_FORTIFY_SOURCE=2"]);
        assert!(opts.undefines.is_empty());
        let (opts, _) = compile(&["-DF(x)=x", "-DG", "-UF", "-UG", "-DG=2", "a.c"]);
        assert_eq!(opts.defines, ["G=2"]);
        assert_eq!(opts.undefines, ["F"]);
        let (opts, _) = compile(&["-DX=1", "-DX=2", "-UY", "a.c"]);
        assert_eq!(opts.defines, ["X=1", "X=2"]);
        assert_eq!(opts.undefines, ["Y"]);
    }

    #[test]
    fn the_include_flags_land_on_the_chain_each_one_names() {
        // A sysroot with nothing under it, so that the library's own directories are the
        // same on every machine this test runs on, which is none of them.
        let (opts, _) = compile(&[
            "-Ii",
            "-iquote",
            "q",
            "-iquote../uapi",
            "-isystem",
            "sys",
            "-isystemsys2",
            "-idirafter",
            "after",
            "--sysroot=/nowhere-at-all",
            "a.c",
        ]);
        let dirs: Vec<&str> = opts.search.dirs().iter().filter_map(|d| d.path.to_str()).collect();
        // The compiler's own headers sit after every `-isystem` and before `-idirafter`,
        // which is where GCC puts its own: a directory the user named outranks ours.
        assert_eq!(dirs, ["q", "../uapi", "i", "sys", "sys2", runtime::DIR, "after"]);
        assert!(!opts.search.dirs()[2].is_system);
        assert!(opts.search.dirs()[3].is_system);
    }

    #[test]
    fn the_librarys_headers_come_after_the_compilers_own_and_go_away_with_them() {
        // Which machine this runs on decides what is on the path, so the test is about the
        // order rather than about the names: ours is on it, the library's follow it, and
        // `-nostdinc` is the one flag that takes both halves of the pair off at once.
        let (opts, _) = compile(&["a.c"]);
        let dirs = opts.search.dirs();
        let ours = dirs.iter().position(|d| d.path.to_str() == Some(runtime::DIR));
        assert_eq!(ours, Some(0), "{dirs:?}");
        assert!(dirs[1..].iter().all(|d| d.is_system), "{dirs:?}");
        let (bare, _) = compile(&["-nostdinc", "a.c"]);
        assert!(bare.search.dirs().is_empty(), "{:?}", bare.search.dirs());
    }

    #[test]
    fn a_sysroot_moves_the_librarys_directories_and_nothing_else() {
        let (opts, _) = compile(&["-isystem", "sys", "--sysroot=/nowhere-at-all", "a.c"]);
        let dirs: Vec<&str> = opts.search.dirs().iter().filter_map(|d| d.path.to_str()).collect();
        assert_eq!(dirs, ["sys", runtime::DIR]);
    }

    #[test]
    fn a_cross_compile_reads_the_targets_own_headers_rather_than_the_ones_next_door() {
        // The target is not the machine this test runs on wherever it runs, so the answer is the
        // same on all of them: the libc's two include directories for that target, the kernel's
        // two, and nothing from here. A header read from here is the quiet failure of section 8.5, a
        // program that builds on the build machine and is wrong everywhere else.
        let (opts, _) = compile(&["--target=riscv64-linux-musl", "-c", "a.c"]);
        let dirs: Vec<&std::path::Path> =
            opts.search.dirs().iter().map(|d| d.path.as_path()).collect();
        let root = cache::dir().join("sysroots").join("riscv64-linux-musl");
        let kernel = cache::dir().join("kernel-headers");
        assert_eq!(dirs.len(), 5, "{dirs:?}");
        assert_eq!(dirs[0], std::path::Path::new(runtime::DIR));
        assert_eq!(dirs[1], root.join("include").join("riscv64"));
        assert_eq!(dirs[2], root.join("include").join("generic"));
        // The kernel's, which are beside the sysroots rather than inside one, because every target
        // that shares an architecture reads the same files.
        assert_eq!(dirs[3], kernel.join("riscv"));
        assert_eq!(dirs[4], kernel.join("generic"));
    }

    #[test]
    fn a_cross_compile_to_something_that_is_not_linux_reads_no_kernel_headers() {
        // The other side of the same answer. Windows has its own system headers and no `linux/` at
        // all, so the list is the libc's own and the question never arises, which is the `None` that
        // `link::cross_kernel` returns rather than a directory nothing would be found in.
        //
        // The libc's own is one directory rather than two here, because mingw-w64 publishes a single
        // header tree for every architecture and `Sysroot::splits_by_arch` says so.
        let (opts, _) = compile(&["--target=x86_64-pc-windows-gnu", "-c", "a.c"]);
        let dirs: Vec<&std::path::Path> =
            opts.search.dirs().iter().map(|d| d.path.as_path()).collect();
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert!(!dirs.iter().any(|dir| dir.ends_with("kernel-headers")), "{dirs:?}");
    }

    #[test]
    fn the_glibc_version_macro_goes_with_the_bundled_tree_and_with_nothing_else() {
        // One tree serves every glibc release, so the release is what the target supplies, and the
        // condition is the same one that chose the directories. A host glibc and a tree somebody
        // named both define `__GLIBC_MINOR__` in their own `features.h`, and two definitions with
        // different values is a warning on every compilation of every file.
        //
        // The architecture is chosen against this machine's rather than written down, because the
        // bundled tree is only in effect for a target that is not this machine. The first version of
        // this test said x86_64-linux-gnu, which is a cross compile on a mac and this machine on a
        // Linux runner, so it passed here and failed there.
        //
        // Unless this machine has the distribution's cross packages for it and nothing fetched, and
        // then those are the headers and their own `features.h` says the release, as it does for a
        // tree somebody named.
        let gnu = format!("--target={}-linux-gnu", cross_arch());
        let (bundled, _) = compile(&[&gnu, "-c", "a.c"]);
        let (link, _) = linking(&[&gnu, "-c", "a.c"]);
        let distro = link::distro_cross(bundled.target, &link).is_some();
        assert_eq!(bundled.glibc_minor, if distro { None } else { Some(44) });
        let pin = format!("{gnu}.2.28");
        let (pinned, _) = compile(&[&pin, "-c", "a.c"]);
        assert_eq!(pinned.glibc_minor, Some(28));

        let (named, _) = compile(&[&gnu, "--sysroot=/nowhere-at-all", "-c", "a.c"]);
        assert_eq!(named.glibc_minor, None);
        let (none, _) = compile(&[&gnu, "-nostdinc", "-c", "a.c"]);
        assert_eq!(none.glibc_minor, None);
        let musl = format!("--target={}-linux-musl", cross_arch());
        let (musl, _) = compile(&[&musl, "-c", "a.c"]);
        assert_eq!(musl.glibc_minor, None);

        // And this machine's own target gets nothing, whatever this machine is, because its headers
        // come from the machine and its own `features.h` defines the macro. On a glibc Linux box
        // that is the case this test had backwards; on a mac it is true for the other reason, which
        // is that Darwin is not a glibc target at all.
        if let Some(host) = Triple::host() {
            let native = format!("--target={}", host.tuple());
            let (native, _) = compile(&[&native, "-c", "a.c"]);
            assert_eq!(native.glibc_minor, None);
        }
    }

    #[test]
    fn a_pinned_release_on_this_machines_own_target_reads_the_bundled_tree() {
        // The end to end half of the answer in `link::cross_for`. A release named for this machine's
        // own target is a cross compile, so the headers are the bundled tree's and the macro says
        // what was asked for rather than what this machine has.
        //
        // Only on a glibc box, because a release is a glibc release: a mac has no `__GLIBC_MINOR__`
        // to get wrong and nothing to pin. That makes this a test the Linux runners carry, which is
        // where the case lives.
        let Some(host) = Triple::host() else { return };
        if host.os != rucc_target::Os::Linux || host.env != rucc_target::Env::Gnu {
            return;
        }
        let pin = format!("--target={}.2.28", host.tuple());
        let (opts, _) = compile(&[&pin, "-c", "a.c"]);
        assert_eq!(opts.glibc_minor, Some(28));
        let root = cache::dir().join("sysroots").join(format!("{}.2.28", host.tuple()));
        let dirs: Vec<&std::path::Path> =
            opts.search.dirs().iter().map(|d| d.path.as_path()).collect();
        assert!(dirs.iter().any(|dir| dir.starts_with(&root)), "{dirs:?}");
        // And nothing of this machine's, which is the failure this was: a program compiled against
        // 2.44 declarations and told it was 2.28.
        assert!(!dirs.iter().any(|dir| *dir == std::path::Path::new("/usr/include")), "{dirs:?}");
    }

    /// An architecture that is not this machine's, out of the three the driver has targets for.
    ///
    /// A test about the bundled sysroot has to name a target that is not the host, because a target
    /// that is the host reads the host's own headers and libraries. Asking which machine this is
    /// beats picking a row and hoping, and it is two lines.
    fn cross_arch() -> &'static str {
        match Triple::host().map(|host| host.arch) {
            Some(rucc_target::Arch::X86_64) => "aarch64",
            _ => "x86_64",
        }
    }

    #[test]
    fn a_glibc_newer_than_the_bundled_tree_is_refused_by_name() {
        // Both versions in the message, because the two things a person can do about it are pin a
        // release the tree has and name a sysroot that has the one they asked for, and neither is a
        // choice they can make without knowing which release the tree is.
        //
        // Not this machine's architecture, for the reason the test above gives: the refusal is about
        // the bundled tree, and the bundled tree is not what a target that is this machine reads.
        let target = format!("--target={}-linux-gnu.2.99", cross_arch());
        let message = refused(&[&target, "-c", "a.c"]);
        assert!(message.contains("asked for glibc 2.99"), "{message}");
        assert!(message.contains("bundled headers are glibc 2.44"), "{message}");
        assert!(message.contains("--sysroot"), "{message}");
    }

    #[test]
    fn a_sysroot_the_user_named_is_still_what_a_cross_compile_reads() {
        // The tree somebody assembled beats the one we would build, on the headers as on the
        // libraries. It is empty here, which is why the list comes out short: the directories under
        // it are checked for rather than assumed, and a tree that is not there offers nothing.
        let (opts, _) =
            compile(&["--target=riscv64-linux-musl", "--sysroot=/nowhere-at-all", "-c", "a.c"]);
        let dirs: Vec<&std::path::Path> =
            opts.search.dirs().iter().map(|d| d.path.as_path()).collect();
        assert_eq!(dirs, [std::path::Path::new(runtime::DIR)]);
    }

    #[test]
    fn dash_i_dash_moves_the_bracket_directories_into_the_quoted_chain() {
        let (opts, _) =
            compile(&["-Iinc1", "-iquote", "inc2", "-I-", "-Iinc3", "-nostdinc", "a.c"]);
        let dirs: Vec<&str> = opts.search.dirs().iter().filter_map(|d| d.path.to_str()).collect();
        assert_eq!(dirs, ["inc1", "inc2", "inc3"]);
        // An angled include sees only what came after the flag.
        assert_eq!(opts.search.start(IncludeForm::Angled), 2);
        assert!(!opts.search.searches_current_dir());
    }

    #[test]
    fn the_prefix_flags_stick_what_iprefix_said_on_the_front_of_what_follows_it() {
        let (opts, _) = compile(&[
            "-iprefix",
            "/tools/",
            "-iwithprefix",
            "late",
            "-iwithprefixbefore",
            "early",
            "-iprefix",
            "/other/",
            "-iwithprefix",
            "last",
            "-nostdinc",
            "a.c",
        ]);
        let dirs: Vec<&str> = opts.search.dirs().iter().filter_map(|d| d.path.to_str()).collect();
        // `-iwithprefixbefore` is an `-I` and the other two are `-isystem`, which is where GCC
        // puts them rather than where its manual says it does.
        assert_eq!(dirs, ["/tools/early", "/tools/late", "/other/last"]);
        assert!(!opts.search.dirs()[0].is_system);
        assert!(opts.search.dirs()[1].is_system);
    }

    #[test]
    fn the_files_named_on_the_command_line_keep_their_order_and_which_flag_named_them() {
        let (opts, _) =
            compile(&["-include", "one.h", "-imacros", "two.h", "-include", "3.h", "a.c"]);
        let names: Vec<&str> = opts.preincludes.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["one.h", "two.h", "3.h"]);
        assert_eq!(opts.preincludes.iter().filter(|p| p.macros_only).count(), 1);
    }

    #[test]
    fn nostdinc_takes_the_compilers_own_headers_off_the_path() {
        let (opts, _) = compile(&["-Ii", "-nostdinc", "a.c"]);
        let dirs: Vec<&str> = opts.search.dirs().iter().filter_map(|d| d.path.to_str()).collect();
        assert_eq!(dirs, ["i"]);
    }

    #[test]
    fn the_dialect_flags_set_the_language_and_the_extensions_separately() {
        let (opts, _) = compile(&["-std=gnu11", "a.c"]);
        assert_eq!(opts.std, Std::C11);
        assert!(opts.gnu_extensions);

        let (opts, _) = compile(&["-std=iso9899:1999", "a.c"]);
        assert_eq!(opts.std, Std::C99);
        assert!(!opts.gnu_extensions);

        let (opts, _) = compile(&["-ansi", "a.c"]);
        assert_eq!(opts.std, Std::C89);
        assert!(!opts.gnu_extensions);
        assert!(!opts.trigraphs, "the dialect turns them on, not the flag");

        let (opts, _) = compile(&["-std=gnu17", "-trigraphs", "a.c"]);
        assert!(opts.trigraphs);

        let (opts, _) = compile(&["-std=gnu2y", "a.c"]);
        assert_eq!(opts.std, Std::C2y);
        assert!(opts.gnu_extensions);

        let e = parse_args(&args(&["-std=c94jr", "a.c"])).unwrap_err();
        assert!(e.message.contains("unknown dialect"), "{}", e.message);
    }

    #[test]
    fn the_dump_letters_are_a_family_and_everything_else_beginning_with_d_is_not() {
        let (opts, _) = compile(&["-dM", "a.c"]);
        assert!(opts.dumps.macros);

        // Packed, the way GCC takes them, and a letter in the family we have not written yet
        // is accepted and does nothing rather than failing a build.
        let (opts, _) = compile(&["-dDM", "a.c"]);
        assert!(opts.dumps.macros);
        let (opts, _) = compile(&["-dD", "a.c"]);
        assert!(!opts.dumps.macros);

        let (opts, _) = compile(&["a.c"]);
        assert!(!opts.dumps.any());

        // `-dumpversion` is a different flag that happens to start the same way, and it is read
        // as itself rather than as a dump of nothing.
        assert_eq!(printed(&["-dumpversion", "a.c"]), "16");
    }

    #[test]
    fn the_msvc_runtime_is_a_compile_flag_and_a_link_one() {
        let (link, _) = linking(&["a.c"]);
        assert_eq!(link.crt, rucc_sysroot::Crt::Static);
        let (link, _) = linking(&["-fms-runtime-lib=dll", "a.c"]);
        assert_eq!(link.crt, rucc_sysroot::Crt::Dll);
        let (opts, _) = compile(&["-fms-runtime-lib=dll", "-c", "a.c"]);
        assert!(opts.ms_dll_runtime);
        let (opts, _) = compile(&["-fms-runtime-lib=dll", "-fms-runtime-lib=static", "-c", "a.c"]);
        assert!(!opts.ms_dll_runtime, "the last one wins");
        let e = parse_args(&args(&["-fms-runtime-lib=dll_dbg", "a.c"])).unwrap_err();
        assert!(e.message.contains("debug"), "{}", e.message);
        let e = parse_args(&args(&["-fms-runtime-lib=shared", "a.c"])).unwrap_err();
        assert!(e.message.contains("static or dll"), "{}", e.message);
    }

    #[test]
    fn the_gcc_version_claimed_is_a_flag_and_the_short_spellings_are_the_ones_people_write() {
        let (opts, _) = compile(&["a.c"]);
        assert_eq!(
            opts.gnuc,
            GnucVersion { major: 16, minor: 0, patch: 0 },
            "the release this compiler is written against, and the earliest one of that series"
        );

        let (opts, _) = compile(&["-fgnuc-version=15.1.0", "a.c"]);
        assert_eq!(opts.gnuc, GnucVersion { major: 15, minor: 1, patch: 0 });

        // A missing component is zero. `gcc -dumpversion` says `15` on a release with no
        // patchlevel and a harness that pastes that back has to be understood.
        let (opts, _) = compile(&["-fgnuc-version=15", "a.c"]);
        assert_eq!(opts.gnuc, GnucVersion { major: 15, minor: 0, patch: 0 });

        assert!(opts.gnuc_given, "a version that was written down is one that was given");
        assert!(!compile(&["a.c"]).0.gnuc_given);

        let (opts, _) = compile(&["-fms-compatibility-version=19.29.30133", "a.c"]);
        assert_eq!(opts.msc.msc_ver(), 1929);
        assert_eq!(opts.msc.msc_full_ver(), 192_930_133);

        let (opts, _) = compile(&["-fgnuc-version=13.2", "a.c"]);
        assert_eq!(opts.gnuc, GnucVersion { major: 13, minor: 2, patch: 0 });

        let e = parse_args(&args(&["-fgnuc-version=15.x", "a.c"])).unwrap_err();
        assert!(e.message.contains("minor that is not a number"), "{}", e.message);

        let e = parse_args(&args(&["-fgnuc-version=1.2.3.4", "a.c"])).unwrap_err();
        assert!(e.message.contains("more than three"), "{}", e.message);
    }

    #[test]
    fn pedantic_has_two_spellings_and_is_not_the_same_knob_as_the_dialect() {
        let (opts, _) = compile(&["-std=c17", "-pedantic", "a.c"]);
        assert!(opts.pedantic);
        assert_eq!(opts.std, Std::C17);

        // The `-W` family's name for it, which is what a build that groups its warning flags
        // tends to write.
        let (opts, _) = compile(&["-Wpedantic", "a.c"]);
        assert!(opts.pedantic);

        let (opts, _) = compile(&["-std=c17", "a.c"]);
        assert!(!opts.pedantic, "a dialect on its own does not diagnose an extension");
    }

    #[test]
    fn dash_p_and_dash_ffreestanding_reach_the_options() {
        let (opts, _) = compile(&["-E", "-P", "-ffreestanding", "a.c"]);
        assert!(!opts.line_markers);
        assert!(!opts.hosted);
        assert_eq!(opts.emit, EmitKind::Preprocessed);
    }

    #[test]
    fn dash_c_is_taken_when_preprocessing() {
        let (opts, _) = compile(&["-E", "-P", "-C", "a.lds.S"]);
        assert_eq!(opts.emit, EmitKind::Preprocessed);
        let (opts, _) = compile(&["-E", "-CC", "a.c"]);
        assert_eq!(opts.emit, EmitKind::Preprocessed);
    }

    /// The two ways a build says it means its own function by a name the C library also has.
    ///
    /// `-fno-builtin` is all of them and `-fno-builtin-<name>` is one, and the second is what a
    /// build writes when it means its own `memcpy` and the library's everything else. The name is
    /// kept as it was written and not checked against anything, because a program is allowed to
    /// mean something by a name this compiler has never heard of.
    #[test]
    fn the_builtin_flags_are_read_in_both_directions_and_one_name_at_a_time() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(opts.builtins, "a library name means the library function by default");
        assert!(opts.no_builtin.is_empty());

        let (opts, _) = compile(&["-c", "-fno-builtin", "a.c"]);
        assert!(!opts.builtins);

        let (opts, _) = compile(&["-c", "-fno-builtin", "-fbuiltin", "a.c"]);
        assert!(opts.builtins, "the last mention decides");

        let (opts, _) = compile(&["-c", "-fno-builtin-memcpy", "-fno-builtin-nonesuch", "a.c"]);
        assert!(opts.builtins, "one name is not the family");
        assert_eq!(opts.no_builtin, vec!["memcpy".to_owned(), "nonesuch".to_owned()]);
    }

    /// `-fvisibility=`, which is on every cmake project that cares about which names it exports
    /// and which was refused as an unknown option until now.
    ///
    /// Four spellings and three answers. `internal` is hidden plus a promise about never taking
    /// the address across a component boundary, and nothing derives anything from that promise
    /// here, so it comes out as the weaker of the two rather than as a refusal that stops a build
    /// over a distinction this compiler does not make.
    #[test]
    fn visibility_takes_the_four_spellings_gcc_takes_and_refuses_the_rest() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.visibility, Visibility::Default, "exported unless something says not");

        for (written, wanted) in [
            ("default", Visibility::Default),
            ("hidden", Visibility::Hidden),
            ("internal", Visibility::Hidden),
            ("protected", Visibility::Protected),
        ] {
            let (opts, _) = compile(&["-c", &format!("-fvisibility={written}"), "a.c"]);
            assert_eq!(opts.visibility, wanted, "{written}");
        }

        // The last mention decides, which is what every other flag of this shape does and what a
        // build that turns something off for one directory relies on.
        let (opts, _) = compile(&["-c", "-fvisibility=hidden", "-fvisibility=default", "a.c"]);
        assert_eq!(opts.visibility, Visibility::Default, "the last mention decides");

        // A spelling gcc does not take is refused rather than read as the default, because a
        // build that meant hidden and got exported is a library with the wrong interface and
        // nothing said about it anywhere.
        let failed = parse_args(&args(&["-fvisibility=none", "a.c"])).expect_err("refused");
        assert!(failed.to_string().contains("is not a visibility"), "{failed}");
    }

    /// `-ffp-contract=`, which is the one flag in the floating point group that is kept rather than
    /// described, and the values are gcc 16's three.
    #[test]
    fn how_far_a_multiply_and_an_addition_may_be_fused_is_asked_for() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.fp_contract, Contract::Off, "a licence nobody granted is not assumed");

        for (written, wanted) in
            [("off", Contract::Off), ("on", Contract::On), ("fast", Contract::Fast)]
        {
            let (opts, _) = compile(&["-c", &format!("-ffp-contract={written}"), "a.c"]);
            assert_eq!(opts.fp_contract, wanted, "{written}");
        }

        let (opts, _) = compile(&["-c", "-ffp-contract=fast", "-ffp-contract=off", "a.c"]);
        assert_eq!(opts.fp_contract, Contract::Off, "the last mention decides");

        // Refused rather than read as one of the three, because a build that asked for no fusing
        // and was given the default would be one whose numbers change and whose command line says
        // they should not. gcc refuses the same spellings and names the same three in its message.
        for bad in ["-ffp-contract=none", "-ffp-contract=", "-ffp-contract=Fast"] {
            let failed = parse_args(&args(&[bad, "a.c"])).expect_err("refused");
            assert!(failed.to_string().contains("is not a contraction"), "{bad}: {failed}");
        }

        // And the other one that takes a value, which is taken and kept nowhere: every operation
        // here is computed in the type it was written in, so `standard` is what happens and the
        // other two are permission to do something this does not do.
        let failed = parse_args(&args(&["-fexcess-precision=long", "a.c"])).expect_err("refused");
        assert!(failed.to_string().contains("is not an excess precision"), "{failed}");
    }

    /// The four prefix mapping flags, which are what a distribution passes to get the same bytes
    /// out of `/build/pkg-1.2` and out of `/home/someone/pkg-1.2`. Three lists rather than one
    /// because gcc has three, and `-ffile-prefix-map=` is the three of them at once.
    #[test]
    fn a_prefix_mapping_flag_goes_on_the_list_its_spelling_names() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(opts.prefix_map.macros.is_empty(), "nothing is rewritten unless it is asked for");
        assert!(opts.prefix_map.debug.is_empty(), "nor here");
        assert!(opts.prefix_map.profile.is_empty(), "nor here");

        let (opts, _) = compile(&["-c", "-fmacro-prefix-map=/build=.", "a.c"]);
        assert_eq!(opts.prefix_map.macros.apply("/build/a.c"), "./a.c", "the one it names");
        assert!(opts.prefix_map.debug.is_empty(), "and not the two it does not");

        let (opts, _) = compile(&["-c", "-fdebug-prefix-map=/build=.", "a.c"]);
        assert_eq!(opts.prefix_map.debug.apply("/build/a.c"), "./a.c", "the one it names");
        assert!(opts.prefix_map.macros.is_empty(), "and not the two it does not");

        let (opts, _) = compile(&["-c", "-fprofile-prefix-map=/build=.", "a.c"]);
        assert_eq!(opts.prefix_map.profile.apply("/build/a.c"), "./a.c", "the one it names");
        assert!(opts.prefix_map.macros.is_empty(), "and not the two it does not");

        let (opts, _) = compile(&["-c", "-ffile-prefix-map=/build=.", "a.c"]);
        for list in [&opts.prefix_map.macros, &opts.prefix_map.debug, &opts.prefix_map.profile] {
            assert_eq!(list.apply("/build/a.c"), "./a.c", "all three at once");
        }

        // Every mention is kept and the last one that matches wins, unlike the flags above whose
        // last mention replaces the earlier ones. A build writes one of these per source root and
        // expects all of them to be in force, which is the whole point of a list.
        let (opts, _) =
            compile(&["-c", "-ffile-prefix-map=/a=one", "-ffile-prefix-map=/b=two", "a.c"]);
        assert_eq!(opts.prefix_map.macros.apply("/a/x.c"), "one/x.c", "the earlier one still acts");
        assert_eq!(opts.prefix_map.macros.apply("/b/x.c"), "two/x.c", "and so does the later one");

        // An argument with no `=` is refused rather than ignored, because a build whose paths were
        // meant to be rewritten and were not is one that ships the build directory's name and says
        // nothing about it. gcc refuses the same thing.
        for bad in ["-fmacro-prefix-map=nope", "-ffile-prefix-map=", "-fdebug-prefix-map=/build"] {
            let failed = parse_args(&args(&[bad, "a.c"])).expect_err("refused");
            assert!(failed.to_string().contains("is not a rewrite for"), "{bad}: {failed}");
        }
    }

    /// `-ffunction-sections` and `-fdata-sections`, which are what make `--gc-sections` able to
    /// drop anything: a linker can leave out a section nothing reaches and cannot leave out half of
    /// one. A kernel and an embedded image are both linked that way.
    ///
    /// Two flags rather than one because gcc has two, and a build that asks for one of them and not
    /// the other is a build that measured something: splitting the code is nearly free at link time
    /// and splitting the data can defeat the linker's ordering of what is next to what.
    #[test]
    fn a_section_per_function_and_a_section_per_variable_are_asked_for_one_at_a_time() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.function_sections, "one text section unless something says otherwise");
        assert!(!opts.data_sections);

        let (opts, _) = compile(&["-c", "-ffunction-sections", "a.c"]);
        assert!(opts.function_sections);
        assert!(!opts.data_sections, "one flag is not the other");

        let (opts, _) = compile(&["-c", "-fdata-sections", "a.c"]);
        assert!(opts.data_sections);
        assert!(!opts.function_sections);

        // Both directions taken, and the off one is what happens anyway rather than a refusal,
        // since a build that writes it is asking for the default.
        let (opts, _) = compile(&[
            "-c",
            "-ffunction-sections",
            "-fno-function-sections",
            "-fdata-sections",
            "-fno-data-sections",
            "a.c",
        ]);
        assert!(!opts.function_sections, "the last mention decides");
        assert!(!opts.data_sections, "the last mention decides");
    }

    /// `-fgnu89-inline`, which is off by default and is not implied by anything on the command
    /// line, since the dialect asks for GNU's reading further in rather than through this.
    #[test]
    fn gnu89_inline_is_off_until_it_is_asked_for_and_the_last_mention_decides() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.gnu89_inline, "C's reading of inline by default");

        let (opts, _) = compile(&["-c", "-fgnu89-inline", "a.c"]);
        assert!(opts.gnu89_inline);

        let (opts, _) = compile(&["-c", "-fgnu89-inline", "-fno-gnu89-inline", "a.c"]);
        assert!(!opts.gnu89_inline, "the last mention decides");

        // The C89 dialects are under GNU's reading whether this was written or not, so the flag
        // stays off there and the dialect is what the checker and the macro set both ask. That is
        // also why `-std=c89 -fno-gnu89-inline` needs no diagnostic: it asks for the reading the
        // dialect already has. gcc refuses that command line, which is measured in the issue.
        let (opts, _) = compile(&["-c", "-std=c89", "a.c"]);
        assert!(!opts.gnu89_inline);
    }

    /// Both spellings of both frame flags, since a build that wants one usually writes the
    /// other beside it for the one file that has to be compiled the ordinary way.
    #[test]
    fn the_two_frame_flags_are_read_in_both_directions() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.frame_pointer, None, "nothing said, so the level decides");
        assert!(opts.keeps_frame_pointer(), "and at -O0 gcc keeps one, so this does too");
        let (opts, _) = compile(&["-c", "-O1", "a.c"]);
        assert!(!opts.keeps_frame_pointer(), "gcc omits it above -O0 and so does this");
        assert!(opts.red_zone, "the psABI has one and nothing said not to use it");

        let (opts, _) = compile(&["-c", "-fno-omit-frame-pointer", "-mno-red-zone", "a.c"]);
        assert_eq!(opts.frame_pointer, Some(true));
        assert!(!opts.red_zone);

        let (opts, _) = compile(&[
            "-c",
            "-fno-omit-frame-pointer",
            "-fomit-frame-pointer",
            "-mno-red-zone",
            "-mred-zone",
            "a.c",
        ]);
        assert_eq!(opts.frame_pointer, Some(false), "the last one wins, as it does in gcc");
        assert!(!opts.keeps_frame_pointer(), "and it wins over the level too");
        assert!(opts.red_zone);
    }

    /// The Fedora line passes `-mtls-dialect=gnu2` on x86-64. Each target takes the values that
    /// gcc takes for it, and refuses the others.
    #[test]
    fn the_tls_dialect_is_checked_for_the_target() {
        for value in ["gnu", "gnu2"] {
            let flag = format!("-mtls-dialect={value}");
            compile(&["--target=x86_64-linux-gnu", &flag, "-c", "a.c"]);
            compile(&["--target=i686-linux-gnu", &flag, "-c", "a.c"]);
            let why = refused(&["--target=aarch64-linux-gnu", &flag, "-c", "a.c"]);
            assert!(why.contains("desc or trad"), "{why}");
        }
        for value in ["desc", "trad"] {
            let flag = format!("-mtls-dialect={value}");
            compile(&["--target=aarch64-linux-gnu", &flag, "-c", "a.c"]);
            let why = refused(&["--target=x86_64-linux-gnu", &flag, "-c", "a.c"]);
            assert!(why.contains("gnu or gnu2"), "{why}");
        }
    }

    /// The Ubuntu line: both frame pointer flags, and the leaf one is read in both directions.
    #[test]
    fn the_leaf_frame_pointer_flag_is_read_in_both_directions() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(opts.leaf_frame_pointer, "a leaf keeps it unless the command line said not to");

        let (opts, _) = compile(&[
            "-c",
            "-O2",
            "-fno-omit-frame-pointer",
            "-mno-omit-leaf-frame-pointer",
            "a.c",
        ]);
        assert!(opts.keeps_frame_pointer() && opts.leaf_frame_pointer);

        let (opts, _) = compile(&["-c", "-momit-leaf-frame-pointer", "a.c"]);
        assert!(!opts.leaf_frame_pointer);
        let (opts, _) =
            compile(&["-c", "-momit-leaf-frame-pointer", "-mno-omit-leaf-frame-pointer", "a.c"]);
        assert!(opts.leaf_frame_pointer, "the last one wins");
    }

    /// A call goes through the PLT unless the line says `-fno-plt`, and the last one wins.
    #[test]
    fn the_plt_flag_is_read_in_both_directions() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(opts.plt, "gcc calls through the PLT unless it was asked not to");
        let (opts, _) = compile(&["-c", "-fno-plt", "a.c"]);
        assert!(!opts.plt);
        let (opts, _) = compile(&["-c", "-fno-plt", "-fplt", "a.c"]);
        assert!(opts.plt, "the last one wins");
    }

    /// `-fhardened` turns on each item of gcc's list, on x86-64 GNU/Linux at `-O2`.
    #[test]
    fn the_hardened_flag_turns_on_gcc_s_list() {
        let (opts, _) = compile(&[LINUX, "-c", "-O2", "-fhardened", "a.c"]);
        assert!(opts.defines.iter().any(|d| d == "_FORTIFY_SOURCE=3"), "{:?}", opts.defines);
        assert!(opts.defines.iter().any(|d| d == "_GLIBCXX_ASSERTIONS"), "{:?}", opts.defines);
        assert_eq!(opts.auto_var_init, Some(0));
        assert_eq!(opts.protector, Protector::Strong);
        assert!(opts.stack_clash);
        assert_eq!(opts.control, Control::Full);
        assert!(notes(&[LINUX, "-c", "-O2", "-fhardened", "a.c"]).is_empty());
        let (link, _) = linking(&[LINUX, "-O2", "-fhardened", "a.c"]);
        assert!(link.hardened);
        let (opts, _) = compile(&[LINUX, "-c", "-O2", "-fhardened", "-fno-hardened", "a.c"]);
        assert_eq!(opts.protector, Protector::None, "the last one wins");

        // AArch64 has no control flow protection in the list.
        let (opts, _) = compile(&["--target=aarch64-unknown-linux-gnu", "-c", "-fhardened", "a.c"]);
        assert_eq!(opts.control, Control::None);
        assert_eq!(opts.protector, Protector::Strong);

        let musl = refused(&["--target=x86_64-unknown-linux-musl", "-c", "-fhardened", "a.c"]);
        assert!(musl.contains("-fhardened"), "{musl}");
    }

    /// A flag that the line names wins over `-fhardened`, and a warning says so, in gcc's words.
    #[test]
    fn a_named_flag_wins_over_the_hardened_flag_with_a_warning() {
        let line = [LINUX, "-c", "-O0", "-fhardened", "-fstack-protector", "-fcf-protection=none"];
        let (opts, _) = compile(&[&line[..], &["a.c"]].concat());
        assert_eq!(opts.protector, Protector::Buffers);
        assert_eq!(opts.control, Control::None);
        assert!(!opts.defines.iter().any(|d| d.starts_with("_FORTIFY_SOURCE")));
        let said = notes(&[&line[..], &["a.c"]].concat());
        assert_eq!(said.len(), 3, "{said:?}");
        assert!(said[0].contains("because optimizations are turned off"), "{said:?}");
        assert!(said.iter().all(|note| note.ends_with("[-Whardened]")), "{said:?}");
        assert!(notes(&[&line[..], &["-Wno-hardened", "a.c"]].concat()).is_empty());

        let said = notes(&[LINUX, "-c", "-O2", "-D_FORTIFY_SOURCE=2", "-fhardened", "a.c"]);
        assert!(said[0].contains("specified in -D or -U"), "{said:?}");

        for other in ["-static", "-no-pie", "-Wl,-z,lazy"] {
            let (link, _) = linking(&[LINUX, "-O2", "-fhardened", other, "a.c"]);
            assert!(!link.hardened, "{other}");
            let said = notes(&[LINUX, "-O2", "-fhardened", other, "a.c"]);
            assert!(said[0].starts_with("linker hardening options"), "{other}: {said:?}");
        }
        assert!(notes(&[LINUX, "-c", "-O2", "-fhardened", "-Wl,-z,lazy", "a.c"]).is_empty());
    }

    /// The Ubuntu flag line, with the paths, the macros, the warnings and the link flags left out,
    /// in the order the line has them.
    #[test]
    fn the_producer_records_the_flags_that_change_the_code() {
        let line = [
            "-Wdate-time",
            "-D_FORTIFY_SOURCE=3",
            "-g",
            "-O2",
            "-fno-omit-frame-pointer",
            "-ffile-prefix-map=/home/tam=.",
            "-flto=auto",
            "-ffat-lto-objects",
            "-fstack-protector-strong",
            "-fstack-clash-protection",
            "-Wformat",
            "-Werror=format-security",
            "-fcf-protection",
            "-I",
            "include",
            "-o",
            "a.o",
            "-c",
            "-Xlinker",
            "-zrelro",
            "-Wl,-z,now",
            "-std=gnu11",
            "a.c",
        ];
        let (opts, _) = compile(&line);
        assert_eq!(
            opts.switches,
            [
                "-g",
                "-O2",
                "-fno-omit-frame-pointer",
                "-flto=auto",
                "-ffat-lto-objects",
                "-fstack-protector-strong",
                "-fstack-clash-protection",
                "-fcf-protection",
                "-std=gnu11",
            ]
        );

        let (opts, _) = compile(&["-c", "-g", "-O2", "-gno-record-gcc-switches", "a.c"]);
        assert!(opts.switches.is_empty(), "{:?}", opts.switches);
        let (opts, _) =
            compile(&["-c", "-gno-record-gcc-switches", "-O2", "-grecord-gcc-switches", "a.c"]);
        assert_eq!(opts.switches, ["-O2"], "the last one wins, and neither is recorded");
        let (opts, _) = compile(&["-c", "-g", "-gz=none", "a.c"]);
        assert_eq!(opts.switches, ["-g"], "-gz does not change the code and is not recorded");
    }

    /// Five flags rather than one with an argument, which is how gcc spells them, and the negative
    /// spelled four ways because a build that turns one off writes whichever it turned on.
    #[test]
    fn the_stack_protector_is_five_flags_and_the_last_one_wins() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.protector, Protector::None, "gcc protects nothing unless it was asked");

        for (flag, want) in [
            ("-fstack-protector", Protector::Buffers),
            ("-fstack-protector-strong", Protector::Strong),
            ("-fstack-protector-all", Protector::All),
            ("-fstack-protector-explicit", Protector::Explicit),
        ] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.protector, want, "{flag}");
        }

        // What a package build does: the strong one in the global flags and one directory that
        // cannot have a protector turning it off on the line after.
        for off in
            ["-fno-stack-protector", "-fno-stack-protector-strong", "-fno-stack-protector-explicit"]
        {
            let (opts, _) = compile(&["-c", "-fstack-protector-strong", off, "a.c"]);
            assert_eq!(opts.protector, Protector::None, "{off}");
        }
        let (opts, _) = compile(&["-c", "-fno-stack-protector", "-fstack-protector-all", "a.c"]);
        assert_eq!(opts.protector, Protector::All, "the last one wins either way round");
    }

    /// A switch rather than a level, because how a frame is taken is one question and which
    /// functions get a canary is another, and gcc spells it that way for the same reason.
    #[test]
    fn taking_a_frame_a_page_at_a_time_is_off_until_it_is_asked_for() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.stack_clash, "gcc takes a frame in one subtraction unless it was asked");

        let (opts, _) = compile(&["-c", "-fstack-clash-protection", "a.c"]);
        assert!(opts.stack_clash);

        // The same shape a package build uses for the protector: on in the global flags and off
        // for the one directory that cannot have it.
        let (opts, _) =
            compile(&["-c", "-fstack-clash-protection", "-fno-stack-clash-protection", "a.c"]);
        assert!(!opts.stack_clash);
        let (opts, _) =
            compile(&["-c", "-fno-stack-clash-protection", "-fstack-clash-protection", "a.c"]);
        assert!(opts.stack_clash, "the last one wins either way round");

        // The two are independent, since one is about the frame and the other about the function.
        let (opts, _) =
            compile(&["-c", "-fstack-clash-protection", "-fstack-protector-strong", "a.c"]);
        assert!(opts.stack_clash);
        assert_eq!(opts.protector, Protector::Strong);
    }

    /// One flag with an argument rather than a family of spellings, because what it asks about is
    /// which of the two edges of a control flow transfer is checked and the two are not separate
    /// questions to the hardware.
    #[test]
    fn which_control_flow_edges_are_checked_is_asked_for_by_name() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.control, Control::None, "gcc's default on the targets this compiler has");

        for (arg, want) in [
            ("-fcf-protection", Control::Full),
            ("-fcf-protection=full", Control::Full),
            ("-fcf-protection=branch", Control::Branch),
            ("-fcf-protection=return", Control::Return),
            ("-fcf-protection=none", Control::None),
            ("-fcf-protection=check", Control::Check),
        ] {
            let (opts, _) = compile(&["-c", arg, "a.c"]);
            assert_eq!(opts.control, want, "{arg}");
        }

        // The shape a package build uses: on in the global flags and off for the one directory
        // that cannot have it, whichever of the two spellings of off it reaches for.
        let (opts, _) = compile(&["-c", "-fcf-protection=full", "-fno-cf-protection", "a.c"]);
        assert_eq!(opts.control, Control::None);
        let (opts, _) = compile(&["-c", "-fno-cf-protection", "-fcf-protection=branch", "a.c"]);
        assert_eq!(opts.control, Control::Branch, "the last one wins either way round");

        // Which functions open with a pad is a second question, asked apart from the first, and
        // gcc takes it on a command line that asked for no pads at all.
        assert!(!opts.manual_endbr);
        let (opts, _) = compile(&[KERNEL_X86, "-c", "-mmanual-endbr", "a.c"]);
        assert!(opts.manual_endbr);
        let (opts, _) = compile(&[KERNEL_X86, "-c", "-mmanual-endbr", "-mno-manual-endbr", "a.c"]);
        assert!(!opts.manual_endbr, "the last one wins");
    }

    /// The profiler is asked for by two spellings, and where its hook goes by two more.
    ///
    /// The two halves are separate on purpose. `-mfentry` on its own says where a call would go and
    /// asks for no call, which is what gcc does with it, and a build system that sets it globally
    /// and asks for the profile per directory needs that to be true rather than an error.
    ///
    /// The link is asserted alongside, because the flag changes it too and a build that compiled
    /// with it and linked without it is a program that calls the hook everywhere and never writes a
    /// profile.
    #[test]
    fn the_profiler_and_where_its_hook_goes_are_two_separate_questions() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.profile);
        assert_eq!(opts.hook, Hook::Platform, "neither was named, so the target decides");

        for arg in ["-pg", "-p"] {
            let (opts, _) = compile(&["-c", arg, "a.c"]);
            assert!(opts.profile, "{arg}");
            let (link, _) = linking(&[arg, "a.c"]);
            assert!(link.profile, "{arg} changes the link as well");
        }

        for (arg, want) in [("-mfentry", Hook::Early), ("-mno-fentry", Hook::Late)] {
            let (opts, _) = compile(&["-c", arg, "a.c"]);
            assert_eq!(opts.hook, want, "{arg}");
            assert!(!opts.profile, "{arg} asks for no call of its own");
        }

        let (opts, _) = compile(&["-c", "-mfentry", "-mno-fentry", "-pg", "a.c"]);
        assert_eq!(opts.hook, Hook::Late, "the last one wins");
        assert!(opts.profile);
    }

    /// Where the profiler's call goes and where it is listed, named on the command line, last one
    /// winning and empty meaning the target's again; on x86-64 only, as the other two are.
    #[test]
    fn the_hook_and_its_list_can_be_named_on_x86_64() {
        let (opts, _) = compile(&[KERNEL_X86, "-c", "a.c"]);
        assert_eq!((opts.fentry_name, opts.fentry_section), (None, None));
        let (opts, _) = compile(&[
            KERNEL_X86,
            "-mfentry-name=one",
            "-mfentry-name=hook",
            "-mfentry-section=calls",
            "-c",
            "a.c",
        ]);
        assert_eq!(opts.fentry_name.as_deref(), Some("hook"));
        assert_eq!(opts.fentry_section.as_deref(), Some("calls"));
        let (opts, _) = compile(&[KERNEL_X86, "-mfentry-name=hook", "-mfentry-name=", "-c", "a.c"]);
        assert_eq!(opts.fentry_name, None);
        for flag in ["-mfentry-name=hook", "-mfentry-section=calls"] {
            assert_eq!(
                refused(&[KERNEL_ARM64, flag, "-c", "a.c"]),
                format!("unknown option `{flag}`")
            );
        }
    }

    /// How much room a patcher is promised, which is one number or two.
    ///
    /// A command line that did not ask is asserted alongside, because the flag has to be written to
    /// mean anything and a build that reserved room nobody asked for would grow every function in
    /// it for nothing.
    #[test]
    fn the_room_a_patcher_is_promised_is_a_number_of_bytes_and_where_they_go() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.patchable, Patchable::default());
        assert!(!opts.patchable.any(), "nothing is reserved unless it was asked for");

        let (opts, _) = compile(&["-c", "-fpatchable-function-entry=16", "a.c"]);
        assert_eq!(opts.patchable, Patchable { total: 16, before: 0 });

        let (opts, _) = compile(&["-c", "-fpatchable-function-entry=5,3", "a.c"]);
        assert_eq!(opts.patchable, Patchable { total: 5, before: 3 });
        assert_eq!(opts.patchable.after(), 2);

        // The last one wins, which is what every other flag of this shape does and what a build
        // that adds one to a command line it did not write is relying on.
        let (opts, _) = compile(&[
            "-c",
            "-fpatchable-function-entry=5,3",
            "-fpatchable-function-entry=2",
            "a.c",
        ]);
        assert_eq!(opts.patchable, Patchable { total: 2, before: 0 });
    }

    /// And a request nothing could satisfy is refused rather than rounded into one that can be.
    #[test]
    fn room_in_front_of_the_label_that_is_more_than_the_room_asked_for_is_refused() {
        for arg in ["-fpatchable-function-entry=1,2", "-fpatchable-function-entry=x"] {
            let e = parse_args(&args(&["-c", arg, "a.c"])).unwrap_err();
            assert!(e.message.contains("is not an amount of room to reserve"), "{}", e.message);
        }
    }

    /// What wraps rather than being undefined, which is two questions and three flags.
    ///
    /// The older flag is the pair of the newer two, which is gcc's own reading of it, so a build
    /// that writes `-fno-strict-overflow` gets both and a build that writes one of the others gets
    /// only what it asked for.
    #[test]
    fn what_overflows_rather_than_being_undefined_is_asked_for_two_ways() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping::NONE, "nothing wraps unless it was asked for");

        let (opts, _) = compile(&["-c", "-fwrapv", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping { signed: true, pointer: false, trap: false });

        let (opts, _) = compile(&["-c", "-fwrapv-pointer", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping { signed: false, pointer: true, trap: false });

        let (opts, _) = compile(&["-c", "-fno-strict-overflow", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping::ALL);

        // And the last one wins, in both directions. A build that turns one of these on globally
        // and off for one directory is relying on that, and so is one that writes the pair and
        // then takes half of it back.
        let (opts, _) = compile(&["-c", "-fwrapv", "-fno-wrapv", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping::NONE);

        let (opts, _) = compile(&["-c", "-fno-strict-overflow", "-fstrict-overflow", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping::NONE);

        let (opts, _) = compile(&["-c", "-fno-strict-overflow", "-fno-wrapv-pointer", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping { signed: true, pointer: false, trap: false });
    }

    /// And the other answer to the signed question cannot be held at the same time as the first.
    ///
    /// A program cannot both wrap and stop, so writing both is writing a contradiction, and gcc
    /// resolves it by letting the last one win rather than by reporting anything. That was measured
    /// against gcc 16 rather than read out of the manual, which says nothing about it: `-ftrapv
    /// -fwrapv` emits no checked calls and `-fwrapv -ftrapv` emits them.
    #[test]
    fn a_signed_overflow_that_stops_is_the_other_answer_and_not_a_third_one() {
        let (opts, _) = compile(&["-c", "-ftrapv", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping { signed: false, pointer: false, trap: true });

        let (opts, _) = compile(&["-c", "-fwrapv", "-ftrapv", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping { signed: false, pointer: false, trap: true });

        let (opts, _) = compile(&["-c", "-ftrapv", "-fwrapv", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping { signed: true, pointer: false, trap: false });

        let (opts, _) = compile(&["-c", "-ftrapv", "-fno-strict-overflow", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping::ALL);

        let (opts, _) = compile(&["-c", "-ftrapv", "-fno-trapv", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping::NONE);

        // And the flag that says what may be assumed says nothing about what happens, so it leaves
        // this alone where it takes the wrapping away. gcc does the same.
        let (opts, _) = compile(&["-c", "-ftrapv", "-fstrict-overflow", "a.c"]);
        assert_eq!(opts.wrapping, Wrapping { signed: false, pointer: false, trap: true });
    }

    /// What a plain `char` is, which is four spellings of two answers and nothing by default.
    ///
    /// Nothing is the target's own answer and has to stay distinct from both of the others, since
    /// the same command line means a signed `char` on x86-64 and an unsigned one on Linux's arm64.
    /// The negative spellings are the other flag rather than a way of asking for the default, which
    /// was measured against gcc 16: `-fno-signed-char` defines `__CHAR_UNSIGNED__` and
    /// `-fno-unsigned-char` does not.
    #[test]
    fn the_signedness_of_a_plain_char_is_asked_for_in_four_ways() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.char_signed, None);

        for flag in ["-fsigned-char", "-fno-unsigned-char"] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.char_signed, Some(true), "{flag}");
        }

        for flag in ["-funsigned-char", "-fno-signed-char"] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.char_signed, Some(false), "{flag}");
        }

        // And the last one wins, which is what a build that sets one globally and the other for a
        // directory relies on.
        let (opts, _) = compile(&["-c", "-funsigned-char", "-fsigned-char", "a.c"]);
        assert_eq!(opts.char_signed, Some(true));

        // And what is asked for reaches the target, because that is what every other part of the
        // compiler asks. The triple is one whose own answer is the opposite, so a session that
        // ignored the flag would still read as signed here.
        let (opts, _) =
            compile(&["-c", "--target=aarch64-unknown-linux-gnu", "-fsigned-char", "a.c"]);
        assert!(Session::new(*opts).target.char_is_signed);
        let (opts, _) = compile(&["-c", "--target=aarch64-unknown-linux-gnu", "a.c"]);
        assert!(!Session::new(*opts).target.char_is_signed);
    }

    /// And the size of an enumeration, which is one question with two spellings.
    #[test]
    fn the_smallest_enumeration_is_asked_for_and_taken_back() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.short_enums);

        let (opts, _) = compile(&["-c", "-fshort-enums", "a.c"]);
        assert!(opts.short_enums);

        let (opts, _) = compile(&["-c", "-fshort-enums", "-fno-short-enums", "a.c"]);
        assert!(!opts.short_enums);

        let (opts, _) = compile(&["-c", "-fno-short-enums", "-fshort-enums", "a.c"]);
        assert!(opts.short_enums);
    }

    /// And Microsoft's reading of an anonymous member, which the target answers where the command
    /// line said nothing. gcc's mingw build has it on and its Linux build has it off, so a header
    /// that closes a nameless union with a macro that expands to nothing is read the way the
    /// compiler that platform ships would read it.
    #[test]
    fn the_microsoft_reading_of_a_member_follows_the_target_until_it_is_asked_for() {
        // Named rather than left to the host, since the answer this asks for is the one a target
        // that is not Windows gives and on a Windows machine the host is not one of those.
        let (opts, _) = compile(&[LINUX, "-c", "a.c"]);
        assert!(!Session::new(*opts).ms_extensions());

        let (opts, _) = compile(&["-c", "--target=x86_64-pc-windows-gnu", "a.c"]);
        assert!(Session::new(*opts).ms_extensions());

        let (opts, _) = compile(&["-c", "-fms-extensions", "a.c"]);
        assert!(Session::new(*opts).ms_extensions());

        let (opts, _) =
            compile(&["-c", "--target=x86_64-pc-windows-gnu", "-fno-ms-extensions", "a.c"]);
        assert!(!Session::new(*opts).ms_extensions());
    }

    /// And a value nothing means is refused rather than taken for the nearest thing it looks like.
    ///
    /// `-fcf-protection=all` is the spelling somebody writes from memory, and a compiler that read
    /// it as `full` would be guessing, while one that let it fall through to the optimizer's `-f`
    /// family would report it as an unknown pass. Neither is the news the build wants.
    #[test]
    fn a_control_flow_protection_nothing_means_is_refused() {
        let e = parse_args(&args(&["-c", "-fcf-protection=all", "a.c"])).unwrap_err();
        assert!(e.message.contains("is not a control flow protection"), "{}", e.message);
        assert!(e.message.contains("full, branch, return, none or check"), "{}", e.message);
    }

    #[test]
    fn mingw_subsystem_and_unicode_flags_are_taken_last_one_winning() {
        let (link, _) = linking(&["-mwindows", "-municode", "-mthreads", "-static-libgcc", "a.c"]);
        assert!(link.gui && link.unicode);
        let (link, _) = linking(&["-mwindows", "-mconsole", "a.c"]);
        assert!(!link.gui);
        let (opts, _) = compile(&["-municode", "-c", "a.c"]);
        assert!(opts.defines.iter().any(|define| define == "UNICODE"), "{:?}", opts.defines);
    }

    #[test]
    fn the_link_flags_are_collected_apart_from_the_compilation() {
        let (link, _) = linking(&[
            "-static",
            "-nostartfiles",
            "-rdynamic",
            "-s",
            "-fuse-ld=mold",
            "-L/opt/lib",
            "-B",
            "/opt/tools",
            "a.c",
        ]);
        assert!(link.is_static);
        assert!(link.no_startfiles);
        let (both, _) = linking(&["-static-pie", "a.c"]);
        assert!(both.is_static && both.pie == Some(true));
        for line in [["-pie", "-static", "a.c"], ["-static", "-pie", "a.c"]] {
            let (plain, _) = linking(&line);
            assert!(plain.is_static && plain.pie == Some(false), "{line:?}");
        }
        assert!(link.export_dynamic);
        assert!(link.strip);
        assert_eq!(link.use_ld.as_deref(), Some("mold"));
        assert_eq!(link.search, vec![PathBuf::from("/opt/lib")]);
        assert_eq!(link.prefixes, vec![PathBuf::from("/opt/tools")]);
    }

    #[test]
    fn a_mingw_link_names_its_output_the_way_mingw_gcc_does() {
        // gcc puts `.exe` on a DLL's name as well when it has no extension, so `-shared` is not an
        // exception, and `-c` links nothing and is. tamnd/rucc#2152.
        let mingw = "--target=x86_64-windows-gnu";
        let (_, plan) = linking(&[mingw, "a.c", "-o", "foo"]);
        assert_eq!(plan.link.expect("expected a link step").output, "foo.exe");
        let (_, plan) = linking(&[mingw, "-shared", "a.c", "-o", "x"]);
        assert_eq!(plan.link.expect("expected a link step").output, "x.exe");
        let (_, plan) = linking(&[mingw, "-shared", "a.c", "-o", "x.dll"]);
        assert_eq!(plan.link.expect("expected a link step").output, "x.dll");
        let (_, plan) = compile(&[mingw, "-c", "a.c", "-o", "x"]);
        assert!(plan.link.is_none());
        assert_eq!(plan.jobs[0].output, Output::File("x".into()));
        let (_, plan) = linking(&[LINUX, "a.c", "-o", "foo"]);
        assert_eq!(plan.link.expect("expected a link step").output, "foo");
    }

    #[test]
    fn a_comma_in_dash_wl_separates_two_arguments() {
        // The target is written down because the name of the object is derived from it, and `a.o`
        // on a Linux host is `a.obj` on a Windows one. What is under test is the splitting of the
        // argument, which has nothing to do with either.
        let (_, plan) = linking(&[LINUX, "-Wl,-rpath,/opt/lib", "-Xlinker", "--as-needed", "a.c"]);
        let link = plan.link.expect("expected a link step");
        assert_eq!(
            link.inputs,
            vec![
                link::Item::Linker("-rpath".into()),
                link::Item::Linker("/opt/lib".into()),
                link::Item::Linker("--as-needed".into()),
                link::Item::File("a.o".into()),
            ]
        );
    }

    /// The MinGW link flags Postgres's `meson.build` writes on the compiler line, which are all
    /// behind `-Wl,` and all words for lld's MinGW driver, reach the link as those words, with
    /// `--stack,N` split in two and `--out-implib=` left whole. tamnd/rucc#1993.
    #[test]
    fn the_mingw_link_flags_postgres_writes_reach_the_linker() {
        let (_, plan) = linking(&[
            "--target=x86_64-w64-mingw32",
            "a.c",
            "-Wl,--allow-multiple-definition",
            "-Wl,--disable-auto-import",
            "-Wl,--stack,4194304",
            "-Wl,--export-all-symbols",
            "-Wl,--out-implib=libpostgres.exe.a",
            "-o",
            "postgres.exe",
        ]);
        let link = plan.link.expect("expected a link step");
        let words: Vec<&str> = link
            .inputs
            .iter()
            .filter_map(|item| match item {
                link::Item::Linker(word) => Some(word.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            words,
            [
                "--allow-multiple-definition",
                "--disable-auto-import",
                "--stack",
                "4194304",
                "--export-all-symbols",
                "--out-implib=libpostgres.exe.a",
            ]
        );
        assert_eq!(link.output, "postgres.exe");
    }

    /// The Darwin link flags Postgres writes on the compiler line, from `Makefile.shlib`,
    /// `Makefile.darwin` and `meson.build`, each taken with its argument in the order written.
    /// tamnd/rucc#2010.
    #[test]
    fn the_darwin_link_flags_postgres_writes_are_taken() {
        let mac = "--target=aarch64-apple-darwin";
        let (link, _) = linking(&[
            mac,
            "-dynamiclib",
            "-install_name",
            "/usr/local/pgsql/lib/libpq.5.dylib",
            "-compatibility_version",
            "5",
            "-current_version",
            "5.18",
            "-exported_symbols_list",
            "exports.list",
            "-headerpad_max_install_names",
            "-isysroot",
            "/sdk",
            "-mmacosx-version-min=13.0",
            "-arch",
            "arm64",
            "a.c",
        ]);
        assert!(link.shared && !link.bundle);
        assert_eq!(link.sysroot, Some(PathBuf::from("/sdk")));
        assert_eq!(link.os_version, rucc_tuple::Version::parse("13.0"));
        let apple: Vec<(&str, Option<&str>)> =
            link.apple.iter().map(|(flag, value)| (flag.as_str(), value.as_deref())).collect();
        assert_eq!(
            apple,
            [
                ("-install_name", Some("/usr/local/pgsql/lib/libpq.5.dylib")),
                ("-compatibility_version", Some("5")),
                ("-current_version", Some("5.18")),
                ("-exported_symbols_list", Some("exports.list")),
                ("-headerpad_max_install_names", None),
            ]
        );

        // A module, which is a bundle checked against the server that will load it.
        let (link, _) = linking(&[mac, "a.c", "-bundle", "-bundle_loader", "postgres", "-o", "m"]);
        assert!(link.bundle && !link.shared);
        assert_eq!(link.apple, [("-bundle_loader".to_owned(), Some("postgres".to_owned()))]);

        // `-shared` is a dynamic library on a Mac, as it is to clang.
        let (link, _) = linking(&[mac, "-shared", "a.c"]);
        assert!(link.shared && !link.bundle);

        // And a flag at the end of the line with its argument missing is said to be.
        let said = refused(&[mac, "a.c", "-bundle_loader"]);
        assert_eq!(said, "-bundle_loader requires an argument");
    }

    /// A run path keeps its place among the files, as `-Wl,-rpath,<dir>` does, on every target.
    #[test]
    fn a_run_path_on_the_compiler_line_goes_to_the_linker_where_it_was() {
        let (_, plan) = linking(&[LINUX, "-rpath", "/opt/lib", "a.c"]);
        let link = plan.link.expect("expected a link step");
        assert_eq!(
            link.inputs,
            vec![
                link::Item::Linker("-rpath".into()),
                link::Item::Linker("/opt/lib".into()),
                link::Item::File("a.o".into()),
            ]
        );
    }

    /// The Apple flags on a target whose linker has never heard of them are refused with clang's
    /// words, and `-arch` has to say what the target already says.
    #[test]
    fn the_darwin_link_flags_are_for_apple_targets_only() {
        for flag in ["-dynamiclib", "-bundle", "-headerpad_max_install_names"] {
            let said = refused(&[LINUX, flag, "a.c"]);
            assert!(said.starts_with(&format!("unsupported option '{flag}' for target")), "{said}");
        }
        let said = refused(&[LINUX, "-bundle_loader", "postgres", "a.c"]);
        assert!(said.starts_with("unsupported option '-bundle_loader'"), "{said}");
        let said = refused(&[LINUX, "-arch", "x86_64", "a.c"]);
        assert!(said.starts_with("unsupported option '-arch'"), "{said}");

        let mac = "--target=aarch64-apple-darwin";
        let (link, _) = linking(&["-arch", "arm64", mac, "a.c"]);
        assert!(link.apple.is_empty());
        let said = refused(&[mac, "-arch", "x86_64", "a.c"]);
        assert!(said.starts_with("-arch x86_64: the target is"), "{said}");
        let said = refused(&[mac, "-arch", "arm64", "-arch", "x86_64", "a.c"]);
        assert!(said.starts_with("-arch x86_64"), "{said}");
    }

    #[test]
    fn a_word_for_the_linker_keeps_its_place_among_the_files_too() {
        // What libtool writes around a set of convenience archives, and what #1279 was. Both words
        // are about the files between them, so the pair collected out of the line and appended to
        // the end is two options that bracket nothing and an archive that went in empty.
        let (_, plan) = linking(&[
            "--target=x86_64-unknown-linux-gnu",
            "a.c",
            "-Wl,--whole-archive",
            "libaesni.a",
            "-Wl,--no-whole-archive",
            "-lm",
        ]);
        let link = plan.link.expect("expected a link step");
        assert_eq!(
            link.inputs,
            vec![
                link::Item::File("a.o".into()),
                link::Item::Linker("--whole-archive".into()),
                link::Item::File("libaesni.a".into()),
                link::Item::Linker("--no-whole-archive".into()),
                link::Item::Library("m".into()),
            ]
        );
        // And it is not a job, because there is nothing to compile in a word for the linker.
        assert_eq!(plan.jobs.len(), 2);
    }

    #[test]
    fn a_word_for_the_linker_on_a_dash_c_line_is_dropped_without_a_word() {
        // GCC says nothing about one either. `-Wl,` on a compile line is what a build system
        // writes when one variable holds the flags for both, and a note here would be a note on
        // every compile of every autotools project.
        let (_, plan) = linking(&["-c", "-Wl,--as-needed", "a.c"]);
        assert!(plan.link.is_none());
        assert!(plan.notes.is_empty(), "{:?}", plan.notes);
        assert_eq!(plan.jobs.len(), 1);
    }

    #[test]
    fn a_library_keeps_its_place_between_the_objects() {
        // Link order is semantic: `-lm` written between two files resolves for the one before
        // it and not for the one after, so a library cannot be collected into a list of its own.
        // The target is named because the suffix of an object is the target's and this asserts
        // on the names: the same command line on a Windows host plans two `.obj` files.
        let (_, plan) = linking(&["--target=x86_64-unknown-linux-gnu", "a.c", "-lm", "b.c"]);
        let link = plan.link.expect("expected a link step");
        assert_eq!(
            link.inputs,
            vec![
                link::Item::File("a.o".into()),
                link::Item::Library("m".into()),
                link::Item::File("b.o".into()),
            ]
        );
        // And it is not a job, because there is nothing to compile in a library.
        assert_eq!(plan.jobs.len(), 2);
    }

    #[test]
    fn a_library_on_a_dash_c_line_is_a_note_rather_than_an_error() {
        let (_, plan) = linking(&["-c", "-lm", "a.c"]);
        assert!(plan.link.is_none());
        assert!(plan.notes.iter().any(|n| n.contains("-lm")), "{:?}", plan.notes);
    }

    #[test]
    fn the_sysroot_reaches_the_linker_as_well_as_the_headers() {
        let (link, _) = linking(&["--sysroot=/opt/root", "a.c"]);
        assert_eq!(link.sysroot, Some(PathBuf::from("/opt/root")));
    }

    fn printed(s: &[&str]) -> String {
        match parse_args(&args(s)).expect("expected an answer") {
            Action::Print(line) => line,
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    fn refused(s: &[&str]) -> String {
        parse_args(&args(s)).expect_err("expected a refusal").message
    }

    /// The dpkg file that Debian passes for `hardening=-pie`, and the Red Hat one that adds `-pie`.
    #[test]
    fn a_spec_file_of_a_distribution_adds_its_flags() {
        let tree = TempTree::new(
            "specs",
            &[
                (
                    "no-pie-link.specs",
                    "*self_spec:\n+ %{!shared:%{!r:%{!fPIE:%{!pie:-fno-PIE -no-pie}}}}\n",
                ),
                ("redhat-hardened-ld", "*self_spec:\n+ %{!static:%{!shared:%{!r:-pie}}}\n"),
            ],
        );
        let no_pie = format!("-specs={}", tree.path("no-pie-link.specs"));
        let (link, _) = linking(&[LINUX, &no_pie, "a.c"]);
        assert_eq!(link.pie, Some(false));
        let (link, _) = linking(&[LINUX, &no_pie, "-shared", "a.c"]);
        assert_eq!(link.pie, None);
        let hardened = format!("-specs={}", tree.path("redhat-hardened-ld"));
        let (link, _) = linking(&[LINUX, &hardened, "a.c"]);
        assert_eq!(link.pie, Some(true));
    }

    #[test]
    fn a_configuration_file_puts_its_flags_before_the_command_line() {
        let tree = TempTree::new(
            "config",
            &[
                (
                    "install/x86_64-linux-gnu.cfg",
                    "-fstack-protector-strong # the default\n\n-D_FORTIFY_SOURCE=3 -O2\n",
                ),
                ("etc/x86_64-linux-gnu.cfg", "-fcf-protection\n"),
                ("etc/aarch64-linux-gnu.cfg", "-mbranch-protection=standard\n"),
            ],
        );
        let dirs = [PathBuf::from(tree.path("install")), PathBuf::from(tree.path("etc"))];
        let line = |words: &[&str]| with_config(args(words), &dirs).expect("the files read");
        let (words, files) = line(&["--target=x86_64-linux-gnu", "-fno-stack-protector", "a.c"]);
        assert_eq!(
            words,
            args(&[
                "-fstack-protector-strong",
                "-D_FORTIFY_SOURCE=3",
                "-O2",
                "-fcf-protection",
                "--target=x86_64-linux-gnu",
                "-fno-stack-protector",
                "a.c"
            ])
        );
        assert_eq!(files.len(), 2, "{files:?}");
        assert!(files[0].ends_with("x86_64-linux-gnu.cfg") && files[0].contains("install"));
        // The row of the target, and no file for a row that has none.
        let (words, _) = line(&["--target=aarch64-unknown-linux-gnu", "a.c"]);
        assert_eq!(words[0], "-mbranch-protection=standard");
        let (words, files) = line(&["--target=riscv64-linux-gnu", "a.c"]);
        assert_eq!((words.len(), files.len()), (2, 0));
        // clang's flag to read none.
        let (words, files) = line(&["--target=x86_64-linux-gnu", "--no-default-config", "a.c"]);
        assert_eq!((words.len(), files.len()), (3, 0));
        // `-v` names each file it read.
        let mut opts = Options::new("x86_64-linux-gnu".parse().unwrap());
        opts.config_files = vec!["/etc/rucc/x86_64-linux-gnu.cfg".to_owned()];
        let banner = verbose_banner(&opts);
        assert!(
            banner.ends_with("\nConfiguration file: /etc/rucc/x86_64-linux-gnu.cfg\n"),
            "{banner}"
        );
        assert!(parse_args(&args(&["--no-default-config", "-c", "a.c"])).is_ok());
    }

    #[test]
    fn dash_v_with_no_input_prints_the_banner_build_systems_read() {
        for line in [&["-v"][..], &["-v", "-pthread"], &["-v", "--target=aarch64-linux-gnu"]] {
            let Action::Verbose(text) = parse_args(&args(line)).expect("an answer") else {
                panic!("{line:?} did not print the banner");
            };
            let first = text.lines().next().unwrap();
            assert!(
                first.starts_with("rucc version ") && first.contains("gcc version 16."),
                "{text}"
            );
            assert!(text.contains("\nThread model: posix\n"), "{text}");
            assert!(text.contains("\nInstalledDir: "), "{text}");
        }
        let Action::Verbose(text) =
            parse_args(&args(&["-v", "--target=aarch64-linux-gnu"])).unwrap()
        else {
            unreachable!()
        };
        assert!(text.contains("\nTarget: aarch64-linux-gnu\n"), "{text}");
        assert!(matches!(
            parse_args(&args(&["-v", "a.c"])),
            Ok(Action::Compile { verbose: true, .. })
        ));
    }

    #[test]
    fn a_warning_flag_gcc_knows_is_taken_even_though_nothing_reads_it() {
        // The rule in section 4.1, and the reason for it is autoconf: a configure script finds
        // out whether a warning flag exists by passing it and looking at the exit status, so a
        // compiler that refuses one gcc knows fails a script written for gcc.
        let (opts, _) = compile(&["-Wall", "-Wextra", "-Wno-format-truncation", "-c", "a.c"]);
        assert!(!opts.warnings_are_errors);
        assert!(opts.warnings);
        // The two spellings that do mean something are still read.
        let (opts, _) = compile(&["-Werror", "-c", "a.c"]);
        assert!(opts.warnings_are_errors);
        let (opts, _) = compile(&["-Werror", "-Wno-error", "-c", "a.c"]);
        assert!(!opts.warnings_are_errors);
        let (opts, _) = compile(&["-w", "-c", "a.c"]);
        assert!(!opts.warnings);
        // Off without being asked, the way gcc has it off, and both spellings are read.
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.system_header_warnings);
        let (opts, _) = compile(&["-Wsystem-headers", "-c", "a.c"]);
        assert!(opts.system_header_warnings);
        let (opts, _) = compile(&["-Wsystem-headers", "-Wno-system-headers", "-c", "a.c"]);
        assert!(!opts.system_header_warnings);
        // Only what the standard requires is fatal under it, which is not `-Werror`.
        let (opts, _) = compile(&["-pedantic-errors", "-c", "a.c"]);
        assert!(opts.pedantic && !opts.warnings_are_errors);
        let warning = |code| {
            rucc_diag::Diagnostic::warning("x".to_owned(), rucc_diag::Span::DUMMY).with_code(code)
        };
        assert!(opts.named_warnings.promoted(&warning("E0513"), false));
        assert!(!opts.named_warnings.promoted(&warning("E0770"), false));
    }

    #[test]
    fn a_warning_flag_gcc_refuses_is_refused_here_too() {
        // Postgres's meson build probes these, and with rucc taking them it ended up passing four
        // clang warnings that the gcc build had dropped.
        for flag in ["-Wcast-function-type-strict", "-Wunused-command-line-argument"] {
            assert_eq!(refused(&[flag, "-c", "a.c"]), format!("unknown option `{flag}`"));
        }
        assert_eq!(
            refused(&["-Werror=unguarded-availability-new", "-c", "a.c"]),
            "`-Werror=unguarded-availability-new`: no option `-Wunguarded-availability-new`"
        );
        assert!(refused(&["-Wno-error=nonsense", "-c", "a.c"]).contains("no option `-Wnonsense`"));
        // gcc takes `-Wno-` of a name it does not know, and says nothing unless something else
        // is said, and it takes C++ and Fortran names on a C compile.
        for flag in
            ["-Wno-cast-function-type-strict", "-Werror=format", "-Wformat=2", "-Wabi-tag", "-W"]
        {
            compile(&[flag, "-c", "a.c"]);
        }
    }

    #[test]
    fn an_argument_for_a_separate_tool_is_refused_rather_than_dropped() {
        // Every one of these says something about the output, so the wrong answer is silence.
        assert!(refused(&["-Wa,--execstack", "-c", "a.c"]).contains("`--execstack`"));
        assert!(refused(&["-Wp,-C", "-c", "a.c"]).contains("separate preprocessor"));
        assert!(refused(&["-specs=/no/such/file", "a.c"]).starts_with("-specs=/no/such/file"));
        assert!(refused(&["-mcmodel=large", "-c", "a.c"]).contains("tiny code models"));
        assert!(refused(&["-gdwarf-3", "-c", "a.c"]).contains("DWARF 4 and 5"));
        // The word size the target does not have, which is a target this compiler was not asked
        // for rather than a flag it does not know.
        let no32 = refused(&["--target=aarch64-unknown-linux-gnu", "-m32", "-c", "a.c"]);
        assert!(no32.contains("32 bit target"), "{no32}");
    }

    /// `-gz` and the two spellings of the split, which are the two questions about the shape of
    /// the debug output rather than about how much of it there is.
    ///
    /// All four values are written: zlib in both of its layouts and zstd in the ELF one, which is
    /// the only one zstd has.
    #[test]
    fn the_shape_of_the_debug_output_is_recorded_even_where_there_is_none_of_it() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.compress, Compress::None, "uncompressed unless somebody asks");
        assert_eq!(compile(&["-gz=none", "-c", "a.c"]).0.compress, Compress::None);
        assert_eq!(compile(&["-gz", "-c", "a.c"]).0.compress, Compress::Zlib);
        assert_eq!(compile(&["-gz=zlib", "-c", "a.c"]).0.compress, Compress::Zlib);
        assert_eq!(compile(&["-gz=zlib-gnu", "-c", "a.c"]).0.compress, Compress::ZlibGnu);
        assert_eq!(compile(&["-gz=zlib", "-gz=none", "-c", "a.c"]).0.compress, Compress::None);
        assert_eq!(compile(&["-gz=zstd", "-c", "a.c"]).0.compress, Compress::Zstd);

        // A value nothing here has heard of is refused rather than rounded to the nearest one,
        // because a build that asked for `zstd` and quietly got `zlib` would ship a file its
        // reader may not understand and would have no way of finding out.
        for bad in ["-gz=gzip", "-gz="] {
            let failed = refused(&[bad, "-c", "a.c"]);
            assert!(failed.contains("is not a way to compress"), "{bad}: {failed}");
        }

        // The split is refused in the direction that would have written a file and taken in the
        // direction that describes what happens. A build system that names the `.dwo` as an
        // output has to hear about it now rather than at the point the file is missing.
        let (opts, _) = compile(&["-gno-split-dwarf", "-g", "-c", "a.c"]);
        assert!(opts.debug_info, "the negative spelling says nothing about how much");
        let failed = refused(&["-gsplit-dwarf", "-c", "a.c"]);
        assert!(failed.contains(".dwo"), "the refusal names the file it would have written");
    }

    /// The `-flto` family, which is the whole of an optimization this compiler does not do.
    ///
    /// Taken rather than refused because ignoring it gives a correct program that is slower than
    /// it could have been, which is section 4.1's hint about speed. The values are still held to
    /// gcc's, so a command line written for clang is told rather than quietly taken.
    #[test]
    fn the_link_time_family_is_read_and_checked_and_nothing_is_done_about_it() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.lto.requested, "nothing asks unless the command line does");

        let (opts, _) = compile(&["-flto", "-c", "a.c"]);
        assert!(opts.lto.requested);
        assert_eq!(opts.lto.jobs, LtoJobs::One, "bare -flto is one process, the way gcc reads it");

        // The last of the two directions wins, the same as every other pair of `-f` spellings.
        assert!(!compile(&["-flto", "-fno-lto", "-c", "a.c"]).0.lto.requested);
        assert!(compile(&["-fno-lto", "-flto", "-c", "a.c"]).0.lto.requested);

        // A count is a count, and asking for one implies asking for the optimization.
        for (spelling, want) in [
            ("auto", LtoJobs::Auto),
            ("jobserver", LtoJobs::Jobserver),
            ("1", LtoJobs::One),
            ("8", LtoJobs::Count(8)),
        ] {
            let (opts, _) = compile(&[&format!("-flto={spelling}"), "-c", "a.c"]);
            assert_eq!(opts.lto.jobs, want, "{spelling}");
            assert!(opts.lto.requested, "{spelling} asks for it too");
        }

        // gcc refuses a zero rather than reading it as `-fno-lto`, and `thin` is clang's spelling
        // of a question gcc answers with `-flto-partition=`, so somebody who wrote it meant a
        // different compiler and gets told so here rather than getting a serial link.
        for bad in ["-flto=0", "-flto=thin", "-flto=full", "-flto=-1"] {
            let failed = refused(&[bad, "-c", "a.c"]);
            assert!(failed.contains("link time jobs"), "{bad}: {failed}");
        }

        // How the program is cut up before the work is spread over it.
        assert_eq!(compile(&["-c", "a.c"]).0.lto.partition, Partition::Balanced, "gcc's default");
        for (spelling, want) in [
            ("balanced", Partition::Balanced),
            ("1to1", Partition::OneToOne),
            ("one", Partition::One),
            ("max", Partition::Max),
            ("none", Partition::None),
        ] {
            let (opts, _) = compile(&[&format!("-flto-partition={spelling}"), "-c", "a.c"]);
            assert_eq!(opts.lto.partition, want, "{spelling}");
        }
        assert!(refused(&["-flto-partition=big", "-c", "a.c"]).contains("partitioning model"));

        // And how hard the bytecode is compressed on its way into the object, which is zstd's
        // range of levels and is the range gcc checks an argument against.
        assert_eq!(compile(&["-c", "a.c"]).0.lto.compression, None, "whatever it does by default");
        assert_eq!(compile(&["-flto-compression-level=0", "-c", "a.c"]).0.lto.compression, Some(0));
        let (opts, _) = compile(&["-flto-compression-level=19", "-c", "a.c"]);
        assert_eq!(opts.lto.compression, Some(19));
        for bad in ["-flto-compression-level=20", "-flto-compression-level=-1"] {
            let failed = refused(&[bad, "-c", "a.c"]);
            assert!(failed.contains("compression level"), "{bad}: {failed}");
        }

        // The two pairs that describe an arrangement rather than ask for one. Every object here
        // holds its machine code, so the fat spelling is what already happens and the other is a
        // smaller file rather than a different program, and the plugin pair is about a tool the
        // design in `spec/09-optimizer.md` never loads.
        for taken in [
            "-ffat-lto-objects",
            "-fno-fat-lto-objects",
            "-fuse-linker-plugin",
            "-fno-use-linker-plugin",
        ] {
            let (opts, _) = compile(&[taken, "-c", "a.c"]);
            assert!(!opts.lto.requested, "{taken} says nothing about whether to do it");
        }
    }

    /// The profile family, which is the only one here that splits down the middle.
    ///
    /// Reading a profile is taken and writing one is refused, and the line between them is the one
    /// section 4.1 draws: ignoring a request to read the counts gives a correct program that is
    /// slower than it could have been, and ignoring a request to write them means a file the build
    /// declared as an output never appears.
    #[test]
    fn reading_a_profile_is_taken_and_writing_one_is_refused() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert!(!opts.profile_data.requested, "nothing asks unless the command line does");
        assert_eq!(opts.profile_data.path, None);

        let (opts, _) = compile(&["-fprofile-use", "-c", "a.c"]);
        assert!(opts.profile_data.requested);
        assert_eq!(opts.profile_data.path, None, "beside the object, the way gcc looks");

        let (opts, _) = compile(&["-fprofile-use=/counts", "-c", "a.c"]);
        assert!(opts.profile_data.requested, "naming a path asks for it too");
        assert_eq!(opts.profile_data.path.as_deref(), Some("/counts"));

        // The last of the two directions wins, the same as every other pair of `-f` spellings.
        assert!(
            !compile(&["-fprofile-use", "-fno-profile-use", "-c", "a.c"]).0.profile_data.requested
        );
        assert!(
            compile(&["-fno-profile-use", "-fprofile-use", "-c", "a.c"]).0.profile_data.requested
        );

        // The rest of the reading half, which is where the files are and three answers about what
        // to make of what is in them.
        let (opts, _) = compile(&[
            "-fprofile-dir=/build/profiles",
            "-fprofile-abs-path",
            "-fprofile-correction",
            "-fprofile-partial-training",
            "-c",
            "a.c",
        ]);
        assert_eq!(opts.profile_data.dir.as_deref(), Some("/build/profiles"));
        assert!(opts.profile_data.absolute);
        assert!(opts.profile_data.correction);
        assert!(opts.profile_data.partial_training);

        // Arc counters, which are done, and the last of the two spellings wins.
        let (opts, _) = compile(&["-fprofile-arcs", "-c", "a.c"]);
        assert!(opts.profile_data.arcs);
        assert!(linking(&["-fprofile-arcs", "a.c"]).0.gcov);
        let (opts, _) = compile(&["-fprofile-arcs", "-fno-profile-arcs", "-c", "a.c"]);
        assert!(!opts.profile_data.arcs);
        assert!(!linking(&["-fprofile-arcs", "-fno-profile-arcs", "a.c"]).0.gcov);

        // The rest of the writing half, which is refused by name. These instrument the program
        // further and the last writes a file beside the object, and a build that got neither and
        // no message would go on to optimize against counts that were never gathered.
        for writing in [
            "-fprofile-generate",
            "-fprofile-generate=/build/profiles",
            "-fcondition-coverage",
            "-fpath-coverage",
        ] {
            let failed = refused(&[writing, "-c", "a.c"]);
            assert!(failed.contains("instrument"), "{writing}: {failed}");
        }

        // The note, and `--coverage`, which is both and the library.
        let (opts, _) = compile(&["-ftest-coverage", "-c", "a.c"]);
        assert!(opts.profile_data.notes && !opts.profile_data.arcs);
        let (opts, _) = compile(&["-ftest-coverage", "-fno-test-coverage", "-c", "a.c"]);
        assert!(!opts.profile_data.notes);
        let (opts, _) = compile(&["--coverage", "-c", "a.c"]);
        assert!(opts.profile_data.notes && opts.profile_data.arcs);
        assert!(linking(&["--coverage", "a.c"]).0.gcov);

        // The negative spelling of the refused half is what already happens, so it is taken.
        let (opts, _) = compile(&["-fno-profile-generate", "-c", "a.c"]);
        assert!(!opts.profile_data.requested, "it asks for nothing");

        // And the flags that describe the instrumentation that is refused above, which are checked
        // and dropped. Checked because a typo is worth finding here rather than on the day the
        // instrumentation lands.
        for taken in [
            "-fprofile-update=single",
            "-fprofile-update=atomic",
            "-fprofile-update=prefer-atomic",
            "-fprofile-reproducible=serial",
            "-fprofile-reproducible=parallel-runs",
            "-fprofile-reproducible=multithreaded",
            "-fprofile-values",
            "-fno-profile-values",
            "-fprofile-info-section",
            "-fprofile-filter-files=a.c",
            "-fprofile-exclude-files=b.c",
            "-fprofile-note=a.gcno",
        ] {
            let (opts, _) = compile(&[taken, "-c", "a.c"]);
            assert!(!opts.profile_data.requested, "{taken} says nothing about reading one");
        }
        assert!(refused(&["-fprofile-update=none", "-c", "a.c"]).contains("update method"));
        assert!(refused(&["-fprofile-reproducible=any", "-c", "a.c"]).contains("reproducibility"));
    }

    /// The sanitizers, which are refused by name and are the one family refused for a reason that
    /// is not about the bytes.
    ///
    /// A sanitizer is a promise that the program is watched while it runs, so a build that asked
    /// for one and was quietly given a program with no checks in it gets a test suite that passes
    /// for the wrong reason rather than a slower program.
    #[test]
    fn a_sanitizer_that_is_still_asked_for_at_the_end_of_the_line_is_refused_by_name() {
        for asked in ["address", "undefined", "thread", "kernel-address", "leak", "memory"] {
            let failed = refused(&[&format!("-fsanitize={asked}"), "-c", "a.c"]);
            assert!(failed.contains(asked), "the refusal names what was asked for: {failed}");
            assert!(failed.contains("-fsafety=detect"), "and the nearest thing: {failed}");
        }

        // A list is every name in it, and the first one still standing is the one named.
        let failed = refused(&["-fsanitize=address,undefined", "-c", "a.c"]);
        assert!(failed.contains("address"), "{failed}");

        // A name that is not one, which is worth its own message: somebody who wrote `-fsanitize`
        // with a typo in it has a different problem from somebody who wrote a real one.
        for bad in ["-fsanitize=bogus", "-fsanitize=address,bogus", "-fno-sanitize=bogus"] {
            let failed = refused(&[bad, "-c", "a.c"]);
            assert!(failed.contains("is not a sanitizer"), "{bad}: {failed}");
        }

        // gcc takes `all` only in the negative, and so does this.
        assert!(refused(&["-fsanitize=all", "-c", "a.c"]).contains("only `-fno-sanitize=all`"));

        // Asking and then taking it back is asking for nothing, which is why the answer waits for
        // the end of the line. A build whose shared flags turn a check on and whose rule for one
        // file turns it off again compiles that file here.
        for pair in [
            ["-fsanitize=address", "-fno-sanitize=address"],
            ["-fsanitize=address,undefined", "-fno-sanitize=all"],
            ["-fsanitize=undefined", "-fno-sanitize=undefined"],
        ] {
            let (opts, _) = compile(&[pair[0], pair[1], "-c", "a.c"]);
            assert_eq!(opts.safety, rucc_session::Safety::Off, "{pair:?} asked for nothing");
        }
        // And the other order still asks, because the last word is the one that counts.
        assert!(!refused(&["-fno-sanitize=address", "-fsanitize=address", "-c", "a.c"]).is_empty());

        // What a check does when it fires is an answer about checks that are refused, so there is
        // nothing left for it to change and it is taken.
        for taken in [
            "-fsanitize-recover=undefined",
            "-fno-sanitize-recover=all",
            "-fsanitize-trap=undefined",
            "-fno-sanitize-trap=all",
            "-fsanitize-undefined-trap-on-error",
            "-fsanitize-address-use-after-scope",
            "-fno-sanitize-address-use-after-scope",
            "-fsanitize-sections=.data",
        ] {
            let (opts, _) = compile(&[taken, "-c", "a.c"]);
            assert_eq!(opts.safety, rucc_session::Safety::Off, "{taken} asks for no checking");
        }
        assert!(refused(&["-fsanitize-recover=bogus", "-c", "a.c"]).contains("is not a sanitizer"));

        // Coverage instrumentation is refused rather than dropped, because a fuzzer with no
        // feedback runs blind and never says so.
        let failed = refused(&["-fsanitize-coverage=trace-pc", "-c", "a.c"]);
        assert!(failed.contains("feedback"), "{failed}");
        let failed = refused(&["-fsanitize-coverage=trace-pc-guard", "-c", "a.c"]);
        assert!(failed.contains("trace-pc or trace-cmp"), "gcc takes two of them: {failed}");
    }

    #[test]
    fn the_levels_gcc_spells_differently_are_the_levels_they_mean() {
        assert_eq!(compile(&["-O", "-c", "a.c"]).0.opt_level, OptLevel::O1);
        assert_eq!(compile(&["-Og", "-c", "a.c"]).0.opt_level, OptLevel::O1);
        assert_eq!(compile(&["-O2", "-c", "a.c"]).0.opt_level, OptLevel::O2);
    }

    #[test]
    fn the_machine_flags_that_name_what_we_already_do_are_taken_and_the_rest_are_not() {
        let line = ["--target=x86_64-unknown-linux-gnu", "-m64", "-march=x86-64-v3"];
        let (opts, _) =
            compile(&[&line[..], &["-mtune=native", "-mabi=sysv", "-c", "a.c"]].concat());
        assert_eq!(opts.target.to_string(), "x86_64-unknown-linux-gnu");
        let wrong = refused(&["--target=x86_64-unknown-linux-gnu", "-mabi=ms", "-c", "a.c"]);
        assert!(wrong.contains("sysv convention"), "{wrong}");
    }

    /// On x86 the word size picks the machine, as it does for gcc built for either, and the last
    /// `-m` and the last `--target=` count wherever they are on the line.
    #[test]
    fn the_word_size_picks_the_x86_machine() {
        let x86 = |line: &[&str]| {
            let (opts, _) = compile(&[line, &["-c", "a.c"]].concat());
            (opts.target.to_string(), opts.sixteen)
        };
        let i686 = "i686-unknown-linux-gnu".to_owned();
        let x86_64 = "x86_64-unknown-linux-gnu".to_owned();
        assert_eq!(x86(&["--target=x86_64-unknown-linux-gnu", "-m32"]), (i686.clone(), false));
        assert_eq!(x86(&["-m32", "--target=x86_64-unknown-linux-gnu"]), (i686.clone(), false));
        assert_eq!(x86(&["--target=x86_64-unknown-linux-gnu", "-m16"]), (i686.clone(), true));
        assert_eq!(x86(&["--target=i686-unknown-linux-gnu", "-m64"]), (x86_64.clone(), false));
        assert_eq!(x86(&["--target=x86_64-unknown-linux-gnu", "-m16", "-m64"]), (x86_64, false));
        assert_eq!(x86(&["--target=i686-unknown-linux-gnu", "-m32"]), (i686, false));
        let arm = refused(&["--target=aarch64-unknown-linux-gnu", "-m16", "-c", "a.c"]);
        assert!(arm.contains("-m16 is for x86"), "{arm}");
    }

    /// Whether a unit built with that command line has the extension called `name`.
    fn has(line: &[&str], name: &str) -> bool {
        let x86 = ["--target=x86_64-unknown-linux-gnu", "-c", "a.c"];
        let (opts, _) = compile(&[&x86[..], line].concat());
        opts.isa.has(rucc_target::Feature::named(name).expect("a feature"))
    }

    #[test]
    fn the_sse_flags_and_the_processor_levels_name_extensions() {
        // tamnd/rucc#2003. Every one of these was an unknown option before, and Postgres's
        // configure probe for the CRC-32C intrinsics is compiled with the first.
        assert!(has(&["-msse4.2"], "sse4.2") && has(&["-msse4.2"], "crc32"));
        assert!(has(&["-msse4.2"], "ssse3") && has(&["-msse4.2"], "popcnt"));
        assert!(!has(&[], "sse3") && !has(&[], "popcnt"));
        assert!(has(&["-mssse3"], "sse3") && !has(&["-mssse3"], "sse4.1"));
        assert!(has(&["-msse4"], "sse4.2") && !has(&["-msse4", "-mno-sse4"], "sse4.1"));
        assert!(has(&["-mpopcnt"], "popcnt") && !has(&["-mpopcnt"], "sse3"));
        assert!(has(&["-mcrc32"], "crc32"));
        assert!(has(&["-mxsave"], "xsave") && !has(&["-mxsave", "-mno-xsave"], "xsave"));
        assert!(!has(&["-msse4.2", "-mno-popcnt"], "popcnt"));
        // A processor supplies what no flag spoke for, whichever order they came in.
        assert!(has(&["-march=x86-64-v2"], "sse4.2"));
        assert!(!has(&["-march=x86-64-v2", "-mno-sse4.2"], "sse4.2"));
        assert!(!has(&["-mno-sse4.2", "-march=x86-64-v2"], "sse4.2"));
        assert!(has(&["-mno-sse4.2", "-march=x86-64-v2"], "sse4.1"));
        assert!(!has(&["-march=x86-64-v2", "-march=x86-64"], "sse3"));
        // One it has no list for is the baseline, as it was when all of them were.
        assert!(!has(&["-march=pentium-m"], "sse3"));
        assert!(has(&["-march=x86-64-v3"], "avx2"));
        // Turning off what is never on is nothing, and the flag is still gcc's.
        assert!(!has(&["-mno-avx512f"], "avx512f"));
    }

    #[test]
    fn an_extension_this_compiler_cannot_provide_for_a_whole_unit_is_refused() {
        let x86 = ["--target=x86_64-unknown-linux-gnu", "-c", "a.c"];
        let said = refused(&[&x86[..], &["-mavx2"]].concat());
        assert!(said.contains("no intrinsics for avx2"), "{said}");
        // Turning the baseline off is what a kernel asks for, and it takes the vector registers
        // away with it.
        let (opts, _) = compile(&[&x86[..], &["-mno-sse2"]].concat());
        assert!(!opts.vector && opts.x87);
        let (opts, _) = compile(&[&x86[..], &["-mno-fxsr"]].concat());
        assert!(opts.vector);
        assert!(refused(&[&x86[..], &["-msse5"]].concat()).contains("unknown option"));
        // No other target has these, whichever side of the target the flag was written on.
        let said = refused(&["-msse4.2", "--target=aarch64-linux-gnu", "-c", "a.c"]);
        assert!(said.contains("unknown option `-msse4.2`"), "{said}");
        let (opts, _) = compile(&["--target=riscv64-linux-gnu", "-march=rv64gc", "-c", "a.c"]);
        assert_eq!(opts.isa, rucc_target::Isa::NONE);
    }

    /// tamnd/rucc#2006. `-march=` on AArch64 decides the CRC32 extension, which is what
    /// `__ARM_FEATURE_CRC32` and the intrinsics in `<arm_acle.h>` follow. PostgreSQL's configure
    /// tries `-march=armv8-a+crc+simd` and then `-march=armv8-a+crc`, and gcc gives plain Armv8-A
    /// none of it.
    #[test]
    fn the_aarch64_march_decides_the_crc_extension() {
        let crc = |line: &[&str]| {
            let args = [&["--target=aarch64-linux-gnu", "-c", "a.c"][..], line].concat();
            let (opts, _) = compile(&args);
            opts.isa.has(rucc_target::Feature::aarch64("crc").expect("a feature"))
        };
        assert!(crc(&["-march=armv8-a+crc"]));
        assert!(crc(&["-march=armv8-a+crc+simd"]));
        assert!(crc(&["-march=armv8.1-a"]));
        assert!(crc(&["-march=armv9-a"]));
        assert!(!crc(&[]));
        assert!(!crc(&["-march=armv8-a"]));
        assert!(!crc(&["-march=armv8-a+simd"]));
        assert!(!crc(&["-march=armv8.2-a+nocrc"]));
        // The last one written is the one that counts, as it is for gcc.
        assert!(!crc(&["-march=armv8-a+crc", "-march=armv8-a"]));
        assert!(crc(&["-march=armv8-a", "-march=armv8-a+crc"]));
        // And `-march=` written before the target is still read for it.
        let (opts, _) = compile(&["-march=armv8-a+crc", "--target=aarch64-linux-gnu", "-c", "a.c"]);
        assert!(opts.isa.has(rucc_target::Feature::aarch64("crc").expect("a feature")));
    }

    #[test]
    fn the_thread_flag_is_a_macro_and_a_library_and_the_library_goes_last() {
        let (opts, plan) = compile(&["-pthread", "-c", "a.c"]);
        assert!(opts.defines.iter().any(|d| d == "_REENTRANT"));
        // After the input, because a static link takes what it needs from a library when it
        // reaches it and not afterwards.
        let names: Vec<&str> = plan.jobs.iter().map(|j| j.input.as_str()).collect();
        assert_eq!(names, vec!["a.c"]);
    }

    #[test]
    fn the_wasm_set_and_features_come_from_mcpu_and_the_feature_flags() {
        use rucc_target::wasm::{Cpu, Feature};
        let wasm = |flags: &[&str]| {
            let line: Vec<&str> = ["--target=wasm32-wasip1"]
                .iter()
                .chain(flags)
                .chain(&["-c", "a.c"])
                .copied()
                .collect();
            compile(&line).0.wasm
        };
        assert_eq!(wasm(&[]), Cpu::Lime1.features());
        assert_eq!(wasm(&["-mcpu=generic"]), Cpu::Generic.features());
        assert_eq!(wasm(&["-mcpu=generic", "-mcpu=mvp"]), Cpu::Mvp.features());
        // The set comes first and the flags after it, whatever the order on the line.
        assert!(wasm(&["-msimd128", "-mcpu=mvp"]).has(Feature::Simd128));
        assert!(!wasm(&["-mno-sign-ext"]).has(Feature::SignExt));
        assert!(!wasm(&["-mcpu=bleeding-edge", "-mno-simd128"]).has(Feature::RelaxedSimd));
        // The target written after the flags still decides that they are wasm flags.
        let (opts, _) =
            compile(&["-mcpu=mvp", "-mtail-call", "--target=wasm32-wasip2", "-c", "a.c"]);
        assert_eq!(opts.wasm, rucc_target::wasm::Features::of(&[Feature::TailCall]));
        // `-mtune=` is taken and changes nothing, and `-mabi=mvp` is the convention wasm32 has.
        assert_eq!(wasm(&["-mtune=x", "-mabi=mvp"]), Cpu::Lime1.features());

        let wasip1 = |flag| refused(&["--target=wasm32-wasip1", flag, "-c", "a.c"]);
        assert!(wasip1("-mcpu=lime2").contains("unknown target CPU 'lime2'"));
        assert!(wasip1("-march=lime1").contains("-mcpu="));
        assert!(wasip1("-matomics").contains("wasm32-wasip1 has no shared memory"));
        assert!(wasip1("-mabi=experimental-mv").contains("multivalue C ABI is not supported"));
        assert!(wasip1("-msimd").contains("unknown option `-msimd`"));
        // The names are wasm's and no other target's.
        assert!(refused(&["-msimd128", "-c", "a.c"]).contains("unknown option `-msimd128`"));
    }

    #[test]
    fn a_wasm_row_takes_the_thread_flag_and_says_that_it_has_one_thread() {
        let line = ["--target=wasm32-wasip1", "-pthread", "-c", "a.c"];
        let (opts, _) = compile(&line);
        assert!(opts.defines.iter().any(|d| d == "_REENTRANT"));
        assert!(!opts.wasm.has(rucc_target::wasm::Feature::Atomics));
        assert_eq!(
            notes(&line),
            ["-pthread has no effect on wasm32-wasip1: the row has one thread"]
        );
        // wasip3 has its threads already, and refuses a flag that takes away what they need.
        assert!(notes(&["--target=wasm32-wasip3", "-pthread", "-c", "a.c"]).is_empty());
        let why = refused(&["--target=wasm32-wasip3", "-mno-bulk-memory", "-c", "a.c"]);
        assert!(why.contains("wasm32-wasip3 needs bulk-memory for its threads"), "{why}");
        // `bulk-memory` turns `bulk-memory-opt` back on, so clang takes this one and so does rucc.
        let (opts, _) = compile(&["--target=wasm32-wasip3", "-mno-bulk-memory-opt", "-c", "a.c"]);
        assert!(opts.wasm.has(rucc_target::wasm::Feature::BulkMemoryOpt));
        // The object has the features that the threads need, as the macros say, also on a set
        // that does not have them.
        let (opts, _) = compile(&["--target=wasm32-wasip3", "-mcpu=mvp", "-c", "a.c"]);
        assert_eq!(opts.wasm, rucc_target::wasm::required(rucc_target::Preview::P3));
    }

    #[test]
    fn the_execution_model_is_a_link_option_of_the_wasm_rows() {
        let (link, _) = linking(&["--target=wasm32-wasip1", "-mexec-model=reactor", "a.c"]);
        assert!(link.reactor);
        let (link, _) = linking(&["--target=wasm32-wasip1", "-mexec-model=command", "a.c"]);
        assert!(!link.reactor);
        let why = refused(&["--target=wasm32-wasip1", "-mexec-model=library", "a.c"]);
        assert!(why.contains("invalid argument 'library' to -mexec-model="), "{why}");
        let why = refused(&["--target=x86_64-linux-gnu", "-mexec-model=reactor", "a.c"]);
        assert!(why.contains("for wasm32 only"), "{why}");
    }

    #[test]
    fn the_version_banner_keeps_our_first_line_and_takes_meson_down_the_gnu_path() {
        let text = printed(&["--version"]);
        let mut lines = text.lines();
        // Every harness we have reads the first line and nothing else.
        assert_eq!(lines.next(), Some(format!("rucc {VERSION}").as_str()));
        // The words meson looks for, in `mesonbuild/compilers/detect.py`.
        assert!(text.contains("Free Software Foundation"), "{text}");
        // GCC's own banner has three lines and so does this one, and the claim is the dialect.
        assert!(lines.next().is_some_and(|l| l.contains("GCC 16")), "{text}");
        assert!(lines.next().is_some() && lines.next().is_none(), "{text}");
    }

    /// The banner under a claimed release, which is GCC's shape with this compiler named where a
    /// distribution names its build. The kernel from 4.18 to 5.11 runs `grep gcc` on the first
    /// line to decide it has GCC, and every kernel copies that line into `CONFIG_CC_VERSION_TEXT`.
    #[test]
    fn a_claimed_release_puts_gcc_and_its_version_on_the_first_line() {
        let text = printed(&["-fgnuc-version=14.2.0", "--version"]);
        let first = text.lines().next().unwrap_or_default();
        assert_eq!(first, format!("gcc (rucc {VERSION}, GNU C persona 14.2.0) 14.2.0"));
        assert!(text.contains("GCC 14 from the Free Software Foundation"), "{text}");
        assert_eq!(text.lines().count(), 3, "{text}");

        // The claim can come after the flag, since it is answered once the line has been read,
        // and a short claim is printed in all three numbers, as GCC prints its own.
        let text = printed(&["--version", "-fgnuc-version=4.9"]);
        assert!(text.starts_with(&format!("gcc (rucc {VERSION}, GNU C persona 4.9.0) 4.9.0\n")));

        // A command line gcc would refuse is refused rather than answered.
        assert!(refused(&["--version", "-fgnuc-version=4.x"]).contains("not a number"));
    }

    /// Before GCC 7 `-dumpversion` was the whole version. From 7 it is the major number the way
    /// the distributions build it, and `-dumpfullversion` is the whole one.
    #[test]
    fn the_version_questions_are_answered_the_way_the_claimed_release_answers_them() {
        let ask = |claim: &str, flags: &[&str]| {
            let claim = format!("-fgnuc-version={claim}");
            let mut line = vec![claim.as_str()];
            line.extend_from_slice(flags);
            printed(&line)
        };
        assert_eq!(ask("4.9.4", &["-dumpversion"]), "4.9.4");
        assert_eq!(ask("4.9.4", &["-dumpfullversion"]), "4.9.4");
        assert_eq!(ask("6.3", &["-dumpversion"]), "6.3.0");
        assert_eq!(ask("7.5.0", &["-dumpversion"]), "7");
        assert_eq!(ask("14.2.0", &["-dumpversion"]), "14");
        assert_eq!(ask("14.2.0", &["-dumpfullversion"]), "14.2.0");
        // The first of the family wins, as it does in GCC, which is what makes the usual way of
        // asking any GCC for its whole version work.
        assert_eq!(ask("14.2.0", &["-dumpfullversion", "-dumpversion"]), "14.2.0");
        assert_eq!(ask("14.2.0", &["-dumpversion", "-dumpfullversion"]), "14");
        assert_eq!(ask("4.9.4", &["-dumpfullversion", "-dumpversion"]), "4.9.4");
        assert_eq!(
            ask("14.2.0", &["-dumpmachine", "-dumpversion", LINUX]),
            "x86_64-unknown-linux-gnu"
        );
    }

    /// The kernel's `GCC_PLUGINS` depends on `include/plugin-version.h` being under what
    /// `-print-file-name=plugin` prints, and this compiler has no plugins to offer.
    #[test]
    fn there_is_never_a_plugin_directory_to_find() {
        let dir = std::env::temp_dir().join(format!("rucc-plugin-{}", std::process::id()));
        let headers = dir.join("plugin").join("include");
        std::fs::create_dir_all(&headers).unwrap();
        std::fs::write(headers.join("plugin-version.h"), "").unwrap();
        // Even with a GCC plugin tree in a directory the search reads, since loading what is in it
        // is not something this compiler can do.
        let search = format!("-L{}", dir.display());
        assert_eq!(printed(&[LINUX, &search, "-print-file-name=plugin"]), "plugin");
        assert_eq!(printed(&["-fgnuc-version=14.2.0", "-print-file-name=plugin"]), "plugin");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The dialect a claimed release compiled when the command line had no `-std=`.
    #[test]
    fn a_claimed_release_brings_its_default_dialect_and_an_explicit_one_still_wins() {
        let dialect = |flags: &[&str]| {
            let mut line = vec![LINUX, "-c", "a.c"];
            line.extend_from_slice(flags);
            let (opts, _) = compile(&line);
            (opts.std, opts.gnu_extensions)
        };
        assert_eq!(dialect(&[]), (Std::C23, true), "no claim, our own default");
        assert_eq!(dialect(&["-fgnuc-version=4.9.4"]), (Std::C89, true));
        assert_eq!(dialect(&["-fgnuc-version=5.1"]), (Std::C11, true));
        assert_eq!(dialect(&["-fgnuc-version=7.5.0"]), (Std::C11, true));
        assert_eq!(dialect(&["-fgnuc-version=8.1"]), (Std::C17, true));
        assert_eq!(dialect(&["-fgnuc-version=14.2.0"]), (Std::C17, true));
        assert_eq!(dialect(&["-fgnuc-version=15.1"]), (Std::C23, true));
        // Whichever order they come in, what the command line said about the dialect wins.
        assert_eq!(dialect(&["-std=gnu11", "-fgnuc-version=4.9.4"]), (Std::C11, true));
        assert_eq!(dialect(&["-fgnuc-version=14.2.0", "-std=c99"]), (Std::C99, false));
        assert_eq!(dialect(&["-fgnuc-version=4.9.4", "-ansi"]), (Std::C89, false));
        // On an MSVC row the claim is `__GNUC__` and nothing more, as it is in clang.
        let (opts, _) =
            compile(&["--target=x86_64-pc-windows-msvc", "-fgnuc-version=4.9.4", "-c", "a.c"]);
        assert_eq!(opts.std, Std::default());
    }

    #[test]
    fn a_claimed_release_before_ten_makes_tentative_definitions_common() {
        let common = |flags: &[&str]| {
            let mut line = vec![LINUX, "-c", "a.c"];
            line.extend_from_slice(flags);
            Session::new(*compile(&line).0).common()
        };
        assert!(common(&["-fgnuc-version=9.5"]));
        assert!(!common(&["-fgnuc-version=10.1"]));
        assert!(!common(&["-fgnuc-version=9.5", "-fno-common"]), "the command line wins");
        let (opts, _) =
            compile(&["--target=x86_64-pc-windows-msvc", "-fgnuc-version=9.5", "-c", "a.c"]);
        assert!(!Session::new(*opts).common());
    }

    #[test]
    fn the_questions_a_build_system_asks_before_it_compiles_anything() {
        let target = "--target=x86_64-unknown-linux-gnu";
        assert_eq!(printed(&[target, "-dumpmachine"]), "x86_64-unknown-linux-gnu");
        assert_eq!(printed(&[target, "-dumpversion"]), "16");
        assert_eq!(printed(&[target, "-dumpfullversion"]), "16.0.0");
        // They follow the release claimed, since that is the one `__GNUC__` says.
        assert_eq!(printed(&[target, "-fgnuc-version=15.2", "-dumpversion"]), "15");
        assert_eq!(printed(&[target, "-fgnuc-version=15.2", "-dumpfullversion"]), "15.2.0");
        assert_eq!(printed(&[target, "-print-multiarch"]), "x86_64-linux-gnu");
        assert_eq!(printed(&[target, "-m32", "-print-multiarch"]), "i386-linux-gnu");
        let os = printed(&[target, "-print-multi-os-directory"]);
        assert!(os == "../lib" || os == "../lib64", "{os}");
        // A name nothing holds comes back unchanged, which is GCC's rule and is what makes the
        // answer safe to paste into a link line whether or not the file is there.
        assert_eq!(printed(&[target, "-print-file-name=no-such-library.a"]), "no-such-library.a");
        assert_eq!(printed(&[target, "-print-prog-name=ld"]), "ld");
        let dirs = printed(&[target, "-print-search-dirs"]);
        assert!(dirs.starts_with("install: "), "{dirs}");
        assert!(dirs.contains("\nlibraries: ="), "{dirs}");
    }

    #[test]
    fn the_sysroot_in_effect_is_the_one_the_command_line_named_or_the_one_for_the_target() {
        // A tree the user named is the answer whatever the target is, because it is the answer to
        // every other question too.
        assert_eq!(printed(&["--sysroot=/opt/cross", "-print-sysroot"]), "/opt/cross");

        // A target that is no machine this suite runs on is read under the cache, and the answer is
        // the root rather than one of the directories under it, since what asks is looking for a
        // file of its own.
        let root = cache::dir().join("sysroots").join("riscv64-linux-musl");
        assert_eq!(
            printed(&["--target=riscv64-linux-musl", "-print-sysroot"]),
            root.display().to_string()
        );

        // And a compile for this machine has no sysroot, which is the empty line GCC prints when it
        // was configured without one rather than a `/` that would be a claim about the filesystem.
        // Except on Windows, which has no C library of its own and reads the fetched tree for its
        // own target as it would for any other.
        let host = Triple::host().expect("a host this compiler knows");
        let own = printed(&[&format!("--target={host}"), "-print-sysroot"]);
        if host.os == rucc_target::Os::Windows {
            let root = cache::dir().join("sysroots").join(host.tuple().to_string());
            assert_eq!(own, root.display().to_string());
        } else {
            assert_eq!(own, "");
        }
    }

    #[test]
    fn the_provenance_of_a_sysroot_is_the_manifest_it_carries() {
        // Section 13.5 wants seven things per input and wants them machine readable, and the manifest
        // is the record that already has them, so the flag prints that rather than a second format.
        let manifest = "rucc sysroot manifest 3\n\
                        target\tx86_64-linux-musl\n\
                        kernel\t6.12\n\
                        include/generic/stdio.h\tmusl-1.2.5\t\
                        https://musl.libc.org/releases/musl-1.2.5.tar.gz\t\
                        0000000000000000000000000000000000000000000000000000000000000000\tmit\t\
                        bundled\n\
                        lib/libc.so\tmusl-1.2.5\t\
                        https://musl.libc.org/releases/musl-1.2.5.tar.gz\t\
                        1111111111111111111111111111111111111111111111111111111111111111\tmit\t\
                        generated\n";
        let tree = TempTree::new("provenance", &[("manifest", manifest)]);
        let sysroot = format!("--sysroot={}", tree.0.display());
        // The kernel line of tamnd/rucc#934 is in the answer without anything here naming it, because
        // the flag parses the record and renders it again rather than picking fields out of it. That
        // is the reason it prints a manifest and not a format of its own.
        //
        // The answer is the file without its last newline, because whatever prints it adds one. The
        // file is what somebody diffs the output against, so the two have to be the same bytes.
        assert_eq!(printed(&[&sysroot, "-print-sysroot-provenance"]) + "\n", manifest);

        // A tree with no manifest in it is a tree somebody assembled themselves, and nothing here
        // knows where any of it came from. Saying nothing is the only honest answer, and a reader can
        // tell it from a manifest with no inputs because that one still has its two header lines.
        let bare = TempTree::new("provenance-bare", &[]);
        assert_eq!(
            printed(&[&format!("--sysroot={}", bare.0.display()), "-print-sysroot-provenance"]),
            ""
        );

        // And a compile for this machine has no sysroot at all, which is the same empty answer
        // `-print-sysroot` gives for it.
        let host = Triple::host().expect("a host this compiler knows");
        assert_eq!(printed(&[&format!("--target={host}"), "-print-sysroot-provenance"]), "");

        // And the other spelling, which section 13.5 is the document that writes.
        assert_eq!(printed(&[&sysroot, "--print-sysroot-provenance"]) + "\n", manifest);

        // tamnd/rucc#1021. The digest of the same tree is the sha256 of that record, so it is one
        // line where the provenance is a few hundred, and it is checkable with `sha256sum` because
        // the bytes it is over are the bytes of the file. The number here is that hash of the
        // fixture above, computed by `sha256sum` rather than by this compiler.
        assert_eq!(
            printed(&[&sysroot, "-print-sysroot-digest"]),
            "d705ae6ebeafeb7fda4bd57cecc7882bf49784b17015664a09cfae25a1b2000a"
        );
        assert_eq!(
            printed(&[&sysroot, "--print-sysroot-digest"]),
            printed(&[&sysroot, "-print-sysroot-digest"])
        );

        // And the two empty answers are empty here too, because a digest of nothing would read as a
        // claim about a sysroot rather than as the absence of one.
        assert_eq!(
            printed(&[&format!("--sysroot={}", bare.0.display()), "-print-sysroot-digest"]),
            ""
        );
        assert_eq!(printed(&[&format!("--target={host}"), "-print-sysroot-digest"]), "");
    }

    #[test]
    fn a_manifest_this_build_cannot_read_is_refused_rather_than_printed() {
        // Passing a file we could not parse to whoever asked would make their parser the one that
        // finds the problem, and the three uses section 13.5 gives for this are all somebody else
        // parsing it.
        let tree = TempTree::new(
            "provenance-bad",
            &[("manifest", "rucc sysroot manifest 3\ntarget\tx86_64-linux-musl\nlib/libc.a\n")],
        );
        let message =
            refused(&[&format!("--sysroot={}", tree.0.display()), "-print-sysroot-provenance"]);
        assert!(message.contains("manifest"), "{message}");
        assert!(message.contains("1 fields where an input has six"), "{message}");

        // The digest is refused for the same file and for a stronger reason: a hash of bytes this
        // build cannot read would be a number that names a record nobody can act on.
        let digest =
            refused(&[&format!("--sysroot={}", tree.0.display()), "-print-sysroot-digest"]);
        assert_eq!(digest, message);
    }

    #[test]
    fn the_two_dependency_flags_that_stop_after_the_rule_stop_after_the_rule() {
        let (opts, _) = compile(&["-M", "a.c"]);
        assert!(opts.deps.emit && opts.deps.instead_of_compiling);
        assert!(opts.deps.system_headers, "plain -M lists them");
        assert_eq!(opts.emit, EmitKind::Preprocessed);

        // Even where a later flag asked for something else, because the family is a mode and
        // the mode is what the run is for.
        let (opts, _) = compile(&["-M", "-c", "a.c"]);
        assert_eq!(opts.emit, EmitKind::Preprocessed);

        let (opts, _) = compile(&["-MM", "a.c"]);
        assert!(!opts.deps.system_headers);
    }

    #[test]
    fn the_two_that_end_in_d_leave_the_compilation_alone() {
        let (opts, _) = compile(&["-MD", "-c", "a.c"]);
        assert!(opts.deps.emit && !opts.deps.instead_of_compiling);
        assert!(opts.deps.system_headers);
        assert_eq!(opts.emit, EmitKind::Object);

        let (opts, _) = compile(&["-MMD", "-c", "a.c"]);
        assert!(opts.deps.emit && !opts.deps.instead_of_compiling);
        assert!(!opts.deps.system_headers);
    }

    #[test]
    fn nothing_puts_the_system_headers_back_once_a_flag_has_taken_them_out() {
        // GCC's rule, and not an oversight in it. The flag asking for fewer of them is read as
        // the answer, because the other one never asked the question.
        let (opts, _) = compile(&["-MM", "-M", "a.c"]);
        assert!(!opts.deps.system_headers);
        let (opts, _) = compile(&["-MD", "-MMD", "-c", "a.c"]);
        assert!(!opts.deps.system_headers);
        let (opts, _) = compile(&["-MMD", "-MD", "-c", "a.c"]);
        assert!(!opts.deps.system_headers);
    }

    #[test]
    fn a_target_arrives_escaped_from_one_flag_and_untouched_from_the_other() {
        let (opts, _) = compile(&["-MM", "-MT", "a b.o", "-MQ", "a b.o", "a.c"]);
        assert_eq!(opts.deps.targets, vec!["a b.o".to_owned(), "a\\ b.o".to_owned()]);
    }

    #[test]
    fn the_rest_of_the_family_is_a_file_and_a_switch() {
        let (opts, _) = compile(&["-MM", "-MF", "dep.d", "-MP", "a.c"]);
        assert_eq!(opts.deps.file.as_deref(), Some("dep.d"));
        assert!(opts.deps.phony);

        for flag in ["-MF", "-MT", "-MQ"] {
            let e = parse_args(&args(&[flag])).unwrap_err();
            assert!(e.message.contains("requires an argument"), "{}", e.message);
        }
    }

    /// Kbuild's spelling, which is how busybox and the kernel ask for every dependency file.
    #[test]
    fn a_dependency_file_asked_for_through_the_preprocessor_is_written_where_it_said() {
        let (opts, _) = compile(&["-Wp,-MD,applets/.applets.o.d", "-c", "a.c"]);
        assert!(opts.deps.emit);
        assert!(opts.deps.system_headers);
        assert_eq!(opts.deps.file.as_deref(), Some("applets/.applets.o.d"));

        let (opts, _) = compile(&["-Wp,-MMD,x.d,-MP,-MT,x.o", "-c", "a.c"]);
        assert!(!opts.deps.system_headers);
        assert!(opts.deps.phony);
        assert_eq!(opts.deps.file.as_deref(), Some("x.d"));
        assert_eq!(opts.deps.targets, vec!["x.o".to_owned()]);
    }

    #[test]
    fn a_preprocessor_flag_this_compiler_does_not_read_is_still_refused_whole() {
        assert!(refused(&["-Wp,-MD", "-c", "a.c"]).contains("separate preprocessor"));
        assert!(refused(&["-Wp,-MD,x.d,-C", "-c", "a.c"]).contains("-Wp,-MD,x.d,-C"));
    }

    #[test]
    fn the_assembler_version_is_its_own_claim_beside_the_gcc_one() {
        let line = |extra: &[&str]| {
            let mut words = vec!["-Wa,--version", "-c", "-x", "assembler", "/dev/null"];
            words.extend_from_slice(extra);
            words.extend_from_slice(&["-o", "/dev/null"]);
            printed(&words)
        };
        let ours = format!("GNU assembler (rucc {VERSION} integrated)");
        assert_eq!(line(&["-fgnu-as-version=2.44"]), format!("{ours} 2.44"));
        assert_eq!(line(&[]), format!("{ours} 2.46"), "the documented default moved");
        assert_eq!(line(&["-fgnu-as-version=2.35.1"]), format!("{ours} 2.35.1"));
        // The two personas do not move each other.
        assert_eq!(line(&["-fgnuc-version=4.9.4"]), format!("{ours} 2.46"));
        let (opts, _) = compile(&["-fgnu-as-version=2.25", "-c", "a.c"]);
        assert_eq!(opts.gnu_as.to_string(), "2.25");
        assert_eq!(opts.gnuc, GnucVersion::default());
        let bad = refused(&["-fgnu-as-version=2.x", "-c", "a.c"]);
        assert!(bad.contains("-fgnu-as-version="), "{bad}");
    }

    #[test]
    fn the_assembler_version_is_asked_the_way_as_version_sh_asks_it() {
        // `scripts/as-version.sh` in the kernel, which puts its flags after the compiler's own and
        // reads the preprocessor's spelling of an assembler file.
        let said = printed(&[
            "-fgnuc-version=14.2.0",
            "-fgnu-as-version=2.44",
            "-Wa,--version",
            "-c",
            "-x",
            "assembler-with-cpp",
            "/dev/null",
            "-o",
            "/dev/null",
        ]);
        assert!(said.starts_with("GNU assembler "), "{said}");
        assert!(said.ends_with(" 2.44"), "{said}");
        // And `-Xassembler`, which is the same word by another road, and a list in one `-Wa,`.
        let x = printed(&["-Xassembler", "--version", "-c", "-x", "assembler", "/dev/null"]);
        assert_eq!(x, format!("GNU assembler (rucc {VERSION} integrated) 2.46"));
        let list = printed(&["-Wa,--noexecstack,--version", "-c", "-x", "assembler", "/dev/null"]);
        assert_eq!(list, x);
    }

    #[test]
    fn what_the_kernel_hands_the_assembler_is_taken_where_it_is_true() {
        let x86 = "--target=x86_64-unknown-linux-gnu";
        let arm = "--target=aarch64-unknown-linux-gnu";
        for word in [
            "-Wa,--noexecstack",
            "-Wa,-mx86-used-note=no",
            "-Wa,--64",
            "-Wa,-Iinclude",
            "-Wa,-I,include",
        ] {
            compile(&[x86, word, "-c", "a.c"]);
        }
        compile(&[x86, "-Xassembler", "--noexecstack", "-c", "a.c"]);
        for word in ["-Wa,--32", "-Wa,-mtune=generic32", "-Wa,-mtune=i486"] {
            compile(&[x86, "-m32", "-fno-pic", word, "-c", "a.c"]);
        }
        for word in ["-Wa,-march=armv8.5-a", "-Wa,-march=armv8.4-a+crc", "-Wa,-mabi=lp64"] {
            compile(&[arm, word, "-c", "a.c"]);
        }
        let (opts, _) = compile(&[x86, "-Wa,--fatal-warnings", "-c", "a.c"]);
        assert!(opts.asm_fatal_warnings, "--fatal-warnings did not reach the assembler");
        assert!(!compile(&[x86, "-c", "a.c"]).0.asm_fatal_warnings);
    }

    #[test]
    fn what_the_assembler_would_not_do_is_refused_by_name() {
        // kbuild's `as-option` takes an exit status of zero as the option being supported, so
        // each of these has to fail or the kernel switches on something the output does not have.
        let x86 = "--target=x86_64-unknown-linux-gnu";
        let arm = "--target=aarch64-unknown-linux-gnu";
        let cases: [(&[&str], &str); 11] = [
            (&[x86, "-Wa,--32"], "`--32`"),
            (&[x86, "-Wa,-mtune=bogus"], "`-mtune=bogus`"),
            (&[arm, "-Wa,--64"], "`--64`"),
            (&[arm, "-Wa,-mx86-used-note=no"], "`-mx86-used-note=no`"),
            (&[x86, "-Wa,-mx86-used-note=yes"], "`-mx86-used-note=yes`"),
            (&[x86, "-Wa,-gdwarf-5"], "debug information"),
            (&[x86, "-Wa,--gdwarf-4"], "debug information"),
            (&[x86, "-Wa,-march=corei7"], "`-march=corei7`"),
            (&[arm, "-Wa,-march=armv7-a"], "`-march=armv7-a`"),
            (&[arm, "-Wa,-mrelax-relocations=no"], "`-mrelax-relocations=no`"),
            (&[x86, "-Wa,--noexecstack,-isa=foo"], "`-Wa,--noexecstack,-isa=foo`"),
        ];
        for (words, needle) in cases {
            let mut line = words.to_vec();
            line.extend_from_slice(&["-c", "a.c"]);
            let why = refused(&line);
            assert!(why.contains(needle), "{words:?}: {why}");
        }
        let lonely = refused(&[x86, "-Xassembler"]);
        assert!(lonely.contains("requires an argument"), "{lonely}");
    }

    /// A directory of sources for one test, removed when the test is done with it.
    struct TempTree(PathBuf);

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl TempTree {
        fn new(name: &str, files: &[(&str, &str)]) -> TempTree {
            let dir = std::env::temp_dir().join(format!("rucc-deps-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temporary directory should be writable");
            for (path, text) in files {
                let at = dir.join(path);
                if let Some(parent) = at.parent() {
                    std::fs::create_dir_all(parent).expect("creating a subdirectory should work");
                }
                std::fs::write(&at, text).expect("writing a temporary file should work");
            }
            TempTree(dir)
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    #[test]
    fn the_rule_names_what_the_includes_found_and_names_each_of_them_once() {
        // End to end, because the list comes from the preprocessor and the format comes from
        // somewhere else, and a test of either half on its own would pass with the two of them
        // wired up backwards.
        let tree = TempTree::new(
            "found",
            &[
                ("a.c", "#include \"one.h\"\n#include \"two.h\"\nint main(void) { return X; }\n"),
                ("one.h", "#define X 0\n"),
                ("two.h", "#include \"one.h\"\n"),
            ],
        );
        let out = tree.path("dep.d");
        let code = run(&args(&["-MM", "-MF", &out, "-o", &tree.path("a.i"), &tree.path("a.c")]));
        assert_eq!(code, 0);

        let text = std::fs::read_to_string(&out).expect("the rule should have been written");
        let names: Vec<&str> = text.split_whitespace().collect();
        // The target, the source, and each header once however many times it was reached.
        assert_eq!(names.first(), Some(&"a.o:"), "{text}");
        assert_eq!(names.iter().filter(|n| n.ends_with("one.h")).count(), 1, "{text}");
        assert_eq!(names.iter().filter(|n| n.ends_with("two.h")).count(), 1, "{text}");
        // And the `-o` went to the file the rule replaced, which is left empty rather than
        // absent because a makefile that named it as a target will look for it.
        assert_eq!(std::fs::read(tree.path("a.i")).expect("the output should exist"), b"");
    }

    #[test]
    fn a_file_that_fails_after_the_preprocessor_still_gets_its_rule() {
        // kbuild's lib/test_fortify compiles code that has to be refused with `-Wp,-MMD,` and
        // hands the rule to `fixdep` afterwards. gcc and clang both leave it behind for an error
        // the preprocessor did not see, and neither does for a header that was not found.
        let tree = TempTree::new(
            "failed",
            &[
                ("bad.c", "#include \"one.h\"\nint f(void) { return y; }\n"),
                ("one.h", "#define X 0\n"),
                ("lost.c", "#include \"nope.h\"\nint x;\n"),
            ],
        );
        let (bad, lost) = (tree.path("bad.d"), tree.path("lost.d"));
        let flag = format!("-Wp,-MMD,{bad}");
        assert_ne!(run(&args(&[&flag, "-c", "-o", &tree.path("bad.o"), &tree.path("bad.c")])), 0);
        let text = std::fs::read_to_string(&bad).expect("the rule should have been written");
        assert!(text.contains("one.h"), "{text}");
        let flag = format!("-Wp,-MMD,{lost}");
        assert_ne!(run(&args(&[&flag, "-c", "-o", &tree.path("lost.o"), &tree.path("lost.c")])), 0);
        assert!(!std::path::Path::new(&lost).exists());
    }

    #[test]
    fn syntax_only_checks_the_file_and_writes_nothing() {
        // What meson's `has_header_symbol` probe does: compile with `-fsyntax-only` and read the
        // exit status. A good file passes and leaves no output behind, a bad one fails.
        let tree = TempTree::new(
            "syntax-only",
            &[
                ("good.c", "int f(int x) { return x + 1; }\n"),
                ("bad.c", "int f(void) { return y; }\n"),
            ],
        );
        let (opts, _) = compile(&["-fsyntax-only", "a.c"]);
        assert_eq!(opts.emit, EmitKind::SyntaxOnly);

        let out = tree.path("good.o");
        assert_eq!(run(&args(&["-fsyntax-only", "-o", &out, &tree.path("good.c")])), 0);
        assert!(!std::path::Path::new(&out).exists(), "-fsyntax-only wrote {out}");
        assert!(!std::path::Path::new(&tree.path("good.s")).exists());
        assert_ne!(run(&args(&["-fsyntax-only", &tree.path("bad.c")])), 0);
    }

    /// Where `-fstack-usage` puts each job's report, one entry per job, for a command line.
    fn stack_usage_files(line: &[&str]) -> Vec<Option<String>> {
        let mut words = vec![LINUX, "-fstack-usage"];
        words.extend_from_slice(line);
        let (_, plan) = compile(&words);
        plan.jobs.iter().map(|job| job.stack_usage.clone()).collect()
    }

    #[test]
    fn a_stack_usage_file_is_named_the_way_gcc_names_it() {
        // Every row was run through gcc 16 with the same command line, and the name is the one it
        // wrote. `rpg frames` finds gcc's file and this compiler's by the same rule, so a name that
        // differs is a function that goes missing from the comparison.
        let cases: &[(&[&str], &[Option<&str>])] = &[
            (&["-c", "sub/a.c"], &[Some("a.su")]),
            (&["-c", "sub/a.c", "-o", "out/x.o"], &[Some("out/x.su")]),
            (&["-c", "sub/a.c", "b.c"], &[Some("a.su"), Some("b.su")]),
            (&["-S", "sub/a.c", "-o", "out/y.s"], &[Some("out/y.su")]),
            (&["-S", "sub/a.c", "-o", "-"], &[Some("a.su")]),
            (&["-E", "sub/a.c", "-o", "out/z.i"], &[None]),
            (&["-fsyntax-only", "sub/a.c"], &[Some("a.su")]),
            (&["-fsyntax-only", "sub/a.c", "-o", "out/x.o"], &[Some("out/x.o-a.su")]),
            (&["sub/a.c"], &[Some("a.su")]),
            (&["sub/a.c", "-lm"], &[Some("a.su")]),
            (&["sub/a.c", "b.c"], &[Some("a-a.su"), Some("a-b.su")]),
            (&["sub/a.c", "b.o"], &[Some("a-a.su"), None]),
            (&["sub/a.c", "-o", "out/prog"], &[Some("out/prog-a.su")]),
            (&["sub/a.c", "-o", "out/lib.so"], &[Some("out/lib.so-a.su")]),
            (&["sub/a.c", "-o", "out/prog.exe"], &[Some("out/prog-a.su")]),
            (&["sub/a.c", "-o", "out/prog", "-dumpbase", "zz"], &[Some("out/zz-a.su")]),
            (&["sub/a.c", "-o", "out/prog", "-dumpdir", "dd-"], &[Some("dd-a.su")]),
            (
                &["sub/a.c", "b.c", "-dumpdir", "dd/", "-dumpbase", "zz"],
                &[Some("dd/zz-a.su"), Some("dd/zz-b.su")],
            ),
            (&["-c", "sub/a.c", "-dumpbase", "foo", "-o", "out/w.o"], &[Some("out/foo.su")]),
            (&["-c", "sub/a.c", "-dumpdir", "dd/", "-dumpbase", "sub/zz"], &[Some("sub/zz.su")]),
            (&["-c", "sub/a.c", "-dumpbase", "zz.c", "-dumpbase-ext", ".c"], &[Some("zz.su")]),
            (&["-c", "sub/a.c", "-dumpdir", "pre", "-o", "out/x.o"], &[Some("prex.su")]),
            (&["-c", "sub/a.c", "-save-temps=cwd", "-o", "out/x.o"], &[Some("x.su")]),
        ];
        for (line, want) in cases {
            let want: Vec<Option<String>> = want.iter().map(|w| w.map(str::to_owned)).collect();
            assert_eq!(stack_usage_files(line), want, "{line:?}");
        }
        // Nothing at all without the flag.
        let (_, plan) = compile(&[LINUX, "-c", "sub/a.c"]);
        assert_eq!(plan.jobs[0].stack_usage, None);
    }

    #[test]
    fn a_stack_usage_file_has_a_line_per_function_where_gcc_would_put_it() {
        let tree = TempTree::new(
            "stack-usage",
            &[
                ("inc/h.h", "static inline int twice(int x) { return x * 2; }\n"),
                (
                    "a.c",
                    "#include \"inc/h.h\"\n\
                     static int helper(int);\n\
                     int grows(int n) { char v[n]; v[0] = (char)n; return v[n - 1] + twice(n); }\n\
                     static int\n\
                     helper(int x)\n\
                     {\n\
                     return x + 1;\n\
                     }\n\
                     int calls(int x) { return helper(x) + grows(x); }\n",
                ),
            ],
        );
        let (source, object) = (tree.path("a.c"), tree.path("a.o"));
        assert_eq!(run(&args(&["-O0", "-fstack-usage", "-c", &source, "-o", &object])), 0);
        let text = std::fs::read_to_string(tree.path("a.su")).expect("a.su should be written");

        let line = |function: &str| {
            let suffix = format!(":{function}");
            let line =
                text.lines().find(|line| line.split('\t').next().unwrap().ends_with(&suffix));
            line.unwrap_or_else(|| panic!("no line for {function} in\n{text}"))
        };
        let expect = |function: &str, at: String, qualifier: &str| {
            let fields: Vec<&str> = line(function).split('\t').collect();
            assert_eq!(fields.len(), 3, "{text}");
            assert_eq!(fields[0], format!("{at}:{function}"), "{text}");
            let bytes: u32 = fields[1].parse().expect("the bytes should be a number");
            assert!(bytes >= 8 && bytes % 8 == 0, "{function} takes {bytes} bytes");
            assert_eq!(fields[2], qualifier, "{text}");
        };
        // A variable length array makes the frame grow while the function runs.
        expect("grows", format!("{source}:3:5"), "dynamic");
        // The definition rather than the declaration above it, and the line the name is on
        // rather than the one the type is on.
        expect("helper", format!("{source}:5:1"), "static");
        expect("calls", format!("{source}:9:5"), "static");
        // A function from a header is reported against the header.
        expect("twice", format!("{}:1:19", tree.path("inc/h.h")), "static");
        assert_eq!(text.lines().count(), 4, "{text}");
    }

    #[test]
    fn a_stack_usage_file_is_empty_when_there_is_nothing_to_report_and_absent_under_dash_e() {
        let tree = TempTree::new(
            "stack-usage-empty",
            &[
                ("good.c", "int f(int x) { return x + 1; }\n"),
                ("bad.c", "int f(void) { return y; }\n"),
            ],
        );
        let good = tree.path("good.c");
        // gcc writes an empty file for a check that compiles nothing and for a file that failed,
        // and a build that looks for one beside every object finds one.
        assert_eq!(
            run(&args(&["-fstack-usage", "-fsyntax-only", &good, "-o", &tree.path("x")])),
            0
        );
        assert_eq!(std::fs::read_to_string(tree.path("x-good.su")).unwrap(), "");
        let bad = tree.path("bad.c");
        assert_ne!(run(&args(&["-fstack-usage", "-c", &bad, "-o", &tree.path("bad.o")])), 0);
        assert_eq!(std::fs::read_to_string(tree.path("bad.su")).unwrap(), "");
        // And none under `-E`, which never reaches a function.
        assert_eq!(run(&args(&["-fstack-usage", "-E", &good, "-o", &tree.path("e.i")])), 0);
        assert!(!std::path::Path::new(&tree.path("e.su")).exists());
    }

    #[test]
    fn a_header_that_is_only_reached_under_a_guard_is_still_a_dependency() {
        // The multiple-include optimization means the second reach never opens the file. It is
        // still a file this translation unit was built from, so it is still in the rule.
        let tree = TempTree::new(
            "guarded",
            &[
                ("a.c", "#include \"g.h\"\n#include \"g.h\"\nint main(void) { return 0; }\n"),
                ("g.h", "#ifndef G\n#define G\n#endif\n"),
            ],
        );
        let out = tree.path("dep.d");
        let code = run(&args(&["-MM", "-MF", &out, "-o", &tree.path("a.i"), &tree.path("a.c")]));
        assert_eq!(code, 0);
        let text = std::fs::read_to_string(&out).expect("the rule should have been written");
        assert_eq!(text.split_whitespace().filter(|n| n.ends_with("g.h")).count(), 1, "{text}");
    }

    #[test]
    fn a_header_the_build_makes_later_is_in_the_rule_by_its_written_name() {
        let tree = TempTree::new(
            "generated",
            &[
                (
                    "a.c",
                    "#include \"config.h\"\n#include <gen/sys.h>\n#include \"real.h\"\nint x;\n",
                ),
                ("real.h", ""),
            ],
        );
        let rule = |flag: &str| {
            let out = tree.path(&format!("{flag}.d"));
            let line = [flag, "-MG", "-MF", &out, "-o", &tree.path("a.i"), &tree.path("a.c")];
            assert_eq!(run(&args(&line)), 0);
            std::fs::read_to_string(&out).expect("the rule should have been written")
        };
        let all = rule("-M");
        let words: Vec<&str> = all.split_whitespace().collect();
        assert!(words.contains(&"config.h") && words.contains(&"gen/sys.h"), "{all}");
        assert!(words.iter().any(|w| w.ends_with("real.h")), "{all}");
        // `-MM` leaves out an angled name, as GCC does.
        let user = rule("-MM");
        assert!(user.contains("config.h") && !user.contains("gen/sys.h"), "{user}");
        // A compile cannot go on without the header.
        assert_eq!(refused(&["-MG", "-c", "a.c"]), "-MG may only be used with -M or -MM");
        assert!(refused(&["-dumpspecs"]).contains("-dumpmachine"));
        assert_eq!(refused(&["-MD", "-MG", "-c", "a.c"]), "-MG may only be used with -M or -MM");
    }

    #[test]
    fn every_imacros_file_is_read_before_every_include_file_whatever_order_they_were_written() {
        // Measured against GCC rather than read: the two flags the other way round produce the
        // same output byte for byte, so the command line order between the two families does not
        // decide anything and the order within one does. The `-include` file here can only see
        // the definition if the `-imacros` file that was written after it ran first.
        let tree = TempTree::new(
            "preinclude",
            &[
                ("a.c", "int main(void) { return 0; }\n"),
                ("i.h", "#ifdef FROM_MACROS\nint saw_it;\n#else\nint missed_it;\n#endif\n"),
                ("m.h", "#define FROM_MACROS 1\nint macros_text;\n"),
            ],
        );
        let out = tree.path("a.i");
        let code = run(&args(&[
            "-E",
            "-include",
            &tree.path("i.h"),
            "-imacros",
            &tree.path("m.h"),
            "-o",
            &out,
            &tree.path("a.c"),
        ]));
        assert_eq!(code, 0);
        let text = std::fs::read_to_string(&out).expect("the output should have been written");
        assert!(text.contains("saw_it"), "{text}");
        // And the text of the `-imacros` file is thrown away, which is the whole difference
        // between the two flags.
        assert!(!text.contains("macros_text"), "{text}");
    }

    #[test]
    fn a_file_the_command_line_named_is_a_prerequisite_the_same_as_one_a_directive_named() {
        let tree = TempTree::new(
            "preinclude-deps",
            &[
                ("a.c", "int main(void) { return 0; }\n"),
                ("i.h", "int from_include;\n"),
                ("m.h", "#define M 1\n"),
            ],
        );
        let out = tree.path("dep.d");
        let code = run(&args(&[
            "-MM",
            "-MF",
            &out,
            "-include",
            &tree.path("i.h"),
            "-imacros",
            &tree.path("m.h"),
            "-o",
            &tree.path("a.i"),
            &tree.path("a.c"),
        ]));
        assert_eq!(code, 0);
        let text = std::fs::read_to_string(&out).expect("the rule should have been written");
        assert!(text.contains("i.h"), "{text}");
        assert!(text.contains("m.h"), "{text}");
    }

    #[test]
    fn a_command_line_include_that_is_nowhere_on_the_path_is_an_error_and_not_a_warning() {
        // Including the directory of the source file, which is not on the path for these: the
        // command line was not written there, so a name in it is relative to where the compiler
        // was run rather than to where the source sits.
        let tree = TempTree::new(
            "preinclude-missing",
            &[("sub/a.c", "int main(void) { return 0; }\n"), ("sub/beside.h", "int x;\n")],
        );
        let code = run(&args(&["-E", "-include", "beside.h", "-o", "-", &tree.path("sub/a.c")]));
        assert_eq!(code, 1);
    }

    #[test]
    fn a_command_line_that_links_names_the_executable_and_not_the_object_it_went_through() {
        // The object a link goes through is in a temporary directory and is gone before `make`
        // reads any of this, so the rule that named it would be a rule for a file that is never
        // there. The target and the file are both the `-o`, which is the executable.
        let (opts, plan) = compile(&["-MD", "sub/a.c", "-o", "prog"]);
        assert_eq!(plan.output.as_deref(), Some("prog"));
        assert_eq!(deps::default_target("sub/a.c", deps_target_output(&opts, &plan)), "prog");
        assert_eq!(
            deps::default_file(&opts.deps, "sub/a.c", plan.output.as_deref()).as_deref(),
            Some("prog.d")
        );
    }

    #[test]
    fn the_plan_keeps_the_output_name_because_the_rule_is_written_from_it() {
        let (_, plan) = compile(&["-MMD", "-c", "sub/a.c", "-o", "obj/x.o"]);
        assert_eq!(plan.output.as_deref(), Some("obj/x.o"));
        let (_, plan) = compile(&["-MMD", "-c", "sub/a.c"]);
        assert_eq!(plan.output, None);
    }

    #[test]
    fn usage_fits_on_a_screen() {
        // Not a style preference. A help text that scrolls is one nobody reads, and this is
        // the cheapest way to keep it honest as flags accumulate. The number goes up only when
        // a family of flags arrives that has nowhere to share a line, which the two pass gates
        // were and which the two fuel flags and `-fsafety=` now are, and it goes up by exactly
        // the lines that family took. The four it went up by last are the flags a build system
        // passes without being asked to: how much to say, what machine to generate for, threads,
        // and the questions `configure` asks before it compiles anything. The one it went up by
        // last is the second line of `--emit`, whose kinds are a family that has now outgrown
        // one line and has nowhere else to go. The two it went up by last are the dependency
        // family, which is eight flags that share nothing with anything above them. The one it
        // went up by last is the four spellings of position independent code, which every
        // configure script writes and which could only have shared the link line, and that line
        // is already four characters short of the limit. The two it went up by last are the rest
        // of the include family, which is six more flags that change where a header is looked for
        // and two that name a header outright. The one it went up by last is the pair that keeps
        // the intermediate files and times the steps, which belong next to the two flags above
        // them that are also about watching a compilation rather than changing one. The two it
        // went up by last are the section flags and the visibility flag, which are what a build
        // that cares about the size of what it ships and about which names it exports writes, and
        // the second of them was already taken and only missing from here. The one it went up by
        // last is the stack protector, which is four spellings of one question and which every
        // distribution puts on every command line it issues, so a build that reads this list
        // looking for it and does not find it has to go and read the specification instead. The one
        // it went up by last is the profiler, which is two spellings of the request and two of
        // where the call goes, and which is about watching a program run rather than about what is
        // generated, so it shares its subject with nothing above it. The one it went up by last is
        // the room a function opens with for something to be written over it later, which takes an
        // argument of its own shape and is what a kernel build asks for, so it fits beside the
        // profiler and nothing else. The one it went up by last is what overflows rather than being
        // undefined, which is three spellings of two questions and which a kernel build and a great
        // deal of code written before the standard settled both pass. The one it went up by last is
        // the other answer to the first of those questions, which could not share the line because
        // what it asks for is the opposite of what the flags on that line ask for. The one it went
        // up by last is the split of the line that lists what this compiler does anyway into that
        // and what it assumes anyway, which are two different claims that were sharing a line until
        // the second of them got a second flag and the line stopped fitting. The one it went up by
        // last is the three flags that change the ABI rather than the code, which have to be given
        // to every file in a program or none of them and which therefore belong somewhere a person
        // reading this list will see them. The one it went up by last is the floating point group,
        // which is two lines rather than one because the first of them is a choice this compiler
        // records and the rest are claims about what it does anyway, and putting a real setting on
        // the same line as three flags that change nothing would be misleading about both. The one
        // it went up by last is the flag that says a write has to stay inside the member it names,
        // which is a setting rather than a claim and so cannot share the line above it, that being
        // the one that picks a tier. The two it went up by last are the prefix mapping family,
        // which is four flags whose whole job is to keep a build's output the same from two
        // different directories, and which a person chasing a reproducible build comes here
        // looking for by name. The one it went up by last is how the debug sections are compressed
        // and whether they go in a file of their own, which are two questions about the shape of
        // the debug output, where the line above them is about how much of it there is. The one it
        // went up by last is the `restrict` contract, which is a setting for the same reason the
        // flag that keeps a write inside its member is and which is the check a person who has been
        // bitten by a vectorizer comes here looking for. The one it went up by last is link time
        // optimization, which is a whole optimization rather than a flag and which says so on its
        // own line, because a build that passes it and reads this looking for what it got is
        // asking a question no other line here answers. The one it went up by last is the sysroot,
        // which is the question somebody asks when a cross build read a file nobody expected, and
        // which has no room on the line above it because the answers there are a path each and this
        // one is the root all of them are under. The one it went up by last is what is inside that
        // root and where each of it came from, which is a question about a whole tree rather than
        // about a path and which is long enough on its own that it could not have shared a line with
        // anything. The one it went up by last is the profile family, which splits down the middle
        // where no other family here does, so the line has to name the half that is taken and the
        // half that is refused or it would be read as taking both. The one it went up by last is
        // the sanitizers, which are what somebody reaching for a checked build writes first and
        // which belong beside the tier that is the nearest thing here to what they asked for. The
        // one it went up by last is the digest of that record, which is the same tree as one number
        // and could not share the line above it because that line prints a few hundred lines and
        // this one prints sixty four characters, and a reader who wants the short answer is looking
        // for it by name rather than reading the long one. The one it went up by last is the
        // sysroot fetch, which is the only command here that gets something from somewhere else and
        // is therefore the one a person wants to have read before they run it rather than after.
        // And the flag beside it that forbids every download, which earns its line by being what a
        // build in a sealed environment passes and by meaning something even though an ordinary
        // compile downloads nothing either way. The one it went up by last is the other fetch, the
        // one behind Microsoft's licence wall, which is a line rather than a paragraph because what
        // a person needs from here is that the command exists and that it will not do anything
        // until they have read a licence it prints for them. The one it went up by last is dlltool
        // mode, which is not a compiler flag at all but a second program behind the same binary,
        // and which a person building mingw-w64 with this compiler has to be able to find without
        // knowing it is there. The one it went up by last is the wasm features and the execution
        // model, which pick what a wasm module may use and whether it is a command or a reactor,
        // and which could not share the machine line above them because that line is already
        // full and asks a different question. The one it went up by last is `--param`, which is
        // how an experiment moves a threshold without a build, and it took the line `--version`
        // had by putting that beside `--help`.
        assert!(USAGE.lines().count() < 75, "usage text has grown past one screen");
    }

    const KERNEL_X86: &str = "--target=x86_64-unknown-linux-gnu";
    const KERNEL_ARM64: &str = "--target=aarch64-unknown-linux-gnu";

    /// The flags kbuild passes whose request is already what this compiler does, on the target
    /// each is for. Every one of them was an unknown option before, and a `cc-option` probe that
    /// is refused drops the flag, so a kernel built with rucc was built with a different line.
    #[test]
    fn a_kernel_flag_that_asks_for_what_happens_is_taken() {
        for flag in [
            "-fverbose-asm",
            "-fno-var-tracking",
            "-fno-var-tracking-assignments",
            "-fno-partial-inlining",
            "-fmerge-constants",
            "-fno-allow-store-data-races",
            "-fzero-init-padding-bits=all",
            "-fno-stack-check",
            "-fno-dwarf2-cfi-asm",
            "-femit-struct-debug-baseonly",
            "-femit-struct-debug-reduced",
            "-femit-struct-debug-detailed=any",
            "-fdiagnostics-show-context=2",
            "-fjump-tables",
            "-ftrivial-auto-var-init=uninitialized",
            "-fzero-call-used-regs=skip",
            "-gz=none",
            "-mskip-rax-setup",
            "-maccumulate-outgoing-args",
            "-mno-apx-features=egpr",
            "-mstack-protector-guard=tls",
            "-mindirect-branch=keep",
            "-mfunction-return=keep",
            "-mharden-sls=none",
        ] {
            compile(&[KERNEL_X86, flag, "-c", "a.c"]);
        }
        for flag in [
            "-mno-outline-atomics",
            "-ffixed-x18",
            "-mlittle-endian",
            "-mbranch-protection=none",
            "-mabi=lp64",
            "-mstrict-align",
        ] {
            compile(&[KERNEL_ARM64, flag, "-c", "a.c"]);
        }
        // Read for the target the line ends up naming, wherever `--target=` was written.
        compile(&["-mno-outline-atomics", KERNEL_ARM64, "-c", "a.c"]);
    }

    /// The speculation hardening flags the kernel builds with, each last one wins, and the jump
    /// tables it turns off beside them. tamnd/rucc#2280.
    #[test]
    fn the_speculation_hardening_flags_are_honored() {
        let asked = |flags: &[&str]| {
            let line: Vec<&str> =
                [KERNEL_X86].iter().chain(flags).chain(&["-c", "a.c"]).copied().collect();
            compile(&line).0
        };
        let none = asked(&[]);
        assert_eq!(
            (none.speculation, none.jump_tables),
            (rucc_target::Speculation::default(), true)
        );
        let kernel = asked(&[
            "-mindirect-branch=thunk-extern",
            "-mindirect-branch-register",
            "-mindirect-branch-cs-prefix",
            "-mfunction-return=thunk-extern",
            "-mharden-sls=all",
            "-fno-jump-tables",
        ]);
        let all = rucc_target::Speculation {
            indirect: rucc_target::Thunk::Extern,
            padded: true,
            returns: rucc_target::Thunk::Extern,
            after_return: true,
            after_jump: true,
        };
        assert_eq!((kernel.speculation, kernel.jump_tables), (all, false));
        let back = asked(&[
            "-mindirect-branch=thunk-extern",
            "-mindirect-branch=keep",
            "-mfunction-return=thunk-extern",
            "-mfunction-return=keep",
            "-mindirect-branch-cs-prefix",
            "-mno-indirect-branch-cs-prefix",
            "-mharden-sls=all",
            "-mharden-sls=none",
            "-fno-jump-tables",
            "-fjump-tables",
        ]);
        assert_eq!(
            (back.speculation, back.jump_tables),
            (rucc_target::Speculation::default(), true)
        );
        let sls = |kind| asked(&[kind]).speculation;
        assert!(sls("-mharden-sls=return").after_return && !sls("-mharden-sls=return").after_jump);
        let jumps = sls("-mharden-sls=indirect-jmp");
        assert!(jumps.after_jump && !jumps.after_return);
        let bad = refused(&[KERNEL_X86, "-mharden-sls=jmp", "-c", "a.c"]);
        assert!(bad.contains("none, return, indirect-jmp or all"), "{bad}");
        // gcc's other two answers, which the vDSO and a program that carries its own thunks ask for.
        let own = asked(&["-mindirect-branch=thunk", "-mfunction-return=thunk-inline"]).speculation;
        assert_eq!(
            (own.indirect, own.returns),
            (rucc_target::Thunk::Comdat, rucc_target::Thunk::Inline)
        );
        let bad = refused(&[KERNEL_X86, "-mindirect-branch=extern", "-c", "a.c"]);
        assert!(bad.contains("keep, thunk, thunk-inline or thunk-extern"), "{bad}");
        // A jump table is a question every target answers.
        assert!(!compile(&[KERNEL_ARM64, "-fno-jump-tables", "-c", "a.c"]).0.jump_tables);
    }

    /// The boundary is a power of two, as gcc spells it, and only on x86, where gcc has the flag.
    /// The least is 3 on x86-64 and 2 on i386, which is what the 32 bit kernel passes. 3 is the
    /// x86-64 kernel's, and it is taken with the vector registers on as well as off,
    /// since the kernel's display code turns SSE back on and keeps the boundary.
    #[test]
    fn the_preferred_stack_boundary_is_a_power_of_two_on_x86() {
        let boundary = |flag: &str| compile(&[KERNEL_X86, flag, "-c", "a.c"]).0.stack_boundary;
        assert_eq!(compile(&[KERNEL_X86, "-c", "a.c"]).0.stack_boundary, None);
        assert_eq!(boundary("-mpreferred-stack-boundary=4"), Some(16));
        assert_eq!(boundary("-mpreferred-stack-boundary=5"), Some(32));
        assert_eq!(boundary("-mpreferred-stack-boundary=12"), Some(4096));
        for bad in ["-mpreferred-stack-boundary=13", "-mpreferred-stack-boundary=2"] {
            assert!(refused(&[KERNEL_X86, bad, "-c", "a.c"]).contains("between 3 and 12"));
        }
        let sse = [KERNEL_X86, "-mpreferred-stack-boundary=3", "-msse", "-msse2", "-c", "a.c"];
        assert_eq!(compile(&sse).0.stack_boundary, Some(8));
        let eight = [KERNEL_X86, "-mpreferred-stack-boundary=3", "-mno-sse", "-c", "a.c"];
        assert_eq!(compile(&eight).0.stack_boundary, Some(8));
        let eight = [KERNEL_X86, "-mno-sse", "-mpreferred-stack-boundary=3", "-c", "a.c"];
        assert_eq!(compile(&eight).0.stack_boundary, Some(8));
        let i386 = |more: &[&str]| {
            let mut line = vec![KERNEL_X86, "-m32", "-fno-pic"];
            line.extend_from_slice(more);
            line.extend_from_slice(&["-c", "a.c"]);
            compile(&line).0.stack_boundary
        };
        assert_eq!(i386(&["-mpreferred-stack-boundary=2"]), Some(4));
        assert_eq!(i386(&["-mpreferred-stack-boundary=2", "-m32"]), Some(4));
        let low = refused(&[KERNEL_X86, "-m32", "-mpreferred-stack-boundary=1", "-c", "a.c"]);
        assert!(low.contains("between 2 and 12"), "{low}");
        let arm = refused(&[KERNEL_ARM64, "-mpreferred-stack-boundary=4", "-c", "a.c"]);
        assert!(arm.contains("unknown option"), "{arm}");
    }

    /// tamnd/rucc#2277. The kernel keeps the vector registers and the x87 stack out of every
    /// function, which is `-mno-sse` and `-mno-80387` on x86-64 and `-mgeneral-regs-only` on
    /// either machine.
    #[test]
    fn the_kernel_can_take_the_vector_and_x87_registers_away() {
        let files = |line: &[&str]| {
            let (opts, _) = compile(&[line, &["-c", "a.c"]].concat());
            (opts.vector, opts.x87)
        };
        assert_eq!(files(&[KERNEL_X86]), (true, true));
        assert_eq!(files(&[KERNEL_X86, "-mno-sse"]), (false, true));
        assert_eq!(files(&[KERNEL_X86, "-mno-80387"]), (true, false));
        assert_eq!(files(&[KERNEL_X86, "-msoft-float"]), (true, false));
        assert_eq!(files(&[KERNEL_X86, "-mno-80387", "-m80387"]), (true, true));
        assert_eq!(files(&[KERNEL_X86, "-mgeneral-regs-only"]), (false, false));
        assert_eq!(files(&[KERNEL_ARM64]), (true, true));
        assert_eq!(files(&[KERNEL_ARM64, "-mgeneral-regs-only"]), (false, true));
        // What the x86-64 kernel passes, all of it.
        let kernel = [KERNEL_X86, "-mno-sse", "-mno-mmx", "-mno-sse2", "-mno-80387"];
        assert_eq!(files(&[&kernel[..], &["-mno-fp-ret-in-387"]].concat()), (false, false));
        // The kernel's display code turns SSE and the x87 stack back on and keeps the flag, and
        // gcc takes that and refuses only a function that returns a `long double`.
        let display =
            [&kernel[..], &["-mno-fp-ret-in-387", "-msse", "-msse2", "-mhard-float", "-c", "a.c"]];
        let (opts, _) = compile(&display.concat());
        assert!(opts.vector && opts.x87 && !opts.x87_return);
        // `-mno-80387` is an x86 flag, and gcc for AArch64 does not know it.
        let said = refused(&[KERNEL_ARM64, "-mno-80387", "-c", "a.c"]);
        assert!(said.contains("unknown option"), "{said}");
        // What the i386 kernel passes. Every extension is already off there, and the x87 stack
        // is where every float is, so `-msoft-float` leaves a float nowhere to be.
        let i386 = [KERNEL_X86, "-m32", "-fno-pic", "-msoft-float", "-mno-sse", "-mno-mmx"];
        let i386 = [&i386[..], &["-mno-sse2", "-mno-3dnow", "-mno-avx"]].concat();
        assert_eq!(files(&i386), (true, false));
        // The flags the i386 kernel builds a floating point unit with, which ask for the SSE2 the
        // backend does its arithmetic in and so are taken, and an extension after them that is not.
        let fpu = [&i386[..], &["-msse", "-msse2", "-mhard-float"]].concat();
        assert_eq!(files(&fpu), (true, true));
        let (opts, _) = compile(&[&fpu[..], &["-c", "a.c"]].concat());
        assert_eq!(opts.isa, rucc_target::Isa::NONE);
        let said = refused(&[KERNEL_X86, "-m32", "-msse2", "-msse3", "-c", "a.c"]);
        assert!(said.contains("-msse3: this compiler builds i386 code"), "{said}");
    }

    /// The canary moved to where a kernel keeps it, which is `%gs:40` up to 6.12 and a symbol read
    /// through `%gs` from 6.13 on, and to a plain global, each as gcc reads the four flags.
    #[test]
    fn the_kernel_can_move_the_canary() {
        use rucc_target::{Guard, Segment};

        let guard = |more: &[&str]| {
            let line = [&[KERNEL_X86, "-c", "a.c"], more].concat();
            compile(&line).0.guard
        };
        assert_eq!(guard(&[]), None);
        let gs = ["-mstack-protector-guard-reg=gs", "-mstack-protector-guard-offset=40"];
        assert_eq!(guard(&gs), Some(Guard::in_segment(Segment::Gs, 40)));
        let hex = ["-mstack-protector-guard-offset=0x28"];
        assert_eq!(guard(&hex), Some(Guard::in_segment(Segment::Fs, 40)));
        let symbol = [
            "-mstack-protector-guard-reg=gs",
            "-mstack-protector-guard-symbol=__ref_stack_chk_guard",
        ];
        let got = guard(&symbol).expect("the guard moved");
        assert_eq!(
            (got.segment, got.symbol, got.table),
            (Some(Segment::Gs), Some("__ref_stack_chk_guard"), false)
        );
        // A symbol wins over an offset, as it does for gcc.
        let both = ["-mstack-protector-guard-symbol=foo", "-mstack-protector-guard-offset=8"];
        assert_eq!(guard(&both).map(|it| (it.segment, it.at)), Some((Some(Segment::Fs), 0)));
        // A global ignores the other three, and under -fPIC its address is read out of the table.
        let global = ["-mstack-protector-guard=global", "-mstack-protector-guard-reg=gs", "-fPIC"];
        let got = guard(&global).expect("the guard moved");
        assert_eq!((got.segment, got.symbol, got.table), (None, Some("__stack_chk_guard"), true));
        // And the later of two wins, which here is the target's own place again.
        let back = ["-mstack-protector-guard=global", "-mstack-protector-guard=tls"];
        assert_eq!(guard(&back), None);
        // The kernel's code model reads it through `%gs` unless a flag names the register.
        let kernel = ["-mcmodel=kernel", "-fno-PIE"];
        assert_eq!(guard(&kernel), Some(Guard::in_segment(Segment::Gs, 40)));
        let fs = ["-mcmodel=kernel", "-fno-PIE", "-mstack-protector-guard-reg=fs"];
        assert_eq!(guard(&fs), Some(Guard::in_segment(Segment::Fs, 40)));

        for bad in [
            "-mstack-protector-guard-reg=ds",
            "-mstack-protector-guard-offset=x",
            "-mstack-protector-guard-offset=4294967296",
            "-mstack-protector-guard-symbol=",
        ] {
            let said = refused(&[KERNEL_X86, bad, "-c", "a.c"]);
            assert!(said.starts_with(bad), "{said}");
        }
    }

    /// An i386 kernel built for SMP reads its canary at `%fs:__stack_chk_guard`, one built without
    /// SMP reads the plain global, and `gcc-x86_32-has-stack-protector.sh` looks for the `%fs` in
    /// the first. The default place on i386 is `%gs:20`, so an offset alone stays behind `%gs`.
    #[test]
    fn an_i386_kernel_can_move_the_canary() {
        use rucc_target::{Guard, Segment};

        let guard = |more: &[&str]| {
            let line =
                [&["--target=x86_64-unknown-linux-gnu", "-m32", "-fno-pic", "-c", "a.c"], more]
                    .concat();
            compile(&line).0.guard
        };
        assert_eq!(guard(&[]), None);
        let smp =
            ["-mstack-protector-guard-reg=fs", "-mstack-protector-guard-symbol=__stack_chk_guard"];
        let got = guard(&smp).expect("the guard moved");
        assert_eq!(
            (got.segment, got.symbol, got.table, got.fail),
            (Some(Segment::Fs), Some("__stack_chk_guard"), false, "__stack_chk_fail")
        );
        let global = guard(&["-mstack-protector-guard=global"]).expect("the guard moved");
        assert_eq!((global.segment, global.symbol), (None, Some("__stack_chk_guard")));
        let offset = ["-mstack-protector-guard-offset=24"];
        assert_eq!(guard(&offset), Some(Guard::in_segment(Segment::Gs, 24)));

        let line = ["--target=i686-unknown-linux-gnu", "-fPIC", "-mstack-protector-guard=global"];
        let said = refused(&[&line[..], &["-c", "a.c"]].concat());
        assert!(said.contains("i386 position independent code"), "{said}");
        // The kernel's probe runs with the default, which is PIE, and gcc puts the symbol in the
        // instruction there too.
        let line = ["--target=i686-unknown-linux-gnu", "-fPIC", "-c", "a.c"];
        let got = compile(&[&line[..], &smp[..]].concat()).0.guard.expect("the guard moved");
        assert_eq!(
            (got.segment, got.symbol, got.table, got.fail),
            (Some(Segment::Fs), Some("__stack_chk_guard"), false, "__stack_chk_fail_local")
        );
    }

    /// An arm64 kernel keeps its canary in the task, `sp_el0` plus an offset, and everything else on
    /// AArch64 Linux reads the global, through the table unless the code is not position
    /// independent. gcc wants all three flags for the first, and so does this.
    #[test]
    fn an_arm64_kernel_can_move_the_canary() {
        use rucc_target::Guard;

        let guard = |more: &[&str]| {
            let line = [&[KERNEL_ARM64, "-c", "a.c"], more].concat();
            compile(&line).0.guard
        };
        let task = [
            "-mstack-protector-guard=sysreg",
            "-mstack-protector-guard-reg=sp_el0",
            "-mstack-protector-guard-offset=0x778",
        ];
        assert_eq!(guard(&task), Some(Guard::in_task(1912)));
        assert_eq!(guard(&["-fno-PIE"]), Some(Guard::global("__stack_chk_guard", false)));
        assert_eq!(guard(&[]), Some(Guard::global("__stack_chk_guard", true)));
        let back = [&task[..], &["-mstack-protector-guard=global"]].concat();
        assert!(refused(&[&[KERNEL_ARM64, "-c", "a.c"], &back[..]].concat()).contains("sysreg"));

        for bad in [
            &task[..1],
            &task[..2],
            &task[1..],
            &[task[0], task[1], "-mstack-protector-guard-offset=12"],
            &[task[0], task[1], "-mstack-protector-guard-offset=32768"],
            &["-mstack-protector-guard-reg=tpidr_el1"],
            &["-mstack-protector-guard=tls"],
        ] {
            let line = [&[KERNEL_ARM64, "-c", "a.c"], bad].concat();
            let said = refused(&line);
            assert!(said.contains("-mstack-protector-guard"), "{bad:?}: {said}");
        }
    }

    /// tamnd/rucc#2282. The last choice written wins, and one gcc does not have is refused.
    #[test]
    fn the_choices_of_trivial_auto_var_init_are_read() {
        let init =
            |flags: &[&str]| compile(&[&[KERNEL_X86, "-c", "a.c"], flags].concat()).0.auto_var_init;
        assert_eq!(init(&[]), None);
        assert_eq!(init(&["-ftrivial-auto-var-init=zero"]), Some(0));
        assert_eq!(init(&["-ftrivial-auto-var-init=pattern"]), Some(0xfe));
        let back = ["-ftrivial-auto-var-init=zero", "-ftrivial-auto-var-init=uninitialized"];
        assert_eq!(init(&back), None);
        let wrong = refused(&[KERNEL_X86, "-ftrivial-auto-var-init=ones", "-c", "a.c"]);
        assert!(wrong.contains("pattern"), "{wrong}");
    }

    #[test]
    fn every_choice_of_zero_call_used_regs_is_read() {
        let zero = |target: &str, flags: &[&str]| {
            compile(&[&[target, "-c", "a.c"], flags].concat()).0.zero_regs
        };
        for target in [KERNEL_X86, KERNEL_ARM64] {
            assert_eq!(zero(target, &[]), None);
            for (choice, read) in [
                ("used-gpr", (false, false, false)),
                ("used-gpr-arg", (false, true, false)),
                ("all-gpr", (true, false, false)),
                ("all-gpr-arg", (true, true, false)),
                ("used", (false, false, true)),
                ("used-arg", (false, true, true)),
                ("all", (true, false, true)),
                ("all-arg", (true, true, true)),
            ] {
                let flag = format!("-fzero-call-used-regs={choice}");
                assert_eq!(zero(target, &[&flag]), Some(read), "{flag}");
            }
            let back = ["-fzero-call-used-regs=all", "-fzero-call-used-regs=skip"];
            assert_eq!(zero(target, &back), None);
        }
        let wrong = refused(&[KERNEL_X86, "-fzero-call-used-regs=some", "-c", "a.c"]);
        assert!(wrong.contains("all-arg"), "{wrong}");
    }

    /// The flags kbuild passes that this compiler cannot honor, each refused with the reason, so
    /// that the person reading the error knows it is not a typo.
    #[test]
    fn a_kernel_flag_that_is_not_honored_says_why() {
        // Refused with no issue, because nothing is planned for them, and still with the reason.
        for flag in ["-fstack-check", "-fplugin=a.so"] {
            let failed = refused(&[KERNEL_X86, flag, "-c", "a.c"]);
            assert!(failed.starts_with(&format!("{flag}: ")) && !failed.contains('#'), "{failed}");
        }
        // A sanitizer rather than a typo, so it gets the refusal every sanitizer gets.
        let failed = refused(&[KERNEL_ARM64, "-fsanitize=shadow-call-stack", "-c", "a.c"]);
        assert!(failed.contains("no sanitizer instrumentation"), "{failed}");
    }

    /// A flag of one architecture is an unknown option on another, as it is to gcc, and not an
    /// answer about a target that was not asked for.
    #[test]
    fn a_kernel_flag_of_the_other_architecture_is_unknown() {
        for (target, flag) in [
            (KERNEL_X86, "-mno-outline-atomics"),
            (KERNEL_X86, "-ffixed-x18"),
            (KERNEL_X86, "-mbranch-protection=none"),
            (KERNEL_ARM64, "-mskip-rax-setup"),
            (KERNEL_ARM64, "-mrecord-mcount"),
            (KERNEL_ARM64, "-mindirect-branch=thunk-extern"),
            (KERNEL_ARM64, "-mfunction-return=thunk-extern"),
            (KERNEL_ARM64, "-mindirect-branch-cs-prefix"),
            (KERNEL_ARM64, "-mmanual-endbr"),
        ] {
            assert_eq!(refused(&[target, flag, "-c", "a.c"]), format!("unknown option `{flag}`"));
        }
    }

    /// `-fshort-wchar` is honored rather than dropped: it is the width and the signedness of
    /// `wchar_t`, which the session puts into the target for everything that asks.
    #[test]
    fn short_wchar_is_carried_to_the_session() {
        let (opts, _) = compile(&[KERNEL_X86, "-c", "a.c"]);
        assert!(!opts.short_wchar);
        let (opts, _) = compile(&[KERNEL_X86, "-fshort-wchar", "-c", "a.c"]);
        let session = Session::new(*opts);
        assert_eq!((session.target.wchar_width, session.target.wchar_is_signed), (16, false));
        let (opts, _) = compile(&[KERNEL_X86, "-fshort-wchar", "-fno-short-wchar", "-c", "a.c"]);
        assert!(!opts.short_wchar, "the last one wins");
    }

    /// `-fmin-function-alignment=` is a floor that `-falign-functions` may raise and not lower,
    /// whichever of the two is written last. The kernel passes it with the boundary its call
    /// padding needs.
    #[test]
    fn the_minimum_function_alignment_is_a_floor() {
        let align = |flags: &[&str]| {
            let mut line = flags.to_vec();
            line.extend(["-c", "a.c"]);
            compile(&line).0.align_functions
        };
        assert_eq!(align(&["-fmin-function-alignment=16"]), None, "the default is sixteen");
        assert_eq!(align(&["-fmin-function-alignment=8"]), None, "and a lower floor is under it");
        assert_eq!(align(&["-fmin-function-alignment=64"]), Some(64));
        assert_eq!(align(&["-fmin-function-alignment=33"]), Some(64), "rounded up as gcc does");
        assert_eq!(align(&["-fmin-function-alignment=32", "-falign-functions=8"]), Some(32));
        assert_eq!(align(&["-falign-functions=8", "-fmin-function-alignment=32"]), Some(32));
        assert_eq!(align(&["-fmin-function-alignment=16", "-falign-functions=64"]), Some(64));
        assert_eq!(align(&["-fno-align-functions", "-fmin-function-alignment=16"]), Some(16));
        let failed = refused(&["-fmin-function-alignment=big", "-c", "a.c"]);
        assert!(failed.contains("number of bytes"), "{failed}");
    }

    /// Every `-W` flag the kernel's Makefiles pass for gcc is one gcc 16 knows, and so one this
    /// compiler takes, and the ones they pass only for clang are refused the way gcc refuses them,
    /// which is what makes `cc-option` and `cc-disable-warning` give gcc's answers.
    #[test]
    fn the_kernel_s_warning_flags_get_gcc_s_answers() {
        for flag in [
            "-Wall",
            "-Wextra",
            "-Wundef",
            "-Wstrict-prototypes",
            "-Wno-trigraphs",
            "-Werror=implicit-function-declaration",
            "-Werror=implicit-int",
            "-Werror=return-type",
            "-Werror=date-time",
            "-Werror=incompatible-pointer-types",
            "-Werror=designated-init",
            "-Wno-format-security",
            "-Wno-frame-address",
            "-Wno-address-of-packed-member",
            "-Wframe-larger-than=2048",
            "-Wvla",
            "-Wno-pointer-sign",
            "-Wcast-function-type",
            "-Wno-array-bounds",
            "-Wno-alloc-size-larger-than",
            "-Wimplicit-fallthrough=5",
            "-Wenum-conversion",
            "-Wno-dangling-pointer",
            "-Wno-stringop-overflow",
            "-Wno-stringop-truncation",
            "-Wno-format-truncation",
            "-Wno-override-init",
            "-Wno-maybe-uninitialized",
            "-Wmissing-declarations",
            "-Wmissing-prototypes",
            "-Wmissing-format-attribute",
            "-Wmissing-include-dirs",
            "-Wold-style-definition",
            "-Wpacked-not-aligned",
            "-Wlogical-op",
            "-Wnested-externs",
            "-Wunterminated-string-initialization",
            "-Walloc-size-larger-than=18446744073709551615",
            "-Wno-unaligned-access",
            "-Wno-format-overflow-non-kprintf",
        ] {
            compile(&[flag, "-c", "a.c"]);
        }
        for flag in ["-Wthread-safety", "-Wdefault-const-init-unsafe"] {
            assert_eq!(refused(&[flag, "-c", "a.c"]), format!("unknown option `{flag}`"));
        }
        for name in ["unknown-warning-option", "option-ignored", "unused-command-line-argument"] {
            let flag = format!("-Werror={name}");
            assert_eq!(refused(&[&flag, "-c", "a.c"]), format!("`{flag}`: no option `-W{name}`"));
        }
    }

    /// The kernel's `CONFIG_CC_HAS_MARCH_NATIVE` probe, which a cross gcc fails.
    #[test]
    fn native_is_refused_where_the_host_is_not_the_target() {
        let (here, there) = if cfg!(target_arch = "x86_64") {
            (KERNEL_X86, KERNEL_ARM64)
        } else {
            (KERNEL_ARM64, KERNEL_X86)
        };
        let why = refused(&[there, "-march=native", "-c", "a.c"]);
        assert!(why.starts_with("bad value `native` for `-march=`"), "{why}");
        if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
            compile(&[here, "-march=native", "-c", "a.c"]);
        }
        compile(&[there, "-mtune=native", "-c", "a.c"]);
    }

    /// kbuild probes these with `cc-option` and gcc 14 refuses them, so a build claiming gcc 14
    /// refuses them too, wherever the claim is on the line, and a claim of the release that added
    /// one takes it.
    #[test]
    fn a_flag_newer_than_the_claimed_gcc_is_unknown_to_it() {
        for flag in [
            "-fzero-init-padding-bits=all",
            "-fdiagnostics-show-context=2",
            "-Wunterminated-string-initialization",
        ] {
            let why = format!("unknown option `{flag}`");
            assert_eq!(refused(&[KERNEL_X86, "-fgnuc-version=14.2.0", flag, "-c", "a.c"]), why);
            assert_eq!(refused(&[KERNEL_X86, flag, "-fgnuc-version=14", "-c", "a.c"]), why);
            compile(&[KERNEL_X86, flag, "-fgnuc-version=16.1.0", "-c", "a.c"]);
        }
        compile(&[KERNEL_X86, "-fgnuc-version=15", "-fzero-init-padding-bits=all", "-c", "a.c"]);
        // gcc 13 brought the flexible array levels, which the kernel probes for on gcc 12.
        for flag in ["-fstrict-flex-arrays=3", "-fstrict-flex-arrays", "-fno-strict-flex-arrays"] {
            let why = format!("unknown option `{flag}`");
            assert_eq!(refused(&[KERNEL_X86, "-fgnuc-version=12.2.0", flag, "-c", "a.c"]), why);
            compile(&[KERNEL_X86, "-fgnuc-version=13", flag, "-c", "a.c"]);
        }
        let flag = "-fmin-function-alignment=16";
        let why = format!("unknown option `{flag}`");
        assert_eq!(refused(&[KERNEL_X86, "-fgnuc-version=13.3.0", flag, "-c", "a.c"]), why);
        compile(&[KERNEL_X86, "-fgnuc-version=14", flag, "-c", "a.c"]);
        let why = refused(&["-fgnuc-version=15", "-fdiagnostics-show-context", "-c", "a.c"]);
        assert_eq!(why, "unknown option `-fdiagnostics-show-context`");
        // gcc takes `-Wno-` of a name it does not know, and kbuild probes the positive form.
        compile(&["-fgnuc-version=14", "-Wno-unterminated-string-initialization", "-c", "a.c"]);
        let flag = "-Werror=unterminated-string-initialization";
        let why = format!("`{flag}`: no option `-Wunterminated-string-initialization`");
        assert_eq!(refused(&["-fgnuc-version=14", flag, "-c", "a.c"]), why);
    }
}
