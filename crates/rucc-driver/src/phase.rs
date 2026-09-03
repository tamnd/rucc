//! The phase graph: what has to happen to each input file, in what order, and where the
//! result goes.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.2.
//!
//! The plan is computed before anything runs and is a plain data structure with no side
//! effects, which is what makes `-###` possible and what makes this testable without a file
//! system. Nothing in here reads a file or spawns a process. Executing the plan is M3, when
//! there is something for the phases to do.

use std::fmt::Write as _;

use rucc_session::{EmitKind, Options};
use rucc_target::Os;

use crate::link::Item;

/// A step in the compilation of one input.
///
/// The order of the variants is the order of the pipeline, and the derived `Ord` is relied on
/// when a mode flag truncates a sequence. `Compile` covers parsing through code generation,
/// which is one phase from the driver's point of view because nothing between them can be
/// stopped at from the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// Translation phases 1 to 4, producing preprocessed source.
    Preprocess,
    /// Parse, check, optimize and generate code, producing assembly.
    Compile,
    /// Assemble, producing an object file.
    Assemble,
    /// Link the objects into an executable or a shared library.
    Link,
}

impl Phase {
    /// The name used in `-###` output and in diagnostics.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Preprocess => "preprocess",
            Phase::Compile => "compile",
            Phase::Assemble => "assemble",
            Phase::Link => "link",
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an input file is, which decides where in the pipeline it enters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputKind {
    /// C source. Extension `.c`, or `-x c`.
    C,
    /// A header compiled on its own. Extension `.h` with `-x c-header`, or `-x c-header`.
    CHeader,
    /// Already preprocessed C. Extension `.i`, or `-x cpp-output`.
    PreprocessedC,
    /// The IR this compiler prints. Extension `.ir`, or `-x ir`.
    ///
    /// Not a GCC input kind, because GCC has no textual IR. It is here because the IR's
    /// printer and its parser are a pair, and a pair is only known to agree if something reads
    /// back what was written: `rucc --emit=ir a.c -o a.ir` and then `rucc --emit=ir a.ir` are
    /// two files a byte comparison has an opinion about, over whatever code is at hand rather
    /// than over the modules a test happens to build.
    Ir,
    /// Assembly. Extension `.s`, or `-x assembler`.
    Assembler,
    /// Assembly that still needs the preprocessor. Extension `.S` or `.sx`, or
    /// `-x assembler-with-cpp`.
    AssemblerWithCpp,
    /// An object file, an archive or a shared library. Anything the linker takes directly.
    LinkerInput,
}

impl InputKind {
    /// The name `-x` uses for this kind, where one exists.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            InputKind::C => "c",
            InputKind::CHeader => "c-header",
            InputKind::PreprocessedC => "cpp-output",
            InputKind::Ir => "ir",
            InputKind::Assembler => "assembler",
            InputKind::AssemblerWithCpp => "assembler-with-cpp",
            InputKind::LinkerInput => "linker-input",
        }
    }

    /// Parses the argument of `-x`.
    ///
    /// # Errors
    ///
    /// Returns the offending name when it is not one we accept. C++ gets its own message,
    /// because "unknown language c++" reads like an oversight and it is a decision.
    pub fn from_x_arg(name: &str) -> Result<InputKind, XError> {
        match name {
            "c" => Ok(InputKind::C),
            "c-header" => Ok(InputKind::CHeader),
            "cpp-output" | "c-cpp-output" => Ok(InputKind::PreprocessedC),
            "ir" => Ok(InputKind::Ir),
            "assembler" => Ok(InputKind::Assembler),
            "assembler-with-cpp" => Ok(InputKind::AssemblerWithCpp),
            "c++" | "c++-header" | "c++-cpp-output" | "objective-c" | "objective-c++" => {
                Err(XError::Unsupported(name.to_owned()))
            }
            _ => Err(XError::Unknown(name.to_owned())),
        }
    }

    /// Classifies an input by its extension, the way `spec/04-driver-and-cli.md` section 4.2
    /// tabulates it.
    ///
    /// An unrecognized extension is a linker input, which is GCC's behavior and is what makes
    /// `rucc foo.o bar.builtin-suffix` work. The exception is a C++ extension, which is a
    /// hard error rather than a confusing link failure later.
    ///
    /// # Errors
    ///
    /// Returns the extension when it names a language that is permanently out of scope.
    pub fn from_path(path: &str) -> Result<InputKind, XError> {
        let ext = extension(path);
        match ext {
            // Matched case-sensitively on purpose: `.S` and `.s` are different languages and
            // conflating them is a real bug on case-insensitive file systems that GCC also
            // has. The comment is here so the next person does not "fix" it.
            "c" => Ok(InputKind::C),
            "i" => Ok(InputKind::PreprocessedC),
            "ir" => Ok(InputKind::Ir),
            "h" => Ok(InputKind::CHeader),
            "s" => Ok(InputKind::Assembler),
            "S" | "sx" => Ok(InputKind::AssemblerWithCpp),
            "cc" | "cpp" | "cxx" | "c++" | "C" | "hpp" | "hxx" | "ii" | "m" | "mm" => {
                Err(XError::Unsupported(ext.to_owned()))
            }
            _ => Ok(InputKind::LinkerInput),
        }
    }

    /// The full phase sequence for this kind, before any mode flag truncates it.
    fn full_sequence(self) -> &'static [Phase] {
        use Phase::{Assemble, Compile, Link, Preprocess};
        match self {
            InputKind::C | InputKind::CHeader => &[Preprocess, Compile, Assemble, Link],
            InputKind::PreprocessedC | InputKind::Ir => &[Compile, Assemble, Link],
            // Note the gap: assembly with a preprocessor skips `Compile` entirely. This is why
            // the sequence is a list rather than a range over the enum.
            InputKind::AssemblerWithCpp => &[Preprocess, Assemble, Link],
            InputKind::Assembler => &[Assemble, Link],
            InputKind::LinkerInput => &[Link],
        }
    }
}

/// Why an input or an `-x` argument was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XError {
    /// A language we do not know at all.
    Unknown(String),
    /// A language we know and will not implement.
    Unsupported(String),
}

impl std::fmt::Display for XError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XError::Unknown(name) => {
                write!(
                    f,
                    "unknown language `{name}`; \
                     accepted: c, c-header, cpp-output, ir, assembler, assembler-with-cpp, none"
                )
            }
            XError::Unsupported(name) => {
                write!(
                    f,
                    "`{name}` is not C, and this compiler is only ever going to compile C; \
                     see the not-in-scope list in spec/00-README.md"
                )
            }
        }
    }
}

impl std::error::Error for XError {}

/// One input file, with the `-x` setting that was in effect where it appeared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    /// The path as it was written on the command line, or the name of a `-l` library.
    pub path: String,
    /// The language forced by an earlier `-x`, if any. `-x none` clears it.
    pub forced: Option<InputKind>,
    /// Whether this came from `-l<name>` rather than being a path.
    ///
    /// A library is an input to the link and is held here rather than beside the other link
    /// flags, because where it falls among the objects is what decides whether it is searched
    /// for what they left undefined. A list of objects and a separate list of libraries would
    /// lose exactly that.
    pub library: bool,
}

impl Input {
    /// An input with no `-x` in effect.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Input {
        Input { path: path.into(), forced: None, library: false }
    }

    /// `-l<name>`, which is an input to the link and to nothing else.
    #[must_use]
    pub fn library(name: impl Into<String>) -> Input {
        Input { path: name.into(), forced: None, library: true }
    }

    /// What this input is, taking `-x` into account.
    ///
    /// # Errors
    ///
    /// Returns the extension when it names a language that is out of scope.
    pub fn kind(&self) -> Result<InputKind, XError> {
        if self.library {
            return Ok(InputKind::LinkerInput);
        }
        match self.forced {
            Some(k) => Ok(k),
            None => InputKind::from_path(&self.path),
        }
    }
}

/// Where the result of a job goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Standard output, which is where `-E` writes when there is no `-o`.
    Stdout,
    /// A path the user can see and named, or that we derived from the input name.
    File(String),
    /// A file the link step consumes and nothing else ever sees. The name is a hint for
    /// `-###` output; the real path is chosen in a temporary directory at execution time.
    Temporary(String),
}

impl Output {
    fn render(&self) -> String {
        match self {
            Output::Stdout => "-".to_owned(),
            Output::File(p) => p.clone(),
            Output::Temporary(p) => format!("{p} (temporary)"),
        }
    }

    /// The path the link step reads, for an output that feeds it.
    fn as_link_input(&self) -> Option<&str> {
        match self {
            Output::File(p) | Output::Temporary(p) => Some(p),
            Output::Stdout => None,
        }
    }
}

/// Everything that has to happen to one input file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The input path as written.
    pub input: String,
    /// What we decided it is.
    pub kind: InputKind,
    /// The phases to run, in order. Empty when the input goes straight to the linker.
    pub phases: Vec<Phase>,
    /// Where the last phase writes.
    pub output: Output,
}

/// The link step, when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkJob {
    /// Objects and libraries, in command line order, because link order is semantic.
    pub inputs: Vec<Item>,
    /// The executable.
    pub output: String,
}

/// The whole plan for one invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// One per input, in command line order.
    pub jobs: Vec<Job>,
    /// The link step, or `None` when a mode flag stopped short of it.
    pub link: Option<LinkJob>,
    /// Things worth saying under `-v` that are not errors, such as an object file passed on a
    /// command line that is not linking.
    pub notes: Vec<String>,
}

/// Why a command line could not be turned into a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError {
    /// Lowercase, no trailing period, the same shape as every other diagnostic.
    pub message: String,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PlanError {}

fn plan_err(message: impl Into<String>) -> PlanError {
    PlanError { message: message.into() }
}

/// The last phase that runs, given what the user asked to be emitted.
///
/// `--emit=tast` and the other intermediate dumps stop where `-S` stops, because they are
/// produced inside the compile phase and there is nothing after them to run.
#[must_use]
pub fn last_phase(emit: EmitKind) -> Phase {
    match emit {
        EmitKind::Preprocessed => Phase::Preprocess,
        EmitKind::Asm | EmitKind::Tast | EmitKind::Ir | EmitKind::MirFinal => Phase::Compile,
        EmitKind::Object => Phase::Assemble,
        EmitKind::Executable => Phase::Link,
    }
}

/// The extension of a path, without the dot, or the empty string when there is none.
fn extension(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        // A leading dot is a hidden file, not an extension, and `.` and `..` are not inputs.
        Some(0) | None => "",
        Some(i) => &name[i + 1..],
    }
}

/// The path without its extension, keeping any directory part off, because GCC writes the
/// output into the current directory rather than next to the source.
fn stem(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(0) | None => name,
        Some(i) => &name[..i],
    }
}

/// The suffix a phase's output carries, for this target.
fn suffix_for(phase: Phase, opts: &Options) -> &'static str {
    match phase {
        Phase::Preprocess => "i",
        // The compile phase is where every intermediate dump comes out, and each of them is a
        // different language, so each gets a name of its own. `rucc --emit=tast a.c` writing
        // `a.s` would be a file that neither an assembler nor a reader could make sense of.
        Phase::Compile => match opts.emit {
            EmitKind::Tast => "tast",
            EmitKind::Ir => "ir",
            EmitKind::MirFinal => "mir",
            _ => "s",
        },
        // MSVC-targeted builds expect `.obj`, and build systems written for that target look
        // for it by name.
        Phase::Assemble => {
            if opts.target.os == Os::Windows {
                "obj"
            } else {
                "o"
            }
        }
        Phase::Link => "",
    }
}

/// The default name of the linked output, which is GCC's `a.out` everywhere but Windows.
fn default_exe(opts: &Options) -> &'static str {
    if opts.target.os == Os::Windows { "a.exe" } else { "a.out" }
}

impl Plan {
    /// Builds the plan for one invocation.
    ///
    /// `output` is the argument of `-o`, if it was given.
    ///
    /// # Errors
    ///
    /// Returns a message when the inputs and the mode flags do not describe a compilation:
    /// an out of scope language, `-o` naming one file for several outputs, or nothing to do.
    pub fn new(opts: &Options, inputs: &[Input], output: Option<&str>) -> Result<Plan, PlanError> {
        if inputs.is_empty() {
            return Err(plan_err("no input files"));
        }
        let last = last_phase(opts.emit);
        let linking = last == Phase::Link;

        let mut kinds = Vec::with_capacity(inputs.len());
        for input in inputs {
            kinds.push(input.kind().map_err(|e| plan_err(format!("{}: {e}", input.path)))?);
        }

        // How many inputs actually write an output of their own. A `.o` on a `-c` line
        // produces nothing, and neither does a `.s` on an `-E` line, so neither may count
        // toward the `-o` check below. When linking there is exactly one output and it is the
        // executable, so nothing counts.
        let producing = if linking {
            0
        } else {
            kinds
                .iter()
                .filter(|k| **k != InputKind::LinkerInput)
                .filter(|k| k.full_sequence().iter().any(|p| *p <= last))
                .count()
        };
        if output.is_some() && !linking && producing > 1 {
            return Err(plan_err("cannot specify -o with multiple inputs when not linking"));
        }

        let mut notes = Vec::new();
        let mut jobs = Vec::with_capacity(inputs.len());
        let mut link_inputs = Vec::new();

        for (input, kind) in inputs.iter().zip(kinds) {
            // An object, an archive or a shared library has nothing done to it. It reaches the
            // linker under the name it was written with, and its name is not derived from
            // anything, which is why this case is separate rather than falling out of the
            // sequence below. Deriving it would rewrite `libm.a` into `libm.o`.
            if kind == InputKind::LinkerInput {
                if linking {
                    link_inputs.push(if input.library {
                        Item::Library(input.path.clone())
                    } else {
                        Item::File(input.path.clone())
                    });
                } else {
                    // GCC warns and carries on here, and configure scripts rely on that, so
                    // this is a note rather than an error.
                    notes.push(format!(
                        "{}: linker input unused because linking was not requested",
                        if input.library {
                            format!("-l{}", input.path)
                        } else {
                            input.path.clone()
                        }
                    ));
                }
                // A library is not a file this compilation does anything to, so it gets no job.
                // One would print a line under `-###` saying nothing happens to it, next to the
                // note above already saying so.
                if input.library {
                    continue;
                }
                jobs.push(Job {
                    input: input.path.clone(),
                    kind,
                    phases: Vec::new(),
                    output: Output::File(input.path.clone()),
                });
                continue;
            }

            let phases: Vec<Phase> =
                kind.full_sequence().iter().copied().filter(|p| *p <= last).collect();
            // `rucc -E a.s` lands here: assembly enters at `Assemble`, which is past where
            // `-E` stops, so there is no phase left to run. GCC carries on rather than
            // failing, and so do we.
            let Some(&final_phase) = phases.last() else {
                notes.push(format!(
                    "{}: input unused because it enters the pipeline after the last phase \
                     the mode flags asked for",
                    input.path
                ));
                jobs.push(Job {
                    input: input.path.clone(),
                    kind,
                    phases,
                    output: Output::File(input.path.clone()),
                });
                continue;
            };
            let named = if producing == 1 { output } else { None };
            let out = if final_phase == Phase::Link {
                // The job stops at the object, and the link step below takes it from here.
                let ext = suffix_for(Phase::Assemble, opts);
                Output::Temporary(format!("{}.{ext}", stem(&input.path)))
            } else if let Some(o) = named {
                // `-o -` is standard output rather than a file of that name, which is what gcc
                // does for everything it compiles, the object file included. Its link step is
                // the exception and writes a file called `-`, because the name goes to the
                // linker and the linker takes it literally.
                if o == "-" { Output::Stdout } else { Output::File(o.to_owned()) }
            } else if final_phase == Phase::Preprocess {
                // `-E` writes to standard output unless it was given a name, which is the one
                // place where the default is not a file.
                Output::Stdout
            } else {
                Output::File(format!("{}.{}", stem(&input.path), suffix_for(final_phase, opts)))
            };
            // An input whose output has the name it has itself would be read and then written
            // over, and what it held would be gone. GCC compares the two names the way they
            // were written and so does this, which catches `rucc --emit=ir a.ir` and leaves
            // the same file reached by two different paths to the file system.
            if let Output::File(path) = &out {
                if *path == input.path {
                    return Err(plan_err(format!(
                        "input file `{}` is the same as the output file",
                        input.path
                    )));
                }
            }

            if linking {
                if let Some(p) = out.as_link_input() {
                    link_inputs.push(Item::File(p.to_owned()));
                }
            }
            jobs.push(Job { input: input.path.clone(), kind, phases, output: out });
        }

        let link = linking.then(|| LinkJob {
            inputs: link_inputs,
            output: output.unwrap_or(default_exe(opts)).to_owned(),
        });

        Ok(Plan { jobs, link, notes })
    }

    /// Renders the plan the way `-###` prints it.
    ///
    /// One line per job, then the link line. This is meant to be read next to `gcc -###`
    /// output when a build behaves differently under the two compilers, so it says what will
    /// happen rather than how it is represented.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for note in &self.notes {
            let _ = writeln!(out, "note: {note}");
        }
        for job in &self.jobs {
            // A linker input has no phases of its own. It shows up in the link line below, or
            // in a note above when there is no link line, and repeating it here would suggest
            // something happens to it.
            if job.phases.is_empty() {
                continue;
            }
            let names: Vec<&str> = job.phases.iter().map(|p| p.as_str()).collect();
            let _ = writeln!(out, "{}: {} -> {}", job.input, names.join(", "), job.output.render());
        }
        if let Some(link) = &self.link {
            let names: Vec<String> = link.inputs.iter().map(ToString::to_string).collect();
            let _ = writeln!(out, "link: {} -> {}", names.join(" "), link.output);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use rucc_session::Options;

    use super::*;

    fn opts(triple: &str) -> Options {
        Options::new(triple.parse().expect("test triple"))
    }

    fn linux() -> Options {
        opts("x86_64-unknown-linux-gnu")
    }

    fn plan(o: &Options, paths: &[&str], output: Option<&str>) -> Plan {
        let inputs: Vec<Input> = paths.iter().map(|p| Input::new(*p)).collect();
        Plan::new(o, &inputs, output).expect("expected a plan")
    }

    #[test]
    fn extensions_map_to_the_table_in_the_spec() {
        assert_eq!(InputKind::from_path("a.c").unwrap(), InputKind::C);
        assert_eq!(InputKind::from_path("a.i").unwrap(), InputKind::PreprocessedC);
        assert_eq!(InputKind::from_path("a.ir").unwrap(), InputKind::Ir);
        assert_eq!(InputKind::from_path("a.h").unwrap(), InputKind::CHeader);
        assert_eq!(InputKind::from_path("a.s").unwrap(), InputKind::Assembler);
        assert_eq!(InputKind::from_path("a.S").unwrap(), InputKind::AssemblerWithCpp);
        assert_eq!(InputKind::from_path("a.sx").unwrap(), InputKind::AssemblerWithCpp);
        assert_eq!(InputKind::from_path("a.o").unwrap(), InputKind::LinkerInput);
        assert_eq!(InputKind::from_path("libm.a").unwrap(), InputKind::LinkerInput);
        assert_eq!(InputKind::from_path("libm.so.6").unwrap(), InputKind::LinkerInput);
    }

    #[test]
    fn ir_enters_where_preprocessed_c_does_and_needs_no_preprocessor() {
        // It is the compiler's own output coming back in, so the phases in front of the walk
        // have already happened to it and the ones after it are the ones still to run.
        assert_eq!(InputKind::from_x_arg("ir").unwrap(), InputKind::Ir);
        assert_eq!(InputKind::Ir.as_str(), "ir");
        assert_eq!(InputKind::Ir.full_sequence(), InputKind::PreprocessedC.full_sequence());
        assert!(!InputKind::Ir.full_sequence().contains(&Phase::Preprocess));
    }

    #[test]
    fn an_input_whose_output_has_its_own_name_is_refused_rather_than_written_over() {
        // `rucc --emit=ir a.ir` would read the file and then write the result over it, and
        // what it held would be gone.
        let mut o = linux();
        o.emit = EmitKind::Ir;
        let inputs = [Input::new("a.ir")];
        let error = Plan::new(&o, &inputs, None).expect_err("expected this to be refused");
        assert!(error.message.contains("is the same as the output file"), "{error}");
        // Naming it something else is fine, and so is the same name reached through `-o`
        // being refused for the same reason.
        assert!(Plan::new(&o, &inputs, Some("b.ir")).is_ok());
        assert!(Plan::new(&o, &inputs, Some("a.ir")).is_err());
    }

    #[test]
    fn capital_s_and_small_s_are_different_languages() {
        // On a case-insensitive file system it is tempting to fold these together. They are
        // not the same: one runs the preprocessor and one does not.
        let hi = InputKind::from_path("a.S").unwrap();
        let lo = InputKind::from_path("a.s").unwrap();
        assert_ne!(hi, lo);
        assert!(hi.full_sequence().contains(&Phase::Preprocess));
        assert!(!lo.full_sequence().contains(&Phase::Preprocess));
    }

    #[test]
    fn a_cplusplus_source_says_why_rather_than_failing_at_link_time() {
        let e = InputKind::from_path("a.cpp").unwrap_err();
        assert!(format!("{e}").contains("only ever going to compile C"), "{e}");
        let e = InputKind::from_x_arg("c++").unwrap_err();
        assert!(matches!(e, XError::Unsupported(_)), "{e:?}");
    }

    #[test]
    fn a_file_with_no_extension_goes_to_the_linker() {
        assert_eq!(InputKind::from_path("crt1").unwrap(), InputKind::LinkerInput);
        assert_eq!(InputKind::from_path(".bashrc").unwrap(), InputKind::LinkerInput);
    }

    #[test]
    fn the_default_line_compiles_and_links_to_a_out() {
        let p = plan(&linux(), &["a.c"], None);
        assert_eq!(
            p.jobs[0].phases,
            vec![Phase::Preprocess, Phase::Compile, Phase::Assemble, Phase::Link]
        );
        assert_eq!(p.jobs[0].output, Output::Temporary("a.o".into()));
        let link = p.link.expect("expected a link step");
        assert_eq!(link.inputs, vec![Item::File("a.o".into())]);
        assert_eq!(link.output, "a.out");
    }

    #[test]
    fn dash_c_stops_at_the_object_and_names_it_after_the_source() {
        let mut o = linux();
        o.emit = EmitKind::Object;
        let p = plan(&o, &["src/a.c", "src/b.c"], None);
        assert!(p.link.is_none());
        assert_eq!(p.jobs[0].output, Output::File("a.o".into()));
        assert_eq!(p.jobs[1].output, Output::File("b.o".into()));
        // Next to the source is what people expect and it is not what GCC does. The object
        // lands in the current directory.
        assert_eq!(p.jobs[0].phases.last(), Some(&Phase::Assemble));
    }

    #[test]
    fn dash_e_writes_to_stdout_unless_it_is_given_a_name() {
        let mut o = linux();
        o.emit = EmitKind::Preprocessed;
        assert_eq!(plan(&o, &["a.c"], None).jobs[0].output, Output::Stdout);
        assert_eq!(plan(&o, &["a.c"], Some("a.i")).jobs[0].output, Output::File("a.i".into()));
    }

    #[test]
    fn a_name_of_one_dash_is_standard_output_and_not_a_file_called_that() {
        let mut o = linux();
        o.emit = EmitKind::Preprocessed;
        assert_eq!(plan(&o, &["a.c"], Some("-")).jobs[0].output, Output::Stdout);
        o.emit = EmitKind::Object;
        assert_eq!(plan(&o, &["a.c"], Some("-")).jobs[0].output, Output::Stdout);
        // The linker is handed the name and makes a file of it, which is gcc's behaviour and
        // is the one place the dash is not standard output.
        let p = plan(&linux(), &["a.c"], Some("-"));
        assert_eq!(p.link.expect("a link step").output, "-");
    }

    #[test]
    fn dash_s_produces_assembly_named_after_the_source() {
        let mut o = linux();
        o.emit = EmitKind::Asm;
        let p = plan(&o, &["dir/a.c"], None);
        assert_eq!(p.jobs[0].output, Output::File("a.s".into()));
        assert_eq!(p.jobs[0].phases, vec![Phase::Preprocess, Phase::Compile]);
    }

    #[test]
    fn an_already_preprocessed_file_skips_the_preprocessor() {
        let p = plan(&linux(), &["a.i"], None);
        assert_eq!(p.jobs[0].phases, vec![Phase::Compile, Phase::Assemble, Phase::Link]);
    }

    #[test]
    fn assembly_with_a_capital_s_is_preprocessed_but_not_compiled() {
        let p = plan(&linux(), &["a.S"], None);
        assert_eq!(p.jobs[0].phases, vec![Phase::Preprocess, Phase::Assemble, Phase::Link]);
        assert!(!p.jobs[0].phases.contains(&Phase::Compile));
    }

    #[test]
    fn objects_on_the_line_reach_the_linker_in_the_order_they_were_written() {
        // Link order is semantic. A plan that reorders it is a plan that produces a different
        // program, and the failure would be a missing symbol nobody could explain.
        let p = plan(&linux(), &["a.o", "b.c", "libm.a"], None);
        let link = p.link.expect("expected a link step");
        assert_eq!(
            link.inputs,
            vec![Item::File("a.o".into()), Item::File("b.o".into()), Item::File("libm.a".into()),]
        );
    }

    #[test]
    fn an_object_on_a_dash_c_line_is_a_note_rather_than_an_error() {
        // Configure scripts do this. Erroring here fails builds that work under GCC.
        let mut o = linux();
        o.emit = EmitKind::Object;
        let p = plan(&o, &["a.c", "b.o"], None);
        assert!(p.jobs[1].phases.is_empty());
        assert_eq!(p.notes.len(), 1);
        assert!(p.notes[0].contains("linker input unused"), "{:?}", p.notes);
    }

    #[test]
    fn dash_o_with_several_compilations_is_rejected() {
        let mut o = linux();
        o.emit = EmitKind::Object;
        let inputs = [Input::new("a.c"), Input::new("b.c")];
        let e = Plan::new(&o, &inputs, Some("out.o")).unwrap_err();
        assert!(e.message.contains("multiple inputs"), "{}", e.message);
    }

    #[test]
    fn dash_o_with_one_compilation_and_some_objects_is_fine() {
        // `rucc -c -o out.o a.c b.o` has exactly one thing to write, so the check above must
        // not count the object.
        let mut o = linux();
        o.emit = EmitKind::Object;
        let inputs = [Input::new("a.c"), Input::new("b.o")];
        let p = Plan::new(&o, &inputs, Some("out.o")).expect("expected a plan");
        assert_eq!(p.jobs[0].output, Output::File("out.o".into()));
    }

    #[test]
    fn dash_x_overrides_the_extension() {
        let inputs = [Input { path: "a.txt".into(), forced: Some(InputKind::C), library: false }];
        let p = Plan::new(&linux(), &inputs, None).expect("expected a plan");
        assert_eq!(p.jobs[0].kind, InputKind::C);
        assert_eq!(p.jobs[0].phases.first(), Some(&Phase::Preprocess));
    }

    #[test]
    fn windows_gets_obj_and_a_exe() {
        let o = opts("x86_64-pc-windows-msvc");
        let p = plan(&o, &["a.c"], None);
        assert_eq!(p.jobs[0].output, Output::Temporary("a.obj".into()));
        assert_eq!(p.link.expect("expected a link step").output, "a.exe");
    }

    #[test]
    fn the_intermediate_dumps_stop_where_dash_s_stops() {
        for emit in [EmitKind::Tast, EmitKind::Ir, EmitKind::MirFinal] {
            assert_eq!(last_phase(emit), Phase::Compile, "{emit:?}");
        }
    }

    #[test]
    fn each_intermediate_dump_is_a_language_of_its_own_and_gets_a_suffix_of_its_own() {
        // They all come out of the compile phase and none of them is assembly, so writing any
        // of them to `a.s` would leave a file that neither an assembler nor a reader can use.
        for (emit, name) in [
            (EmitKind::Asm, "a.s"),
            (EmitKind::Tast, "a.tast"),
            (EmitKind::Ir, "a.ir"),
            (EmitKind::MirFinal, "a.mir"),
        ] {
            let mut o = linux();
            o.emit = emit;
            assert_eq!(plan(&o, &["a.c"], None).jobs[0].output, Output::File(name.into()));
        }
    }

    #[test]
    fn an_input_that_enters_after_the_last_phase_is_a_note_rather_than_an_error() {
        // `rucc -E a.s` has nothing to preprocess. GCC carries on, and a configure script
        // that probes with a mixed input list depends on that.
        let mut o = linux();
        o.emit = EmitKind::Preprocessed;
        let p = plan(&o, &["a.c", "b.s"], None);
        assert!(p.jobs[1].phases.is_empty());
        assert_eq!(p.notes.len(), 1);
        assert!(p.notes[0].contains("after the last phase"), "{:?}", p.notes);
        // And it must not count against `-o`, because only one file is being written.
        let inputs = [Input::new("a.c"), Input::new("b.s")];
        assert!(Plan::new(&o, &inputs, Some("out.i")).is_ok());
    }

    #[test]
    fn no_inputs_is_an_error() {
        assert!(Plan::new(&linux(), &[], None).is_err());
    }

    #[test]
    fn the_rendering_says_what_will_happen() {
        let p = plan(&linux(), &["a.c", "b.o"], None);
        let text = p.render();
        assert!(
            text.contains("a.c: preprocess, compile, assemble, link -> a.o (temporary)"),
            "{text}"
        );
        assert!(text.contains("link: a.o b.o -> a.out"), "{text}");
        // The object has nothing done to it, so it appears once, in the link line.
        assert_eq!(text.matches("b.o").count(), 1, "{text}");
    }
}
