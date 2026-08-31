//! The token the expander works on, which is a pp-token plus expansion bookkeeping.
//!
//! Design: `spec/05-preprocessor.md` section 5.3.
//!
//! `rucc_lex::PpToken` is sixteen bytes and `spec/05-preprocessor.md` section 5.2 wants it to
//! stay that way, because there is one of them per token of every header in the build and
//! the lexer is the hottest loop in the compiler. Expansion needs two more things per token:
//! a hide set, and the place the outermost macro was invoked so that a diagnostic inside a
//! macro can point at the call rather than at the definition.
//!
//! Those go on a separate, wider type rather than on `PpToken`. The working set of the
//! expander is one translation unit's worth of live tokens, not every token of every header,
//! so the extra eight bytes are affordable here and not there.

use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_lex::{PpToken, PpTokenKind, Punct, TokenFlags};

use crate::hide::HideSet;

/// A token in flight through macro expansion.
///
/// Construct one from a lexed token with [`Tok::new`]. The fields are readable because
/// everything downstream matches on them, but the placemarker flag is not settable from
/// outside the crate, because a placemarker escaping the expander would be a token with no
/// spelling and no meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tok {
    /// What kind of token this is.
    pub kind: PpTokenKind,
    /// Whitespace and origin flags, carried through from the lexer.
    pub flags: TokenFlags,
    /// The interned spelling, or `None` for a punctuator, whose spelling is fixed.
    pub value: Option<Symbol>,
    /// Where the token is spelled: in a macro body for a token that came from one, in the
    /// user's file for a token that did not.
    pub span: Span,
    /// Where the outermost macro invocation that produced this token was written, or
    /// [`Span::DUMMY`] for a token the user wrote directly.
    ///
    /// This is the span a diagnostic points at, with `span` becoming a note, which is what
    /// makes an error three macros deep readable.
    pub expansion: Span,
    /// The macro names that must not expand this token again.
    pub hides: HideSet,
    /// True for a placemarker, the empty token that `##` needs so that pasting an empty
    /// argument onto something yields the something rather than an error.
    ///
    /// Placemarkers exist only inside substitution and are dropped before the result leaves
    /// the expander, per `spec/05-preprocessor.md` section 5.3.
    pub(crate) placemarker: bool,
}

impl Tok {
    /// A token straight from the lexer, with an empty hide set and no expansion point.
    #[inline]
    pub fn new(pp: PpToken) -> Tok {
        Tok {
            kind: pp.kind,
            flags: pp.flags,
            value: pp.value,
            span: pp.span,
            expansion: Span::DUMMY,
            hides: HideSet::EMPTY,
            placemarker: false,
        }
    }

    /// The punctuator this token is, if it is one.
    #[inline]
    pub fn punct(self) -> Option<Punct> {
        match self.kind {
            PpTokenKind::Punct(p) if !self.placemarker => Some(p),
            _ => None,
        }
    }

    /// Whether this token is the punctuator `p`.
    #[inline]
    pub fn is(self, p: Punct) -> bool {
        self.punct() == Some(p)
    }

    /// The identifier this token is, if it is one.
    #[inline]
    pub fn ident(self) -> Option<Symbol> {
        match self.kind {
            PpTokenKind::Ident => self.value,
            _ => None,
        }
    }

    /// Whether this is the empty token `##` uses for an absent argument.
    #[inline]
    pub fn is_placemarker(self) -> bool {
        self.placemarker
    }

    /// The span to report a diagnostic about this token at.
    ///
    /// The invocation point when there is one, because a user reading an error wants the
    /// line they wrote, not a line in a header they have never opened.
    #[inline]
    pub fn report_span(self) -> Span {
        if self.expansion.is_dummy() { self.span } else { self.expansion }
    }

    /// A placemarker at `span`.
    pub(crate) fn placemarker_at(span: Span) -> Tok {
        Tok {
            kind: PpTokenKind::Other,
            flags: TokenFlags::EMPTY,
            value: None,
            span,
            expansion: Span::DUMMY,
            hides: HideSet::EMPTY,
            placemarker: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_diag::Span;

    use super::*;

    fn pp(kind: PpTokenKind) -> PpToken {
        PpToken { kind, flags: TokenFlags::EMPTY, value: None, span: Span::new(0, 1) }
    }

    #[test]
    fn a_lexed_token_starts_with_an_empty_hide_set() {
        let t = Tok::new(pp(PpTokenKind::Punct(Punct::Plus)));
        assert_eq!(t.hides, HideSet::EMPTY);
        assert!(t.expansion.is_dummy());
        assert!(!t.is_placemarker());
    }

    #[test]
    fn a_placemarker_is_not_a_punctuator() {
        let t = Tok::placemarker_at(Span::new(3, 3));
        assert!(t.is_placemarker());
        assert_eq!(t.punct(), None);
        assert_eq!(t.ident(), None);
    }

    #[test]
    fn a_token_from_a_macro_is_reported_at_the_call() {
        let mut t = Tok::new(pp(PpTokenKind::Ident));
        assert_eq!(t.report_span(), Span::new(0, 1));
        t.expansion = Span::new(40, 44);
        assert_eq!(t.report_span(), Span::new(40, 44));
    }
}
