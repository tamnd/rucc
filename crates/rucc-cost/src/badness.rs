//! How bad a call is to inline, which is the order the inliner's second pass takes calls in.
//!
//! Design: sections 33.4 and 33.5 of `spec/optimizer/33-inlining.md`, and section 40.11 of
//! `spec/optimizer/40-cost-model.md`, which this discharges.
//!
//! The second pass keeps the calls it could still inline in a heap and takes the one with the least
//! badness first, measuring again what each decision changed. Everything that goes into the number
//! is here and nowhere else, so the question of why one call went in before another has one place
//! to be answered.
//!
//! gcc's formula, from the comment in `edge_badness` at `gcc/ipa-inline.cc:1350`, is
//!
//! ```text
//!                  time_saved * frequency
//! goodness =  ---------------------------------------------
//!             growth * overall_growth' * (caller + growth)
//!
//! badness = - goodness
//! ```
//!
//! where `overall_growth` is how much the unit grows if the callee is inlined at every call, and
//! the prime is the piecewise function in [`overall`]. The frequency is a guess, since M4 has no
//! profile, and section 40.11 holds a guess to [`INLINE_FREQUENCY_CLAMP`] times the caller's entry
//! so that three loops the predictor guessed at cannot make a call a thousand times more urgent
//! than it is. That clamp is the one change from gcc's form. A call that does not grow the program
//! comes before every call that does, and the hints of section 33.5 shift what is left.

use crate::heuristics::{INLINE_FREQUENCY_CLAMP, INLINE_GROWTH_SQUARING_BOUND};

/// What section 33.5's hints say about one call, the ones that change a limit or a badness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hints {
    /// The copy makes a loop's trip count or stride known, or turns a call through a pointer
    /// into a direct one. gcc's `loop_iterations`, `loop_stride` and `indirect_call`, which it
    /// treats as one group.
    pub enables: bool,
    /// A `__builtin_constant_p` in the body asks about a parameter the call passes a constant for,
    /// which is gcc's `builtin_constant_p` and the other group.
    pub asks: bool,
    /// The callee was declared `inline`.
    pub declared: bool,
}

impl Hints {
    /// What a size limit of `base` becomes for a call with these hints, when a hint raises a limit
    /// by `percent`.
    ///
    /// This is `inline_insns_single` and `inline_insns_auto` at `gcc/ipa-inline.cc:470`. One group
    /// of hints scales the limit by the percentage. Both scale it by the square of the percentage
    /// over a hundred, held to a million, which at gcc's 200 is four hundred times the limit. That
    /// is far more than section 33.5's reading of it, and it is what gcc does, so it is what this
    /// does: with both kinds of hint the limit is out of the way and only the bounds on growth are
    /// left to stop the call.
    #[must_use]
    pub fn limit(self, base: u32, percent: u32) -> u64 {
        let base = u64::from(base);
        let percent = u64::from(percent);
        match (self.enables, self.asks) {
            (true, true) => base * (percent * percent).min(1_000_000) / 100,
            (true, false) | (false, true) => base * percent / 100,
            (false, false) => base,
        }
    }
}

/// What is known about one call when it is weighed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Call {
    /// How many instructions the copy adds to the caller, which is the body less the call.
    pub growth: i64,
    /// How much work the call no longer does each time it runs once it is a copy: the call
    /// itself, and what the constants it passes fold out of the body.
    pub saved: f64,
    /// How often the call runs for each time its caller does, when there is a guess at it.
    pub frequency: Option<f64>,
    /// How many loops the call is in, which is what is left to go on with no frequency.
    pub depth: u32,
    /// How much the unit grows if every call to the callee is inlined, less the body when nothing
    /// would be left to call it.
    pub overall: i64,
    /// How many instructions the caller has before the copy.
    pub caller: i64,
    /// What the hints of section 33.5 say about it.
    pub hints: Hints,
}

/// The badness of a call that makes the program smaller, with its growth added, so that among
/// those the one that saves the most comes first. Far below anything the formula can give.
const SHRINKS: f64 = -1.0e12;

/// gcc's piecewise overall growth term: squared below [`INLINE_GROWTH_SQUARING_BOUND`], linear
/// above it, and the two meeting at the bound.
///
/// The square says that a function with few callers which can be inlined at all of them, and then
/// has no body of its own, is worth much more than one inlined at some of its fifty callers.
/// Squared without limit the term would leave a large function with many callers impossible to
/// inline anywhere, so above the bound it goes on growing by one for every one.
#[must_use]
pub fn overall(growth: i64) -> f64 {
    let bound = i64::from(INLINE_GROWTH_SQUARING_BOUND);
    if growth < bound { (growth * growth) as f64 } else { (growth + bound * bound - bound) as f64 }
}

/// How bad a call is to inline, the least bad being the one to take first.
///
/// [`Call::frequency`] is held between one and [`INLINE_FREQUENCY_CLAMP`], which is section
/// 40.11's form. With no frequency at all it is gcc's fourth case, the growth halved for every
/// loop the call is in, up to eight of them.
#[must_use]
pub fn badness(call: &Call) -> f64 {
    let base = if call.growth <= 0 {
        SHRINKS + call.growth as f64
    } else if let Some(frequency) = call.frequency {
        let frequency = frequency.clamp(1.0, f64::from(INLINE_FREQUENCY_CLAMP));
        // gcc takes a call that saves nothing as saving a little, so that it still has an order.
        let saved = if call.saved > 0.0 { call.saved } else { 1.0 / 256.0 };
        let mut denominator = call.growth as f64 * (call.caller + call.growth).max(1) as f64;
        if call.overall > 0 {
            denominator *= overall(call.overall);
        }
        -(saved * frequency) / denominator
    } else {
        call.growth as f64 / f64::from(1u32 << call.depth.min(8))
    };
    let mut badness = base;
    if call.hints.enables || call.overall <= 0 {
        badness = favour(badness, 2);
    }
    if call.hints.asks {
        badness = favour(badness, 4);
    }
    if call.hints.declared {
        badness = favour(badness, 3);
    }
    badness
}

/// Whether inlining saves at least `percent` of the time the call and its caller take, which is
/// gcc's `big_speedup_p` and lets a call over its limit in once.
///
/// `callee` is how long the body takes as it stands, `specialized` how long the copy takes once
/// the constants the call passes have folded it, `call` what the call itself costs, `frequency`
/// how often it runs for each time the caller does and `caller` how long the caller takes, the
/// call included. The copy takes the call out of the caller, which is how
/// `compute_inlined_call_time` at `gcc/ipa-inline.cc:790` counts it.
#[must_use]
pub fn big_speedup(
    callee: f64,
    specialized: f64,
    call: f64,
    frequency: f64,
    caller: f64,
    percent: u32,
) -> bool {
    let frequency = frequency.clamp(1.0, f64::from(INLINE_FREQUENCY_CLAMP));
    let before = callee * frequency + caller;
    let after = ((specialized - call) * frequency + caller).max(1.0 / 256.0);
    (before - after) * 100.0 > before * f64::from(percent)
}

/// A badness made better by a factor of two to the `shift`, which is gcc's `sreal::shift` toward
/// the front of the heap whichever side of zero the badness is on.
fn favour(badness: f64, shift: u32) -> f64 {
    let by = f64::from(1u32 << shift);
    if badness > 0.0 { badness / by } else { badness * by }
}

#[cfg(test)]
mod tests {
    use super::{Call, Hints, badness, big_speedup, overall};
    use crate::heuristics::{INLINE_GROWTH_SQUARING_BOUND, INLINE_HINT_PERCENT};

    fn call(growth: i64) -> Call {
        Call {
            growth,
            saved: 4.0,
            frequency: Some(1.0),
            depth: 0,
            overall: growth * 2,
            caller: 40,
            hints: Hints::default(),
        }
    }

    #[test]
    fn the_two_pieces_of_the_overall_growth_term_meet_at_the_bound() {
        let bound = i64::from(INLINE_GROWTH_SQUARING_BOUND);
        assert_eq!(overall(bound - 1), ((bound - 1) * (bound - 1)) as f64);
        assert_eq!(overall(bound), (bound * bound) as f64);
        assert_eq!(overall(bound + 1), (bound * bound + 1) as f64);
    }

    #[test]
    fn a_call_that_shrinks_the_program_comes_before_any_that_grows_it() {
        let mut hot = call(1);
        hot.saved = 1000.0;
        hot.frequency = Some(100.0);
        assert!(badness(&call(0)) < badness(&hot));
        assert!(badness(&call(-3)) < badness(&call(0)));
    }

    #[test]
    fn a_call_in_a_loop_comes_before_the_same_call_outside_one() {
        let mut inside = call(10);
        inside.frequency = Some(10.0);
        assert!(badness(&inside) < badness(&call(10)));
    }

    #[test]
    fn a_frequency_is_believed_only_up_to_the_clamp() {
        let mut guessed = call(10);
        guessed.frequency = Some(100.0);
        let mut nested = call(10);
        nested.frequency = Some(10_000.0);
        assert_eq!(badness(&guessed), badness(&nested));
    }

    #[test]
    fn a_callee_with_one_caller_comes_before_one_with_many() {
        let mut one = call(10);
        one.overall = 10;
        let mut many = call(10);
        many.overall = 500;
        assert!(badness(&one) < badness(&many));
    }

    #[test]
    fn with_no_frequency_a_loop_halves_the_growth() {
        let mut outside = call(16);
        outside.frequency = None;
        let mut inside = outside;
        inside.depth = 2;
        assert_eq!(badness(&outside), 16.0);
        assert_eq!(badness(&inside), 16.0 / 4.0);
    }

    #[test]
    fn each_hint_moves_a_call_toward_the_front() {
        let plain = badness(&call(10));
        for hints in [
            Hints { enables: true, ..Hints::default() },
            Hints { asks: true, ..Hints::default() },
            Hints { declared: true, ..Hints::default() },
        ] {
            assert!(badness(&Call { hints, ..call(10) }) < plain, "{hints:?}");
        }
    }

    #[test]
    fn a_hint_raises_a_limit_as_gcc_raises_it() {
        let none = Hints::default();
        let one = Hints { enables: true, ..none };
        let both = Hints { enables: true, asks: true, ..none };
        assert_eq!(none.limit(15, INLINE_HINT_PERCENT), 15);
        assert_eq!(one.limit(15, INLINE_HINT_PERCENT), 30);
        assert_eq!(both.limit(15, INLINE_HINT_PERCENT), 6000);
        // At `-O3` the square is 360000, under the million it is held to.
        assert_eq!(both.limit(70, 600), 252_000);
    }

    #[test]
    fn a_big_speedup_is_measured_against_the_caller_too() {
        // A body of forty that folds to ten saves most of what a small caller does.
        assert!(big_speedup(40.0, 10.0, 3.0, 1.0, 10.0, 30));
        // The same in a caller that takes a thousand saves too little of it.
        assert!(!big_speedup(40.0, 10.0, 3.0, 1.0, 1000.0, 30));
    }
}
