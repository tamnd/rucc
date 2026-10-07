//! Conditional constant propagation, over the bits of each value that are known.
//!
//! Design: `spec/optimizer/14-constant-propagation.md`, and tamnd/rucc#2339.
//!
//! ```c
//! int x = 1, done = 0;
//! for (int i = 0; i < n; i++) {
//!     if (x != 1) done = 1;
//!     x = 1;
//! }
//! return done;
//! ```
//!
//! After scalar replacement `x` and `done` are parameters of the loop header, each fed by the entry
//! and by the latch. Every other pass looks at one of them and sees a parameter fed partly by
//! itself, and a parameter fed by itself could be anything. This pass starts from the other end: it
//! assumes every block is unreachable and every value undefined, and only gives that up where the
//! function forces it to. `x` is one on the way in and one on the way round, so `x != 1` is false,
//! so the edge that sets `done` is never taken, so `done` is zero on both of the edges that are.
//! Iterating the fold and the branch removal to a fixpoint does not get there, because both start
//! from the assumption that every edge is taken and never have a reason to drop it.
//!
//! # Bits, not constants
//!
//! What is known about a value is [`Bits`], the same known bits the range analysis keeps beside its
//! intervals, and a constant is the case where all of them are known. Section 14.2 says this is
//! gcc's design and why it is the better one: that a value is even, or fits in eight bits, is a fact
//! a constant cannot hold and a later comparison or mask can use. It is also what keeps the fixpoint
//! short. Each fact only ever loses known bits, so a value changes at most once for each bit it has,
//! where an interval can grow by one on every trip round a loop.
//!
//! The arithmetic is the table in [`crate::range::ops`], asked with the operands turned into ranges
//! that know nothing but their bits. Section 14.4 says the pass should not contain arithmetic of its
//! own, and the ranges already have every operation this reads, with the flags taken into account.
//!
//! # The fixpoint and what is done with it
//!
//! Two worklists, as Wegman and Zadeck describe it: blocks that have just become reachable, every
//! instruction in which is looked at, and values whose fact just changed, every reader of which is
//! looked at again. A branch whose condition is known sends only the edge it takes, and a block
//! parameter is what the reachable edges into its block pass.
//!
//! A branch on a value still undefined when the lists run dry is a branch the analysis never
//! decided, and assuming neither of its edges is taken would be concluding something from nothing.
//! Every such branch has all its edges taken and the fixpoint resumes, which is what LLVM does about
//! the same thing. Since there is no instruction in the IR that makes an undefined value, the only
//! way to get one is a cycle nothing enters, and a branch in one of those is unreachable anyway.
//!
//! Section 14.6 says fuel gates the substitution and not the fixpoint, because a fixpoint stopped
//! early holds assumptions nothing has checked yet. So the fixpoint always runs to the end, and then
//! each value whose bits are all known becomes a constant for as long as the fuel lasts. No block is
//! deleted and no branch is rewritten here: a branch whose condition became a constant is a fold for
//! `prune` and `simplify-cfg`, which run after this, as section 06.5 has it.
//!
//! Two more things are done with bits short of a constant. An access whose address is proved more
//! aligned than it says takes the larger alignment, and an extension of a truncation is what was
//! truncated when the bits the truncation dropped are known to be what the extension puts back.

use std::cell::RefCell;

use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, Extra, Flags, Func, Imm, Inst, InstData, IntPred, MemInfo, MemOrder, Opcode, Type,
    Value, ValueList,
};

use crate::range::ops::{self, Truth};
use crate::range::{Bits, Range};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats, uses};

/// What the pass calls itself, which is what `-fdump-ir=after-sccp` spells.
pub const NAME: &str = "sccp";

/// Recorded once for each value that became a constant.
const REPLACED: &str = "value proved constant and replaced";

/// Recorded for a value proved constant after the fuel ran out.
const NO_FUEL: &str = "value proved constant and not replaced, the pass ran out of fuel";

/// Recorded for each branch edge the fixpoint never took.
const DEAD_EDGE: &str = "branch edge proved never taken";

/// Recorded for each branch on a value the fixpoint never settled, which then takes every edge.
const UNDECIDED: &str = "branch on an undefined value given all its edges";

/// Recorded for each access whose address was proved more aligned than the access said.
const ALIGNED: &str = "access alignment raised to what its address is proved to be";

/// Recorded for an access that could have been given a larger alignment after the fuel ran out.
const NO_FUEL_ALIGN: &str = "access alignment not raised, the pass ran out of fuel";

/// Recorded once for each extension of a truncation that gave back what was truncated.
const UNEXTENDED: &str = "extension of a truncation whose dropped bits were known replaced by \
                          what was truncated";

/// Recorded for an extension that could have gone after the fuel ran out.
const NO_FUEL_UNEXTEND: &str = "extension of a truncation kept, the pass ran out of fuel";

/// How many bits a pointer is followed at.
///
/// The IR gives a pointer no width, because that belongs to the target, so the fixpoint follows
/// one at 64 bits and keeps only what it knows of the low [`POINTER_KNOWN`] of them. Every
/// target has pointers at least that wide, so an address worked out in that many bits is the same
/// whichever target it is for, and what a 32-bit `inttoptr` puts above them is never assumed.
const POINTER: u32 = 64;

/// How many of a pointer's low bits the fixpoint may know. See [`POINTER`].
const POINTER_KNOWN: u32 = 32;

/// The most an access is aligned to on the strength of its address, in bytes.
///
/// Sixteen is the widest access either x86-64 or AArch64 asks an alignment of, so more than that
/// buys nothing and only makes the IR say more than anything reads.
const ALIGN_LIMIT: u32 = 16;

/// The pass. It holds nothing, because everything it knows it works out from the function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sccp;

impl Pass for Sccp {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a value that is the same on every path the function can take becomes a constant"
    }

    fn preserves(&self) -> Preserved {
        // Constants where instructions were and at the top of blocks, and no edge touched. The
        // operands the replaced instructions read are read by fewer things now, which is the
        // liveness. A branch whose condition is now a constant, or is compared with one, is also
        // guessed differently by the predictors, so the frequencies go too.
        Preserved::ALL.without(Analysis::Liveness).without(Analysis::Frequencies)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(solved) = Solver::solve(func, &mut stats) else { return stats };
        replace(func, &solved, fuel, &mut stats);
        align(func, &solved, fuel, &mut stats);
        unextend(func, &solved, fuel, &mut stats);
        stats
    }
}

/// What is known about one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fact {
    /// Nothing has reached it yet, which is the optimistic start.
    Undefined,
    /// An integer, and these of its bits.
    Known(Bits),
    /// Anything else, about which this pass says nothing.
    Varying,
}

impl Fact {
    /// What a value is known to be when it could have come from either.
    fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Undefined, fact) | (fact, Self::Undefined) => fact,
            (Self::Known(a), Self::Known(b)) => Self::Known(a.join(b)),
            _ => Self::Varying,
        }
    }
}

/// Whether this pass keeps bits for a value of this type.
///
/// An address is one, for its low bits, which is what says how aligned it is. It is never turned
/// into a constant, because its high bits are never known, with the one exception of the null
/// pointer, which is zero at every width. See [`POINTER`].
fn tracked(ty: Type) -> bool {
    (ty.is_int() && ty.is_scalar()) || ty.is_ptr()
}

/// How many bits a value of this type is followed at.
fn followed(ty: Type) -> u32 {
    if ty.is_ptr() { POINTER } else { ty.bits() }
}

/// The fact for a value this pass knows nothing about, which is every bit unknown for an integer.
fn nothing(ty: Type) -> Fact {
    if tracked(ty) { Fact::Known(Bits::unknown(followed(ty))) } else { Fact::Varying }
}

/// The value, as a constant, when every one of its bits is known.
fn constant(fact: Fact, ty: Type) -> Option<u128> {
    let Fact::Known(bits) = fact else { return None };
    (ty.is_int() && ty.is_scalar() && bits.unknown_bits() == 0).then_some(bits.value())
}

/// What may be known of an address, which is its low [`POINTER_KNOWN`] bits and nothing above.
///
/// Except for the null pointer. A zero made an address is zero whatever width the target gives an
/// address, so knowing all of it assumes nothing about the target. It is what a pointer every way
/// round a loop passes `NULL` to comes out as, and keeping it is what lets a test of that pointer
/// against `NULL` be read: the kernel's `if (vi && ...)` in front of a warning, where gcc knows `vi`
/// is null on every path and the warning goes.
fn address(fact: Fact) -> Fact {
    let Fact::Known(bits) = fact else { return fact };
    if null(fact) {
        return fact;
    }
    let low = (1u128 << POINTER_KNOWN) - 1;
    let all = (1u128 << POINTER) - 1;
    let unknown = bits.unknown_bits() | (all & !low);
    Fact::Known(Bits::from_parts(bits.value() & low, unknown, POINTER))
}

/// Whether this is the null pointer, every bit known and every one of them zero.
fn null(fact: Fact) -> bool {
    matches!(fact, Fact::Known(bits) if bits.unknown_bits() == 0 && bits.value() == 0)
}

/// What the fixpoint came to, apart from the function so the function can be rewritten with it.
struct Solved {
    /// What is known about each value, by [`Value::index`].
    facts: Vec<Fact>,
    /// Whether each block is reachable, by [`Block::index`].
    reached: Vec<bool>,
}

/// Which arms of a terminator are taken.
enum Arms {
    /// Every one, because the condition could be anything or the terminator has no condition.
    Every,
    /// None yet, because nothing has reached the condition.
    NoneYet,
    /// This one, by position, because the condition settles it.
    One(usize),
}

/// The fixpoint while it runs.
struct Solver<'a> {
    func: &'a Func,
    /// What is known about each value, by [`Value::index`].
    facts: Vec<Fact>,
    /// Whether each block is reachable, by [`Block::index`].
    reached: Vec<bool>,
    /// The instructions that read each value as one of their own arguments.
    readers: Vec<Vec<Inst>>,
    /// Where each value is passed to a block, as the branch, the arm by position and the
    /// parameter that takes it.
    edges: Vec<Vec<(Inst, usize, Value)>>,
    /// Which arms of each terminator have been taken, by position.
    taken: Set<(Inst, usize)>,
    /// The branches on an undefined value that were given every edge.
    forced: Set<Inst>,
    /// Blocks to look at every instruction of.
    blocks: Vec<Block>,
    /// Values whose readers to look at again.
    values: Vec<Value>,
}

impl<'a> Solver<'a> {
    /// Runs the fixpoint over the function, or nothing for a declaration.
    fn solve(func: &'a Func, stats: &mut Stats) -> Option<Solved> {
        let entry = func.entry()?;
        let counts = func.counts();
        let mut readers = vec![Vec::new(); counts.values];
        let mut edges = vec![Vec::new(); counts.values];
        // A value passed to a block goes to the one parameter that takes it rather than to the
        // whole branch. A computed goto passes the same values to every one of its arms, and
        // looking at the branch again for each change would walk every arm each time, and once
        // for each arm the value was passed to as well, tamnd/rucc#2857.
        for block in func.blocks() {
            for inst in func.insts(block) {
                for &value in &func[func[inst].args] {
                    let list = &mut readers[value.index()];
                    if list.last() != Some(&inst) {
                        list.push(inst);
                    }
                }
                for (arm, call) in func.successors(inst).enumerate() {
                    for (&param, &value) in func[call.block].params.iter().zip(&func[call.args]) {
                        edges[value.index()].push((inst, arm, param));
                    }
                }
            }
        }
        let mut solver = Self {
            func,
            facts: vec![Fact::Undefined; counts.values],
            reached: vec![false; counts.blocks],
            readers,
            edges,
            taken: Set::default(),
            forced: Set::default(),
            blocks: Vec::new(),
            values: Vec::new(),
        };
        // The entry's parameters are the function's, which come from a caller nothing here sees.
        for &param in &func[entry].params {
            solver.facts[param.index()] = nothing(func[param].ty);
        }
        solver.reach(entry);
        loop {
            solver.settle();
            if !solver.force(stats) {
                break;
            }
        }
        for block in func.blocks().filter(|block| solver.reached[block.index()]) {
            let Some(term) = func.terminator(block) else { continue };
            let arms = func.successors(term).count();
            let dead = (0..arms).filter(|&arm| !solver.taken.contains(&(term, arm))).count();
            for _ in 0..dead {
                stats.note(DEAD_EDGE);
            }
        }
        Some(Solved { facts: solver.facts, reached: solver.reached })
    }

    /// Works both lists until neither has anything left on it.
    fn settle(&mut self) {
        loop {
            if let Some(block) = self.blocks.pop() {
                for inst in self.func.insts(block) {
                    self.visit(inst);
                }
                continue;
            }
            let Some(value) = self.values.pop() else { return };
            for index in 0..self.readers[value.index()].len() {
                let inst = self.readers[value.index()][index];
                let reached =
                    self.func.block_of(inst).is_some_and(|block| self.reached[block.index()]);
                if reached {
                    self.visit(inst);
                }
            }
            for index in 0..self.edges[value.index()].len() {
                let (term, arm, param) = self.edges[value.index()][index];
                if self.taken.contains(&(term, arm)) {
                    self.pass(param, value);
                }
            }
        }
    }

    /// Gives every edge to each reachable branch still on an undefined value, and says whether
    /// there was one.
    fn force(&mut self, stats: &mut Stats) -> bool {
        let mut any = false;
        for block in self.func.blocks() {
            if !self.reached[block.index()] {
                continue;
            }
            let Some(term) = self.func.terminator(block) else { continue };
            let data = &self.func[term];
            if !matches!(data.opcode, Opcode::BrIf | Opcode::Switch) || self.forced.contains(&term)
            {
                continue;
            }
            let Some(&cond) = self.func[data.args].first() else { continue };
            if self.facts[cond.index()] == Fact::Undefined {
                self.forced.insert(term);
                stats.note(UNDECIDED);
                self.visit(term);
                any = true;
            }
        }
        any
    }

    /// Marks a block reachable and queues its instructions, the first time.
    fn reach(&mut self, block: Block) {
        if !self.reached[block.index()] {
            self.reached[block.index()] = true;
            self.blocks.push(block);
        }
    }

    /// Joins what a value is known to be with something new, and queues its readers if that
    /// changed anything.
    fn learn(&mut self, value: Value, fact: Fact) {
        let old = self.facts[value.index()];
        let new = old.join(fact);
        if new != old {
            self.facts[value.index()] = new;
            self.values.push(value);
        }
    }

    /// Looks at one instruction in a reachable block.
    fn visit(&mut self, inst: Inst) {
        if self.func.is_terminator(inst) {
            // An asm goto is a terminator with outputs, and they come from the asm, which nothing
            // here can see into.
            for value in self.func[inst].results() {
                self.learn(value, nothing(self.func[value].ty));
            }
            self.branch(inst);
            return;
        }
        let data = &self.func[inst];
        let fact = match (data.results, data.results().next()) {
            (1, Some(one)) => self.transfer(inst, self.func[one].ty),
            _ => Fact::Varying,
        };
        for value in self.func[inst].results() {
            let fact = if fact == Fact::Varying { nothing(self.func[value].ty) } else { fact };
            self.learn(value, fact);
        }
    }

    /// Takes the arms of a terminator its condition allows, passing on what each passes the
    /// first time it is taken. What an argument learns after that reaches its parameter along
    /// the edge [`Solver::settle`] follows.
    fn branch(&mut self, term: Inst) {
        // The function outlives the solver's borrow of itself, so its lists are read in place
        // while the solver learns from them.
        let func = self.func;
        let only = match self.arm(term) {
            Arms::One(arm) => Some(arm),
            Arms::NoneYet => return,
            Arms::Every => None,
        };
        for (arm, call) in func.successors(term).enumerate() {
            if only.is_some_and(|only| only != arm) || !self.taken.insert((term, arm)) {
                continue;
            }
            for (&param, &arg) in func[call.block].params.iter().zip(&func[call.args]) {
                self.pass(param, arg);
            }
            self.reach(call.block);
        }
    }

    /// Joins what an argument is known to be into the parameter that takes it.
    fn pass(&mut self, param: Value, arg: Value) {
        let fact =
            if tracked(self.func[param].ty) { self.facts[arg.index()] } else { Fact::Varying };
        self.learn(param, fact);
    }

    /// Which arms of a terminator its condition lets through.
    fn arm(&self, term: Inst) -> Arms {
        let data = &self.func[term];
        if !matches!(data.opcode, Opcode::BrIf | Opcode::Switch) || self.forced.contains(&term) {
            return Arms::Every;
        }
        let Some(&cond) = self.func[data.args].first() else { return Arms::Every };
        let ty = self.func[cond].ty;
        let bits = match self.facts[cond.index()] {
            Fact::Undefined => return Arms::NoneYet,
            Fact::Known(bits) => bits,
            Fact::Varying => return Arms::Every,
        };
        if data.opcode == Opcode::BrIf {
            // The first arm is the one taken when the condition is not zero, which a single known
            // one bit settles as well as a known value does.
            if bits.value() != 0 {
                return Arms::One(0);
            }
            return if bits.unknown_bits() == 0 { Arms::One(1) } else { Arms::Every };
        }
        let Some(value) = constant(Fact::Known(bits), ty) else { return Arms::Every };
        let Extra::Switch(at) = data.extra else { return Arms::Every };
        let cases = &self.func[self.func[at].cases];
        let mask = Bits::exactly(u128::MAX, ty.bits()).value();
        // The default is the first target and each case is one after its value.
        let case = cases.iter().position(|imm| imm.unsigned() & mask == value);
        Arms::One(case.map_or(0, |case| case + 1))
    }

    /// What an instruction's one result is known to be, from what its operands are.
    fn transfer(&self, inst: Inst, ty: Type) -> Fact {
        let fact = self.bits_of(inst, ty);
        if ty.is_ptr() { address(fact) } else { fact }
    }

    /// What [`Solver::transfer`] works out, before an address has its high bits forgotten.
    fn bits_of(&self, inst: Inst, ty: Type) -> Fact {
        if !tracked(ty) {
            return Fact::Varying;
        }
        let data = &self.func[inst];
        let args = &self.func[data.args];
        let width = followed(ty);
        // A local is at an address that is a multiple of its alignment, which is what the frame
        // promises when it places one.
        if data.opcode == Opcode::Alloca {
            let Extra::Mem(mem) = data.extra else { return nothing(ty) };
            let align = self.func[mem].align.max(1);
            if !align.is_power_of_two() {
                return nothing(ty);
            }
            let low = u128::from(align - 1);
            let all = (1u128 << POINTER) - 1;
            return Fact::Known(Bits::from_parts(0, all & !low, POINTER));
        }
        if data.opcode == Opcode::IConst {
            let Extra::Imm(at) = data.extra else { return nothing(ty) };
            return Fact::Known(Bits::exactly(self.func[at].unsigned(), width));
        }
        if data.opcode == Opcode::Select {
            let [cond, then, other] = *args else { return nothing(ty) };
            return match self.facts[cond.index()] {
                Fact::Undefined => Fact::Undefined,
                Fact::Known(bits) if bits.value() != 0 => self.facts[then.index()],
                Fact::Known(bits) if bits.unknown_bits() == 0 => self.facts[other.index()],
                _ => self.facts[then.index()].join(self.facts[other.index()]),
            };
        }
        if !reads(data.opcode) {
            return nothing(ty);
        }
        // Every opcode the table reads takes one operand or two.
        let mut known = [(0, Bits::unknown(1)); 2];
        if args.len() > known.len() {
            return nothing(ty);
        }
        for (at, &arg) in args.iter().enumerate() {
            let arg_ty = self.func[arg].ty;
            match self.facts[arg.index()] {
                Fact::Undefined => return Fact::Undefined,
                Fact::Known(bits) if tracked(arg_ty) => known[at] = (followed(arg_ty), bits),
                _ => return nothing(ty),
            }
        }
        let pred = match data.extra {
            Extra::IntPred(pred) => Some(pred),
            _ => None,
        };
        let asked =
            Asked { opcode: data.opcode, flags: data.flags, pred, width, known, args: args.len() };
        ANSWERS.with_borrow_mut(|answers| {
            if let Some(&fact) = answers.get(&asked) {
                return fact;
            }
            let ranges = known.map(|(width, bits)| Range::full(width).narrow(bits));
            let fact = match arithmetic(data, &ranges[..args.len()], width) {
                Some(range) if range.is_empty() => Fact::Undefined,
                Some(range) if range.width() == width => Fact::Known(range.bits()),
                _ => nothing(ty),
            };
            if answers.len() >= ANSWERED {
                answers.clear();
            }
            answers.insert(asked, fact);
            fact
        })
    }
}

/// Everything the range table reads to work out a result: the operation, its flags and the
/// predicate of a comparison, the width of the result, and the width and the known bits of each
/// operand. The same question has the same answer, which is what lets [`ANSWERS`] keep it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Asked {
    opcode: Opcode,
    flags: Flags,
    pred: Option<IntPred>,
    width: u32,
    known: [(u32, Bits); 2],
    args: usize,
}

/// How many answers [`ANSWERS`] holds before it starts over.
const ANSWERED: usize = 1 << 16;

thread_local! {
    // The range table's answers, kept from one run to the next. An inlining decision runs this
    // pass on a copy of a callee for each call it weighs, and the copies ask the same questions
    // over and over: on monocypher at -O2 working the answers out again was a quarter of the
    // compile, tamnd/rucc#3052. A result whose range is empty or of another width is kept as the
    // fact it becomes, which only reads the width, so the answer does not depend on the type.
    static ANSWERS: RefCell<Map<Asked, Fact>> = RefCell::new(Map::default());
}

/// Whether the transfer for this opcode is the range table's, which is the list of operations
/// whose result is a function of their operands and nothing else.
const fn reads(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::Trunc
            | Opcode::ZExt
            | Opcode::SExt
            | Opcode::ICmp
            | Opcode::Ctlz
            | Opcode::Cttz
            | Opcode::Ctpop
            | Opcode::PtrAdd
            | Opcode::PtrToInt
            | Opcode::IntToPtr
    )
}

/// The range of the result, from the ranges of the operands, or nothing where the operands are not
/// the shape the operation takes.
fn arithmetic(data: &InstData, ranges: &[Range], width: u32) -> Option<Range> {
    let flags = data.flags;
    let pair = || match *ranges {
        [a, b] if a.width() == b.width() => Some((a, b)),
        _ => None,
    };
    let range = match data.opcode {
        Opcode::Add => pair().map(|(a, b)| ops::add(a, b, flags))?,
        Opcode::Sub => pair().map(|(a, b)| ops::sub(a, b, flags))?,
        Opcode::Mul => pair().map(|(a, b)| ops::mul(a, b, flags))?,
        Opcode::And => pair().map(|(a, b)| ops::and(a, b))?,
        Opcode::Or => pair().map(|(a, b)| ops::or(a, b))?,
        Opcode::Xor => pair().map(|(a, b)| ops::xor(a, b))?,
        Opcode::Shl => pair().map(|(a, b)| ops::shl(a, b, flags))?,
        Opcode::LShr => pair().map(|(a, b)| ops::lshr(a, b, flags))?,
        Opcode::AShr => pair().map(|(a, b)| ops::ashr(a, b, flags))?,
        // The offset is as wide as the target's addresses, which may be narrower than the width a
        // pointer is followed at, and the low bits of a sum do not depend on how it was widened.
        Opcode::PtrAdd => {
            let [base, offset] = *ranges else { return None };
            let offset = if offset.width() < base.width() {
                ops::sext(offset, base.width())
            } else {
                offset
            };
            if offset.width() != base.width() {
                return None;
            }
            let sum = ops::add(base, offset, Flags::NONE);
            sum.narrow(low_sum(base.bits(), offset.bits(), base.width()))
        }
        Opcode::PtrToInt | Opcode::IntToPtr => {
            let from = *ranges.first()?;
            match from.width().cmp(&width) {
                std::cmp::Ordering::Greater => ops::trunc(from, width),
                std::cmp::Ordering::Equal => from,
                std::cmp::Ordering::Less => ops::zext(from, width),
            }
        }
        Opcode::Trunc => ops::trunc(*ranges.first()?, width),
        Opcode::ZExt => ops::zext(*ranges.first()?, width),
        Opcode::SExt => ops::sext(*ranges.first()?, width),
        Opcode::ICmp => {
            let Extra::IntPred(pred) = data.extra else { return None };
            let (a, b) = pair()?;
            match ops::compare(pred, a, b) {
                Truth::Always => Range::exactly(1, width),
                Truth::Never => Range::exactly(0, width),
                Truth::Either => Range::between(0, 1, width),
            }
        }
        // A count is never more than the width of what it counts.
        Opcode::Ctlz | Opcode::Cttz | Opcode::Ctpop => {
            Range::between(0, u128::from(ranges.first()?.width()), width)
        }
        _ => return None,
    };
    Some(range)
}

/// Turns every value the fixpoint proved constant into one, while the fuel lasts.
///
/// An instruction whose result is a constant becomes the constant where it stands, the way
/// [`crate::fold`] does it. A block parameter gets a constant at the top of its block and its
/// readers pointed at that, and the parameter itself is left for the passes that remove unread ones.
fn replace(func: &mut Func, solved: &Solved, fuel: &mut Fuel, stats: &mut Stats) {
    let blocks: Vec<Block> = func.blocks().filter(|block| solved.reached[block.index()]).collect();
    let mut forward = Map::default();
    let zero = zero(func);
    for block in blocks {
        let params = func[block].params.clone();
        for param in params {
            let ty = func[param].ty;
            let fact = solved.facts[param.index()];
            let (value, number) = match (constant(fact, ty), zero) {
                (Some(value), _) => (value, ty),
                (None, Some(number)) if ty.is_ptr() && null(fact) => (0, number),
                _ => continue,
            };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            let Some(first) = func.insts(block).next() else { continue };
            let at = func.add_imm(Imm::from_bits(value));
            let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) };
            let span = func.span(first);
            let made = func.create_inst(data, &[number], span);
            func.insert_before(made, first);
            let mut result = func[made].results().next().expect("a constant is one value");
            if ty.is_ptr() {
                let args = func.push_values(&[result]);
                let data = InstData { args, ..InstData::new(Opcode::IntToPtr) };
                let cast = func.create_inst(data, &[ty], span);
                func.insert_before(cast, first);
                result = func[cast].results().next().expect("a cast is one value");
            }
            forward.insert(param, result);
            stats.optimized(REPLACED);
        }
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let data = func[inst];
            if !reads(data.opcode) && data.opcode != Opcode::Select {
                continue;
            }
            let Some(result) = data.results().next() else { continue };
            let ty = func[result].ty;
            // A null pointer made for a parameter just above is newer than the fixpoint, and is
            // already what it would be replaced with.
            let Some(&fact) = solved.facts.get(result.index()) else { continue };
            let Some(value) = constant(fact, ty) else { continue };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            let at = func.add_imm(Imm::from_bits(value));
            let data = &mut func[inst];
            data.opcode = Opcode::IConst;
            data.flags = Flags::NONE;
            data.args = ValueList::EMPTY;
            data.extra = Extra::Imm(at);
            stats.optimized(REPLACED);
        }
    }
    if !forward.is_empty() {
        uses::substitute(func, &forward);
    }
}

/// The integer type the function already makes its null pointers from, if it makes one.
///
/// A null pointer put in a parameter's place is a zero and an `inttoptr` of it, and the zero wants
/// a type. The IR does not say how wide an address is, so the type is the one the front end chose,
/// read off a null pointer it wrote. A parameter that is null on every way in got that from one of
/// those, so there is always one to find when there is something to replace.
fn zero(func: &Func) -> Option<Type> {
    for block in func.blocks() {
        for inst in func.insts(block) {
            if func[inst].opcode != Opcode::IntToPtr {
                continue;
            }
            let &[number] = &func[func[inst].args] else { continue };
            let rucc_ir::Def::Result { inst: made, .. } = func[number].def else { continue };
            if func[made].opcode != Opcode::IConst {
                continue;
            }
            let Extra::Imm(at) = func[made].extra else { continue };
            if func[at].bits() == 0 {
                return Some(func[number].ty);
            }
        }
    }
    None
}

/// What a sum's low bits are, from the low bits both operands have known.
///
/// Below the lowest bit either operand does not know, no carry can come from anything unknown, so
/// those bits of the sum are the sum of what is known. The intervals lose this, and it is what
/// says `alloca` plus 8 is still a multiple of 8.
fn low_sum(a: Bits, b: Bits, width: u32) -> Bits {
    let known = (a.unknown_bits() | b.unknown_bits()).trailing_zeros().min(width);
    let all = if width >= u128::BITS { u128::MAX } else { (1u128 << width) - 1 };
    let low = if known >= u128::BITS { u128::MAX } else { (1u128 << known) - 1 };
    Bits::from_parts(a.value().wrapping_add(b.value()) & low, all & !low, width)
}

/// Gives each access the alignment its address is proved to have, when that is more than it says.
///
/// Section 14.2: a pointer whose low three bits are known zero makes an eight byte access aligned,
/// which is gcc's `get_value_from_alignment`. What the access records is what the backend reads
/// when it decides whether one move does it, so this is the one place the fact changes code. An
/// atomic access is left as it is, since its alignment is part of what it promises and was
/// settled when it was written. A bulk operation takes the smaller of its two addresses.
fn align(func: &mut Func, solved: &Solved, fuel: &mut Fuel, stats: &mut Stats) {
    let blocks: Vec<Block> = func.blocks().filter(|block| solved.reached[block.index()]).collect();
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let Extra::Mem(mem) = func[inst].extra else { continue };
            let info = func[mem];
            if info.order != MemOrder::NotAtomic {
                continue;
            }
            let args = &func[func[inst].args];
            let addresses: Vec<Value> = match (func[inst].opcode, args) {
                (Opcode::Load | Opcode::Memset, [at, ..]) | (Opcode::Store, [_, at, ..]) => {
                    vec![*at]
                }
                (Opcode::Memcpy | Opcode::Memmove, [to, from, ..]) => vec![*to, *from],
                _ => continue,
            };
            let mut zeros = u32::MAX;
            for at in addresses {
                // A null pointer [`replace`] made is newer than the fixpoint and proves nothing.
                let Some(&Fact::Known(bits)) = solved.facts.get(at.index()) else {
                    zeros = 0;
                    break;
                };
                zeros = zeros.min(bits.low_zeros());
            }
            let proved = 1u32 << zeros.min(ALIGN_LIMIT.trailing_zeros());
            if proved <= info.align {
                continue;
            }
            if !fuel.take() {
                stats.missed(NO_FUEL_ALIGN);
                continue;
            }
            let raised = func.add_mem(MemInfo { align: proved, ..info });
            func[inst].extra = Extra::Mem(raised);
            stats.optimized(ALIGNED);
        }
    }
}

/// Points the readers of `sext (trunc x)` and `zext (trunc x)` at `x`, when the bits the
/// truncation dropped are known to be what the extension puts back.
///
/// `mcxt_methods[header & 15]` with the mask made an `int` is that shape: the `and` is truncated
/// to 32 bits and sign extended back to 64 to index the array, and the four bits the `and` leaves
/// say the sign bit was clear all along. Postgres' `pfree` and `repalloc` do it on every call, and
/// x86-64 wrote a `movslq` between the `and` and the multiply for it (tamnd/rucc#1994).
///
/// A zero extension needs every dropped bit known zero. A sign extension needs the dropped bits
/// and the sign bit of what was kept to be known and all the same, zero or one.
fn unextend(func: &mut Func, solved: &Solved, fuel: &mut Fuel, stats: &mut Stats) {
    let blocks: Vec<Block> = func.blocks().filter(|block| solved.reached[block.index()]).collect();
    let mut forward = Map::default();
    for block in blocks {
        for inst in func.insts(block) {
            let data = &func[inst];
            if !matches!(data.opcode, Opcode::SExt | Opcode::ZExt) {
                continue;
            }
            let (Some(result), &[narrow]) = (data.results().next(), &func[data.args]) else {
                continue;
            };
            let rucc_ir::Def::Result { inst: cut, .. } = func[narrow].def else { continue };
            if func[cut].opcode != Opcode::Trunc {
                continue;
            }
            let &[wide] = &func[func[cut].args] else { continue };
            let ty = func[result].ty;
            if func[wide].ty != ty || !ty.is_int() || !ty.is_scalar() {
                continue;
            }
            let Some(&Fact::Known(bits)) = solved.facts.get(wide.index()) else { continue };
            let kept = func[narrow].ty.bits();
            let from = if data.opcode == Opcode::SExt { kept - 1 } else { kept };
            let top = mask(ty.bits()) & !mask(from);
            let zeros = bits.value() & top == 0;
            let ones = data.opcode == Opcode::SExt && bits.value() & top == top;
            if top & bits.unknown_bits() != 0 || !(zeros || ones) {
                continue;
            }
            if !fuel.take() {
                stats.missed(NO_FUEL_UNEXTEND);
                continue;
            }
            forward.insert(result, wide);
            stats.optimized(UNEXTENDED);
        }
    }
    if !forward.is_empty() {
        uses::substitute(func, &forward);
    }
}

/// The low `width` bits.
const fn mask(width: u32) -> u128 {
    if width >= u128::BITS { u128::MAX } else { (1u128 << width) - 1 }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::Sccp;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it under this much fuel,
    /// printed, and checked by the verifier on the way out.
    fn run(body: &str, fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            Sccp.run(&mut module[id], &mut an, fuel);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn solved(body: &str) -> String {
        run(body, &mut Fuel::unlimited())
    }

    /// The loop from the top of the module. `x` goes round as one, so the edge that would set
    /// `done` is never taken and `done` goes round as zero.
    const LOOP: &str = r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = iconst.i32 0
    jump block1(%1, %2, %2)
block1(%3: i32, %4: i32, %5: i32):
    %6 = icmp slt %5, %0
    br_if %6, block2, block5
block2:
    %7 = icmp ne %3, %1
    br_if %7, block3, block4(%4)
block3:
    %8 = iconst.i32 1
    jump block4(%8)
block4(%9: i32):
    %10 = iconst.i32 1
    %11 = add.i32 %5, %10
    jump block1(%10, %9, %11)
block5:
    return %4
}
"#;

    #[test]
    fn a_value_that_goes_round_a_loop_unchanged_is_a_constant() {
        let out = solved(LOOP);
        assert!(out.contains("return %"), "{out}");
        assert!(!out.contains("icmp ne"), "the comparison is a constant, {out}");
        assert!(out.contains("icmp slt"), "and the loop test is not, {out}");
        let ret = out.lines().find(|line| line.contains("return")).expect("a return");
        let returned = ret.trim().trim_start_matches("return ").to_string();
        let def = out
            .lines()
            .find(|line| line.trim().starts_with(&format!("{returned} =")))
            .unwrap_or_else(|| panic!("{returned} is defined, {out}"));
        assert!(def.contains("iconst.i32 0"), "done is zero, {out}");
    }

    /// A branch whose condition is known takes one arm, so what the other arm passes does not
    /// reach the block they meet in.
    #[test]
    fn a_known_branch_sends_only_the_arm_it_takes() {
        let out = solved(
            r#"
func @f() -> i32, linkage(external) {
block0:
    %0 = iconst.i32 3
    %1 = iconst.i32 3
    %2 = icmp eq %0, %1
    br_if %2, block1, block2
block1:
    %3 = iconst.i32 7
    jump block3(%3)
block2:
    %4 = iconst.i32 9
    jump block3(%4)
block3(%5: i32):
    %6 = add.i32 %5, %0
    return %6
}
"#,
        );
        assert!(out.contains("iconst.i32 10"), "seven and three, {out}");
        assert!(!out.contains("add"), "{out}");
    }

    /// A switch on a known value takes its case, and a parameter the default would have set is
    /// what the case sets.
    #[test]
    fn a_known_switch_sends_only_its_case() {
        let out = solved(
            r#"
func @f() -> i32, linkage(external) {
block0:
    %0 = iconst.i32 2
    switch %0, block1(%0), [1 => block1(%0), 2 => block2]
block1(%1: i32):
    %2 = mul.i32 %1, %1
    return %2
block2:
    %3 = iconst.i32 5
    jump block1(%3)
}
"#,
        );
        assert!(out.contains("iconst.i32 25"), "{out}");
        assert!(!out.contains("mul"), "{out}");
    }

    /// Every arm of the switch is taken while the counter it passes is still one. What the
    /// counter learns after that, going round the loop, still has to reach each arm's parameter
    /// even though the switch is not taken again.
    #[test]
    fn an_argument_that_changes_after_its_arm_was_taken_still_reaches_the_parameter() {
        let out = solved(
            r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 0
    jump block1(%1)
block1(%2: i32):
    %3 = iconst.i32 1
    %4 = add.i32 %2, %3
    switch %0, block2(%4), [1 => block2(%4), 2 => block3(%4)]
block2(%5: i32):
    %6 = icmp slt %5, %0
    br_if %6, block1(%5), block4(%5)
block3(%7: i32):
    jump block4(%7)
block4(%8: i32):
    return %8
}
"#,
        );
        assert!(out.contains("%4 = add %2, %3"), "{out}");
        assert!(out.contains("return %8"), "the counter is not a constant, {out}");
    }

    /// Known bits are facts short of a constant: whatever `x` is, `x & 12` has its low two bits
    /// clear, so testing them is testing a zero.
    #[test]
    fn the_known_bits_of_an_and_settle_a_test_of_the_bits_it_cleared() {
        let out = solved(
            r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 12
    %2 = and.i32 %0, %1
    %3 = iconst.i32 3
    %4 = and.i32 %2, %3
    return %4
}
"#,
        );
        assert!(out.contains("and %0, %1"), "the first and is not a constant, {out}");
        assert!(out.contains("%4 = iconst.i32 0"), "the second is, {out}");
    }

    /// `(long) (int) (x & 15)` is `x & 15`, since the four bits the mask leaves have the sign bit
    /// of the `int` clear, and the same goes for a zero extension. Without the mask the sign bit
    /// could be anything and the extension stays.
    #[test]
    fn an_extension_of_a_truncation_whose_dropped_bits_are_known_is_what_was_truncated() {
        let out = solved(
            r#"
func @f(i64) -> i64, linkage(external) {
block0(%0: i64):
    %1 = iconst.i64 15
    %2 = and.i64 %0, %1
    %3 = trunc.i32 %2
    %4 = sext.i64 %3
    %5 = zext.i64 %3
    %6 = add.i64 %4, %5
    %7 = trunc.i32 %0
    %8 = sext.i64 %7
    %9 = iconst.i64 2147483648
    %10 = and.i64 %0, %9
    %11 = trunc.i32 %10
    %12 = sext.i64 %11
    %13 = zext.i64 %11
    %14 = add.i64 %6, %8
    %15 = add.i64 %14, %12
    %16 = add.i64 %15, %13
    return %16
}
"#,
        );
        assert!(out.contains("%6 = add %2, %2"), "{out}");
        assert!(out.contains("%14 = add %6, %8"), "the unmasked one stays, {out}");
        assert!(out.contains("%15 = add %14, %12"), "bit 31 is the sign of the int, {out}");
        assert!(out.contains("%16 = add %15, %10"), "and is zero extended as it was, {out}");
    }

    /// A parameter that is a constant on every edge into its block is that constant, and one that
    /// differs between two taken edges is left alone.
    #[test]
    fn a_parameter_is_what_every_taken_edge_agrees_on() {
        let out = solved(
            r#"
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i32 4
    %2 = iconst.i32 6
    br_if %0, block1(%1, %1), block1(%1, %2)
block1(%3: i32, %4: i32):
    %5 = add.i32 %3, %4
    return %5
}
"#,
        );
        assert!(out.contains("add %"), "the second parameter is four or six, {out}");
        let add = out.lines().find(|line| line.contains("add %")).expect("the add is there");
        assert!(!add.contains("%3"), "the first parameter is a constant now, {out}");
    }

    /// Fuel stops the replacing and not the fixpoint, so what is replaced with a little fuel is a
    /// prefix of what is replaced with all of it and never a value the fixpoint had not finished.
    #[test]
    fn fuel_stops_the_replacing_and_not_the_fixpoint() {
        let none = run(LOOP, &mut Fuel::of(0));
        assert!(none.contains("icmp ne %3, %1"), "{none}");
        let one = run(LOOP, &mut Fuel::of(1));
        let all = solved(LOOP);
        assert_ne!(one, none, "one replacement was made");
        assert_ne!(one, all, "and only one");
    }

    /// A block only a dead edge reaches is left as it was, branch and all, since nothing the
    /// fixpoint says about it was ever checked against a path the program can take, and removing it
    /// is `prune`'s work.
    #[test]
    fn a_block_nothing_reaches_is_left_alone() {
        let out = solved(
            r#"
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i32 0
    %2 = icmp ne %1, %1
    br_if %2, block2(%0), block1
block1:
    %3 = iconst.i32 2
    return %3
block2(%4: i1):
    br_if %4, block3, block1
block3:
    %5 = iconst.i32 1
    return %5
}
"#,
        );
        assert!(out.contains("br_if %4"), "an unreachable branch is left as it was, {out}");
        assert!(out.contains("block2(%4: i1)"), "and so is its parameter, {out}");
    }

    /// A load through a pointer eight bytes into a local aligned to sixteen, with one byte of
    /// alignment on the access, which is what a packed or a `char *` access looks like.
    const INTO_LOCAL: &str = "
func @f() -> i64, linkage(external) {
block0:
    %0 = alloca, size 32, align 16
    %1 = iconst.i64 8
    %2 = ptr_add %0, %1
    %3 = load.i64 %2, align 1
    return %3
}
";

    #[test]
    fn an_access_into_an_aligned_local_takes_the_alignment_its_address_has() {
        let out = solved(INTO_LOCAL);
        assert!(out.contains("load.i64 %2, align 8"), "{out}");
    }

    #[test]
    fn an_odd_offset_proves_nothing() {
        let out = solved(&INTO_LOCAL.replace("iconst.i64 8", "iconst.i64 9"));
        assert!(out.contains("load.i64 %2, align 1"), "{out}");
    }

    #[test]
    fn an_address_that_came_in_as_an_argument_proves_nothing() {
        let out = solved(
            "
func @f(ptr) -> i64, linkage(external) {
block0(%0: ptr):
    %1 = load.i64 %0, align 1
    return %1
}
",
        );
        assert!(out.contains("load.i64 %0, align 1"), "{out}");
    }

    #[test]
    fn the_low_bits_of_an_aligned_local_are_known_as_an_integer() {
        let out = solved(
            "
func @f() -> i64, linkage(external) {
block0:
    %0 = alloca, size 8, align 8
    %1 = ptrtoint.i64 %0
    %2 = iconst.i64 7
    %3 = and %1, %2
    return %3
}
",
        );
        assert!(out.contains("%3 = iconst.i64 0"), "{out}");
    }

    #[test]
    fn a_pointer_is_never_a_constant_whatever_its_low_bits_are() {
        let out = solved(
            "
func @f() -> i64, linkage(external) {
block0:
    %0 = alloca, size 8, align 8
    %1 = ptrtoint.i64 %0
    return %1
}
",
        );
        assert!(out.contains("ptrtoint"), "{out}");
    }

    #[test]
    fn a_pointer_that_is_null_every_way_round_a_loop_is_null() {
        // Two separate nulls, one on the way in and one on the way round, which is what is left
        // of a pointer nothing in the loop assigns once the inliner has put a `NULL` on each path.
        let out = solved(
            "
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i64 0
    %2 = inttoptr.ptr %1
    jump block1(%2, %0)
block1(%3: ptr, %4: i32):
    %5 = iconst.i64 0
    %6 = inttoptr.ptr %5
    %7 = icmp ne %3, %6
    br_if %7, block2, block3
block2:
    %8 = load.i32 %3, align 4
    return %8
block3:
    %9 = iconst.i32 1
    %10 = sub.i32 %4, %9
    %11 = iconst.i64 0
    %12 = inttoptr.ptr %11
    jump block1(%12, %10)
}
",
        );
        assert!(!out.contains("icmp"), "the test of the pointer is known, {out}");
        assert!(out.contains("iconst.i1 0\n    br_if"), "{out}");
    }

    #[test]
    fn a_pointer_null_on_one_way_in_and_not_the_other_is_not_known() {
        let out = solved(
            "
func @f(i1, ptr) -> i32, linkage(external) {
block0(%0: i1, %1: ptr):
    %2 = iconst.i64 0
    %3 = inttoptr.ptr %2
    br_if %0, block1(%3), block1(%1)
block1(%4: ptr):
    %5 = icmp ne %4, %3
    %6 = zext.i32 %5
    return %6
}
",
        );
        assert!(out.contains("icmp ne %4"), "{out}");
    }

    #[test]
    fn a_parameter_that_joins_two_locals_is_as_aligned_as_the_lesser() {
        let out = solved(
            "
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = alloca, size 16, align 16
    %2 = alloca, size 16, align 4
    br_if %0, block1(%1), block1(%2)

block1(%3: ptr):
    %4 = load.i32 %3, align 1
    return %4
}
",
        );
        assert!(out.contains("load.i32 %3, align 4"), "{out}");
    }

    #[test]
    fn what_an_asm_goto_writes_is_not_known() {
        // The output comes from the asm, so the test on it after the join has to stay.
        let out = solved(
            r#"
func @mark(i64), linkage(external);

func @f(ptr, i64) -> i32, linkage(external) {
block0(%0: ptr, %1: i64):
    %2 = inline_asm.i64.volatile "1: movq %1,%0", "=r,m", ""(%0), labels [block1, block2]
block1:
    %3 = iconst.i64 10
    %4 = icmp uge %1, %3
    br_if %4, block3(%3), block3(%2)
block2:
    %5 = iconst.i32 0
    return %5
block3(%6: i64):
    %7 = iconst.i64 0
    %8 = icmp ne %6, %7
    br_if %8, block4, block5
block4:
    call @mark(%6) : (i64)
    jump block5
block5:
    %9 = iconst.i32 1
    return %9
}
"#,
        );
        assert!(out.contains("call @mark"), "{out}");
        assert!(out.contains("icmp ne"), "{out}");
    }

    #[test]
    fn fuel_stops_the_raising() {
        let out = run(INTO_LOCAL, &mut Fuel::of(0));
        assert!(out.contains("align 1"), "{out}");
    }
}
