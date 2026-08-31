//! Macro expansion, conditionals, include resolution, and the header cache.
//!
//! Design: `spec/05-preprocessor.md`. Layer rank 5, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! Macro expansion is implemented: object-like and function-like macros, `#` and `##`,
//! variadics in both the standard and the GNU spelling, `__VA_OPT__`, and the GNU comma
//! swallowing extension, all on hide sets rather than on a depth counter.
//!
//! Directives are implemented: `#define`, `#undef`, the whole conditional family with the
//! `#if` expression evaluator, `#error`, `#warning`, `#line`, `#pragma`, the `_Pragma`
//! operator, and `#include` and `#include_next` against a search path that follows GCC's
//! order. `#embed` is recognised and refused, because it needs the parser.
//!
//! A header is read once. `#pragma once` and the multiple include optimization, which spots
//! the ordinary `#ifndef` wrapper and skips the file rather than reading it and throwing the
//! result away, both do that.
//!
//! The `__has_*` family is implemented. `__has_include` and `__has_include_next` ask the
//! search path the same question the directive on the same line would ask it, and the rest
//! answer out of the matrix in `rucc-gnu`, which means they answer no for almost everything
//! until the parser lands. That is the point of them.
//!
//! The predefined macro set is generated from the target description rather than hardcoded,
//! and arrives as two synthetic files, `<built-in>` and `<command-line>`, so that a
//! diagnostic about one of them says where it came from. `__DATE__` and `__TIME__` are in it
//! because they are fixed for a translation unit. The ones that are not fixed, `__FILE__`,
//! `__FILE_NAME__`, `__BASE_FILE__`, `__LINE__`, `__INCLUDE_LEVEL__` and `__COUNTER__`, are
//! answered by the expander out of the source map at the place they are used.
//!
//! `print` writes the token stream back out the way `-E` does, with GCC's line markers, GCC's
//! blank line padding, the indentation the source had, and a space wherever two tokens would
//! otherwise read back as one. `-P` turns the markers and the padding off.
//!
//! ```
//! use rucc_base::Interner;
//! use rucc_diag::SourceMap;
//! use rucc_lex::PpTokenKind;
//! use rucc_pp::{Context, Preprocessor};
//! use rucc_session::{MemoryFileSystem, SearchPath};
//!
//! let mut fs = MemoryFileSystem::new();
//! fs.insert("/square.h", b"#define SQUARE(x) ((x) * (x))\n".to_vec());
//!
//! let mut interner = Interner::new();
//! let mut sources = SourceMap::new();
//! let main = b"#include \"square.h\"\n#if SQUARE(2) == 4\nSQUARE(3)\n#endif\n";
//! let file = sources.add("/main.c", main.to_vec())?;
//!
//! let search = SearchPath::new();
//! let mut cx = Context::new(&mut interner, &mut sources, &fs, &search);
//! let mut pp = Preprocessor::new();
//! let out = pp.run(file, &mut cx);
//! assert!(pp.diagnostics().is_empty());
//!
//! let spelled: Vec<&str> = out
//!     .iter()
//!     .map(|t| match t.kind {
//!         PpTokenKind::Punct(p) => p.as_str(),
//!         _ => interner.resolve(t.value.unwrap()),
//!     })
//!     .collect();
//! assert_eq!(spelled.concat(), "((3)*(3))");
//! # Ok::<(), rucc_diag::SourceMapFull>(())
//! ```
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-pp/0.1.0")]

mod cond;
mod directive;
mod expand;
mod hide;
mod include;
mod macros;
mod predef;
mod print;
mod token;

pub use crate::directive::{LineDirective, Preprocessor};
pub use crate::expand::Expander;
pub use crate::hide::{HideSet, HideSets};
pub use crate::include::Context;
pub use crate::macros::{Builtin, MacroDef, MacroTable, parse_define};
pub use crate::predef::{BUILT_IN, COMMAND_LINE, GnucVersion, Predef, Timestamp};
pub use crate::print::{PrintOptions, print};
pub use crate::token::Tok;

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M1";

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_diag::{Diagnostic, SourceMap};
    use rucc_lex::{Options, PpToken, PpTokenKind, TokenFlags, tokenize};

    use super::*;

    /// A preprocessor with a macro table, wired up the way the directive layer will do it.
    struct Pp {
        interner: Interner,
        macros: MacroTable,
        expander: Expander,
        /// Empty, because these tests are about substitution rather than about where a token
        /// came from. The tests for the macros that ask that question live in `directive`,
        /// where there is a file to ask about.
        sources: SourceMap,
    }

    impl Pp {
        fn new() -> Pp {
            Pp {
                interner: Interner::new(),
                macros: MacroTable::new(),
                expander: Expander::new(),
                sources: SourceMap::new(),
            }
        }

        fn lex(&mut self, src: &str) -> Vec<PpToken> {
            let (tokens, errors) = tokenize(src.as_bytes(), 0, Options::new(), &mut self.interner);
            assert!(errors.is_empty(), "test input should lex cleanly: {errors:?}");
            tokens.into_iter().filter(|t| t.kind != PpTokenKind::Eof).collect()
        }

        /// `define("F(a) a + 1")`, that is, everything after the `#define`.
        fn define(&mut self, line: &str) {
            let tokens = self.lex(line);
            let (def, errors) = parse_define(&tokens, &mut self.interner);
            assert!(errors.is_empty(), "definition should be clean: {errors:?}");
            self.macros.define(def.expect("should parse"), &self.interner);
        }

        fn undef(&mut self, name: &str) {
            let sym = self.interner.intern(name);
            self.macros.undef(sym);
        }

        fn expand(&mut self, src: &str) -> Vec<Tok> {
            let tokens = self.lex(src);
            self.expander.expand(&tokens, &self.macros, &mut self.interner, &self.sources)
        }

        /// The expansion, with one space wherever the tokens are separated. This is close to
        /// what `-E` prints and it is what makes these tests readable next to the standard's
        /// own examples.
        fn text(&mut self, src: &str) -> String {
            let out = self.expand(src);
            let mut text = String::new();
            for (at, tok) in out.iter().enumerate() {
                if at > 0 && tok.flags.has(TokenFlags::LEADING_SPACE) {
                    text.push(' ');
                }
                match tok.kind {
                    PpTokenKind::Punct(p) => text.push_str(p.as_str()),
                    _ => text.push_str(
                        self.interner.resolve(tok.value.expect("every non-punctuator interns")),
                    ),
                }
            }
            text
        }

        fn errors(&mut self) -> Vec<Diagnostic> {
            self.expander.take_diagnostics()
        }
    }

    #[test]
    fn an_object_like_macro_is_replaced_by_its_body() {
        let mut pp = Pp::new();
        pp.define("N 42");
        assert_eq!(pp.text("int a = N;"), "int a = 42;");
    }

    #[test]
    fn an_undefined_identifier_is_left_alone() {
        let mut pp = Pp::new();
        assert_eq!(pp.text("int a = N;"), "int a = N;");
    }

    #[test]
    fn a_function_like_macro_needs_a_parenthesis_to_be_invoked() {
        let mut pp = Pp::new();
        pp.define("f(x) x");
        assert_eq!(pp.text("f"), "f", "a bare name is an ordinary identifier");
        assert_eq!(pp.text("f (1)"), "1", "whitespace before the parenthesis is fine");
    }

    #[test]
    fn arguments_are_expanded_before_they_are_substituted() {
        let mut pp = Pp::new();
        pp.define("ONE 1");
        pp.define("f(x) (x + x)");
        assert_eq!(pp.text("f(ONE)"), "(1 + 1)");
    }

    #[test]
    fn an_empty_argument_is_an_argument() {
        let mut pp = Pp::new();
        pp.define("f(x) [x]");
        assert_eq!(pp.text("f()"), "[]");
    }

    #[test]
    fn a_macro_with_no_parameters_takes_no_arguments() {
        let mut pp = Pp::new();
        pp.define("f() nothing");
        assert_eq!(pp.text("f()"), "nothing");
    }

    #[test]
    fn a_comma_inside_parentheses_does_not_split_an_argument() {
        let mut pp = Pp::new();
        pp.define("f(x) [x]");
        assert_eq!(pp.text("f((1, 2))"), "[(1, 2)]");
    }

    #[test]
    fn an_invocation_may_span_lines() {
        let mut pp = Pp::new();
        pp.define("f(a, b) a b");
        assert_eq!(pp.text("f(1,\n   2)"), "1 2");
    }

    #[test]
    fn a_replacement_may_consume_tokens_that_follow_the_invocation() {
        let mut pp = Pp::new();
        pp.define("f(x) [x]");
        pp.define("g f");
        // `g` expands to `f`, and the parenthesis it needs is not in the replacement list, it
        // is in the text after the invocation of `g`. This is the case that forces the
        // pushback stream rather than expanding each macro into an isolated list.
        assert_eq!(pp.text("g(1)"), "[1]");
    }

    #[test]
    fn a_parenthesis_that_has_not_expanded_yet_does_not_count() {
        let mut pp = Pp::new();
        pp.define("lparen (");
        pp.define("f(x) [x]");
        pp.define("g f lparen 1 )");
        // Rescanning reaches `f` while the next token is still `lparen`, so `f` is not an
        // invocation and stays an identifier even though a parenthesis appears there a moment
        // later. GCC and Clang agree, and getting this wrong is a way to expand things nobody
        // asked for.
        assert_eq!(pp.text("g"), "f ( 1 )");
    }

    #[test]
    fn a_macro_does_not_expand_inside_its_own_expansion() {
        let mut pp = Pp::new();
        pp.define("A A + 1");
        assert_eq!(pp.text("A"), "A + 1");
    }

    #[test]
    fn mutually_recursive_macros_terminate_with_both_names_left() {
        let mut pp = Pp::new();
        pp.define("A B");
        pp.define("B A");
        assert_eq!(pp.text("A"), "A");
        assert_eq!(pp.text("B"), "B");
    }

    #[test]
    fn a_hidden_name_stays_hidden_when_it_is_carried_outwards() {
        // The case a depth counter gets wrong: `x` is hidden inside `f`, and the result of
        // `f` is then substituted into `g`, where a counter would have unwound and let it
        // expand again.
        let mut pp = Pp::new();
        pp.define("f(x) x");
        pp.define("g(y) [y]");
        pp.define("h f(h)");
        assert_eq!(pp.text("g(h)"), "[h]");
    }

    #[test]
    fn stringify_puts_the_argument_in_quotes() {
        let mut pp = Pp::new();
        pp.define("str(x) #x");
        assert_eq!(pp.text("str(hello)"), "\"hello\"");
    }

    #[test]
    fn stringify_uses_the_unexpanded_argument() {
        let mut pp = Pp::new();
        pp.define("N 42");
        pp.define("str(x) #x");
        pp.define("xstr(x) str(x)");
        assert_eq!(pp.text("str(N)"), "\"N\"");
        assert_eq!(pp.text("xstr(N)"), "\"42\"", "one level of indirection expands first");
    }

    #[test]
    fn stringify_collapses_whitespace_and_drops_it_at_the_edges() {
        let mut pp = Pp::new();
        pp.define("str(x) #x");
        assert_eq!(pp.text("str(  a   +    b  )"), "\"a + b\"");
    }

    #[test]
    fn stringify_escapes_quotes_and_backslashes_inside_literals() {
        let mut pp = Pp::new();
        pp.define("str(x) #x");
        assert_eq!(pp.text(r#"str("a\n")"#), r#""\"a\\n\"""#);
    }

    #[test]
    fn paste_joins_two_tokens_into_one() {
        let mut pp = Pp::new();
        pp.define("cat(a, b) a ## b");
        assert_eq!(pp.text("cat(foo, bar)"), "foobar");
        assert_eq!(pp.text("cat(1, 2)"), "12");
        assert_eq!(pp.text("cat(+, =)"), "+=");
    }

    #[test]
    fn paste_uses_the_unexpanded_arguments() {
        let mut pp = Pp::new();
        pp.define("N 42");
        pp.define("cat(a, b) a ## b");
        assert_eq!(pp.text("cat(N, N)"), "NN");
    }

    #[test]
    fn the_result_of_a_paste_is_rescanned() {
        let mut pp = Pp::new();
        pp.define("foobar yes");
        pp.define("cat(a, b) a ## b");
        assert_eq!(pp.text("cat(foo, bar)"), "yes");
    }

    #[test]
    fn pasting_an_empty_argument_leaves_the_other_side() {
        let mut pp = Pp::new();
        pp.define("cat(a, b) a ## b");
        assert_eq!(pp.text("cat(foo,)"), "foo");
        assert_eq!(pp.text("cat(, bar)"), "bar");
        assert_eq!(pp.text("cat(,)"), "");
    }

    #[test]
    fn a_paste_that_does_not_make_a_token_is_an_error_and_both_tokens_survive() {
        let mut pp = Pp::new();
        pp.define("cat(a, b) a ## b");
        assert_eq!(pp.text("cat(+, foo)"), "+foo");
        let errors = pp.errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Some("E0313"));
        assert!(errors[0].message.contains("pasting `+` and `foo`"));
    }

    #[test]
    fn variadic_arguments_arrive_as_one_argument_with_the_commas_intact() {
        let mut pp = Pp::new();
        pp.define("f(fmt, ...) g(fmt, __VA_ARGS__)");
        assert_eq!(pp.text("f(\"%d %d\", 1, 2)"), "g(\"%d %d\", 1, 2)");
    }

    #[test]
    fn the_gnu_named_variadic_form_works_the_same_way() {
        let mut pp = Pp::new();
        pp.define("f(fmt, rest...) g(fmt, rest)");
        assert_eq!(pp.text("f(a, b, c)"), "g(a, b, c)");
    }

    #[test]
    fn a_variadic_macro_may_be_called_with_nothing_for_the_variadic_part() {
        let mut pp = Pp::new();
        pp.define("f(a, ...) [a __VA_ARGS__]");
        assert_eq!(pp.text("f(1)"), "[1 ]");
    }

    #[test]
    fn the_gnu_comma_swallowing_extension_drops_the_comma() {
        let mut pp = Pp::new();
        pp.define("log(fmt, ...) printf(fmt, ## __VA_ARGS__)");
        assert_eq!(pp.text("log(\"hi\")"), "printf(\"hi\")");
        assert_eq!(pp.text("log(\"%d\", 1)"), "printf(\"%d\", 1)");
    }

    #[test]
    fn va_opt_appears_only_when_there_are_variable_arguments() {
        let mut pp = Pp::new();
        pp.define("log(fmt, ...) printf(fmt __VA_OPT__(,) __VA_ARGS__)");
        assert_eq!(pp.text("log(\"hi\")"), "printf(\"hi\" )");
        assert_eq!(pp.text("log(\"%d\", 1)"), "printf(\"%d\" , 1)");
    }

    #[test]
    fn va_opt_contents_are_substituted_like_any_other_replacement() {
        let mut pp = Pp::new();
        pp.define("f(a, ...) [a __VA_OPT__(and __VA_ARGS__ done)]");
        assert_eq!(pp.text("f(1)"), "[1 ]");
        assert_eq!(pp.text("f(1, 2)"), "[1 and 2 done]");
    }

    #[test]
    fn va_opt_pastes_as_a_unit() {
        let mut pp = Pp::new();
        pp.define("f(a, ...) a ## __VA_OPT__(x)");
        assert_eq!(pp.text("f(y)"), "y", "with no variable arguments it is a placemarker");
        assert_eq!(pp.text("f(y, 1)"), "yx");
    }

    #[test]
    fn too_few_arguments_are_reported_against_the_definition() {
        let mut pp = Pp::new();
        pp.define("f(a, b) a b");
        assert_eq!(pp.text("f(1)"), "f", "the arguments are consumed, as GCC and Clang do");
        let errors = pp.errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Some("E0312"));
        assert_eq!(errors[0].children.len(), 1, "the definition is worth pointing at");
    }

    #[test]
    fn an_unterminated_argument_list_is_reported_at_the_parenthesis() {
        let mut pp = Pp::new();
        pp.define("f(a) a");
        assert_eq!(pp.text("f(1"), "f");
        let errors = pp.errors();
        assert_eq!(errors[0].code, Some("E0311"));
    }

    #[test]
    fn a_token_from_a_macro_is_reported_at_the_invocation() {
        let mut pp = Pp::new();
        pp.define("N 42");
        let out = pp.expand("  N");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].expansion, out[0].report_span());
        assert_eq!(out[0].expansion.lo, 2, "the invocation is at offset 2 in the input");
        assert_ne!(out[0].span, out[0].expansion, "the spelling is in the macro body");
    }

    #[test]
    fn hide_sets_are_shared_rather_than_rebuilt() {
        let mut pp = Pp::new();
        pp.define("A 1");
        pp.define("B 2");
        for _ in 0..100 {
            pp.text("A B A B");
        }
        assert!(
            pp.expander.hide_sets() <= 3,
            "the empty set plus one per macro, however many times they are used"
        );
    }

    #[test]
    fn expansion_is_the_same_every_time() {
        let mut first = Pp::new();
        let mut second = Pp::new();
        for pp in [&mut first, &mut second] {
            pp.define("N 42");
            pp.define("cat(a, b) a ## b");
            pp.define("log(fmt, ...) printf(fmt, ## __VA_ARGS__)");
        }
        let src = "cat(x, y) N log(\"a\") log(\"b\", 1)";
        assert_eq!(first.text(src), second.text(src));
    }

    /// The example from the standard, 6.10.4.5 in C23. It is the closest thing the
    /// preprocessor has to a conformance test: every clause of the substitution rules shows
    /// up in it and nothing about it is accidental. The expected text is the standard's own,
    /// which GCC and Clang both reproduce.
    #[test]
    fn the_standards_rescanning_example() {
        let mut pp = Pp::new();
        pp.define("x 3");
        pp.define("f(a) f(x * (a))");
        pp.undef("x");
        pp.define("x 2");
        pp.define("g f");
        pp.define("z z[0]");
        pp.define("h g(~");
        pp.define("m(a) a(w)");
        pp.define("w 0,1");
        pp.define("t(a) a");
        pp.define("p() int");
        pp.define("q(x) x");
        pp.define("r(x,y) x ## y");
        pp.define("str(x) # x");

        assert_eq!(
            pp.text("f(y+1) + f(f(z)) % t(t(g)(0) + t)(1);"),
            "f(2 * (y+1)) + f(2 * (f(2 * (z[0])))) % f(2 * (0)) + t(1);"
        );
        // The standard writes `2+(3,4)` here with no space. Clang prints one, because its
        // `-E` writer adds a separator after an expansion whether or not the tokens would
        // run together. Both are conforming and the token sequence is the same either way.
        assert_eq!(
            pp.text("g(x+(3,4)-w) | h 5) & m\n(f)^m(m);"),
            "f(2 * (2+(3,4)-0,1)) | f(2 * (~ 5)) & f(2 * (0,1))^m(0,1);"
        );
        assert_eq!(
            pp.text("p() i[q()] = { q(1), r(2,3), r(4,), r(,5), r(,) };"),
            "int i[] = { 1, 23, 4, 5, };"
        );
        assert_eq!(
            pp.text("char c[2][6] = { str(hello), str() };"),
            "char c[2][6] = { \"hello\", \"\" };"
        );
        assert!(pp.errors().is_empty(), "the standard's example is well formed");
    }

    /// The variadic example from the standard, 6.10.4.5 again.
    #[test]
    fn the_standards_variadic_example() {
        let mut pp = Pp::new();
        pp.define("debug(...) fprintf(stderr, __VA_ARGS__)");
        pp.define("showlist(...) puts(#__VA_ARGS__)");
        pp.define("report(test, ...) ((test)?puts(#test): printf(__VA_ARGS__))");

        assert_eq!(pp.text("debug(\"Flag\");"), "fprintf(stderr, \"Flag\");");
        assert_eq!(pp.text("debug(\"X = %d\\n\", x);"), "fprintf(stderr, \"X = %d\\n\", x);");
        assert_eq!(
            pp.text("showlist(The first, second, and third items.);"),
            "puts(\"The first, second, and third items.\");"
        );
        assert_eq!(
            pp.text("report(x>y, \"x is %d but y is %d\", x, y);"),
            "((x>y)?puts(\"x>y\"): printf(\"x is %d but y is %d\", x, y));"
        );
        assert!(pp.errors().is_empty(), "the standard's example is well formed");
    }

    #[test]
    fn milestone_is_recorded() {
        assert!(MILESTONE.starts_with('M'));
    }
}
