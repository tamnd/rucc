//! The driver: command line parsing, the phase graph, job scheduling and the linker
//! invocation.
//!
//! Design: `spec/04-driver-and-cli.md`. Layer rank 12, see `spec/18-package-layout.md`.
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
//! `-I`, `-iquote`, `-isystem`, `-idirafter`, `--sysroot=`, `-isysroot`, `-P`, `-std=`,
//! `-fgnuc-version=`, `-ansi`, `-ffreestanding`, `-fno-builtin`, `-fno-builtin-<name>`,
//! `-pedantic` and `-Werror`.
//! The phases after them still say they are not implemented.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-driver/0.5.1")]

pub mod compile;
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
use rucc_session::{Dumps, EmitKind, Options, Session, Std, runtime};
use rucc_target::Triple;

use crate::link::LinkOptions;

pub use crate::compile::{Artifact, Compiled, compile, compile_ir};
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
  --sysroot=<dir>        look for the library's headers under <dir>, -isysroot too
  -P, -dM                with -E: leave out the markers, or dump the macros
  -std=<dialect>         c89 through c23, and the gnu spellings
  -fgnuc-version=<v>     the GCC release to claim, default 7.0.0
  -x <lang>              treat later inputs as <lang>, or none to stop
  -O<level>              optimize: 0, 1, 2, 3, s, z
  -fsafety=<tier>        check memory safety: off, detect, enforce, kernel
  -f<pass> -fno-<pass> -fdump-ir=<what> -fopt-info[-<kind>][=FILE]
  -fpass-fuel=<pass>=<n>, -fpass-fuel-global=<n>   stop a pass, or all of them, after n
  -fdisable-<pass>[=<funcs>], -fenable-<pass>[=<funcs>]   run a pass on some functions only
  -g, -fno-omit-frame-pointer, -mno-red-zone   debug info, keep a frame pointer, no red zone
  -l<name>, -L <dir>, -B <dir>   link a library, where to look for one, where our own tools are
  -static -shared -pie -no-pie -nostdlib -nostartfiles -nodefaultlibs -rdynamic -s   how to link
  -Wl,<arg>, -Xlinker <arg>, -fuse-ld=<name>   hand an argument to the linker, or pick one
  -Werror -pedantic      warnings are errors, diagnose what the standard forbids
  -j[n]                  compile n translation units at once, default all
  -v, -###               print each phase as it runs, or without running any
  --target=<triple>      generate code for <triple>
  --emit=<kind>          exe, obj, asm, preprocessed, tast, ir, mir-final
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
    let mut output = None;
    let mut link = LinkOptions::default();
    // `-x` applies to inputs that come after it and stays in effect until the next one, which
    // is why it is tracked across the loop rather than attached to a single argument.
    let mut forced: Option<InputKind> = None;

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
            "-c" => opts.emit = EmitKind::Object,
            "-S" => opts.emit = EmitKind::Asm,
            "-E" => opts.emit = EmitKind::Preprocessed,
            "-g" => opts.debug_info = true,
            "-Werror" => opts.warnings_are_errors = true,
            "-P" => opts.line_markers = false,
            "-ansi" => {
                opts.std = Std::C89;
                opts.gnu_extensions = false;
            }
            // `-Wpedantic` is the same flag under the name the `-W` family gives it, which is
            // the spelling a build system that groups its warning flags tends to write.
            "-pedantic" | "-Wpedantic" => opts.pedantic = true,
            "-ffreestanding" => opts.hosted = false,
            "-fhosted" => opts.hosted = true,
            "-fno-builtin" => opts.builtins = false,
            "-fbuiltin" => opts.builtins = true,
            // Both directions of each, because a build system that wants one of these usually
            // writes it beside the flag that turns it back off for one directory.
            "-fno-omit-frame-pointer" => opts.frame_pointer = true,
            "-fomit-frame-pointer" => opts.frame_pointer = false,
            "-mno-red-zone" => opts.red_zone = false,
            "-mred-zone" => opts.red_zone = true,
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
            }
            _ if arg.starts_with("--emit=") => {
                let k = &arg["--emit=".len()..];
                opts.emit = k
                    .parse()
                    .map_err(|()| err(format!("unknown --emit kind `{k}`, see --help")))?;
            }
            _ if arg.starts_with("-O") => {
                opts.opt_level = arg[2..]
                    .parse()
                    .map_err(|()| err(format!("unknown optimization level `{arg}`")))?;
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
            _ if arg.starts_with("-Z") => {
                return Err(err(format!(
                    "`{arg}` is not an unstable option this compiler has, see \
                     spec/04-driver-and-cli.md section 4.11 for the ones it does"
                )));
            }
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
    link.sysroot = sysroot.clone();
    if !nostdinc {
        opts.search.push_system(runtime::DIR);
        // And the library's after ours, which is the other half of the same order. They go on
        // here rather than at the point `--target=` or `--sysroot=` was read because either
        // one changes the answer and the last word on both is the end of the loop.
        for dir in library::system_dirs(opts.target, sysroot.as_deref()) {
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
    let _ = writeln!(out, "target: {}", t.triple);
    let _ = writeln!(out, "arch: {}", t.triple.arch.as_str());
    let _ = writeln!(out, "os: {}", t.triple.os.as_str());
    let _ = writeln!(out, "env: {}", t.triple.env.as_str());
    let _ = writeln!(out, "object-format: {}", t.object_format.as_str());
    let _ = writeln!(out, "pointer-width: {}", t.pointer_width);
    let _ = writeln!(out, "long-width: {}", t.long_width);
    let _ = writeln!(out, "long-double-width: {}", t.long_double_width);
    let _ = writeln!(out, "endian: {}", if t.little_endian { "little" } else { "big" });
    let _ = writeln!(out, "char-signed: {}", t.char_is_signed);
    let _ = writeln!(out, "va-list: {}", t.va_list.as_str());
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
    // Last because it is the one key with more than one line under it, and the only one
    // whose value is a property of the machine rather than of the command line.
    for dir in sess.opts.search.dirs() {
        let system = if dir.is_system { " (system)" } else { "" };
        let _ = writeln!(out, "include: {}{system}", dir.path.display());
    }
    out
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
        let result = preprocess(opts, &job.input, &fs);
        for message in &result.messages {
            let _ = writeln!(stderr, "{message}");
        }
        if result.failed() {
            failed = true;
            continue;
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
    for job in &plan.jobs {
        if !job.phases.contains(&Phase::Compile) {
            continue;
        }
        // An input of IR is read back rather than compiled, since the C it came from is not
        // here any more. Everything after this is the same, so the two paths meet again at the
        // messages and the file the result is written to.
        let result = if job.kind == InputKind::Ir {
            compile_ir(opts, &job.input, &fs)
        } else {
            compile(opts, &job.input, &fs)
        };
        fired.merge(&result.fired);
        failed |= !write_dumps(&job.input, &result.dumps, &mut stderr);
        failed |= !remarks.write(&result.remarks, &mut stderr);
        for message in &result.messages {
            let _ = writeln!(stderr, "{message}");
        }
        if result.failed() {
            failed = true;
            continue;
        }
        if let Err(e) = write_out(&job.output, result.artifact.bytes()) {
            let _ = writeln!(stderr, "rucc: error: {e}");
            failed = true;
        }
    }
    failed |= !write_coverage(opts, &fired, &mut stderr);
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
            let result = if job.kind == InputKind::Ir {
                compile_ir(opts, &job.input, &fs)
            } else {
                compile(opts, &job.input, &fs)
            };
            fired.merge(&result.fired);
            failed |= !write_dumps(&job.input, &result.dumps, &mut stderr);
            failed |= !remarks.write(&result.remarks, &mut stderr);
            for message in &result.messages {
                let _ = writeln!(stderr, "{message}");
            }
            if result.failed() {
                failed = true;
                continue;
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
    match link::run(&linker, &args) {
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
    use rucc_session::{GnucVersion, OptLevel};

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
    fn dash_x_names_what_it_accepts_when_it_does_not_know_a_language() {
        let e = parse_args(&args(&["-x", "fortran", "a.c"])).unwrap_err();
        assert!(e.message.contains("assembler-with-cpp"), "{}", e.message);
    }

    #[test]
    fn an_unknown_flag_is_an_error_rather_than_a_shrug() {
        let e = parse_args(&args(&["-fno-such-thing", "a.c"])).unwrap_err();
        assert!(e.message.contains("unknown option"), "{}", e.message);
    }

    #[test]
    fn asking_for_nested_functions_is_told_why_it_is_not_coming() {
        let e = parse_args(&args(&["-fnested-functions", "a.c"])).unwrap_err();
        assert!(e.message.contains("trampoline"), "{}", e.message);
        assert!(parse_args(&args(&["-fno-nested-functions", "a.c"])).is_ok());
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
        assert_eq!(keys.len(), 19);
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

        // `-dumpversion` is a different flag that happens to start the same way. We have not
        // written it, and saying so beats reading it as a dump of nothing.
        let e = parse_args(&args(&["-dumpversion", "a.c"])).unwrap_err();
        assert!(e.message.contains("unknown option"), "{}", e.message);
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

    #[test]
    fn usage_fits_on_a_screen() {
        // Not a style preference. A help text that scrolls is one nobody reads, and this is
        // the cheapest way to keep it honest as flags accumulate. The number goes up only when
        // a family of flags arrives that has nowhere to share a line, which the two pass gates
        // were and which the two fuel flags and `-fsafety=` now are, and it goes up by exactly
        // the lines that family took.
        assert!(USAGE.lines().count() < 37, "usage text has grown past one screen");
    }
}
