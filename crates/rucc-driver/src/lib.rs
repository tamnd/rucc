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
//! One phase runs: `-E` reads the file, runs phase 4 over it and writes the result, to `-o`
//! or to standard output. The flags that phase reads are real with it, which is `-D`, `-U`,
//! `-I`, `-iquote`, `-isystem`, `-idirafter`, `-P`, `-std=`, `-fgnuc-version=`, `-ansi` and
//! `-ffreestanding`.
//! The phases after it still say they are not implemented.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-driver/0.2.8")]

mod map;
pub mod phase;
pub mod preprocess;
pub mod schedule;

use std::fmt::Write as _;
use std::io::Write as _;

use rucc_session::{Dumps, EmitKind, Options, Session, Std};
use rucc_target::Triple;

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
    /// Print the phase plan and exit successfully, which is what `-###` asks for.
    PrintPlan(Box<Plan>),
    /// Compile the given inputs.
    Compile {
        /// The resolved options.
        opts: Box<Options>,
        /// What to do to each input, and in what order.
        plan: Box<Plan>,
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
  -o <file>              write output to <file>
  -D <name>[=<value>]    define a macro, value 1 if none is given
  -U <name>              undefine a macro, after every -D
  -I <dir>               add <dir> to the include search path
  -iquote -isystem -idirafter <dir>   the other search chains
  -P, -dM                with -E: leave out the markers, or dump the macros
  -std=<dialect>         c89 through c23, and the gnu spellings
  -fgnuc-version=<v>     the GCC release to claim, default 4.2.1
  -x <lang>              treat later inputs as <lang>, or none to stop
  -O<level>              optimize: 0, 1, 2, 3, s, z
  -g                     emit debug information
  -Werror                treat warnings as errors
  -j[n]                  compile n translation units at once, default all
  -v, -###               print each phase as it runs, or without running any
  --target=<triple>      generate code for <triple>
  --emit=<kind>          exe, obj, asm, preprocessed, tast, ir, mir-final
  --print-config         print the resolved configuration and exit
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
    let mut print_plan = false;
    let mut verbose = false;
    let mut jobs = Jobs::default();
    let mut output = None;
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
            "-ffreestanding" => opts.hosted = false,
            "-fhosted" => opts.hosted = true,
            "-o" => {
                output = Some(args.get(i).ok_or_else(|| err("-o requires an argument"))?.clone());
                i += 1;
            }
            // The flags that take a directory only in the separated form. GCC spells them
            // this way and nothing writes `-iquotedir`, so accepting the joined form would
            // mean guessing at a path that starts with the flag's own letters.
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
            _ if arg.starts_with("-fgnuc-version=") => {
                let v = &arg["-fgnuc-version=".len()..];
                opts.gnuc = v.parse().map_err(err)?;
            }
            _ if arg.starts_with("-j") => {
                jobs = Jobs::parse(&arg[2..]).map_err(err)?;
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
            _ if arg.starts_with('-') && arg.len() > 1 => {
                // Silently ignoring an unknown flag is how a build ends up not doing what
                // its author asked. spec/13-gnu-compat.md section 13.4 makes this an error
                // for the flags that change code generation, and the safe default until the
                // flag table is populated is to reject everything we do not know.
                return Err(err(format!("unknown option `{arg}`")));
            }
            _ => inputs.push(Input { path: arg.to_owned(), forced }),
        }
    }

    // The target has to be resolved before the configuration is printed, so this check comes
    // after the loop rather than at the point `--print-config` was seen.
    if print_config {
        return Ok(Action::PrintConfig(Box::new(opts)));
    }
    let plan = Plan::new(&opts, &inputs, output.as_deref()).map_err(|e| err(e.message))?;
    if print_plan {
        return Ok(Action::PrintPlan(Box::new(plan)));
    }
    Ok(Action::Compile { opts: Box::new(opts), plan: Box::new(plan), jobs, verbose })
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
    let _ = writeln!(out, "opt-level: {}", sess.opts.opt_level);
    let _ = writeln!(out, "emit: {}", sess.opts.emit.as_str());
    let _ = writeln!(out, "debug-info: {}", sess.opts.debug_info);
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
        match &job.output {
            Output::Stdout => {
                let mut stdout = std::io::stdout().lock();
                if let Err(e) = stdout.write_all(result.text.as_bytes()) {
                    let _ = writeln!(stderr, "rucc: error: writing to standard output: {e}");
                    failed = true;
                }
            }
            Output::File(path) | Output::Temporary(path) => {
                if let Err(e) = std::fs::write(path, result.text.as_bytes()) {
                    let _ = writeln!(stderr, "rucc: error: {path}: {e}");
                    failed = true;
                }
            }
        }
    }
    i32::from(failed)
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
        Ok(Action::PrintPlan(plan)) => {
            print!("{}", plan.render());
            0
        }
        Ok(Action::Compile { opts, plan, jobs, verbose }) => {
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
            let mut stderr = std::io::stderr().lock();
            // Everything after phase 4 is M2 and M3 in spec/17-milestones.md. The plan above
            // is real and can be inspected with `-###`. Saying so is better than a panic, and
            // better than pretending to have produced an object.
            let _ = writeln!(
                stderr,
                "rucc: error: running the {} phase is not implemented yet; \
                 use -E for preprocessed output, and see spec/17-milestones.md for the rest",
                opts.emit.as_str()
            );
            1
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

    #[test]
    fn collects_inputs_and_flags() {
        let (opts, plan) = compile(&["-c", "-O2", "-g", "a.c", "b.c"]);
        let paths: Vec<&str> = plan.jobs.iter().map(|j| j.input.as_str()).collect();
        assert_eq!(paths, vec!["a.c", "b.c"]);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.emit, EmitKind::Object);
        assert!(opts.debug_info);
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
        let Action::PrintPlan(plan) = a else { panic!("expected a plan dump") };
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
    }

    #[test]
    fn print_config_has_one_key_per_line_and_a_fixed_order() {
        let opts = Options::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let text = print_config(&opts);
        let keys: Vec<&str> =
            text.lines().map(|l| l.split(':').next().unwrap_or_default()).collect();
        assert_eq!(keys[0], "version");
        assert_eq!(keys[1], "target");
        assert_eq!(keys.len(), 14);
        assert!(text.ends_with('\n'));
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
        let (opts, _) =
            compile(&["-Ii", "-iquote", "q", "-isystem", "sys", "-idirafter", "after", "a.c"]);
        let dirs: Vec<&str> = opts.search.dirs().iter().filter_map(|d| d.path.to_str()).collect();
        assert_eq!(dirs, ["q", "i", "sys", "after"]);
        assert!(!opts.search.dirs()[1].is_system);
        assert!(opts.search.dirs()[2].is_system);
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
            GnucVersion { major: 4, minor: 2, patch: 1 },
            "conservative by default"
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
    fn dash_p_and_dash_ffreestanding_reach_the_options() {
        let (opts, _) = compile(&["-E", "-P", "-ffreestanding", "a.c"]);
        assert!(!opts.line_markers);
        assert!(!opts.hosted);
        assert_eq!(opts.emit, EmitKind::Preprocessed);
    }

    #[test]
    fn usage_fits_on_a_screen() {
        // Not a style preference. A help text that scrolls is one nobody reads, and this is
        // the cheapest way to keep it honest as flags accumulate.
        assert!(USAGE.lines().count() < 30, "usage text has grown past one screen");
    }
}
