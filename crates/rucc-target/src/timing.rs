//! How long each of a machine's instructions takes, and which part of the machine it takes it on.
//!
//! Design: `spec/optimizer/38-scheduling-and-layout.md` sections 38.1 and 38.6.
//!
//! A scheduler puts the instructions of a block in the order that finishes soonest, and the only
//! thing that makes one order finish sooner than another is that the machine does not answer every
//! instruction in one cycle. So a scheduler needs two numbers about each instruction: how long
//! after it starts what it wrote may be read, and what it was using while it ran, because two
//! instructions that want the same part of the machine cannot both start in the same cycle however
//! independent they are.
//!
//! Those two numbers are this. They are a target's answer, for the reason every other description
//! in this crate is: the pass is in a pipeline crate, `spec/10-backend.md` section 10.8 says a
//! pipeline crate holds no target specific code, so an opcode is a name to it and how long a name
//! takes is something it is told.
//!
//! # Why the numbers are allowed to be wrong
//!
//! They are measurements of a particular processor, taken from published tables, and a program
//! compiled with them runs on whatever processor the person who runs it has. Spec 10.5 settles
//! what to do about that: "an incorrect model produces slow code rather than wrong code, which is
//! the right failure mode". A schedule is a permutation of instructions that were already going to
//! run, so a model that is wrong about every number produces a program that computes the same
//! thing at a different speed.
//!
//! What a wrong model must not do is be wrong quietly. [`TimingInsts::accurate`] is how a model
//! says which kind it is, and it is here from the first model rather than added when the first
//! model turns out to be wrong. `gcc/params.opt:77` has the same flag, `cycle-accurate-model`,
//! `Init(1)`, and is unusually direct about what it is for: "Whether the scheduling description is
//! mostly a cycle-accurate model of the target processor and is likely to spill aggressively to
//! fill any pipeline bubbles."
//!
//! A model that says `false` is one whose latencies are worth believing and whose picture of the
//! machine's units is not, because the latencies come out of a table of measured numbers and the
//! units are a summary of a pipeline nobody wrote down here. `rucc_codegen::schedule` reads it
//! exactly that way: it orders by latency either way, and it only holds an instruction back for
//! want of a free unit when the model says it is worth believing about units.
//!
//! # What is not in here
//!
//! How many micro-operations an instruction decodes to, which port each of them goes to, and what
//! the machine does when the queue in front of one fills. That is what a cycle accurate model is
//! and it is what `gcc/config/*/*.md`'s automata are built out of. No target here has one, every
//! target here says so, and section 38.8 owes the measurement that says how much that costs.

/// What a scheduler has to know about a machine to put a block in an order.
#[derive(Debug, Clone, Copy)]
pub struct TimingInsts {
    /// What a rule file and the machine IR put in front of this target's opcodes, such as `x64.`.
    pub prefix: &'static str,
    /// Which processor the numbers describe, and where they were read out of.
    ///
    /// A sentence rather than a name, because the useful thing to know about a model is not what
    /// it is called but what it was taken from and when. It is printed by `--print-config` and it
    /// is the first thing anybody comparing two runs of a benchmark wants.
    pub model: &'static str,
    /// Whether the numbers are a cycle accurate model of that processor's pipeline.
    ///
    /// See the module comment. No target here says `true`, and a target that starts saying it has
    /// to mean it: the scheduler answers this by enforcing the unit counts below cycle by cycle,
    /// which turns a wrong unit count from a heuristic that led nowhere into instructions held
    /// back for a reason that was not real.
    pub accurate: bool,
    /// How many instructions the machine starts in one cycle.
    pub width: u32,
    /// How many of each unit the machine has.
    ///
    /// [`Unit::Free`] and [`Unit::Fixed`] answer with the width, since an instruction that needs no
    /// unit is held back by nothing but the width, and answering zero would be a machine that
    /// cannot run a `nop`.
    pub slots: fn(Unit) -> u32,
    /// What an instruction of that name costs, or [`None`] for a name this target does not have.
    ///
    /// [`None`] rather than a guess, for the reason [`crate::MachineInsts::operands`] answers
    /// [`None`]: a pass that is told a made up number about an instruction nobody described has no
    /// way to find out it was made up, and a pass that is told nothing stops.
    pub timing: fn(&str) -> Option<Timing>,
}

/// What one instruction costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// How many cycles after it starts before what it wrote may be read.
    ///
    /// Zero for an instruction that encodes to nothing, which several of this machine's do: they
    /// are there to tell the allocator where a value already is, and a schedule that thought they
    /// took a cycle would be a schedule built around instructions that are not in the output.
    pub latency: u32,
    /// Which part of the machine it is using while it runs.
    pub unit: Unit,
}

/// The parts of a machine a scheduler counts.
///
/// A summary of a real processor's ports rather than a description of them. What it has to get
/// right is which instructions compete with each other, and the ones that compete are the ones
/// that are scarce: there are several units that add and one that divides, so a block full of
/// divisions is limited by the divider and a block full of additions is limited by the width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Unit {
    /// Ordinary integer work: an addition, a shift, a comparison, a move between registers.
    Int,
    /// An integer multiply, which every machine here has fewer of than it has adders.
    Mul,
    /// An integer divide, which is the one integer instruction that is not pipelined anywhere.
    Div,
    /// A read of memory, including the read folded into an instruction that then does arithmetic.
    Load,
    /// A write of memory.
    Store,
    /// A branch, a call and a return.
    Branch,
    /// Floating point arithmetic, including the conversions between floating point and integers.
    Float,
    /// A floating point divide, which is not pipelined for the reason the integer one is not.
    FloatDiv,
    /// Nothing the machine has to find room for, which is what an instruction that encodes to
    /// nothing costs.
    Free,
    /// Something the model does not describe, and which nothing may be reordered around.
    ///
    /// A fence, a trap, a landing pad, a run of padding a patcher was promised, a hint to a spin
    /// loop, and the instruction that sets the old floating point unit's rounding mode. Each of
    /// them is in the function for a reason that is not a value anything reads, so the operands do
    /// not say what it does and a scheduler reading only the operands would move it or move
    /// something past it. `rucc_codegen::schedule` stops at one.
    ///
    /// It is a unit rather than a flag of its own because a scheduler asks one question of the
    /// model about each instruction and this is one of the answers: the machine is doing something
    /// here, and what it is doing is not on the list.
    Fixed,
}

impl Unit {
    /// Every unit, which is what a target's own test walks to check it answered about all of them.
    pub const ALL: &'static [Self] = &[
        Self::Int,
        Self::Mul,
        Self::Div,
        Self::Load,
        Self::Store,
        Self::Branch,
        Self::Float,
        Self::FloatDiv,
        Self::Free,
        Self::Fixed,
    ];
}

impl PartialEq for TimingInsts {
    /// Whether the two are the same model, which is what the target's own name for it says.
    ///
    /// The two functions are left out. Comparing those would be comparing addresses, and the
    /// compiler is right that an address says nothing here: one function can have two of them and
    /// two functions can share one. Every one of these is a `static` a target wrote out by hand
    /// with its name in [`TimingInsts::model`], so the name is the question anybody holding two of
    /// these is asking.
    fn eq(&self, other: &Self) -> bool {
        self.prefix == other.prefix
            && self.model == other.model
            && self.accurate == other.accurate
            && self.width == other.width
    }
}

impl Eq for TimingInsts {}

impl TimingInsts {
    /// The name with this target's prefix taken off, which is how its own description spells it.
    #[must_use]
    pub fn bare<'a>(&self, name: &'a str) -> &'a str {
        name.strip_prefix(self.prefix).unwrap_or(name)
    }

    /// What an instruction of that name costs on this machine.
    #[must_use]
    pub fn of(&self, name: &str) -> Option<Timing> {
        (self.timing)(self.bare(name))
    }

    /// How many of that unit this machine has, never fewer than one.
    ///
    /// Never fewer than one because a unit no instruction can ever get a slot on is a scheduler
    /// that does not terminate, and a target that wrote a zero meant that the unit is not there
    /// rather than that the instructions needing it never run.
    #[must_use]
    pub fn slots(&self, unit: Unit) -> u32 {
        match unit {
            Unit::Free | Unit::Fixed => self.width.max(1),
            unit => (self.slots)(unit).max(1),
        }
    }
}
