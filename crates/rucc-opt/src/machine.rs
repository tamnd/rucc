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
//! It is a copy rather than a borrow because it is a few words, a pointer to a table nobody writes,
//! the goal, and the extensions the module is built for with the target's table of bit counts. A
//! pass that wants both the machine and an analysis out of the same cache would
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
use rucc_ir::{Func, Module, Opcode, Type};
use rucc_session::OptLevel;
use rucc_target::{BitCount, CountInst, Isa, TargetInfo};

/// The target a pass is compiling for, and which of its two cost tables applies.
#[derive(Clone, Copy)]
pub struct Machine {
    costs: Option<&'static dyn TargetCosts>,
    goal: Goal,
    /// The extensions a function without a `target` attribute of its own is built for.
    isa: Isa,
    /// The bit counts the target has one instruction for, which the code generator reads too.
    counts: &'static [CountInst],
    /// Whether a `select` of a pointer or a float is lowered. See `TargetInfo::selects_any`.
    selects_any: bool,
    /// How many bits a pointer has, which is also how wide the index of an address is.
    pointer_bits: u32,
}

impl Machine {
    /// The machine a module is being compiled for at that level.
    ///
    /// Both halves come from things the pipeline already has, which is why no flag was added for
    /// this. The architecture is on the module, because a module is built for a target and says
    /// so, and the goal is whether the level optimizes for size, which is one call on the level.
    #[must_use]
    pub fn of(module: &Module, level: OptLevel) -> Self {
        let target = TargetInfo::for_tuple(module.tuple);
        Self::with(rucc_cost::for_tuple(module.tuple), Goal::for_size(level.is_size()))
            .counting(target.counts)
            .selecting_any(target.selects_any)
            .pointing(target.pointer_width)
    }

    /// The same machine with the extensions the module is built for, which is what `-m` and
    /// `-march=` said.
    ///
    /// A separate call because the module does not carry them, the options do. Without it a
    /// function with no `target` attribute is taken to be built for no extension at all, which is
    /// the answer that makes no count look cheap.
    #[must_use]
    pub const fn built_for(self, isa: Isa) -> Self {
        Self { isa, ..self }
    }

    /// The same machine with that table of bit counts, which is what a test that builds one by
    /// hand wants. [`Machine::of`] takes the target's.
    #[must_use]
    pub const fn counting(self, counts: &'static [CountInst]) -> Self {
        Self { counts, ..self }
    }

    /// The same machine with that answer for whether a `select` of a pointer or a float is
    /// lowered, which is what a test that builds one by hand wants. [`Machine::of`] takes the
    /// target's.
    #[must_use]
    pub const fn selecting_any(self, selects_any: bool) -> Self {
        Self { selects_any, ..self }
    }

    /// The same machine with pointers of that many bits, which is what a test that builds one by
    /// hand wants. [`Machine::of`] takes the target's, and [`Machine::with`] starts at 64.
    #[must_use]
    pub const fn pointing(self, pointer_bits: u32) -> Self {
        Self { pointer_bits, ..self }
    }

    /// How many bits a pointer has on this target.
    ///
    /// An index added to a pointer is as wide as the pointer, so a counter of that width indexes an
    /// address with no extension in front of it. That is 32 bits on wasm32 and 64 on the others.
    #[must_use]
    pub const fn pointer_bits(self) -> u32 {
        self.pointer_bits
    }

    /// Whether the code generator lowers a `select` of a value of this type.
    ///
    /// Every target lowers one of an integer of 8, 16, 32 or 64 bits, which are the four widths
    /// `crates/rucc-ir/src/term.rs` names a `select` at. A target whose `TargetInfo::selects_any`
    /// is true also lowers one of a pointer, a `float` and a `double`. A wider integer, a wider
    /// float, a bit and a vector have no lowering on any target.
    #[must_use]
    pub fn selects(self, ty: Type) -> bool {
        if !ty.is_scalar() {
            return false;
        }
        if ty.is_int() {
            return matches!(ty.bits(), 8 | 16 | 32 | 64);
        }
        self.selects_any && (ty == Type::PTR || (ty.is_float() && matches!(ty.bits(), 32 | 64)))
    }

    /// The machine for costs already in hand.
    ///
    /// What [`Machine::of`] is written in terms of, and what a caller that resolved a target some
    /// other way uses. `None` is a target with no cost table, which is every architecture rucc has
    /// no back end for.
    #[must_use]
    pub const fn with(costs: Option<&'static dyn TargetCosts>, goal: Goal) -> Self {
        Self { costs, goal, isa: Isa::NONE, counts: &[], selects_any: false, pointer_bits: 64 }
    }

    /// A machine nobody has a cost table for, which is what a test that does not care wants.
    ///
    /// Named for what it is rather than called `default`, because a default machine is the thing
    /// this module exists to avoid: a pass that got one silently would be optimizing for a target
    /// that does not exist.
    #[must_use]
    pub const fn unknown() -> Self {
        Self::with(None, Goal::Speed)
    }

    /// Whether the code generator selects one instruction for that bit count at that width in
    /// this function.
    ///
    /// The function's own `target` attribute where it has one and the module's extensions where it
    /// does not, which is the choice the code generator makes, read from the table it reads. A
    /// count with no instruction behind it is written out as a dozen shifts and masks and a
    /// multiply, so a pass that would put one where a loop was asks here first.
    #[must_use]
    pub fn counts_in_one(self, func: &Func, opcode: Opcode, bits: u32) -> bool {
        let of = match opcode {
            Opcode::Ctpop => BitCount::Ones,
            Opcode::Ctlz => BitCount::LeadingZeros,
            Opcode::Cttz => BitCount::TrailingZeros,
            _ => return false,
        };
        CountInst::in_one(self.counts, of, bits, func.target.unwrap_or(self.isa))
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
