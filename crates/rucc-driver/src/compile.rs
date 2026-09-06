//! Running the front end over one file, from the bytes on disk to the typed tree.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.3, and the `M2` exit criterion in
//! `spec/17-milestones.md` that says `--emit=tast` works.
//!
//! [`preprocess`](mod@crate::preprocess) stops after phase 4 because `-E` stops there. This
//! carries on: phase 7, the parse, and the checking. It is one function rather than four composed
//! ones because of what the four share. The tokens hold interned symbols, the untyped tree holds
//! tokens, the typed tree holds the untyped tree's spans, and none of them owns the table it is
//! reading, so one [`Session`] has to outlive all of them and there has to be one place that
//! holds it.

use std::path::Path;

use rucc_base::Interner;
use rucc_codegen::coverage::Fired;
use rucc_codegen::pipeline::{self, Machine};
use rucc_diag::{Diagnostic, Severity, Span};
use rucc_lex::{Convert, Keywords, PpToken, convert};
use rucc_sema::{Checker, Context as CheckContext};
use rucc_session::{EmitKind, FileSystem, Options, Session};
use rucc_target::TargetInfo;

use crate::preprocess::render;

/// What a compilation produced, which is text for most of the kinds and bytes for one of them.
///
/// Two variants rather than a string, because an object file is not text and a `Vec<u8>` holding
/// UTF-8 for six kinds and a file format for the seventh would leave every reader guessing which
/// it had. [`Artifact::Nothing`] is what a compilation that stopped early gives back, and it is
/// not the same as an empty file: nothing is written for it at all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Artifact {
    /// The compilation stopped before it produced anything, or the kind asked for produces
    /// nothing yet.
    #[default]
    Nothing,
    /// Text, which is every kind up to and including assembly.
    Text(String),
    /// An object file, which is `-c`.
    Object(Vec<u8>),
}

impl Artifact {
    /// The bytes to write, which is nothing at all for [`Artifact::Nothing`].
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        match self {
            Artifact::Nothing => &[],
            Artifact::Text(text) => text.as_bytes(),
            Artifact::Object(bytes) => bytes,
        }
    }
}

/// What compiling one file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    /// What to write, which is nothing when the compilation failed or produced nothing.
    pub artifact: Artifact,
    /// The diagnostics, already rendered, one per element, in the order they were reported.
    pub messages: Vec<String>,
    /// How many of them were errors.
    pub errors: u32,
    /// Which lowering rules this file fired, for `-Zrule-coverage`.
    ///
    /// Empty for a compilation that stopped before the back end, which every kind up to and
    /// including `--emit=ir` does. That is not the same as a rule set nothing reaches and the
    /// caller unions these rather than reading one, so a file that fired nothing adds nothing.
    pub fired: Fired,
    /// What `-fdump-ir=` asked to see, in the order the passes ran.
    ///
    /// The optimizer does not write files, because nothing below the driver in
    /// `spec/18-package-layout.md` knows what a file is, so the text comes back here and the
    /// caller decides where it goes.
    pub dumps: Vec<rucc_opt::Dump>,
    /// What `-fopt-info` asked to hear, already rendered, one remark per line.
    ///
    /// Empty when the flag was not given, and also empty when it was given and no pass had
    /// anything of the kinds asked for to say. Those two are the same text and different facts,
    /// which is why a misspelled keyword is an error rather than a quiet nothing.
    pub remarks: String,
}

impl Compiled {
    /// Whether anything went wrong badly enough that the output should not be used.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.errors > 0
    }

    /// The text that was produced, and the empty string for anything that is not text.
    ///
    /// A caller that asked for one of the text kinds knows which it asked for, so this saves it
    /// matching on a variant it has already ruled out.
    #[must_use]
    pub fn text(&self) -> &str {
        match &self.artifact {
            Artifact::Text(text) => text,
            _ => "",
        }
    }
}

/// Compiles one file as far as `opts.emit` asks for and renders the result.
///
/// `name` is the path as the user wrote it, which is the name every diagnostic about the file
/// uses. Every kind but the executable produces something today, and that one runs the same front
/// end and gives back nothing, so that a file with a mistake in it is reported the same way
/// whichever kind was asked for, rather than compiling silently until the part that is written
/// notices.
///
/// The checking is skipped when the parse reported an error. The two poisoning rules mean a
/// diagnosed expression produces no further complaints, but a declaration the parser had to skip
/// past leaves no declaration behind at all, and every later use of that name would be reported
/// as undeclared. One mistake is worth one message.
#[must_use]
pub fn compile(opts: &Options, name: &str, fs: &dyn FileSystem) -> Compiled {
    let mut sess = Session::new(opts.clone());
    // Before anything else interns a name. The keyword symbols have to be one unbroken run for
    // a lookup to be a subtraction, and the preprocessor interns every identifier it reads, so
    // building this after the expansion would mean building it after `char` had been seen.
    let keywords = Keywords::new(&mut sess.interner, opts.std, opts.gnu_extensions);
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    // Filled in by the back end when there is one, and empty for every kind that stops before it.
    let mut fired = Fired::new();
    // Filled in by the optimizer, and only when `-fdump-ir=` asked for something.
    let mut dumps = Vec::new();
    let mut remarks = String::new();

    let bytes = match fs.read(Path::new(name)) {
        Ok(bytes) => bytes,
        Err(e) => return failure(format!("{name}: {e}")),
    };
    let Ok(file) = sess.sources.add_shared(name, bytes, None) else {
        return failure(format!("{name}: the source map has no room left for this file"));
    };

    // Phases 1 to 4. The expanded stream is turned into pp-tokens straight away, because the
    // include context borrows the source map that rendering a diagnostic reads and the borrow
    // has to end before anything is rendered.
    let mut pp = rucc_pp::Preprocessor::new();
    let predef = rucc_pp::Predef::for_options(opts);
    let expanded: Vec<PpToken> = {
        let mut cx = rucc_pp::Context::new(&mut sess.interner, &mut sess.sources, fs, &opts.search);
        cx.lex = rucc_lex::Options::for_dialect(opts.std, opts.gnu_extensions);
        if pp.predefine(&sess.target, &predef, &mut cx).is_err() {
            return failure(format!("{name}: the source map has no room for the built in macros"));
        }
        pp.run(file, &mut cx).iter().map(|token| token.to_pp()).collect()
    };
    diagnostics.extend(pp.take_diagnostics());

    // Phase 7, which is where a spelling becomes a keyword and a preprocessing number becomes
    // a constant of a type.
    let cx = Convert {
        keywords: &keywords,
        interner: &sess.interner,
        target: &sess.target,
        std: opts.std,
        gnu: opts.gnu_extensions,
        pedantic: opts.pedantic,
    };
    let (tokens, complaints) = convert(&expanded, &cx);
    diagnostics.extend(complaints);

    let parsed = rucc_parse::parse(
        &tokens,
        rucc_parse::Context {
            interner: &sess.interner,
            std: opts.std,
            gnu: opts.gnu_extensions,
            pedantic: opts.pedantic,
            error_limit: opts.error_limit as usize,
        },
    );
    let parse_failed = parsed.diagnostics.iter().any(|d| d.severity.is_fatal());
    diagnostics.extend(parsed.diagnostics);

    let mut artifact = Artifact::Nothing;
    // Zero when nothing instruments, which is the truthful summary of a file built without
    // `-fsafety`: no checks went in, so none is standing, and every call it makes is unmodelled.
    let mut instrumented = Instrumented::default();
    if !parse_failed {
        let mut checker = Checker::new(
            &parsed.ast,
            CheckContext {
                names: &sess.interner,
                target: &sess.target,
                std: opts.std,
                gnu: opts.gnu_extensions,
                pedantic: opts.pedantic,
                error_limit: opts.error_limit as usize,
                // A freestanding program has no C library, so a name that is the library's
                // everywhere else is the program's own here and means whatever it defined.
                builtins: opts.builtins && opts.hosted,
                no_builtin: &opts.no_builtin,
            },
        );
        checker.check_unit();
        let checked = checker.finish();
        if !checked.failed() {
            match opts.emit {
                EmitKind::Tast => {
                    artifact = Artifact::Text(rucc_sema::print(
                        &checked.tast,
                        &checked.types,
                        &sess.interner,
                    ));
                }
                EmitKind::Ir
                | EmitKind::MirFinal
                | EmitKind::Asm
                | EmitKind::Object
                | EmitKind::Executable
                | EmitKind::SafetySummary => {
                    let mut lowered = rucc_lower::lower(
                        name,
                        rucc_lower::Context {
                            tast: &checked.tast,
                            types: &checked.types,
                            target: &sess.target,
                            names: &mut sess.interner,
                        },
                    );
                    // The walk reports what it cannot build, and what it did build is printed
                    // anyway: a file with one construct missing from it is more use to read
                    // than nothing at all, and the errors are what stop it being compiled.
                    let failed = lowered.diagnostics.iter().any(|d| d.severity.is_fatal());
                    if !failed {
                        // The verifier runs on everything the walk builds, always. It is the
                        // one check that a bug in the walk cannot talk its way past, and a
                        // wrong instruction found here costs a message rather than an hour
                        // in front of a debugger over the assembly it turned into.
                        if let Err(errors) = rucc_ir::verify(&lowered.module, &sess.interner) {
                            for error in errors {
                                diagnostics.push(internal(&format!("invalid IR, {error}")));
                            }
                        } else if let Err(complaints) =
                            instrument(&mut lowered.module, &mut sess.interner, opts)
                                .map(|done| instrumented = done)
                        {
                            diagnostics.extend(complaints);
                        } else if let Err(complaints) = optimize(
                            &mut lowered.module,
                            &sess.interner,
                            opts,
                            name,
                            &mut dumps,
                            &mut remarks,
                        ) {
                            diagnostics.extend(complaints);
                        } else if opts.emit == EmitKind::SafetySummary {
                            // After the optimizer, because the number that matters is how many
                            // checks are still standing and there is no way to know that before it
                            // has run. Before the back end, because the back end turns a check into
                            // a call and a summary of calls is not a summary of checks.
                            artifact = Artifact::Text(
                                rucc_safety::summarize(
                                    &lowered.module,
                                    &sess.interner,
                                    name,
                                    opts.safety.as_str(),
                                    instrumented.checks,
                                    instrumented.interposed,
                                    instrumented.crossings,
                                )
                                .render(),
                            );
                        } else if opts.emit == EmitKind::Ir {
                            // After the optimizer rather than before it, so that `--emit=ir -O2`
                            // is the IR the back end will be given rather than the IR it would
                            // have been given at `-O0`. There is no other way to see what a pass
                            // did without reading the assembly it turned into.
                            artifact =
                                Artifact::Text(rucc_ir::print(&lowered.module, &sess.interner));
                        } else {
                            // The back end, which is every pass after the IR and which is
                            // where a construct nothing has a rule for is finally noticed.
                            match generate(
                                &mut lowered.module,
                                &mut sess.interner,
                                &sess.target,
                                opts,
                                &mut fired,
                            ) {
                                Ok(made) => artifact = made,
                                Err(complaints) => diagnostics.extend(complaints),
                            }
                        }
                    }
                    diagnostics.extend(lowered.diagnostics);
                }
                _ => {}
            }
        }
        diagnostics.extend(checked.diagnostics);
    }

    let mut messages = Vec::with_capacity(diagnostics.len());
    let mut errors = 0;
    for diag in &diagnostics {
        // `-w` drops the warning here rather than at the several hundred places one is raised,
        // and it drops it before the count, so `-w -Werror` compiles. A warning that was never
        // raised is not a warning there is anything to promote.
        if !opts.warnings && diag.severity == Severity::Warning {
            continue;
        }
        if diag.severity.is_fatal()
            || (diag.severity == Severity::Warning && opts.warnings_are_errors)
        {
            errors += 1;
        }
        messages.push(render(diag, &sess.sources, opts.warnings_are_errors));
    }
    if errors > 0 {
        // A tree built from a file that did not compile is not a tree anything should read.
        artifact = Artifact::Nothing;
    }
    // Kept even when the compilation failed, because a rule that fired did fire and a report about
    // which rules a corpus reaches should not lose the ones a file with a mistake in it reached.
    Compiled { artifact, messages, errors, fired, dumps, remarks }
}

/// Reads one file of IR, checks it, and prints it back.
///
/// This is the compiler's own textual IR arriving as an input rather than leaving as an output,
/// which is what makes the round trip in the M2 exit criterion something to run rather than
/// something to believe: what the printer wrote is read back, verified, and written again, and
/// the two files are either the same bytes or they are not.
///
/// The verifier runs here for the reason it runs after the walk. A module that was printed by
/// this compiler has been through it once already, and one that a person edited has not.
#[must_use]
pub fn compile_ir(opts: &Options, name: &str, fs: &dyn FileSystem) -> Compiled {
    let mut sess = Session::new(opts.clone());
    if opts.emit != EmitKind::Ir {
        return failure(format!(
            "{name}: an input of IR can only be emitted as IR, and `--emit={}` asks for what \
             the C in front of it became",
            opts.emit.as_str()
        ));
    }
    let bytes = match fs.read(Path::new(name)) {
        Ok(bytes) => bytes,
        Err(e) => return failure(format!("{name}: {e}")),
    };
    let Ok(text) = std::str::from_utf8(bytes.as_slice()) else {
        return failure(format!("{name}: this is not text, so it is not IR"));
    };

    let module = match rucc_ir::parse(text, &mut sess.interner) {
        Ok(module) => module,
        Err(error) => {
            return failure(format!("{name}:{}: {}", error.line, error.message));
        }
    };
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    if let Err(errors) = rucc_ir::verify(&module, &sess.interner) {
        for error in errors {
            diagnostics.push(invalid(&format!("invalid IR, {error}")));
        }
    }
    let mut messages = Vec::with_capacity(diagnostics.len());
    for diag in &diagnostics {
        messages.push(render(diag, &sess.sources, opts.warnings_are_errors));
    }
    let errors = u32::try_from(messages.len()).unwrap_or(u32::MAX);
    let artifact = if errors > 0 {
        Artifact::Nothing
    } else {
        Artifact::Text(rucc_ir::print(&module, &sess.interner))
    };
    // Nothing here reaches the back end, so no rule fired and there is nothing to record.
    Compiled {
        artifact,
        messages,
        errors,
        fired: Fired::new(),
        dumps: Vec::new(),
        remarks: String::new(),
    }
}

/// Puts the memory safety checks in and redirects the calls that cross the boundary, when
/// `-fsafety=` asked for them.
///
/// Between the walk and the optimizer, which is where section 15.3 of
/// `spec/safe-memory/15-integration.md` puts it and which is the whole design in one line: the
/// checks go in while the addresses the program computes still exist, and the optimizer then
/// discharges the ones it can prove. Every sanitizer that came before instruments after the
/// optimizer so that its checks cannot be deleted, and pays for all of them forever.
///
/// The calls to the C library are redirected here too, and in the same window and for a related
/// reason. `spec/safe-memory/10-boundaries.md` section 10.3 wants a `memcpy` modelled by a wrapper
/// that performs the judgements, and `rucc_safety::wrap` is why that has to happen before the
/// optimizer sees the call rather than after.
///
/// The verifier runs again afterwards, for the reason it runs after the walk. This pass rewrites
/// every function in the module, and a pass that produced IR nothing else accepts should say so
/// here rather than in the assembly it turned into.
///
/// # Errors
///
/// When the inserted checks left the module in a state the verifier refuses, which is a bug in
/// this compiler and not in the program being compiled.
fn instrument(
    module: &mut rucc_ir::Module,
    names: &mut Interner,
    opts: &Options,
) -> Result<Instrumented, Vec<Diagnostic>> {
    if !opts.safety.instruments() {
        return Ok(Instrumented::default());
    }
    let checks = rucc_safety::run(module);
    // Before the optimizer rather than beside the check lowering, which is what
    // `rucc_safety::wrap` argues out: `memcpy` is a name an optimizer knows things about, and a
    // pass that turns a short copy into a pair of loads and stores would leave behind accesses the
    // check insertion has already finished walking past.
    let interposed = rucc_safety::redirect(module, names);
    // After the redirection, so that a call this build models with a wrapper is not also counted
    // as a crossing it did not model.
    let crossings = rucc_safety::witness(module, names);
    match rucc_ir::verify(module, names) {
        Ok(()) => Ok(Instrumented { checks, interposed, crossings }),
        Err(errors) => Err(errors
            .iter()
            .map(|e| internal(&format!("invalid IR after check insertion, {e}")))
            .collect()),
    }
}

/// What the instrumentation did, which nothing but the summary reads.
///
/// Carried out of [`instrument`] rather than recovered from the module afterwards because neither
/// number survives the optimizer: a check that was discharged leaves nothing behind saying it was
/// ever there, and a call that was pointed at a wrapper looks like a call that always named one.
#[derive(Clone, Copy, Debug, Default)]
struct Instrumented {
    /// How many checks of each class went in.
    checks: rucc_safety::Counts,
    /// How many calls were pointed at an interposition wrapper.
    interposed: usize,
    /// How many places a pointer crosses to or from code this build did not instrument.
    crossings: rucc_safety::Sites,
}

/// Runs the optimizer over the module, and collects whatever the dumps asked for.
///
/// The level chooses a pipeline, the `-f` flags edit it, and at `-O0` there is nothing in it, so
/// this is a walk over an empty list rather than a branch on the level. See section 9.1 of
/// `spec/09-optimizer.md` for why the pipelines are written out rather than assembled.
///
/// # Errors
///
/// When a pass left the module in a state the verifier refuses, which is a bug in the pass and
/// not in the program being compiled, so it is reported as an internal error the way a bad
/// lowering is.
fn optimize(
    module: &mut rucc_ir::Module,
    names: &Interner,
    opts: &Options,
    file: &str,
    dumps: &mut Vec<rucc_opt::Dump>,
    remarks: &mut String,
) -> Result<(), Vec<Diagnostic>> {
    let mut settings = rucc_opt::Options::for_level(opts.opt_level);
    settings.toggles.clone_from(&opts.passes);
    settings.fuel = opts.pass_fuel.iter().cloned().collect();
    settings.global_fuel = opts.pass_fuel_global;
    settings.verify |= opts.verify_each;
    for (on, spec) in &opts.pass_gates {
        // Same argument as the dumps below: every spelling in here was checked while the
        // arguments were parsed, so a rejection now is this compiler disagreeing with itself.
        if let Err(why) = settings.gates.add(*on, spec) {
            return Err(vec![internal(&why)]);
        }
    }
    for spec in &opts.dump_ir {
        // Every spelling in here was checked while the arguments were parsed, so a rejection
        // now is this compiler disagreeing with itself rather than the command line being wrong.
        if let Err(why) = settings.dumps.add(spec) {
            return Err(vec![internal(&why)]);
        }
    }
    let mut wants = rucc_opt::Wants::none();
    for spec in &opts.opt_info {
        // Same argument as the dumps above: every spelling was checked while the arguments were
        // parsed, so a rejection now is the compiler disagreeing with itself.
        if let Err(why) = wants.add(spec) {
            return Err(vec![internal(&why)]);
        }
    }
    let report = rucc_opt::run(module, names, &settings);
    remarks.push_str(&rucc_opt::optinfo::render(file, &report, names, wants));
    dumps.extend(report.dumps);
    match report.broke.is_empty() {
        true => Ok(()),
        false => Err(report.broke.iter().map(|why| internal(why)).collect()),
    }
}

/// Runs the back end over every function in `module` and writes what came out.
///
/// One machine function per definition in the module, in the order the module holds them, every
/// register physical and every frame offset a constant. A declaration has no body and is skipped,
/// because there is nothing in it to compile.
///
/// What the last step is, is the only thing `--emit=mir-final`, `-S` and `-c` disagree about. The
/// three read the same functions and differ in whether they are printed as machine IR, printed as
/// assembly, or encoded and put in a file, which is the point of section 11.1 of
/// `spec/11-asm-objects-debug.md`: a listing that disagrees with the object file beside it is
/// worse than no listing, and the way to make that impossible is to have one description of an
/// instruction and two ways of writing it down.
///
/// # Errors
///
/// One diagnostic per function the back end could not compile, or one about the target when no
/// back end covers it at all. Every function is attempted rather than stopping at the first, so a
/// file with three constructs missing from the rule set reports three rather than one at a time.
fn generate(
    module: &mut rucc_ir::Module,
    names: &mut Interner,
    target: &TargetInfo,
    opts: &Options,
    fired: &mut Fired,
) -> Result<Artifact, Vec<Diagnostic>> {
    let Some(machine) = Machine::for_target(target) else {
        return Err(vec![unsupported(&format!(
            "there is no back end for {} in this compiler yet, so there is nothing to generate",
            target.triple
        ))]);
    };
    let flags = pipeline::Flags { frame_pointer: opts.frame_pointer, red_zone: opts.red_zone };

    // The checks become calls here rather than beside the insertion, because the id each one
    // carries is an index into a table and a row for a check the optimizer deleted is a row nothing
    // will ever name. Section 6.3.1 of `spec/safe-memory/06-instrumentation.md` is what this
    // eventually becomes and `rucc_safety::lower` says why it is not that yet.
    //
    // It is inside the back end rather than beside the optimizer so that `--emit=ir` still shows
    // the checks. The IR a person reads should say what the compiler decided, not how it spelled it
    // for the machine.
    if opts.safety.instruments() {
        rucc_safety::lower(module, names);
        if let Err(errors) = rucc_ir::verify(module, names) {
            return Err(errors
                .iter()
                .map(|e| internal(&format!("invalid IR after check lowering, {e}")))
                .collect());
        }
    }

    let mut funcs = Vec::new();
    let mut complaints = Vec::new();
    for id in module.funcs() {
        if module[id].is_declaration() {
            continue;
        }
        match pipeline::compile_recording(&mut module[id], names, &machine, flags, fired) {
            Ok(func) => funcs.push(func),
            Err(why) => {
                let name = names.resolve(module[id].name).to_owned();
                // The function knows where the instruction came from, so the message lands on
                // the line somebody wrote rather than on the file as a whole.
                let span = why.inst().map_or(Span::DUMMY, |inst| module[id].span(inst));
                let said = format!("cannot generate code for '{name}': {why}");
                complaints.push(unsupported_at(&said, span));
            }
        }
    }
    if !complaints.is_empty() {
        return Err(complaints);
    }
    // The variables the file defines, which go through the back end the way the functions did not:
    // there is nothing in a variable to select instructions for, so the module is what says what
    // one is right up to the point where it is written down.
    let globals = match opts.emit {
        EmitKind::Asm | EmitKind::Object | EmitKind::Executable => {
            rucc_asm::globals(module, names).map_err(refused)?
        }
        _ => rucc_asm::Globals::default(),
    };
    // A failure in either of the last two is a bug here rather than a program this compiler is
    // behind on, because every instruction in a function that got this far came out of the same
    // description both of them read and every register in it has been allocated.
    match opts.emit {
        EmitKind::Asm => {
            rucc_asm::print(&funcs, &globals, names, target).map(Artifact::Text).map_err(refused)
        }
        // An executable is an object as far as this gets: one is what each file of a link
        // contributes, and the linker is what turns them into the other.
        EmitKind::Object | EmitKind::Executable => {
            let text = rucc_asm::assemble(&funcs, names, target).map_err(refused)?;
            let data = globals.image();
            // A format with no writer is a target this compiler is behind on and anything else
            // the writer refused is a bug here, and the two are not the same news to get.
            rucc_object::write(&text, &data, target).map(Artifact::Object).map_err(
                |why| match why {
                    rucc_object::Error::Format { .. } => vec![unsupported(&why.to_string())],
                    rucc_object::Error::Refused { .. } => vec![internal(&why.to_string())],
                },
            )
        }
        _ => Ok(Artifact::Text(rucc_mir::print(&funcs, names, target.regs))),
    }
}

/// What the assembler said, as the kind of news it is.
///
/// One of these is about a program and the rest are about this compiler. A thread-local variable
/// is valid C that the back end does not build yet, and everything else the assembler refuses is
/// something that should never have reached it.
fn refused(why: rucc_asm::Error) -> Vec<Diagnostic> {
    match why {
        rucc_asm::Error::Thread { .. } => vec![unsupported(&why.to_string())],
        _ => vec![internal(&why.to_string())],
    }
}

/// A diagnostic about a program this compiler is not finished enough to compile.
///
/// Not an internal error, because nothing here is wrong: the program is valid C and the part of
/// the back end that would handle it has not been written. The note says so, so that a report
/// about one of these is filed against the milestone rather than as a miscompilation.
fn unsupported(message: &str) -> Diagnostic {
    unsupported_at(message, Span::DUMMY)
}

/// The same, about somewhere in the file rather than about the file.
///
/// The note names the issue tracker rather than `spec/17-milestones.md`, which is a document
/// about the plan: a reader who follows it wants to know whether the construct in front of them
/// is already written down as work, and the milestone list does not answer that.
fn unsupported_at(message: &str, span: Span) -> Diagnostic {
    Diagnostic::error(message.to_owned(), span)
        .with_code("E0653")
        .note("this construct is not lowered yet, see https://github.com/tamnd/rucc/issues", span)
}

/// A diagnostic about IR that was handed to us rather than built by us.
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::error(message.to_owned(), Span::DUMMY).with_code("E0661")
}

/// A diagnostic about this compiler rather than about the program it was given.
fn internal(message: &str) -> Diagnostic {
    Diagnostic::error(format!("internal error: {message}"), Span::DUMMY)
        .with_code("E0652")
        .note("this is a bug in rucc rather than in the program, please report it", Span::DUMMY)
}

/// A result that is nothing but one message, for the failures that happen before there is
/// anything to compile.
fn failure(message: String) -> Compiled {
    Compiled {
        artifact: Artifact::Nothing,
        messages: vec![format!("rucc: error: {message}")],
        errors: 1,
        fired: Fired::new(),
        dumps: Vec::new(),
        remarks: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use rucc_session::{MemoryFileSystem, Std};
    use rucc_target::Triple;

    use super::*;

    fn options() -> Options {
        let mut opts = Options::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        opts.emit = EmitKind::Tast;
        opts
    }

    fn run(opts: &Options, source: &str) -> Compiled {
        let mut fs = MemoryFileSystem::new();
        fs.insert("/main.c", source.to_owned().into_bytes());
        compile(opts, "/main.c", &fs)
    }

    /// Options with the compiler's own headers on the search path and nothing else, which is
    /// what a freestanding compilation is. There is no file system underneath these tests,
    /// so a header that reached for one would fail to resolve and say so.
    fn freestanding() -> Options {
        let mut opts = options();
        opts.hosted = false;
        opts.search.push_system(rucc_session::runtime::DIR);
        opts
    }

    /// The typed tree of a freestanding `source`, insisting that it compiled cleanly.
    fn shipped(source: &str) -> String {
        let result = run(&freestanding(), source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text().to_owned()
    }

    /// The typed tree of `source`, insisting that it compiled cleanly.
    fn tast(source: &str) -> String {
        let result = run(&options(), source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text().to_owned()
    }

    #[test]
    fn the_shipped_stdarg_declares_a_list_and_the_four_operators() {
        let text = shipped(concat!(
            "#include <stdarg.h>\n",
            "int sum(int n, ...) {\n",
            "  va_list ap, copy;\n",
            "  va_start(ap, n);\n",
            "  va_copy(copy, ap);\n",
            "  int total = va_arg(ap, int) + va_arg(copy, int);\n",
            "  va_end(ap);\n",
            "  va_end(copy);\n",
            "  return total;\n",
            "}\n",
        ));
        assert!(text.contains("va-start"), "{text}");
        assert!(text.contains("va-copy"), "{text}");
        assert!(text.contains("va-arg"), "{text}");
        assert!(text.contains("va-end"), "{text}");
    }

    /// glibc includes `<stdarg.h>` this way from every header that declares a `vprintf`, and
    /// what it wants is the type without the four macro names. Answering the whole header
    /// would put `va_start` in the way of a program that has its own.
    #[test]
    fn stdarg_hands_out_the_type_alone_when_that_is_all_that_was_asked_for() {
        let text = shipped(concat!(
            "#define __need___va_list\n",
            "#include <stdarg.h>\n",
            "int vprint(const char *f, __gnuc_va_list ap);\n",
            "#ifdef va_start\n",
            "#error va_start should not be defined\n",
            "#endif\n",
            "#ifdef _VA_LIST_DEFINED\n",
            "#error va_list should not have been made\n",
            "#endif\n",
        ));
        assert!(text.contains("vprint"), "{text}");
    }

    /// The same protocol on `<stddef.h>`, which glibc uses far more heavily: `<stdio.h>` asks
    /// for `size_t` and `NULL` and would be wrong to receive `offsetof` as well.
    #[test]
    fn stddef_answers_one_piece_at_a_time_and_the_next_request_still_gets_through() {
        let text = shipped(concat!(
            "#define __need_size_t\n",
            "#include <stddef.h>\n",
            "#ifdef offsetof\n",
            "#error offsetof should not be defined yet\n",
            "#endif\n",
            "#define __need_ptrdiff_t\n",
            "#include <stddef.h>\n",
            "#include <stddef.h>\n",
            "size_t a;\n",
            "ptrdiff_t b;\n",
            "wchar_t c;\n",
            "max_align_t d;\n",
            "void *e = NULL;\n",
            "struct P { int x; long y; };\n",
            "size_t f = offsetof(struct P, y);\n",
        ));
        assert!(text.contains("decl #0 a : unsigned long"), "{text}");
        assert!(text.contains("decl #1 b : long"), "{text}");
    }

    #[test]
    fn the_shipped_limits_and_float_are_the_targets_own_answers() {
        let text = shipped(concat!(
            "#include <limits.h>\n",
            "#include <float.h>\n",
            "int bits = CHAR_BIT;\n",
            "long big = LONG_MAX;\n",
            "int low = INT_MIN;\n",
            "int radix = FLT_RADIX;\n",
            "int digits = DBL_MANT_DIG;\n",
        ));
        assert!(text.contains("const 8 : int"), "{text}");
        assert!(text.contains("const 9223372036854775807 : long"), "{text}");
        assert!(text.contains("const 2 : int"), "{text}");
        assert!(text.contains("const 53 : int"), "{text}");
    }

    /// Freestanding, so there is no library header to chain to and `<stdint.h>` writes the
    /// whole set out itself. The widths are the ones the target picked, which is the only
    /// reason this header is the compiler's.
    #[test]
    fn the_shipped_stdint_writes_the_whole_set_when_there_is_no_library_to_defer_to() {
        let text = shipped(concat!(
            "#include <stdint.h>\n",
            "int64_t a = INT64_C(1);\n",
            "uint_least16_t b;\n",
            "intptr_t c;\n",
            "uintmax_t d = UINTMAX_MAX;\n",
            "int wide = sizeof(int_fast64_t);\n",
        ));
        assert!(text.contains("decl #0 a : long"), "{text}");
        assert!(text.contains("decl #1 b : unsigned short"), "{text}");
        assert!(text.contains("decl #2 c : long"), "{text}");
    }

    #[test]
    fn the_three_formality_headers_still_have_to_work() {
        let text = shipped(concat!(
            "#include <stdbool.h>\n",
            "#include <stdalign.h>\n",
            "#include <iso646.h>\n",
            "#include <stdnoreturn.h>\n",
            "int t = true and not false;\n",
            "_Alignas(16) char buf[16];\n",
            "int a = alignof(long);\n",
        ));
        assert!(text.contains("decl #0 t : int"), "{text}");
        assert!(text.contains("const 8 : unsigned long"), "{text}");
    }

    /// Including everything twice has to change nothing, because that is what happens in any
    /// program large enough to matter and a guard that is wrong shows up nowhere else.
    #[test]
    fn every_shipped_header_can_be_included_twice() {
        let mut source = String::new();
        for _ in 0..2 {
            for name in rucc_session::runtime::names() {
                source.push_str(&format!("#include <{name}>\n"));
            }
        }
        source.push_str("int x;\n");
        let text = shipped(&source);
        assert!(text.starts_with("decl #0 x : int"), "{text}");
    }

    #[test]
    fn a_file_that_is_not_there_says_so_and_produces_nothing() {
        let fs = MemoryFileSystem::new();
        let result = compile(&options(), "/nope.c", &fs);
        assert!(result.failed());
        assert!(result.messages[0].contains("/nope.c"), "{:?}", result.messages);
        assert!(result.text().is_empty());
    }

    #[test]
    fn an_object_comes_out_with_its_type_its_linkage_and_how_much_of_a_definition_it_is() {
        let text = tast("int x = 1;\n");
        let expected = "\
decl #0 x : int object external static defined
  init
    +0
      const 1 : int
";
        assert_eq!(text, expected);
    }

    #[test]
    fn the_macros_are_expanded_before_anything_is_parsed() {
        // The whole pipeline in one line. The bound came out of a macro, so it was expanded,
        // converted from a preprocessing number to a constant of a type, parsed as an
        // expression, and folded to the number the array type carries.
        let text = tast("#define N 2\nint a[N];\n");
        assert!(text.starts_with("decl #0 a : int[2] object external static tentative"), "{text}");
    }

    /// A pragma survives the preprocessor on purpose, since what one means is not its
    /// business, and nothing after it has a place for a `#` in the grammar. `pack` is the one
    /// the parser reads and every other line is walked past. Both spellings are here because
    /// they arrive by different routes and only one of them was ever on a line of its own in
    /// the source.
    #[test]
    fn a_pragma_is_not_a_declaration_and_the_parse_walks_past_the_ones_it_does_not_read() {
        let text = tast(concat!(
            "#pragma pack(4)\n",
            "struct s { int a; };\n",
            "#pragma pack()\n",
            "int b;\n",
            "_Pragma(\"GCC visibility push(default)\") int c;\n",
        ));
        assert!(text.contains("decl #0 b : int"), "{text}");
        assert!(text.contains("decl #1 c : int"), "{text}");
    }

    /// Every number in these two tests was read off gcc 16 on x86-64 under `-std=gnu23`
    /// rather than reasoned about, which is why they are written as assertions the program
    /// makes about itself: a compilation with no messages is every one of them holding.
    ///
    /// This half is the attributes. `packed` takes the padding out, on the record or on one
    /// member, `aligned` raises and never lowers, and the two written together are the
    /// combination that packs and then aligns the whole thing.
    #[test]
    fn the_layout_attributes_move_the_members_and_the_record_the_way_gcc_lays_them_out() {
        tast(concat!(
            "struct A { char c; int i; } __attribute__((packed));\n",
            "_Static_assert(sizeof(struct A) == 5 && _Alignof(struct A) == 1, \"A\");\n",
            "_Static_assert(__builtin_offsetof(struct A, i) == 1, \"A.i\");\n",
            // `aligned` with nothing in the parentheses is the largest alignment the target
            // has, which gcc calls BIGGEST_ALIGNMENT and which is sixteen everywhere here.
            "struct B { char c; int i; } __attribute__((aligned));\n",
            "_Static_assert(sizeof(struct B) == 16 && _Alignof(struct B) == 16, \"B\");\n",
            "struct C { char c; int i __attribute__((packed)); };\n",
            "_Static_assert(sizeof(struct C) == 5 && _Alignof(struct C) == 1, \"C\");\n",
            "_Static_assert(__builtin_offsetof(struct C, i) == 1, \"C.i\");\n",
            "struct D { char c; int i; } __attribute__((packed, aligned(4)));\n",
            "_Static_assert(sizeof(struct D) == 8 && _Alignof(struct D) == 4, \"D\");\n",
            "_Static_assert(__builtin_offsetof(struct D, i) == 1, \"D.i\");\n",
            "struct E { char c; _Alignas(8) int i; };\n",
            "_Static_assert(sizeof(struct E) == 16 && _Alignof(struct E) == 8, \"E\");\n",
            "_Static_assert(__builtin_offsetof(struct E, i) == 8, \"E.i\");\n",
            "struct F { char c; int i __attribute__((aligned(8))); };\n",
            "_Static_assert(sizeof(struct F) == 16 && _Alignof(struct F) == 8, \"F\");\n",
            // Two the record already had, so the attribute asks for nothing new, and two
            // where four was already there, so the attribute is ignored rather than obeyed.
            "struct G { char c; short s; } __attribute__((aligned(2)));\n",
            "_Static_assert(sizeof(struct G) == 4 && _Alignof(struct G) == 2, \"G\");\n",
            "struct H { char c; int i; } __attribute__((aligned(2)));\n",
            "_Static_assert(sizeof(struct H) == 8 && _Alignof(struct H) == 4, \"H\");\n",
            // `packed` on a member takes the padding out in front of that member alone, so on
            // the first one it does nothing and on the second one it does all of it.
            "struct I { [[gnu::packed]] char c; int i; };\n",
            "_Static_assert(sizeof(struct I) == 8 && _Alignof(struct I) == 4, \"I\");\n",
            "struct J { char c; [[gnu::packed]] int i; };\n",
            "_Static_assert(sizeof(struct J) == 5 && _Alignof(struct J) == 1, \"J\");\n",
            "struct M { char c; int i : 5; int j : 20; } __attribute__((packed));\n",
            "_Static_assert(sizeof(struct M) == 5 && _Alignof(struct M) == 1, \"M\");\n",
            "struct N { char c; long long l; } __attribute__((aligned(32)));\n",
            "_Static_assert(sizeof(struct N) == 32 && _Alignof(struct N) == 32, \"N\");\n",
            "union L { char c; int i; } __attribute__((packed));\n",
            "_Static_assert(sizeof(union L) == 4 && _Alignof(union L) == 1, \"L\");\n",
            // The armoured spellings, which are the ones a system header writes, since a
            // program is entitled to a macro called `packed` and is not entitled to one called
            // `__packed__`. The two names are one attribute and the layout is the same one.
            "struct O { char c; int i; } __attribute__((__packed__));\n",
            "_Static_assert(sizeof(struct O) == 5 && _Alignof(struct O) == 1, \"O\");\n",
            "struct P { char c; int i; } __attribute__((__aligned__(8)));\n",
            "_Static_assert(sizeof(struct P) == 8 && _Alignof(struct P) == 8, \"P\");\n",
        ));
    }

    /// The same attribute on a declaration rather than on a type, which asks that this object or
    /// this function be at a multiple of that, and which is where a program that has to hand a
    /// buffer to hardware or keep two counters off one cache line writes it.
    ///
    /// A raise and never a lower, which is the one place it does not agree with `_Alignas`: below
    /// what the type already has, `_Alignas` is a constraint violation and this is ignored without
    /// a word. `__alignof__` of the object answers what the object got and not what its type has,
    /// because that is the question a program asking it is asking.
    #[test]
    fn the_aligned_attribute_on_a_declaration_raises_what_that_one_object_is_aligned_to() {
        tast(concat!(
            "int v __attribute__((aligned(64)));\n",
            "_Static_assert(__alignof__(v) == 64, \"v\");\n",
            // Written on the specifiers rather than after the declarator, which asks the same
            // thing and is the spelling a header is more likely to use.
            "__attribute__((aligned(32))) int w;\n",
            "_Static_assert(__alignof__(w) == 32, \"w\");\n",
            "[[gnu::aligned(16)]] int x;\n",
            "_Static_assert(__alignof__(x) == 16, \"x\");\n",
            // Two below the four an `int` already has, so nothing is asked for and nothing is
            // said, and the type still answers for the object.
            "int y __attribute__((aligned(2)));\n",
            "_Static_assert(__alignof__(y) == 4, \"y\");\n",
            // A local, which is the same question one scope down.
            "void f(void) { int a __attribute__((aligned(128)));\n",
            "_Static_assert(__alignof__(a) == 128, \"a\"); (void)a; }\n",
            // The type is untouched by any of it: `aligned` on a declaration says where that
            // declaration goes and says nothing about every other `int` in the program.
            "_Static_assert(__alignof__(int) == 4, \"int\");\n",
            // A function, which has no alignment of its own for this to be measured against and
            // takes whatever was asked for.
            "void g(void) __attribute__((aligned(256)));\n",
            "void g(void) {}\n",
            "_Static_assert(__alignof__(g) == 256, \"g\");\n",
        ));
    }

    /// And what the object file says, which is the half that makes the answer above true. A
    /// function is at a fixed offset inside the text section, so it is at a multiple of two
    /// hundred and fifty six only if the section is at one too.
    #[test]
    fn what_a_declaration_asked_to_be_aligned_to_is_what_the_assembler_is_told() {
        let text = asm(concat!(
            "int v __attribute__((aligned(64)));\n",
            "void g(void) __attribute__((aligned(256)));\n",
            "void g(void) {}\n",
            "void plain(void) {}\n",
        ));
        assert!(text.contains("\t.p2align\t6\n\t.type\tv, @object\n"), "{text}");
        assert!(text.contains("\t.p2align\t8, 0x90\n\t.globl\tg\n"), "{text}");
        assert!(text.contains("\t.p2align\t4, 0x90\n\t.globl\tplain\n"), "{text}");
    }

    /// And the one position where the attribute means something else. On a declaration it raises
    /// what that one object is aligned to, and on a typedef it says what the type is aligned to,
    /// which gcc lets it lower as well: `typedef int L __attribute__((aligned(2)))` really is an
    /// `int` at a multiple of two and a record with one in it really is smaller for it.
    ///
    /// The size is left alone, which is gcc's answer rather than an omission here. An aligned
    /// typedef whose alignment is larger than what it stands for keeps the size it stands for,
    /// and gcc refuses an array of one rather than padding the elements out to fit.
    #[test]
    fn an_aligned_typedef_says_what_an_object_of_it_is_aligned_to_and_may_lower_it() {
        tast(concat!(
            "typedef int L __attribute__((aligned(2)));\n",
            "_Static_assert(__alignof__(L) == 2, \"L\");\n",
            "_Static_assert(_Alignof(L) == 2, \"L alignof\");\n",
            // Below what an `int` has, which is the half a declaration cannot ask for.
            "_Static_assert(sizeof(L) == 4, \"L size\");\n",
            "struct T { char c; L x; };\n",
            "_Static_assert(sizeof(struct T) == 6, \"T\");\n",
            "_Static_assert(__builtin_offsetof(struct T, x) == 2, \"T.x\");\n",
            // And upwards, which is the ordinary direction and the one a header writes.
            "typedef int H __attribute__((aligned(16)));\n",
            "_Static_assert(__alignof__(H) == 16, \"H\");\n",
            "_Static_assert(sizeof(H) == 4, \"H size\");\n",
            "struct U { char c; H x; };\n",
            "_Static_assert(sizeof(struct U) == 32, \"U\");\n",
            "_Static_assert(__builtin_offsetof(struct U, x) == 16, \"U.x\");\n",
            // A typedef of a typedef, where the nearer one is the one the declaration was
            // written with and is the one that answers.
            "typedef L M __attribute__((aligned(8)));\n",
            "_Static_assert(__alignof__(M) == 8, \"M\");\n",
            // And one that asked for nothing, which still has whatever the one behind it asked
            // for because it is the same type spelled again.
            "typedef L N;\n",
            "_Static_assert(__alignof__(N) == 2, \"N\");\n",
            // The type it stands for is untouched by any of it.
            "_Static_assert(__alignof__(int) == 4, \"int\");\n",
        ));
        let text = asm(concat!(
            "typedef int L __attribute__((aligned(2)));\n",
            "typedef int H __attribute__((aligned(16)));\n",
            "L low;\n",
            "H high;\n",
        ));
        assert!(text.contains("\t.p2align\t1\n\t.type\tlow, @object\n"), "{text}");
        assert!(text.contains("\t.p2align\t4\n\t.type\thigh, @object\n"), "{text}");
    }

    /// The attribute that builds a type rather than changing a layout. `vector_size(n)` says the
    /// declared type is `n` bytes of what was written, taken as lanes, and every operator over
    /// one is that operator over each lane.
    ///
    /// The size is in bytes and not in lanes, which is the part a reader gets backwards: sixteen
    /// of `int` is four lanes and sixteen of `char` is sixteen. A vector is aligned to its own
    /// size, which is what a machine that has the registers wants and what gcc gives one here.
    #[test]
    fn the_vector_size_attribute_builds_a_type_of_lanes_and_measures_it_in_bytes() {
        tast(concat!(
            "typedef int __attribute__((vector_size(16))) v4si;\n",
            "_Static_assert(sizeof(v4si) == 16 && _Alignof(v4si) == 16, \"v4si\");\n",
            "typedef char __attribute__((vector_size(16))) v16qi;\n",
            "_Static_assert(sizeof(v16qi) == 16, \"v16qi\");\n",
            // One lane, which is a power of two and is a vector rather than the type it was
            // written on: the operators it takes are the vector's and not the scalar's.
            "typedef int __attribute__((vector_size(4))) v1si;\n",
            "_Static_assert(sizeof(v1si) == 4, \"v1si\");\n",
            // The armoured spelling and the bracket one, which are the same attribute.
            "typedef float __attribute__((__vector_size__(8))) v2sf;\n",
            "_Static_assert(sizeof(v2sf) == 8, \"v2sf\");\n",
            "typedef short [[gnu::vector_size(8)]] v4hi;\n",
            "_Static_assert(sizeof(v4hi) == 8, \"v4hi\");\n",
            // A lane is what a subscript answers with, and a vector is not a pointer: there is
            // nothing to decay and the lane type is the one the arithmetic happens in.
            "v4si g;\n",
            "_Static_assert(sizeof(g[0]) == 4, \"lane\");\n",
            "_Static_assert(sizeof(g + g) == 16, \"whole\");\n",
            // A scalar beside a vector stands for itself in every lane, so the answer is still
            // the vector and not the wider of the two types.
            "_Static_assert(sizeof(g + 1) == 16, \"broadcast\");\n",
            // An array of them, which is the ordinary way a program holds several.
            "_Static_assert(sizeof(v4si[3]) == 48, \"array\");\n",
        ));
    }

    /// A whole vector written into an array of them, and a vector named by a type name rather
    /// than by a typedef.
    ///
    /// Both are the same question asked twice. A vector is filled like an array of its lanes when
    /// a list is written into it, so a braced element that is itself a vector has to be taken
    /// whole rather than started as the first lane, and the type of what was written is the only
    /// thing that says which was meant. And a type name is where a compound literal and a cast
    /// spell the type out, which a macro taking a lane type and a lane count does, so the
    /// attribute has to be read there and not only on a declaration.
    #[test]
    fn a_vector_is_written_whole_into_an_array_of_them_and_named_by_a_type_name() {
        tast(concat!(
            "typedef int __attribute__((vector_size(8))) v2si;\n",
            "v2si table[] = { (v2si){ 1, 2 }, (v2si){ 3, 4 } };\n",
            "_Static_assert(sizeof(table) == 16, \"two of them and not eight lanes\");\n",
            // The size written out rather than named, which is the spelling a macro expands to.
            "v2si written = (int __attribute__((vector_size(8)))){ 5, 6 };\n",
            "_Static_assert(sizeof((int __attribute__((vector_size(16)))){ 0 }) == 16, \"named\");\n",
            // A lane is still a lane, so a list of them fills the vector the way it always did
            // and the rule above did not turn brace elision off.
            "v2si lanes[2] = { 1, 2, 3, 4 };\n",
            "_Static_assert(sizeof(lanes) == 16, \"still elided\");\n",
        ));
    }

    /// A lane written rather than read, and a shift whose two vectors are not the same type.
    ///
    /// Both are places where a vector is not the aggregate it looks like. A subscript of one is
    /// an lvalue because the vector it came from is an object, so a lane can be assigned to and
    /// has an address, and a qualifier written on the vector reaches every lane the way it does
    /// on an array. And a shift is the one lanewise operator whose sides are not brought to a
    /// single type, since the right side counts rather than computes.
    #[test]
    fn a_lane_is_assignable_and_a_shift_takes_a_count_of_its_own_lane() {
        let result = run(
            &options(),
            concat!(
                "typedef int __attribute__((vector_size(16))) v4si;\n",
                "typedef unsigned __attribute__((vector_size(16))) v4ui;\n",
                "void write(v4si *out, v4ui a, v4si b, int n) {\n",
                "  v4si v = { 1, 2, 3, 4 };\n",
                "  v[0] = n;\n",
                "  v[1] += n;\n",
                "  v[2]++;\n",
                "  *&v[3] = n;\n",
                // The count is signed and the value is not, which no other operator allows.
                "  v4ui shifted = a >> b;\n",
                "  shifted <<= b;\n",
                // A scalar stands in every lane on either side of a shift, which is the half
                // that looks wrong: the shape of the answer comes off the count here.
                "  *out = v + (v4si)shifted + (1 << b);\n",
                "}\n",
                // A qualifier on the vector is a qualifier on the lane, so there is nothing here
                // to write to.
                "void refused(const v4si c) {\n",
                "  c[0] = 1;\n",
                "}\n",
            ),
        );
        assert_eq!(result.messages.len(), 1, "{:?}", result.messages);
        assert!(result.messages[0].contains("assignment of read-only"), "{:?}", result.messages);
    }

    /// The third layout attribute, and the one that is refused rather than read. Reversing the
    /// byte order of every scalar in a record is not something a compiler can do half of, and a
    /// compilation that ignored it would lay the record out in the host's order and hand back
    /// every field with its bytes the wrong way round. Both spellings are here because a header
    /// writes the armoured one, and the member is here because the refusal has to arrive before
    /// the layout is used rather than after.
    #[test]
    fn a_record_that_asks_for_the_other_byte_order_is_refused_rather_than_laid_out_in_this_one() {
        let opts = options();
        let big = "struct s { int i; } __attribute__((scalar_storage_order(\"big-endian\")));\n";
        assert_eq!(
            run(&opts, big).messages,
            ["/main.c:1:36: error: 'scalar_storage_order' is not implemented yet [E0688]\n\
              /main.c:1:36: note: every scalar in this record would be read in the wrong byte \
              order"]
        );

        let armoured =
            "struct s { int i; } __attribute__((__scalar_storage_order__(\"little-endian\")));\n";
        let messages = run(&opts, armoured).messages;
        assert!(messages[0].contains("[E0688]"), "{messages:?}");

        // The attribute in front of the body reaches the same list as the one behind it, and
        // the C23 spelling in gcc's namespace is the same attribute written a third way.
        let front = "struct __attribute__((scalar_storage_order(\"big-endian\"))) s { int i; };\n";
        assert!(run(&opts, front).messages[0].contains("[E0688]"), "{front}");
        let standard = "struct s { int i; } [[gnu::scalar_storage_order(\"big-endian\")]];\n";
        assert!(run(&opts, standard).messages[0].contains("[E0688]"), "{standard}");
    }

    /// Where a bit-field goes, which packing decides and which is the part of all this that
    /// is not what the names suggest. A bit-field goes at the next free bit unless that would
    /// make it span more storage than its own type occupies, and then it moves to the next
    /// boundary of its alignment. Any packing at all takes that rule out, and `#pragma pack`
    /// counts even where it lowers nothing, which is the fourth and seventh cases here.
    ///
    /// Nothing in the language can be asked where a bit-field is, since `offsetof` refuses one
    /// and every size below comes out the same either way, so what is asked is the byte a read
    /// of the field loads from.
    #[test]
    fn packing_is_what_decides_whether_a_bit_field_may_straddle_its_own_storage() {
        // A `char` field after twelve bits, which will not straddle unpacked and does packed.
        assert_eq!(bit_field_byte("struct s { int x : 12; char y : 6; };"), 2);
        assert_eq!(
            bit_field_byte("struct s { int x : 12; char y : 6; } __attribute__((packed));"),
            1
        );
        assert_eq!(
            bit_field_byte("struct s { int x : 12; __attribute__((packed)) char y : 6; };"),
            1
        );
        assert_eq!(bit_field_byte("#pragma pack(4)\nstruct s { int x : 12; char y : 6; };"), 1);
        // A thirty bit field after a byte, which is the case the rule was written for.
        assert_eq!(bit_field_byte("struct s { char x; int y : 30; };"), 4);
        assert_eq!(bit_field_byte("struct s { char x; int y : 30; } __attribute__((packed));"), 1);
        // Four is what an `int` asked for anyway, so this caps nothing and still counts.
        assert_eq!(bit_field_byte("#pragma pack(4)\nstruct s { char x; int y : 30; };"), 1);
        assert_eq!(bit_field_byte("#pragma pack(2)\nstruct s { char x; int y : 30; };"), 1);
    }

    /// The byte a read of `s.y` loads from, which is where the bit-field was placed.
    fn bit_field_byte(record: &str) -> u64 {
        let source = format!("{record}\nint f(struct s *p) {{ return p->y; }}\n");
        let body = body(&source);
        let Some((before, _)) = body.split_once("ptr_add") else { return 0 };
        let (_, constant) = before.rsplit_once("iconst.i64 ").expect("an offset constant");
        constant.lines().next().expect("a line").trim().parse().expect("a byte offset")
    }

    /// An attribute in the middle of a specifier list, which is where a member usually carries
    /// one and which was read and then thrown away. The `[[...]]` spelling and whatever was
    /// written in front of the declaration are collected as the list is walked and the
    /// `__attribute__` spelling is put straight on the specifiers, and the two were assigned
    /// over each other rather than joined.
    #[test]
    fn an_attribute_among_the_specifiers_is_kept_beside_the_ones_written_in_front() {
        tast(concat!(
            "struct a { char c; __attribute__((aligned(8))) int i; };\n",
            "_Static_assert(sizeof(struct a) == 16 && _Alignof(struct a) == 8, \"a\");\n",
            "_Static_assert(__builtin_offsetof(struct a, i) == 8, \"a.i\");\n",
            "struct b { char c; __attribute__((packed)) int i; };\n",
            "_Static_assert(sizeof(struct b) == 5 && _Alignof(struct b) == 1, \"b\");\n",
            "_Static_assert(__builtin_offsetof(struct b, i) == 1, \"b.i\");\n",
            "typedef struct { char c; int i; } __attribute__((packed)) c;\n",
            "_Static_assert(sizeof(c) == 5 && _Alignof(c) == 1, \"c\");\n",
        ));
    }

    /// The other half, which is `#pragma pack`. It caps a member's alignment where `packed`
    /// drops it, so `pack(2)` leaves a `short` where it was and moves an `int`, and it caps a
    /// member the program asked to align as well, which is where the two differ. It is read
    /// at the closing brace of the body, so a line written in the middle of one settles the
    /// whole record rather than the members after it, and `push` and `pop` nest.
    #[test]
    fn pragma_pack_caps_every_member_and_is_read_where_the_body_closes() {
        tast(concat!(
            "#pragma pack(1)\n",
            "struct A { char c; int i; };\n",
            "_Static_assert(sizeof(struct A) == 5 && _Alignof(struct A) == 1, \"A\");\n",
            "_Static_assert(__builtin_offsetof(struct A, i) == 1, \"A.i\");\n",
            "#pragma pack()\n",
            "struct B { char c; int i; };\n",
            "_Static_assert(sizeof(struct B) == 8 && _Alignof(struct B) == 4, \"B\");\n",
            "#pragma pack(2)\n",
            "struct C { char c; int i; double d; };\n",
            "_Static_assert(sizeof(struct C) == 14 && _Alignof(struct C) == 2, \"C\");\n",
            "_Static_assert(__builtin_offsetof(struct C, d) == 6, \"C.d\");\n",
            // A member the program aligned, which `pack` caps and `packed` would not.
            "struct K { char c; int i __attribute__((aligned(8))); };\n",
            "_Static_assert(sizeof(struct K) == 6 && _Alignof(struct K) == 2, \"K\");\n",
            "_Static_assert(__builtin_offsetof(struct K, i) == 2, \"K.i\");\n",
            // The record's own `aligned` is not a member's, so it is not capped.
            "struct J { char c; int i; } __attribute__((aligned(8)));\n",
            "_Static_assert(sizeof(struct J) == 8 && _Alignof(struct J) == 8, \"J\");\n",
            "#pragma pack()\n",
            "#pragma pack(push, 1)\n",
            "struct D { char c; short s; };\n",
            "_Static_assert(sizeof(struct D) == 3 && _Alignof(struct D) == 1, \"D\");\n",
            "#pragma pack(pop)\n",
            "struct E { char c; short s; };\n",
            "_Static_assert(sizeof(struct E) == 4 && _Alignof(struct E) == 2, \"E\");\n",
            // Written in the middle of a body, and it still settles the whole record.
            "struct H { char c;\n",
            "#pragma pack(1)\n",
            "  int i; };\n",
            "_Static_assert(sizeof(struct H) == 5 && _Alignof(struct H) == 1, \"H\");\n",
            "#pragma pack(1)\n",
            "struct I { char c;\n",
            "#pragma pack()\n",
            "  int i; };\n",
            "_Static_assert(sizeof(struct I) == 8 && _Alignof(struct I) == 4, \"I\");\n",
            "#pragma pack()\n",
            // Nested pushes, each one giving back what the one under it had.
            "#pragma pack(push, 8)\n",
            "#pragma pack(push, 1)\n",
            "struct P { char c; int i; };\n",
            "_Static_assert(sizeof(struct P) == 5 && _Alignof(struct P) == 1, \"P\");\n",
            "#pragma pack(pop)\n",
            "struct Q { char c; int i; };\n",
            "_Static_assert(sizeof(struct Q) == 8 && _Alignof(struct Q) == 4, \"Q\");\n",
            "#pragma pack(pop)\n",
            // A cap above what every member already asks for changes nothing at all.
            "#pragma pack(16)\n",
            "struct R { char c; int i; };\n",
            "_Static_assert(sizeof(struct R) == 8 && _Alignof(struct R) == 4, \"R\");\n",
            "#pragma pack()\n",
            "#pragma pack(1)\n",
            "struct S { char c; int i : 5; int j : 20; };\n",
            "_Static_assert(sizeof(struct S) == 5 && _Alignof(struct S) == 1, \"S\");\n",
            "union T { char c; int i; };\n",
            "_Static_assert(sizeof(union T) == 4 && _Alignof(union T) == 1, \"T\");\n",
            "#pragma pack()\n",
        ));
    }

    /// A line the reader cannot make sense of is a warning and the line is dropped, which is
    /// what GCC does with one, and these are its words for each of them. The last line is the
    /// one nothing else would reach, since it stands after every record in the file.
    #[test]
    fn a_pack_line_that_is_not_one_is_reported_in_the_words_gcc_uses() {
        let result = run(
            &options(),
            concat!(
                "#pragma pack 4\n",
                "#pragma pack(pop)\n",
                "#pragma pack(3)\n",
                "#pragma pack(1) junk\n",
                "#pragma pack(push, 1\n",
                "#pragma pack(x)\n",
                // These two are well formed and say nothing. Zero is how a line asks for the
                // target's own alignments back without writing empty parentheses.
                "#pragma pack(0)\n",
                "#pragma pack(push)\n",
                "struct s { char c; int i; };\n",
                "#pragma pack(pop)\n",
                "#pragma pack(pop, foo)\n",
            ),
        );
        let expected = [
            "missing `(` after `#pragma pack` - ignored",
            "`#pragma pack (pop)` encountered without matching `#pragma pack (push)`",
            "alignment must be a small power of two, not 3",
            "junk at end of `#pragma pack`",
            "malformed `#pragma pack(push[, id][, <n>])` - ignored",
            "unknown action `x` for `#pragma pack` - ignored",
            "`#pragma pack(pop, foo)` encountered without matching `#pragma pack(push, foo)`",
        ];
        assert_eq!(result.messages.len(), expected.len(), "{:?}", result.messages);
        for (message, want) in result.messages.iter().zip(expected) {
            assert!(message.contains(want), "expected {want:?} in {message:?}");
        }
    }

    /// The two typedef spellings of the 128 bit types. gcc offers them as keywords rather
    /// than as typedefs in a header, which is the only way a program that includes nothing at
    /// all can still use them, and Apple's `<mach/arm/_structs.h>` is one such program.
    #[test]
    fn the_wide_integer_answers_to_all_three_of_its_names() {
        let text = tast("__uint128_t a; __int128_t b; unsigned __int128 c;\n");
        assert!(text.contains("decl #0 a : unsigned __int128"), "{text}");
        assert!(text.contains("decl #1 b : __int128"), "{text}");
        assert!(text.contains("decl #2 c : unsigned __int128"), "{text}");
    }

    #[test]
    fn every_conversion_the_language_performs_is_a_node_in_the_output() {
        // The point of a typed tree. The source has one operator and the output has the
        // widening that operator asked for, spelled out, so that nothing downstream has to
        // work out the conversion rules a second time.
        let text = tast("long f(int a, long b) { return a + b; }\n");
        assert!(text.contains("convert arithmetic"), "{text}");
    }

    #[test]
    fn a_mistake_in_each_phase_reaches_the_caller_and_writes_no_tree() {
        for source in [
            "#error stop\n",
            "int f(void) { return 1 + ; }\n",
            "int f(void) { return undeclared; }\n",
        ] {
            let result = run(&options(), source);
            assert!(result.failed(), "expected this to fail:\n{source}");
            assert!(
                result.text().is_empty(),
                "a file that did not compile wrote a tree:\n{source}"
            );
        }
    }

    #[test]
    fn one_undeclared_name_is_one_message_and_not_one_per_use() {
        // The poisoning rule from `spec/06-lexer-and-parser.md` section 6.8, seen from the
        // outside. Three uses of a name that was never declared, and the operators over them
        // say nothing at all.
        let result = run(&options(), "int f(void) { return nope + nope * nope; }\n");
        assert_eq!(result.errors, 1, "{:?}", result.messages);
    }

    #[test]
    fn a_declaration_the_parser_skipped_does_not_become_an_undeclared_name_as_well() {
        // The reason the checking is skipped after a failed parse. The parser gave up on the
        // first line and there is no `x` in the tree, so a checker run over it would report
        // every use of `x` below as undeclared, which is a second message about one mistake.
        let result = run(&options(), "int x = ;\nint f(void) { return x; }\n");
        assert_eq!(result.errors, 1, "{:?}", result.messages);
    }

    #[test]
    fn werror_turns_a_warning_into_an_error_in_the_count_and_in_the_word() {
        let source = "int f(void) { char c = 300; return c; }\n";
        let plain = run(&options(), source);
        assert_eq!(plain.errors, 0, "{:?}", plain.messages);
        assert_eq!(plain.messages.len(), 1, "expected a warning about the narrowed constant");
        assert!(!plain.text().is_empty(), "a warning is not a reason to write nothing");

        let mut opts = options();
        opts.warnings_are_errors = true;
        let strict = run(&opts, source);
        assert!(strict.failed());
        assert!(strict.text().is_empty(), "and under -Werror it is a reason to write nothing");
        for message in &strict.messages {
            assert!(!message.contains("warning:"), "{message}");
        }
    }

    #[test]
    fn w_drops_the_warning_before_werror_can_promote_it() {
        let source = "int f(void) { char c = 300; return c; }\n";
        let mut opts = options();
        opts.warnings = false;
        let quiet = run(&opts, source);
        assert_eq!(quiet.messages, Vec::<String>::new());
        assert_eq!(quiet.errors, 0);
        assert!(!quiet.text().is_empty(), "and the file still compiles");

        // A build that passes both means it wants neither, and the order it wrote them in is not
        // something to make it think about.
        opts.warnings_are_errors = true;
        let both = run(&opts, source);
        assert_eq!(both.messages, Vec::<String>::new());
        assert!(!both.failed(), "-w -Werror is not an error about a warning nobody saw");
    }

    #[test]
    fn the_dialect_reaches_the_keywords_and_the_checking() {
        // `typeof` is C23's and GNU's, so the same source is a declaration under one dialect
        // and a mistake under the other, which is the keyword table being built per dialect.
        let source = "typeof(1) x;\n";
        let mut opts = options();
        opts.std = Std::C23;
        opts.gnu_extensions = false;
        assert!(!run(&opts, source).failed(), "{:?}", run(&opts, source).messages);

        opts.std = Std::C17;
        assert!(run(&opts, source).failed());
    }

    #[test]
    fn asking_for_a_kind_that_is_not_written_yet_runs_the_front_end_and_writes_nothing() {
        let mut opts = options();
        opts.emit = EmitKind::Object;
        let result = run(&opts, "int x = 1;\n");
        assert!(!result.failed(), "{:?}", result.messages);
        assert!(result.text().is_empty());
        // And it still finds what the checking finds, so a later kind on a broken file is not
        // a silent success.
        assert!(run(&opts, "int f(void) { return undeclared; }\n").failed());
    }

    /// The machine code of `source`, insisting that it compiled cleanly.
    fn mir(source: &str) -> String {
        let mut opts = options();
        opts.emit = EmitKind::MirFinal;
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text().to_owned()
    }

    /// The whole compiler in one assertion, which is what this emit kind is for.
    ///
    /// C in, machine instructions out, every register a real one and every frame offset a
    /// number. Everything between the two is checked somewhere else, one pass at a time. What is
    /// checked here is that the passes are joined up and that the driver runs them.
    #[test]
    fn a_function_goes_from_c_to_instructions_with_real_registers_in_them() {
        let text = mir("int add(int a, int b) { return a + b; }\n");
        assert!(text.starts_with("mfunc @add {"), "{text}");
        assert!(text.contains("x64.add_rr_32"), "{text}");
        assert!(text.contains("x64.ret"), "{text}");
        // A virtual register is what the allocator was there to remove, so one left in the
        // output is the difference between code and something that looks like code.
        assert!(!text.contains('%'), "{text}");
    }

    /// A declaration has no body, so there is nothing to generate for one and nothing is.
    #[test]
    fn a_function_with_no_body_produces_no_machine_function() {
        let text = mir("int g(int);\nint f(int a) { return g(a); }\n");
        assert_eq!(text.matches("mfunc @").count(), 1, "{text}");
        assert!(text.contains("mfunc @f {"), "{text}");
        assert!(text.contains("x64.call"), "{text}");
    }

    /// Two functions come out in the order the module holds them, which is source order.
    #[test]
    fn every_definition_in_the_file_is_generated_and_they_keep_their_order() {
        let text = mir("int a(int x) { return x; }\nint b(int x) { return x; }\n");
        let first = text.find("mfunc @a").expect("the first function");
        let second = text.find("mfunc @b").expect("the second function");
        assert!(first < second, "{text}");
    }

    /// The target reaches the back end, so the same C is different instructions on Windows.
    #[test]
    fn the_target_decides_which_convention_the_generated_code_follows() {
        let mut opts = options();
        opts.emit = EmitKind::MirFinal;
        let linux = run(&opts, "int f(int a) { return a; }\n").text().to_owned();
        assert!(linux.contains("$rdi"), "{linux}");

        opts.target = "x86_64-pc-windows-msvc".parse::<Triple>().unwrap();
        let windows = run(&opts, "int f(int a) { return a; }\n").text().to_owned();
        assert!(windows.contains("$rcx"), "{windows}");
        assert!(!windows.contains("$rdi"), "{windows}");
    }

    /// A target with no back end says so rather than generating something for another machine.
    #[test]
    fn a_target_this_has_no_back_end_for_is_reported_rather_than_generated() {
        let mut opts = options();
        opts.emit = EmitKind::MirFinal;
        opts.target = "aarch64-unknown-linux-gnu".parse::<Triple>().unwrap();
        let result = run(&opts, "int f(int a) { return a; }\n");
        assert!(result.failed());
        assert!(result.messages[0].contains("no back end for aarch64"), "{:?}", result.messages);
        assert!(result.text().is_empty());
    }

    /// A construct the rule set does not reach yet is named, along with the function it is in.
    ///
    /// The message is about this compiler being unfinished rather than about the program, which
    /// is valid C either way, so it carries the note that says where the work is tracked. Both
    /// functions are attempted, so a file that is ahead of the back end in three places says so
    /// three times rather than one recompilation at a time.
    #[test]
    fn a_construct_the_back_end_cannot_reach_yet_is_reported_against_its_function() {
        let mut opts = options();
        opts.emit = EmitKind::MirFinal;
        let source = "long double a(long double x) { return x; }\n\
                      long double b(long double x) { return x; }\n";
        let result = run(&opts, source);
        assert!(result.failed());
        assert_eq!(result.messages.len(), 2, "{:?}", result.messages);
        assert!(result.messages[0].contains("cannot generate code for 'a'"), "{:?}", result);
        assert!(result.messages[0].contains("x87 stack"), "{:?}", result);
        assert!(result.messages[1].contains("cannot generate code for 'b'"), "{:?}", result);
        assert!(result.text().is_empty());
    }

    /// An opcode the rule language has no word for is named anyway, and pointed at.
    ///
    /// The rule language's spelling is the better name when there is one, but an opcode it has
    /// no word for is exactly the opcode no rule lowers, so falling back to the opcode and the
    /// type is what makes the message say anything at all in the cases that happen. The span is
    /// the instruction's own, so the message lands on the line rather than on the file.
    #[test]
    fn an_opcode_with_no_name_in_the_rule_language_is_named_by_its_own_spelling() {
        let mut opts = options();
        opts.emit = EmitKind::MirFinal;
        let result = run(&opts, "int f(int a) {\n  __int128 wide = a;\n  return (int) wide;\n}\n");
        assert!(result.failed());
        assert!(
            result.messages[0].contains("no rule lowers a `sext` producing a `i128`"),
            "{result:?}"
        );
        assert!(result.messages[0].contains(":2:"), "the line the widening is on: {result:?}");
        assert!(!result.messages[0].contains("this instruction"), "{result:?}");
    }

    /// The note names the issue tracker, which is where a reader finds out whether it is known.
    #[test]
    fn the_note_on_unfinished_work_points_at_the_issues_rather_than_at_the_plan() {
        let mut opts = options();
        opts.emit = EmitKind::MirFinal;
        let result = run(&opts, "int f(int a) { __int128 wide = a; return (int) wide; }\n");
        assert!(result.failed());
        let note = result.messages.iter().find(|line| line.contains("note:")).expect("a note");
        assert!(note.contains("https://github.com/tamnd/rucc/issues"), "{note}");
        assert!(!note.contains("spec/17-milestones.md"), "{note}");
    }

    /// The two frame flags reach the frame, which is the only thing either of them does.
    #[test]
    fn the_frame_flags_on_the_command_line_reach_the_generated_frame() {
        let source = "int f(int a) { return a; }\n";
        assert!(!mir(source).contains("$rbp"), "a leaf needs no frame pointer by default");

        let mut opts = options();
        opts.emit = EmitKind::MirFinal;
        opts.frame_pointer = true;
        let kept = run(&opts, source).text().to_owned();
        assert!(kept.contains("x64.push_64 $rbp"), "{kept}");
    }

    /// The assembly of `source`, insisting that it compiled cleanly.
    fn asm(source: &str) -> String {
        let mut opts = options();
        opts.emit = EmitKind::Asm;
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text().to_owned()
    }

    /// `-S`, which is the same compiler as the kind above it with a different last step.
    ///
    /// What the assembly says is checked in `rucc-asm`, one instruction at a time and against the
    /// target's own description of what an instruction is. What is checked here is that a C file
    /// goes all the way to a listing an assembler would take, which means the directives around
    /// the function as well as the instructions in it.
    #[test]
    fn a_function_goes_from_c_to_assembly_an_assembler_would_take() {
        let text = asm("int add(int a, int b) { return a + b; }\n");
        assert!(text.contains("\t.globl\tadd\n"), "{text}");
        assert!(text.contains("\t.type\tadd, @function\n"), "{text}");
        assert!(text.contains("\nadd:\n"), "{text}");
        assert!(text.contains("\taddl\t"), "{text}");
        assert!(text.contains("\tret\n"), "{text}");
        assert!(text.contains("\t.size\tadd, .-add\n"), "{text}");
        // Without this the stack the program runs on is executable, which is not a default
        // anybody chose and is not a thing a reader would notice missing.
        assert!(text.contains(".note.GNU-stack"), "{text}");
    }

    /// A call through a function pointer, which is a different instruction from a call to a name.
    ///
    /// Both are in the one function on purpose. What is being read is that the two calls are told
    /// apart all the way down: one carries a name the linker resolves and one carries a register,
    /// and neither turns into the other on the way.
    #[test]
    fn a_call_through_a_function_pointer_goes_through_the_register_it_is_in() {
        let text = asm("int g(int);\nint f(int (*p)(int), int a) { return p(a) + g(a); }\n");
        assert!(text.contains("\tcall\t*%"), "{text}");
        assert!(text.contains("\tcall\tg\n"), "{text}");
        // The address arrived in the first argument register and the argument the call passes has
        // to end up there, so the two cannot be the same register and the compiler has to have
        // moved one of them.
        assert!(text.contains("%rdi"), "{text}");
    }

    /// A name at file scope, which is the one address a function cannot compute for itself.
    #[test]
    fn the_address_of_a_global_is_read_from_the_instruction_pointer() {
        let text = asm("extern int counter;\nint f(void) { return counter; }\n");
        assert!(text.contains("\tleaq\tcounter(%rip), "), "{text}");
    }

    /// A cast between a pointer and an integer as wide as one, which is every one C writes here.
    #[test]
    fn a_cast_between_a_pointer_and_an_integer_leaves_the_value_where_it_is() {
        let text = asm("long f(void *p) { return (long)p; }\n");
        // Every instruction in the body is a full width move or the return. The copies are the
        // allocator taking no hints, and what matters here is what is not among them: nothing
        // narrows the value and nothing widens it again, which is what a cast that did something
        // would look like.
        for line in text.lines().filter(|line| line.starts_with('\t') && !line.contains('.')) {
            let mnemonic = line.split_whitespace().next().unwrap_or("");
            assert!(matches!(mnemonic, "movq" | "ret"), "{line} in\n{text}");
        }
    }

    /// The arguments past the sixth arrive in the caller's memory rather than in a register, and
    /// where that memory is depends on what the prologue did, so this is checked at the end of the
    /// pipeline rather than in the middle of it.
    #[test]
    fn an_argument_past_the_last_register_is_read_out_of_the_caller_s_stack() {
        let six = "long a, long b, long c, long d, long e, long f";
        let text = asm(&format!("long f({six}, long g, long h) {{ return g + h; }}\n"));

        // Nothing is pushed and no frame is taken, so the only thing between the stack pointer and
        // the caller's arguments is the return address the call pushed. Which is where gcc 16.2.0
        // reads them from too, at `-O0`, in the same two instructions.
        assert!(text.contains("\tmovq\t8(%rsp), "), "{text}");
        assert!(text.contains("\tmovq\t16(%rsp), "), "{text}");

        // A narrower one is read at its own width, because the bits above it are bits the
        // convention says nothing about, and one in the other register file with the other file's
        // instruction.
        let narrow = asm(&format!("int f({six}, int g) {{ return g; }}\n"));
        assert!(narrow.contains("\tmovl\t8(%rsp), "), "{narrow}");
        let eight =
            "double a, double b, double c, double d, double e, double f, double g, double h";
        let float = asm(&format!("double f({eight}, double i) {{ return i; }}\n"));
        assert!(float.contains("\tmovsd\t8(%rsp), "), "{float}");
    }

    /// The other end of the same thing. What the caller writes is at the stack pointer, because
    /// that is the bottom of its frame and the bottom of its frame is where the callee looks.
    #[test]
    fn a_call_writes_the_arguments_with_no_register_left_at_the_stack_pointer() {
        let six = "1, 2, 3, 4, 5, 6";
        let decl = "long g(long, long, long, long, long, long, long, long);\n";
        let text = asm(&format!("{decl}long f(void) {{ return g({six}, 7, 8); }}\n"));

        assert!(text.contains("\tmovq\t%"), "{text}");
        assert!(text.contains(", (%rsp)\n"), "{text}");
        assert!(text.contains(", 8(%rsp)\n"), "{text}");
        // And it reserved the bytes it wrote into, so nothing else in the frame is on top of them.
        assert!(text.contains("\tsubq\t$"), "{text}");

        // A narrower one is written at its own width, matching what the callee reads it back with.
        let narrow = "int g(int, int, int, int, int, int, int);\n";
        let text = asm(&format!("{narrow}int f(void) {{ return g({six}, 7); }}\n"));
        assert!(text.contains("\tmovl\t%"), "{text}");
        assert!(text.contains(", (%rsp)\n"), "{text}");
    }

    /// The count a variadic callee on this convention reads is a count of vector registers, so a
    /// float that ran out of them and went to memory is not in it.
    #[test]
    fn a_variadic_call_counts_registers_and_not_arguments() {
        let nine = "1., 2., 3., 4., 5., 6., 7., 8., 9.";
        let decl = "int g(int, ...);\n";
        let text = asm(&format!("{decl}int f(void) {{ return g(0, {nine}); }}\n"));

        assert!(text.contains("\tmovl\t$8, "), "eight registers, not nine: {text}");
        assert!(text.contains("\tmovsd\t%"), "{text}");
        assert!(text.contains(", (%rsp)\n"), "{text}");
    }

    /// The callee's half of the same convention. Every argument register it was handed is written
    /// into its frame on the way in, because which of them hold anything is a thing only the caller
    /// knew, and the ones the signature does name are left out because `va_start` sets the offsets
    /// past them and nothing ever reads their slots.
    #[test]
    fn a_variadic_function_writes_the_argument_registers_it_was_handed_into_its_frame() {
        let body =
            "__builtin_va_list ap; __builtin_va_start(ap, n); __builtin_va_end(ap); return n;";
        let text = asm(&format!("int f(int n, ...) {{ {body} }}\n"));

        // Five general purpose registers and eight vector ones, since the one parameter the
        // signature names took the first of the six.
        let stores = |mnemonic: &str| text.matches(&format!("\t{mnemonic}\t%")).count();
        assert!(text.contains(", 8(%r"), "the second slot, not the first: {text}");
        assert!(!text.contains(", 0(%r"), "{text}");
        assert_eq!(stores("movsd"), 8, "every vector register: {text}");

        // And the area is one of the function's own stack objects, so the frame holds it.
        assert!(text.contains("\tsubq\t$"), "{text}");
    }

    /// What `va_start` writes is the four fields of the list, and the two numbers among them are
    /// where the arguments the signature names left the walk over each file's registers.
    #[test]
    fn va_start_writes_the_four_fields_the_psabi_describes() {
        let start = "__builtin_va_list ap; __builtin_va_start(ap, d);";
        let params = "int a, int b, int c, double d";
        let text = asm(&format!("int f({params}, ...) {{ {start} return a; }}\n"));

        // Three integers took three of the six general purpose registers, and one double took one
        // of the eight vector ones, so the walk starts at twenty four bytes into the first half and
        // sixteen bytes into the second, which begins at forty eight.
        assert!(text.contains("	movl	$24, "), "{text}");
        assert!(text.contains("	movl	$64, "), "{text}");
        // The other two fields are addresses rather than numbers, so each is stored as a word and
        // each is a `lea` away. One of them reaches above the frame, which is where the caller's
        // arguments are and is the only thing in this function that is not below the stack pointer.
        assert!(text.contains(", 8(%r"), "{text}");
        assert!(text.contains(", 16(%r"), "{text}");
        let frame: u32 = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("subq	$")?.split(',').next()?.parse().ok())
            .expect("a variadic function takes a frame for the save area");
        let above = |line: &str| {
            let at: u32 = line.trim().strip_prefix("leaq	")?.split('(').next()?.parse().ok()?;
            Some(at > frame)
        };
        assert!(text.lines().filter_map(above).any(|it| it), "{frame}: {text}");
    }

    /// A `va_arg` is a branch on whether the argument it wants is still in the save area, and which
    /// of the two halves it walks is the type's answer.
    #[test]
    fn va_arg_branches_on_whether_the_argument_is_still_in_the_save_area() {
        let read = "__builtin_va_list ap; __builtin_va_start(ap, n);";
        let ints = format!("int f(int n, ...) {{ {read} return __builtin_va_arg(ap, int); }}\n");
        let text = asm(&ints);

        // The last general purpose slot begins at forty, so an offset above it is an argument the
        // caller left in its own memory instead.
        assert!(text.contains("$40, "), "{text}");
        assert!(text.contains("	cmpl	"), "{text}");
        assert!(text.contains("	setbe	"), "unsigned, since an offset is a count of bytes: {text}");

        let arg = "__builtin_va_arg(ap, double)";
        let text = asm(&format!("double f(int n, ...) {{ {read} return {arg}; }}\n"));
        assert!(text.contains("$160, "), "the last vector slot: {text}");
    }

    /// A structure assigned is a copy of a known size, and a copy of a known size is a run of
    /// moves rather than a call to a library this compiler has no way to reach yet.
    #[test]
    fn a_structure_assignment_is_a_move_for_each_word_of_it() {
        let decl = "struct pair { long a, b; };\n";
        let body = "struct pair p = *q; return p.a + p.b;";
        let text = asm(&format!("{decl}long f(struct pair *q) {{ {body} }}\n"));

        assert!(!text.contains("memcpy"), "nothing calls the library: {text}");
        assert!(!text.contains("\tcall"), "{text}");
        // Sixteen bytes aligned to eight is two words, and each is a load and a store.
        assert!(text.matches("\tmovq\t").count() >= 4, "two words each way: {text}");
    }

    /// A word is as wide as the object is aligned to and no wider, so a character array is copied
    /// a byte at a time and a structure of longs eight bytes at a time.
    #[test]
    fn how_wide_a_word_of_a_copy_is_follows_the_alignment() {
        let decl = "struct bytes { char a[8]; };\n";
        let body = "struct bytes p = *q; return p.a[0];";
        let text = asm(&format!("{decl}int f(struct bytes *q) {{ {body} }}\n"));

        // Eight bytes aligned to one is eight words, and each is a load and a store.
        assert!(text.matches("\tmovb\t").count() >= 16, "a byte at a time: {text}");
    }

    /// What an initialiser does not name is zero, which the front end writes as a fill and this
    /// writes as the byte spread across each word.
    #[test]
    fn the_part_of_an_initialiser_that_names_nothing_is_stored_as_zero() {
        let decl = "struct wide { long a, b, c; };\n";
        let text = asm(&format!("{decl}long f(void) {{ struct wide w = {{ 7 }}; return w.c; }}\n"));

        assert!(!text.contains("memset"), "nothing calls the library: {text}");
        assert!(text.contains("\tmovq\t$0, ") || text.contains("$0, %"), "the zero: {text}");
    }

    /// A copy too large to be worth unrolling is a call to the runtime, which is the C library on
    /// a hosted target and `rucc-builtins` on a freestanding one.
    #[test]
    fn a_copy_too_large_to_unroll_calls_the_runtime() {
        let decl = "struct huge { char a[4096]; };\n";
        let mut opts = options();
        opts.emit = EmitKind::Asm;
        let source = format!("{decl}void f(struct huge *p, struct huge *q) {{ *p = *q; }}\n");
        let result = run(&opts, &source);
        assert!(!result.failed(), "{:?}", result.messages);
        let text = result.text();
        assert!(text.contains("call") && text.contains("memcpy"), "{text}");
        // The size in the register the convention passes the third argument in, which is what
        // says the call was built from the convention and not from the shape of the IR.
        assert!(text.contains("4096"), "the size travels: {text}");
    }

    /// A frame that had to force its own alignment cannot say how far away the caller's stack
    /// pointer was, so it reaches back through the frame pointer instead.
    #[test]
    fn a_realigned_frame_reads_them_through_the_frame_pointer() {
        let six = "long a, long b, long c, long d, long e, long f";
        let body = "_Alignas(32) long wide[4]; wide[0] = g; return wide[0];";
        let text = asm(&format!("long f({six}, long g) {{ {body} }}\n"));

        // The frame pointer is saved and pointed at where it was saved before the alignment is
        // forced, so the caller's arguments stay a constant distance from it: one word for the
        // saved frame pointer and one for the return address.
        assert!(text.contains("\tandq\t$-32, %rsp"), "{text}");
        assert!(text.contains("\tmovq\t16(%rbp), "), "{text}");
        assert!(!text.contains("\tmovq\t16(%rsp), "), "{text}");
    }

    /// The object format decides the directives, and the target decides the object format.
    #[test]
    fn the_target_decides_how_the_assembly_is_spelled() {
        let mut opts = options();
        opts.emit = EmitKind::Asm;
        opts.target = "x86_64-apple-darwin".parse::<Triple>().unwrap();
        let text = run(&opts, "int f(void) { return 0; }\n").text().to_owned();
        assert!(text.contains("__TEXT,__text"), "{text}");
        assert!(text.contains("\n_f:\n"), "{text}");
        assert!(!text.contains(".note.GNU-stack"), "{text}");
    }

    /// The object file of `source`, insisting that it compiled cleanly.
    fn obj(source: &str) -> Vec<u8> {
        let mut opts = options();
        opts.emit = EmitKind::Object;
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        match result.artifact {
            Artifact::Object(bytes) => bytes,
            other => panic!("expected an object, got {other:?}"),
        }
    }

    /// `-c`, which is the last step of the three the back end can end with.
    ///
    /// What is in the file is checked in `rucc-object`, a field at a time. What is checked here is
    /// that a C file goes all the way to one, which is the whole compiler in one line and the
    /// thing that stops working when a layer between them changes its mind about something.
    #[test]
    fn a_function_goes_from_c_to_an_object_a_linker_would_take() {
        let bytes = obj("int add(int a, int b) { return a + b; }\n");
        assert_eq!(&bytes[..4], b"\x7fELF", "an object file starts by saying it is one");
        let text = asm("int add(int a, int b) { return a + b; }\n");
        assert!(
            text.contains("\taddl\t"),
            "and the listing of it is the same instructions:\n{text}"
        );
    }

    /// A variable this file defines, which is what a reference to one has to resolve against.
    #[test]
    fn a_variable_goes_from_c_to_the_section_it_belongs_in() {
        let text = asm("int counter = 42;\nstatic int hidden;\nconst int fixed = 7;\n");
        assert!(text.contains("\t.data\n\t.globl\tcounter\n"), "{text}");
        assert!(text.contains("\ncounter:\n\t.long\t42\n"), "{text}");
        assert!(text.contains("\t.size\tcounter, .-counter\n"), "{text}");
        // A zeroed variable carries its size and none of its bytes, and a `static` one is not
        // announced to the linker at all, which is the whole of what `static` means here.
        assert!(text.contains("\t.bss\n\t.p2align\t2\n"), "{text}");
        assert!(text.contains("\nhidden:\n\t.space\t4\n"), "{text}");
        assert!(!text.contains(".globl\thidden"), "{text}");
        // Nothing writes through it, so it goes in a page the loader can map read only and every
        // process running the program can share.
        assert!(text.contains("\t.section\t.rodata\n"), "{text}");
    }

    /// A bit-field with a value in it, which is written as the bytes the value lands in.
    ///
    /// The interesting one is the field whose lowest byte is zero. The bytes a bit-field
    /// initializer makes are put together first and then taken back out as the run they make,
    /// and taking them out starts at the byte the field starts at, so a zero byte at the front
    /// used to end the object up in `.bss` with the rest of its value thrown away.
    #[test]
    fn a_bit_field_initializer_writes_every_byte_of_the_value_and_not_only_the_ones_that_are_set() {
        let text = asm("struct s { unsigned f : 20; } x = { 0x12300 };\n");
        assert!(text.contains("\t.data\n"), "there is something to write: {text}");
        assert!(text.contains("\nx:\n\t.ascii\t\"\\000#\\001\"\n"), "and it is the value: {text}");

        // Two fields, the first of them zero, which is the same thing said with the zero byte
        // inside the run rather than at the front of it.
        let text = asm("struct s { unsigned a : 8; unsigned b : 8; } x = { 0, 3 };\n");
        assert!(text.contains("\nx:\n\t.ascii\t\"\\000\\003\"\n"), "{text}");

        // Wider than an `int`, which is the same code and is worth saying because the value no
        // longer fits in the thirty two bits a bit-field used to be read at.
        let text = asm("struct s { unsigned long long f : 40; } x = { 0x100000 };\n");
        assert!(text.contains("\nx:\n\t.ascii\t\"\\000\\000\\020\"\n\t.space\t5\n"), "{text}");

        // Nothing in it, which still costs no bytes in the file.
        let text = asm("struct s { unsigned f : 20; } x = { 0 };\n");
        assert!(text.contains("\t.bss\n"), "an object of zeroes is zeroes: {text}");
        assert!(text.contains("\nx:\n\t.space\t4\n"), "{text}");
    }

    /// A string literal, which is a variable the program never named.
    #[test]
    fn a_string_literal_is_a_variable_with_a_name_no_program_could_write() {
        let text = asm("const char *f(void) { return \"hi\"; }\n");
        assert!(text.contains("\t.ascii\t\"hi\\000\"\n"), "{text}");
        assert!(text.contains("\t.section\t.rodata\n"), "{text}");
        let label = text
            .lines()
            .find(|line| line.starts_with(".Lstr"))
            .unwrap_or_else(|| panic!("a label for the literal in\n{text}"));
        assert!(!text.contains(&format!(".globl\t{}", label.trim_end_matches(':'))), "{text}");
    }

    /// A variable holding the address of another one, which is the only hole an image has in it.
    #[test]
    fn an_address_in_an_initializer_is_left_to_the_linker() {
        let source = "int counter;\nint *p = &counter;\n";
        let text = asm(source);
        assert!(text.contains("\np:\n\t.quad\tcounter\n"), "{text}");
        // And in the object it is eight zero bytes and a relocation, which is what the two paths
        // being one description is for.
        let bytes = obj(source);
        assert!(bytes.windows(8).any(|w| w == b"counter\0"), "the object has to name it");
    }

    /// A thread-local variable, which is valid C that the back end does not build yet.
    #[test]
    fn a_thread_local_variable_is_reported_as_work_that_is_not_done() {
        let mut opts = options();
        opts.emit = EmitKind::Asm;
        let result = run(&opts, "_Thread_local int x = 1;\n");
        assert!(result.failed(), "every thread sharing one variable is worse than a message");
        assert!(result.messages.iter().any(|m| m.contains("thread-local")), "{:?}", result);
        // Not an internal error: nothing here is wrong and the note says where the work is.
        assert!(!result.messages.iter().any(|m| m.contains("internal")), "{:?}", result);
    }

    /// Not a rewording of the check above: what the two paths agree about is the point.
    #[test]
    fn the_object_and_the_listing_are_two_spellings_of_one_compilation() {
        // A call, because it is the one thing whose spelling in the two differs completely: the
        // listing writes a name and the object writes four zero bytes and a relocation asking the
        // linker for the same name. If either path had lost the callee, one of these would fail.
        let source = "int callee(void); int g(void) { return callee(); }\n";
        let bytes = obj(source);
        assert!(
            bytes.windows(7).any(|w| w == b"callee\0"),
            "the object has to name the callee for the linker to find it"
        );
        let text = asm(source);
        assert!(text.contains("\tcall\tcallee\n"), "{text}");
    }

    /// What a file of a link contributes is an object, and the default emit is a link.
    ///
    /// This is here because getting it wrong is silent in the worst way: an empty file is a valid
    /// empty linker script, so a link fed one gets as far as reporting every symbol of the file as
    /// undefined and says nothing about the compilation that produced nothing.
    #[test]
    fn compiling_for_an_executable_produces_an_object_and_not_a_dump() {
        let mut opts = options();
        // What a command line with no `-c` and no `-S` on it asks for.
        opts.emit = EmitKind::Executable;
        let result = run(&opts, "int main(void) { return 0; }\n");
        assert_eq!(result.messages, Vec::<String>::new());
        match result.artifact {
            Artifact::Object(bytes) => assert_eq!(&bytes[..4], b"\x7fELF"),
            other => panic!("expected an object, got {other:?}"),
        }
    }

    /// A target with a back end but no object writer says so rather than writing the wrong file.
    #[test]
    fn a_platform_with_no_object_writer_is_said_so_rather_than_written_as_elf() {
        let mut opts = options();
        opts.emit = EmitKind::Object;
        opts.target = "x86_64-apple-darwin".parse::<Triple>().unwrap();
        let result = run(&opts, "int f(void) { return 0; }\n");
        assert!(result.failed(), "an object nobody can read is worse than a message");
        assert!(
            result.messages.iter().any(|m| m.contains("no object writer")),
            "{:?}",
            result.messages
        );
    }

    /// The IR of `source`, insisting that it compiled cleanly.
    fn ir(source: &str) -> String {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text().to_owned()
    }

    /// What was said about `source`, insisting that something was.
    fn errors(source: &str) -> Vec<String> {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let result = run(&opts, source);
        assert!(result.failed(), "expected this to be refused:\n{source}");
        result.messages
    }

    /// The body of the one function in `source`, which is what most of these are about.
    fn body(source: &str) -> String {
        let text = ir(source);
        let (_, rest) = text.split_once("{\n").expect("a function definition");
        let (body, _) = rest.rsplit_once("}\n").expect("a function definition");
        body.to_owned()
    }

    /// The IR of `source` at one safety tier, insisting that it compiled cleanly.
    fn safe_ir(tier: rucc_session::Safety, source: &str) -> String {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        opts.safety = tier;
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text().to_owned()
    }

    const READS_THROUGH_A_POINTER: &str = "int read(int *p) { return p[1]; }\n";

    #[test]
    fn a_build_that_did_not_ask_for_the_monitor_is_compiled_the_way_it_always_was() {
        // This is the load bearing test of the whole flag. The monitor is being built in the open
        // and every build in the world is compiled by this compiler with the flag absent, so a
        // check that leaked into that path would be a regression for everybody.
        let text = ir(READS_THROUGH_A_POINTER);
        assert!(!text.contains("check_"), "{text}");
        assert!(!text.contains("cap_of"), "{text}");
    }

    #[test]
    fn asking_for_a_tier_puts_the_checks_in_before_the_optimizer_sees_them() {
        let text = safe_ir(rucc_session::Safety::Detect, READS_THROUGH_A_POINTER);
        assert!(text.contains("cap_of"), "{text}");
        assert!(text.contains("check_bounds"), "{text}");
        assert!(text.contains("check_live"), "{text}");
        // The subscript is address arithmetic, so J2 applies to it as well as J1.
        assert!(text.contains("check_deriv"), "{text}");
    }

    #[test]
    fn the_three_tiers_that_are_not_off_all_check_the_same_accesses_so_far() {
        // What separates them is the reporter and the boundary, which are milestones S2 and S3.
        // Pinning it here means the day they stop agreeing, this test says so rather than the
        // difference going unnoticed.
        let detect = safe_ir(rucc_session::Safety::Detect, READS_THROUGH_A_POINTER);
        for tier in [rucc_session::Safety::Enforce, rucc_session::Safety::Kernel] {
            assert_eq!(safe_ir(tier, READS_THROUGH_A_POINTER), detect, "{tier}");
        }
    }

    /// The safety summary of `source` at one tier, insisting that it compiled cleanly.
    fn summary(tier: rucc_session::Safety, source: &str) -> String {
        let mut opts = options();
        opts.emit = EmitKind::SafetySummary;
        opts.safety = tier;
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text().to_owned()
    }

    #[test]
    fn the_summary_counts_the_checks_that_went_in_and_the_ones_still_standing() {
        let text = summary(rucc_session::Safety::Detect, READS_THROUGH_A_POINTER);
        assert!(text.contains("\"tier\": \"detect\""), "{text}");
        // One load, so one of each of the two access checks, and the subscript is a derivation.
        assert!(
            text.contains("\"bounds\": { \"emitted\": 1, \"remaining\": 1, \"discharged\": 0 }"),
            "{text}"
        );
        assert!(
            text.contains(
                "\"derivation\": { \"emitted\": 1, \"remaining\": 1, \"discharged\": 0 }"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_build_without_the_monitor_summarises_as_a_build_with_no_checks_in_it() {
        // Which is the honest summary rather than an error. A build system that emits a summary
        // for every unit should get one for the units nobody asked to instrument too, and the
        // zeroes are what say that the guarantee over that file is nothing at all.
        let text = summary(rucc_session::Safety::Off, READS_THROUGH_A_POINTER);
        assert!(text.contains("\"tier\": \"off\""), "{text}");
        assert!(
            text.contains("\"bounds\": { \"emitted\": 0, \"remaining\": 0, \"discharged\": 0 }"),
            "{text}"
        );
    }

    #[test]
    fn a_call_the_boundary_models_is_counted_apart_from_one_it_does_not() {
        let text = summary(
            rucc_session::Safety::Detect,
            "void *memcpy(void *, const void *, unsigned long);\n\
             int puts(const char *);\n\
             void f(char *d, char *s) { memcpy(d, s, 4); puts(d); }\n",
        );
        assert!(text.contains("\"interposed\": 1"), "{text}");
        assert!(text.contains("\"puts\""), "{text}");
        // The wrapper it was pointed at is ours, so it is not on the list of things this build
        // failed to model. Counting it there would make instrumenting a file look worse than
        // leaving it alone.
        assert!(!text.contains("__rucc_wrap_memcpy\""), "{text}");
    }

    #[test]
    fn the_two_directions_a_pointer_crosses_the_boundary_are_counted_apart() {
        // `f` is a name the linker can bind to and takes a pointer, so a pointer arrives there.
        // `notes_open` is a library this build did not instrument, so a pointer comes back from
        // it. Both are crossings and neither is the other, which is why there are two numbers.
        let text = summary(
            rucc_session::Safety::Detect,
            "void *notes_open(void);\n\
             char *f(char *p) { char *q = notes_open(); return q ? q : p; }\n",
        );
        assert!(text.contains("\"crossings\": { \"entered\": 1, \"returned\": 1 }"), "{text}");
        assert!(text.contains("\"notes_open\""), "{text}");
    }

    #[test]
    fn a_static_function_nobody_takes_the_address_of_is_not_a_crossing() {
        // Nothing outside the file can reach it, so a witness on its parameters would be counting
        // a crossing that does not happen.
        let text = summary(
            rucc_session::Safety::Detect,
            "static int len(const char *p) { return p ? 1 : 0; }\n\
             int f(void) { return len(\"x\"); }\n",
        );
        assert!(text.contains("\"crossings\": { \"entered\": 0, \"returned\": 0 }"), "{text}");
    }

    #[test]
    fn a_witness_reaches_the_assembler_as_a_call_to_the_runtime() {
        // The count only means anything if the call is really there, and a summary saying one is
        // there is not evidence that the back end emitted it.
        let text = safe_asm(rucc_session::Safety::Detect, "char *f(char *p) { return p; }\n");
        assert!(text.contains("\tcall\t__rucc_cap_witness\n"), "{text}");
    }

    #[test]
    fn a_pointer_turned_into_an_integer_is_on_the_trust_set() {
        let text = summary(
            rucc_session::Safety::Detect,
            "unsigned long f(int *p) { return (unsigned long) p; }\n",
        );
        assert!(text.contains("\"exposed\": 1"), "{text}");
    }

    /// The assembly of `source` at one safety tier, insisting that it compiled cleanly.
    fn safe_asm(tier: rucc_session::Safety, source: &str) -> String {
        let mut opts = options();
        opts.emit = EmitKind::Asm;
        opts.safety = tier;
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text().to_owned()
    }

    #[test]
    fn a_check_reaches_the_assembler_as_a_call_to_the_runtime() {
        let text = safe_asm(rucc_session::Safety::Detect, READS_THROUGH_A_POINTER);
        assert!(text.contains("\tcall\t__rucc_check_bounds\n"), "{text}");
        assert!(text.contains("\tcall\t__rucc_check_live\n"), "{text}");
        assert!(text.contains("\tcall\t__rucc_check_deriv\n"), "{text}");
    }

    #[test]
    fn every_check_that_reached_the_assembler_has_a_row_describing_it() {
        // Three checks and three descriptors, each in the section the runtime's reporter reads.
        // The width is `rucc_safety::lower::WIDTH` and the row is `rucc_safe_rt::fail::Descriptor`,
        // and the two agreeing is what makes the address a check is handed mean anything.
        let text = safe_asm(rucc_session::Safety::Detect, READS_THROUGH_A_POINTER);
        let section = format!("\t.section\t{},", rucc_safety::SECTION);
        assert_eq!(text.matches(&section).count(), 3, "{text}");
        for index in 0..3 {
            let name = format!("__rucc_safety_desc_{index}");
            // Defined once and referenced once, because a descriptor nothing points at describes
            // nothing and a reference with no definition does not link.
            assert!(text.contains(&format!("{name}:\n")), "{text}");
            assert!(text.contains(&format!("{name}(%rip)")), "{text}");
        }
        assert!(!text.contains("__rucc_safety_desc_3"), "{text}");
    }

    /// `__builtin_constant_p` is answered in the front end and never reaches the IR.
    ///
    /// gcc folds it after optimization, so its answer for an argument that is not written as a
    /// constant can differ between `-O0` and `-O2`. What is checked here is the front end's
    /// answer, which is the same at every level, and the four cases where gcc gives the same
    /// answer at both levels are the ones measured on gcc 16: a literal is one, a variable is
    /// zero, a string literal is one and the address of an object is zero.
    #[test]
    fn builtin_constant_p_is_folded_where_it_is_written_rather_than_called() {
        let text = ir(concat!(
            "int g;\n",
            "int a = __builtin_constant_p(1);\n",
            "int b = __builtin_constant_p(g);\n",
            "int c = __builtin_constant_p(\"abc\");\n",
            "int d = __builtin_constant_p(&g);\n",
            "int e = __builtin_constant_p(1.5);\n",
            "int h = __builtin_choose_expr(__builtin_constant_p(3), 11, 22);\n",
        ));
        assert!(text.contains("global @a : i32 = 1,"), "{text}");
        assert!(text.contains("global @b : i32 = 0,"), "{text}");
        assert!(text.contains("global @c : i32 = 1,"), "{text}");
        assert!(text.contains("global @d : i32 = 0,"), "{text}");
        assert!(text.contains("global @e : i32 = 1,"), "{text}");
        assert!(text.contains("global @h : i32 = 11,"), "{text}");
        assert!(!text.contains("__builtin_constant_p"), "it is not a call to anything:\n{text}");

        // The argument is not evaluated, which is what gcc does with it as well, so `i` is
        // still zero. The second constant is the answer, which nothing reads and which the
        // first pass that looks for dead code will take out.
        let text = body("int f(void) { int i = 0; __builtin_constant_p(i++); return i; }\n");
        assert_eq!(text, "block0:\n    %0 = iconst.i32 0\n    %1 = iconst.i32 0\n    return %0\n");
    }

    /// A library builtin is the library function of the same name, and the call says so.
    ///
    /// A program writes `__builtin_strlen` rather than `strlen` to reach the function the C
    /// library promises where its own name has been taken by a macro, and to say that the usual
    /// meaning is the one intended. So the name in the program and the name in the object file
    /// are two different names and the call carries the second one. gcc folds several of these
    /// when the arguments allow it, which is an optimization on top of a call that is already
    /// right rather than instead of it, so nothing here depends on any folding happening.
    #[test]
    fn a_call_to_a_library_builtin_reaches_the_library_function() {
        let text = body("void f(void) { __builtin_abort(); }\n");
        assert_eq!(text, "block0:\n    call @abort() : ()\n    return\n");

        // Nothing declared either of these and nothing had to: the prefix is what says the name
        // belongs to the implementation, and the type comes out of `features.toml`.
        let text = ir("int f(const char *s) { return __builtin_puts(s) + __builtin_strlen(s); }\n");
        assert!(text.contains("call @puts(%0) : (ptr) -> i32"), "{text}");
        assert!(text.contains("call @strlen(%0) : (ptr) -> i64"), "{text}");
        assert!(!text.contains("__builtin_"), "the prefix is not part of any name here:\n{text}");
    }

    /// The absolute value family is four instructions and not a call, whoever declared the name.
    ///
    /// `abs`, `labs` and `llabs` are reserved to the implementation, so a program that writes one
    /// means the one the C library promises and the compiler is allowed to know what it does. The
    /// program in `gcc.c-torture/execute/20021127-1.c` is the one that insists: it defines `llabs`
    /// to abort and expects the call not to reach it. Measured against gcc 16.2.0, which writes a
    /// `neg` and a `cmovns` and never calls the definition either.
    ///
    /// The most negative value comes back as itself, which is what the arithmetic gives and what
    /// gcc's pair of instructions gives, and C says the answer is undefined there.
    #[test]
    fn the_absolute_value_family_is_the_magnitude_and_not_a_call() {
        let text = body(concat!(
            "long long llabs(long long);\n",
            "long long f(long long x) { return llabs(x); }\n",
        ));
        assert!(text.contains("%1 = iconst.i64 63"), "{text}");
        assert!(text.contains("%2 = ashr %0, %1"), "{text}");
        assert!(text.contains("%3 = xor %0, %2"), "{text}");
        assert!(text.contains("%4 = sub %3, %2"), "{text}");
        assert!(!text.contains("call"), "the call does not happen:\n{text}");

        // The narrower two, whose width comes from the type the library gives the name and not
        // from anything at the call.
        let text = body("int abs(int);\nint f(int x) { return abs(x); }\n");
        assert!(text.contains("iconst.i32 31"), "{text}");
        let text = body("long labs(long);\nlong f(long x) { return labs(x); }\n");
        assert!(text.contains("iconst.i64 63"), "{text}");

        // The prefixed spelling is the same node, and it is what a program writes to reach the
        // library's meaning where the plain name has been taken.
        let text = body("long long f(long long x) { return __builtin_llabs(x); }\n");
        assert!(!text.contains("call"), "{text}");

        // A definition of the name in the same file changes nothing, which is the whole point.
        let text = ir(concat!(
            "long long llabs(long long b);\n",
            "long long g(long long x) { return llabs(x); }\n",
            "long long llabs(long long b) { return 7; }\n",
        ));
        assert!(!text.contains("call @llabs"), "{text}");
    }

    /// A byte swap is one instruction and not a call, and nothing had to declare it.
    ///
    /// SQLite writes these for its page headers and glibc's `<endian.h>` defines `htobe32` and its
    /// neighbours as exactly these, so a program that reads a file format reaches one without ever
    /// naming it. There is no object file anywhere that defines `__builtin_bswap32`, so a call left
    /// standing here would not link.
    #[test]
    fn a_byte_swap_is_arithmetic_and_not_a_call() {
        let text = body("unsigned f(unsigned x) { return __builtin_bswap32(x); }\n");
        assert_eq!(text, "block0(%0: i32):\n    %1 = bswap %0\n    return %1\n");

        // The argument is converted by the prototype the way any other call's would be, so the
        // swap happens at the width the name says and not at the width the program wrote.
        let text = body("unsigned f(unsigned char c) { return __builtin_bswap32(c); }\n");
        assert!(text.contains("zext.i32 %0"), "widened first: {text}");
        assert!(text.contains("bswap %1"), "and swapped at four bytes: {text}");
    }

    /// Each of the three reverses in the width its name says, which is the type of the node.
    ///
    /// The width matters more here than it looks. `__builtin_bswap16` is the two bytes of a
    /// `uint16_t` exchanged, and if the node came out at the machine's width instead then the bits
    /// above the value would be dragged into the answer and the result would be zero.
    #[test]
    fn the_byte_swaps_reverse_at_the_width_their_name_says() {
        for (name, ty, width) in [
            ("__builtin_bswap16", "unsigned short", "i16"),
            ("__builtin_bswap32", "unsigned", "i32"),
            ("__builtin_bswap64", "unsigned long long", "i64"),
        ] {
            let source = format!("{ty} f({ty} x) {{ return {name}(x); }}\n");
            let text = body(&source);
            assert_eq!(
                text,
                format!("block0(%0: {width}):\n    %1 = bswap %0\n    return %1\n"),
                "{name}"
            );
        }
    }

    /// The three bit counts the IR has an instruction for are that instruction and not a call.
    ///
    /// Fifteen rows of `features.toml` come out of five questions, and three of the five are one
    /// instruction each. The kernel's bitmap search is built on them, ffmpeg counts leading zeroes
    /// in its bitstream reader and SQLite uses one to size a page, so a call left standing here
    /// would not link against anything and would be slow if it did.
    #[test]
    fn the_bit_counts_are_instructions_and_not_calls() {
        let text = body("int f(unsigned x) { return __builtin_clz(x); }\n");
        assert_eq!(text, "block0(%0: i32):\n    %1 = ctlz %0\n    return %1\n");

        let text = body("int f(unsigned x) { return __builtin_ctz(x); }\n");
        assert_eq!(text, "block0(%0: i32):\n    %1 = cttz %0\n    return %1\n");

        let text = body("int f(unsigned x) { return __builtin_popcount(x); }\n");
        assert_eq!(text, "block0(%0: i32):\n    %1 = ctpop %0\n    return %1\n");
    }

    /// The width counted is the operand's and the width answered is `int`, which are two different
    /// things at every spelling but the narrowest.
    ///
    /// This is the mistake the family invites. `__builtin_clz` of a value counts the leading zeroes
    /// of it narrowed to `unsigned int` and `__builtin_clzll` counts them at sixty four bits, and
    /// those are different numbers for the same value. What decides it is the prototype the row
    /// carries, so the count happens after the conversion and the narrowing back to `int` happens
    /// after the count.
    #[test]
    fn the_bit_counts_ask_about_the_width_their_name_says() {
        let text = body("int f(unsigned long long x) { return __builtin_clzll(x); }\n");
        assert!(text.starts_with("block0(%0: i64):"), "counted at eight bytes: {text}");
        assert!(text.contains("%1 = ctlz %0"), "{text}");
        assert!(text.contains("trunc.i32 %1"), "and answered in an int: {text}");

        // The same value asked about at the narrower width, which converts first and so counts
        // something else.
        let text = body("int f(unsigned long long x) { return __builtin_clz(x); }\n");
        assert!(text.contains("trunc.i32 %0"), "narrowed to what was asked about: {text}");
        assert!(text.contains("ctlz %1"), "and counted there: {text}");

        let text = body("int f(unsigned long x) { return __builtin_popcountl(x); }\n");
        assert!(text.contains("%1 = ctpop %0"), "{text}");
        assert!(!text.contains("call"), "{text}");
    }

    /// A parity is whether the count of set bits is odd, which is that count and its low bit.
    ///
    /// Not the machine's parity flag, which on x86-64 is over the low byte of a result and so is a
    /// different question, and not the count itself, since C says the answer is zero or one.
    #[test]
    fn a_parity_is_the_low_bit_of_the_set_bit_count() {
        let text = body("int f(unsigned x) { return __builtin_parity(x); }\n");
        assert!(text.contains("%1 = ctpop %0"), "{text}");
        assert!(text.contains("iconst.i32 1"), "{text}");
        assert!(text.contains("and %1, %2"), "the low bit of it: {text}");
    }

    /// `__builtin_ffs` is the trailing zero count and one, kept only when there was a bit to find.
    ///
    /// The one in the family defined at zero, where it answers zero. Written as a mask rather than
    /// as a branch: the count and the comparison do not depend on each other and both are cheap, so
    /// a branch would buy nothing and cost two blocks and a join.
    #[test]
    fn the_first_set_bit_is_one_based_and_zero_for_a_zero() {
        let text = body("int f(int x) { return __builtin_ffs(x); }\n");
        assert!(text.contains("%1 = cttz %0"), "{text}");
        assert!(text.contains("%4 = add %1, %2"), "one more than the count: {text}");
        assert!(text.contains("%5 = icmp ne %0, %3"), "whether there was a bit at all: {text}");
        assert!(text.contains("%7 = sub %3, %6"), "spread to a mask: {text}");
        assert!(text.contains("%8 = and %4, %7"), "and kept only then: {text}");
        assert!(!text.contains("br_if"), "no branch: {text}");
    }

    /// The three overflow checks are arithmetic and a flag, and not a call to anything.
    ///
    /// gcc has emitted these since 5.0 and there is no object file that defines one, so a call left
    /// standing here would not link. SQLite reaches all three within twenty lines of each other, in
    /// `sqlite3AddInt64` and its two neighbours, which is the reason they were done now.
    ///
    /// The IR instruction answers two things at once, the wrapped value and whether it wrapped,
    /// which is a shape nothing else in the IR has. The store is the builtin writing the answer
    /// through the pointer it was handed.
    #[test]
    fn an_overflow_check_is_arithmetic_and_not_a_call() {
        let text =
            body("int f(int a, int b, int *r) { return __builtin_add_overflow(a, b, r); }\n");
        assert!(text.contains("%3, %4 = sadd_overflow.(i32, i1) %0, %1"), "{text}");
        assert!(text.contains("store %3 -> %2"), "{text}");
        assert!(!text.contains("call"), "{text}");

        let text =
            body("int f(int a, int b, int *r) { return __builtin_sub_overflow(a, b, r); }\n");
        assert!(text.contains("ssub_overflow.(i32, i1) %0, %1"), "{text}");

        let text =
            body("int f(int a, int b, int *r) { return __builtin_mul_overflow(a, b, r); }\n");
        assert!(text.contains("smul_overflow.(i32, i1) %0, %1"), "{text}");

        // Unsigned operands get the unsigned form, which is a different question about the same
        // arithmetic: an unsigned sum wraps where a signed one of the same bits does not.
        let text = body(
            "int f(unsigned a, unsigned b, unsigned *r) { return __builtin_add_overflow(a, b, r); }\n",
        );
        assert!(text.contains("uadd_overflow.(i32, i1) %0, %1"), "{text}");
    }

    /// The arithmetic happens at a type that holds every value all three written types can hold.
    ///
    /// That is what makes the check exact. `unsigned int` and `int` in one call need thirty three
    /// bits between them, so the add is done at sixty four with each operand extended the way its
    /// own signedness says: the unsigned one zero extended, the signed one sign extended. Sign
    /// extending the unsigned one would turn three billion into a negative number before the
    /// addition ever saw it.
    #[test]
    fn an_overflow_check_is_done_at_a_type_that_holds_every_operand() {
        let text = body(
            "int f(unsigned a, int b, long long *r) { return __builtin_add_overflow(a, b, r); }\n",
        );
        assert!(text.contains("%3 = zext.i64 %0"), "the unsigned operand keeps its value: {text}");
        assert!(text.contains("%4 = sext.i64 %1"), "and so does the signed one: {text}");
        assert!(text.contains("sadd_overflow.(i64, i1) %3, %4"), "{text}");

        // Three types that agree need no extension at all, which is what nearly every real call
        // is written as.
        let text = body(
            "int f(long long a, long long b, long long *r) { return __builtin_mul_overflow(a, b, r); }\n",
        );
        assert!(text.contains("smul_overflow.(i64, i1) %0, %1"), "{text}");
        assert!(!text.contains("sext."), "{text}");
        // The one widening left is the answer, which is a bit becoming the `int` C says it is.
        assert!(!text.contains("zext.i64"), "{text}");
    }

    /// The wrapped answer is written through the pointer whether or not it fit.
    ///
    /// That is gcc's rule and it is what makes the builtin usable as a wrapping add with a flag on
    /// the side. A destination narrower than the arithmetic is narrowed and widened back, and the
    /// answer being different is the second half of the test: the instruction says whether the
    /// arithmetic itself needed more room, and the round trip says whether what came out survived
    /// the trip down to where it was going.
    #[test]
    fn an_overflow_check_writes_the_wrapped_answer_whether_or_not_it_fit() {
        let text =
            body("int f(int a, int b, char *r) { return __builtin_sub_overflow(a, b, r); }\n");
        assert!(text.contains("%3, %4 = ssub_overflow.(i32, i1) %0, %1"), "{text}");
        assert!(text.contains("%5 = trunc.i8 %3"), "narrowed to where it goes: {text}");
        assert!(text.contains("%6 = sext.i32 %5"), "and back: {text}");
        assert!(text.contains("%7 = icmp ne %6, %3"), "which is whether it fit: {text}");
        assert!(text.contains("store %5 -> %2"), "the narrowed value is stored either way: {text}");
        assert!(text.contains("%8 = or %4, %7"), "and either bit is an overflow: {text}");
    }

    /// A call needing more than sixty four bits is refused by name rather than got wrong.
    ///
    /// Two ways to reach it: a `__int128` operand, and a sixty four bit unsigned type mixed with a
    /// signed one, which needs sixty five bits to represent both. gcc handles the second by being
    /// cleverer in the mixed case rather than by widening. Until that is written, the message says
    /// what the call needed.
    #[test]
    fn a_call_needing_more_than_sixty_four_bits_says_so() {
        let refused = concat!(
            "int f(unsigned long long a, long long b, long long *r) {\n",
            "    return __builtin_add_overflow(a, b, r);\n",
            "}\n",
        );
        let messages = errors(refused);
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].contains("E0694"), "{messages:?}");
        assert!(messages[0].contains("wider than 64 bits"), "{messages:?}");
    }

    /// An operand that is not an integer at all is the older message, from the type checking every
    /// type generic builtin shares.
    #[test]
    fn an_overflow_check_over_something_that_is_not_an_integer_says_so() {
        let messages =
            errors("int f(double a, int b, int *r) { return __builtin_add_overflow(a, b, r); }\n");
        assert!(messages.iter().any(|line| line.contains("E0671")), "{messages:?}");

        let messages =
            errors("int f(int a, int b, double *r) { return __builtin_add_overflow(a, b, r); }\n");
        assert!(messages.iter().any(|line| line.contains("E0671")), "{messages:?}");
    }

    /// An ordered access is an ordered access in the IR, with the ordering the program wrote.
    ///
    /// Which is the point of the node existing at all. An ordering is not an argument anything is
    /// passed, it is a thing the IR says about an access, so the number in the source is read once
    /// in the front end and after that the ordering travels on the instruction where every pass
    /// that moves code can see it.
    ///
    /// SQLite is why these are done: `AtomicLoad` and `AtomicStore` in `sqlite3.c` are
    /// `__atomic_load_n` and `__atomic_store_n` at the relaxed ordering, and there are thirty five
    /// calls to the pair.
    #[test]
    fn an_ordered_access_is_ordered_in_the_ir() {
        let text = body("int f(int *p) { return __atomic_load_n(p, 0); }\n");
        assert!(text.contains("atomic_load.i32 %0, align 4, relaxed"), "{text}");

        let text = body("long f(long *p) { return __atomic_load_n(p, 2); }\n");
        assert!(text.contains("atomic_load.i64 %0, align 8, acquire"), "{text}");

        let text = body("void f(int *p, int v) { __atomic_store_n(p, v, 3); }\n");
        assert!(text.contains("atomic_store %1 -> %0, align 4, release"), "{text}");

        let text = body("void f(int *p, int v) { __atomic_store_n(p, v, 5); }\n");
        assert!(text.contains("atomic_store %1 -> %0, align 4, seq_cst"), "{text}");

        // The value is converted to what the pointer points at before it is stored, which is what
        // the call would have done if it had a prototype to convert against.
        let text = body("void f(char *p, int v) { __atomic_store_n(p, v, 0); }\n");
        assert!(text.contains("trunc.i8 %1"), "{text}");
        assert!(text.contains("atomic_store %2 -> %0, align 1, relaxed"), "{text}");
    }

    /// On this machine the ordered access is the plain instruction, except at the strongest
    /// ordering of a store.
    ///
    /// x86-64 is total store order: every load is already an acquire and every store is already a
    /// release, and an aligned access no wider than a word is indivisible whether or not anybody
    /// asked. So the whole family is `mov` and the one thing the machine does not give away is a
    /// store staying in front of a later load, which is `mfence` behind the store. Every line below
    /// is what gcc 16.2.0 writes for the same function.
    #[test]
    fn an_ordered_access_is_the_plain_instruction_on_this_machine() {
        let text = asm("int f(int *p) { return __atomic_load_n(p, 5); }\n");
        assert!(text.contains("movl\t(%rdi), %eax"), "{text}");
        assert!(!text.contains("mfence"), "a load needs no barrier here: {text}");

        let text = asm("void f(int *p, int v) { __atomic_store_n(p, v, 3); }\n");
        assert!(text.contains("movl\t%esi, (%rdi)"), "{text}");
        assert!(!text.contains("mfence"), "a release store needs no barrier here: {text}");

        let text = asm("void f(int *p, int v) { __atomic_store_n(p, v, 5); }\n");
        let (before, after) = text.split_once("mfence").expect("a barrier: {text}");
        assert!(before.contains("movl\t%esi, (%rdi)"), "the store comes first: {text}");
        assert!(!after.contains("movl"), "and nothing else is between them: {text}");
    }

    /// A barrier is one instruction at the strongest ordering and no instruction below it.
    ///
    /// The same reasoning the other way round. An acquire, a release and an acquire release fence
    /// are already true of every program running on this machine, and what a program wanted from
    /// one is that the compiler not move accesses across it, which is already so by the time any
    /// instruction is picked. Sequential consistency is the one that costs something.
    ///
    /// `__sync_synchronize` is the older family's spelling of the strongest one and compiles to
    /// exactly the same instruction, which is what SQLite calls twice in `sqlite3.c`.
    #[test]
    fn a_barrier_is_one_instruction_at_the_strongest_ordering_and_none_below_it() {
        assert!(asm("void f(void) { __atomic_thread_fence(5); }\n").contains("mfence"));
        assert!(asm("void f(void) { __sync_synchronize(); }\n").contains("mfence"));

        for weaker in ["1", "2", "3", "4"] {
            let source = format!("void f(void) {{ __atomic_thread_fence({weaker}); }}\n");
            assert!(!asm(&source).contains("mfence"), "{weaker} costs nothing here");
        }
    }

    /// A memory order an operation cannot carry is read as the strongest one, and said so about.
    ///
    /// There are three ways the number is not one the operation can take: it is not a constant at
    /// all, it is not one of the six the headers define, or it is one of them and means nothing for
    /// this operation, which is a release load or an acquire store. All three become sequential
    /// consistency, which is stronger than anything the program could have meant, so a program that
    /// wrote nonsense gets a correct answer rather than a fast one. gcc does the same.
    ///
    /// The last two also warn, because the number was written down and is wrong. The first does
    /// not: gcc takes a computed order, and so does the C11 spelling, so a warning there would fire
    /// on correct programs.
    #[test]
    fn a_memory_order_an_operation_cannot_carry_is_read_as_the_strongest() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;

        let acquire_store = run(&opts, "void f(int *p, int v) { __atomic_store_n(p, v, 2); }\n");
        assert!(acquire_store.text().contains("seq_cst"), "{:?}", acquire_store.text());
        assert!(acquire_store.messages[0].contains("[W0333]"), "{:?}", acquire_store.messages);

        let nonsense = run(&opts, "int f(int *p) { return __atomic_load_n(p, 99); }\n");
        assert!(nonsense.text().contains("seq_cst"), "{:?}", nonsense.text());
        assert!(nonsense.messages[0].contains("[W0333]"), "{:?}", nonsense.messages);

        let computed = run(&opts, "int f(int *p, int n) { return __atomic_load_n(p, n); }\n");
        assert!(computed.text().contains("seq_cst"), "{:?}", computed.text());
        assert_eq!(computed.messages, Vec::<String>::new(), "a computed order is not a mistake");
    }

    /// A conversion between a float and the widest unsigned integer, which the machine has not got.
    ///
    /// Every other conversion between a float and an integer is the signed one at some width with a
    /// widening in front or a narrowing behind. These two are not, because there is no signed width
    /// that holds every value of an unsigned sixty four bit integer, so each is the signed
    /// conversion with arithmetic around it that brings the value into range and puts it back.
    ///
    /// What is checked here is that the conversion happens at all and that it happens without a
    /// branch. gcc writes a branch for both; this writes the choice as a mask, because every rewrite
    /// in that pass stays inside the block it started in. The arithmetic itself is checked in
    /// `rucc-codegen`, where it can be run against the answer rather than read in the assembly.
    #[test]
    fn a_conversion_between_a_float_and_the_widest_unsigned_integer_is_written_without_a_branch() {
        let text = asm("double f(unsigned long long x) { return (double)x; }\n");
        assert!(text.contains("cvtsi2sdq"), "the signed conversion is what runs: {text}");
        assert!(text.contains("shrq"), "with the value halved first: {text}");
        assert!(text.contains("addsd"), "and doubled after: {text}");
        assert!(!text.contains("\tj"), "and no branch anywhere: {text}");

        let text = asm("unsigned long long f(double d) { return (unsigned long long)d; }\n");
        assert!(text.contains("cvttsd2siq"), "the signed conversion is what runs: {text}");
        assert!(text.contains("subsd"), "with half the range taken off first: {text}");
        assert!(text.contains("shlq\t$63"), "and the top bit put back: {text}");
        assert!(!text.contains("\tj"), "and no branch anywhere: {text}");
    }

    /// The plain names are the library's only where nothing else has taken them.
    ///
    /// Four ways a program says it means something else. A `static` definition is its own
    /// function and the name outside the file is somebody else's. A declaration of another type
    /// is another function. `-fno-builtin` and `-fno-builtin-<name>` say so outright, and
    /// `-ffreestanding` says there is no C library for the name to be the name of. Every one of
    /// these was measured against gcc 16.2.0, which calls the program's function in all of them.
    ///
    /// The `__builtin_` spelling goes on meaning the library's function through all of it, which
    /// is what the prefix is for and what lets a freestanding build reach one deliberately.
    #[test]
    fn a_plain_name_the_program_took_is_the_programs_own_function() {
        let taken = concat!(
            "static long long llabs(long long b) { return 7; }\n",
            "long long f(long long x) { return llabs(x); }\n",
        );
        assert!(ir(taken).contains("call @llabs"), "a static definition is the program's own");

        let retyped = concat!("int llabs(int b);\n", "int f(int x) { return llabs(x); }\n",);
        assert!(ir(retyped).contains("call @llabs"), "another type is another function");

        let plain = concat!(
            "long long llabs(long long b);\n",
            "long long f(long long x) { return llabs(x); }\n",
        );
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        assert!(!run(&opts, plain).text().contains("call @llabs"), "the library's by default");

        opts.builtins = false;
        assert!(run(&opts, plain).text().contains("call @llabs"), "-fno-builtin");

        opts.builtins = true;
        opts.no_builtin = vec!["llabs".to_owned()];
        assert!(run(&opts, plain).text().contains("call @llabs"), "-fno-builtin-llabs");
        let one = "long labs(long b);\nlong f(long x) { return labs(x); }\n";
        assert!(!run(&opts, one).text().contains("call @labs"), "one name and not the family");

        // `-ffreestanding` reaches the front end as the same answer, which is what the driver
        // does with it in `compile`, and the prefixed spelling is untouched by any of it.
        opts.no_builtin = Vec::new();
        opts.builtins = false;
        let prefixed = "long long f(long long x) { return __builtin_llabs(x); }\n";
        assert!(!run(&opts, prefixed).text().contains("call @llabs"), "the prefix is a promise");
    }

    /// The hint builtins are their first argument, and nothing is left of the hint.
    ///
    /// Which way a branch is expected to go is the whole of what they say, and there is nothing
    /// here that reads a branch weight yet, so what reaches the IR is the value and the hint is
    /// gone. The one thing the prototype has to keep doing is converting: gcc gives both of them
    /// a `long` result, so `sizeof(__builtin_expect((char)1, 1))` is eight and a narrower argument
    /// widens before it is answered with.
    ///
    /// The arguments after the first are checked and then dropped, so a side effect in one does
    /// not happen. That is what gcc does with them too, measured on gcc 16.2.0: the `i` below
    /// comes back zero there as well.
    #[test]
    fn the_hint_builtins_are_their_first_argument_and_the_hint_leaves_no_trace() {
        let text = ir(concat!(
            "long a = __builtin_expect(7, 1);\n",
            "long b = __builtin_expect_with_probability(9, 1, 0.9);\n",
            "unsigned long c = sizeof(__builtin_expect((char)1, 1));\n",
        ));
        assert!(text.contains("global @a : i64 = 7,"), "{text}");
        assert!(text.contains("global @b : i64 = 9,"), "{text}");
        assert!(text.contains("global @c : i64 = 8,"), "{text}");
        assert!(!text.contains("__builtin_expect"), "it is not a call to anything:\n{text}");

        // A narrower argument is widened by the prototype before it is handed back, and it is
        // widened with its sign, since the parameter is a signed `long`.
        let text = body("long f(char c) { return __builtin_expect(c, 1); }\n");
        assert!(text.contains("sext"), "{text}");

        // The second argument is not evaluated, so `i` is still zero, and neither is the third.
        // What is left of each statement is the first argument widened, which nothing reads and
        // which the first pass that looks for dead code will take out.
        let one = "block0:\n    %0 = iconst.i32 0\n    %1 = iconst.i32 1\n    %2 = sext.i64 %1\n    return %0\n";
        assert_eq!(body("int f(void) { int i = 0; __builtin_expect(1, i++); return i; }\n"), one);
        let source = "int g(void) { int i = 0; __builtin_expect_with_probability(1, i++, 0.5); return i; }\n";
        assert_eq!(body(source), one);
    }

    /// A point control does not arrive at, in both of the ways the compiler has one.
    ///
    /// `__builtin_unreachable()` is the promise written down, and a function whose body can run
    /// off the bottom is the walk arriving at the same place on its own. Neither writes an
    /// instruction, which is what gcc 16.2.0 does at `-O0`: it emits the epilogue and the `ret`
    /// for both of the functions below and nothing else, and the two of them come out byte for
    /// byte the same there.
    ///
    /// The `ret` is the part worth holding on to. It is not there because anything runs it, it is
    /// there because a function whose last instruction is not a return is one that falls into
    /// whatever the assembler puts after it.
    #[test]
    fn a_promise_that_control_does_not_arrive_writes_no_instruction() {
        let promised = "int f(int x) { if (x) return 1; __builtin_unreachable(); }\n";
        let text = ir(promised);
        assert!(text.contains("    unreachable_hint\n"), "{text}");
        assert!(!text.contains("call"), "it is not a call to anything:\n{text}");

        // The statement after it is still lowered. Continuing to translate a path the program
        // promised is dead is one of the things a compiler may do with undefined behaviour, and
        // it is the one that keeps a program built at `-O0` behaving the way it was watched to.
        let after = body("int g(int x) { __builtin_unreachable(); return x; }\n");
        assert!(after.contains("return"), "{after}");

        // Both functions are the same instructions, because the hint writes none of them and the
        // terminator underneath it writes none either.
        let text = asm(promised);
        let mine = text.split_once("\nf:\n").expect("a definition").1;
        let mine = mine.split_once("\t.size").expect("a definition").0;
        let plain = asm("int f(int x) { if (x) return 1; }\n");
        let plain = plain.split_once("\nf:\n").expect("a definition").1;
        let plain = plain.split_once("\t.size").expect("a definition").0;
        assert_eq!(mine, plain);
        assert!(mine.trim_end().ends_with("ret"), "{mine}");
        assert!(!mine.contains("ud2"), "{mine}");
    }

    /// The two names stay apart, which is what having both of them is for.
    ///
    /// The one the program wrote is what the call is checked against and what a diagnostic about
    /// it says, and the one the library defines is what the call ends up carrying. A compiler
    /// that kept only the second would report this against `abort`, which is a function the
    /// program never mentions.
    #[test]
    fn a_library_builtin_is_diagnosed_under_the_name_the_program_wrote() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let messages = run(&opts, "void f(void) { __builtin_abort(1); }\n").messages;
        assert!(
            messages.iter().any(|m| m.contains("__builtin_abort")),
            "expected the written name in {messages:?}"
        );
    }

    /// A builtin nothing lowers is refused where it is written, rather than at the link.
    ///
    /// The names are one from each shape the table holds: a `__builtin_` with a prototype, one
    /// whose type comes from the call it was written in, and one from each of the two older
    /// families whose prefix is not `__builtin_`. What the message has to carry is the name,
    /// because the whole complaint about the link error this replaces is that the name in it was
    /// one the compiler chose.
    #[test]
    fn a_builtin_nothing_lowers_is_refused_by_name() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        for (builtin, call) in [
            ("__builtin_return_address", "(int)(long)__builtin_return_address(0)"),
            ("__builtin_alloca", "(int)(long)__builtin_alloca(8)"),
            ("__atomic_exchange_n", "__atomic_exchange_n(&counter, 1, 0)"),
            ("__sync_fetch_and_add", "(int)__sync_fetch_and_add(&counter, 1)"),
        ] {
            let source = format!("int counter;\nint f(void) {{ return {call}; }}\n");
            let messages = run(&opts, &source).messages;
            let named = messages.iter().any(|m| m.contains(builtin) && m.contains("E0686"));
            assert!(named, "expected {builtin} to be refused by name in {messages:?}");
        }
    }

    /// The refusal is about a call and not about the name, so the rest of what C does with one
    /// still works.
    ///
    /// `sizeof` does not evaluate its operand, so nothing is called and there is nothing to
    /// refuse; the type of the call is what it asks for and that comes from the front end. A
    /// program that defines the name itself gets the function it wrote, which is not what this
    /// is for but is what a definition in front of us means.
    #[test]
    fn what_is_refused_is_the_call_and_not_the_name() {
        let text = ir("unsigned long n = sizeof(__builtin_return_address(0));\n");
        assert!(text.contains("global @n : i64 = 8,"), "{text}");

        let text = ir(concat!(
            "void *__builtin_return_address(unsigned x) { return 0; }\n",
            "void *f(void) { return __builtin_return_address(0); }\n",
        ));
        assert!(text.contains("call @__builtin_return_address"), "{text}");
    }

    /// A `static` function nothing refers to is not emitted, and one that is refered to is.
    ///
    /// The pair is written as one program so that the two answers come out of one walk. What
    /// makes the difference is the call in `main` and nothing else about either definition.
    #[test]
    fn a_static_function_nothing_refers_to_is_not_emitted() {
        let text = ir("static int dropped(void) { return 1; }\n\
                       static int kept(void) { return 2; }\n\
                       int main(void) { return kept(); }\n");
        assert!(text.contains("func @kept"), "{text}");
        assert!(!text.contains("dropped"), "{text}");
    }

    /// The set is transitive, so two of them that only call each other are both dropped.
    ///
    /// Counting the references to a name would keep this pair, since each is named once, and
    /// that is the mistake this is here to catch: what decides it is whether a root reaches the
    /// definition, and a root is something the file has a reason to emit on its own.
    #[test]
    fn two_static_functions_that_only_call_each_other_are_both_dropped() {
        let text = ir("static int ping(void);\n\
                       static int pong(void) { return ping(); }\n\
                       static int ping(void) { return pong(); }\n\
                       int main(void) { return 0; }\n");
        assert!(!text.contains("ping"), "{text}");
        assert!(!text.contains("pong"), "{text}");
    }

    /// Everything that names a function keeps it, whether or not the name is being called.
    ///
    /// An address taken in a body, an image that holds one, and a body that is only reached
    /// through another `static` function are three different ways for a definition to be needed
    /// and none of them is a call at the top level of a reachable function.
    #[test]
    fn naming_a_static_function_anywhere_keeps_it() {
        let text = ir("static int by_address(void) { return 1; }\n\
                       static int in_an_image(void) { return 2; }\n\
                       static int deeper(void) { return 3; }\n\
                       static int reaches_deeper(void) { return deeper(); }\n\
                       static int (*table[1])(void) = {in_an_image};\n\
                       int main(void) {\n\
                         int (*p)(void) = by_address;\n\
                         return p() + table[0]() + reaches_deeper();\n\
                       }\n");
        for kept in ["by_address", "in_an_image", "deeper", "reaches_deeper"] {
            assert!(text.contains(&format!("func @{kept}")), "expected {kept} in:\n{text}");
        }
    }

    /// An attribute that says something outside the file reaches it keeps the definition.
    ///
    /// None of the five is implemented as anything else yet, and this is the part of each of
    /// them that a program notices first: a symbol a linker script names or a function the
    /// run-up to `main` calls is not written about anywhere a C file can see.
    #[test]
    fn an_attribute_keeps_a_static_function_nothing_refers_to() {
        for attribute in ["used", "retain", "constructor", "destructor", "__used__"] {
            let source = format!(
                "__attribute__(({attribute})) static int kept(void) {{ return 1; }}\n\
                 int main(void) {{ return 0; }}\n"
            );
            let text = ir(&source);
            assert!(text.contains("func @kept"), "for {attribute}:\n{text}");
        }
    }

    /// A function with external linkage is emitted whatever this file does with it, because
    /// another one may call it, and that is what external linkage is.
    #[test]
    fn a_function_anything_could_call_is_emitted_without_being_called() {
        let text =
            ir("int nobody_here_calls_it(void) { return 1; }\nint main(void) { return 0; }\n");
        assert!(text.contains("func @nobody_here_calls_it"), "{text}");
    }

    /// Four of the classification builtins are operators C already has, and become those.
    ///
    /// What the standard's macro promises over the operator is that it does not raise the
    /// invalid operation exception on a quiet NaN. This compiler does not model floating point
    /// exceptions, so there is nothing left for a node of its own to carry and a second way of
    /// spelling a comparison would be a second thing every pass has to know about.
    #[test]
    fn a_classification_c_has_an_operator_for_is_that_operator() {
        for (builtin, operator) in [
            ("__builtin_isgreater", "binary >"),
            ("__builtin_isgreaterequal", "binary >="),
            ("__builtin_isless", "binary <"),
            ("__builtin_islessequal", "binary <="),
        ] {
            let source = format!("int f(double x, double y) {{ return {builtin}(x, y); }}\n");
            let text = tast(&source);
            assert!(text.contains(&format!("{operator} : int")), "for {builtin}:\n{text}");
        }
    }

    /// The rest of the family are comparisons in the IR and never a call to anything.
    ///
    /// `math.h` defines the macro of each of these names as the builtin of the same name, so
    /// there is no function under any of them for a call to reach. `isunordered` and
    /// `islessgreater` are predicates the IR's comparison already has, `isnan` is the value that
    /// is unordered with itself, and the two that ask about a magnitude are written against the
    /// infinities. `signbit` is the one that is not a question about the value, since a negative
    /// zero compares equal to a positive one, so its answer comes from the bits.
    #[test]
    fn the_classification_builtins_are_comparisons_and_not_calls() {
        let text = body("int f(double x, double y) { return __builtin_isunordered(x, y); }\n");
        assert_eq!(
            text,
            "block0(%0: f64, %1: f64):\n    %2 = fcmp uno %0, %1\n    %3 = zext.i32 \
                          %2\n    return %3\n"
        );

        // Not `x != y`, which is true when the two are unordered and so is true of a NaN.
        let text = body("int f(double x, double y) { return __builtin_islessgreater(x, y); }\n");
        assert!(text.contains("fcmp one %0, %1"), "{text}");

        let text = body("int f(double x) { return __builtin_isnan(x); }\n");
        assert!(text.contains("fcmp uno %0, %0"), "{text}");

        let text = body("int f(double x) { return __builtin_isinf(x); }\n");
        assert!(text.contains("fconst.f64 0x7ff0000000000000"), "{text}");
        assert!(text.contains("fconst.f64 0xfff0000000000000"), "{text}");
        assert!(text.contains("%3 = fcmp oeq %0, %1"), "{text}");
        assert!(text.contains("%4 = fcmp oeq %0, %2"), "{text}");
        assert!(text.contains("%5 = or %3, %4"), "{text}");

        // Strictly between the two infinities, which a NaN is not, because an ordered comparison
        // against either of them is false. That is what makes this one test rather than two.
        let text = body("int f(double x) { return __builtin_isfinite(x); }\n");
        assert!(text.contains("%3 = fcmp olt %2, %0"), "{text}");
        assert!(text.contains("%4 = fcmp olt %0, %1"), "{text}");
        assert!(text.contains("%5 = and %3, %4"), "{text}");

        let text = body("int f(double x) { return __builtin_signbit(x); }\n");
        assert!(text.contains("%1 = bitcast.i64 %0"), "{text}");
        assert!(text.contains("icmp slt %1, %2"), "{text}");

        // The same question of a value in the target's widest format, where the bits are eighty
        // and the object they sit in is sixteen bytes.
        let text = body("int f(long double x) { return __builtin_signbitl(x); }\n");
        assert!(text.contains("%1 = bitcast.i80 %0"), "{text}");

        // The operand is evaluated once however many times it is compared, which is the whole
        // reason these are nodes rather than a rewriting into the operators.
        let text = body("double g(void);\nint f(void) { return __builtin_isnan(g()); }\n");
        assert_eq!(text.matches("call @g()").count(), 1, "{text}");
    }

    /// A spelling that names a width converts its argument before it asks.
    ///
    /// gcc gives `__builtin_isinff` a `float` parameter and `__builtin_isinf` no parameter type
    /// at all, and the difference is visible rather than academic: `1e300` does not fit in a
    /// `float`, so converting it first is an infinity and not converting it is not. Both numbers
    /// here are what gcc 16 gives.
    #[test]
    fn a_classification_spelling_that_names_a_width_converts_before_it_asks() {
        let text = ir(concat!(
            "int a = __builtin_isinff(1e300);\n",
            "int b = __builtin_isinf(1e300);\n",
            // Folded here rather than compared at run time, because a question about a value has
            // an answer as soon as the value is a constant, and an initializer for an object
            // with static storage duration has to have one.
            "int c = __builtin_isnan(0.0);\n",
            "int d = __builtin_signbit(-0.0);\n",
            "int e = __builtin_islessgreater(1.0, 2.0);\n",
        ));
        assert!(text.contains("global @a : i32 = 1,"), "{text}");
        assert!(text.contains("global @b : i32 = 0,"), "{text}");
        assert!(text.contains("global @c : i32 = 0,"), "{text}");
        assert!(text.contains("global @d : i32 = 1,"), "{text}");
        assert!(text.contains("global @e : i32 = 1,"), "{text}");
    }

    /// An argument that is not floating point is refused, in gcc's words.
    #[test]
    fn a_classification_builtin_refuses_an_argument_that_is_not_floating_point() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let source = concat!(
            "int a(int x) { return __builtin_isnan(x); }\n",
            "int b(int x, int y) { return __builtin_isunordered(x, y); }\n",
            "int c(double x) { return __builtin_isnan(x, x); }\n",
        );
        let messages = run(&opts, source).messages;
        assert_eq!(
            messages,
            [
                "/main.c:1:23: error: non-floating-point argument in call to function \
                 '__builtin_isnan' [E0685]",
                "/main.c:2:30: error: non-floating-point arguments in call to function \
                 '__builtin_isunordered' [E0685]",
                "/main.c:3:26: error: too many arguments to function '__builtin_isnan' [E0511]",
            ]
        );
    }

    /// The three of the family that need a constant of the format other than an infinity.
    ///
    /// `isnormal` is the one that needs the smallest normal, and it is asked of the magnitude, so
    /// the sign comes off first and what is left is the same shape as `isfinite`. `isinf_sign` is
    /// the one whose answer is a number: the two comparisons `isinf` builds, subtracted rather
    /// than combined. `fpclassify` is four questions of one value and five answers to pick from,
    /// and the picking is a mask because all five are constants and neither of them can have an
    /// effect.
    #[test]
    fn the_last_three_classification_builtins_are_comparisons_and_not_calls() {
        let text = body("int f(double x) { return __builtin_isnormal(x); }\n");
        // The sign off, which is the magnitude, and then the range, asked of the bits rather than
        // of the number, since the encoding of a value whose sign bit is clear rises with the
        // value in every format this compiles for.
        assert!(text.contains("%1 = bitcast.i64 %0"), "{text}");
        assert!(text.contains("%2 = iconst.i64 9223372036854775807"), "{text}");
        assert!(text.contains("%3 = and %1, %2"), "{text}");
        assert!(text.contains("%4 = iconst.i64 4503599627370496"), "{text}");
        assert!(text.contains("%5 = iconst.i64 9218868437227405312"), "{text}");
        assert!(text.contains("%6 = icmp uge %3, %4"), "{text}");
        assert!(text.contains("%7 = icmp ult %3, %5"), "{text}");
        assert!(text.contains("%8 = and %6, %7"), "{text}");

        // The same question in the target's widest format, where the smallest normal has the
        // leading significand bit stored rather than implied, so its encoding is two bits and not
        // one.
        let text = body("int f(long double x) { return __builtin_isnormal(x); }\n");
        assert!(text.contains("%4 = iconst.i80 27670116110564327424"), "{text}");
        assert!(text.contains("%5 = iconst.i80 604453686435277732577280"), "{text}");

        let text = body("int f(double x) { return __builtin_isinf_sign(x); }\n");
        assert!(text.contains("%3 = fcmp oeq %0, %1"), "{text}");
        assert!(text.contains("%4 = fcmp oeq %0, %2"), "{text}");
        assert!(text.contains("%7 = sub %5, %6"), "{text}");

        let text = body("int f(double x) { return __builtin_fpclassify(0, 1, 2, 3, 4, x); }\n");
        assert!(text.contains("fcmp uno %0, %0"), "{text}");
        assert!(text.contains("fcmp oeq %0, %6"), "{text}");
        // Four questions, each of them a bit widened into the type of the answer and then spread
        // into a mask that picks between the answer and whatever the questions after it settled
        // on. Nothing sign extends, because no rule lowers a sign extension out of one bit.
        assert_eq!(text.matches(" = zext.i32 ").count(), 4, "{text}");
        assert_eq!(text.matches(" = xor ").count(), 4, "{text}");
        assert!(!text.contains("call"), "{text}");

        // The value is evaluated once however many questions are asked of it, which is the whole
        // reason `fpclassify` is a node rather than the chain of tests it turns into.
        let text = body(concat!(
            "double g(void);\n",
            "int f(void) { return __builtin_fpclassify(0, 1, 2, 3, 4, g()); }\n",
        ));
        assert_eq!(text.matches("call @g()").count(), 1, "{text}");
    }

    /// Each of the three answers a constant where its operand is one.
    ///
    /// glibc's `fpclassify` macro is exactly this builtin, so a program that writes
    /// `fpclassify(0.0)` in a static initializer is writing this, and it has to have a value at
    /// translation time or the program is refused rather than merely compiled slowly. Every
    /// number here is what gcc 16 gives.
    #[test]
    fn the_last_three_classification_builtins_fold_where_their_operand_is_a_constant() {
        let text = ir(concat!(
            "int a = __builtin_isnormal(1.0);\n",
            "int b = __builtin_isnormal(0.0);\n",
            "int c = __builtin_isnormal(1.0 / 0.0);\n",
            "int d = __builtin_isinf_sign(-1.0 / 0.0);\n",
            "int e = __builtin_isinf_sign(1.0);\n",
            "int g = __builtin_fpclassify(0, 1, 2, 3, 4, 0.0);\n",
            "int h = __builtin_fpclassify(0, 1, 2, 3, 4, 1.0);\n",
            "int i = __builtin_fpclassify(0, 1, 2, 3, 4, 1.0 / 0.0);\n",
        ));
        assert!(text.contains("global @a : i32 = 1,"), "{text}");
        assert!(text.contains("global @b : i32 = 0,"), "{text}");
        assert!(text.contains("global @c : i32 = 0,"), "{text}");
        assert!(text.contains("global @d : i32 = -1,"), "{text}");
        assert!(text.contains("global @e : i32 = 0,"), "{text}");
        assert!(text.contains("global @g : i32 = 4,"), "{text}");
        assert!(text.contains("global @h : i32 = 2,"), "{text}");
        assert!(text.contains("global @i : i32 = 1,"), "{text}");
    }

    /// `fpclassify` refuses what gcc refuses, in gcc's words.
    ///
    /// The five answers have to be integer constant expressions, because what the builtin does is
    /// pick one of them and a pick between values that are not known here would be a chain of
    /// conditionals over expressions the call has already evaluated.
    #[test]
    fn fpclassify_refuses_an_answer_that_is_not_an_integer_constant() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let source = concat!(
            "int a(double x, int n) { return __builtin_fpclassify(0, 1, n, 3, 4, x); }\n",
            "int b(double x) { return __builtin_fpclassify(0, 1, 2, 3, x); }\n",
            "int c(int x) { return __builtin_fpclassify(0, 1, 2, 3, 4, x); }\n",
        );
        let messages = run(&opts, source).messages;
        assert_eq!(
            messages,
            [
                "/main.c:1:60: error: non-const integer argument 3 in call to function \
                 '__builtin_fpclassify' [E0687]",
                "/main.c:2:26: error: too few arguments to function '__builtin_fpclassify' \
                 [E0511]",
                "/main.c:3:23: error: non-floating-point argument in call to function \
                 '__builtin_fpclassify' [E0685]",
            ]
        );
    }

    /// A builtin whose answer is a constant is one, and is not a call to the library.
    ///
    /// This is the reason the family is answered in the front end at all. `double x =
    /// __builtin_inf();` at file scope initializes an object with static storage duration, so
    /// there is no point in the program at which a call could be made, and a compiler that
    /// lowered it to one would reject a program gcc accepts. Every number here is the encoding
    /// gcc 16 gives on x86-64.
    #[test]
    fn a_builtin_whose_answer_is_a_constant_is_one_and_not_a_call() {
        let text = ir(concat!(
            "double a = __builtin_inf();\n",
            "float b = __builtin_huge_valf();\n",
            "long double c = __builtin_infl();\n",
            "double d = __builtin_huge_val();\n",
        ));
        assert!(text.contains("global @a : f64 = 0x7ff0000000000000,"), "{text}");
        assert!(text.contains("global @b : f32 = 0x7f800000,"), "{text}");
        assert!(text.contains("f80 0x7fff8000000000000000"), "{text}");
        assert!(text.contains("global @d : f64 = 0x7ff0000000000000,"), "{text}");
        assert!(!text.contains("call"), "{text}");
    }

    /// A nan is written with the payload the program asked for.
    ///
    /// The string is read the way `strtoull` reads a number, which is what the library function
    /// of the same name does with it, and a string that is not one at all leaves the call for the
    /// library to answer at run time. A quiet nan has the high fraction bit set and a signalling
    /// one does not, except that a signalling nan with nothing in it would be an infinity, so it
    /// gets the next bit down instead. Every encoding here was measured against gcc 16, the two
    /// `long double` ones on a machine with the x87 format.
    #[test]
    fn a_nan_is_written_with_the_payload_the_program_asked_for() {
        let text = ir(concat!(
            "double a = __builtin_nan(\"\");\n",
            "double b = __builtin_nan(\"0x1\");\n",
            // Octal, since there is a leading zero, so this is eight and not ten.
            "double c = __builtin_nan(\"010\");\n",
            "double d = __builtin_nans(\"\");\n",
            "double e = __builtin_nans(\"0x1\");\n",
            "float f = __builtin_nanf(\"0x1\");\n",
            "float g = __builtin_nansf(\"\");\n",
            "long double h = __builtin_nansl(\"\");\n",
        ));
        assert!(text.contains("global @a : f64 = 0x7ff8000000000000,"), "{text}");
        assert!(text.contains("global @b : f64 = 0x7ff8000000000001,"), "{text}");
        assert!(text.contains("global @c : f64 = 0x7ff8000000000008,"), "{text}");
        assert!(text.contains("global @d : f64 = 0x7ff4000000000000,"), "{text}");
        assert!(text.contains("global @e : f64 = 0x7ff0000000000001,"), "{text}");
        assert!(text.contains("global @f : f32 = 0x7fc00001,"), "{text}");
        assert!(text.contains("global @g : f32 = 0x7fa00000,"), "{text}");
        assert!(text.contains("f80 0x7fffa000000000000000"), "{text}");

        // A payload that is not a number, and one that is not known until run time, are both
        // left to the library, which is the same thing gcc emits for either of them.
        let text = ir(concat!(
            "double f(const char *p) { return __builtin_nan(p); }\n",
            "double g(void) { return __builtin_nans(\"1x\"); }\n",
        ));
        assert_eq!(text.matches("call @nan(").count(), 1, "{text}");
        assert_eq!(text.matches("call @nans(").count(), 1, "{text}");
    }

    /// The length and the order of a string literal are known here.
    ///
    /// A program that asks for either of them is asking about something the translation already
    /// has in front of it, and folding is not only an optimization: `execute/921007-1.c` in the
    /// torture suite calls `__builtin_strcmp` in a file that defines its own `strcmp` with a
    /// different signature, so leaving the call behind is a name collision that gcc does not
    /// have. The comparison is over `unsigned char`, which is why the second one is negative.
    #[test]
    fn the_length_and_the_order_of_a_string_literal_are_known_here() {
        let text = ir(concat!(
            "unsigned long a = __builtin_strlen(\"hello\");\n",
            "unsigned long b = __builtin_strlen(\"a\\0bc\");\n",
            "int c = __builtin_strcmp(\"X\", \"X\\376\") < 0;\n",
            "int d = __builtin_strcmp(\"abc\", \"abc\");\n",
            "int e = __builtin_strcmp(\"abc\", \"ab\") > 0;\n",
        ));
        assert!(text.contains("global @a : i64 = 5,"), "{text}");
        assert!(text.contains("global @b : i64 = 1,"), "{text}");
        assert!(text.contains("global @c : i32 = 1,"), "{text}");
        assert!(text.contains("global @d : i32 = 0,"), "{text}");
        assert!(text.contains("global @e : i32 = 1,"), "{text}");
        assert!(!text.contains("call"), "{text}");

        // An argument that is not a literal is the library's to answer, as it has to be.
        let text = ir("unsigned long f(const char *p) { return __builtin_strlen(p); }\n");
        assert!(text.contains("call @strlen("), "{text}");
    }

    /// A sign builtin is a mask over the bits, and is not a call.
    ///
    /// `fabs` and `copysign` are in the math library rather than the C one, so a program that
    /// only ever wrote the prefixed spelling never asked for `-lm` and a call left behind here
    /// would not link. Neither needs anything the library has: one clears the sign bit and the
    /// other takes it from the second operand, and every other bit goes through untouched.
    #[test]
    fn a_sign_builtin_is_a_mask_over_the_bits_and_not_a_call() {
        let text = body("double f(double x) { return __builtin_fabs(x); }\n");
        assert!(text.contains("bitcast.i64 %0"), "{text}");
        assert!(text.contains("iconst.i64 9223372036854775807"), "{text}");
        assert!(text.contains("and %1, %2"), "{text}");
        assert!(text.contains("bitcast.f64 %3"), "{text}");
        assert!(!text.contains("call"), "{text}");

        let text = body("double f(double x, double y) { return __builtin_copysign(x, y); }\n");
        assert!(text.contains("iconst.i64 -9223372036854775808"), "{text}");
        assert!(text.contains("%8 = or %4, %7"), "{text}");
        assert!(!text.contains("call"), "{text}");

        // The x87 format, whose value is eighty bits sitting in an object of sixteen. The mask is
        // as wide as the value and not as wide as the object, so the padding is not part of it.
        let text = body("long double f(long double x) { return __builtin_fabsl(x); }\n");
        assert!(text.contains("bitcast.i80 %0"), "{text}");
        assert!(text.contains("bitcast.f80"), "{text}");

        // The width a name does not spell out is `double`, so a `float` argument widens first and
        // the answer is a `double`, which is what gcc's declaration of it says.
        let text = body("double f(float x) { return __builtin_fabs(x); }\n");
        assert!(text.contains("fpext.f64 %0"), "{text}");
        assert!(text.contains("bitcast.i64 %1"), "{text}");
    }

    /// The sign builtins answer a zero and a nan the way the bits say.
    ///
    /// This is why they are described over the bits rather than written with comparisons and
    /// negation. A negative zero compares equal to a positive one and has a sign bit to clear,
    /// and a nan compares equal to nothing at all and keeps its payload through both operations.
    /// `execute/ieee/copysign1.c` in the torture suite is the test that notices, because it
    /// compares its answers with `memcmp`. Every number here is what gcc 16 gives, the two in the
    /// x87 format measured on a machine that has it.
    #[test]
    fn the_sign_builtins_answer_a_zero_and_a_nan_the_way_the_bits_say() {
        let text = ir(concat!(
            "double a = __builtin_fabs(-3.5);\n",
            "double b = __builtin_copysign(1.0, -0.0);\n",
            "double c = __builtin_copysign(0.0, -2.0);\n",
            // The payload survives both, and only the sign bit moves.
            "double d = __builtin_copysign(-__builtin_nan(\"\"), 1.0);\n",
            "double e = __builtin_fabs(-__builtin_nan(\"0x1\"));\n",
            "float g = __builtin_copysignf(-0.0f, 2.0f);\n",
            "long double h = __builtin_copysignl(1.0L, -1.0L);\n",
            "long double i = __builtin_fabsl(-__builtin_infl());\n",
        ));
        assert!(text.contains("global @a : f64 = 0x400c000000000000,"), "{text}");
        assert!(text.contains("global @b : f64 = 0xbff0000000000000,"), "{text}");
        assert!(text.contains("global @c : f64 = 0x8000000000000000,"), "{text}");
        assert!(text.contains("global @d : f64 = 0x7ff8000000000000,"), "{text}");
        assert!(text.contains("global @e : f64 = 0x7ff8000000000001,"), "{text}");
        assert!(text.contains("global @g : f32 = 0x0,"), "{text}");
        assert!(text.contains("f80 0xbfff8000000000000000"), "{text}");
        assert!(text.contains("f80 0x7fff8000000000000000"), "{text}");
    }

    /// A `constexpr` object is a named constant, which is the whole reason the keyword exists.
    ///
    /// C23 6.6p8 puts two of them on the list an integer constant expression is built from: one
    /// of an arithmetic type, and a member of one of a structure or union type. A subscript of
    /// one is not on the list and is a variably modified type in gcc 16 as well, and every
    /// number here is what gcc 16 gives on x86-64.
    #[test]
    fn a_constexpr_object_is_a_constant_wherever_one_is_required() {
        let text = ir(concat!(
            "constexpr int side = 4;\n",
            "constexpr int wider = side + 1;\n",
            "constexpr double half = 1.5;\n",
            "struct point { int x; int y; };\n",
            "constexpr struct point origin = { 5, 6 };\n",
            "int square[side * side];\n",
            "int rectangle[wider];\n",
            "int rounded[(int)half * 2];\n",
            "int across[origin.y];\n",
            "enum named { four = side };\n",
            "int e = four;\n",
        ));
        assert!(text.contains("global @square : bytes 64 ="), "{text}");
        assert!(text.contains("global @rectangle : bytes 20 ="), "{text}");
        assert!(text.contains("global @rounded : bytes 8 ="), "{text}");
        assert!(text.contains("global @across : bytes 24 ="), "{text}");
        assert!(text.contains("global @e : i32 = 4,"), "{text}");

        // A `const` object is not one of them, which is what makes `int a[n];` a variable
        // length array in C and is the distinction the keyword was added to draw.
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let konst = "const int n = 1;\nint a[n];\n";
        let message = "/main.c:2:5: error: variably modified 'a' at file scope [E0538]";
        assert_eq!(run(&opts, konst).messages, [message]);

        // Nor is a subscript of one, which gcc 16 refuses in the same words.
        let subscript = "constexpr int t[3] = { 1, 2, 3 };\nint a[t[1]];\n";
        assert_eq!(run(&opts, subscript).messages, [message]);

        // And `constexpr` implies `const`, so the address of one is an address of a `const`.
        let address = "constexpr int c = 3;\nint *p = &c;\n";
        let warning = "/main.c:2:6: warning: initialization discards 'const' qualifier from \
             pointer target type [E0514]";
        assert_eq!(run(&opts, address).messages, [warning]);
    }

    /// A definition that names its parameters and then declares them under the list.
    ///
    /// The declarations say what the types are, 6.9.1p6, and what the function takes is those
    /// types with the default argument promotions over them, which is what a caller of an
    /// unprototyped function hands over. A prototype already in scope overrules the promoted
    /// types, since a header saying `int narrow(char);` over a definition written this way is
    /// the pairing all the code written this way relies on and 6.7.6.3p15 is read that way by
    /// every compiler.
    #[test]
    fn an_old_style_definition_takes_its_types_from_the_declarations_under_its_list() {
        // C17, since the default dialect is the one that warns about the form and this is
        // about what it means rather than about the warning.
        let mut opts = options();
        opts.std = Std::C17;
        let source = concat!(
            "int add(a, b)\n",
            "int a;\n",
            "int b;\n",
            "{ return a + b; }\n",
            "int promoted(c)\n",
            "char c;\n",
            "{ return c; }\n",
            "int narrow(char);\n",
            "int narrow(c)\n",
            "char c;\n",
            "{ return c; }\n",
            "int first(a)\n",
            "int a[4];\n",
            "{ return a[0]; }\n",
        );
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        let text = result.text();
        assert!(text.contains("add : int(int, int) function external defined"), "{text}");
        assert!(text.contains("promoted : int(int) function external defined"), "{text}");
        // The body still sees the `char` it was declared as, whatever the caller hands over.
        assert!(text.contains("c : char object automatic defined"), "{text}");
        assert!(text.contains("narrow : int(char) function external defined"), "{text}");
        // An array parameter is a pointer here as much as it is in a prototype.
        assert!(text.contains("first : int(int *) function external defined"), "{text}");
    }

    /// What the two halves of an old-style parameter list can disagree about.
    ///
    /// Each of these is a sentence gcc 16 has, and every message below is the one it prints,
    /// read off it on x86-64 rather than reasoned about. The last two are the dialect: a name
    /// with no declaration is an `int` in C89 and a diagnostic from C99 on, and the whole form
    /// left the language in C23, where gcc still takes it and warns.
    #[test]
    fn the_two_halves_of_an_old_style_parameter_list_have_to_agree() {
        let mut opts = options();
        opts.std = Std::C17;
        for (source, message) in [
            ("int f(a, a)\nint a;\n{ return a; }\n", "1:10: error: multiple parameters named 'a'"),
            (
                "int f(a)\nint a;\nint b;\n{ return a; }\n",
                "3:5: error: declaration for parameter 'b' but no such parameter",
            ),
            ("int f(a)\nint a;\nint a;\n{ return a; }\n", "3:5: error: redefinition of parameter"),
            ("int f(a)\nint a = 1;\n{ return a; }\n", "2:5: error: parameter 'a' is initialized"),
            (
                "int f(a)\nstatic int a;\n{ return a; }\n",
                "2:12: error: storage class specified for parameter 'a'",
            ),
            (
                "int f(char);\nint f(a)\nshort a;\n{ return a; }\n",
                "2:7: error: argument 'a' doesn't match prototype",
            ),
        ] {
            let result = run(&opts, source);
            assert!(result.failed(), "expected this to fail:\n{source}");
            assert!(result.messages[0].contains(message), "{:?}", result.messages);
        }

        // A name the declarations never mention. C89 gave it an `int` and gcc still takes it
        // in that dialect, and every dialect after it made the same line a diagnostic.
        let implicit = "int f(a, b)\nint a;\n{ return a + b; }\n";
        let mut older = options();
        older.std = Std::C89;
        assert!(!run(&older, implicit).failed(), "{:?}", run(&older, implicit).messages);
        let result = run(&opts, implicit);
        assert!(
            result.messages[0].contains("1:10: error: type of 'b' defaults to 'int'"),
            "{:?}",
            result.messages
        );

        // C23 took the form out of the language and gcc kept accepting it with a warning, and
        // a warning is what this is, because the code written this way is not going to be
        // rewritten and refusing it would put the compiler out of reach of it.
        let mut newer = options();
        newer.std = Std::C23;
        let plain = "int f(a)\nint a;\n{ return a; }\n";
        let result = run(&newer, plain);
        assert!(!result.failed(), "{:?}", result.messages);
        assert_eq!(
            result.messages,
            ["/main.c:1:5: warning: old-style function definition [E0412]"]
        );
        assert!(run(&opts, plain).messages.is_empty(), "and nothing to say in the dialects before");
    }

    /// A type nothing is ever an object of is a type `sizeof` still has to answer about, which
    /// is what `991014-1.c` in the gcc.c-torture execution suite asks.
    ///
    /// The limit is `PTRDIFF_MAX` and it is the same one for an array and for a record, so a
    /// record of every byte an object may have is laid out and one byte more is refused. All
    /// four numbers are what gcc 16 gives on x86-64.
    #[test]
    fn a_type_is_refused_when_it_passes_the_largest_object_and_not_before() {
        let text = ir(concat!(
            "struct huge_struct { short buf[(1L << 62) - 256]; int a, b, c, d; };\n",
            "struct brim { char buf[9223372036854775807L]; };\n",
            "struct bitty { char buf[9223372036854775800L]; int x : 1; };\n",
            "unsigned long h = sizeof(struct huge_struct);\n",
            "unsigned long b = sizeof(struct brim);\n",
            "unsigned long y = sizeof(struct bitty);\n",
        ));
        assert!(text.contains("global @h : i64 = 9223372036854775312,"), "{text}");
        assert!(text.contains("global @b : i64 = 9223372036854775807,"), "{text}");
        assert!(text.contains("global @y : i64 = 9223372036854775804,"), "{text}");

        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let over = "struct over { char buf[9223372036854775800L]; char x[8]; };\n";
        let message = "/main.c:1:1: error: type 'struct over' is too large [E0560]";
        assert_eq!(run(&opts, over).messages, [message]);
        let array = "struct wide { short buf[1L << 62]; };\n";
        let message = "/main.c:1:25: error: size of array 'buf' exceeds \
             maximum object size '9223372036854775807' [E0537]";
        assert_eq!(run(&opts, array).messages[0], message);
    }

    /// A byte in the source that is not part of a character, which only a literal may hold.
    ///
    /// The source cannot be a `&str` here, which is the whole point: a file is bytes and only
    /// mostly text.
    fn compile_bytes(source: &[u8]) -> Compiled {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let mut fs = MemoryFileSystem::new();
        fs.insert("/main.c", source.to_vec());
        compile(&opts, "/main.c", &fs)
    }

    /// A raw byte inside a string literal is that byte, which gcc has always taken and which is
    /// the only place in a source file where a byte does not have to be part of a character.
    /// Replacing it would give the object three bytes rather than one, since the replacement
    /// character is three bytes of UTF-8, so the object would not be the one that was written
    /// even where the diagnostic is ignored. Anywhere else the byte is still a mistake, which
    /// is where gcc draws the same line.
    #[test]
    fn a_byte_that_is_not_a_character_is_kept_in_a_literal_and_refused_outside_one() {
        let mut source = b"char s[] = \"a".to_vec();
        source.push(0xff);
        source.extend_from_slice(b"b\";\nchar c = '");
        source.push(0xff);
        source.extend_from_slice(b"';\n");
        let result = compile_bytes(&source);
        assert_eq!(result.messages, Vec::<String>::new(), "a raw byte in a literal is that byte");
        assert!(result.text().contains(r#"bytes "a\ffb\00""#), "{}", result.text());
        // Plain `char` is signed on this target, so the constant is minus one rather than 255.
        assert!(result.text().contains("global @c : i8 = -1,"), "{}", result.text());

        let mut stray = b"int a".to_vec();
        stray.push(0xff);
        stray.extend_from_slice(b" = 1;\n");
        let result = compile_bytes(&stray);
        assert!(
            result.messages.iter().any(|m| m.contains("source is not valid UTF-8 here")),
            "{:?}",
            result.messages
        );
    }

    #[test]
    fn an_object_becomes_a_global_with_an_image_and_a_function_becomes_a_func() {
        let text = ir("int x = 7;\nint add(int a, int b) { return a + b; }\n");
        assert!(text.contains("global @x : i32 = 7, align 4, linkage(external)\n"), "{text}");
        let expected = "\
func @add(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = add.nsw %0, %1
    return %2
}
";
        assert!(text.contains(expected), "{text}");
    }

    #[test]
    fn a_local_nothing_takes_the_address_of_is_a_value_and_never_a_stack_slot() {
        let text = body("int f(int n) { int a = n + 1; int b = a * 2; return a + b; }\n");
        assert!(!text.contains("alloca"), "{text}");
        assert!(!text.contains("load"), "{text}");
        assert!(!text.contains("store"), "{text}");
    }

    #[test]
    fn a_local_whose_address_is_taken_gets_a_slot_in_the_entry_block() {
        let text = body("int g(int *);\nint f(void) { int a = 1; return g(&a); }\n");
        let expected = "\
block0:
    %0 = alloca, size 4, align 4
    %1 = iconst.i32 1
    store %1 -> %0, align 4
    %2 = call @g(%0) : (ptr) -> i32
    return %2
";
        assert_eq!(text, expected);
    }

    #[test]
    fn a_loop_carries_what_it_changes_as_block_parameters() {
        // The whole point of building SSA during the walk rather than after it: `i` and
        // `total` are values that arrive on an edge, and neither has ever been in memory.
        let text = body(
            "int f(int n) {\n  int total = 0;\n  for (int i = 0; i < n; i++) total += i;\n  \
             return total;\n}\n",
        );
        assert!(!text.contains("alloca"), "{text}");
        assert!(text.contains("block1(%3: i32, %4: i32):"), "{text}");
        assert!(text.contains("jump block1("), "{text}");
    }

    #[test]
    fn a_comparison_used_as_a_condition_is_not_widened_and_narrowed_again() {
        let text = body("int f(int a, int b) { if (a < b) return 1; return 0; }\n");
        assert!(text.contains("icmp slt %0, %1"), "{text}");
        assert!(!text.contains("zext"), "{text}");
    }

    #[test]
    fn the_right_side_of_a_short_circuit_is_in_a_block_of_its_own() {
        let text = body("int f(int a, int b) { return a && b; }\n");
        let expected = "\
block0(%0: i32, %1: i32):
    %2 = iconst.i32 0
    %3 = icmp ne %0, %2
    %4 = iconst.i1 0
    br_if %3, block1, block2(%4)

block1:
    %5 = iconst.i32 0
    %6 = icmp ne %1, %5
    jump block2(%6)

block2(%7: i1):
    %8 = zext.i32 %7
    return %8
";
        assert_eq!(text, expected);
    }

    #[test]
    fn code_after_a_return_is_not_built_and_does_not_leave_an_empty_block_behind() {
        let text = body("int f(int a) { if (a) return 1; else return 2; return 3; }\n");
        // Three blocks, the test and the two arms. The join the `return 3` would need is
        // never created, because a block nothing branches to is not a block.
        assert!(!text.contains("block3"), "{text}");
        assert!(!text.contains("iconst.i32 3"), "{text}");
    }

    #[test]
    fn falling_off_the_end_returns_zero_from_main_and_nothing_from_a_void_function() {
        assert!(body("int main(void) { }\n").contains("iconst.i32 0\n    return"));
        assert_eq!(body("void f(void) { }\n"), "block0:\n    return\n");
        assert!(body("int f(void) { }\n").contains("unreachable"));
    }

    #[test]
    fn a_structure_is_copied_rather_than_held_in_a_value() {
        let text = body(
            "struct point { int x, y; };\n\
             int f(void) { struct point p = { 1, 2 }; struct point q = p; return q.x; }\n",
        );
        assert!(text.contains("memcpy"), "{text}");
    }

    #[test]
    fn an_initializer_that_leaves_part_of_an_object_unwritten_zeroes_it_first() {
        let text = body("int f(void) { int a[4] = { 1 }; return a[3]; }\n");
        assert!(text.contains("memset"), "{text}");
    }

    #[test]
    fn a_switch_is_one_branch_and_a_case_that_falls_through_carries_what_it_wrote() {
        let text = body(
            "int f(int x) { int r = 0; switch (x) { case 1: r = 1; case 2: r += 2; break; \
             default: r = 4; } return r; }\n",
        );
        let expected = "\
block0(%0: i32):
    %1 = iconst.i32 0
    switch %0, block1, [1 => block2, 2 => block3(%1)]

block1:
    %2 = iconst.i32 4
    jump block4(%2)

block2:
    %3 = iconst.i32 1
    jump block3(%3)

block3(%4: i32):
    %5 = iconst.i32 2
    %6 = add.nsw %4, %5
    jump block4(%6)

block4(%7: i32):
    return %7
";
        assert_eq!(text, expected);
    }

    #[test]
    fn a_case_range_is_tested_for_rather_than_put_in_the_table() {
        // GNU's `case 1 ... 9`. Nine table entries would be nine here and four billion for the
        // range a program is allowed to write, so it is a subtraction and one unsigned compare.
        let text = body("int f(int x) { switch (x) { case 1 ... 9: return 1; } return 0; }\n");
        assert!(text.contains("%2 = sub %0, %1"), "{text}");
        assert!(text.contains("icmp ule"), "{text}");
        assert!(!text.contains("switch"), "{text}");
    }

    #[test]
    fn break_leaves_the_switch_and_continue_leaves_the_loop_around_it() {
        let text = body(
            "int f(int n) { int t = 0; for (int i = 0; i < n; i++) { switch (i) { \
             case 0: continue; case 1: break; default: t += i; } t++; } return t; }\n",
        );
        // The `continue` goes to the step and the `break` goes to the `t++` after the switch,
        // which is also where the default falls out to.
        assert!(text.contains("switch %3, block4, [0 => block5, 1 => block6]"), "{text}");
        assert!(text.contains("block5:\n    jump block7("), "{text}");
        assert!(text.contains("block6:\n    jump block8("), "{text}");
    }

    #[test]
    fn a_switch_with_nothing_to_branch_on_still_runs_what_comes_after_it() {
        assert_eq!(body("void f(int x) { switch (x) { } }\n"), "block0(%0: i32):\n    return\n");
    }

    #[test]
    fn a_label_a_loop_is_only_entered_through_builds_the_loop_around_it() {
        // A branch into the middle of a loop that nothing else reaches, the Duff's device shape.
        // The `while` is not reached in order, so the walk starts a block nothing branches to and
        // builds it from there. What comes out is the loop with an edge straight into its body,
        // and the header that nothing arrives at is pruned.
        let text = body(
            "int f(int x, int n) { switch (x) { case 1: break; while (n) { case 2: n--; } } \
             return n; }\n",
        );
        // `case 2` lands on the body, `case 1` and the default land on the return, and the test
        // at the bottom of the loop comes back round to the body.
        assert!(text.contains("switch %0, block1(%1), [1 => block2, 2 => block3(%1)]"), "{text}");
        assert!(text.contains("block3(%3: i32):\n    %4 = iconst.i32 1"), "{text}");
        assert!(text.contains("block5:\n    jump block3("), "{text}");
    }

    #[test]
    fn a_goto_into_a_loop_body_enters_it_without_the_test() {
        // The same thing through a `goto`. The first pass through the body runs whatever the
        // label is on, and only then does the loop reach its own test.
        let text = body("int f(int x, int n) { goto in; while (n) { in: n--; } return n; }\n");
        assert!(text.starts_with("block0(%0: i32, %1: i32):\n    jump block1(%1)"), "{text}");
        assert!(text.contains("block1(%2: i32):\n    %3 = iconst.i32 1"), "{text}");
        assert!(text.contains("br_if %7, block3, block4"), "{text}");
    }

    #[test]
    fn a_goto_is_a_jump_to_the_block_the_label_starts() {
        let text = body("int f(int x) { int r = 0; if (x) goto out; r = 1; out: return r; }\n");
        // Both edges into `out` carry what `r` holds on the way, and neither is a stack slot.
        assert!(!text.contains("alloca"), "{text}");
        assert!(text.contains("block3(%4: i32):\n    return %4"), "{text}");
        assert_eq!(text.matches("jump block3(").count(), 2, "{text}");
    }

    #[test]
    fn a_backward_goto_is_a_loop_and_carries_what_it_changes() {
        let text =
            body("int f(int n) { int i = 0; again: if (i < n) { i++; goto again; } return i; }\n");
        assert!(!text.contains("alloca"), "{text}");
        assert!(text.contains("block1(%2: i32):"), "{text}");
        assert!(text.contains("jump block1(%5)"), "{text}");
    }

    #[test]
    fn a_label_nothing_reaches_is_taken_out_rather_than_left_for_the_verifier() {
        // A block nothing branches to is not a legal function, and which labels are dead is not
        // known until the last statement has been walked, since the `goto` is allowed to be it.
        assert_eq!(
            body("int f(int x) { return x; spare: return 0; }\n"),
            "block0(%0: i32):\n    return %0\n"
        );
    }

    #[test]
    fn a_bit_field_is_read_by_loading_the_bytes_it_lies_in_and_shifting() {
        let text = body(
            "struct s { unsigned a : 3; signed b : 5; };\nint f(struct s *p) { return p->b; }\n",
        );
        // One byte holds both fields, and the signed one needs no mask: shifting it down
        // arithmetically is what says its top bit is a sign.
        assert_eq!(
            text,
            "\
block0(%0: ptr):
    %1 = load.i8 %0, align 1
    %2 = iconst.i8 3
    %3 = ashr %1, %2
    %4 = sext.i32 %3
    return %4
"
        );
    }

    #[test]
    fn a_store_to_a_bit_field_does_not_write_a_byte_it_has_no_bit_in() {
        // C11 says an ordinary member beside a bit-field is a memory location of its own, so
        // the four byte store this would take is a data race in a program that has none. The
        // three bytes of `a` go in as two and one, and `c` is not touched.
        let text =
            body("struct s { int a : 24; char c; };\nvoid f(struct s *p, int v) { p->a = v; }\n");
        assert_eq!(
            text,
            "\
block0(%0: ptr, %1: i32):
    %2 = iconst.i32 16777215
    %3 = and %1, %2
    %4 = trunc.i16 %3
    store %4 -> %0, align 2
    %5 = iconst.i32 16
    %6 = lshr %3, %5
    %7 = trunc.i8 %6
    %8 = iconst.i64 2
    %9 = ptr_add %0, %8
    store %7 -> %9, align 1
    return
"
        );
    }

    #[test]
    fn what_an_assignment_to_a_bit_field_is_worth_is_what_fits_in_it() {
        let text =
            body("struct s { unsigned b : 5; };\nunsigned f(struct s *p) { return p->b = 33; }\n");
        // 33 does not fit in five bits, and 1 is both what goes in the field and what the
        // assignment is worth.
        assert!(text.contains("%3 = iconst.i8 31\n    %4 = and %2, %3"), "{text}");
        assert!(text.ends_with("%9 = zext.i32 %4\n    return %9\n"), "{text}");
    }

    #[test]
    fn an_assignment_a_statement_throws_away_builds_none_of_what_it_is_worth() {
        // The value of an assignment to a bit-field takes a shift to build, and a statement
        // has no use for it. Nothing here reads back what was stored.
        let text = body("struct s { signed b : 5; };\nvoid f(struct s *p) { p->b = 3; }\n");
        assert_eq!(text.matches("ashr").count(), 0, "{text}");
        assert!(text.ends_with("store %8 -> %0, align 1\n    return\n"), "{text}");
    }

    #[test]
    fn a_bit_field_in_an_initializer_goes_in_over_bytes_that_were_zeroed_first() {
        // A bit-field writes part of a byte and leaves the rest of it alone, so the object has
        // to be zero before it goes in or what the initializer did not name is whatever the
        // stack held.
        let text = body(
            "struct s { int a : 3; int b; };\nint f(void) { struct s v = { 1 }; return v.b; }\n",
        );
        assert!(text.contains("memset %0, %1, size 8, align 4"), "{text}");
    }

    #[test]
    fn the_image_of_a_static_bit_field_is_the_bytes_the_fields_share() {
        // Two fields in one byte are not two entries in the image, because an image is written
        // in bytes: they are the byte they are both in.
        let text = ir("struct s { unsigned a : 3; unsigned b : 5; } g = { 1, 2 };\n");
        assert!(
            text.contains("global @g : bytes 4 = { bytes \"\\11\", zero 3 }, align 4"),
            "{text}"
        );
    }

    #[test]
    fn an_initialized_flexible_array_member_makes_the_object_larger_than_its_type() {
        // `sizeof` answers without the array and the definition has to hold what was written, so
        // the object is the size of its image. gcc 16 gives these four, three and two bytes and
        // so does this. The image used to be written at the size the type had, which left the
        // verifier looking at twenty bytes going into four.
        let text = ir(concat!(
            "struct a { int i; int j[]; } x = { 1, { 2, 0, 2, 3 } };\n",
            "struct b { char c; char p[]; } y = { 'o', \"wx\" };\n",
            "struct c { char c; char p[]; } z = { '9', { 'e', 'b' } };\n",
            "char s[2] = \"hi\";\n",
        ));
        assert!(
            text.contains("global @x : bytes 20 = { i32 1, i32 2, i32 0, i32 2, i32 3 }"),
            "{text}"
        );
        assert!(text.contains("global @y : bytes 4 = { i8 111, bytes \"wx\\00\" }"), "{text}");
        assert!(text.contains("global @z : bytes 3 = { i8 57, i8 101, i8 98 }"), "{text}");
        // The array with a length of its own still cuts the literal down to it, which is the
        // one case in C where a string initializer drops its terminator.
        assert!(text.contains("global @s : bytes 2 = { bytes \"hi\" }"), "{text}");
    }

    #[test]
    fn a_definition_takes_a_parameter_it_left_unnamed() {
        // The entry block's parameters are the definition's, and one the front end dropped for
        // having no name left the two lists different lengths, which the walk read as an
        // old-style definition and refused. gcc has taken these for far longer than C23 has.
        let text = ir("int f(int a, int) { return a; }\n");
        assert!(text.contains("func @f(i32, i32) -> i32"), "{text}");
        assert!(text.contains("block0(%0: i32, %1: i32):"), "{text}");

        // The unnamed one first, so that the named one is the second parameter of the entry
        // block and not the first: the list says the order and not only how many there are.
        let text = ir("int g(int, int n) { return n; }\n");
        assert!(text.contains("block0(%0: i32, %1: i32):\n    return %1\n"), "{text}");
    }

    #[test]
    fn an_assignment_of_a_structure_is_the_object_it_wrote() {
        // `d = e = c` used to be refused, because the middle assignment is a value of structure
        // type and the walk had nowhere to read one from. What an assignment is worth is the
        // value it stored, so the object it stored into is the answer and the chain is three
        // copies out of the one source with no temporary in it.
        let text = body(concat!(
            "struct s { int f; int g; };\n",
            "void h(struct s *a, struct s *c, struct s *d, struct s *e)\n",
            "{ *d = *e = a[0] = *c; }\n",
        ));
        assert_eq!(text.matches("memcpy").count(), 3, "{text}");
        assert!(text.contains("memcpy %8, %1, size 8, align 4\n"), "{text}");
        assert!(text.contains("memcpy %3, %8, size 8, align 4\n"), "{text}");
        assert!(text.contains("memcpy %2, %3, size 8, align 4\n"), "{text}");
    }

    #[test]
    fn a_string_literal_stops_at_the_end_of_the_array_it_is_filling() {
        // The excess used to be laid into the object anyway, so the row after was written over
        // and the image refused the entry that came to it. C 6.7.10p14 says the terminator goes
        // in only if there is room for it, and gcc discards the rest of a literal that is longer
        // still, which is what the first of these is and why it warns.
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let result = run(
            &opts,
            concat!(
                "const char a[2][3] = { \"1234\", \"xyz\" };\n",
                "static const char b[3][5] = { \"12345\", \"678\", \"9\" };\n",
                "union u { struct { char x[4]; char y[4]; }; struct { char z[8]; }; };\n",
                "const union u c = { { \"1234\", \"567\" } };\n",
            ),
        );
        let text = result.text();
        assert_eq!(
            result.messages,
            ["/main.c:1:24: warning: initializer-string for array of 'const char' is too long \
              (5 chars into 3 available) [E0637]"]
        );
        assert!(text.contains("global @a : bytes 6 = { bytes \"123\", bytes \"xyz\" }"), "{text}");
        assert!(
            text.contains(
                "global @b : bytes 15 = { bytes \"12345\", bytes \"678\\00\", zero 1, \
                 bytes \"9\\00\", zero 3 }"
            ),
            "{text}"
        );
        // The eight bytes are four, three and a terminator, and then the byte the shorter
        // literal left for the string in the other member of the union to end at.
        assert!(
            text.contains("global @c : bytes 8 = { bytes \"1234\", bytes \"567\\00\" }"),
            "{text}"
        );
    }

    #[test]
    fn a_cast_of_a_record_to_its_own_type_is_the_object_that_was_cast() {
        // gcc accepts one and does nothing with it, which sema already had. Lowering asked for
        // the object under it and had no arm for a cast, so `(struct s)x` in an initializer was
        // refused with E0519. It is one copy out of the object named, not two.
        let text = body(concat!(
            "struct s { int a, b; };\nstruct v { struct s s; int t; };\n",
            "void g(struct v *);\n",
            "void f(struct s *p) { struct v w = { (struct s)*p, 5 }; g(&w); }\n",
        ));
        assert_eq!(text.matches("memcpy").count(), 1, "{text}");
    }

    #[test]
    fn a_compound_literal_read_in_a_static_initializer_lays_its_bytes_into_the_image() {
        // C 6.7.11p4 says a compound literal at file scope has static storage duration, which
        // makes it a constant element, and tcc and c-testsuite both write one. Sema used to call
        // it a non constant because reading it is a node of its own and the read was what it
        // looked at, and lowering had no way to put an object where it wanted a number.
        let text = ir(concat!(
            "struct s { int x; };\n",
            "struct t { struct s s; int o; } a = { (struct s){ 2 }, 3 };\n",
            "int n = (int){ 7 };\n",
            "struct u { struct s p; struct s q; } b = { (struct s){ 1 }, (struct s){ } };\n",
        ));
        assert!(text.contains("global @a : bytes 8 = { i32 2, i32 3 }"), "{text}");
        assert!(text.contains("global @n : i32 = 7,"), "{text}");
        // The second literal names nothing, so what it puts in is the zeros of its own size and
        // not the tail of the object it went in, which would have been the same bytes by luck.
        assert!(text.contains("global @b : bytes 8 = { i32 1, zero 4 }"), "{text}");
    }

    #[test]
    fn the_address_of_a_compound_literal_asks_for_the_object_it_points_at() {
        // Nothing declares a compound literal, so the reference is the only thing that can ask
        // for it to be emitted. The image named `.Lanon.0` and the module defined no such
        // symbol, which the link would have been the first to find out.
        let text = ir("struct s { int x; };\nstruct s *q = &(struct s){ 9 };\n");
        assert!(text.contains("global @.Lanon.0 : i32 = 9, align 4, linkage(internal)"), "{text}");
        assert!(text.contains("global @q : bytes 8 = { addr.8 @.Lanon.0 }"), "{text}");
    }

    #[test]
    fn an_object_of_no_size_at_all_has_an_image_with_nothing_in_it() {
        // A zero length array, which gcc allows and real code uses as the tail of a structure.
        // The image is there and holds nothing, which is not the global that has no image at
        // all, and the IR reader used to stop on the empty one.
        let text = ir("unsigned char foo[1][0];\n");
        assert!(text.contains("global @foo : bytes 0 = {}, align 1"), "{text}");
    }

    #[test]
    fn a_null_pointer_in_an_image_is_the_bits_an_address_has_room_for() {
        // `NULL` in a static initializer, which every program has. The IR type is `ptr` and a
        // `ptr` has no width of its own, so the width the bits are cut to is the target's.
        let text = ir("void *p = 0;\nchar *q = (char *) 4096;\n");
        assert!(text.contains("global @p : i64 = 0, align 8"), "{text}");
        assert!(text.contains("global @q : i64 = 4096, align 8"), "{text}");
    }

    #[test]
    fn an_object_another_module_defines_may_be_one_that_cannot_be_written_through() {
        // Which the verifier used to refuse, having read a declaration as a definition with
        // nothing in it. `extern const` is how a program names something in the library's read
        // only data, and glibc and Darwin both have one in a header a real program includes.
        let text = ir("extern const int limit;\nint f(void) { return limit; }\n");
        assert!(
            text.contains("global @limit : bytes 4, align 4, linkage(external), constant"),
            "{text}"
        );
    }

    #[test]
    fn a_conditional_whose_value_is_an_object_answers_where_the_object_is() {
        // A structure is not a value in the IR, so the two arms cannot be joined as one. The
        // addresses can, and the answer is the address of whichever arm was taken rather than
        // a copy of it into a third place: both arms outlive the expression, so a copy would
        // be one nothing could observe. SQLite's parser writes one of these.
        let text = body(
            "\
struct s { int a, b; };
struct s pick(int c, struct s x, struct s y) { return c ? x : y; }
",
        );
        // The join takes an address, each arm hands it the one it has, and nothing is copied.
        assert!(text.contains("block3(%7: ptr)"), "{text}");
        assert!(text.contains("jump block3(%3)") && text.contains("jump block3(%4)"), "{text}");
        assert!(!text.contains("memcpy"), "the arms are joined rather than copied: {text}");
    }

    #[test]
    fn a_structure_that_fits_in_registers_travels_as_the_registers_it_fits_in() {
        // `struct pair` is two eightbytes on SysV, one of them integer, so the signature says
        // one `i64` in each direction and the body takes the object apart and puts it back
        // together around the call.
        let text = ir("\
struct pair { int a, b; };
struct pair make(int a, int b);
struct pair twice(struct pair p) { return make(p.a, p.b); }
");
        assert!(text.contains("func @make(i32, i32) -> i64"), "{text}");
        assert!(text.contains("func @twice(i64) -> i64"), "{text}");
    }

    #[test]
    fn a_structure_too_large_for_the_registers_travels_as_where_its_bytes_are() {
        // Over two eightbytes the caller passes the bytes in the argument area, which is
        // `byval`, and passes somewhere to write the return value, which is `sret`. Neither is
        // a parameter the program wrote and both are parameters the function has.
        let text = ir("\
struct big { double v[8]; };
struct big grow(struct big b);
struct big twice(struct big b) { return grow(grow(b)); }
");
        assert!(
            text.contains("func @grow(ptr sret(64, align 8), ptr byval(64, align 8))"),
            "{text}"
        );
        assert!(text.contains("block0(%0: ptr, %1: ptr):"), "{text}");
        // The inner call writes into a slot and the outer one reads the same slot, so the
        // object between the two calls is never copied anywhere.
        assert_eq!(text.matches("call @grow").count(), 2, "{text}");
    }

    #[test]
    fn a_structure_passed_to_a_variadic_function_says_so_at_the_call() {
        // The bytes travel in the argument area the same way they would for a parameter, and
        // `printf` has no parameter there to say it on, so the call says it instead. The one
        // that fits in registers says nothing, because travelling as the registers it fits in
        // is what an argument does when nothing says otherwise.
        let text = ir("\
struct big { double v[8]; };
struct pair { int a, b; };
int p(const char *, ...);
int f(struct big b, struct pair q) { return p(\"\", 1, b, q); }
");
        assert!(
            text.contains("call @p(%4, %5, %2 byval(64, align 8), %6) : (ptr, ...) -> i32"),
            "{text}"
        );
    }

    #[test]
    fn what_a_call_produced_is_somewhere_before_anything_is_read_out_of_it() {
        // `make(1, 2).b` has no object to read a member of until one is made, and what makes it
        // is a slot the returned registers are written to.
        let body = body(
            "\
struct pair { int a, b; };
struct pair make(int a, int b);
int second(void) { return make(1, 2).b; }
",
        );
        assert!(body.starts_with("block0:\n    %0 = alloca, size 8, align 4\n"), "{body}");
        assert!(body.contains("store %3 -> %0, align 4\n"), "{body}");
    }

    #[test]
    fn a_structure_of_floats_travels_in_floating_point_registers_on_aarch64() {
        // The same declaration, classified by a different ABI: three `float` members are an
        // eightbyte of two of them and a half eightbyte of the third on SysV, and three vector
        // registers on AAPCS64.
        let source = "\
struct hfa { float x, y, z; };
int take(struct hfa h);
int give(struct hfa h) { return take(h); }
";
        assert!(ir(source).contains("func @take(f64, f32) -> i32"), "{}", ir(source));
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        opts.target = "aarch64-unknown-linux-gnu".parse::<Triple>().unwrap();
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new());
        assert!(result.text().contains("func @take(f32, f32, f32) -> i32"), "{}", result.text());
    }

    #[test]
    fn an_array_whose_length_is_not_a_constant_is_a_slot_made_where_its_declaration_is() {
        // The size is a multiplication rather than a number, the slot is taken from the stack
        // where the declaration is, and the scope it was declared in gives it back.
        let source = "\
int use(int *);
void f(int n) {
  {
    int a[n];
    use(a);
  }
  use(0);
}
";
        let body = body(source);
        assert!(body.contains("mul.nsw"), "{body}");
        assert!(body.contains("stacksave"), "{body}");
        assert!(body.contains("alloca %"), "{body}");
        assert!(body.contains("stackrestore"), "{body}");
    }

    #[test]
    fn a_goto_out_of_the_scope_of_one_gives_its_stack_back_on_the_way() {
        // The label is outside the block the array is in, so arriving there means the array is
        // gone, and the restore that says so goes in front of the branch. The `goto` is written
        // before the walk knows where the label is, which is why the restore is put there at
        // the end rather than built where the branch was.
        let source = "\
int use(int *);
int f(int n) {
  {
    int a[n];
    if (use(a)) goto out;
    use(0);
  }
out:
  return 0;
}
";
        let body = body(source);
        // Two ways out of the block and a restore on each: the jump and the end of the block.
        assert_eq!(body.matches("stackrestore").count(), 2, "{body}");
        let (_, after) = body.split_once("stackrestore").expect("the stack is given back");
        assert!(after.starts_with(" %4\n    jump block"), "{body}");
    }

    #[test]
    fn a_goto_to_a_label_the_array_is_still_alive_at_leaves_the_stack_alone() {
        // The label is after the declaration and in the same block, so control that arrives
        // there arrives somewhere the array exists. Giving it back would be giving back an
        // object the next statement reads.
        let source = "\
int use(int *);
int f(int n) {
  int a[n];
again:
  if (use(a)) goto again;
  return 0;
}
";
        let body = body(source);
        assert!(body.contains("stacksave"), "{body}");
        assert!(!body.contains("stackrestore"), "{body}");
    }

    #[test]
    fn a_goto_back_to_a_label_in_front_of_one_gives_it_back_every_time_round() {
        // A loop written out of a `goto`, with the array made inside it. The label is in the
        // same block as the declaration and before it, which is a place where the array does
        // not exist yet, so the jump there leaves its scope and has to give the stack back. A
        // compiler that skips this restore grows the stack once per iteration.
        let source = "\
int use(int *);
int f(int n) {
again:
  {
    int a[n];
    if (use(a)) goto again;
  }
  return 0;
}
";
        let body = body(source);
        assert_eq!(body.matches("stacksave").count(), 1, "{body}");
        let (_, after) = body.split_once("stackrestore").expect("the stack is given back");
        assert!(after.starts_with(" %4\n    jump block1\n"), "{body}");
    }

    #[test]
    fn the_head_of_a_for_loop_is_a_scope_that_closes_where_the_loop_is_left() {
        // The scope opened for `for (int a[n];;)` used to stay open, and a scope left open is
        // not one mark nobody reads. The marks are a stack, so the next close took this one
        // instead of its own, and the body of the loop gave back nothing while the block after
        // the loop restored a pointer saved inside it. The verifier refused that, which is how
        // it was found.
        let source = "\
int f(void);
void t(void) {
  int count = 10;
  for (; count--;) {
    int b[f()];
    int i;
    for (i = 0; i < f(); i++) {
      b[i] = count;
    }
  }
}
";
        let body = body(source);
        // One save, in the body, and one restore for it, also in the body: the block the
        // restore is in is the one the inner loop leaves through, and it goes back round the
        // outer loop rather than out of it.
        assert_eq!(body.matches("stacksave").count(), 1, "{body}");
        let (_, after) = body.split_once("stackrestore").expect("the stack is given back");
        let (next, _) = after.split_once("\n\n").expect("a block after the restore");
        assert!(next.contains("jump block1("), "{body}");
    }

    #[test]
    fn how_long_one_of_those_is_was_decided_where_it_was_declared_and_not_where_it_is_asked() {
        // What C says about the length being evaluated once: `sizeof a` after `n` changed is
        // still as long as the array is, which is what `n` was when the array came into being.
        let source = "\
unsigned long f(int n) {
  int a[n];
  n = 0;
  return sizeof a;
}
";
        let body = body(source);
        // One read of the parameter, at the declaration, and the answer is built out of it.
        assert_eq!(body.matches("sext.i64 %0").count(), 2, "{body}");
    }

    #[test]
    fn a_block_in_the_middle_of_an_expression_is_walked_where_the_expression_is() {
        // GNU's statement expression: the statements happen where they are written and the last
        // one is the value, so the temporary in it never becomes a slot and never is copied.
        let source = "\
int use(int);
int f(int x) {
  return ({
    int t = use(x);
    t * t;
  });
}
";
        let expected = "\
block0(%0: i32):
    %1 = call @use(%0) : (i32) -> i32
    %2 = mul.nsw %1, %1
    return %2
";
        assert_eq!(body(source), expected);
    }

    #[test]
    fn one_of_those_that_control_never_leaves_is_lowered_and_what_follows_it_is_dropped() {
        // A macro that always jumps, which is what this shape is in real code. The value is
        // never taken, and the block the rest of the expression would have been built in is
        // one nothing branches to, so it goes with the other unreachable blocks.
        let source = "int f(int x) { return ({ return x; 0; }); }\n";
        assert_eq!(body(source), "block0(%0: i32):\n    return %0\n");
    }

    #[test]
    fn one_argument_off_a_variable_argument_list_stays_an_intrinsic() {
        // What it becomes is the target's answer, and this is not where the target's answers
        // are, so the walk writes down which list and which type and leaves it at that. Two of
        // them are two instructions, since each moves the list on.
        let source = "double f(__builtin_va_list ap) { return __builtin_va_arg(ap, double) + __builtin_va_arg(ap, double); }\n";
        let expected = "\
block0(%0: ptr):
    %1 = va_arg.f64 %0
    %2 = va_arg.f64 %0
    %3 = fadd %1, %2
    return %3
";
        assert_eq!(body(source), expected);
    }

    #[test]
    fn one_that_reads_a_structure_answers_where_the_object_is() {
        // An aggregate is not a value, so there is nothing for the result of `va_arg` to be and
        // the object form is a second instruction. What it answers is an address, so it is a
        // place already and the walk copies nothing out of it: the copy here is the one the
        // initializer asks for, into the variable being declared. The size and the alignment
        // travel with it because they are what steps the list on and what a target that has to
        // put registers somewhere needs to know. So does the classification, which says the two
        // halves of this one arrived in general purpose registers: that is an answer about a C
        // type, and this is the last place that still has one.
        //
        // The slot is aligned to sixteen and the copy into it to eight, which is not a
        // disagreement. Sixteen is what a local aggregate of sixteen bytes gets whatever its
        // members ask for, and eight is what the type asks for and so what the copy may assume
        // about the object it is reading from.
        let source = "\
struct s { int a; long b; };
long f(__builtin_va_list ap) { struct s v = __builtin_va_arg(ap, struct s); return v.b; }
";
        let expected = "\
block0(%0: ptr):
    %1 = alloca, size 16, align 16
    %2 = va_object %0, size 16, align 8, in(int 8 at 0, int 8 at 8)
    memcpy %1, %2, size 16, align 8
    %3 = iconst.i64 8
    %4 = ptr_add %1, %3
    %5 = load.i64 %4, align 8
    return %5
";
        assert_eq!(body(source), expected);
    }

    /// Which register file each eightbyte arrived in is the whole of what the classification adds,
    /// and an object with no slots at all is one it sent to the caller's argument area, which is
    /// what everything over two eightbytes is whatever its members are.
    #[test]
    fn the_classification_says_which_registers_the_object_arrived_in() {
        let source = "\
struct s { double a; double b; };
double f(__builtin_va_list ap) { struct s v = __builtin_va_arg(ap, struct s); return v.a; }
";
        assert!(
            body(source)
                .contains("va_object %0, size 16, align 8, in(float f64 at 0, float f64 at 8)"),
            "{}",
            body(source)
        );

        let big = "\
struct s { long a[4]; };
long f(__builtin_va_list ap) { struct s v = __builtin_va_arg(ap, struct s); return v.a[0]; }
";
        assert!(body(big).contains("va_object %0, size 32, align 8\n"), "{}", body(big));
    }

    #[test]
    fn a_jump_to_an_address_branches_to_every_label_the_function_takes_the_address_of() {
        // GNU's computed goto. Which label the address holds is not known here, so all of them
        // are listed, and the values arriving at one are passed on every edge the same way they
        // are on an ordinary branch.
        let source = "\
int f(int c) {
  void *p = c ? &&one : &&two;
  goto *p;
one:
  return 1;
two:
  return 2;
}
";
        let expected = "\
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp ne %0, %1
    br_if %2, block1, block2

block1:
    %3 = block_addr block3
    jump block4(%3)

block2:
    %4 = block_addr block5
    jump block4(%4)

block3:
    %5 = iconst.i32 1
    return %5

block4(%6: ptr):
    indirect_br %6, block3, block5

block5:
    %7 = iconst.i32 2
    return %7
";
        assert_eq!(body(source), expected);
    }

    #[test]
    fn a_jump_to_an_address_no_label_in_the_function_has_arrives_nowhere() {
        // The address came from outside the function, and a jump to a label in another function
        // is undefined. The expression is still evaluated, since a call in it has to happen.
        let source = "void **next(void);
void f(void) { goto *next(); }
";
        let expected = "\
block0:
    %0 = call @next() : () -> ptr
    unreachable
";
        assert_eq!(body(source), expected);
    }

    #[test]
    fn an_asm_with_no_operands_is_volatile_and_the_clobbers_are_the_whole_of_what_it_says() {
        // Nothing reads a result, so the only thing that keeps it is that it is volatile, which
        // a basic asm implies.
        let source = "void f(void) { __asm__(\"mfence\" ::: \"memory\"); }\n";
        let expected = "\
block0:
    inline_asm.volatile \"mfence\", \"\", \"memory\"()
    return
";
        assert_eq!(body(source), expected);
    }

    #[test]
    fn the_constraints_are_one_list_in_the_order_the_template_counts_the_operands() {
        // The outputs first and then the inputs, which is the numbering `%0` and `%1` use. An
        // output in a register is a result, and one that is read as well is an argument too.
        let source = "\
int f(int x, int y) {
  int r;
  __asm__(\"addl %2, %0\" : \"=r\"(r), \"+r\"(y) : \"r\"(x));
  return r + y;
}
";
        let expected = "\
block0(%0: i32, %1: i32):
    %2, %3 = inline_asm.(i32, i32) \"addl %2, %0\", \"=r,+r,r\", \"\"(%1, %0)
    %4 = add.nsw %2, %3
    return %4
";
        assert_eq!(body(source), expected);
    }

    #[test]
    fn a_memory_operand_travels_as_the_address_of_an_object_that_is_given_a_slot() {
        // The assembly is handed a pointer, so the object cannot live in a value, and the scan
        // that runs before the walk has to have known that or there would be nothing to point
        // at. A structure travels this way whatever else its constraint allows, since there is
        // no register that holds one.
        let source = "\
struct pair { int a, b; };
int f(int x) {
  int slot = x;
  struct pair p = { x, x };
  __asm__(\"incl %0\" : \"+m\"(slot), \"=m\"(p));
  return slot + p.a;
}
";
        let text = body(source);
        assert!(text.contains("inline_asm \"incl %0\", \"+m,=m\", \"\"(%1, %2)\n"), "{text}");
        assert!(text.contains("%1 = alloca, size 4, align 4\n"), "{text}");
        assert!(text.contains("%2 = alloca, size 8, align 4\n"), "{text}");
    }

    #[test]
    fn an_asm_goto_falls_through_to_its_first_target_and_writes_its_outputs_there() {
        // The output is only in scope where the instruction dominates, which is the fall through
        // block, so the edge to the label carries the value the object had before the assembly
        // ran. That is what document 11 asks for and it is what putting the fall through first
        // buys.
        let source = "\
int f(int x) {
  int r = 7;
  __asm__ goto(\"cbnz %0, %l1\" : \"=r\"(r) : \"r\"(x) :: away);
  return r;
away:
  return r;
}
";
        let expected = "\
block0(%0: i32):
    %1 = iconst.i32 7
    %2 = inline_asm.volatile \"cbnz %0, %l1\", \"=r,r\", \"\"(%0), labels [block1, block2]

block1:
    return %2

block2:
    return %1
";
        assert_eq!(body(source), expected);
    }

    #[test]
    fn an_asm_statement_that_is_not_well_formed_is_reported_in_the_words_gcc_uses() {
        // The operands are checked here rather than by the assembler, because by the time the
        // assembler sees the template the operands have become registers and it has nothing left
        // to say about the C that named them.
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        for (source, expected) in [
            (
                "void f(int x) { __asm__(\"\" : \"r\"(x)); }\n",
                "output operand constraint lacks '='",
            ),
            (
                "void f(int x) { __asm__(\"\" : \"=r\"(x + 1)); }\n",
                "lvalue required in 'asm' statement",
            ),
            (
                "const int g = 1;\nvoid f(void) { __asm__(\"\" : \"=r\"(g)); }\n",
                "read-only variable 'g' used as 'asm' output",
            ),
            (
                "void f(int x) { __asm__(\"\" : : \"=r\"(x)); }\n",
                "input operand constraint contains '='",
            ),
            (
                "void f(void) { __asm__(\"\" : : \"m\"(1)); }\n",
                "memory input 0 is not directly addressable",
            ),
            ("void f(void) { __asm__(L\"\"); }\n", "wide string literal in 'asm'"),
            (
                "void f(int x, int y) { __asm__(\"\" : [a] \"=r\"(x) : [a] \"r\"(y)); }\n",
                "duplicate asm operand name 'a'",
            ),
            ("void f(int x) { __asm__(\"%[in]\" : \"=r\"(x)); }\n", "undefined named operand 'in'"),
        ] {
            let result = run(&opts, source);
            assert!(result.failed(), "expected this to be reported:\n{source}");
            assert!(
                result.messages.iter().any(|m| m.contains(expected)),
                "{expected}\n{:?}",
                result.messages
            );
        }
    }

    #[test]
    fn what_the_walk_cannot_build_yet_is_reported_rather_than_mislowered() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        for source in [
            "int f(int n) { void *p = &&out; if (n) goto *p; { int a[n]; out: return 1; } }\n",
            "int f(int n) { int a[n]; __asm__ goto(\"\" ::::out); out: return a[0]; }\n",
        ] {
            let result = run(&opts, source);
            assert!(result.failed(), "expected this to be reported:\n{source}");
            assert!(
                result.messages.iter().any(|m| m.contains("not supported yet")),
                "{:?}",
                result.messages
            );
        }
    }

    /// Compiles `source` to IR, reads that back as an input, and gives back both texts.
    fn round_trip(source: &str) -> (String, String) {
        let printed = ir(source);
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let mut fs = MemoryFileSystem::new();
        fs.insert("/main.ir", printed.clone().into_bytes());
        let result = compile_ir(&opts, "/main.ir", &fs);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to read back:\n{printed}");
        (printed, result.text().to_owned())
    }

    #[test]
    fn ir_that_arrives_as_an_input_is_read_back_and_written_out_the_same() {
        // The other half of the round trip test below, through the driver rather than through
        // the library, which is what makes the property something to run over a real program
        // rather than over the modules a test builds.
        let (printed, again) = round_trip(
            "struct point { int x, y; };\n             static const char greeting[] = \"hi\";\n             int puts(const char *);\n             int f(int n) { struct point p = { n, 1 }; puts(greeting); return p.x; }\n",
        );
        assert_eq!(printed, again);
    }

    #[test]
    fn ir_that_is_not_ir_says_which_line_stopped_it() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let mut fs = MemoryFileSystem::new();
        let text = "\
; ModuleID = 'a.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-S128\"

func @f(), linkage(external) {
block0:
    frobnicate
}
";
        fs.insert("/main.ir", text.as_bytes().to_vec());
        let result = compile_ir(&opts, "/main.ir", &fs);
        assert!(result.failed());
        assert!(result.messages[0].contains("/main.ir:8"), "{:?}", result.messages);
    }

    #[test]
    fn ir_that_reads_but_does_not_hold_together_is_reported_by_the_verifier() {
        // A module that a person edited has not been through the verifier, and the return of
        // an `i32` from a function that returns nothing is the kind of thing editing produces.
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let mut fs = MemoryFileSystem::new();
        let text = "\
; ModuleID = 'a.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-S128\"

func @f(), linkage(external) {
block0:
    %0 = iconst.i32 1
    return %0
}
";
        fs.insert("/main.ir", text.as_bytes().to_vec());
        let result = compile_ir(&opts, "/main.ir", &fs);
        assert!(result.failed());
        assert!(result.messages[0].contains("invalid IR"), "{:?}", result.messages);
    }

    #[test]
    fn a_typed_tree_is_not_something_an_input_of_ir_can_produce() {
        // The C that became this is not here any more, so there is nothing to print a tree of.
        let mut fs = MemoryFileSystem::new();
        fs.insert("/main.ir", Vec::new());
        let result = compile_ir(&options(), "/main.ir", &fs);
        assert!(result.failed());
        assert!(result.messages[0].contains("can only be emitted as IR"), "{:?}", result.messages);
    }

    #[test]
    fn the_printed_ir_reads_back_as_the_same_module() {
        // The M2 exit criterion: the text is the module and nothing about it is lost by
        // writing it down. Anything the printer invents or the parser drops shows up here.
        let text = ir("\
struct point { int x, y; };
static const char greeting[] = \"hi\";
int table[4] = { 1, 2, 3 };
int puts(const char *);
double half(double x) { return x / 2.0; }
int f(int n) {
  int total = 0;
  for (int i = 0; i < n; i++) {
    if (i == 3) continue;
    total += table[i];
  }
  switch (n) {
    case 0: total = 1;
    case 1: total++; break;
    default: total = -total;
  }
  struct point p = { total, 1 };
  int *q = &p.y;
  puts(greeting);
  return p.x + *q;
}
int dispatch(int c) {
  void *p = c ? &&one : &&two;
  goto *p;
one:
  return 1;
two:
  return 2;
}
int assembly(int x, int *p) {
  int r;
  __asm__ volatile(\"xadd %0, %2\" : \"=r\"(r), \"+m\"(*p) : \"0\"(x) : \"cc\");
  __asm__ goto(\"cbnz %0, %l1\" : : \"r\"(r) : : away);
  return r;
away:
  return 0;
}
");
        let mut names = Interner::new();
        let module = rucc_ir::parse(&text, &mut names).expect("the printer writes what it reads");
        assert_eq!(rucc_ir::print(&module, &names), text);
    }
}
