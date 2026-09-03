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
//! Options, optimisation levels, emit kinds, diagnostic counting, the source map every span
//! is resolved against, the file system the compiler reads through, the include search path
//! and the headers the compiler itself ships are real. The parallel job model is still a
//! placeholder.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-session/0.3.2")]

mod fs;
pub mod runtime;

pub use crate::fs::{Dir, FileSystem, Found, IncludeForm, MemoryFileSystem, SearchPath, path_key};

use std::fmt;
use std::str::FromStr;

use rucc_base::Interner;
use rucc_diag::{Diagnostic, Severity, SourceMap};
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

/// Which C the source is written in.
///
/// The GNU variants are the same language with `__STRICT_ANSI__` left undefined, so the
/// dialect and the extension question are two fields rather than ten variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Std {
    /// `-std=c89`, and `-ansi`.
    C89,
    /// `-std=c99`.
    C99,
    /// `-std=c11`.
    C11,
    /// `-std=c17`, which is C11 with the defect reports applied.
    C17,
    /// `-std=c23`. The default, matching current GCC.
    #[default]
    C23,
}

impl Std {
    /// What `__STDC_VERSION__` says, which C89 does not define at all.
    pub const fn stdc_version(self) -> Option<&'static str> {
        match self {
            Std::C89 => None,
            Std::C99 => Some("199901L"),
            Std::C11 => Some("201112L"),
            Std::C17 => Some("201710L"),
            Std::C23 => Some("202311L"),
        }
    }

    /// The name in `-std=`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Std::C89 => "c89",
            Std::C99 => "c99",
            Std::C11 => "c11",
            Std::C17 => "c17",
            Std::C23 => "c23",
        }
    }

    /// Whether this dialect has `_Atomic`, `_Thread_local` and the rest of C11.
    pub const fn has_c11(self) -> bool {
        matches!(self, Std::C11 | Std::C17 | Std::C23)
    }

    /// Reads a `-std=` argument, and says whether the GNU extensions came with it.
    ///
    /// Every alias GCC takes is here, including the `iso9899` spellings and the year based
    /// ones, because a build system that passes `-std=iso9899:1999` is passing what its
    /// author tested against and rejecting it helps nobody. An unknown dialect is `None`
    /// rather than a guess, since guessing means compiling a different language than the one
    /// asked for.
    #[must_use]
    pub fn from_flag(name: &str) -> Option<(Std, bool)> {
        let gnu = name.starts_with("gnu");
        let std = match name {
            "c89" | "c90" | "gnu89" | "gnu90" | "iso9899:1990" | "iso9899:199409" => Std::C89,
            "c99" | "c9x" | "gnu99" | "gnu9x" | "iso9899:1999" | "iso9899:199x" => Std::C99,
            "c11" | "c1x" | "gnu11" | "gnu1x" | "iso9899:2011" => Std::C11,
            "c17" | "c18" | "gnu17" | "gnu18" | "iso9899:2017" | "iso9899:2018" => Std::C17,
            "c23" | "c2x" | "gnu23" | "gnu2x" => Std::C23,
            _ => return None,
        };
        Some((std, gnu))
    }
}

/// The GCC release the compiler claims to be, as `__GNUC__`, `__GNUC_MINOR__` and
/// `__GNUC_PATCHLEVEL__`.
///
/// Design: `spec/04-driver-and-cli.md` section 4.5, which makes this a knob rather than a
/// constant and says to start conservative and raise it as the matrix in `rucc-gnu` fills in.
///
/// The default is seven, which is the lowest claim that gets a modern glibc. glibc gates most
/// of what it hands a caller on `__GNUC_PREREQ`, so the claim decides which half of
/// `sys/cdefs.h` we get, and below seven `bits/floatn-common.h` writes `typedef float _Float32;`
/// over a keyword this compiler already has. Every header that reaches it stops there, which
/// was most of them: on Ubuntu 24.04's glibc 2.39 the claim of 4.2.1 that stood here before got
/// 180 of 214 headers through and seven gets 202, and the amalgamated sqlite goes from four
/// errors to none.
///
/// It is still deliberately low. Claiming a version whose promises have not been kept means
/// being handed syntax the compiler cannot parse, so this moves when there is a measurement
/// saying it can. Thirteen and sixteen were measured alongside seven and came out identical on
/// glibc, on the macOS SDK and on sqlite, so the next move up is cheap; it is a separate one
/// because nothing yet needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GnucVersion {
    /// `__GNUC__`.
    pub major: u32,
    /// `__GNUC_MINOR__`.
    pub minor: u32,
    /// `__GNUC_PATCHLEVEL__`.
    pub patch: u32,
}

impl Default for GnucVersion {
    fn default() -> GnucVersion {
        GnucVersion { major: 7, minor: 0, patch: 0 }
    }
}

impl FromStr for GnucVersion {
    type Err = String;

    /// Reads `-fgnuc-version=`, which is `15`, `15.1` or `15.1.0`.
    ///
    /// The short forms are not a convenience, they are what people write. A missing component
    /// is zero, the same way GCC treats a release with no patchlevel.
    fn from_str(text: &str) -> Result<GnucVersion, String> {
        let mut parts = text.split('.');
        let mut next = |what: &str| -> Result<u32, String> {
            match parts.next() {
                None => Ok(0),
                Some(field) => {
                    field.parse().map_err(|_| format!("`{text}` has a {what} that is not a number"))
                }
            }
        };
        let major = next("major")?;
        let minor = next("minor")?;
        let patch = next("patchlevel")?;
        if parts.next().is_some() {
            return Err(format!("`{text}` has more than three components"));
        }
        Ok(GnucVersion { major, minor, patch })
    }
}

/// What the `-d` family asks to be dumped alongside, or instead of, the preprocessed output.
///
/// Design: `spec/04-driver-and-cli.md` section 4.4.
///
/// GCC spells these as letters packed into one flag, so `-dDI` is two of them, and a letter it
/// does not know is ignored rather than rejected. That last part is deliberate on GCC's side
/// and worth copying: the family is a debugging aid and a build that passes `-dumpbase` should
/// not die on the `-d`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dumps {
    /// `-dM`. Print the macros that are defined at the end, and nothing else.
    pub macros: bool,
}

impl Dumps {
    /// The letters GCC's preprocessor takes after `-d`.
    ///
    /// `M` is the macros, `D` is the macros in place, `N` is their names only, `I` is the
    /// `#include` lines and `U` is the macros as they are used. Only `M` does anything so far.
    const LETTERS: &'static str = "MDNIU";

    /// Whether `arg` is a flag from this family rather than something else beginning with
    /// `-d`.
    ///
    /// The check is here rather than in the driver so that the set of letters and the set of
    /// flags accepted cannot drift apart. It matters because `-dumpversion` also begins with
    /// `-d`, and a family that swallowed every such flag would turn a flag we have not written
    /// into a dump of nothing.
    #[must_use]
    pub fn is_family(arg: &str) -> bool {
        match arg.strip_prefix("-d") {
            Some("") | None => false,
            Some(letters) => letters.chars().all(|c| Dumps::LETTERS.contains(c)),
        }
    }

    /// Reads the letters after `-d`, ignoring the ones we do not implement yet.
    pub fn add(&mut self, letters: &str) {
        for letter in letters.chars() {
            if letter == 'M' {
                self.macros = true;
            }
        }
    }

    /// Whether anything at all was asked for.
    #[must_use]
    pub const fn any(self) -> bool {
        self.macros
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
    /// The dialect, from `-std=`.
    pub std: Std,
    /// Whether the GNU extensions are on, which is `-std=gnu23` rather than `-std=c23`.
    pub gnu_extensions: bool,
    /// Whether `-pedantic` was given, which is what turns a use of an extension from silence
    /// into a diagnostic. It is not the same knob as the dialect: `-std=c17 -pedantic` warns
    /// about a construct that `-std=c17` alone accepts without a word.
    pub pedantic: bool,
    /// The GCC release claimed, from `-fgnuc-version=`.
    pub gnuc: GnucVersion,
    /// Whether there is a standard library, which is `-ffreestanding` turned around.
    pub hosted: bool,
    /// `-D` in command line order. `FOO` means `FOO=1`, as GCC has it.
    pub defines: Vec<String>,
    /// `-U` in command line order, applied after the defines because `-U` wins.
    pub undefines: Vec<String>,
    /// Where a header is looked for.
    pub search: SearchPath,
    /// Whether `-E` writes line markers, which `-P` turns off.
    pub line_markers: bool,
    /// What the `-d` family asks for.
    pub dumps: Dumps,
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
            std: Std::default(),
            gnu_extensions: true,
            pedantic: false,
            gnuc: GnucVersion::default(),
            hosted: true,
            defines: Vec::new(),
            undefines: Vec::new(),
            search: SearchPath::new(),
            line_markers: true,
            dumps: Dumps::default(),
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
    /// Every file read during the compilation, and the flat coordinate space their spans
    /// live in.
    ///
    /// This is on the session rather than passed around separately because a span is only
    /// meaningful against the map that issued it, and one map per compilation is the rule
    /// that makes that true by construction.
    pub sources: SourceMap,
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
            sources: SourceMap::new(),
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
    fn a_version_claim_reads_the_way_gcc_prints_one() {
        // `gcc -dumpfullversion` gives all three, `gcc -dumpversion` gives one, and both are
        // things a script pastes straight into a flag.
        let all = |v: &str| v.parse::<GnucVersion>().unwrap();
        assert_eq!(all("15.1.0"), GnucVersion { major: 15, minor: 1, patch: 0 });
        assert_eq!(all("15"), GnucVersion { major: 15, minor: 0, patch: 0 });
        assert_eq!(all("4.2"), GnucVersion { major: 4, minor: 2, patch: 0 });
        assert!("".parse::<GnucVersion>().is_err());
        assert!("15.".parse::<GnucVersion>().is_err(), "a trailing dot is a typo, not a zero");
        assert!("1.2.3.4".parse::<GnucVersion>().is_err());
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
    fn the_session_carries_the_source_map_spans_are_resolved_against() {
        let mut s = session();
        let file = s.sources.add("a.c", b"int x;\n".to_vec()).unwrap();
        let start = s.sources.file(file).start;
        assert_eq!(s.sources.render_position(start + 4), "a.c:1:5");
    }

    #[test]
    fn the_session_carries_the_resolved_target() {
        let s = session();
        assert_eq!(s.target.pointer_width, 64);
        assert!(s.target.char_is_signed);
    }
}
