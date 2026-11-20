//! The seven phases `-O2` time is divided into, and the share of it each one is allowed.
//!
//! Design: `spec/optimizer/42-measurement.md` section 42.7, and tamnd/rucc#2972. Spec 16.4 holds
//! `-O2` to one and a half times `clang -O2`, and section 42.7 divides that between seven phases so
//! that a pass that takes a phase over its share is a finding with a name, rather than a slower
//! compiler nobody can explain.
//!
//! Every optimizer pass says which phase it is in with [`crate::Pass::phase`], next to its name and
//! its description, and a pass over the whole module says it with a `PHASE` next to its `NAME`. The
//! front end and the back end are not passes, and the driver puts their time in [`Phase::Front`],
//! [`Phase::Selection`], [`Phase::Allocation`] and [`Phase::Machine`] itself.
//!
//! # What counts as an analysis
//!
//! What [`crate::Analyses`] builds: the graph, the two dominator trees, the loops, the frontiers,
//! control dependence, the frequencies, liveness and pressure, and memory SSA when the redundant
//! load pass puts it in. The time a pass spends waiting for one of those to be built is taken out
//! of the pass and counted under [`Phase::Analyses`]. The alias oracle, the range query and SCEV
//! answer one question at a time as a pass asks it, and that time stays with the pass that asked,
//! the same as the memory SSA walk does (see `crate::memssa`). So the analyses' share is a floor,
//! and a pass that spends its time asking about aliases is over its own phase rather than this one.

use std::time::Duration;

/// One of the seven rows of the table in section 42.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// Reading, preprocessing, parsing, checking, and lowering to the IR.
    Front,
    /// The analyses the passes ask for, built before the question is answered.
    Analyses,
    /// The e-graph and the rewrite rules.
    Rewriting,
    /// The rest of the scalar and loop pipeline, the passes over the whole module included.
    Scalar,
    /// Lowering the IR to what the machine has rules for, and choosing the instructions.
    Selection,
    /// Register allocation.
    Allocation,
    /// Everything in the back end after selection that is not allocation, and writing the output.
    Machine,
}

impl Phase {
    /// All seven, in the order section 42.7 lists them.
    pub const ALL: [Phase; 7] = [
        Phase::Front,
        Phase::Analyses,
        Phase::Rewriting,
        Phase::Scalar,
        Phase::Selection,
        Phase::Allocation,
        Phase::Machine,
    ];

    /// What `-frucc-trace` and `cargo xtask budget` call it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Phase::Front => "front",
            Phase::Analyses => "analyses",
            Phase::Rewriting => "rewriting",
            Phase::Scalar => "scalar",
            Phase::Selection => "selection",
            Phase::Allocation => "allocation",
            Phase::Machine => "machine",
        }
    }

    /// The phase with that name, the other way round from [`Phase::name`].
    #[must_use]
    pub fn named(name: &str) -> Option<Phase> {
        Phase::ALL.into_iter().find(|phase| phase.name() == name)
    }

    /// Its row of the table in section 42.7, in the words the table uses.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Phase::Front => "Front end and semantic analysis",
            Phase::Analyses => "Analyses (alias, memory SSA, ranges, loops, SCEV)",
            Phase::Rewriting => "The e-graph and rewriting",
            Phase::Scalar => "The remaining scalar and loop pipeline",
            Phase::Selection => "Selection and lowering",
            Phase::Allocation => "Register allocation",
            Phase::Machine => "Scheduling, layout, machine-level passes",
        }
    }

    /// Its share of `-O2` time in percent, from the table in section 42.7.
    ///
    /// These are targets and not predictions. A phase over its share is a finding to be explained,
    /// which is what `cargo xtask budget` names the passes for.
    #[must_use]
    pub const fn share(self) -> u32 {
        match self {
            Phase::Front => 25,
            Phase::Analyses | Phase::Rewriting => 15,
            Phase::Scalar => 20,
            Phase::Selection => 8,
            Phase::Allocation => 12,
            Phase::Machine => 5,
        }
    }
}

/// Time added up by phase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Budget {
    spent: [Duration; 7],
}

impl Budget {
    /// Counts some time under one phase.
    pub fn add(&mut self, phase: Phase, time: Duration) {
        self.spent[phase as usize] += time;
    }

    /// Everything another one counted, counted here as well.
    pub fn merge(&mut self, other: &Budget) {
        for phase in Phase::ALL {
            self.add(phase, other.spent(phase));
        }
    }

    /// The time counted under one phase.
    #[must_use]
    pub fn spent(&self, phase: Phase) -> Duration {
        self.spent[phase as usize]
    }

    /// The time counted under all seven.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.spent.iter().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shares_are_the_whole_of_the_time() {
        assert_eq!(Phase::ALL.iter().map(|phase| phase.share()).sum::<u32>(), 100);
    }

    #[test]
    fn every_phase_is_found_again_by_its_name() {
        for phase in Phase::ALL {
            assert_eq!(Phase::named(phase.name()), Some(phase));
        }
        assert_eq!(Phase::named("optimize"), None);
    }

    #[test]
    fn a_budget_adds_up_by_phase_and_in_total() {
        let mut budget = Budget::default();
        budget.add(Phase::Scalar, Duration::from_millis(3));
        budget.add(Phase::Scalar, Duration::from_millis(2));
        budget.add(Phase::Front, Duration::from_millis(1));
        let mut more = Budget::default();
        more.add(Phase::Front, Duration::from_millis(4));
        budget.merge(&more);
        assert_eq!(budget.spent(Phase::Scalar), Duration::from_millis(5));
        assert_eq!(budget.spent(Phase::Front), Duration::from_millis(5));
        assert_eq!(budget.spent(Phase::Machine), Duration::ZERO);
        assert_eq!(budget.total(), Duration::from_millis(10));
    }
}
