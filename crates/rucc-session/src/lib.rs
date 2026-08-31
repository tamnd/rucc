//! The `Session`: the options, the interner and the diagnostic sink that every stage of a
//! single compilation is handed.
//!
//! Design: `spec/03-architecture.md` and `spec/04-driver-and-cli.md`. Layer rank 3, see
//! `spec/18-package-layout.md`.
//!
//! Everything below the driver reaches the outside world through this type and not through
//! `std::fs`, `std::env` or `println!`. That is the whole reason the compiler can be used as
//! a library and tested without spawning a process, and it is enforced by the layer rule
//! rather than by discipline.
//!
//! # Status
//!
//! Options, optimisation levels, emit kinds and diagnostic counting are real. The parallel
//! job model and the file system abstraction land with the rest of `M0` and `M1`.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-session/0.0.1")]

use std::fmt;
use std::str::FromStr;

use rucc_base::Interner;
use rucc_diag::{Diagnostic, Severity};
use rucc_target::{TargetInfo, Triple};

/// An optimisation level.
///
/// `spec/16-performance.md` section 16.4 gives each level a throughput budget and a code
/// quality budget, and the levels exist to make that tradeoff explicit rather than to be a
/// dial. There is no `-O4`, because a level nobody can state the contract for is a level
/// nobody can test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum OptLevel {
    /// `-O0`. Compile as fast as possible and keep every variable inspectable.
    #[default]
    O0,
    /// `-O1`. The cheap wins, at roughly the cost of `-O0`.
    O1,
    /// `-O2`. The full pipeline. This is the level the code quality claim is about.
    O2,
    /// `-O3`. `-O2` plus the transformations that trade size for speed.
    O3,
    /// `-Os`. Optimise for size, at roughly `-O2` compile time.
    Os,
    /// `-Oz`. Optimise for size, aggressively.
    Oz,
}

impl OptLevel {
    /// The flag that selects this level.
    pub const fn as_flag(self) -> &'static str {
        match self {
            OptLevel::O0 => "-O0",
            OptLevel::O1 => "-O1",
            OptLevel::O2 => "-O2",
            OptLevel::O3 => "-O3",
            OptLevel::Os => "-Os",
            OptLevel::Oz => "-Oz",
        }
    }

    /// Whether this level optimises for size rather than speed.
    pub const fn is_size(self) -> bool {
        matches!(self, OptLevel::Os | OptLevel::Oz)
    }

    /// Whether the middle end runs at all.
    pub const fn runs_optimizer(self) -> bool {
        !matches!(self, OptLevel::O0)
    }
}

impl fmt::Display for OptLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_flag())
    }
}

impl FromStr for OptLevel {
    type Err = ();

    /// Parses the part after `-O`, so `""` is `-O` which GCC treats as `-O1`.
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "0" => OptLevel::O0,
            "" | "1" => OptLevel::O1,
            "2" => OptLevel::O2,
            // GCC accepts `-O4` and above and treats them as `-O3`. Build systems in the
            // wild do pass them, so matching that is cheaper than being right.
            "3" | "4" | "5" | "6" | "7" | "8" | "9" => OptLevel::O3,
            "s" => OptLevel::Os,
            "z" => OptLevel::Oz,
            _ => return Err(()),
        })
    }
}

/// What the compiler should produce.
///
/// The intermediate forms are not a debugging convenience bolted on later. Every one of them
/// is a documented textual form that round-trips, which is what makes the per-stage testing
/// in `spec/15-testing.md` section 15.2 possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
// Deliberately not `#[non_exhaustive]`. Adding a variant here has to break every
// match that needs to change, in this workspace and in anyone else's code. That is
// the property `spec/10-backend.md` section 10.8 is claiming when it says adding a
// target is a data change: the compiler tells you every place the data is read.
pub enum EmitKind {
    /// A linked executable. The default.
    #[default]
    Executable,
    /// An object file, `-c`.
    Object,
    /// Assembly text, `-S`.
    Asm,
    /// Preprocessed source, `-E`.
    Preprocessed,
    /// The typed AST, `--emit=tast`.
    Tast,
    /// The IR, `--emit=ir`.
    Ir,
    /// The machine IR after register allocation, `--emit=mir-final`.
    MirFinal,
}

impl EmitKind {
    /// The name used by `--emit=` and by `--print-config`.
    pub const fn as_str(self) -> &'static str {
        match self {
            EmitKind::Executable => "exe",
            EmitKind::Object => "obj",
            EmitKind::Asm => "asm",
            EmitKind::Preprocessed => "preprocessed",
            EmitKind::Tast => "tast",
            EmitKind::Ir => "ir",
            EmitKind::MirFinal => "mir-final",
        }
    }
}

impl FromStr for EmitKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "exe" => EmitKind::Executable,
            "obj" => EmitKind::Object,
            "asm" => EmitKind::Asm,
            "preprocessed" => EmitKind::Preprocessed,
            "tast" => EmitKind::Tast,
            "ir" => EmitKind::Ir,
            "mir-final" => EmitKind::MirFinal,
            _ => return Err(()),
        })
    }
}

/// Everything a compilation was asked to do.
///
/// Options are a plain value with no interior mutability, so a caller can build one, clone
/// it, tweak one field and run a second compilation, which is exactly what the differential
/// testing in `spec/15-testing.md` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// The target to generate code for.
    pub target: Triple,
    /// The optimisation level.
    pub opt_level: OptLevel,
    /// What to produce.
    pub emit: EmitKind,
    /// Whether to emit debug information.
    pub debug_info: bool,
    /// Whether warnings are errors.
    pub warnings_are_errors: bool,
    /// How many diagnostics to print before giving up. Past a certain point the output is
    /// noise from a single earlier mistake, and GCC's default of no limit is not a kindness.
    pub error_limit: u32,
}

impl Options {
    /// Default options for `target`.
    pub fn new(target: Triple) -> Self {
        Self {
            target,
            opt_level: OptLevel::default(),
            emit: EmitKind::default(),
            debug_info: false,
            warnings_are_errors: false,
            error_limit: 20,
        }
    }
}

/// One compilation.
///
/// Holds the options, the string interner and the diagnostics raised so far. Passing a
/// `&mut Session` is how a stage reports a problem, and the return value of a stage says
/// what it produced, never whether it succeeded: that question is answered by
/// [`Session::has_errors`].
#[derive(Debug)]
pub struct Session {
    /// What this compilation was asked to do.
    pub opts: Options,
    /// Everything known about the target.
    pub target: TargetInfo,
    /// The one interner for the compilation.
    pub interner: Interner,
    diagnostics: Vec<Diagnostic>,
    error_count: u32,
    warning_count: u32,
}

impl Session {
    /// A session for `opts`.
    pub fn new(opts: Options) -> Self {
        let target = TargetInfo::new(opts.target);
        Self {
            opts,
            target,
            interner: Interner::with_capacity(1024),
            diagnostics: Vec::new(),
            error_count: 0,
            warning_count: 0,
        }
    }

    /// Records a diagnostic.
    ///
    /// Under `-Werror` a warning is promoted here, once, rather than at every site that
    /// raises one.
    pub fn emit(&mut self, mut diag: Diagnostic) {
        if self.opts.warnings_are_errors && diag.severity == Severity::Warning {
            diag.severity = Severity::Error;
        }
        match diag.severity {
            Severity::Error | Severity::Ice => self.error_count += 1,
            Severity::Warning => self.warning_count += 1,
            Severity::Note | Severity::Help => {}
        }
        self.diagnostics.push(diag);
    }

    /// Everything raised so far, in the order it was raised.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Whether anything fatal has been raised.
    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    /// How many errors have been raised.
    pub fn error_count(&self) -> u32 {
        self.error_count
    }

    /// How many warnings have been raised.
    pub fn warning_count(&self) -> u32 {
        self.warning_count
    }

    /// Whether the error limit has been reached and the caller should stop.
    pub fn error_limit_reached(&self) -> bool {
        self.opts.error_limit != 0 && self.error_count >= self.opts.error_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(Options::new("x86_64-unknown-linux-gnu".parse().unwrap()))
    }

    #[test]
    fn optimisation_levels_parse_the_way_gcc_spells_them() {
        assert_eq!("".parse::<OptLevel>().unwrap(), OptLevel::O1);
        assert_eq!("0".parse::<OptLevel>().unwrap(), OptLevel::O0);
        assert_eq!("2".parse::<OptLevel>().unwrap(), OptLevel::O2);
        assert_eq!("9".parse::<OptLevel>().unwrap(), OptLevel::O3);
        assert_eq!("s".parse::<OptLevel>().unwrap(), OptLevel::Os);
        assert!("q".parse::<OptLevel>().is_err());
    }

    #[test]
    fn only_o0_skips_the_optimizer() {
        assert!(!OptLevel::O0.runs_optimizer());
        assert!(OptLevel::O1.runs_optimizer());
        assert!(OptLevel::Oz.runs_optimizer());
    }

    #[test]
    fn emit_kinds_round_trip_through_their_names() {
        for k in [
            EmitKind::Executable,
            EmitKind::Object,
            EmitKind::Asm,
            EmitKind::Preprocessed,
            EmitKind::Tast,
            EmitKind::Ir,
            EmitKind::MirFinal,
        ] {
            assert_eq!(k.as_str().parse::<EmitKind>().unwrap(), k);
        }
    }

    #[test]
    fn errors_are_counted_and_warnings_are_not() {
        let mut s = session();
        s.emit(Diagnostic::error("no", rucc_diag::Span::DUMMY));
        s.emit(Diagnostic::warning("hmm", rucc_diag::Span::DUMMY));
        assert_eq!(s.error_count(), 1);
        assert_eq!(s.warning_count(), 1);
        assert!(s.has_errors());
        assert_eq!(s.diagnostics().len(), 2);
    }

    #[test]
    fn werror_promotes_once_at_the_sink() {
        let mut opts = Options::new("x86_64-unknown-linux-gnu".parse().unwrap());
        opts.warnings_are_errors = true;
        let mut s = Session::new(opts);
        s.emit(Diagnostic::warning("hmm", rucc_diag::Span::DUMMY));
        assert_eq!(s.error_count(), 1);
        assert_eq!(s.warning_count(), 0);
        assert_eq!(s.diagnostics()[0].severity, Severity::Error);
    }

    #[test]
    fn the_error_limit_can_be_switched_off() {
        let mut opts = Options::new("x86_64-unknown-linux-gnu".parse().unwrap());
        opts.error_limit = 0;
        let mut s = Session::new(opts);
        for _ in 0..100 {
            s.emit(Diagnostic::error("no", rucc_diag::Span::DUMMY));
        }
        assert!(!s.error_limit_reached());
    }

    #[test]
    fn the_session_carries_the_resolved_target() {
        let s = session();
        assert_eq!(s.target.pointer_width, 64);
        assert!(s.target.char_is_signed);
    }
}
