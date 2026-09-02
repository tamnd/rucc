//! Phase 7: preprocessing tokens become the tokens the parser reads.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.1.
//!
//! This is the join between the preprocessor and the parser, and it is the last place where a
//! spelling means anything. An identifier becomes a keyword when the dialect says that spelling
//! is one, a preprocessing number becomes a typed value, a literal becomes elements, a run of
//! adjacent string literals becomes one literal, and everything else is already what it is.
//!
//! # Where the values live
//!
//! A [`Token`] is sixteen bytes, the same budget a pp-token has and for the same reason: a
//! large translation unit is tens of millions of them and the parser walks them more than once.
//! A converted constant does not fit in sixteen bytes, so it does not live in the token. The
//! token holds a small index and the values live in four vectors beside them, which is also the
//! shape the parser wants, since it reaches for a constant's value at one node out of a hundred
//! and reads the kind at every one.
//!
//! # Where the warnings come from
//!
//! The conversions report what a constant did through [`Remarks`] and never decide that any of
//! it is a warning, because they do not hold the span. This is the layer that holds it, so this
//! is where a remark becomes a diagnostic. Which remarks are warnings at all depends on
//! `-pedantic`, and the split was measured on gcc 13.3 rather than guessed: a multi-character
//! constant, an escape out of range, an overflowing floating constant and a decimal constant
//! that came out unsigned are warnings with no flag at all, and the extensions, the escape gcc
//! invented, the imaginary suffix and everything the dialect does not have yet are quiet until
//! `-pedantic` asks.
//!
//! # What is not here
//!
//! Nothing turns a token back into text. `-E` prints pp-tokens, which is the stage before this
//! one, so a spelling is never reconstructed from a converted value.

use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_session::Std;
use rucc_target::TargetInfo;

use crate::keyword::{Keyword, Keywords};
use crate::literal::{CharConstant, LiteralError, StringLiteral};
use crate::number::{FloatConstant, IntConstant, IntError};
use crate::remarks::Remarks;
use crate::token::{PpToken, PpTokenKind, Punct, TokenFlags};

/// What a token is.
///
/// Two bytes, so that a [`Token`] fits in sixteen. The categories that carry a value carry it
/// in the token's `value` field instead of in the variant, which is what keeps it that small.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// A keyword in this dialect.
    Keyword(Keyword),
    /// An identifier, whose symbol is in [`Token::value`].
    Ident,
    /// An integer constant, indexed by [`Token::value`] into [`Tokens::ints`].
    Int,
    /// A floating constant, indexed by [`Token::value`] into [`Tokens::floats`].
    Float,
    /// A character constant, indexed by [`Token::value`] into [`Tokens::chars`].
    Char,
    /// A string literal, indexed by [`Token::value`] into [`Tokens::strings`]. One token per run
    /// of adjacent literals, because that is one literal.
    Str,
    /// A punctuator.
    Punct(Punct),
    /// End of the translation unit.
    Eof,
}

/// One token, as the parser reads it.
///
/// Sixteen bytes, checked by a test below, and laid out the way [`PpToken`] is for the same
/// reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// What it is.
    pub kind: TokenKind,
    /// Whether it started a line and whether anything came before it, carried through from the
    /// pp-token so that a diagnostic can say "did you mean to write this on its own line".
    pub flags: TokenFlags,
    /// The symbol for an identifier, the index into the matching vector of [`Tokens`] for a
    /// constant or a literal, and zero for everything else.
    pub value: u32,
    /// Where it came from, which for a run of string literals covers the whole run.
    pub span: Span,
}

impl Token {
    /// Whether this is the end of the translation unit.
    #[inline]
    #[must_use]
    pub const fn is_eof(self) -> bool {
        matches!(self.kind, TokenKind::Eof)
    }

    /// The keyword, when this is one.
    #[inline]
    #[must_use]
    pub const fn keyword(self) -> Option<Keyword> {
        match self.kind {
            TokenKind::Keyword(word) => Some(word),
            _ => None,
        }
    }

    /// The punctuator, when this is one.
    #[inline]
    #[must_use]
    pub const fn punct(self) -> Option<Punct> {
        match self.kind {
            TokenKind::Punct(punct) => Some(punct),
            _ => None,
        }
    }

    /// The identifier's symbol, when this is one.
    #[inline]
    #[must_use]
    pub const fn ident(self) -> Option<Symbol> {
        match self.kind {
            TokenKind::Ident => Some(Symbol::from_raw(self.value)),
            _ => None,
        }
    }
}

/// The tokens of a translation unit, with the values they refer to.
#[derive(Debug, Default)]
pub struct Tokens {
    /// The tokens themselves, ending in one [`TokenKind::Eof`].
    pub tokens: Vec<Token>,
    /// The integer constants, in the order they were converted.
    pub ints: Vec<IntConstant>,
    /// The floating constants, in the order they were converted.
    pub floats: Vec<FloatConstant>,
    /// The character constants, in the order they were converted.
    pub chars: Vec<CharConstant>,
    /// The string literals, one per run of adjacent ones.
    pub strings: Vec<StringLiteral>,
    /// The `#pragma` lines, in the order they were written.
    pub pragmas: Vec<Pragma>,
}

/// One `#pragma` line, and where in the token stream it stood.
///
/// The line is kept beside the tokens rather than in them. A pragma can appear anywhere a
/// line can, which is between any two tokens at all, so leaving it in the stream would mean
/// every rule in the parser had to know about a token that is not part of any grammar. What a
/// consumer needs instead is where it was, and [`Pragma::before`] is that.
///
/// Nothing acts on one yet. The record is here so that when something does, `#pragma pack`
/// most likely, the position it has to be applied at has not already been thrown away.
#[derive(Debug, Clone)]
pub struct Pragma {
    /// The index in [`Tokens::tokens`] of the token this line came before.
    pub before: u32,
    /// The tokens of the line, with the `#` and the `pragma` taken off.
    pub tokens: Vec<Token>,
    /// The `#`, which is where a diagnostic about the line points.
    pub span: Span,
}

impl Tokens {
    /// The integer constant `token` refers to, and [`None`] when it is not an integer constant.
    #[must_use]
    pub fn int(&self, token: Token) -> Option<&IntConstant> {
        match token.kind {
            TokenKind::Int => self.ints.get(token.value as usize),
            _ => None,
        }
    }

    /// The floating constant `token` refers to, and [`None`] when it is not one.
    #[must_use]
    pub fn float(&self, token: Token) -> Option<&FloatConstant> {
        match token.kind {
            TokenKind::Float => self.floats.get(token.value as usize),
            _ => None,
        }
    }

    /// The character constant `token` refers to, and [`None`] when it is not one.
    #[must_use]
    pub fn character(&self, token: Token) -> Option<&CharConstant> {
        match token.kind {
            TokenKind::Char => self.chars.get(token.value as usize),
            _ => None,
        }
    }

    /// The string literal `token` refers to, and [`None`] when it is not one.
    #[must_use]
    pub fn string(&self, token: Token) -> Option<&StringLiteral> {
        match token.kind {
            TokenKind::Str => self.strings.get(token.value as usize),
            _ => None,
        }
    }
}

/// Everything phase 7 needs that is not the tokens.
#[derive(Debug, Clone, Copy)]
pub struct Convert<'a> {
    /// The keyword table, built for this dialect before any source was read.
    pub keywords: &'a Keywords,
    /// Where the spellings are, since a pp-token carries a symbol rather than text.
    pub interner: &'a Interner,
    /// The target, which decides what a constant's type is and what a wide element is.
    pub target: &'a TargetInfo,
    /// The dialect, which decides what is a keyword and what earns a remark.
    pub std: Std,
    /// Whether the GNU extensions are on, which is `-std=gnu17` rather than `-std=c17`. gcc
    /// offers the C11 encoding prefixes from gnu99 on, so this decides whether `u8"x"` in a
    /// C99 program is a literal or an identifier next to one.
    pub gnu: bool,
    /// Whether `-pedantic` is on, which is the difference between a remark that is a warning
    /// and one that is nothing at all.
    pub pedantic: bool,
}

/// Converts a stream of preprocessing tokens into tokens.
///
/// The stream is what came out of macro expansion, so it has no directives left in it. A
/// spelling that will not convert produces a diagnostic and a token that stands in for it, so
/// that one bad constant does not cost the rest of the file its parse.
#[must_use]
pub fn convert(pp: &[PpToken], cx: &Convert<'_>) -> (Tokens, Vec<Diagnostic>) {
    let mut out = Tokens { tokens: Vec::with_capacity(pp.len()), ..Tokens::default() };
    let mut diagnostics = Vec::new();
    let mut index = 0;
    while index < pp.len() {
        index = one(pp, index, cx, &mut out, &mut diagnostics);
    }
    if out.tokens.last().is_none_or(|last| !last.is_eof()) {
        // Every caller of this ends up indexing past the last real token, so the stream always
        // ends in one of these even when the input did not.
        let end =
            out.tokens.last().map_or(Span::new(0, 0), |last| Span::new(last.span.hi, last.span.hi));
        out.tokens.push(Token {
            kind: TokenKind::Eof,
            flags: TokenFlags::EMPTY,
            value: 0,
            span: end,
        });
    }
    (out, diagnostics)
}

/// Converts the pp-token at `index`, appends what it became, and answers where the next one
/// starts.
///
/// One call is one token out, except for a run of adjacent string literals, which is one
/// literal, and for a `#pragma` line, which is one token followed by the line's own tokens.
fn one(
    pp: &[PpToken],
    index: usize,
    cx: &Convert<'_>,
    out: &mut Tokens,
    diagnostics: &mut Vec<Diagnostic>,
) -> usize {
    let token = pp[index];
    let mut index = index + 1;
    match token.kind {
        PpTokenKind::Ident => out.tokens.push(identifier(token, cx)),
        PpTokenKind::Number => {
            out.tokens.push(number(token, cx, &mut out.ints, &mut out.floats, diagnostics));
        }
        PpTokenKind::CharConst => {
            out.tokens.push(char_const(token, cx, &mut out.chars, diagnostics));
        }
        PpTokenKind::StringLit => {
            // A run of adjacent literals is one literal, so the run is taken here rather
            // than left for the parser, which would have to know the encoding rules to do
            // it and would be the second place that knows them.
            let start = index - 1;
            while pp.get(index).is_some_and(|next| next.kind == PpTokenKind::StringLit) {
                index += 1;
            }
            let run = &pp[start..index];
            out.tokens.push(string_lit(run, cx, &mut out.strings, diagnostics));
        }
        // `# pragma` at the start of a line. The preprocessor leaves these in the stream on
        // purpose, because what a pragma means is not its business, and this is where the
        // line stops being a `#` the parser would choke on and becomes a record of its own.
        PpTokenKind::Punct(Punct::Hash)
            if token.flags.has(TokenFlags::START_OF_LINE)
                && pp.get(index).is_some_and(|next| is_pragma(*next, cx)) =>
        {
            index += 1;
            let before = u32::try_from(out.tokens.len()).unwrap_or(u32::MAX);
            let mut line = Tokens::default();
            while pp.get(index).is_some_and(|next| {
                !matches!(next.kind, PpTokenKind::Eof) && !next.flags.has(TokenFlags::START_OF_LINE)
            }) {
                index = one(pp, index, cx, out, diagnostics);
                line.tokens.push(out.tokens.pop().expect("one token out"));
            }
            out.pragmas.push(Pragma { before, tokens: line.tokens, span: token.span });
        }
        PpTokenKind::Punct(punct) => out.tokens.push(Token {
            kind: TokenKind::Punct(punct),
            flags: token.flags,
            value: 0,
            span: token.span,
        }),
        PpTokenKind::Eof => out.tokens.push(Token {
            kind: TokenKind::Eof,
            flags: token.flags,
            value: 0,
            span: token.span,
        }),
        // A stray byte is a legal pp-token and never a token, and a header name cannot get
        // here at all, because only a directive asks for one and no directive survives to
        // this point. Both are reported and dropped, since there is nothing to stand in
        // for either of them.
        PpTokenKind::Other | PpTokenKind::HeaderName => {
            let text = spelling(token, cx);
            diagnostics.push(Diagnostic::error(format!("stray '{text}' in program"), token.span));
        }
    }
    index
}

/// Whether a pp-token is the word `pragma`, which is the only thing a `#` at the start of a
/// line can be followed by this late: every other directive was acted on and removed.
fn is_pragma(token: PpToken, cx: &Convert<'_>) -> bool {
    token.kind == PpTokenKind::Ident && spelling(token, cx) == "pragma"
}

/// The spelling of a pp-token that has one.
fn spelling<'a>(token: PpToken, cx: &Convert<'a>) -> &'a str {
    token.value.map_or("", |symbol| cx.interner.resolve(symbol))
}

/// An identifier, which the dialect may have made a keyword.
fn identifier(token: PpToken, cx: &Convert<'_>) -> Token {
    let symbol = token.value.expect("an identifier carries its spelling");
    let kind = match cx.keywords.get(symbol) {
        Some(word) => TokenKind::Keyword(word),
        None => TokenKind::Ident,
    };
    Token { kind, flags: token.flags, value: symbol.raw(), span: token.span }
}

/// A preprocessing number, which is an integer constant, a floating one, or neither.
fn number(
    token: PpToken,
    cx: &Convert<'_>,
    ints: &mut Vec<IntConstant>,
    floats: &mut Vec<FloatConstant>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Token {
    let text = spelling(token, cx);
    // Which of the two it is, is the integer path's answer rather than a guess made here: the
    // grammars overlap at the front and only one of them can tell where the number stops.
    match crate::number::integer(text, cx.std, cx.target) {
        Ok(value) => {
            report(value.remarks, None, token.span, cx, diagnostics);
            ints.push(value);
            let index = u32::try_from(ints.len() - 1).expect("that many constants in one file");
            Token { kind: TokenKind::Int, flags: token.flags, value: index, span: token.span }
        }
        Err(IntError::Floating) => match crate::number::floating(text, cx.std, cx.target) {
            Ok(value) => {
                report(value.remarks, Some(value.ty.name()), token.span, cx, diagnostics);
                floats.push(value);
                let index =
                    u32::try_from(floats.len() - 1).expect("that many constants in one file");
                Token { kind: TokenKind::Float, flags: token.flags, value: index, span: token.span }
            }
            Err(error) => {
                diagnostics.push(Diagnostic::error(error.message(), token.span));
                // A zero of the right shape, so that the expression around it still parses and
                // the user sees the one error they made rather than the ten it caused.
                floats.push(zero_float(cx));
                let index =
                    u32::try_from(floats.len() - 1).expect("that many constants in one file");
                Token { kind: TokenKind::Float, flags: token.flags, value: index, span: token.span }
            }
        },
        Err(error) => {
            diagnostics.push(Diagnostic::error(error.message(), token.span));
            ints.push(IntConstant {
                value: 0,
                ty: crate::number::IntConstantType::Standard(rucc_types::IntKind::Int),
                remarks: Remarks::NONE,
            });
            let index = u32::try_from(ints.len() - 1).expect("that many constants in one file");
            Token { kind: TokenKind::Int, flags: token.flags, value: index, span: token.span }
        }
    }
}

/// The `0.0` that stands in for a floating constant that would not convert.
fn zero_float(cx: &Convert<'_>) -> FloatConstant {
    let ty = crate::number::FloatConstantType::Double;
    FloatConstant {
        value: rucc_base::float::Float::zero(ty.format(cx.target), false),
        ty,
        imaginary: false,
        remarks: Remarks::NONE,
    }
}

/// A character constant.
fn char_const(
    token: PpToken,
    cx: &Convert<'_>,
    chars: &mut Vec<CharConstant>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Token {
    let text = spelling(token, cx);
    let value = match crate::literal::character(text, cx.std, cx.gnu, cx.target) {
        Ok(value) => {
            report(value.remarks, None, token.span, cx, diagnostics);
            value
        }
        Err(error) => {
            diagnostics.push(Diagnostic::error(error.message(), token.span));
            CharConstant {
                value: 0,
                encoding: crate::literal::Encoding::Plain,
                remarks: Remarks::NONE,
            }
        }
    };
    chars.push(value);
    let index = u32::try_from(chars.len() - 1).expect("that many constants in one file");
    Token { kind: TokenKind::Char, flags: token.flags, value: index, span: token.span }
}

/// A run of adjacent string literals, which is one literal.
fn string_lit(
    run: &[PpToken],
    cx: &Convert<'_>,
    strings: &mut Vec<StringLiteral>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Token {
    let first = run[0];
    let span = first.span.to(run[run.len() - 1].span);
    let texts: Vec<&str> = run.iter().map(|token| spelling(*token, cx)).collect();
    let value = match crate::literal::strings(&texts, cx.std, cx.gnu, cx.target) {
        Ok(value) => {
            report(value.remarks, None, span, cx, diagnostics);
            value
        }
        Err(error) => {
            diagnostics.push(Diagnostic::error(error.message(), span));
            // An empty literal of the encoding the run asked for, if it managed to agree on
            // one, so that a `char *` initialised from it is still a `char *`.
            let encoding = if error == LiteralError::MixedEncodings {
                crate::literal::Encoding::Plain
            } else {
                crate::literal::Encoding::read_prefix(texts[0])
            };
            StringLiteral { elements: Vec::new(), encoding, remarks: Remarks::NONE }
        }
    };
    strings.push(value);
    let index = u32::try_from(strings.len() - 1).expect("that many literals in one file");
    Token { kind: TokenKind::Str, flags: first.flags, value: index, span }
}

/// Turns the remarks a conversion made into the diagnostics this dialect wants.
///
/// `type_name` is the type a floating constant came out as, which the overflow wording names.
/// The split between the warnings that need `-pedantic` and the ones that do not was measured
/// on gcc 13.3 rather than guessed at.
fn report(
    remarks: Remarks,
    type_name: Option<&str>,
    span: Span,
    cx: &Convert<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if remarks.is_none() {
        return;
    }

    // On by default in gcc, because every one of these is a value that is not what the source
    // looks like it says.
    let always: [(Remarks, &str); 6] = [
        (Remarks::MULTICHARACTER, "multi-character character constant"),
        (Remarks::TOO_LONG, "character constant too long for its type"),
        (Remarks::UNKNOWN_ESCAPE, "unknown escape sequence"),
        (Remarks::HEX_ESCAPE_OUT_OF_RANGE, "hex escape sequence out of range"),
        (Remarks::OCTAL_ESCAPE_OUT_OF_RANGE, "octal escape sequence out of range"),
        (Remarks::UNSIGNED, "integer constant is so large that it is unsigned"),
    ];
    for (remark, message) in always {
        if remarks.has(remark) {
            diagnostics.push(Diagnostic::warning(message, span));
        }
    }
    if remarks.has(Remarks::OUT_OF_RANGE) {
        let ty = type_name.unwrap_or("double");
        diagnostics
            .push(Diagnostic::warning(format!("floating constant exceeds range of '{ty}'"), span));
    }
    if remarks.has(Remarks::TRUNCATED) {
        diagnostics.push(Diagnostic::warning("floating constant truncated to zero", span));
    }

    if !cx.pedantic {
        return;
    }
    // Quiet without `-pedantic`, because each of these is a value the compiler understood
    // perfectly well and only the standard objects to.
    let pedantic: [(Remarks, &str); 9] = [
        (Remarks::NON_ISO_ESCAPE, "non-ISO-standard escape sequence"),
        (Remarks::DOUBLE_SUFFIX, "suffix for double constant is a GCC extension"),
        (Remarks::IMAGINARY, "imaginary constants are a GCC extension"),
        (Remarks::BINARY, "binary constants are a C23 feature or GCC extension"),
        (Remarks::EXTENDED_SUFFIX, "non-standard suffix on floating constant"),
        (Remarks::HEX_FLOAT, "use of C99 hexadecimal floating constant"),
        (Remarks::LONG_LONG, "use of C99 long long integer constant"),
        (Remarks::SEPARATORS, "digit separators are a C23 feature"),
        (Remarks::BIT_INT, "'_BitInt' constants are a C23 feature"),
    ];
    for (remark, message) in pedantic {
        if remarks.has(remark) {
            diagnostics.push(Diagnostic::warning(message, span));
        }
    }
    if remarks.has(Remarks::UCN) {
        diagnostics.push(Diagnostic::warning(
            "universal character names are only valid in C++ and C99",
            span,
        ));
    }
}

#[cfg(test)]
mod tests {
    use rucc_target::Triple;

    use super::*;
    use crate::lexer::{Options, tokenize};

    /// Everything a conversion needs, built the way a driver would build it: the keyword table
    /// first, before any source has been interned.
    struct Fixture {
        interner: Interner,
        keywords: Keywords,
        target: TargetInfo,
        std: Std,
        gnu: bool,
        pedantic: bool,
    }

    impl Fixture {
        fn new(std: Std) -> Fixture {
            let mut interner = Interner::new();
            let keywords = Keywords::new(&mut interner, std, true);
            let target =
                TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
            Fixture { interner, keywords, target, std, gnu: false, pedantic: false }
        }

        /// Lexes and converts `src`, which is what a translation unit with no directives does.
        fn run(&mut self, src: &str) -> (Tokens, Vec<String>) {
            let (pp, lex_diagnostics) =
                tokenize(src.as_bytes(), 0, Options::new(), &mut self.interner);
            assert!(lex_diagnostics.is_empty(), "the scanner disliked the source: {src}");
            let cx = Convert {
                keywords: &self.keywords,
                interner: &self.interner,
                target: &self.target,
                std: self.std,
                gnu: self.gnu,
                pedantic: self.pedantic,
            };
            let (tokens, diagnostics) = convert(&pp, &cx);
            (tokens, diagnostics.iter().map(|d| d.message.clone()).collect())
        }
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        Fixture::new(Std::C23).run(src).0.tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn a_token_is_sixteen_bytes() {
        // The same budget a pp-token has, and for the same reason: a large translation unit
        // holds tens of millions of these and the parser walks them more than once.
        assert_eq!(size_of::<Token>(), 16);
    }

    #[test]
    fn a_declaration_converts_into_keywords_an_identifier_and_a_constant() {
        assert_eq!(
            kinds("int x = 1;"),
            vec![
                TokenKind::Keyword(Keyword::Int),
                TokenKind::Ident,
                TokenKind::Punct(Punct::Eq),
                TokenKind::Int,
                TokenKind::Punct(Punct::Semi),
                TokenKind::Eof,
            ]
        );
    }

    /// Which spellings are keywords is the dialect's business, and phase 7 is where it lands.
    #[test]
    fn the_dialect_decides_which_identifiers_are_keywords() {
        let mut c89 = Fixture::new(Std::C89);
        let (tokens, _) = c89.run("restrict");
        assert_eq!(tokens.tokens[0].kind, TokenKind::Ident);
        let mut c99 = Fixture::new(Std::C99);
        let (tokens, _) = c99.run("restrict");
        assert_eq!(tokens.tokens[0].kind, TokenKind::Keyword(Keyword::Restrict));
    }

    #[test]
    fn a_number_becomes_whichever_kind_of_constant_it_is() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, diagnostics) = fixture.run("1 2.5 0x1p3 1u");
        assert!(diagnostics.is_empty());
        let kinds: Vec<_> = tokens.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Int,
                TokenKind::Float,
                TokenKind::Float,
                TokenKind::Int,
                TokenKind::Eof
            ]
        );
        assert_eq!(tokens.int(tokens.tokens[0]).expect("an integer").value, 1);
        assert!(tokens.float(tokens.tokens[1]).is_some());
        // The index is into the vector for that kind, so the second float is the second entry
        // and the second integer is not.
        assert_eq!(tokens.tokens[2].value, 1);
        assert_eq!(tokens.tokens[3].value, 1);
        assert_eq!(tokens.int(tokens.tokens[3]).expect("an integer").value, 1);
        // Asking the wrong kind for its value gets nothing rather than the wrong constant.
        assert!(tokens.float(tokens.tokens[0]).is_none());
        assert!(tokens.string(tokens.tokens[0]).is_none());
    }

    /// A run of adjacent literals is one token, because it is one object, and the span covers
    /// all of it so that a diagnostic underlines the whole thing.
    #[test]
    fn adjacent_string_literals_become_one_token() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, diagnostics) = fixture.run(r#"char *s = "a" "b" L"c";"#);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let literal = tokens
            .tokens
            .iter()
            .find(|t| t.kind == TokenKind::Str)
            .copied()
            .expect("a string literal");
        let value = tokens.string(literal).expect("the literal");
        assert_eq!(value.elements, vec![0x61, 0x62, 0x63]);
        assert_eq!(value.encoding, crate::literal::Encoding::Wide);
        assert_eq!(tokens.tokens.iter().filter(|t| t.kind == TokenKind::Str).count(), 1);
        // The span runs from the first quote to the last.
        assert_eq!(literal.span.lo, 10);
        assert_eq!(literal.span.hi, 22);
    }

    #[test]
    fn a_character_constant_carries_its_value_and_its_warning() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, diagnostics) = fixture.run("'ab'");
        assert_eq!(diagnostics, vec!["multi-character character constant".to_owned()]);
        assert_eq!(tokens.character(tokens.tokens[0]).expect("a constant").value, 0x6162);
    }

    /// Measured on gcc 13.3: these are warnings with no flag at all, because each one is a
    /// value that is not what the source looks like it says.
    #[test]
    fn the_warnings_that_need_no_flag_are_given_without_one() {
        let mut fixture = Fixture::new(Std::C17);
        let (_, diagnostics) = fixture.run(r"'abcde' '\q' '\x1ff' '\400' 1e400 1e-400");
        assert_eq!(
            diagnostics,
            vec![
                "character constant too long for its type".to_owned(),
                "unknown escape sequence".to_owned(),
                "hex escape sequence out of range".to_owned(),
                "octal escape sequence out of range".to_owned(),
                "floating constant exceeds range of 'double'".to_owned(),
                "floating constant truncated to zero".to_owned(),
            ]
        );
    }

    /// And these are quiet until `-pedantic` asks, which was measured the same way.
    #[test]
    fn the_warnings_that_need_pedantic_wait_for_it() {
        let mut quiet = Fixture::new(Std::C17);
        let (_, diagnostics) = quiet.run(r"1.0d 1.0i 0b1010 '\e'");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let mut loud = Fixture::new(Std::C17);
        loud.pedantic = true;
        let (_, diagnostics) = loud.run(r"1.0d 1.0i 0b1010 '\e'");
        assert_eq!(
            diagnostics,
            vec![
                "suffix for double constant is a GCC extension".to_owned(),
                "imaginary constants are a GCC extension".to_owned(),
                "binary constants are a C23 feature or GCC extension".to_owned(),
                "non-ISO-standard escape sequence".to_owned(),
            ]
        );
    }

    #[test]
    fn the_overflow_warning_names_the_type_the_constant_actually_has() {
        let mut fixture = Fixture::new(Std::C23);
        let (_, diagnostics) = fixture.run("1e400f");
        assert_eq!(diagnostics, vec!["floating constant exceeds range of 'float'".to_owned()]);
    }

    /// One bad constant costs one diagnostic and nothing else, because the token it stands in
    /// for is still there and the rest of the declaration still parses.
    #[test]
    fn a_constant_that_will_not_convert_still_leaves_a_token_behind() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, diagnostics) = fixture.run("int x = 1.2.3;");
        assert_eq!(diagnostics.len(), 1);
        let kinds: Vec<_> = tokens.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Keyword(Keyword::Int),
                TokenKind::Ident,
                TokenKind::Punct(Punct::Eq),
                TokenKind::Float,
                TokenKind::Punct(Punct::Semi),
                TokenKind::Eof,
            ]
        );

        let mut fixture = Fixture::new(Std::C23);
        let (tokens, diagnostics) = fixture.run("int x = 42ux;");
        assert_eq!(diagnostics, vec!["invalid suffix on integer constant".to_owned()]);
        assert_eq!(tokens.int(tokens.tokens[3]).expect("a stand in").value, 0);
    }

    #[test]
    fn a_run_of_literals_with_two_prefixes_is_refused_the_way_gcc_refuses_it() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, diagnostics) = fixture.run(r#"u"a" L"b""#);
        assert_eq!(
            diagnostics,
            vec!["unsupported non-standard concatenation of string literals".to_owned()]
        );
        assert!(tokens.string(tokens.tokens[0]).expect("a stand in").elements.is_empty());
    }

    /// A stray byte is a legal pp-token, so the scanner passes it through and this is the layer
    /// that has to say no.
    #[test]
    fn a_stray_byte_is_an_error_here_and_nowhere_earlier() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, diagnostics) = fixture.run("a ` b");
        assert_eq!(diagnostics, vec!["stray '`' in program".to_owned()]);
        let kinds: Vec<_> = tokens.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![TokenKind::Ident, TokenKind::Ident, TokenKind::Eof]);
    }

    #[test]
    fn the_stream_always_ends_in_end_of_file() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, _) = fixture.run("");
        assert_eq!(tokens.tokens.len(), 1);
        assert!(tokens.tokens[0].is_eof());
        // Even when the input had none, which is what a caller building a stream by hand does.
        let (tokens, _) = convert(
            &[],
            &Convert {
                keywords: &fixture.keywords,
                interner: &fixture.interner,
                target: &fixture.target,
                std: fixture.std,
                gnu: false,
                pedantic: false,
            },
        );
        assert_eq!(tokens.tokens.len(), 1);
        assert!(tokens.tokens[0].is_eof());
    }

    #[test]
    fn a_token_says_what_it_is_without_the_caller_matching_on_the_kind() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, _) = fixture.run("int x;");
        assert_eq!(tokens.tokens[0].keyword(), Some(Keyword::Int));
        assert_eq!(tokens.tokens[0].ident(), None);
        assert!(tokens.tokens[1].ident().is_some());
        assert_eq!(tokens.tokens[2].punct(), Some(Punct::Semi));
        assert_eq!(tokens.tokens[2].keyword(), None);
    }

    /// The preprocessor leaves a `#pragma` line alone on purpose, so this is the only layer
    /// that can take it out, and a `#` reaching the parser is a syntax error every time.
    #[test]
    fn a_pragma_line_leaves_the_stream_and_is_kept_beside_it() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, diagnostics) = fixture.run("int a;\n#pragma pack(4)\nint b;");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let kinds: Vec<_> = tokens.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Keyword(Keyword::Int),
                TokenKind::Ident,
                TokenKind::Punct(Punct::Semi),
                TokenKind::Keyword(Keyword::Int),
                TokenKind::Ident,
                TokenKind::Punct(Punct::Semi),
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens.pragmas.len(), 1);
        let pragma = &tokens.pragmas[0];
        // Three tokens in and three to go, which is the second declaration, which is the one
        // a `#pragma pack` here would have to apply to.
        assert_eq!(pragma.before, 3);
        let kinds: Vec<_> = pragma.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Ident,
                TokenKind::Punct(Punct::LParen),
                TokenKind::Int,
                TokenKind::Punct(Punct::RParen),
            ]
        );
    }

    /// The two ends of a file are where an off-by-one in the line loop shows up, so both are
    /// here: nothing before the first pragma, and nothing after the last.
    #[test]
    fn a_pragma_at_either_end_of_the_file_is_still_a_line() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, diagnostics) = fixture.run("#pragma once\nint a;\n#pragma GCC poison x");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(tokens.tokens.len(), 4);
        assert_eq!(tokens.pragmas.len(), 2);
        assert_eq!(tokens.pragmas[0].before, 0);
        assert_eq!(tokens.pragmas[0].tokens.len(), 1);
        assert_eq!(tokens.pragmas[1].before, 3);
        assert_eq!(tokens.pragmas[1].tokens.len(), 3);
    }

    /// A `#` that is not a pragma is still a token, because at this point every directive has
    /// already been acted on and anything left is the program's own mistake to hear about.
    #[test]
    fn a_hash_that_is_not_a_pragma_is_left_where_it_is() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, _) = fixture.run("#define x\nint pragma;\n# pragma");
        assert_eq!(tokens.tokens[0].kind, TokenKind::Punct(Punct::Hash));
        // The word alone is an identifier, and a `#` in the middle of a line is not a
        // directive, so the only pragma here is the one written as one.
        assert_eq!(tokens.pragmas.len(), 1);
    }

    /// gcc's own spellings of the 128 bit types, which are typedef names everywhere else and
    /// keywords here because there is nowhere to write the typedef.
    #[test]
    fn the_two_extra_spellings_of_the_wide_integer_are_keywords() {
        let kinds = kinds("__int128_t a; __uint128_t b;");
        assert_eq!(kinds[0], TokenKind::Keyword(Keyword::Int128T));
        assert_eq!(kinds[3], TokenKind::Keyword(Keyword::UInt128T));
        // Not a dialect question. gcc offers them at every level and so do we.
        let mut c89 = Fixture::new(Std::C89);
        let (tokens, _) = c89.run("__uint128_t");
        assert_eq!(tokens.tokens[0].kind, TokenKind::Keyword(Keyword::UInt128T));
    }

    /// The flags survive the conversion, because a diagnostic about a token that should have
    /// been on its own line needs to know that it was not.
    #[test]
    fn the_flags_come_through_from_the_preprocessing_token() {
        let mut fixture = Fixture::new(Std::C23);
        let (tokens, _) = fixture.run("a\n b");
        assert!(tokens.tokens[0].flags.has(TokenFlags::START_OF_LINE));
        assert!(tokens.tokens[1].flags.has(TokenFlags::START_OF_LINE));
        assert!(tokens.tokens[1].flags.has(TokenFlags::LEADING_SPACE));
    }
}
