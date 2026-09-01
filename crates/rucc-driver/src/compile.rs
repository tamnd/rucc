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
    fn what_the_walk_cannot_build_yet_is_reported_rather_than_mislowered() {
        let mut opts = options();
        opts.emit = EmitKind::Ir;
        for source in [
            "int f(int n) { int a[n]; a[0] = 1; return a[0]; }\n",
            "int f(int x) { void *p = &&out; goto *p; out: return x; }\n",
            "struct s { int a[4]; };\nint f(struct s v);\nint g(struct s v) { return f(v); }\n",
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
");
        let mut names = rucc_base::Interner::new();
        let module = rucc_ir::parse(&text, &mut names).expect("the printer writes what it reads");
        assert_eq!(rucc_ir::print(&module, &names), text);
    }
}
