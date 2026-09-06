//! How likely an edge is taken, how often a block runs, and how much either is worth believing.
//!
//! Design: `spec/optimizer/11-profile-and-frequency.md`.
//!
//! Almost every cost decision in the optimizer is a form of the question "is this code hot", and
//! this is where the answer comes from. Inlining, unrolling, block layout, spill placement and
//! if-conversion all read block frequency, so a frequency that is wrong makes all of them wrong
//! together in a way that is very hard to attribute to anything.
//!
//! # A number is not enough
//!
//! Section 11.1 calls the provenance field the best idea in GCC's profile machinery, and this
//! module is built around it. A [`Probability`] and a [`Frequency`] each carry a [`Quality`]
//! saying where the number came from, arithmetic on them degrades the quality to the worse of the
//! two inputs, and there is no way to build either one without saying what its quality is.
//!
//! The reason is what happens without it. A compilation with measured data for half a program and
//! guesses for the other half computes with both constantly, and one guess laundered through three
//! arithmetic operations comes out indistinguishable from a measurement. The inliner then makes an
//! aggressive decision on a fabricated number, and nothing anywhere says so. With the quality on
//! the value, a consumer that should behave differently on a guess can ask, and one that forgot to
//! ask is at least reading a number whose history is still attached to it.
//!
//! The same rule runs in the other direction, which is section 11.6: a static predictor may only
//! write a probability whose quality is [`Quality::Guessed`], and only where what is already there
//! is worth less. A heuristic that overrides a measurement is a heuristic that is wrong by
//! construction, because the measurement is the thing the heuristic is trying to approximate.
//!
//! # Fixed point, and saturating
//!
//! Both types are scaled integers rather than floats. Section 11.3 asks for that and
//! `spec/03-architecture.md`'s determinism rule is why: a float result depends on evaluation order
//! and on the host's excess precision, frequencies feed cost comparisons, and cost comparisons
//! decide what code comes out. A frequency that differs in the last bit between two hosts is a
//! reproducibility failure rather than a rounding question.
//!
//! Nested loops multiply, so a frequency deep in a loop nest grows fast. Every operation here
//! saturates, and it saturates in the type rather than at the call sites, which is section 11.6's
//! second failure mode: a saturation that each caller is responsible for is a saturation that one
//! caller forgets.

use std::fmt;

use rucc_cost::heuristics::HOT_BLOCK_FRACTION;

/// How much a probability or a frequency is worth believing.
///
/// The order is the point, and it is GCC's order out of `enum profile_quality` in
/// `gcc/profile-count.h`: worse first, so the quality of a computed value is the smaller of what
/// went into it and `Ord` says so without a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Quality {
    /// Nobody has said anything about this one.
    ///
    /// What a block a pass created gets until the pass says otherwise. It is not zero and it is
    /// not one, it is the absence of a claim, and a consumer that treats it as either is the
    /// third failure mode in section 11.6.
    Unknown,

    /// A static predictor said so, from the shape of the code and nothing else.
    ///
    /// Everything in M4 is this, because there is no profile data yet. A guess is still much
    /// better than nothing: the hit rates in section 11.2 are measurements of how people write
    /// programs, and those have held up for thirty years.
    Guessed,

    /// It came from a measurement, and then a transformation scaled it.
    ///
    /// Splitting a block, unrolling a loop or threading a jump all divide a measured count across
    /// paths that were not measured separately. The result is worth more than a guess and less
    /// than what was measured, which is exactly what this says.
    Adjusted,

    /// Measured, and nothing has touched it since.
    ///
    /// Also what a branch on a constant gets, because that one is not a measurement or a guess. It
    /// is arithmetic.
    Precise,
}

impl Quality {
    /// How the quality reads in a dump.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Guessed => "guessed",
            Self::Adjusted => "adjusted",
            Self::Precise => "precise",
        }
    }

    /// Whether this came from running the program rather than from looking at it.
    ///
    /// The question a consumer asks when it is about to do something it could not undo. Section
    /// 40.5's rule that a statically predicted branch never counts as predictable is this
    /// predicate, and it is here rather than at each call site so that the rule is one thing.
    #[must_use]
    pub const fn is_measured(self) -> bool {
        matches!(self, Self::Adjusted | Self::Precise)
    }
}

impl fmt::Display for Quality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How likely one edge out of a block is taken, and how much that is worth believing.
///
/// Held as parts of [`Probability::SCALE`], which is ten thousand, so a hit rate written as a
/// whole percent is exact and one written to two decimal places is too. GCC's `REG_BR_PROB_BASE`
/// is the same idea at the same size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Probability {
    parts: u32,
    quality: Quality,
}

impl Probability {
    /// What a probability is out of.
    pub const SCALE: u32 = 10_000;

    /// A probability of `parts` out of [`Probability::SCALE`], believed this much.
    ///
    /// There is no constructor that does not say where the number came from, which is section
    /// 11.6's second failure mode closed off at the type. More than the scale is not a
    /// probability, and it is clamped rather than refused, because the callers that can produce
    /// one are all doing arithmetic where the answer is certainty.
    #[must_use]
    pub const fn new(parts: u32, quality: Quality) -> Self {
        let parts = if parts > Self::SCALE { Self::SCALE } else { parts };
        Self { parts, quality }
    }

    /// A hit rate written as a whole percentage, which is how section 11.2 writes all of them.
    #[must_use]
    pub const fn percent(percent: u32, quality: Quality) -> Self {
        Self::new(percent.saturating_mul(Self::SCALE / 100), quality)
    }

    /// The edge is always taken, and that is arithmetic rather than a guess.
    #[must_use]
    pub const fn always() -> Self {
        Self { parts: Self::SCALE, quality: Quality::Precise }
    }

    /// The edge is never taken.
    #[must_use]
    pub const fn never() -> Self {
        Self { parts: 0, quality: Quality::Precise }
    }

    /// Nothing is known about this edge, so it is even and says so.
    ///
    /// The starting point for a two way branch no predictor matched. Even and guessed is a
    /// different statement from even and measured, and the second one is a real fact about a
    /// branch that is genuinely unpredictable.
    #[must_use]
    pub const fn even() -> Self {
        Self { parts: Self::SCALE / 2, quality: Quality::Guessed }
    }

    /// The parts out of [`Probability::SCALE`].
    #[must_use]
    pub const fn parts(self) -> u32 {
        self.parts
    }

    /// How much this is worth believing.
    #[must_use]
    pub const fn quality(self) -> Quality {
        self.quality
    }

    /// The other edge out of the same branch.
    #[must_use]
    pub const fn complement(self) -> Self {
        Self { parts: Self::SCALE - self.parts, quality: self.quality }
    }

    /// Both, for an edge reached by taking this one and then that one.
    ///
    /// The quality is the worse of the two, which is the whole reason these are not bare numbers.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        let parts = u64::from(self.parts) * u64::from(other.parts) / u64::from(Self::SCALE);
        // The product of two things at most the scale is at most the scale, so the cast is exact.
        Self { parts: parts as u32, quality: self.quality.min(other.quality) }
    }

    /// Whether a branch this likely one way is one a machine will predict correctly.
    ///
    /// Section 40.5, and the part of it that matters is not the threshold. A probability a static
    /// predictor guessed never counts as predictable however extreme it is, because the branch
    /// predictor in the machine is looking at what the program does and the predictor here is
    /// looking at what the program says. Guessing that a loop exit is not taken 89 times in 100 is
    /// not evidence about any particular branch.
    #[must_use]
    pub fn is_predictable(self) -> bool {
        if !self.quality.is_measured() {
            return false;
        }
        let margin = rucc_cost::heuristics::PREDICTABLE_BRANCH_PERCENT * (Self::SCALE / 100);
        self.parts <= margin || self.parts >= Self::SCALE - margin
    }
}

impl fmt::Display for Probability {
    /// As a percentage, with the two decimal places only when they say something.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.parts / (Self::SCALE / 100);
        let rest = self.parts % (Self::SCALE / 100);
        if rest == 0 { write!(f, "{whole}%") } else { write!(f, "{whole}.{rest:02}%") }
    }
}

/// How often a block runs, relative to one entry to the function it is in.
///
/// The entry block is [`Frequency::ENTRY`], which is one. A block inside a loop predicted to run
/// ten times is ten. A block on an error path is a fraction. The unit is deliberately relative:
/// how often this block runs compared to the whole function is a question that can be answered
/// without a profile, and how often it runs compared to the rest of the program cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Frequency {
    /// Scaled by [`Probability::SCALE`], so that a frequency and a probability are the same
    /// fixed point and multiplying one by the other is exact.
    scaled: u64,
    quality: Quality,
}

impl Frequency {
    /// One execution per entry to the function, which is what the entry block gets.
    ///
    /// Precise, because it is not a claim about the program. It is the definition of the unit.
    pub const ENTRY: Self = Self { scaled: Probability::SCALE as u64, quality: Quality::Precise };

    /// The block does not run at all.
    pub const NEVER: Self = Self { scaled: 0, quality: Quality::Precise };

    /// Nobody has computed one for this block yet.
    ///
    /// Zero, so that a consumer that ignores the quality is at least conservative rather than
    /// wrong in the direction that puts cold code in the hot section. The quality is what says the
    /// zero means nothing.
    pub const UNKNOWN: Self = Self { scaled: 0, quality: Quality::Unknown };

    /// As high as this goes, which is what everything saturates to.
    ///
    /// A frequency here means the arithmetic ran out of room, which nested loops will do to any
    /// fixed size number. What it must not do is wrap, because a hot block that comes out cold is
    /// a decision nobody can explain afterwards.
    pub const MAX: Self = Self { scaled: u64::MAX, quality: Quality::Guessed };

    /// Runs this many times per entry to the function, believed this much.
    #[must_use]
    pub const fn times(count: u32, quality: Quality) -> Self {
        Self { scaled: (count as u64).saturating_mul(Probability::SCALE as u64), quality }
    }

    /// The raw fixed point value, scaled by [`Probability::SCALE`].
    ///
    /// For a dump or a comparison. A consumer doing arithmetic on this rather than on the
    /// [`Frequency`] is a consumer that has dropped the quality on the floor.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.scaled
    }

    /// How much this is worth believing.
    #[must_use]
    pub const fn quality(self) -> Quality {
        self.quality
    }

    /// Whether the arithmetic ran out of room getting here.
    #[must_use]
    pub const fn is_saturated(self) -> bool {
        self.scaled == u64::MAX
    }

    /// This block's frequency carried along an edge taken this often.
    #[must_use]
    pub fn along(self, edge: Probability) -> Self {
        let scaled =
            (u128::from(self.scaled) * u128::from(edge.parts())) / u128::from(Probability::SCALE);
        Self {
            scaled: u64::try_from(scaled).unwrap_or(u64::MAX),
            quality: self.quality.min(edge.quality()),
        }
    }

    /// Two paths into the same block.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self {
            scaled: self.scaled.saturating_add(other.scaled),
            quality: self.quality.min(other.quality),
        }
    }

    /// This block, once per iteration of a loop that runs `iterations` times.
    ///
    /// The caller clamps the iteration count before it gets here, per section 11.2. Saturating
    /// multiplication keeps the arithmetic honest, but a nest of loops each claimed to run four
    /// billion times has already lost the argument somewhere further up.
    #[must_use]
    pub fn repeated(self, iterations: u32) -> Self {
        Self {
            scaled: self.scaled.saturating_mul(u64::from(iterations)),
            quality: self.quality.min(Quality::Guessed),
        }
    }

    /// Whether this block is hot compared with the rest of the function it is in.
    ///
    /// Section 11.4, and GCC's `hot-bb-frequency-fraction`: at least one part in
    /// [`HOT_BLOCK_FRACTION`] of the entry block. This is the question the register allocator and
    /// the loop passes are asking, and it is answerable with no profile at all, because it is a
    /// comparison between two blocks that were predicted the same way.
    ///
    /// An entry frequency of zero is not a scale to be hot against, so nothing is hot in a
    /// function that never runs.
    #[must_use]
    pub fn is_hot_in_function(self, entry: Self) -> bool {
        if entry.scaled == 0 {
            return false;
        }
        self.scaled >= entry.scaled.div_ceil(u64::from(HOT_BLOCK_FRACTION))
    }

    /// Whether this block is hot compared with the whole program.
    ///
    /// A different question from [`Frequency::is_hot_in_function`], which is why section 11.4 asks
    /// for two predicates named so they cannot be confused. The section placement decision wants
    /// this one: a block that runs a thousand times per call in a function called twice is hot in
    /// its function and cold in the program.
    ///
    /// It is [`Hotness::Unknown`] today and will be until there is whole program profile data,
    /// which is document 35 and is after M4. The predicate exists now so that every caller is
    /// written against three answers from the start. A boolean that quietly means "hot, or we have
    /// no idea" is how cold code ends up in the hot section, and retrofitting the third answer
    /// into callers written against two is the part that does not happen.
    #[must_use]
    pub const fn is_hot_in_program(self) -> Hotness {
        Hotness::Unknown
    }
}

impl fmt::Display for Frequency {
    /// As a multiple of the entry, to two decimal places, with the quality after it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_saturated() {
            return write!(f, "saturated ({})", self.quality);
        }
        let scale = u64::from(Probability::SCALE);
        let whole = self.scaled / scale;
        let rest = (self.scaled % scale) / (scale / 100);
        write!(f, "{whole}.{rest:02} ({})", self.quality)
    }
}

/// What a hotness question can answer.
///
/// Three answers rather than two, because the third one is real. Section 11.4 is specific that a
/// consumer has to handle it explicitly rather than folding it into either of the others, and the
/// two directions it could be folded are both wrong: treating unknown as hot puts cold code in the
/// hot section, and treating it as cold puts the hot path there instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Hotness {
    /// It runs often enough to spend on.
    Hot,
    /// It does not.
    Cold,
    /// There is no data that answers this, and there is no defensible guess either.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::{Frequency, Hotness, Probability, Quality};

    #[test]
    fn the_qualities_are_ordered_worst_first_so_the_minimum_is_the_degraded_one() {
        assert!(Quality::Unknown < Quality::Guessed);
        assert!(Quality::Guessed < Quality::Adjusted);
        assert!(Quality::Adjusted < Quality::Precise);
        assert!(!Quality::Guessed.is_measured());
        assert!(Quality::Adjusted.is_measured());
    }

    #[test]
    fn a_probability_above_certainty_is_certainty() {
        let over = Probability::new(Probability::SCALE + 1, Quality::Guessed);
        assert_eq!(over.parts(), Probability::SCALE);
        assert_eq!(Probability::percent(200, Quality::Guessed).parts(), Probability::SCALE);
    }

    #[test]
    fn the_two_edges_out_of_a_branch_add_up_to_one() {
        let taken = Probability::percent(89, Quality::Guessed);
        assert_eq!(taken.parts() + taken.complement().parts(), Probability::SCALE);
        assert_eq!(taken.complement().complement(), taken);
        // The complement of a guess is a guess. Knowing an edge is unlikely because a heuristic
        // said the other one was likely is still the heuristic talking.
        assert_eq!(taken.complement().quality(), Quality::Guessed);
    }

    #[test]
    fn a_measurement_combined_with_a_guess_comes_out_a_guess() {
        // This is the whole point of the quality field. Without it the product below is a number
        // that looks exactly like a measurement of a path nobody ever measured.
        let measured = Probability::percent(50, Quality::Precise);
        let guessed = Probability::percent(50, Quality::Guessed);
        let both = measured.and(guessed);
        assert_eq!(both.parts(), Probability::SCALE / 4);
        assert_eq!(both.quality(), Quality::Guessed);
    }

    #[test]
    fn a_statically_predicted_branch_is_never_predictable_however_extreme_it_is() {
        // Section 40.5, and the reason if-conversion has to ask. A predictor saying a loop exit is
        // taken once in a hundred is a statement about loops, not about this branch, and the
        // machine's branch predictor has the actual history.
        assert!(!Probability::percent(99, Quality::Guessed).is_predictable());
        assert!(Probability::percent(99, Quality::Precise).is_predictable());
        assert!(!Probability::percent(90, Quality::Precise).is_predictable());
        assert!(Probability::percent(1, Quality::Adjusted).is_predictable());
    }

    #[test]
    fn a_frequency_carried_along_an_edge_takes_the_worse_of_the_two_qualities() {
        let ten = Frequency::times(10, Quality::Precise);
        let along = ten.along(Probability::percent(30, Quality::Guessed));
        assert_eq!(along.raw(), 3 * u64::from(Probability::SCALE));
        assert_eq!(along.quality(), Quality::Guessed);
    }

    #[test]
    fn the_arithmetic_saturates_rather_than_wrapping() {
        // Nested loops multiply, and a hot block that wraps to cold is a decision nobody can
        // explain afterwards. Every operation has to hold this, not just the one that overflowed
        // in whatever test was written the day it was noticed.
        assert!(Frequency::MAX.plus(Frequency::ENTRY).is_saturated());
        assert!(Frequency::MAX.repeated(2).is_saturated());
        assert!(Frequency::times(u32::MAX, Quality::Guessed).repeated(u32::MAX).is_saturated());
        // Along an edge is the one direction that cannot overflow, since a probability is at most
        // one, and it still must not lose the top of the range on the way through.
        assert_eq!(Frequency::MAX.along(Probability::always()).raw(), u64::MAX);
    }

    #[test]
    fn hot_in_a_function_is_one_part_in_a_thousand_of_the_entry() {
        let entry = Frequency::ENTRY;
        let thousandth = Frequency { scaled: entry.raw() / 1000, quality: Quality::Guessed };
        let less = Frequency { scaled: entry.raw() / 1000 - 1, quality: Quality::Guessed };
        assert!(thousandth.is_hot_in_function(entry));
        assert!(!less.is_hot_in_function(entry));
        assert!(Frequency::times(10, Quality::Guessed).is_hot_in_function(entry));
        assert!(!Frequency::NEVER.is_hot_in_function(entry));
    }

    #[test]
    fn nothing_is_hot_in_a_function_that_never_runs() {
        // Not an arithmetic edge case. A function whose entry frequency is zero is one the caller
        // has already decided is unreachable, and every block in it being hot because zero is a
        // thousandth of zero would put all of it in the hot section.
        assert!(!Frequency::ENTRY.is_hot_in_function(Frequency::NEVER));
    }

    #[test]
    fn hot_in_the_program_says_it_does_not_know_even_about_a_measured_frequency() {
        // Deliberate, and it stays that way until there is whole program data. The trap this
        // guards is somebody answering the question from the only number in reach, which is the
        // frequency within the function, and quietly making the two predicates the same one.
        assert_eq!(Frequency::ENTRY.is_hot_in_program(), Hotness::Unknown);
        assert_eq!(Frequency::times(1000, Quality::Precise).is_hot_in_program(), Hotness::Unknown);
    }

    #[test]
    fn what_a_dump_shows() {
        assert_eq!(Probability::percent(73, Quality::Guessed).to_string(), "73%");
        assert_eq!(Probability::new(7345, Quality::Guessed).to_string(), "73.45%");
        assert_eq!(Probability::always().to_string(), "100%");
        assert_eq!(Frequency::ENTRY.to_string(), "1.00 (precise)");
        assert_eq!(Frequency::UNKNOWN.to_string(), "0.00 (unknown)");
        assert_eq!(Frequency::times(12, Quality::Guessed).to_string(), "12.00 (guessed)");
        assert_eq!(Frequency::MAX.to_string(), "saturated (guessed)");
    }
}
