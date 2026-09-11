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
//! `-fgnuc-version=`, `-ansi`, `-ffreestanding`, `-fno-builtin`, `-fno-builtin-<name>`,
//! `-fgnu89-inline`, `-pedantic` and `-Werror`.
//! The phases after them still say they are not implemented.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-driver/0.10.20")]

pub mod cache;
pub mod compile;
pub mod deps;
pub mod library;
pub mod link;
mod map;
pub mod phase;
pub mod preprocess;
pub mod schedule;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use rucc_codegen::coverage::{self, Fired};
use rucc_codegen::pressure::Pressure;
use rucc_pp::Dependency;
use rucc_session::{
    Compress, Control, Dumps, EmitKind, Hook, Options, Pic, PrefixMap, Preinclude, Protector,
    SaveTemps, Session, Std, Wrapping, runtime,
};
use rucc_sysroot::{Manifest, Sysroot};
use rucc_target::Triple;

use crate::link::LinkOptions;

pub use crate::compile::{Artifact, Compiled, Temps, compile, compile_ir};
pub use crate::phase::{Input, InputKind, Job, LinkJob, Output, Phase, Plan};
pub use crate::preprocess::{OsFileSystem, Preprocessed, preprocess};
pub use crate::schedule::Jobs;

/// The compiler's version, taken from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Print usage and exit successfully.
    Help,
    /// Print the version and exit successfully.
    Version,
    /// Print one line and exit successfully, which is what the `-dump` and `-print` family do.
    ///
    /// A build system asks these before it compiles anything, and what it does with the answer
    /// is paste it into a path or into another command line, so each one is a single line with
    /// no decoration around it.
    Print(String),
    /// Print the resolved configuration and exit successfully.
    PrintConfig(Box<Options>),
    /// Print the passes the level will run and exit successfully.
    PrintPipeline(Box<Options>),
    /// Print the phase plan and the link line and exit successfully, which is `-###`.
    PrintPlan {
        /// The resolved options, which is what says what the link line is for.
        opts: Box<Options>,
        /// What to do to each input, and in what order.
        plan: Box<Plan>,
        /// What the command line said about linking.
        link: Box<LinkOptions>,
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
    /// `-dumpversion` and `-dumpfullversion`, which are the same three numbers here.
    Version,
    /// `-print-multiarch`, the directory name a distribution files this target under.
    Multiarch,
    /// `-print-search-dirs`, in the three lines GCC prints.
    SearchDirs,
    /// `-print-sysroot`, the root the headers and the libraries are read under.
    Sysroot,
    /// `-print-sysroot-provenance`, what is in that root and where each of it came from.
    SysrootProvenance,
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
  -D <name>[=<value>], -U <name>      define a macro, or undefine one after every -D
  -I <dir>               add <dir> to the include search path
  -iquote -isystem -idirafter <dir>   the other chains, -nostdinc drops ours
  -I-, -iprefix <p>, -iwithprefix[before] <dir>   the older spellings of those
  -include <file>, -imacros <file>    read <file> first, the second for its macros only
  --sysroot=<dir>        look for the library's headers under <dir>, -isysroot too
  -P, -dM                with -E: leave out the markers, or dump the macros
  -M -MM -MD -MMD        write a make rule for the source, the last two compile as well
  -MF <file> -MT <t> -MQ <t> -MP   where the rule goes, what it builds, targets with no recipe
  -std=<dialect>         c89 through c23, and the gnu spellings
  -fgnuc-version=<v>     the GCC release to claim, default 7.0.0
  -x <lang>              treat later inputs as <lang>, or none to stop
  -O<level>              optimize: 0, 1, 2, 3, s, z
  -fsafety=<tier>        check memory safety: off, detect, enforce, kernel
  -f[no-]sanitize=<what>   the negative is taken, the positive is refused by name
  -f[no-]safety-subobject   a write has to stay inside the member it names
  -f[no-]safety-restrict    two restrict pointers of one block may not meet
  -f<pass> -fno-<pass> -fdump-ir=<what> -fopt-info[-<kind>][=FILE]
  -fpass-fuel=<pass>=<n>, -fpass-fuel-global=<n>   stop a pass, or all of them, after n
  -fdisable-<pass>[=<funcs>], -fenable-<pass>[=<funcs>]   run a pass on some functions only
  -g -g0 -gdwarf-5, -fno-omit-frame-pointer, -mno-red-zone   debug info, frame pointer, red zone
  -gz[=none|zlib|zlib-gnu|zstd] -gno-split-dwarf   compress debug sections, one file not two
  -flto[=auto|jobserver|<n>] -fno-lto -ffat-lto-objects   read, and not done yet
  -fprofile-use[=<path>] -fprofile-dir=<dir>   read too, where -fprofile-generate is refused
  -f[no-]stack-protector[-strong|-all], -f[no-]stack-clash-protection, -fcf-protection=<edges>
  -ffunction-sections -fdata-sections   a section per function or variable, for --gc-sections
  -fvisibility=<what>    default, hidden, internal or protected, when nothing in the source said
  -l<name>, -L <dir>, -B <dir>   link a library, where to look for one, where our own tools are
  -fPIC -fpic -fPIE -fpie, -fno-common, -pipe   what it does anyway
  -f[no-]strict-aliasing, -f[no-]delete-null-pointer-checks   what it assumes anyway
  -static -shared -pie -no-pie -nostdlib -nostartfiles -nodefaultlibs -rdynamic -s   how to link
  -Wl,<arg>, -Xlinker <arg>, -fuse-ld=<name>   hand an argument to the linker, or pick one
  -Werror -pedantic -pedantic-errors -w   how much to say, and whether it is fatal
  -m64 -march= -mtune= -mcpu= -mabi= -mcmodel=   what machine to generate for
  -pg -p, -mfentry -mno-fentry   call a profiler on the way in, and where that call goes
  -fpatchable-function-entry=<n>[,<m>]   room at the top of every function to patch later
  -fwrapv, -fwrapv-pointer, -fno-strict-overflow   signed or pointer overflow wraps
  -ftrapv                signed overflow stops the program instead
  -f[no-]signed-char, -f[no-]unsigned-char, -f[no-]short-enums   change the ABI
  -ffp-contract=<how>    fuse a multiply and an addition: fast, on or off
  -fexcess-precision=<how>, -f[no-]rounding-math, -f[no-]trapping-math   what it does anyway
  -ffile-prefix-map=<old>=<new>   rewrite that front of every path we put in the output
  -fmacro-prefix-map= -fdebug-prefix-map= -fprofile-prefix-map=   the same, one output each
  -pthread               build for more than one thread, and link the library for it
  -dumpmachine -dumpversion -print-multiarch -print-search-dirs   what this compiler is
  -print-file-name=<name> -print-prog-name=<name>   where a file or a program is
  -print-sysroot         the root the headers and the libraries are read under
  -print-sysroot-provenance   every input under it, where it came from and its licence
  -j[n]                  compile n translation units at once, default all
  -v, -###               print each phase as it runs, or without running any
  -save-temps[=cwd|obj], -time   keep the .i and the .s, say how long each step took
  --target=<triple>      generate code for <triple>
  --emit=<kind>          exe, obj, asm, preprocessed, tast, ir, mir-final,
                         safety-summary, type-granules
  --print-config, --print-pipeline    print the configuration or the pipeline, and exit
  --version              print the version and exit
  -h, --help             print this message and exit

See spec/04-driver-and-cli.md for the full flag reference.
";

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
const SANITIZERS: [&str; 34] = [
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
    "alias",
    "restrict",
    "memory",
];

/// Parses a command line, without the program name.
///
/// # Errors
///
/// Returns the message to print when the arguments do not name a compilation this compiler
/// can attempt.
pub fn parse_args(args: &[String]) -> Result<Action, CliError> {
    let host = Triple::host()
        .ok_or_else(|| err("this host is not a supported target and no --target was given"))?;
    let mut opts = Options::new(host);
    let mut inputs: Vec<Input> = Vec::new();
    let mut print_config = false;
    let mut print_pipeline = false;
    let mut print_plan = false;
    let mut verbose = false;
    let mut jobs = Jobs::default();
    let mut nostdinc = false;
    let mut sysroot: Option<PathBuf> = None;
    // The whole ten field target, kept beside the three field one because `--target=` can pin a
    // libc version and `Triple` has nowhere to put it. It decides `__GLIBC_MINOR__` and nothing
    // else today, and `None` is a command line that named no target, which is this machine.
    let mut pinned: Option<rucc_tuple::TargetTuple> = None;
    let mut output = None;
    let mut link = LinkOptions::default();
    let mut query: Option<Query> = None;
    let mut threads = false;
    // Which sanitizers are still asked for by the end of the command line. Accumulated across the
    // loop rather than answered where it was read, because `-fno-sanitize=` turns one off and a
    // build that asks for a check and then takes it back has asked for nothing. What happens to a
    // set that is not empty is decided after the loop.
    let mut sanitizers: Vec<&str> = Vec::new();
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

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        match arg {
            "-h" | "--help" => return Ok(Action::Help),
            "--version" => return Ok(Action::Version),
            "--print-config" => print_config = true,
            "--print-pipeline" => print_pipeline = true,
            "-###" => print_plan = true,
            "-v" => verbose = true,
            // The files a compilation goes through, kept rather than thrown away. The bare
            // spelling means `=obj` and not `=cwd`, which is not what the manual says and is what
            // gcc 16 does; `SaveTemps::Object` carries the measurement.
            "-save-temps" => opts.save_temps = SaveTemps::Object,
            _ if arg.starts_with("-save-temps=") => {
                opts.save_temps = arg["-save-temps=".len()..].parse().map_err(err)?;
            }
            // How long each step took. A misspelling of this is worth rejecting rather than
            // ignoring, since a run that says nothing looks like a compilation that took no time.
            "-time" => opts.time = true,
            "-c" => opts.emit = EmitKind::Object,
            "-S" => opts.emit = EmitKind::Asm,
            "-E" => opts.emit = EmitKind::Preprocessed,
            "-g" => opts.debug_info = true,
            // GCC's own levels of how much debug information to write. Zero is none and every
            // other number is some, and this compiler has one amount, so the numbers above zero
            // all mean the same thing here. `-ggdb` is the same flag asking for whatever the
            // debugger on the machine prefers, which is what we emit anyway.
            "-g0" => opts.debug_info = false,
            "-g1" | "-g2" | "-g3" | "-ggdb" | "-ggdb1" | "-ggdb2" | "-ggdb3" => {
                opts.debug_info = true;
            }
            // The version of DWARF to write. We write DWARF 5 and nothing else, so a build that
            // asks for another version is told rather than handed a file it cannot read.
            "-gdwarf" | "-gdwarf-5" => opts.debug_info = true,
            _ if arg.starts_with("-gdwarf-") => {
                return Err(err(format!(
                    "{arg}: this compiler writes DWARF 5 and no other version, see \
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
            "-gsplit-dwarf" => {
                return Err(err(format!(
                    "{arg}: this compiler writes no separate `.dwo` file, and a build that \
                     expects one beside each object would wait for a file that never arrives, \
                     see spec/11-debug-info.md"
                )));
            }
            // How the debug sections are compressed. There are none yet, so every answer produces
            // the same bytes and taking the flag promises nothing that is not kept. The value is
            // still checked, because a typo in a distribution's flags is worth finding when the
            // compiler reads it rather than when somebody later wonders why nothing got smaller.
            // Bare `-gz` means `zlib`, which the manual leaves for the reader to discover.
            "-gz" => opts.compress = Compress::Zlib,
            _ if arg.starts_with("-gz=") => {
                let how = &arg["-gz=".len()..];
                opts.compress = how.parse().map_err(|()| {
                    err(format!(
                        "`{how}` is not a way to compress debug sections, which is none, zlib, \
                         zlib-gnu or zstd"
                    ))
                })?;
            }
            "-Werror" => opts.warnings_are_errors = true,
            // Nothing that is not fatal is said at all. Read at the one place a diagnostic goes
            // through rather than here, so that a warning `-w` dropped is not counted either.
            "-w" => opts.warnings = false,
            "-pedantic-errors" => {
                opts.pedantic = true;
                opts.warnings_are_errors = true;
            }
            "-P" => opts.line_markers = false,
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
            "-dumpmachine" => query = Some(Query::Machine),
            "-dumpversion" | "-dumpfullversion" => query = Some(Query::Version),
            "-print-multiarch" => query = Some(Query::Multiarch),
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
            }
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
            // Both directions of each, because a build system that wants one of these usually
            // writes it beside the flag that turns it back off for one directory.
            "-fno-omit-frame-pointer" => opts.frame_pointer = true,
            "-fomit-frame-pointer" => opts.frame_pointer = false,
            "-mno-red-zone" => opts.red_zone = false,
            "-mred-zone" => opts.red_zone = true,
            // Four flags rather than one with an argument, which is how gcc spells them and how
            // every build line writes them. Last one wins, because a package build puts
            // `-fstack-protector-strong` in its global flags and a directory that cannot have one
            // turns it back off on the line after.
            "-fno-stack-protector" | "-fno-stack-protector-all" | "-fno-stack-protector-strong" => {
                opts.protector = Protector::None;
            }
            "-fstack-protector" => opts.protector = Protector::Buffers,
            "-fstack-protector-strong" => opts.protector = Protector::Strong,
            "-fstack-protector-all" => opts.protector = Protector::All,
            // The other half of what a hardened build asks for, and it is a question about the
            // frame rather than about the function, so it is a switch rather than a level.
            "-fstack-clash-protection" => opts.stack_clash = true,
            "-fno-stack-clash-protection" => opts.stack_clash = false,
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
            // GCC drops its own include directory along with the system ones, because its
            // headers are half of a pair with the library's and half a pair is worse than
            // none. A build that passes this is supplying the whole set itself.
            "-nostdinc" => nostdinc = true,
            "-o" => {
                output = Some(args.get(i).ok_or_else(|| err("-o requires an argument"))?.clone());
                i += 1;
            }
            // The flags that take a directory only in the separated form. GCC spells them
            // this way and nothing writes `-iquotedir`, so accepting the joined form would
            // mean guessing at a path that starts with the flag's own letters.
            // Apple's spelling of `--sysroot`, and the one its own build systems pass. The
            // two mean the same thing here: the configured directories are under there rather
            // than under the root.
            "-isysroot" => {
                let dir = args.get(i).ok_or_else(|| err("-isysroot requires an argument"))?;
                i += 1;
                sysroot = Some(PathBuf::from(dir));
            }
            "-iquote" | "-isystem" | "-idirafter" => {
                let dir = args.get(i).ok_or_else(|| err(format!("{arg} requires an argument")))?;
                i += 1;
                match arg {
                    "-iquote" => opts.search.push_quote(dir.clone()),
                    "-isystem" => opts.search.push_system(dir.clone()),
                    _ => opts.search.push_after(dir.clone()),
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
            "-x" => {
                let lang = args.get(i).ok_or_else(|| err("-x requires an argument"))?;
                i += 1;
                forced = if lang == "none" {
                    None
                } else {
                    Some(InputKind::from_x_arg(lang).map_err(|e| err(format!("{e}")))?)
                };
            }
            // Not a GCC flag. spec/03-architecture.md section 3.5 compiles several
            // translation units in one process rather than making the build system fork, and
            // section 3.8's determinism check compares `-j1` against `-j16`, so the knob has
            // to exist and has to be spelled the way `make` spells it.
            // `-DFOO`, `-D FOO` and the same for `-U` and `-I`. Both forms are in wide use
            // and a build system may produce either, so both are read here rather than
            // being normalised by whatever generated the command line.
            _ if arg.starts_with("-D") => {
                let value = joined_or_next(arg, 2, args, &mut i)?;
                opts.defines.push(value);
            }
            _ if arg.starts_with("-U") => {
                let value = joined_or_next(arg, 2, args, &mut i)?;
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
            _ if arg.starts_with("-fgnuc-version=") => {
                let v = &arg["-fgnuc-version=".len()..];
                opts.gnuc = v.parse().map_err(err)?;
            }
            // spec/13-gnu-compat.md section 13.3 promises this flag an error that says why rather
            // than the unknown option one, because a build reaching for it is asking for a feature
            // and deserves to be told it is not coming rather than told the spelling is wrong.
            // The negative form is what this compiler does anyway, so it is taken and dropped.
            "-fnested-functions" => {
                return Err(err(
                    "nested functions are not supported: a call to one goes through a trampoline \
                     written on the stack, which no target that enforces an unexecutable stack \
                     allows",
                ));
            }
            "-fno-nested-functions" => {}
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
            "-fPIC" | "-fpic" => opts.pic = Pic::Library,
            // Not a synonym of the pair above, which is what they were treated as until #756. The
            // library is the expensive answer and gcc makes it the one that has to be asked for,
            // so this is also what nothing at all means.
            "-fPIE" | "-fpie" => opts.pic = Pic::Executable,
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
            // The other direction is a request, not a description, and it is one this compiler
            // cannot grant, so it gets the treatment section 13.3 asks for rather than the unknown
            // option error. Answering it by carrying on would be answering a different question:
            // the code would still be position independent, which is correct everywhere an
            // ordinary program runs and is wrong in a kernel, where the flag is written precisely
            // because there is no loader to fill a global offset table in.
            "-fno-pic" | "-fno-pie" => {
                return Err(err(
                    "position dependent code is not supported: an address that may be in another \
                     object is loaded out of the global offset table, and nothing here emits the \
                     absolute form this asks for. Use -no-pie if what you meant was how to link",
                ));
            }
            // A section per function and a section per variable, which is what makes
            // `--gc-sections` able to drop anything: a linker can leave out a section nothing
            // reaches and cannot leave out half of one. Both directions are taken, and the off
            // one is the default rather than a refusal, since a build that writes it is asking
            // for what happens anyway.
            "-ffunction-sections" => opts.function_sections = true,
            "-fno-function-sections" => opts.function_sections = false,
            "-fdata-sections" => opts.data_sections = true,
            "-fno-data-sections" => opts.data_sections = false,
            // Another description of what this compiler does. A file scope declaration with no
            // initializer is written into `.bss` as an ordinary defined symbol, not offered to the
            // linker as a common one for it to merge, which is what `-fno-common` asks for and what
            // gcc has done by default since 10. Nothing in the front end produces `Linkage::Common`
            // at all.
            "-fno-common" => {}
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
            // And the request, which is the one that cannot be granted. It is a real difference and
            // not a preference: two files each writing `int g;` link under `-fcommon` and are a
            // duplicate definition without it, which is the whole reason the flag survives.
            "-fcommon" => {
                return Err(err(
                    "a tentative definition is written into .bss as its own symbol here, and \
                     nothing emits the common symbol this asks the linker to merge. Give the \
                     variable a definition in one file and declare it extern in the others",
                ));
            }
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
            // The floating point group, which goes the same way and for the same reason, and which
            // is worth writing out because the reason is easy to get backwards.
            //
            // Each of these has a restrictive spelling and a permissive one. The restrictive ones,
            // `-frounding-math` and `-ftrapping-math`, say that the rounding mode may have been
            // changed and that an exception raised by an operation may be looked at, so an
            // arithmetic the compiler folds at compile time is an arithmetic whose rounding and
            // whose exception the program does not get. Nothing here folds any floating point
            // arithmetic in a function body: `0.1 + 0.2` is an `fadd` and `1.0 / 0.0` is a divide
            // that runs, at every level. So both of those describe what already happens.
            //
            // The permissive ones are the other half, and they are licences rather than requests
            // for an answer. `-fno-rounding-math` says the rounding mode is the default one and
            // `-fno-trapping-math` says nothing looks at the exceptions, which together are
            // permission to fold. Not folding is the conservative side of that permission and is
            // what a program is entitled to whichever was written, so the flag costs speed and not
            // correctness, which is the test section 4.1 puts a licence through. `-ftrapping-math`
            // is also gcc's default, so a build spelling it out is a build asking for what it
            // already has.
            "-frounding-math" | "-fno-rounding-math" => {}
            "-ftrapping-math" | "-fno-trapping-math" => {}
            // About temporary files rather than about code. There is nothing between the phases of
            // one compilation here to write to a file in the first place.
            "-pipe" => {}
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
            "-shared" => link.shared = true,
            "-pie" => link.pie = Some(true),
            "-no-pie" | "-nopie" => link.pie = Some(false),
            "-nostdlib" => link.no_stdlib = true,
            "-nostartfiles" => link.no_startfiles = true,
            "-nodefaultlibs" => link.no_defaultlibs = true,
            "-fno-builtins-lib" => link.no_builtins_lib = true,
            "-fbuiltins-lib" => link.no_builtins_lib = false,
            "-rdynamic" | "-export-dynamic" => link.export_dynamic = true,
            "-s" => link.strip = true,
            "-Xlinker" => {
                let next = args.get(i).ok_or_else(|| err("-Xlinker requires an argument"))?;
                i += 1;
                link.passthrough.push(next.clone());
            }
            _ if arg.starts_with("-Wl,") => {
                // Commas separate arguments rather than being part of one, which is what makes
                // `-Wl,-rpath,/opt/lib` two words to the linker and one word here.
                link.passthrough.extend(arg["-Wl,".len()..].split(',').map(str::to_owned));
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
            _ if arg.starts_with("--target=") => {
                let t = &arg["--target=".len()..];
                opts.target = t.parse().map_err(|e| err(format!("{e}")))?;
                // The same string again, as the model that has room for a libc version. A spelling
                // the three field parser took and this one does not is not an error, because the
                // one that decides what is compiled has already accepted it and the only thing
                // lost is a version nobody asked for.
                pinned = t.parse().ok();
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
            "-O" | "-Og" => opts.opt_level = rucc_session::OptLevel::O1,
            // The union of `-O3` and `-ffast-math`, and the second half of that changes what
            // floating point arithmetic means. Refused rather than taken as `-O3`, because a
            // build that asks for fast math and is quietly given ordinary arithmetic gets a
            // slower program than it asked for and a build that is given fast math it did not
            // ask for gets a wrong one.
            "-Ofast" => {
                return Err(err(
                    "-Ofast is -O3 with fast math, and fast math is not implemented, see \
                     spec/04-driver-and-cli.md section 4.6",
                ));
            }
            _ if arg.starts_with("-O") => {
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
            // refused because of what ignoring it does. There is none of it here yet, so a build
            // that asks for it gets a program that is correct and slower than it could have been,
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
            // Writing the counts rather than reading them, which is refused rather than taken and
            // is the same line `-gsplit-dwarf` falls on the far side of. Ignoring these means a
            // file a build declared as an output never appears: the instrumented program writes a
            // `.gcda` as it exits and `-ftest-coverage` writes a `.gcno` beside the object, and a
            // two stage build that got neither would go on to optimize against no counts at all
            // and report coverage of nothing, with nothing along the way saying so. The objects
            // say the rest: gcc's `-fprofile-generate` object holds 375 bytes of code where a
            // plain one holds 71, and 296 bytes of counters that a plain one does not have, so
            // this is a flag that changes the output rather than a hint about speed.
            "-fprofile-arcs"
            | "--coverage"
            | "-fcondition-coverage"
            | "-fpath-coverage"
            | "-fprofile-generate" => {
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
            "-ftest-coverage" => {
                return Err(err(format!(
                    "{arg}: this compiler writes no `.gcno` file beside the object, and a build \
                     that expects one would wait for a file that never arrives, see \
                     spec/04-driver-and-cli.md"
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
            "-fno-test-coverage" | "-fno-profile-arcs" | "-fno-profile-generate" => {}
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
            _ if arg.strip_prefix("-fno-").is_some_and(|n| rucc_opt::pass::find(n).is_some()) => {
                opts.passes.push((arg["-fno-".len()..].to_owned(), false));
            }
            _ if arg.strip_prefix("-f").is_some_and(|n| rucc_opt::pass::find(n).is_some()) => {
                opts.passes.push((arg["-f".len()..].to_owned(), true));
            }
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
            // The word size, which is a statement about the target and is taken as one. A build
            // that says the size the target already has is saying nothing, and one that says the
            // other size is asking for a target this compiler does not have, which it is told
            // rather than being given the wrong one.
            "-m64" | "-m32" | "-mx32" => {
                let want: u32 = match arg {
                    "-m64" => 64,
                    _ => 32,
                };
                let have = rucc_target::TargetInfo::new(opts.target).pointer_width;
                if have != want {
                    return Err(err(format!(
                        "{arg} asks for a {want} bit target and {} is {have} bit, use \
                         --target= to name the one you mean",
                        opts.target
                    )));
                }
            }
            // Which processor in the family to generate for. This compiler emits the base
            // instruction set of the architecture and nothing above it, so a program built with
            // any of these runs on the machine that was named; it is a program that could have
            // been faster rather than a program that is wrong, which is what makes these safe to
            // take and ignore where a flag that changed the meaning of the code would not be.
            _ if arg.starts_with("-march=")
                || arg.starts_with("-mtune=")
                || arg.starts_with("-mcpu=") => {}
            // The calling convention, which is not safe to ignore. Taken when it names the one
            // the target already uses and refused otherwise.
            _ if arg.starts_with("-mabi=") => {
                let want = &arg["-mabi=".len()..];
                let have = match opts.target.arch {
                    rucc_target::Arch::X86_64 => "sysv",
                    rucc_target::Arch::Aarch64 => "lp64",
                    rucc_target::Arch::Riscv64 => "lp64d",
                };
                if want != have {
                    return Err(err(format!(
                        "{arg}: {} uses the {have} convention and this compiler has no other",
                        opts.target
                    )));
                }
            }
            // How far apart the pieces of the program may be. The small model is what we emit and
            // it is every hosted program's default; the kernel model is a different one and a
            // build that asks for it and does not get it links and then does not run.
            "-mcmodel=small" => {}
            _ if arg.starts_with("-mcmodel=") => {
                return Err(err(format!(
                    "{arg}: this compiler emits the small code model and no other, see \
                     spec/12-targets.md"
                )));
            }
            // GCC's own scripting language for how the driver builds a command line.
            // `spec/04-driver-and-cli.md` section 4.4 settles that we will not have it, so a
            // build reaching for it is told which flags do the same job.
            _ if arg.starts_with("-specs=") => {
                return Err(err(
                    "-specs= is not supported: the parts of it builds rely on are -B, -L, \
                     -nostdlib, -nostartfiles and -Wl,, see spec/04-driver-and-cli.md \
                     section 4.4",
                ));
            }
            // Arguments meant for a separate assembler or preprocessor, which this compiler does
            // not have: both are inside it and neither reads a command line. Refused rather than
            // dropped, because every one of these says something about the output and a build
            // that asked for `-Wa,--noexecstack` and was silently given an executable stack got
            // the opposite of what it asked for.
            _ if arg.starts_with("-Wa,") || arg.starts_with("-Wp,") => {
                return Err(err(format!(
                    "`{arg}` is an argument for a separate assembler or preprocessor, and both \
                     are inside this compiler rather than programs it runs"
                )));
            }
            "-Xassembler" | "-Xpreprocessor" => {
                return Err(err(format!(
                    "{arg} hands an argument to a separate assembler or preprocessor, and both \
                     are inside this compiler rather than programs it runs"
                )));
            }
            // Everything else in the `-W` family. `spec/04-driver-and-cli.md` section 4.1 has
            // this one as a rule about build systems rather than about warnings: autoconf finds
            // out whether a warning flag exists by passing it and looking at the exit status, so
            // a compiler that refuses one it has not heard of fails a configure script written
            // for a GCC newer than itself. The names are not checked against a list because this
            // compiler has no warning groups for a list to be of, which #485 is about.
            _ if arg.starts_with("-W") => {}
            // Flags that name something this compiler does not do and would not do differently
            // if it did. `-fno-ident` is about a comment in the output that we do not write
            // either way, and the others are about a way of ordering the compilation that has
            // been GCC's only way for twenty years. Section 4.1 asks for the list to be short
            // and for adding to it to be deliberate, which is why it is written out here.
            "-fno-ident"
            | "-fident"
            | "-funit-at-a-time"
            | "-fno-unit-at-a-time"
            | "-shared-libgcc"
            | "-static-libgcc" => {}
            _ if arg.starts_with('-') && arg.len() > 1 => {
                // Silently ignoring an unknown flag is how a build ends up not doing what
                // its author asked. spec/13-gnu-compat.md section 13.4 makes this an error
                // for the flags that change code generation, and the safe default until the
                // flag table is populated is to reject everything we do not know.
                return Err(err(format!("unknown option `{arg}`")));
            }
            _ => inputs.push(Input { path: arg.to_owned(), forced, library: false }),
        }
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
    link.sysroot = sysroot.clone();
    // Where a sysroot for a target that is not this machine would be. Read once, here, rather than
    // inside the link line, because a link line that read the environment could only be tested on a
    // machine whose environment said the right thing, and the link line is the last thing that
    // touches a binary. `spec/cross-compile/13-distribution.md` section 13.2 owns the answer.
    link.cache = Some(cache::dir());
    // And the ten field spelling of the target, because the release on it decides two things the
    // three field one cannot say: whether a target that is this architecture is still a cross
    // compile, and which directory under the cache it is against. After the loop because the last
    // `--target=` on the command line is the one that counts.
    link.pinned = pinned;
    // After the loop rather than where `-pthread` was read, so that it lands after the objects
    // that refer to it. A static link takes the definitions it needs from a library when it
    // reaches it and not afterwards, so a library before the objects is a library that answers
    // nothing.
    if threads {
        inputs.push(Input::library("pthread"));
    }
    if let Some(query) = query {
        return Ok(Action::Print(answer(&query, &opts, &link)?));
    }
    // `-M` and `-MM` produce the rule and nothing else, so the run stops after phase 4 whatever
    // else the command line asked for. Read here rather than where the flag was, because a `-c`
    // written after it has to lose and the loop cannot know that until it has ended. The output
    // file is where the rule goes rather than where an object would have gone, and the last
    // phase being the preprocessor is what makes that true without a second rule for it.
    if opts.deps.instead_of_compiling {
        opts.emit = EmitKind::Preprocessed;
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
        for dir in
            library::header_dirs(opts.target, sysroot.as_deref(), cross.as_ref(), kernel.as_ref())
        {
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
    let plan = Plan::new(&opts, &inputs, output.as_deref()).map_err(|e| err(e.message))?;
    if print_plan {
        return Ok(Action::PrintPlan {
            opts: Box::new(opts),
            plan: Box::new(plan),
            link: Box::new(link),
        });
    }
    Ok(Action::Compile {
        opts: Box::new(opts),
        plan: Box::new(plan),
        link: Box::new(link),
        jobs,
        verbose,
    })
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
        Query::Version => VERSION.to_owned(),
        Query::Multiarch => link::multiarch(opts.target),
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
        Query::SysrootProvenance => {
            let Some(root) = sysroot_root(opts, link) else {
                return Ok(String::new());
            };
            let path = Sysroot::at(root, opts.target.tuple()).manifest_path();
            match std::fs::read_to_string(&path) {
                // Read and rendered rather than copied out, so that what comes back is the format
                // this build understands. A file this build cannot read is a file whose lines it
                // cannot vouch for, and printing it anyway would pass the problem to whoever parses
                // the output next.
                Ok(text) => Manifest::parse(&text)
                    .map_err(|why| err(format!("{}: {why}", path.display())))?
                    .render(),
                // A tree with no manifest in it is a tree somebody laid out themselves and pointed
                // `--sysroot` at, and nothing here knows where any of it came from. The answer is
                // nothing, which a reader can tell apart from a manifest with no inputs in it
                // because that one still has its header line.
                Err(why) if why.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(why) => return Err(err(format!("{}: {why}", path.display()))),
            }
        }
        Query::FileName(name) => found(name),
        // The name GCC gives the library of routines a compiler's output calls that the C
        // library does not have. Ours is built in and there is no file, so the answer is the
        // name itself, which is what GCC prints when it cannot find one either.
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

/// The root both of the sysroot answers are about.
///
/// One function rather than a copy in each, because the second flag exists to say what is inside the
/// tree the first one names, and two answers that disagreed about which tree that is would be a
/// difference nobody would think to look for. `--sysroot` wins over ours because it wins everywhere
/// else.
fn sysroot_root(opts: &Options, link: &LinkOptions) -> Option<PathBuf> {
    link.sysroot
        .clone()
        .or_else(|| link::cross_sysroot(opts.target, link).map(|at| at.root().to_path_buf()))
}

/// Renders the passes this level will run, in order, with what each one does.
///
/// The level is the whole of the answer unless a `-f` flag edited it, which is section 9.1 of
/// `spec/09-optimizer.md`: a level is a list somebody wrote down rather than something that
/// emerges from which flags happen to be set, and this is how that list is read.
#[must_use]
pub fn print_pipeline(opts: &Options) -> String {
    let mut settings = rucc_opt::Options::for_level(opts.opt_level);
    settings.toggles.clone_from(&opts.passes);
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
    let _ = writeln!(out, "opt-level: {}", sess.opts.opt_level);
    let _ = writeln!(out, "safety: {}", sess.opts.safety);
    let _ = writeln!(out, "emit: {}", sess.opts.emit.as_str());
    let _ = writeln!(out, "debug-info: {}", sess.opts.debug_info);
    let _ = writeln!(out, "frame-pointer: {}", sess.opts.frame_pointer);
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
    let mut stderr = std::io::stderr().lock();
    let mut failed = false;
    for job in &plan.jobs {
        if !job.phases.first().is_some_and(|p| *p == Phase::Preprocess) {
            // An input that is already preprocessed, or an object file. GCC passes these
            // through untouched, and the plan has already said so in its notes.
            continue;
        }
        let started = std::time::Instant::now();
        let result = preprocess(opts, &job.input, &fs);
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

/// Runs the front end over every input that has a compile phase, and writes what came out.
///
/// The same rule as [`preprocess_all`]: one input that fails does not stop the others, and the
/// exit status is a failure either way. An input that is already assembly or an object has no
/// compile phase and is passed over here, which the plan has already said in its notes.
fn compile_all(opts: &Options, plan: &Plan) -> i32 {
    let fs = OsFileSystem::new();
    let mut stderr = std::io::stderr().lock();
    let mut failed = false;
    let (mut remarks, ok) = Remarks::new(opts.opt_info_file.as_ref(), &mut stderr);
    failed |= !ok;
    let mut fired = Fired::new();
    let mut pressure = Pressure::new();
    for job in &plan.jobs {
        if !job.phases.contains(&Phase::Compile) {
            continue;
        }
        // An input of IR is read back rather than compiled, since the C it came from is not
        // here any more. Everything after this is the same, so the two paths meet again at the
        // messages and the file the result is written to.
        let started = std::time::Instant::now();
        let result = if job.kind == InputKind::Ir {
            compile_ir(opts, &job.input, &fs)
        } else {
            compile(opts, &job.input, &fs)
        };
        if opts.time {
            say_time(&job.input, started.elapsed(), &mut stderr);
        }
        fired.merge(&result.fired);
        pressure.merge(&result.pressure);
        failed |= !write_dumps(&job.input, &result.dumps, &mut stderr);
        failed |= !remarks.write(&result.remarks, &mut stderr);
        for message in &result.messages {
            let _ = writeln!(stderr, "{message}");
        }
        // Before the failure below, because a compilation that stopped in the back end is exactly
        // the one whose preprocessed source somebody wants to look at.
        failed |= !write_temps(job, &result.temps, &mut stderr);
        if result.failed() {
            failed = true;
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
        let dir = std::env::temp_dir().join(format!("rucc-{}", std::process::id()));
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
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "rucc: error: there is nothing to link");
        return 1;
    };
    // Before anything is compiled, because a linker that is not on the machine is worth knowing
    // about in the second it takes to look rather than after the compilation.
    // And before that, whether this link has a line at all and whether what it reads is on the
    // machine. Both are answerable now, and a target whose sysroot has not been built is worth
    // saying so about before the compilation rather than after it.
    if let Err(why) = link::preflight(opts.target, link) {
        return complain(why);
    }
    let linker = match link::find(opts.target, link) {
        Ok(linker) => linker,
        Err(why) => return complain(why),
    };

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
    {
        let mut stderr = std::io::stderr().lock();
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
            if !job.phases.contains(&Phase::Compile) {
                continue;
            }
            let started = std::time::Instant::now();
            let result = if job.kind == InputKind::Ir {
                compile_ir(opts, &job.input, &fs)
            } else {
                compile(opts, &job.input, &fs)
            };
            if opts.time {
                say_time(&job.input, started.elapsed(), &mut stderr);
            }
            fired.merge(&result.fired);
            pressure.merge(&result.pressure);
            failed |= !write_dumps(&job.input, &result.dumps, &mut stderr);
            failed |= !remarks.write(&result.remarks, &mut stderr);
            for message in &result.messages {
                let _ = writeln!(stderr, "{message}");
            }
            failed |= !write_temps(job, &result.temps, &mut stderr);
            if result.failed() {
                failed = true;
                continue;
            }
            // A `-MD` on a command line that links writes the rule next to the executable and
            // names the executable as its target, since that is the file this source builds
            // here. The object it went through is in a temporary directory and is gone by the
            // time `make` reads any of this.
            if opts.deps.emit {
                failed |= !write_deps(opts, plan, job, &result.deps, &mut stderr);
            }
            if !matches!(result.artifact, Artifact::Object(_)) {
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
    }
    if failed {
        // Nothing is linked from a compilation that did not finish. A linker run over the objects
        // that did compile would report every function of the file that did not as undefined,
        // which is a page of messages about a mistake already reported once.
        return 1;
    }

    // The items in command line order with the temporaries filled in. A library contributes no
    // job and passes through, and every file item takes the next job's real output, which is
    // what keeps a library that was written between two objects between them here.
    let mut outputs = produced.into_iter();
    let mut items = Vec::with_capacity(job.inputs.len());
    for item in &job.inputs {
        match item {
            link::Item::Library(name) => items.push(link::Item::Library(name.clone())),
            link::Item::File(_) => match outputs.next() {
                Some(path) => items.push(link::Item::File(path)),
                None => return complain("the plan asks the linker for a file nothing produced"),
            },
        }
    }

    let args = match link::line(opts.target, link, &items, &job.output) {
        Ok(args) => args,
        Err(why) => return complain(why),
    };
    if verbose {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{}", link::render(&linker, &args));
    }
    let started = std::time::Instant::now();
    let ran = link::run(&linker, &args);
    if opts.time {
        // The one step of a compilation that really is another program, so this line is the same
        // measurement gcc's is and names the linker the way gcc names `collect2`.
        let mut stderr = std::io::stderr().lock();
        say_time(&linker.name, started.elapsed(), &mut stderr);
    }
    match ran {
        Ok(()) => 0,
        // The linker has already said what was wrong on its own error output, and repeating that
        // linking failed would only push its message further up the screen.
        Err(link::Error::Refused { .. }) => 1,
        Err(why) => complain(why),
    }
}

/// Prints one driver level message and gives back the exit status that goes with it.
fn complain(why: impl std::fmt::Display) -> i32 {
    let mut stderr = std::io::stderr().lock();
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
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(bytes).map_err(|e| format!("writing to standard output: {e}"))
        }
        Output::File(path) | Output::Temporary(path) => {
            std::fs::write(path, bytes).map_err(|e| format!("{path}: {e}"))
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
            print!("{USAGE}");
            0
        }
        Ok(Action::Version) => {
            println!("rucc {VERSION}");
            0
        }
        Ok(Action::Print(line)) => {
            println!("{line}");
            0
        }
        Ok(Action::PrintConfig(opts)) => {
            print!("{}", print_config(&opts));
            0
        }
        Ok(Action::PrintPipeline(opts)) => {
            print!("{}", print_pipeline(&opts));
            0
        }
        Ok(Action::PrintPlan { opts, plan, link }) => {
            print!("{}", plan.render());
            // The line as it would be typed, which is the half of `-###` that section 4.3 says
            // arrives with the link. It is printed even when the linker is not on this machine,
            // because what a build wants from `-###` is what the compiler would do.
            if let Some(job) = &plan.link {
                match link_line(&opts, &link, job) {
                    Ok(line) => println!("{line}"),
                    Err(why) => {
                        let mut stderr = std::io::stderr().lock();
                        let _ = writeln!(stderr, "rucc: error: {why}");
                        return 1;
                    }
                }
            }
            0
        }
        Ok(Action::Compile { opts, plan, link, jobs, verbose }) => {
            {
                let mut stderr = std::io::stderr().lock();
                if verbose {
                    let _ = write!(stderr, "{}", plan.render());
                    let _ = writeln!(stderr, "workers: {}", jobs.count());
                }
            }
            if opts.emit == EmitKind::Preprocessed {
                return preprocess_all(&opts, &plan);
            }
            if opts.emit != EmitKind::Executable {
                return compile_all(&opts, &plan);
            }
            link_all(&opts, &plan, &link, verbose)
        }
        Err(e) => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "rucc: error: {e}");
            let _ = writeln!(stderr, "rucc: note: run `rucc --help` for usage");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_session::{
        Contract, GnucVersion, IncludeForm, LtoJobs, OptLevel, Partition, Patchable, Visibility,
    };

    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        assert_eq!(parse_args(&args(&["-c", "--help", "x.c"])).unwrap(), Action::Help);
        assert_eq!(parse_args(&args(&["--version"])).unwrap(), Action::Version);
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
    fn asking_for_nested_functions_is_told_why_it_is_not_coming() {
        let e = parse_args(&args(&["-fnested-functions", "a.c"])).unwrap_err();
        assert!(e.message.contains("trampoline"), "{}", e.message);
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
            "-fno-common",
            "-fstrict-aliasing",
            "-fno-strict-aliasing",
            "-fdelete-null-pointer-checks",
            "-fno-delete-null-pointer-checks",
            "-frounding-math",
            "-fno-rounding-math",
            "-ftrapping-math",
            "-fno-trapping-math",
            "-fexcess-precision=standard",
            "-fexcess-precision=fast",
            "-fexcess-precision=16",
            "-pipe",
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
    fn asking_the_linker_to_merge_tentative_definitions_is_told_why_it_is_not_coming() {
        // The one of that family that is a request rather than a description, and it is a real
        // difference: two files each writing `int g;` link under it and do not without it.
        let e = parse_args(&args(&["-fcommon", "a.c"])).unwrap_err();
        assert!(e.message.contains(".bss"), "{}", e.message);
        assert!(e.message.contains("extern"), "the way out is worth saying: {}", e.message);
    }

    #[test]
    fn asking_for_position_dependent_code_is_told_why_it_is_not_coming() {
        for flag in ["-fno-pic", "-fno-pie"] {
            let e = parse_args(&args(&[flag, "a.c"])).unwrap_err();
            assert!(e.message.contains("global offset table"), "{flag}: {}", e.message);
            // The one it may have meant, since the two are a letter apart and one of them is
            // about linking and is taken.
            assert!(e.message.contains("-no-pie"), "{flag}: {}", e.message);
        }
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
    }

    #[test]
    fn print_config_has_one_key_per_line_and_a_fixed_order() {
        let opts = Options::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let text = print_config(&opts);
        let keys: Vec<&str> =
            text.lines().map(|l| l.split(':').next().unwrap_or_default()).collect();
        assert_eq!(keys[0], "version");
        assert_eq!(keys[1], "target");
        assert_eq!(keys.len(), 25);
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
    fn print_pipeline_answers_with_the_passes_the_level_asked_for() {
        let a = parse_args(&args(&["--print-pipeline", "-O2"])).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        let text = print_pipeline(&opts);
        assert!(text.starts_with("level: -O2\n"), "{text}");
        assert!(text.contains("fold"), "{text}");

        let a = parse_args(&args(&["--print-pipeline"])).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        // One pass runs at `-O0` and it is the one that removes code nothing reaches, which is
        // not an optimization. See issue 359.
        assert!(print_pipeline(&opts).contains("1: simplify-cfg,"), "{}", print_pipeline(&opts));

        let a = parse_args(&args(&["--print-pipeline", "-fno-simplify-cfg"])).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        // And with that one turned off there is nothing left, which the dump says rather than
        // printing an empty list.
        assert!(print_pipeline(&opts).contains("no passes"), "{}", print_pipeline(&opts));
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
        // is about, which is that the toggles can empty a level.
        let mut off = vec!["--print-pipeline".to_owned(), "-O2".to_owned()];
        off.extend(rucc_opt::PASSES.iter().map(|p| format!("-fno-{}", p.name())));
        let spelled: Vec<&str> = off.iter().map(String::as_str).collect();
        let a = parse_args(&args(&spelled)).unwrap();
        let Action::PrintPipeline(opts) = a else { panic!("expected a pipeline dump") };
        assert!(print_pipeline(&opts).contains("no passes"), "{}", print_pipeline(&opts));
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
    fn the_include_flags_land_on_the_chain_each_one_names() {
        // A sysroot with nothing under it, so that the library's own directories are the
        // same on every machine this test runs on, which is none of them.
        let (opts, _) = compile(&[
            "-Ii",
            "-iquote",
            "q",
            "-isystem",
            "sys",
            "-idirafter",
            "after",
            "--sysroot=/nowhere-at-all",
            "a.c",
        ]);
        let dirs: Vec<&str> = opts.search.dirs().iter().filter_map(|d| d.path.to_str()).collect();
        // The compiler's own headers sit after every `-isystem` and before `-idirafter`,
        // which is where GCC puts its own: a directory the user named outranks ours.
        assert_eq!(dirs, ["q", "i", "sys", runtime::DIR, "after"]);
        assert!(!opts.search.dirs()[1].is_system);
        assert!(opts.search.dirs()[2].is_system);
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
        // all, so the list is the libc's two and the question never arises, which is the `None` that
        // `link::cross_kernel` returns rather than a directory nothing would be found in.
        let (opts, _) = compile(&["--target=x86_64-pc-windows-gnu", "-c", "a.c"]);
        let dirs: Vec<&std::path::Path> =
            opts.search.dirs().iter().map(|d| d.path.as_path()).collect();
        assert_eq!(dirs.len(), 3, "{dirs:?}");
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
        let gnu = format!("--target={}-linux-gnu", cross_arch());
        let (bundled, _) = compile(&[&gnu, "-c", "a.c"]);
        assert_eq!(bundled.glibc_minor, Some(44));
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
        if host.env != rucc_target::Env::Gnu {
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
        assert_eq!(printed(&["-dumpversion", "a.c"]), VERSION);
    }

    #[test]
    fn the_gcc_version_claimed_is_a_flag_and_the_short_spellings_are_the_ones_people_write() {
        let (opts, _) = compile(&["a.c"]);
        assert_eq!(
            opts.gnuc,
            GnucVersion { major: 7, minor: 0, patch: 0 },
            "the lowest claim a modern glibc gives its own declarations to"
        );

        let (opts, _) = compile(&["-fgnuc-version=15.1.0", "a.c"]);
        assert_eq!(opts.gnuc, GnucVersion { major: 15, minor: 1, patch: 0 });

        // A missing component is zero. `gcc -dumpversion` says `15` on a release with no
        // patchlevel and a harness that pastes that back has to be understood.
        let (opts, _) = compile(&["-fgnuc-version=15", "a.c"]);
        assert_eq!(opts.gnuc, GnucVersion { major: 15, minor: 0, patch: 0 });

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
        assert!(!opts.frame_pointer, "gcc omits it above -O0 and so does this");
        assert!(opts.red_zone, "the psABI has one and nothing said not to use it");

        let (opts, _) = compile(&["-c", "-fno-omit-frame-pointer", "-mno-red-zone", "a.c"]);
        assert!(opts.frame_pointer);
        assert!(!opts.red_zone);

        let (opts, _) = compile(&[
            "-c",
            "-fno-omit-frame-pointer",
            "-fomit-frame-pointer",
            "-mno-red-zone",
            "-mred-zone",
            "a.c",
        ]);
        assert!(!opts.frame_pointer, "the last one wins, as it does in gcc");
        assert!(opts.red_zone);
    }

    /// Four flags rather than one with an argument, which is how gcc spells them, and the negative
    /// spelled three ways because a build that turns one off writes whichever it turned on.
    #[test]
    fn the_stack_protector_is_four_flags_and_the_last_one_wins() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.protector, Protector::None, "gcc protects nothing unless it was asked");

        for (flag, want) in [
            ("-fstack-protector", Protector::Buffers),
            ("-fstack-protector-strong", Protector::Strong),
            ("-fstack-protector-all", Protector::All),
        ] {
            let (opts, _) = compile(&["-c", flag, "a.c"]);
            assert_eq!(opts.protector, want, "{flag}");
        }

        // What a package build does: the strong one in the global flags and one directory that
        // cannot have a protector turning it off on the line after.
        for off in ["-fno-stack-protector", "-fno-stack-protector-strong"] {
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
        assert!(link.export_dynamic);
        assert!(link.strip);
        assert_eq!(link.use_ld.as_deref(), Some("mold"));
        assert_eq!(link.search, vec![PathBuf::from("/opt/lib")]);
        assert_eq!(link.prefixes, vec![PathBuf::from("/opt/tools")]);
    }

    #[test]
    fn a_comma_in_dash_wl_separates_two_arguments() {
        let (link, _) = linking(&["-Wl,-rpath,/opt/lib", "-Xlinker", "--as-needed", "a.c"]);
        assert_eq!(link.passthrough, vec!["-rpath", "/opt/lib", "--as-needed"]);
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

    #[test]
    fn a_warning_flag_this_compiler_has_not_heard_of_is_taken_rather_than_refused() {
        // The rule in section 4.1, and the reason for it is autoconf: a configure script finds
        // out whether a warning flag exists by passing it and looking at the exit status, so a
        // compiler that refuses one it does not know fails a script written for a newer GCC.
        let (opts, _) = compile(&["-Wall", "-Wextra", "-Wno-format-truncation", "-c", "a.c"]);
        assert!(!opts.warnings_are_errors);
        assert!(opts.warnings);
        // The two spellings that do mean something are still read.
        let (opts, _) = compile(&["-Werror", "-c", "a.c"]);
        assert!(opts.warnings_are_errors);
        let (opts, _) = compile(&["-w", "-c", "a.c"]);
        assert!(!opts.warnings);
        let (opts, _) = compile(&["-pedantic-errors", "-c", "a.c"]);
        assert!(opts.pedantic && opts.warnings_are_errors);
    }

    #[test]
    fn an_argument_for_a_separate_tool_is_refused_rather_than_dropped() {
        // Every one of these says something about the output, so the wrong answer is silence.
        assert!(refused(&["-Wa,--noexecstack", "-c", "a.c"]).contains("separate assembler"));
        assert!(refused(&["-Wp,-DX", "-c", "a.c"]).contains("separate assembler"));
        assert!(refused(&["-specs=/x", "a.c"]).contains("-specs= is not supported"));
        assert!(refused(&["-mcmodel=kernel", "-c", "a.c"]).contains("small code model"));
        assert!(refused(&["-gdwarf-4", "-c", "a.c"]).contains("DWARF 5"));
        assert!(refused(&["-Ofast", "-c", "a.c"]).contains("fast math"));
        // The word size the target does not have, which is a target this compiler was not asked
        // for rather than a flag it does not know.
        let no32 = refused(&["--target=x86_64-unknown-linux-gnu", "-m32", "-c", "a.c"]);
        assert!(no32.contains("32 bit target"), "{no32}");
    }

    /// `-gz` and the two spellings of the split, which are the two questions about the shape of
    /// the debug output rather than about how much of it there is.
    ///
    /// Both answers here are about what happens when there is debug information to shape, and
    /// there is none yet, so what is being asserted is that the flags are read and remembered
    /// rather than that anything changed in the output. That is the whole of what taking them
    /// claims, and it is worth a test because the day `rucc-debug` writes a section this is where
    /// it comes to find out what the command line said.
    #[test]
    fn the_shape_of_the_debug_output_is_recorded_even_where_there_is_none_of_it() {
        let (opts, _) = compile(&["-c", "a.c"]);
        assert_eq!(opts.compress, Compress::None, "uncompressed unless somebody asks");

        // Bare `-gz` is `-gz=zlib`, measured against gcc 16 rather than read out of the manual,
        // which describes the flag without ever saying which algorithm it picks.
        assert_eq!(compile(&["-gz", "-c", "a.c"]).0.compress, Compress::Zlib);
        for (spelling, want) in [
            ("none", Compress::None),
            ("zlib", Compress::Zlib),
            ("zlib-gnu", Compress::ZlibGnu),
            ("zstd", Compress::Zstd),
        ] {
            let (opts, _) = compile(&[&format!("-gz={spelling}"), "-c", "a.c"]);
            assert_eq!(opts.compress, want, "{spelling}");
        }

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

        // Writing one, which is refused by name. The first four instrument the program and the
        // last writes a file beside the object, and a build that got neither and no message would
        // go on to optimize against counts that were never gathered.
        for writing in [
            "-fprofile-generate",
            "-fprofile-generate=/build/profiles",
            "-fprofile-arcs",
            "--coverage",
            "-fcondition-coverage",
            "-fpath-coverage",
        ] {
            let failed = refused(&[writing, "-c", "a.c"]);
            assert!(failed.contains("instrument"), "{writing}: {failed}");
        }
        assert!(refused(&["-ftest-coverage", "-c", "a.c"]).contains(".gcno"), "it names the file");

        // The negative spellings of the refused half are what already happens, so they are taken.
        for taken in ["-fno-profile-generate", "-fno-profile-arcs", "-fno-test-coverage"] {
            let (opts, _) = compile(&[taken, "-c", "a.c"]);
            assert!(!opts.profile_data.requested, "{taken} asks for nothing");
        }

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
    fn the_questions_a_build_system_asks_before_it_compiles_anything() {
        let target = "--target=x86_64-unknown-linux-gnu";
        assert_eq!(printed(&[target, "-dumpmachine"]), "x86_64-unknown-linux-gnu");
        assert_eq!(printed(&[target, "-dumpversion"]), VERSION);
        assert_eq!(printed(&[target, "-dumpfullversion"]), VERSION);
        assert_eq!(printed(&[target, "-print-multiarch"]), "x86_64-linux-gnu");
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
        let host = Triple::host().expect("a host this compiler knows");
        assert_eq!(printed(&[&format!("--target={host}"), "-print-sysroot"]), "");
    }

    #[test]
    fn the_provenance_of_a_sysroot_is_the_manifest_it_carries() {
        // Section 13.5 wants seven things per input and wants them machine readable, and the manifest
        // is the record that already has them, so the flag prints that rather than a second format.
        let manifest = "rucc sysroot manifest 2\n\
                        target\tx86_64-linux-musl\n\
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
        assert_eq!(printed(&[&sysroot, "-print-sysroot-provenance"]), manifest);

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
        assert_eq!(printed(&[&sysroot, "--print-sysroot-provenance"]), manifest);
    }

    #[test]
    fn a_manifest_this_build_cannot_read_is_refused_rather_than_printed() {
        // Passing a file we could not parse to whoever asked would make their parser the one that
        // finds the problem, and the three uses section 13.5 gives for this are all somebody else
        // parsing it.
        let tree = TempTree::new(
            "provenance-bad",
            &[("manifest", "rucc sysroot manifest 2\ntarget\tx86_64-linux-musl\nlib/libc.a\n")],
        );
        let message =
            refused(&[&format!("--sysroot={}", tree.0.display()), "-print-sysroot-provenance"]);
        assert!(message.contains("manifest"), "{message}");
        assert!(message.contains("1 fields where an input has six"), "{message}");
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
        // which belong beside the tier that is the nearest thing here to what they asked for.
        assert!(USAGE.lines().count() < 69, "usage text has grown past one screen");
    }
}
