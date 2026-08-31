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
use rucc_lex::{PpTokenKind, TokenFlags};

use crate::include::spelling;
use crate::macros::{Builtin, MacroDef, MacroTable};
use crate::token::Tok;

/// Every macro that is defined, as `#define` lines, sorted by name and newline terminated.
///
/// Empty when nothing is defined, which cannot happen in a real compilation but is what a
/// caller building a table by hand will see.
#[must_use]
pub fn macros(table: &MacroTable, interner: &Interner) -> String {
    let mut all = table.sorted();
    all.sort_by_key(|m| interner.resolve(m.name));
    let mut out = String::new();
    for def in all {
        define(&mut out, def, interner);
        out.push('\n');
    }
    out
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
    if let Some(builtin) = def.builtin {
        // A builtin has no body to print. GCC prints the name again, which reads oddly but is
        // the honest answer: there is no text, and what the macro stands for depends on where
        // it is used.
        out.push(' ');
        out.push_str(match builtin {
            Builtin::File => "__FILE__",
            Builtin::FileName => "__FILE_NAME__",
            Builtin::BaseFile => "__BASE_FILE__",
            Builtin::Line => "__LINE__",
            Builtin::IncludeLevel => "__INCLUDE_LEVEL__",
            Builtin::Counter => "__COUNTER__",
        });
        return;
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
    use rucc_diag::Span;

    use super::*;
    use crate::macros::parse_define;

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
        macros(&table, &interner)
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

    #[test]
    fn a_builtin_has_no_body_and_says_its_own_name() {
        let mut interner = Interner::new();
        let mut table = MacroTable::new();
        for (name, builtin) in Builtin::ALL {
            table.define_builtin(interner.intern(name), builtin, Span::new(0, 1));
        }
        let text = macros(&table, &interner);
        assert!(text.contains("#define __FILE__ __FILE__\n"), "{text}");
        assert!(text.contains("#define __COUNTER__ __COUNTER__\n"), "{text}");
        assert_eq!(text.lines().count(), Builtin::ALL.len());
    }
}
