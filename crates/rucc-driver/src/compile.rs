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

use rucc_diag::{Diagnostic, Severity};
use rucc_lex::{Convert, Keywords, PpToken, convert};
use rucc_sema::{Checker, Context as CheckContext};
use rucc_session::{EmitKind, FileSystem, Options, Session};

use crate::preprocess::render;

/// What compiling one file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    /// The text to write, empty when there was nothing to write or the compilation failed.
    pub text: String,
    /// The diagnostics, already rendered, one per element, in the order they were reported.
    pub messages: Vec<String>,
    /// How many of them were errors.
    pub errors: u32,
}

impl Compiled {
    /// Whether anything went wrong badly enough that the output should not be used.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.errors > 0
    }
}

/// Compiles one file as far as `opts.emit` asks for and renders the result.
///
/// `name` is the path as the user wrote it, which is the name every diagnostic about the file
/// uses. Only [`EmitKind::Tast`] produces text today. Every later kind runs the same front end
/// and gives back nothing, so that a file with a mistake in it is reported the same way
/// whichever of them was asked for, rather than compiling silently until the part that is
/// written notices.
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

    let mut text = String::new();
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
            },
        );
        checker.check_unit();
        let checked = checker.finish();
        if !checked.failed() && opts.emit == EmitKind::Tast {
            text = rucc_sema::print(&checked.tast, &checked.types, &sess.interner);
        }
        diagnostics.extend(checked.diagnostics);
    }

    let mut messages = Vec::with_capacity(diagnostics.len());
    let mut errors = 0;
    for diag in &diagnostics {
        if diag.severity.is_fatal()
            || (diag.severity == Severity::Warning && opts.warnings_are_errors)
        {
            errors += 1;
        }
        messages.push(render(diag, &sess.sources, opts.warnings_are_errors));
    }
    if errors > 0 {
        // A tree built from a file that did not compile is not a tree anything should read.
        text.clear();
    }
    Compiled { text, messages, errors }
}

/// A result that is nothing but one message, for the failures that happen before there is
/// anything to compile.
fn failure(message: String) -> Compiled {
    Compiled { text: String::new(), messages: vec![format!("rucc: error: {message}")], errors: 1 }
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

    /// The typed tree of `source`, insisting that it compiled cleanly.
    fn tast(source: &str) -> String {
        let result = run(&options(), source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text
    }

    #[test]
    fn a_file_that_is_not_there_says_so_and_produces_nothing() {
        let fs = MemoryFileSystem::new();
        let result = compile(&options(), "/nope.c", &fs);
        assert!(result.failed());
        assert!(result.messages[0].contains("/nope.c"), "{:?}", result.messages);
        assert!(result.text.is_empty());
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
        assert!(text.starts_with("decl #0 a : int [2] object external static tentative"), "{text}");
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
            assert!(result.text.is_empty(), "a file that did not compile wrote a tree:\n{source}");
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
        assert!(!plain.text.is_empty(), "a warning is not a reason to write nothing");

        let mut opts = options();
        opts.warnings_are_errors = true;
        let strict = run(&opts, source);
        assert!(strict.failed());
        assert!(strict.text.is_empty(), "and under -Werror it is a reason to write nothing");
        for message in &strict.messages {
            assert!(!message.contains("warning:"), "{message}");
        }
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
    fn asking_for_a_later_kind_runs_the_same_front_end_and_writes_nothing_yet() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let result = run(&opts, "int x = 1;\n");
        assert!(!result.failed(), "{:?}", result.messages);
        assert!(result.text.is_empty());
        // And it still finds what the checking finds, so `--emit=ir` on a broken file is not a
        // silent success.
        assert!(run(&opts, "int f(void) { return undeclared; }\n").failed());
    }
}
