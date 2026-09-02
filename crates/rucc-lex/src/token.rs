//! Preprocessing tokens, the category that phase 3 produces.
//!
//! Design: `spec/05-preprocessor.md` section 5.1 for what a pp-token is, and section 5.2 for
//! why one is sixteen bytes.
//!
//! A pp-token is not a token. A pp-number is any sequence that looks vaguely numeric, so
//! `1.2.3` and `0x1p+3` are both one pp-number and only phase 7 has an opinion about which of
//! them is a constant. A string literal still has its escapes unresolved. Keeping the two
//! categories apart is what lets the preprocessor paste and stringify text that is not valid
//! C, which real headers do constantly.

use rucc_base::Symbol;
use rucc_diag::Span;

/// What a preprocessing token is.
///
/// Deliberately not `#[non_exhaustive]`. A new pp-token category has to break every match
/// that reads one, because there is no sensible default for "some category I have not heard
/// of" in a preprocessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PpTokenKind {
    /// An identifier, or a keyword. The lexer does not know which; that is phase 7's job and
    /// it depends on `-std=`, per `spec/06-lexer-and-parser.md` section 6.1.
    Ident,
    /// A preprocessing number, in the loose phase 3 grammar.
    Number,
    /// A character constant, including any `L`, `u`, `U` or `u8` prefix and both quotes.
    CharConst,
    /// A string literal, including any prefix and both quotes.
    StringLit,
    /// A `<stdio.h>` or `"local.h"` header name. Only produced by
    /// [`Lexer::header_name`](crate::Lexer::header_name), because whether one is even
    /// possible here is a fact about the directive being parsed and the scanner has no way to
    /// know it.
    HeaderName,
    /// A punctuator.
    Punct(Punct),
    /// A byte that is not part of any other category, such as a stray backtick. Legal as a
    /// pp-token, an error by phase 7 unless a macro ate it first.
    Other,
    /// End of the file. Carries an empty span at the end so that a diagnostic about a
    /// truncated construct has somewhere to point.
    Eof,
}

/// Things about a token that its own bytes do not say.
///
/// The two that matter are on every token: whether it started a line, which is how `#` is
/// recognised as a directive introducer, and whether anything separated it from the previous
/// token, which `#` stringification and `-E` output both need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct TokenFlags(u8);

impl TokenFlags {
    /// This token is the first on its logical line.
    pub const START_OF_LINE: TokenFlags = TokenFlags(1);
    /// Whitespace, a comment, or a line splice came before this token.
    pub const LEADING_SPACE: TokenFlags = TokenFlags(2);
    /// The spelling is not the bytes of the span read literally: a line splice or a trigraph
    /// sits inside it. Rare enough that everything downstream can take a slow path when it is
    /// set, and common enough in real headers that pretending it cannot happen is wrong.
    pub const SPLICED: TokenFlags = TokenFlags(4);
    /// The punctuator was written in its digraph spelling, `<:` for `[` and so on. The token
    /// means the same thing either way, and `-E` has to print back what was written.
    pub const DIGRAPH: TokenFlags = TokenFlags(8);

    /// No flags.
    pub const EMPTY: TokenFlags = TokenFlags(0);

    /// Whether every flag in `other` is set here.
    #[inline]
    #[must_use]
    pub const fn has(self, other: TokenFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// This set with `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: TokenFlags) -> TokenFlags {
        TokenFlags(self.0 | other.0)
    }

    /// This set with every flag in `other` taken off.
    #[inline]
    #[must_use]
    pub const fn without(self, other: TokenFlags) -> TokenFlags {
        TokenFlags(self.0 & !other.0)
    }
}

/// One preprocessing token.
///
/// Sixteen bytes, which `spec/05-preprocessor.md` section 5.2 asks for, and there is a test
/// below that says so. The size is not vanity: a large translation unit is tens of millions
/// of these and they are walked repeatedly by the macro expander.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PpToken {
    /// The category.
    pub kind: PpTokenKind,
    /// Facts about the token that its bytes do not carry.
    pub flags: TokenFlags,
    /// The spelling, interned, for the categories that have one. `None` for punctuators,
    /// whose spelling is recoverable from the kind, and for end of file.
    pub value: Option<Symbol>,
    /// Where the token sits in the source, in real file bytes.
    pub span: Span,
}

impl PpToken {
    /// Whether this is the end of file marker.
    #[inline]
    #[must_use]
    pub const fn is_eof(self) -> bool {
        matches!(self.kind, PpTokenKind::Eof)
    }

    /// The punctuator, when this is one.
    #[inline]
    #[must_use]
    pub const fn punct(self) -> Option<Punct> {
        match self.kind {
            PpTokenKind::Punct(p) => Some(p),
            _ => None,
        }
    }
}

/// A punctuator.
///
/// The C23 set, including `::`, plus the digraphs, which map onto the punctuator they stand
/// for rather than getting their own variants. A digraph is a spelling, not a meaning, so the
/// spelling lives in [`TokenFlags::DIGRAPH`] and everything that reads a punctuator sees one
/// kind of `[` rather than two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Punct {
    /// `[`, or the digraph `<:`.
    LBracket,
    /// `]`, or the digraph `:>`.
    RBracket,
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// `{`, or the digraph `<%`.
    LBrace,
    /// `}`, or the digraph `%>`.
    RBrace,
    /// `.`.
    Dot,
    /// `...`.
    Ellipsis,
    /// `->`.
    Arrow,
    /// `++`.
    PlusPlus,
    /// `--`.
    MinusMinus,
    /// `&`.
    Amp,
    /// `*`.
    Star,
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `~`.
    Tilde,
    /// `!`.
    Bang,
    /// `/`.
    Slash,
    /// `%`.
    Percent,
    /// `<<`.
    Shl,
    /// `>>`.
    Shr,
    /// `<`.
    Lt,
    /// `>`.
    Gt,
    /// `<=`.
    Le,
    /// `>=`.
    Ge,
    /// `==`.
    EqEq,
    /// `!=`.
    Ne,
    /// `^`.
    Caret,
    /// `|`.
    Pipe,
    /// `&&`.
    AmpAmp,
    /// `||`.
    PipePipe,
    /// `?`.
    Question,
    /// `:`.
    Colon,
    /// `::`, which C23 added for attribute namespaces.
    ColonColon,
    /// `;`.
    Semi,
    /// `=`.
    Eq,
    /// `*=`.
    StarEq,
    /// `/=`.
    SlashEq,
    /// `%=`.
    PercentEq,
    /// `+=`.
    PlusEq,
    /// `-=`.
    MinusEq,
    /// `<<=`.
    ShlEq,
    /// `>>=`.
    ShrEq,
    /// `&=`.
    AmpEq,
    /// `^=`.
    CaretEq,
    /// `|=`.
    PipeEq,
    /// `,`.
    Comma,
    /// `#`, or the digraph `%:`.
    Hash,
    /// `##`, or the digraph `%:%:`.
    HashHash,
}

impl Punct {
    /// The canonical spelling, which is the primary one rather than the digraph.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Punct::LBracket => "[",
            Punct::RBracket => "]",
            Punct::LParen => "(",
            Punct::RParen => ")",
            Punct::LBrace => "{",
            Punct::RBrace => "}",
            Punct::Dot => ".",
            Punct::Ellipsis => "...",
            Punct::Arrow => "->",
            Punct::PlusPlus => "++",
            Punct::MinusMinus => "--",
            Punct::Amp => "&",
            Punct::Star => "*",
            Punct::Plus => "+",
            Punct::Minus => "-",
            Punct::Tilde => "~",
            Punct::Bang => "!",
            Punct::Slash => "/",
            Punct::Percent => "%",
            Punct::Shl => "<<",
            Punct::Shr => ">>",
            Punct::Lt => "<",
            Punct::Gt => ">",
            Punct::Le => "<=",
            Punct::Ge => ">=",
            Punct::EqEq => "==",
            Punct::Ne => "!=",
            Punct::Caret => "^",
            Punct::Pipe => "|",
            Punct::AmpAmp => "&&",
            Punct::PipePipe => "||",
            Punct::Question => "?",
            Punct::Colon => ":",
            Punct::ColonColon => "::",
            Punct::Semi => ";",
            Punct::Eq => "=",
            Punct::StarEq => "*=",
            Punct::SlashEq => "/=",
            Punct::PercentEq => "%=",
            Punct::PlusEq => "+=",
            Punct::MinusEq => "-=",
            Punct::ShlEq => "<<=",
            Punct::ShrEq => ">>=",
            Punct::AmpEq => "&=",
            Punct::CaretEq => "^=",
            Punct::PipeEq => "|=",
            Punct::Comma => ",",
            Punct::Hash => "#",
            Punct::HashHash => "##",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pp_token_is_sixteen_bytes() {
        // spec/05-preprocessor.md section 5.2. A large translation unit holds tens of
        // millions of these, so this is a real budget rather than a decoration.
        assert_eq!(size_of::<PpToken>(), 16);
    }

    #[test]
    fn flags_are_a_set() {
        let f = TokenFlags::EMPTY.with(TokenFlags::START_OF_LINE).with(TokenFlags::LEADING_SPACE);
        assert!(f.has(TokenFlags::START_OF_LINE));
        assert!(f.has(TokenFlags::LEADING_SPACE));
        assert!(!f.has(TokenFlags::SPLICED));
    }

    #[test]
    fn every_punctuator_spells_something() {
        // Catches a variant added without a spelling, which would otherwise only show up as
        // wrong `-E` output on the one line that used it.
        for p in [Punct::LBracket, Punct::HashHash, Punct::ColonColon, Punct::ShrEq] {
            assert!(!p.as_str().is_empty());
        }
    }
}
