//! What a pass knows about the machine it is compiling for.
//!
//! Design: section 40.12 of `spec/optimizer/40-cost-models.md`, which writes down the interface a
//! pass asks a target through, and tamnd/rucc#655, which is the observation that no pass could
//! reach it. The cost tables have existed since the crate did and nothing outside `rucc-cost` ever
//! called `for_arch`, because a pass is handed a function and a function does not know what it is
//! being compiled for.
//!
//! # Why it sits on the analysis cache
//!
//! [`crate::Analyses`] is the one thing every pass is already handed besides the function and the
//! fuel, so putting the machine there is what makes it reachable without a fourth argument on
//! every `run`. The cache is per function and so is the machine's lifetime as far as a pass is
//! concerned, and the pipeline builds one for the module and hands out copies.
//!
//! It is a copy rather than a borrow because it is two words, a pointer to a table nobody writes
//! and the goal. A pass that wants both the machine and an analysis out of the same cache would
//! otherwise be holding two borrows of it, one of them mutable, which is a fight with the borrow
//! checker over a value cheaper to copy than to reference.
//!
//! # A target with no back end
//!
//! `rucc_cost::for_arch` answers `None` for AArch64 and RISC-V, because neither has a back end and
//! neither has a cost table, and a default table would be numbers nobody chose that every pass
//! would believe. So [`Machine::costs`] is an `Option` and a pass that needs a number has to say
//! what it does without one. What it should do is nothing, and say so as a missed remark: an
//! optimization that guessed at the cost model would be a pass tuned for a machine that is not the
//! one being compiled for.

use rucc_cost::{CostTable, Cycles, Goal, RegClass, TargetCosts, TuneFlag};
use rucc_ir::Module;
use rucc_session::OptLevel;

/// The target a pass is compiling for, and which of its two cost tables applies.
#[derive(Clone, Copy)]
pub struct Machine {
    costs: Option<&'static dyn TargetCosts>,
    goal: Goal,
}

impl Machine {
    /// The machine a module is being compiled for at that level.
    ///
    /// Both halves come from things the pipeline already has, which is why no flag was added for
    /// this. The architecture is on the module, because a module is built for a target and says
    /// so, and the goal is whether the level optimizes for size, which is one call on the level.
    #[must_use]
    pub fn of(module: &Module, level: OptLevel) -> Self {
        Self::with(rucc_cost::for_tuple(module.tuple), Goal::for_size(level.is_size()))
    }

    /// The machine for costs already in hand.
    ///
    /// What [`Machine::of`] is written in terms of, and what a caller that resolved a target some
    /// other way uses. `None` is a target with no cost table, which is every architecture rucc has
    /// no back end for.
    #[must_use]
    pub const fn with(costs: Option<&'static dyn TargetCosts>, goal: Goal) -> Self {
        Self { costs, goal }
    }

    /// A machine nobody has a cost table for, which is what a test that does not care wants.
    ///
    /// Named for what it is rather than called `default`, because a default machine is the thing
    /// this module exists to avoid: a pass that got one silently would be optimizing for a target
    /// that does not exist.
    #[must_use]
    pub const fn unknown() -> Self {
        Self { costs: None, goal: Goal::Speed }
    }

    /// The costs for this target, or nothing for one with no table.
    #[must_use]
    pub fn costs(self) -> Option<&'static dyn TargetCosts> {
        self.costs
    }

    /// Which table applies, which is the goal the level asked for.
    #[must_use]
    pub const fn goal(self) -> Goal {
        self.goal
    }

    /// The cost table for this target and goal, or nothing for a target with no table.
    #[must_use]
    pub fn table(self) -> Option<&'static CostTable> {
        self.costs.map(|costs| costs.table(self.goal))
    }

    /// What this target answers for a tuning flag, and the flag's documented default without one.
    ///
    /// A default is right here and wrong for a number, which is the asymmetry `Tuning::untuned`
    /// already records: a tuning flag is a question whose safe answer is written down, and a cost
    /// is a measurement of a machine.
    #[must_use]
    pub fn tune(self, flag: TuneFlag) -> bool {
        self.costs.is_some_and(|costs| costs.tune(flag))
    }

    /// How many registers of that bank the allocator hands out, or nothing without a target.
    #[must_use]
    pub fn allocatable(self, class: RegClass) -> Option<u32> {
        self.costs.map(|costs| costs.allocatable(class))
    }

    /// What a branch of that predictability costs, or nothing without a target.
    #[must_use]
    pub fn branch_cost(self, predictable: bool) -> Option<Cycles> {
        self.costs.map(|costs| costs.branch_cost(self.goal, predictable))
    }

    /// What the machine is called in a dump, and `unknown` for a target with no table.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.costs.map_or("unknown", TargetCosts::name)
    }
}

impl std::fmt::Debug for Machine {
    /// The name and the goal, because a `dyn TargetCosts` has no `Debug` and the pointer would say
    /// nothing to anybody reading a dump.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Machine({}, {})", self.name(), self.goal)
    }
}

/// Test helpers, which are here rather than in `crate::testing` because that module is also read
/// by two integration tests through a `#[path]` include and so cannot name anything in the crate.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::Machine;
    use crate::analysis::Analyses;
    use rucc_cost::Goal;

    /// An empty analysis cache for the target the tests are written against.
    ///
    /// x86-64, because it is the only one with a cost table and a test that reads a cost wants a
    /// real number rather than a `None` that makes the pass under test do nothing. A test that
    /// means to check what a pass does without a target says so with `Machine::unknown` at the
    /// call site.
    pub(crate) fn analyses() -> Analyses {
        Analyses::new(machine())
    }

    /// The machine the tests are written against, which is x86-64 optimizing for speed.
    pub(crate) fn machine() -> Machine {
        Machine::with(rucc_cost::for_arch(rucc_target::Arch::X86_64), Goal::Speed)
    }
}

#[cfg(test)]
mod tests {
    use super::Machine;
    use rucc_cost::{Goal, RegClass};

    #[test]
    fn a_machine_nobody_has_a_table_for_answers_nothing_rather_than_a_number() {
        let machine = Machine::unknown();
        assert!(machine.costs().is_none());
        assert!(machine.table().is_none());
        assert!(machine.allocatable(RegClass::Integer).is_none());
        assert!(machine.branch_cost(false).is_none());
        assert_eq!(machine.name(), "unknown");
    }

    #[test]
    fn the_two_banks_of_x86_64_are_not_the_same_size() {
        // The general purpose bank gives up the stack pointer and the frame pointer and the vector
        // bank gives up neither, so a loop holding only floating point values has two more
        // registers of room than one holding integers. The pass that asked before this existed
        // read one constant for both and got the smaller.
        let machine = Machine::with(rucc_cost::for_arch(rucc_target::Arch::X86_64), Goal::Speed);
        assert_eq!(machine.allocatable(RegClass::Integer), Some(12));
        assert_eq!(machine.allocatable(RegClass::Float), Some(14));
    }
}
