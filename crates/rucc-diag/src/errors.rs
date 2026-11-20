//! Collecting diagnostics, and the limit on how many one run will produce.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.8.
//!
//! Every pass that reports needs the same three things: somewhere to put the diagnostics, a
//! count of the ones that are errors, and a point at which it stops. It is here rather than in
//! the parser that first needed it because semantic analysis needs exactly the same sink and the
//! limit is one number for the whole compiler, not one per pass that happens to reach it.
//!
//! # Why counting errors is not what stops a cascade
//!
//! It is not. Every recovery leaves a poisoned node behind and a diagnostic about a poisoned
//! node is not reported, which is what [`Errors::push_unless`] is for, and the limit here is the
//! backstop for the file that is broken in more ways than one. A flag saying that something
//! already went wrong is close enough to work on small inputs and wrong on real ones, because it
//! either suppresses errors in code that was fine or fails to suppress the third message about
//! the same broken subexpression.

use crate::{Diagnostic, Severity};

/// How many errors are reported before a pass gives up.
///
/// The number is clang's, measured rather than assumed: clang 23.1 stops after twenty with
/// `too many errors emitted, stopping now`, and gcc 13.3 has no default limit at all and will
/// print every error a file produces. Twenty is the better default of the two, because the
/// errors after the twentieth in a file that is this broken are almost always consequences of
/// the ones before them, and the accepted flag for changing it is gcc's `-fmax-errors=N`, with
/// zero meaning no limit.
pub const DEFAULT_ERROR_LIMIT: usize = 20;

/// The diagnostics a pass produced, and the limit on how many it will produce.
#[derive(Debug)]
pub struct Errors {
    diagnostics: Vec<Diagnostic>,
    errors: usize,
    limit: usize,
    stopped: bool,
}

impl Default for Errors {
    fn default() -> Self {
        Errors::new(DEFAULT_ERROR_LIMIT)
    }
}

impl Errors {
    /// A sink that stops after `limit` errors, or that never stops when `limit` is zero.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Errors { diagnostics: Vec::new(), errors: 0, limit, stopped: false }
    }

    /// Records a diagnostic.
    ///
    /// Once the limit is reached nothing more is recorded, warnings included. The pass is about
    /// to stop and a warning arriving after the note that says so reads as though the compiler
    /// carried on regardless.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        if self.stopped {
            return;
        }
        let fatal = diagnostic.severity.is_fatal();
        let span = diagnostic.span;
        self.diagnostics.push(diagnostic);
        if fatal {
            self.errors += 1;
            if self.limit != 0 && self.errors >= self.limit {
                self.diagnostics.push(Diagnostic::new(
                    Severity::Note,
                    "too many errors emitted, stopping now",
                    span,
                ));
                self.stopped = true;
            }
        }
    }

    /// Records a diagnostic unless `suppressed` says the node it is about is already poisoned.
    ///
    /// Every pass that recovers leaves a poisoned node behind, and a message about such a node
    /// is not reported, which is what actually stops one error becoming twenty. What counts as
    /// poisoned is a fact about a tree rather than about a diagnostic, so the caller answers the
    /// question and this only honours the answer.
    ///
    /// The suppression is deliberately shallow: the question is whether the node the message is
    /// about is itself poisoned, not whether anything underneath it is. A poisoned operand makes
    /// its parent poisoned at the point the parent is built, so the answer propagates through
    /// the tree rather than through a walk of it, and a walk would make reporting an error cost
    /// the size of the subtree.
    pub fn push_unless(&mut self, suppressed: bool, diagnostic: Diagnostic) {
        if !suppressed {
            self.push(diagnostic);
        }
    }

    /// Whether the pass should stop, because it has reported as many errors as it will.
    #[inline]
    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// How many errors have been reported. Warnings and notes are not counted.
    #[inline]
    #[must_use]
    pub fn errors(&self) -> usize {
        self.errors
    }

    /// Whether anything was reported at all.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// How many diagnostics were reported, of every severity.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    /// What has been reported so far, in the order it was reported.
    ///
    /// For a caller that wants to look at the diagnostics and carry on, which is what a test
    /// does and what a pass that reports at the end of each function will do.
    #[inline]
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// What was reported, in the order it was reported.
    #[must_use]
    pub fn finish(self) -> Vec<Diagnostic> {
        self.diagnostics
    }
}

#[cfg(test)]
mod tests {
    use crate::Span;

    use super::*;

    #[test]
    fn the_limit_stops_the_run_and_says_so() {
        let mut errors = Errors::new(3);
        for _ in 0..10 {
            errors.push(Diagnostic::error("no", Span::empty_at(0)));
        }
        assert!(errors.stopped());
        assert_eq!(errors.errors(), 3);
        let diagnostics = errors.finish();
        assert_eq!(diagnostics.len(), 4);
        assert_eq!(diagnostics[3].severity, Severity::Note);
        assert_eq!(diagnostics[3].message, "too many errors emitted, stopping now");
    }

    #[test]
    fn a_limit_of_zero_never_stops() {
        let mut errors = Errors::new(0);
        for _ in 0..64 {
            errors.push(Diagnostic::error("no", Span::empty_at(0)));
        }
        assert!(!errors.stopped());
        assert_eq!(errors.len(), 64);
    }

    #[test]
    fn warnings_do_not_count_against_the_limit() {
        let mut errors = Errors::default();
        assert!(errors.is_empty());
        for _ in 0..64 {
            errors.push(Diagnostic::warning("hmm", Span::empty_at(0)));
        }
        assert_eq!(errors.errors(), 0);
        assert!(!errors.stopped());
    }

    #[test]
    fn a_message_about_a_poisoned_node_is_held_back() {
        let mut errors = Errors::default();
        let at = Span::empty_at(0);
        errors.push_unless(true, Diagnostic::error("about the broken one", at));
        assert!(errors.is_empty());
        errors.push_unless(false, Diagnostic::error("about the good one", at));
        assert_eq!(errors.len(), 1);
    }
}
