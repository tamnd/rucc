//! What things cost on a target, and every tuning constant the optimizer has.
//!
//! This is document 40 of `spec/optimizer/`, made real. The crate exists because of the failure
//! that document opens with: a compiler where each pass has its own idea of what an operation
//! costs is a compiler where two passes undo each other, and neither of them is wrong. Putting the
//! numbers in one place does not make them right, but it makes them one thing that can be measured
//! and changed, instead of thirty things that have to be found first.
//!
//! # What is in here
//!
//! [`Cycles`] and [`Bytes`], which are separate types so that a cost in time is never compared
//! against a threshold in space. [`Cost`], which is a time and a complexity compared
//! lexicographically with an explicit infinity, from GCC's `comp_cost`. [`CostTable`], which a
//! target fills in completely or not at all. [`TuneFlag`], which is the half of a cost model that
//! is a boolean rather than a number. And [`heuristics`], which is the file every threshold in
//! every pass has to come from.
//!
//! # Two tables, not one table and a policy
//!
//! Section 40.3 reads `ix86_cur_cost()` at `gcc/config/i386/i386.h:269` and takes the design
//! from it: optimizing for size is a different cost table, not a weighting applied to the same
//! one. `-Os` selects the second table and every pass then goes on asking the same questions in
//! the same way. It makes `-Os` behaviour inspectable as data, and it means no pass has to
//! remember to ask whether it is optimizing for size, which is the sort of thing a pass forgets
//! in exactly one of its five decisions.
//!
//! # What is not in here
//!
//! Anything derived from a function. Register pressure, block frequency and branch predictability
//! are all things section 40.6 wants computed once per function and shared, and all three need the
//! IR, so they belong with the analyses rather than with the target description. This crate is
//! below the IR on purpose.

#![doc(html_root_url = "https://docs.rs/rucc-cost/0.10.19")]

pub mod cost;
pub mod cycles;
pub mod heuristics;
pub mod table;
pub mod tune;
pub mod x86_64;

pub use cost::{Complexity, Cost};
pub use cycles::{Bytes, Cycles};
pub use table::{AddrMode, Builder, CostTable, Width};
pub use tune::{TuneFlag, Tuning};

use rucc_target::Arch;
use rucc_tuple::TargetTuple;

/// Which of a target's two tables is wanted.
///
/// A named type rather than a bare `bool`, because `table(true)` at a call site is a coin toss for
/// the reader and `table(Goal::Size)` is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Goal {
    /// Make it fast. `-O1`, `-O2`, `-O3`.
    Speed,
    /// Make it small. `-Os` and `-Oz`.
    Size,
}

impl Goal {
    /// The goal for a level that has already been asked whether it optimizes for size.
    ///
    /// Takes the answer rather than the level, because the level lives in `rucc-session` and this
    /// crate sits below it. That is not a workaround for the layer rule, it is the layer rule
    /// working: what an instruction costs on a machine has nothing to do with how the driver was
    /// invoked, and a dependency the other way would say it did. The caller writes
    /// `Goal::for_size(level.is_size())`, which is one line and reads correctly.
    #[must_use]
    pub const fn for_size(size: bool) -> Self {
        if size { Self::Size } else { Self::Speed }
    }

    /// The goal as it appears in a dump.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Speed => "speed",
            Self::Size => "size",
        }
    }
}

impl std::fmt::Display for Goal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which bank of registers a value needs.
///
/// Two, which is the split every target rucc has a back end for or plans one for. A vector lands
/// in the floating point bank because on x86-64 the same registers hold both, and a target where
/// that is wrong is a target that needs a third variant here rather than a different rule.
///
/// It is in this crate rather than with the pressure analysis that counts them because the target
/// is what says how many of each it hands out, and this crate is where a target is described. The
/// question of which bank holds a given IR type is the other half and it stays with the analysis,
/// since this crate sits below the IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegClass {
    /// Integers, pointers and capabilities, which the general purpose registers hold.
    Integer,
    /// Floating point and vectors, which on every target rucc targets share a bank.
    Float,
}

impl RegClass {
    /// Both of them, for a caller that reports each.
    pub const ALL: [Self; 2] = [Self::Integer, Self::Float];

    /// How many there are, for the arrays keyed by one.
    pub const COUNT: usize = Self::ALL.len();

    /// Where this bank sits in a class-indexed array.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// How it reads in a dump.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Float => "float",
        }
    }
}

impl std::fmt::Display for RegClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a pass asks a target about costs, per section 40.12.
///
/// Three kinds of answer: a number that comes from one of the two tables, a boolean that does not
/// depend on the goal at all, and a number that does not either. Whether a microarchitecture
/// prefers an `lea` to an `add` is not a different fact when optimizing for size, and neither is
/// how many registers the allocator hands out.
pub trait TargetCosts: Send + Sync {
    /// The table for this goal.
    fn table(&self, goal: Goal) -> &CostTable;

    /// What this target answers for a tuning flag.
    fn tune(&self, flag: TuneFlag) -> bool;

    /// How many registers of that bank the allocator will actually hand out, per section 40.6.
    ///
    /// Not how many the machine has. The number a pass wants is how many a value may be put in
    /// after the ones with a job of their own are taken out, because a pass that hoists up to the
    /// architectural count has hoisted into registers that were never available and the spill it
    /// caused lands inside the loop it was trying to help.
    ///
    /// There is no default. A target that has not answered this has not been asked how its
    /// register file is spent, and a default here would be a number nobody chose that every pass
    /// consulting it would believe.
    fn allocatable(&self, class: RegClass) -> u32;

    /// What the target is called in a dump.
    fn name(&self) -> &'static str;

    /// What an unpredictable branch costs at this goal, per section 40.5.
    ///
    /// Provided rather than left to each pass, because `BRANCH_COST` at
    /// `gcc/config/i386/i386.h:2023` is three cases in one line and getting one of them wrong is
    /// how a well predicted branch ends up if-converted.
    ///
    /// Two of the three cases read the table. Optimizing for speed, a predictable branch is free
    /// and an unpredictable one costs whatever the target says; optimizing for size, a branch is
    /// the same number of bytes either way, because the branch predictor does not shorten the
    /// encoding. The one case that does not read the table is the free one, and it does not
    /// because zero is a claim about hardware rather than about this machine.
    fn branch_cost(&self, goal: Goal, predictable: bool) -> Cycles {
        if goal == Goal::Speed && predictable {
            return heuristics::BRANCH_COST_PREDICTABLE;
        }
        self.table(goal).branch_cost
    }
}

/// The costs for a target, or nothing for one nobody has written a table for.
///
/// x86-64 is the only answer today, because it is the only back end rucc has. The function exists
/// anyway so that the second target is a file and a match arm rather than a redesign.
#[must_use]
pub fn for_arch(arch: Arch) -> Option<&'static dyn TargetCosts> {
    match arch {
        Arch::X86_64 => Some(x86_64::COSTS),
        _ => None,
    }
}

/// The costs for the target a tuple names, which is what a caller holding a module asks.
///
/// Two enumerations name the same three machines. `rucc_tuple::Arch` is what a target tuple
/// carries and it can spell architectures rucc has no back end for, and [`Arch`] is what the cost
/// tables are keyed by because it is the list of architectures a back end exists for. So the
/// mapping is partial, and everything it does not name answers the same as an architecture with a
/// name and no table.
///
/// It is here rather than at the caller because there is one right mapping and a second copy of it
/// somewhere else would be a place for the two to disagree.
#[must_use]
pub fn for_tuple(tuple: TargetTuple) -> Option<&'static dyn TargetCosts> {
    let arch = match tuple.arch() {
        rucc_tuple::Arch::X86_64 => Arch::X86_64,
        rucc_tuple::Arch::Aarch64 => Arch::Aarch64,
        rucc_tuple::Arch::Riscv64 => Arch::Riscv64,
        _ => return None,
    };
    for_arch(arch)
}

#[cfg(test)]
mod tests {
    use super::{Goal, TuneFlag, for_arch};
    use rucc_target::Arch;

    #[test]
    fn the_only_backend_has_costs() {
        let costs = for_arch(Arch::X86_64).expect("x86-64 is the back end rucc has");
        assert_eq!(costs.name(), "x86-64");
        assert!(!costs.table(Goal::Speed).add.is_infinite());
    }

    #[test]
    fn a_target_with_no_back_end_has_no_costs_rather_than_made_up_ones() {
        // The alternative would be a default table, and a default table is a set of numbers
        // nobody chose that every pass would believe.
        assert!(for_arch(Arch::Aarch64).is_none());
    }

    #[test]
    fn a_tuning_flag_does_not_depend_on_the_goal() {
        // There is nothing to assert against here except the shape of the interface: `tune` takes
        // no goal, so it cannot answer differently for `-Os`. The test is here to fail if
        // somebody adds one.
        let costs = for_arch(Arch::X86_64).unwrap();
        for flag in TuneFlag::ALL {
            let _: bool = costs.tune(flag);
        }
    }
}
