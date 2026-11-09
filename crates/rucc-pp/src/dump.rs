//! `-dM`: the macro table written back out as the `#define` lines that would produce it.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.4, and `spec/04-driver-and-cli.md` section
//! 4.5 for why the predefined set has to be printable at all.
//!
//! This is the check on the predefined set that a person can actually run. `rucc -dM -E -x c
//! /dev/null | sort` against `gcc -dM -E -x c /dev/null | sort` is one diff, and every entry
//! in it is either a macro we get wrong or a promise we have not made yet. Nothing else gives
//! that list, and a list nobody can produce is a list nobody checks.
//!
//! The output is sorted by name rather than printed in table order. GCC prints its hash order,
//! which is stable for GCC and means nothing to us, and the whole point of the output is being
//! diffed, so the order that makes a diff readable wins.

use rucc_base::Interner;
use rucc_diag::Span;
use rucc_lex::{PpTokenKind, TokenFlags};

use crate::include::spelling;
use crate::macros::{MacroDef, MacroTable};
use crate::token::Tok;

/// Every macro that is defined, as `#define` lines, sorted by name and newline terminated.
///
/// Empty when nothing is defined, which cannot happen in a real compilation but is what a
/// caller building a table by hand will see.
///
/// The macros whose value the preprocessor works out are not in the list, which is what GCC does.
/// These are the ones with no body, such as `__FILE__` and `__COUNTER__`, and `__DATE__` and
/// `__TIME__` as `generated` defines them. `generated` is where `<built-in>` is in the source map.
/// A program that defines `__DATE__` itself gets it listed.
#[must_use]
pub fn macros(table: &MacroTable, interner: &Interner, generated: Option<Span>) -> String {
    let mut all = table.sorted();
    all.sort_by_key(|m| interner.resolve(m.name));
    let mut out = String::new();
    for def in all {
        if worked_out(def, interner, generated) {
            continue;
        }
        define(&mut out, def, interner);
        out.push('\n');
    }
    out
}

/// Whether the preprocessor works out what this macro stands for, so that `-dM` leaves it out.
fn worked_out(def: &MacroDef, interner: &Interner, generated: Option<Span>) -> bool {
    let stamp = matches!(interner.resolve(def.name), "__DATE__" | "__TIME__");
    def.builtin.is_some() || (stamp && generated.is_some_and(|file| file.contains(def.span.lo)))
}

/// One `#define` line, without the newline.
fn define(out: &mut String, def: &MacroDef, interner: &Interner) {
    out.push_str("#define ");
    out.push_str(interner.resolve(def.name));
    if def.function_like {
        out.push('(');
        for (at, param) in def.params.iter().enumerate() {
            if at > 0 {
                out.push_str(", ");
            }
            out.push_str(interner.resolve(*param));
        }
        if let Some(rest) = def.variadic {
            if !def.params.is_empty() {
                out.push_str(", ");
            }
            // `...` for the standard spelling and `name...` for the GNU one. The two are not
            // interchangeable, because the GNU form is what `__VA_ARGS__` is not called in the
            // body, and a dump that printed the standard form for both would not read back as
            // the same macro.
            let name = interner.resolve(rest);
            if name != "__VA_ARGS__" {
                out.push_str(name);
            }
            out.push_str("...");
        }
        out.push(')');
    }
    for (at, token) in def.body.iter().enumerate() {
        // A space before the body, then whatever spacing the definition had. The first token
        // of a body never carries a leading space flag, because the space after the name is
        // what separated it from the name.
        if at == 0 || token.flags.has(TokenFlags::LEADING_SPACE) {
            out.push(' ');
        }
        out.push_str(text(*token, interner));
    }
}

/// A body token as it was written.
fn text(token: rucc_lex::PpToken, interner: &Interner) -> &str {
    match token.kind {
        // A macro body is never empty of meaning at the end, so end of file cannot appear
        // here, but matching on it rather than assuming keeps this total.
        PpTokenKind::Eof => "",
        _ => spelling(Tok::new(token), interner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macros::{Builtin, parse_define};

    /// Builds a table from `#define` bodies written the way a user writes them.
    fn table(lines: &[&str]) -> (MacroTable, Interner) {
        let mut interner = Interner::new();
        let mut table = MacroTable::new();
        for line in lines {
            let mut lexer = rucc_lex::Lexer::new(line.as_bytes(), 0, rucc_lex::Options::new());
            let mut tokens = Vec::new();
            loop {
                let token = lexer.next_token(&mut interner);
                if token.is_eof() {
                    break;
                }
                tokens.push(token);
            }
            let (def, _) = parse_define(&tokens, &mut interner);
            table.define(def.expect("the test wrote a valid define"), &interner);
        }
        (table, interner)
    }

    fn dump(lines: &[&str]) -> String {
        let (table, interner) = table(lines);
        macros(&table, &interner, None)
    }

    #[test]
    fn an_object_like_macro_comes_back_the_way_it_went_in() {
        assert_eq!(dump(&["N 2"]), "#define N 2\n");
        assert_eq!(dump(&["EMPTY"]), "#define EMPTY\n");
    }

    #[test]
    fn the_output_is_sorted_by_name_because_the_point_of_it_is_being_diffed() {
        // Not table order and not definition order. A diff against GCC is the reason this
        // output exists, and a diff whose lines moved is a diff nobody reads.
        assert_eq!(dump(&["Z 1", "A 2", "M 3"]), "#define A 2\n#define M 3\n#define Z 1\n");
    }

    #[test]
    fn a_function_like_macro_keeps_its_parameters_and_the_two_variadic_spellings_apart() {
        assert_eq!(dump(&["ADD(a, b) a + b"]), "#define ADD(a, b) a + b\n");
        assert_eq!(dump(&["F() 1"]), "#define F() 1\n");
        assert_eq!(dump(&["V(...) __VA_ARGS__"]), "#define V(...) __VA_ARGS__\n");
        assert_eq!(dump(&["W(a, ...) __VA_ARGS__"]), "#define W(a, ...) __VA_ARGS__\n");
        // The GNU form names the variadic parameter, and printing it as `...` would be a
        // different macro: the body says `rest`, which the standard spelling does not have.
        assert_eq!(dump(&["G(rest...) rest"]), "#define G(rest...) rest\n");
    }

    #[test]
    fn the_spacing_inside_a_body_is_the_spacing_that_was_written() {
        // Not reformatted. This output is diffed against GCC's, and GCC prints what it stored,
        // so a tidier body here would be a difference on every line that has an operator in it.
        assert_eq!(dump(&["A 1+2"]), "#define A 1+2\n");
        assert_eq!(dump(&["B 1 + 2"]), "#define B 1 + 2\n");
        assert_eq!(dump(&["C (x)"]), "#define C (x)\n");
    }

    /// GCC lists none of the macros it works out the value of, so a diff of the two lists has
    /// no line for them. `__TIME__` also made two runs of the list differ.
    #[test]
    fn a_macro_the_preprocessor_works_out_is_not_listed() {
        let (mut table, mut interner) =
            table(&["__DATE__ \"Jan  1 1970\"", "__TIME__ \"00:00:00\"", "N 1"]);
        for (name, builtin) in Builtin::ALL {
            table.define_builtin(interner.intern(name), builtin, Span::new(0, 1));
        }
        // The two stamps are in `<built-in>`, and the builtins have no body.
        let generated = Some(Span::new(0, 100));
        assert_eq!(macros(&table, &interner, generated), "#define N 1\n");
        // A program that defines `__DATE__` itself gets it listed, as GCC does.
        let text = macros(&table, &interner, None);
        assert!(text.contains("#define __DATE__ \"Jan  1 1970\"\n"), "{text}");
        assert!(text.contains("#define __TIME__ \"00:00:00\"\n"), "{text}");
        assert!(!text.contains("__FILE__"), "{text}");
        assert!(!text.contains("__COUNTER__"), "{text}");
    }
}
