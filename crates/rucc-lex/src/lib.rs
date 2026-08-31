//! Translation phases 1 to 3, pp-tokens, and the fast scanner.
//!
//! Design: `spec/05-preprocessor.md` sections 5.1 and 5.2, and `spec/06-lexer-and-parser.md`
//! section 6.1 for what happens to these tokens next. Layer rank 4, see
//! `spec/18-package-layout.md`.
//!
//! This is the hottest loop in the compiler at `-O0`, and it is also the place where being
//! clever costs correctness, so the two shapes it takes are worth stating plainly.
//!
//! Phases 1 and 2 are resolved lazily by the cursor, never by rewriting the buffer. A span is
//! always a range of real bytes in the file the user wrote, even when the token's spelling is
//! not those bytes read in order, and a token that crossed a splice or a trigraph says so
//! through [`TokenFlags::SPLICED`].
//!
//! Phase 3 is a loop over a 256-entry dispatch table, and identifiers are interned during the
//! scan rather than in a second pass, so nothing after this crate ever compares identifier
//! text.
//!
//! ```
//! use rucc_base::Interner;
//! use rucc_lex::{Options, PpTokenKind, tokenize};
//!
//! let mut interner = Interner::new();
//! let (tokens, diagnostics) = tokenize(b"int x = 1;", 0, Options::new(), &mut interner);
//! assert!(diagnostics.is_empty());
//! assert_eq!(tokens[0].kind, PpTokenKind::Ident);
//! assert_eq!(interner.resolve(tokens[0].value.unwrap()), "int");
//! ```
//!
//! # Status
//!
//! Phases 1 to 3 are real, along with the pp-token model, the dispatch table and interning
//! during the scan. Memory mapped input and the SIMD skips for whitespace and comment bodies
//! are the remaining performance items in M1 and are tracked on the milestone issue. Phases 4
//! to 6, which is directives and macro expansion, belong to `rucc-pp`.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-lex/0.0.1")]

mod class;
mod cursor;
mod lexer;
mod token;

pub use crate::lexer::{Lexer, Options, tokenize};
pub use crate::token::{PpToken, PpTokenKind, Punct, TokenFlags};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M1";

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    /// The kinds and spellings of every token in `src`, which is what almost every test here
    /// wants to assert on.
    fn scan(src: &str) -> (Vec<(PpTokenKind, String)>, Vec<String>) {
        let mut interner = Interner::new();
        let (tokens, diagnostics) = tokenize(src.as_bytes(), 0, Options::new(), &mut interner);
        let out = tokens
            .iter()
            .filter(|t| !t.is_eof())
            .map(|t| {
                let text = match t.value {
                    Some(sym) => interner.resolve(sym).to_owned(),
                    None => t.punct().map_or_else(String::new, |p| p.as_str().to_owned()),
                };
                (t.kind, text)
            })
            .collect();
        (out, diagnostics.iter().map(|d| d.message.clone()).collect())
    }

    fn spellings(src: &str) -> Vec<String> {
        scan(src).0.into_iter().map(|(_, text)| text).collect()
    }

    #[test]
    fn a_declaration_lexes_into_the_tokens_it_looks_like() {
        let (tokens, diagnostics) = scan("int x = 1;");
        assert!(diagnostics.is_empty());
        assert_eq!(
            tokens,
            vec![
                (PpTokenKind::Ident, "int".to_owned()),
                (PpTokenKind::Ident, "x".to_owned()),
                (PpTokenKind::Punct(Punct::Eq), "=".to_owned()),
                (PpTokenKind::Number, "1".to_owned()),
                (PpTokenKind::Punct(Punct::Semi), ";".to_owned()),
            ]
        );
    }

    #[test]
    fn punctuators_take_the_longest_match() {
        assert_eq!(spellings(">>="), vec![">>="]);
        assert_eq!(spellings(">> ="), vec![">>", "="]);
        assert_eq!(spellings("a->b"), vec!["a", "->", "b"]);
        assert_eq!(spellings("x+++y"), vec!["x", "++", "+", "y"]);
        assert_eq!(spellings("..."), vec!["..."]);
        assert_eq!(spellings(".."), vec![".", "."]);
        assert_eq!(spellings("[[gnu::packed]]"), vec!["[", "[", "gnu", "::", "packed", "]", "]"]);
    }

    #[test]
    fn digraphs_mean_the_same_thing_as_what_they_stand_for() {
        let (tokens, _) = scan("<% <: %: %:%: :> %>");
        let kinds: Vec<_> = tokens.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            vec![
                PpTokenKind::Punct(Punct::LBrace),
                PpTokenKind::Punct(Punct::LBracket),
                PpTokenKind::Punct(Punct::Hash),
                PpTokenKind::Punct(Punct::HashHash),
                PpTokenKind::Punct(Punct::RBracket),
                PpTokenKind::Punct(Punct::RBrace),
            ]
        );
    }

    #[test]
    fn a_digraph_says_it_was_written_as_one() {
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(b"<: [", 0, Options::new(), &mut interner);
        assert!(tokens[0].flags.has(TokenFlags::DIGRAPH));
        assert!(!tokens[1].flags.has(TokenFlags::DIGRAPH));
    }

    #[test]
    fn a_pp_number_is_looser_than_a_constant() {
        // Both of these are one pp-token. Only phase 7 has an opinion about `1.2.3`, and
        // splitting it here would break `##` pasting that assembles a number from pieces.
        assert_eq!(spellings("0x1p+3"), vec!["0x1p+3"]);
        assert_eq!(spellings("1.2.3"), vec!["1.2.3"]);
        assert_eq!(spellings(".5f"), vec![".5f"]);
        assert_eq!(spellings("1e-9"), vec!["1e-9"]);
        assert_eq!(spellings("0b1010"), vec!["0b1010"]);
        assert_eq!(spellings("42wb"), vec!["42wb"]);
    }

    #[test]
    fn c23_digit_separators_stay_inside_the_number() {
        assert_eq!(spellings("1'000'000"), vec!["1'000'000"]);
        // The apostrophe only separates when an identifier character follows, so this is a
        // number and then a character constant rather than one very confused number.
        assert_eq!(spellings("1 'a'"), vec!["1", "'a'"]);
    }

    #[test]
    fn literal_prefixes_belong_to_the_literal() {
        let (tokens, _) = scan(r#"L"wide" u8"utf8" u'c' U"big" L'w' u8'x'"#);
        let kinds: Vec<_> = tokens.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            vec![
                PpTokenKind::StringLit,
                PpTokenKind::StringLit,
                PpTokenKind::CharConst,
                PpTokenKind::StringLit,
                PpTokenKind::CharConst,
                PpTokenKind::CharConst,
            ]
        );
        assert_eq!(tokens[0].1, "L\"wide\"");
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_literal() {
        assert_eq!(spellings(r#""a\"b" x"#), vec![r#""a\"b""#, "x"]);
        assert_eq!(spellings(r"'\\' y"), vec![r"'\\'", "y"]);
    }

    #[test]
    fn a_literal_does_not_run_past_the_end_of_its_line() {
        // One missing quote must not swallow the rest of the file, which is the difference
        // between one error and a hundred.
        let (tokens, diagnostics) = scan("char *s = \"oops;\nint x;");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("missing terminating quote"));
        assert!(tokens.iter().any(|(k, text)| *k == PpTokenKind::Ident && text == "int"));
    }

    #[test]
    fn comments_are_whitespace_and_leave_a_space_behind() {
        assert_eq!(spellings("a/*b*/c"), vec!["a", "c"]);
        assert_eq!(spellings("a//b\nc"), vec!["a", "c"]);
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(b"a/*b*/c", 0, Options::new(), &mut interner);
        assert!(tokens[1].flags.has(TokenFlags::LEADING_SPACE));
    }

    #[test]
    fn an_unterminated_comment_is_reported_once() {
        let (_, diagnostics) = scan("int x; /* and then nothing");
        assert_eq!(diagnostics, vec!["unterminated comment".to_owned()]);
    }

    #[test]
    fn a_token_after_a_comment_that_crossed_a_line_still_starts_a_line() {
        // `# define` after a multi-line comment is a directive. GCC agrees, and real headers
        // are written this way, so getting it wrong means silently dropping a definition.
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(b"x /*\n*/ #define F 1", 0, Options::new(), &mut interner);
        assert!(!tokens[0].flags.has(TokenFlags::START_OF_LINE) || tokens[0].span.lo == 0);
        assert!(tokens[1].flags.has(TokenFlags::START_OF_LINE));
        assert_eq!(tokens[1].punct(), Some(Punct::Hash));
    }

    #[test]
    fn the_first_token_of_a_line_says_so() {
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(b"a b\nc", 0, Options::new(), &mut interner);
        assert!(tokens[0].flags.has(TokenFlags::START_OF_LINE));
        assert!(!tokens[1].flags.has(TokenFlags::START_OF_LINE));
        assert!(tokens[2].flags.has(TokenFlags::START_OF_LINE));
    }

    #[test]
    fn a_splice_joins_one_identifier_and_the_span_still_covers_real_bytes() {
        let mut interner = Interner::new();
        let (tokens, diagnostics) = tokenize(b"in\\\nt", 0, Options::new(), &mut interner);
        assert!(diagnostics.is_empty());
        assert_eq!(interner.resolve(tokens[0].value.unwrap()), "int");
        assert!(tokens[0].flags.has(TokenFlags::SPLICED));
        // The span covers all five bytes of the file, backslash and newline included, which
        // is what a caret under the identifier has to underline.
        assert_eq!(tokens[0].span.lo, 0);
        assert_eq!(tokens[0].span.hi, 5);
    }

    #[test]
    fn a_splice_inside_a_punctuator_still_makes_one_punctuator() {
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(b">\\\n>=", 0, Options::new(), &mut interner);
        assert_eq!(tokens[0].punct(), Some(Punct::ShrEq));
        assert!(tokens[0].flags.has(TokenFlags::SPLICED));
    }

    #[test]
    fn a_clean_token_is_not_marked_spliced() {
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(b"int", 0, Options::new(), &mut interner);
        assert!(!tokens[0].flags.has(TokenFlags::SPLICED));
    }

    #[test]
    fn trigraphs_are_off_by_default() {
        let (tokens, _) = scan("??=define");
        assert_eq!(tokens[0].0, PpTokenKind::Punct(Punct::Question));
        let mut interner = Interner::new();
        let opts = Options { trigraphs: true };
        let (on, _) = tokenize(b"??=define", 0, opts, &mut interner);
        assert_eq!(on[0].punct(), Some(Punct::Hash));
        assert_eq!(interner.resolve(on[1].value.unwrap()), "define");
    }

    #[test]
    fn a_stray_byte_is_a_token_rather_than_a_hard_stop() {
        // A pp-token that is nothing else is legal here and only becomes an error in phase 7,
        // because a macro is allowed to consume it first.
        let (tokens, diagnostics) = scan("a ` b");
        assert!(diagnostics.is_empty());
        assert_eq!(tokens[1].0, PpTokenKind::Other);
        assert_eq!(tokens[1].1, "`");
    }

    #[test]
    fn a_file_that_is_only_whitespace_lexes_to_end_of_file() {
        let mut interner = Interner::new();
        let (tokens, diagnostics) = tokenize(b"  \n\t\n", 0, Options::new(), &mut interner);
        assert!(diagnostics.is_empty());
        assert_eq!(tokens.len(), 1);
        assert!(tokens[0].is_eof());
    }

    #[test]
    fn the_empty_file_lexes_to_end_of_file() {
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(b"", 0, Options::new(), &mut interner);
        assert_eq!(tokens.len(), 1);
        assert!(tokens[0].is_eof());
    }

    #[test]
    fn spans_are_offset_by_where_the_file_sits() {
        // One flat coordinate space across the translation unit, per `rucc-diag`, so a file
        // that is not the first one still produces spans nobody has to translate.
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(b"ab", 1000, Options::new(), &mut interner);
        assert_eq!(tokens[0].span.lo, 1000);
        assert_eq!(tokens[0].span.hi, 1002);
    }

    #[test]
    fn a_header_name_is_only_scanned_when_a_directive_asks_for_one() {
        let mut interner = Interner::new();
        let mut lexer = Lexer::new(b"<stdio.h>", 0, Options::new());
        let header = lexer.header_name(&mut interner).expect("a header name starts here");
        assert_eq!(header.kind, PpTokenKind::HeaderName);
        assert_eq!(interner.resolve(header.value.unwrap()), "<stdio.h>");

        // The same bytes read as ordinary tokens are comparisons, which is exactly why the
        // scanner refuses to guess and the directive has to ask.
        let (tokens, _) = scan("<stdio.h>");
        assert_eq!(tokens[0].0, PpTokenKind::Punct(Punct::Lt));
    }

    #[test]
    fn a_quoted_header_name_works_and_a_computed_one_declines() {
        let mut interner = Interner::new();
        let mut lexer = Lexer::new(b" \"local.h\"", 0, Options::new());
        let header = lexer.header_name(&mut interner).expect("a header name starts here");
        assert_eq!(interner.resolve(header.value.unwrap()), "\"local.h\"");

        let mut lexer = Lexer::new(b"MACRO_NAME", 0, Options::new());
        assert!(lexer.header_name(&mut interner).is_none());
    }

    #[test]
    fn identifiers_are_interned_during_the_scan_and_repeat_for_free() {
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(b"foo bar foo", 0, Options::new(), &mut interner);
        assert_eq!(tokens[0].value, tokens[2].value);
        assert_ne!(tokens[0].value, tokens[1].value);
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn a_universal_character_name_is_part_of_the_identifier() {
        // Whether `é` names something that may appear in an identifier depends on
        // `-std=`, so phase 3 only has to keep it attached to the identifier it was written
        // in. Splitting it here would turn one name into three tokens.
        let (tokens, diagnostics) = scan(r"café = 1;");
        assert!(diagnostics.is_empty());
        assert_eq!(tokens[0], (PpTokenKind::Ident, r"café".to_owned()));
        assert_eq!(tokens[1].0, PpTokenKind::Punct(Punct::Eq));
    }

    #[test]
    fn a_backslash_with_a_trailing_space_splices_and_says_so() {
        // GCC warns and splices. Both halves matter: a lot of existing code has a stray space
        // after a backslash in a macro definition, and the space is invisible in an editor,
        // so the one time it changes the meaning nobody can see why.
        let (tokens, diagnostics) = scan("in\\  \nt x;");
        assert_eq!(tokens[0], (PpTokenKind::Ident, "int".to_owned()));
        assert_eq!(
            diagnostics,
            vec!["backslash and line ending separated by whitespace".to_owned()]
        );
    }

    #[test]
    fn utf8_in_an_identifier_survives_the_scan() {
        let (tokens, diagnostics) = scan("café = 1;");
        assert!(diagnostics.is_empty());
        assert_eq!(tokens[0].1, "café");
    }

    #[test]
    fn every_byte_of_the_file_ends_up_in_exactly_one_span_or_in_trivia() {
        // The property that keeps `-E` honest: spans never overlap and never run backwards.
        let src = "int main(void) { return 0; } /* c */ \"s\" 'c' 1.5e+3 // end\n";
        let mut interner = Interner::new();
        let (tokens, _) = tokenize(src.as_bytes(), 0, Options::new(), &mut interner);
        let mut last = 0;
        for t in &tokens {
            assert!(t.span.lo >= last, "spans went backwards at {:?}", t.kind);
            assert!(t.span.hi >= t.span.lo);
            last = t.span.hi;
        }
        assert_eq!(last as usize, src.len());
    }

    #[test]
    fn milestone_is_recorded() {
        assert!(MILESTONE.starts_with('M'));
    }
}
