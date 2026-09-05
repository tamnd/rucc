//! What a pass has to say about what it did, and about what it wanted to do and could not.
//!
//! Section 42.2 of `spec/optimizer/42-measurement.md` counted the instrumented events in GCC and
//! got about a hundred across a compiler with three hundred passes, concentrated in the dozen
//! files somebody had already spent a bad week in. The shape of that number is the problem: a
//! counter you call is a counter you can forget to call, so instrumentation ends up where
//! somebody was already suffering rather than everywhere it is needed.
//!
//! So here a pass does not call a counter. It returns one. [`crate::Pass::run`] hands back a
//! [`Stats`] and there is no other way for a pass to say it changed anything, because
//! [`Stats::changed`] is what the pass manager reads to decide whether to run the verifier and
//! whether the dumps are worth taking. A pass that transforms without recording is a pass whose
//! transformation the manager does not believe happened, which fails the tests in
//! [`crate::pipeline`] rather than quietly working. That is the one structural improvement over
//! GCC that section 42.2 asks for, and it is only available before there are passes to retrofit.
//!
//! # The three kinds
//!
//! `optimized`, `missed` and `note`, which are three of the four keywords GCC's `-fopt-info`
//! takes, and they mean the same things here. The one that matters is `missed`. A pass that only
//! reports its successes cannot be tuned, because the question a person has at a slow loop is not
//! what the compiler did, it is what the compiler nearly did. Every pass in here is expected to
//! have at least one `missed` site, and a pass that has none is a pass that has not been asked
//! the question yet.
//!
//! # Why the text is `&'static str`
//!
//! An event names a site in a pass rather than a fact about a program, so the set of them is
//! fixed at compile time and small. That is what makes the counts addable: two runs over
//! different files produce counts of the same named things, so the corpus can total them across a
//! thousand programs and get a number that means something. A formatted string carrying a
//! variable in it would make every event unique and every total one.

use std::fmt;

/// Which of the three things a remark is.
///
/// The names are the ones `-fopt-info` uses, in lower case, because the person reading the output
/// is usually holding a GCC manual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// The pass rewrote something. This is the only kind that counts as a change.
    Optimized,
    /// The pass found a site it could have rewritten and did not. The reason belongs in the text.
    Missed,
    /// Something worth saying that is neither, such as what an analysis concluded.
    Note,
}

impl Kind {
    /// The word `-fopt-info` spells this with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Optimized => "optimized",
            Self::Missed => "missed",
            Self::Note => "note",
        }
    }

    /// The kind that word names, if it names one.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "optimized" => Some(Self::Optimized),
            "missed" => Some(Self::Missed),
            "note" => Some(Self::Note),
            _ => None,
        }
    }

    /// Every kind, in the order they are printed in.
    pub const ALL: [Self; 3] = [Self::Optimized, Self::Missed, Self::Note];
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One named thing a pass did or did not do, and how many times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// Which of the three it is.
    pub kind: Kind,
    /// The site, in words, as a thing that happened to one instruction or one loop. Read after a
    /// count, as in "3 x instruction folded to a constant", so it is singular and has no number
    /// of its own in it.
    pub what: &'static str,
    /// How many times, which is at least one because an event with a count of zero is not
    /// recorded.
    pub count: u32,
}

/// What one pass has to say about one function.
///
/// Built by the pass as it works, and read by the pass manager afterwards. The events come out in
/// the order they were first recorded rather than sorted, so a pass that records its sites in the
/// order it visits them produces output that reads like the walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// One entry per distinct kind and text, in the order the first of each arrived.
    events: Vec<Event>,
}

impl Stats {
    /// Nothing recorded yet, which is what every pass starts from and what a pass that found
    /// nothing to do ends with.
    #[must_use]
    pub const fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// Records that the pass rewrote something, once.
    ///
    /// Call this at the rewrite and not at the end, because a count kept in a local and written
    /// once is a count that is wrong on every early return.
    pub fn optimized(&mut self, what: &'static str) {
        self.record(Kind::Optimized, what, 1);
    }

    /// Records that the pass could have rewritten something and did not, once.
    pub fn missed(&mut self, what: &'static str) {
        self.record(Kind::Missed, what, 1);
    }

    /// Records something that is neither a rewrite nor a missed one, once.
    pub fn note(&mut self, what: &'static str) {
        self.record(Kind::Note, what, 1);
    }

    /// Adds `count` to the event with this kind and text, creating it if it is new.
    ///
    /// A count of zero does nothing at all, so a pass may add a number it computed without first
    /// checking whether the number is zero and without producing an event that says a thing
    /// happened no times.
    pub fn record(&mut self, kind: Kind, what: &'static str, count: u32) {
        if count == 0 {
            return;
        }
        match self.events.iter_mut().find(|it| it.kind == kind && it.what == what) {
            Some(event) => event.count += count,
            None => self.events.push(Event { kind, what, count }),
        }
    }

    /// Takes everything in `other` into this, keeping the order the two of them are already in.
    ///
    /// What the pass manager does across the functions of a module, so that the total for a pass
    /// is one set of named counts rather than one per function.
    pub fn merge(&mut self, other: &Self) {
        for event in &other.events {
            self.record(event.kind, event.what, event.count);
        }
    }

    /// Whether the pass changed the function.
    ///
    /// One `optimized` event is a change and any number of `missed` and `note` events is not.
    /// This is the whole reason the record is a return value: the pass manager has no other way
    /// to find out, so recording the rewrite is not a thing a pass can leave until later.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.events.iter().any(|event| event.kind == Kind::Optimized)
    }

    /// Whether the pass said anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Everything recorded, in the order it first arrived.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Everything of one kind, in the same order.
    pub fn of(&self, kind: Kind) -> impl Iterator<Item = &Event> {
        self.events.iter().filter(move |event| event.kind == kind)
    }

    /// How many times this exact thing was recorded, which is zero when it never was.
    #[must_use]
    pub fn count(&self, kind: Kind, what: &str) -> u32 {
        self.events
            .iter()
            .find(|it| it.kind == kind && it.what == what)
            .map_or(0, |event| event.count)
    }

    /// How many times anything of this kind was recorded.
    #[must_use]
    pub fn total(&self, kind: Kind) -> u32 {
        self.of(kind).map(|event| event.count).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Stats};

    #[test]
    fn a_pass_that_recorded_nothing_changed_nothing() {
        let stats = Stats::new();
        assert!(!stats.changed());
        assert!(stats.is_empty());
        assert_eq!(stats.total(Kind::Optimized), 0);
    }

    #[test]
    fn only_an_optimized_event_is_a_change() {
        let mut stats = Stats::new();
        stats.missed("nothing to see");
        stats.note("an analysis said something");
        assert!(!stats.changed(), "a miss is not a change");
        assert!(!stats.is_empty(), "it still said something");
        stats.optimized("a rewrite");
        assert!(stats.changed());
    }

    #[test]
    fn the_same_event_twice_is_one_event_with_a_count_of_two() {
        let mut stats = Stats::new();
        stats.optimized("folded");
        stats.optimized("folded");
        stats.optimized("removed");
        assert_eq!(stats.events().len(), 2);
        assert_eq!(stats.count(Kind::Optimized, "folded"), 2);
        assert_eq!(stats.count(Kind::Optimized, "removed"), 1);
        assert_eq!(stats.total(Kind::Optimized), 3);
    }

    #[test]
    fn the_same_words_under_two_kinds_are_two_events() {
        let mut stats = Stats::new();
        stats.optimized("folded");
        stats.missed("folded");
        assert_eq!(stats.events().len(), 2);
        assert_eq!(stats.count(Kind::Optimized, "folded"), 1);
        assert_eq!(stats.count(Kind::Missed, "folded"), 1);
    }

    #[test]
    fn recording_a_count_of_zero_does_not_make_an_event() {
        let mut stats = Stats::new();
        stats.record(Kind::Optimized, "folded", 0);
        assert!(stats.is_empty(), "an event saying a thing happened no times");
        assert!(!stats.changed());
    }

    #[test]
    fn events_come_out_in_the_order_they_first_arrived() {
        let mut stats = Stats::new();
        stats.missed("first");
        stats.optimized("second");
        stats.missed("first");
        let seen: Vec<&str> = stats.events().iter().map(|event| event.what).collect();
        assert_eq!(seen, ["first", "second"], "the second `first` moved it");
    }

    #[test]
    fn merging_adds_the_counts_and_keeps_the_left_hand_order() {
        let mut left = Stats::new();
        left.optimized("folded");
        left.missed("out of fuel");
        let mut right = Stats::new();
        right.optimized("removed");
        right.optimized("folded");
        left.merge(&right);
        let seen: Vec<(&str, u32)> =
            left.events().iter().map(|event| (event.what, event.count)).collect();
        assert_eq!(seen, [("folded", 2), ("out of fuel", 1), ("removed", 1)]);
    }

    #[test]
    fn merging_an_empty_record_changes_nothing() {
        let mut stats = Stats::new();
        stats.optimized("folded");
        let before = stats.clone();
        stats.merge(&Stats::new());
        assert_eq!(stats, before);
    }

    #[test]
    fn the_words_are_the_ones_opt_info_uses_and_they_round_trip() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(Kind::parse("all"), None, "`all` is every kind and not one of them");
        assert_eq!(Kind::parse("Optimized"), None);
    }

    #[test]
    fn one_kind_at_a_time_is_in_the_order_it_was_recorded() {
        let mut stats = Stats::new();
        stats.missed("a");
        stats.optimized("b");
        stats.missed("c");
        let missed: Vec<&str> = stats.of(Kind::Missed).map(|event| event.what).collect();
        assert_eq!(missed, ["a", "c"]);
        assert_eq!(stats.total(Kind::Missed), 2);
    }
}
