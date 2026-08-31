//! The expansion trace: which macros a token came out of, and where each was written.
//!
//! Design: `spec/05-preprocessor.md` section 5.5, which asks that every token produced by
//! expansion carry its spelling location, its expansion location, and a pointer into a trace,
//! so that a diagnostic can print the chain from the error site up through three nested macros
//! to the call the user wrote. `spec/03-architecture.md` section 3.4 calls that chain the
//! single most useful thing a C compiler can do.
//!
//! A token already carried two spans. The one it did not carry is the middle: with only the
//! spelling and the outermost invocation, an error in a macro three deep says where the user
//! typed and where the text lives and nothing about how one became the other, which is the
//! part that is hard to work out by hand.
//!
//! # Shape
//!
//! One step per macro traversed, each pointing at the step outside it, so the chain is a linked
//! list running inwards out. Expansion goes the other way, outermost macro first, so a step is
//! built pointing at the one already there. Steps are interned on their contents, and that is
//! what makes this affordable: every token of one replacement list has the same name, the same
//! invocation and the same chain above it, so they all intern to one step. A hundred token
//! replacement list adds one node, not a hundred.
//!
//! There is one table per translation unit, so a [`TraceId`] from one is meaningless in
//! another, the same rule the hide sets follow.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_diag::Span;

/// A pointer into a [`Traces`] table, or [`TraceId::NONE`] for a token the user wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraceId(u32);

impl TraceId {
    /// No expansion, which is index zero in every table.
    ///
    /// A constant rather than a table lookup because a token straight from the lexer has one
    /// and building it should not need the table to exist yet.
    pub const NONE: TraceId = TraceId(0);

    /// Whether this token came out of no macro at all.
    #[inline]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// The underlying index, for packing a trace into a token.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// One macro traversed on the way from the user's text to a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Step {
    /// The macro that was expanded.
    pub macro_name: Symbol,
    /// Where its invocation was written. Inside another macro's body for every step but the
    /// outermost, and in a file the user wrote for that one.
    pub at: Span,
    /// The step this one sits inside, or [`TraceId::NONE`] if this is the outermost.
    pub outer: TraceId,
}

/// The interning table for expansion traces.
#[derive(Debug, Default)]
pub struct Traces {
    /// Every distinct step. A [`TraceId`] of `n` is `steps[n - 1]`, so that zero can mean no
    /// expansion without a placeholder entry that has to be built out of a `Symbol` nobody
    /// has yet.
    steps: Vec<Step>,
    map: HashMap<Step, TraceId>,
}

impl Traces {
    /// An empty table.
    pub fn new() -> Traces {
        Traces::default()
    }

    /// Records that `macro_name`, invoked at `at`, was reached from `outer`.
    ///
    /// The result is the trace the tokens it produces should carry. Expansion works outermost
    /// macro first, so the chain above a step is always already interned by the time the step
    /// is, and the list grows inwards.
    pub fn push(&mut self, macro_name: Symbol, at: Span, outer: TraceId) -> TraceId {
        let step = Step { macro_name, at, outer };
        if let Some(&found) = self.map.get(&step) {
            return found;
        }
        // Saturating rather than wrapping. A translation unit with four billion distinct
        // expansion steps has stopped being a translation unit, and reusing an index would
        // point a diagnostic at a macro that has nothing to do with it.
        let Ok(next) = u32::try_from(self.steps.len() + 1) else {
            return outer;
        };
        let id = TraceId(next);
        self.steps.push(step);
        self.map.insert(step, id);
        id
    }

    /// One step, or `None` for [`TraceId::NONE`].
    #[must_use]
    pub fn step(&self, id: TraceId) -> Option<Step> {
        self.steps.get((id.0 as usize).checked_sub(1)?).copied()
    }

    /// The chain from the outermost macro inwards, which is the order a reader wants it.
    ///
    /// The list points the other way, because a step can only be interned once the chain above
    /// it exists, so this walks it and reverses. Chains are a handful of steps long in the
    /// worst real case, so the allocation is not worth avoiding.
    #[must_use]
    pub fn chain(&self, id: TraceId) -> Vec<Step> {
        let mut out = Vec::new();
        let mut at = id;
        // A cycle cannot happen, because `push` only ever points a new step at an existing
        // one, but a bound costs nothing and a compiler that hangs is worse than one that is
        // wrong in a way you can see.
        while let Some(step) = self.step(at) {
            out.push(step);
            at = step.outer;
            if out.len() > 256 {
                break;
            }
        }
        out.reverse();
        out
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    /// Two names, since there is no way to make a `Symbol` without an interner and no reason
    /// to want one.
    fn names() -> (Interner, Symbol, Symbol) {
        let mut interner = Interner::new();
        let cat = interner.intern("CAT");
        let outer = interner.intern("OUTER");
        (interner, cat, outer)
    }

    fn span(lo: u32) -> Span {
        Span::new(lo, lo + 1)
    }

    #[test]
    fn a_token_the_user_wrote_has_no_chain() {
        let traces = Traces::new();
        assert!(TraceId::NONE.is_none());
        assert_eq!(traces.step(TraceId::NONE), None);
        assert!(traces.chain(TraceId::NONE).is_empty());
    }

    #[test]
    fn the_chain_reads_from_the_outermost_macro_inwards() {
        // What `#define CAT(a,b) a##b` used by `#define OUTER(y) CAT(y,+)` builds: `OUTER` is
        // expanded first, then `CAT` is found in what it produced.
        let (_interner, cat_name, outer_name) = names();
        let mut traces = Traces::new();
        let outer = traces.push(outer_name, span(40), TraceId::NONE);
        let cat = traces.push(cat_name, span(20), outer);
        let chain = traces.chain(cat);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].macro_name, outer_name);
        assert_eq!(chain[0].at, span(40));
        assert_eq!(chain[1].macro_name, cat_name);
        assert_eq!(chain[1].at, span(20));
    }

    #[test]
    fn the_same_step_twice_is_stored_once() {
        // The reason this is affordable. Every token of one replacement list arrives here with
        // the same name, the same invocation and the same chain above it.
        let (_interner, cat_name, _) = names();
        let mut traces = Traces::new();
        let first = traces.push(cat_name, span(20), TraceId::NONE);
        let second = traces.push(cat_name, span(20), TraceId::NONE);
        assert_eq!(first, second);
        assert_eq!(traces.steps.len(), 1);
        // A different chain above is a different step, even for the same macro at the same
        // place, because the same header can be reached two ways.
        let third = traces.push(cat_name, span(20), first);
        assert_ne!(third, first);
        assert_eq!(traces.chain(third).len(), 2);
    }

    #[test]
    fn two_macros_of_the_same_name_at_different_places_are_different_steps() {
        let (_interner, cat_name, _) = names();
        let mut traces = Traces::new();
        let here = traces.push(cat_name, span(20), TraceId::NONE);
        let there = traces.push(cat_name, span(90), TraceId::NONE);
        assert_ne!(here, there);
    }
}
