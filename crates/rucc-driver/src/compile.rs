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

use rucc_diag::{Diagnostic, Severity, Span};
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
/// uses. [`EmitKind::Tast`] and [`EmitKind::Ir`] produce text today. Every later kind runs the
/// same front end and gives back nothing, so that a file with a mistake in it is reported the
/// same way whichever of them was asked for, rather than compiling silently until the part
/// that is written notices.
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
        if !checked.failed() {
            match opts.emit {
                EmitKind::Tast => {
                    text = rucc_sema::print(&checked.tast, &checked.types, &sess.interner);
                }
                EmitKind::Ir => {
                    let lowered = rucc_lower::lower(
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
                        } else {
                            text = rucc_ir::print(&lowered.module, &sess.interner);
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
    let text = if errors > 0 { String::new() } else { rucc_ir::print(&module, &sess.interner) };
    Compiled { text, messages, errors }
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
        result.text
    }

    /// The typed tree of `source`, insisting that it compiled cleanly.
    fn tast(source: &str) -> String {
        let result = run(&options(), source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text
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
    fn asking_for_a_kind_that_is_not_written_yet_runs_the_front_end_and_writes_nothing() {
        let mut opts = options();
        opts.emit = EmitKind::MirFinal;
        let result = run(&opts, "int x = 1;\n");
        assert!(!result.failed(), "{:?}", result.messages);
        assert!(result.text.is_empty());
        // And it still finds what the checking finds, so a later kind on a broken file is not
        // a silent success.
        assert!(run(&opts, "int f(void) { return undeclared; }\n").failed());
    }

    /// The IR of `source`, insisting that it compiled cleanly.
    fn ir(source: &str) -> String {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        let result = run(&opts, source);
        assert_eq!(result.messages, Vec::<String>::new(), "expected this to compile:\n{source}");
        result.text
    }

    /// The body of the one function in `source`, which is what most of these are about.
    fn body(source: &str) -> String {
        let text = ir(source);
        let (_, rest) = text.split_once("{\n").expect("a function definition");
        let (body, _) = rest.rsplit_once("}\n").expect("a function definition");
        body.to_owned()
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
    fn a_label_control_cannot_fall_into_is_reported_rather_than_dropped() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        // A branch into the middle of a loop that nothing else reaches, once through a `switch`
        // and once through a `goto`. The walk builds a loop from the top, so lowering either of
        // these without the edge into the body would be a miscompile.
        for source in [
            "int f(int x, int n) { switch (x) { case 1: break; while (n) { case 2: n--; } } \
             return n; }\n",
            "int f(int x, int n) { goto in; while (n) { in: n--; } return n; }\n",
        ] {
            let result = run(&opts, source);
            assert!(result.failed(), "expected this to be reported:\n{source}");
            assert!(
                result.messages.iter().any(|m| m.contains("a label control cannot fall into")),
                "{:?}",
                result.messages
            );
        }
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
        assert!(result.text.contains("func @take(f32, f32, f32) -> i32"), "{}", result.text);
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
            "int f(int n) { int a[n]; goto out; out: return a[0]; }\n",
            "int f(int n) { int a[n]; void *p = &&out; goto *p; out: return a[0]; }\n",
            "struct s { double a[8]; };\nint p(const char *, ...);\nint g(struct s v) { return p(\"\", v); }\n",
            "struct s { int a; };\nstruct s f(__builtin_va_list ap) { return __builtin_va_arg(ap, struct s); }\n",
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
        (printed, result.text)
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
        let mut names = rucc_base::Interner::new();
        let module = rucc_ir::parse(&text, &mut names).expect("the printer writes what it reads");
        assert_eq!(rucc_ir::print(&module, &names), text);
    }
}
