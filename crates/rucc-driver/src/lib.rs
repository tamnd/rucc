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
//! `spec/17-milestones.md`. Nothing compiles C yet, and asking it to says so.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-driver/0.0.1")]

use std::fmt::Write as _;
use std::io::Write as _;

use rucc_session::{EmitKind, Options, Session};
use rucc_target::Triple;

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
    /// Compile the given inputs.
    Compile { opts: Box<Options>, inputs: Vec<String> },
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
  -O<level>              optimize: 0, 1, 2, 3, s, z
  -g                     emit debug information
  -Werror                treat warnings as errors
  --target=<triple>      generate code for <triple>
  --emit=<kind>          exe, obj, asm, preprocessed, tast, ir, mir-final
  --print-config         print the resolved configuration and exit
  --version              print the version and exit
  -h, --help             print this message and exit

See spec/04-driver-and-cli.md for the full flag reference.
";

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
    let mut inputs = Vec::new();
    let mut print_config = false;
    // `-o` is accepted and recorded by the caller once there is something to write. Parsing
    // it here keeps the driver honest about which flags it consumes.
    let mut output = None;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        match arg {
            "-h" | "--help" => return Ok(Action::Help),
            "--version" => return Ok(Action::Version),
            "--print-config" => print_config = true,
            "-c" => opts.emit = EmitKind::Object,
            "-S" => opts.emit = EmitKind::Asm,
            "-E" => opts.emit = EmitKind::Preprocessed,
            "-g" => opts.debug_info = true,
            "-Werror" => opts.warnings_are_errors = true,
            "-o" => {
                output = Some(args.get(i).ok_or_else(|| err("-o requires an argument"))?.clone());
                i += 1;
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
            _ => inputs.push(arg.to_owned()),
        }
    }

    // The target has to be resolved before the configuration is printed, so this check comes
    // after the loop rather than at the point `--print-config` was seen.
    if print_config {
        return Ok(Action::PrintConfig(Box::new(opts)));
    }
    if inputs.is_empty() {
        return Err(err("no input files"));
    }
    if output.is_some() && inputs.len() > 1 && opts.emit != EmitKind::Executable {
        return Err(err("cannot specify -o with multiple inputs when not linking"));
    }
    Ok(Action::Compile { opts: Box::new(opts), inputs })
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
        Ok(Action::Compile { .. }) => {
            // M0 in spec/17-milestones.md is the skeleton and nothing more. Saying so is
            // better than a panic, and better than pretending to have produced an object.
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(
                stderr,
                "rucc: error: compiling C is not implemented yet; \
                 the frontend lands in M1 and M2, see spec/17-milestones.md"
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
    use rucc_session::OptLevel;

    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        assert_eq!(parse_args(&args(&["-c", "--help", "x.c"])).unwrap(), Action::Help);
        assert_eq!(parse_args(&args(&["--version"])).unwrap(), Action::Version);
    }

    #[test]
    fn collects_inputs_and_flags() {
        let a = parse_args(&args(&["-c", "-O2", "-g", "a.c", "b.c"])).unwrap();
        let Action::Compile { opts, inputs } = a else { panic!("expected a compilation") };
        assert_eq!(inputs, vec!["a.c", "b.c"]);
        assert_eq!(opts.opt_level, OptLevel::O2);
        assert_eq!(opts.emit, EmitKind::Object);
        assert!(opts.debug_info);
    }

    #[test]
    fn a_bare_dash_o_means_o1_the_way_gcc_reads_it() {
        let a = parse_args(&args(&["-O", "a.c"])).unwrap();
        let Action::Compile { opts, .. } = a else { panic!("expected a compilation") };
        assert_eq!(opts.opt_level, OptLevel::O1);
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
    fn usage_fits_on_a_screen() {
        // Not a style preference. A help text that scrolls is one nobody reads, and this is
        // the cheapest way to keep it honest as flags accumulate.
        assert!(USAGE.lines().count() < 30, "usage text has grown past one screen");
    }
}
