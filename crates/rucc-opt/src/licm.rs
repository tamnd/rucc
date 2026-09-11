//! Moves a computation whose operands do not change in a loop to the block in front of it.
//!
//! Design: `spec/optimizer/27-licm.md`, with 27.1 for the legality answer, 27.2 for the cost, 27.5
//! for what is built and 27.6 for the ways it is wrong.
//!
//! The oldest loop optimization and the one whose interesting part is not the move. Taking an
//! instruction out of a block and putting it in another is a dozen lines. The three questions in
//! front of it are the pass.
//!
//! # Invariance is one walk, because the IR is in SSA
//!
//! A value does not change in a loop when what defines it is outside the loop, or when everything
//! it reads does not change. The second clause looks like a fixpoint and is not: a definition
//! dominates its uses, so walking the loop's blocks in reverse postorder reaches every definition
//! before every use of it, and one pass gives the transitive answer.
//!
//! Memory is the part section 27.5 says needs a fixpoint, and what this does instead is ask a
//! smaller question. The IR can thread memory through the instructions that touch it as an operand
//! of type `mem`, and where it does, a load whose memory operand is defined outside the loop is a
//! load nothing in the loop wrote before, which the same operand walk settles with nothing added.
//! Where it does not, and none of the pipelines this pass runs in do, a load has only its address
//! for an operand and an unchanging address says nothing at all about what is behind it.
//!
//! So a load in a loop that writes memory anywhere is left where it is. Not because it is not
//! invariant, but because nothing here can tell. That is coarse and it is honest, and the two ways
//! out of it are the same one: give the pass the memory chain, or give it the module so it can ask
//! [`crate::alias`]. A load in a loop that writes nothing is invariant on the address alone, and
//! that is most of the loops that read a global in the first place.
//!
//! # The three way answer, and why header copying comes first
//!
//! Section 27.1's enum: a store or a call may not move at all, pure arithmetic may move anywhere,
//! and in between are the instructions that are fine to move as long as they were going to run.
//! A load faults on a bad address and a division traps on a zero divisor, so moving one in front of
//! a loop that runs zero times is a program that crashes where the original returned.
//!
//! What settles it is [`crate::PostDominators`]: an instruction in a block the header cannot get
//! past without entering runs on every entry to the loop, so working it out in front of the loop is
//! working it out exactly when it was going to be worked out anyway. In an unrotated `while` the
//! only such block is the header itself. In the `do-while` that [`crate::header_copy`] leaves, it
//! is the whole body. That is section 27.1's point made concrete: **header copying is a
//! prerequisite for this pass being useful, not a separate nicety.**
//!
//! Post-dominance is only half of that, and the other half is what the instructions in front do.
//! A block the header cannot get past is still a block the program never arrives at if something
//! on the way stops it, and a call is the thing that stops it: a callee may exit, may loop forever
//! or may jump out, and what a call does comes from the module, which a pass holding one function
//! does not have. So a call ends the guarantee for everything behind it in the same turn round the
//! loop, and so does a trapping instruction, for the same reason from the other side. That is
//! `goes_on`, and `gcc.c-torture/execute/pr38819.c` is the program that says why: its loop body
//! calls a function that calls `exit` and then divides by zero, so a pass that asked only about
//! post-dominance would hoist the division and crash a program that returns.
//!
//! An infinite loop is the exception, and it is why the fake exits are consulted. Post-dominance
//! over a loop with no way out is answered against an edge document 06.8's analysis invented, so a
//! block that post-dominates the header there might still be one an infinite path avoids. This
//! declines those loops rather than believing an invented edge.
//!
//! # Where it puts things, and the two shapes it will not touch
//!
//! The preheader, in front of its terminator. That placement is always legal and the argument is
//! short: a value defined outside the loop dominates the header, the header's immediate dominator
//! is the preheader, so the definition dominates the preheader too. A loop without a preheader is
//! left alone, since there is nowhere to put anything, and section 26 owns making one.
//!
//! That is also the whole of the answer to section 27.6's irreducible region, which has several
//! ways in and therefore no preheader. It does not get that far here: [`crate::loops`] reports an
//! irreducible region separately from the natural loops and this pass is only handed the natural
//! ones, so a region with two entries is not a loop it can see rather than a loop it declines.
//!
//! The preheader is also the block [`speculate`] is asked about, rather than the block the
//! instruction is in, and the difference between those two is a miscompilation. A division under
//! `if (d)` has a divisor the ranges know is not zero, because a range is narrowed by the branches
//! that dominate the block it is asked about. Ask where the division is and the answer is that it
//! may go anywhere. Ask where it would go and the answer is that it may not, which is the true one,
//! since the guard that made it safe is not in front of the preheader.
//!
//! # The cost, which is a register rather than an instruction
//!
//! Moving a computation out of a loop is not free and section 27.2 is blunt about why: the value is
//! now live across the whole loop, and a loop that ran in registers and now spills is paying a load
//! and a store per iteration to save an add per iteration. So the question is not whether the
//! computation is expensive, it is whether it is more expensive than a register.
//!
//! The answer is document 40.6's pressure model, which is a count rather than an estimate, and
//! [`heuristics::LICM_EXPENSIVE`], which is GCC's line between the two. Where the loop already
//! holds as many values as the machine has registers, less document 40.6's margin, only the
//! genuinely expensive operations move and the rest stay where they are. Each move made in a loop
//! is one more value live across it, so the room left is counted down as the pass spends it.
//!
//! A constant and the address of a symbol are free, and a free value moves only as a passenger of
//! something that is not. That is arranged in `trim`, which is also where the reason it cannot
//! simply be refused up front is written down.
//!
//! # What this does not do yet
//!
//! Store motion, section 27.3, which turns a store to an unchanging address into a load in front of
//! the loop and a store after it. It needs an alias query against every memory access in the loop,
//! and an alias query needs the module, which a pass holding one function does not have. It is the
//! half with the risk and it should arrive with the measurement section 27.7 asks for.
//!
//! Hoisting a call, for the same reason from the other side: a call is safe to move when it is
//! `const`, and what a call is comes from the attributes on the callee, which live in the module.
//! [`crate::purity`] has the answer and nothing hands it to a pass.

use std::collections::HashSet;

use rucc_cost::heuristics;
use rucc_ir::{Block, Flags, Func, Inst, Opcode, Value};

use crate::cfg::Cfg;
use crate::dom::{Dominators, PostDominators};
use crate::live::Liveness;
use crate::loops::{LoopId, Loops};
use crate::machine::Machine;
use crate::pressure::{Class, Pressure, class_of};
use crate::range::query::Ranges;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats, speculate};

const HOISTED: &str = "computation moved in front of the loop, nothing in the loop changes it";
const SPECULATIVE: &str =
    "left in the loop, it does not run on every entry and working it out early could fault";
const EFFECTS: &str = "left in the loop, moving it would change what the program does";
const PRESSURE: &str = "left in the loop, it is cheaper than the register holding it would cost";
const MEMORY: &str = "left in the loop, the loop writes memory and nothing here says which memory";
const NO_PREHEADER: &str = "loop left as it was, it has not been canonicalized";
const SPINS: &str = "loop left as it was, it has no way out, so nothing in it is known to run";
const NO_FUEL: &str = "loop left as it was, the pass ran out of fuel";

/// Section 27.5's pass.
#[derive(Debug)]
pub struct Licm;

/// The one instance, which is what the pipelines name.
pub static LICM: Licm = Licm;

impl Pass for Licm {
    fn name(&self) -> &'static str {
        "licm"
    }

    fn describe(&self) -> &'static str {
        "moves a computation whose operands do not change in a loop in front of the loop"
    }

    fn preserves(&self) -> Preserved {
        // No edge moves and no block appears, so everything about the shape of the function is
        // what it was. What changes is where values are live, and that is not a side effect of
        // the transformation, it is the transformation.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let machine = an.machine();
        let cfg = an.cfg(func).clone();
        let loops = an.loops(func).clone();
        if loops.count() == 0 {
            return stats;
        }
        let dom = an.dominators(func).clone();
        let post = an.post_dominators(func).clone();
        let invented: HashSet<Block> = post.fake_exits().iter().copied().collect();

        // Innermost first, so a value hoisted out of an inner loop lands in the outer loop's body
        // and is looked at again on the outer loop's turn. That is what carries a computation all
        // the way out of a nest in one run rather than one level per run.
        let mut order: Vec<LoopId> = loops.all().collect();
        order.sort_by_key(|&id| std::cmp::Reverse(loops.depth(id)));

        let mut pressure = Pressure::of(func, &cfg, &Liveness::of(func, &cfg));
        for id in order {
            let job = Job {
                machine,
                cfg: &cfg,
                dom: &dom,
                post: &post,
                loops: &loops,
                invented: &invented,
            };
            if job.run(func, &pressure, id, fuel, &mut stats) {
                // The counts inside the loop just changed and the next loop out is about to be
                // asked what it holds. Recomputing is linear in the function and the alternative
                // is deciding the outer loop against a number the inner loop invalidated.
                pressure = Pressure::of(func, &cfg, &Liveness::of(func, &cfg));
            }
        }
        stats
    }
}

/// Section 27.1's three way legality answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Move {
    /// It may go anywhere its operands reach.
    Anywhere,
    /// It may go only where it was going to run anyway.
    IfItWasGoingToRun,
    /// It stays.
    Nowhere,
}

/// What one loop is being looked at against, gathered once so the walk below reads.
struct Job<'a> {
    machine: Machine,
    cfg: &'a Cfg,
    dom: &'a Dominators,
    post: &'a PostDominators,
    loops: &'a Loops,
    invented: &'a HashSet<Block>,
}

impl Job<'_> {
    /// Hoists what this loop will let go of, and says whether anything moved.
    fn run(
        &self,
        func: &mut Func,
        pressure: &Pressure,
        id: LoopId,
        fuel: &mut Fuel,
        stats: &mut Stats,
    ) -> bool {
        let Some(preheader) = self.loops.preheader(self.cfg, id) else {
            stats.missed(NO_PREHEADER);
            return false;
        };
        let Some(landing) = func.terminator(preheader) else {
            return false;
        };
        let plan = self.plan(func, pressure, id, preheader, fuel, stats);
        for inst in &plan {
            // Unlink and relink, in the order the plan was made, which is dominator order, so an
            // operand hoisted with its user arrives in front of it. Section 27.6 names the other
            // order as the way a chain comes out wrong.
            func.remove_inst(*inst);
            func.insert_before(*inst, landing);
            stats.optimized(HOISTED);
        }
        !plan.is_empty()
    }

    /// Which instructions of this loop are worth moving and legal to move, in the order to move
    /// them in.
    ///
    /// Separate from the moving because the range query holds the function and the moving needs it
    /// back. That is Rust noticing something real: deciding against a function while changing it is
    /// how a pass ends up reading an answer about a program that no longer exists.
    ///
    /// `preheader` is where everything in the plan is going, and it is passed in rather than worked
    /// out here because it is what the safety question is asked about. A fact that holds inside the
    /// loop is not a fact in front of it.
    fn plan(
        &self,
        func: &Func,
        pressure: &Pressure,
        id: LoopId,
        preheader: Block,
        fuel: &mut Fuel,
        stats: &mut Stats,
    ) -> Vec<Inst> {
        let header = self.loops.header(id);
        let inside: HashSet<Block> = self.loops.blocks(id).iter().copied().collect();
        // A loop with a way out has its post-dominance answered against edges the program has.
        // One without does not, so it is declined rather than decided on an invented edge.
        let spins = self.loops.blocks(id).iter().any(|block| self.invented.contains(block));
        if spins {
            stats.missed(SPINS);
        }
        // Asked once for the loop rather than once per load, because the answer is about the loop.
        let writes = self
            .loops
            .blocks(id)
            .iter()
            .any(|block| func.insts(*block).any(|inst| func[inst].opcode.writes_memory()));
        let ends = self
            .loops
            .blocks(id)
            .iter()
            .any(|block| func.insts(*block).any(|inst| ends_a_lifetime(func, inst)));
        let mut ranges = Ranges::new(func, self.cfg, self.dom);
        let mut plan = Vec::new();
        // The ones in the plan that are only in it because something after them might want them.
        // A cheap computation under pressure is worth moving when it is a link in a chain that
        // ends in something expensive and is not worth moving on its own, and which of those it
        // is cannot be known at the point it is read, because what reads it has not been read
        // yet. So it goes in provisionally and [`trim`] takes it out again, which is the same
        // answer a cost free instruction already gets and for the same reason.
        let mut passengers: HashSet<Inst> = HashSet::new();
        let mut moved: HashSet<Value> = HashSet::new();
        // Every value moved out is one more live across the loop, which is one register less to
        // decide the next one against. Taking it off the allocatable count says that once instead
        // of at each of the comparisons below, and it is per bank because a value moved into a
        // floating point register does not take an integer one.
        //
        // The two banks do not start at the same number, because the general purpose one gives up
        // the stack pointer and the frame pointer and the vector one gives up neither. A target
        // with no cost table answers nothing, and nothing here means no room at all rather than a
        // number invented on its behalf: the pressure test then refuses every hoist that is not
        // free, which is the same thing this pass does under real pressure.
        let mut room = [0; Class::COUNT];
        for class in Class::ALL {
            room[class.index()] = self.machine.allocatable(class).unwrap_or(0);
        }

        // Whether the program is still known to be on its way to what comes next. It starts true
        // at the header and goes false at the first instruction the program might not come back
        // from, and it never goes true again, because reverse postorder is the order one turn
        // round the loop runs its blocks in and a block seen later cannot run earlier.
        let mut reaching = !spins;

        for block in self.cfg.reverse_postorder() {
            if !inside.contains(&block) {
                continue;
            }
            let entered = self.post.post_dominates(block, header);
            for inst in func.insts(block) {
                // Both halves are needed and neither implies the other. The block being one the
                // header cannot get past says every entry to the loop arrives here. `reaching`
                // says nothing in front of it stops the program on the way.
                let runs = reaching && entered;
                if !goes_on(func, inst, &mut ranges, block) {
                    reaching = false;
                }
                if func.is_terminator(inst) {
                    continue;
                }
                let Some(result) = func[inst].results().next() else {
                    continue;
                };
                let Some(class) = class_of(func[result].ty) else {
                    continue;
                };
                if !self.unchanging(func, id, inst, &moved) {
                    continue;
                }
                // The address does not change, which is not the question. What is behind it is,
                // and asking that needs either the memory chain, which is not in this function, or
                // the module, which is not handed to a pass. Both are absent, so anything that
                // reads memory stays in a loop that writes any.
                //
                // A question about an allocation is not a question about what is in it, which is
                // why [`asks_the_plane`] is allowed past this. The loop still has to be one that
                // does not end a lifetime, and that is [`ends_a_lifetime`] rather than `writes`.
                let settled = asks_the_plane(func[inst].opcode) && !ends;
                if writes
                    && !settled
                    && func[inst].opcode.touches_memory()
                    && func.mem_in(inst).is_none()
                {
                    stats.missed(MEMORY);
                    continue;
                }
                let cost = cost(func, inst);
                match movement(speculate::why_not(func, inst, &mut ranges, preheader)) {
                    Move::Anywhere => (),
                    Move::IfItWasGoingToRun if runs => (),
                    Move::IfItWasGoingToRun => {
                        stats.missed(SPECULATIVE);
                        continue;
                    }
                    Move::Nowhere => {
                        stats.missed(EFFECTS);
                        continue;
                    }
                }
                let bank = class.index();
                // A free instruction is not asked to pay, because it is only in the plan as a
                // passenger and [`trim`] takes it out again if nothing else in the plan wanted it.
                if cost > 0 {
                    let tight = pressure.is_tight(self.loops, id, class, room[bank]);
                    if tight && cost < heuristics::LICM_EXPENSIVE {
                        passengers.insert(inst);
                    }
                    if !fuel.take() {
                        stats.missed(NO_FUEL);
                        return trim(func, plan, &passengers, stats);
                    }
                    room[bank] = room[bank].saturating_sub(1);
                }
                moved.extend(func[inst].results());
                plan.push(inst);
            }
        }
        trim(func, plan, &passengers, stats)
    }

    /// Whether nothing in the loop changes what this instruction reads.
    ///
    /// The memory operand is one of the operands, so a load of memory the loop wrote is answered
    /// here along with everything else and needs no separate walk.
    fn unchanging(&self, func: &Func, id: LoopId, inst: Inst, moved: &HashSet<Value>) -> bool {
        func[func[inst].args]
            .iter()
            .all(|arg| self.loops.is_invariant(func, id, *arg) || moved.contains(arg))
    }
}

/// Takes the instructions nothing else in the plan needed back out of it.
///
/// Two kinds ride along. A constant or the address of a symbol costs nothing to work out again, so
/// moving one out of a loop on its own buys nothing and costs a register held for the length of the
/// loop. It still has to be in the plan while the plan is being made, because a load of a global is
/// only invariant once the address it reads is going with it, and refusing the address up front
/// would refuse the load as well. The other kind is `passengers`, the ones a full loop would have
/// turned down on their own: a cheap computation under pressure is worth moving when something
/// expensive downstream is waiting on it and is not worth moving otherwise, and what reads it has
/// not been read yet at the point it is decided. Both come out here if nobody boarded behind them.
///
/// Backwards, because the plan is in dependency order and a passenger is wanted by something after
/// it. One walk answers the whole chain for the same reason the invariance walk does, and a chain
/// of ten cheap links ending in nothing comes out in that one walk rather than one link per run.
///
/// The pressure miss is counted here rather than where it is decided, because an instruction that
/// went on to carry an expensive one out of the loop was not left in the loop and reporting it as
/// missed would say the opposite of what happened.
fn trim(func: &Func, plan: Vec<Inst>, passengers: &HashSet<Inst>, stats: &mut Stats) -> Vec<Inst> {
    let mut wanted: HashSet<Value> = HashSet::new();
    let mut keep = Vec::with_capacity(plan.len());
    for inst in plan.into_iter().rev() {
        if !func[inst].results().any(|value| wanted.contains(&value)) {
            if cost(func, inst) == 0 {
                continue;
            }
            if passengers.contains(&inst) {
                stats.missed(PRESSURE);
                continue;
            }
        }
        wanted.extend(func[func[inst].args].iter().copied());
        keep.push(inst);
    }
    keep.reverse();
    keep
}

/// Section 27.1's enum, read off why the value may not be worked out early.
///
/// The reason matters rather than the opcode. A load whose address is proved good may go anywhere
/// and a volatile load may go nowhere, and both of them are loads.
fn movement(why: Option<&'static str>) -> Move {
    match why {
        None => Move::Anywhere,
        // The three that are only a problem on a run that was not going to reach them.
        Some(speculate::BY_ZERO | speculate::OVERFLOW | speculate::ADDRESS) => {
            Move::IfItWasGoingToRun
        }
        Some(_) => Move::Nowhere,
    }
}

/// Whether the program, having started this instruction, is certain to go on to the next one.
///
/// Post-dominance answers a question about the shape of the function and this answers the other
/// half, which is about what the instructions in front do. A block the header cannot get past is
/// still a block the program never arrives at if something on the way stops it, and there are two
/// ways to stop it. One is a call, since a callee may exit, may loop forever or may jump out, and
/// what a call does comes from the module, which a pass holding one function does not have, so
/// every call is one that might not come back. The other is an instruction that traps, which is
/// exactly the instruction this pass is careful about moving, read here at the block it is in
/// rather than at the preheader because the question is whether it traps where it stands.
///
/// `gcc.c-torture/execute/pr38819.c` is the program that says why. Its loop body calls a function
/// that calls `exit` and then divides by zero, and the division is invariant, so a pass that asked
/// only whether the body post-dominates the header would work it out in front of the loop and
/// crash a program that returns.
fn goes_on(func: &Func, inst: Inst, ranges: &mut Ranges<'_>, at: Block) -> bool {
    match func[inst].opcode {
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => false,
        // A promise that control does not get here, so nothing after it runs either.
        Opcode::UnreachableHint => false,
        Opcode::SDiv | Opcode::SRem | Opcode::UDiv | Opcode::URem | Opcode::Load => {
            movement(speculate::why_not(func, inst, ranges, at)) != Move::IfItWasGoingToRun
        }
        _ => true,
    }
}

/// Whether this asks the allocator about an object rather than reading what is in one.
///
/// `cap_extent` and `cap_extent_back` read the planes and nothing else does. What they answer is
/// how much room there is from a pointer to the end of whatever holds it, and a store through a
/// pointer does not change that, so the loop writing memory is not the question for them the way it
/// is for a load. What is the question is whether the loop ends the allocation, which
/// [`ends_a_lifetime`] answers.
///
/// This is not an exception to the rule above it so much as the rule being asked about the right
/// memory. The comment there says the pass would need a memory chain or the module to know what is
/// behind an address, and for these two it needs neither: `spec/safe-memory/05-representation.md`
/// puts the planes somewhere the program cannot reach and section 6.2.4 calls the checks
/// `readonly`, so the set of things that can change the answer is small enough to list.
const fn asks_the_plane(opcode: Opcode) -> bool {
    matches!(opcode, Opcode::CapExtent | Opcode::CapExtentBack)
}

/// Whether this could end the lifetime of something a plane holds a row for.
///
/// The same list `crate::split` refuses a loop for, and for the same reason: a call that might free
/// changes what the planes say, and so does assembly nobody can read and the two instructions that
/// end a lifetime by saying so. A call the module summary calls `nofree` is not one of them, which
/// is what makes this worth asking at all, since a loop with a `memcpy` in it is still a loop whose
/// allocations stay where they are.
fn ends_a_lifetime(func: &Func, inst: Inst) -> bool {
    match func[inst].opcode {
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => {
            !func[inst].flags.contains(Flags::NOFREE)
        }
        Opcode::InlineAsm | Opcode::MetaEnd | Opcode::MetaTransfer => true,
        _ => false,
    }
}

/// Section 27.2's table, which GCC's `stmt_cost` opens by admitting is ad hoc.
///
/// The numbers are not prices. They sort instructions into three groups: the ones there is no point
/// moving, the ones worth moving when there is room, and the ones worth moving even when there is
/// not. What makes the table worth copying rather than inventing is the reasoning behind two of its
/// entries. A conditional is expensive here because moving it in front of the loop is what lets
/// document 30 split the loop on it, so the cost model is encoding a pass interaction rather than a
/// price. Anything touching memory is expensive because, as GCC puts it, hoisting memory references
/// out should almost surely be a win.
fn cost(func: &Func, inst: Inst) -> u32 {
    match func[inst].opcode {
        // Worked out again wherever it is wanted, so there is nothing to move. The address of a
        // symbol is in here with the constants because that is what it is on the only target there
        // is: one instruction reading the program counter and a link time constant, with no
        // operands, so a copy of it costs what recomputing it costs and holding one across a loop
        // costs a register for nothing.
        Opcode::IConst | Opcode::FConst | Opcode::GlobalAddr | Opcode::BlockAddr => 0,
        // `crate::pass` never sees one of these reach the back end: `rucc_safety::lower` removes
        // every `cap_of` once the checks that read it have become calls. So it holds no register
        // and a copy of it costs nothing, which is what the four above have in common.
        Opcode::CapOf => 0,
        Opcode::Load
        | Opcode::Select
        | Opcode::Call
        | Opcode::CallIndirect
        | Opcode::Mul
        | Opcode::SDiv
        | Opcode::UDiv
        | Opcode::SRem
        | Opcode::URem
        | Opcode::FMul
        | Opcode::FDiv
        | Opcode::FRem
        | Opcode::Shl
        | Opcode::LShr
        | Opcode::AShr
        | Opcode::ICmp
        | Opcode::FCmp => heuristics::LICM_EXPENSIVE,
        // Both become a call to the runtime in `rucc_safety::lower`, and the census in
        // `spec/safe-memory/13-performance.md` section 13.1 measured one at about 377 instructions.
        // Left at the default they price as an add, and then the pressure test throws them out of
        // exactly the loops where they cost the most.
        Opcode::CapExtent | Opcode::CapExtentBack => heuristics::LICM_EXPENSIVE,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::{Interner, Symbol};
    use rucc_ir::{
        Block, Builder, Def, Extra, Flags, Func, Global, Inst, InstData, IntPred, MemInfo,
        MemOrder, Module, Opcode, Restrict, Signature, Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{
        EFFECTS, HOISTED, LICM, MEMORY, NO_FUEL, NO_PREHEADER, PRESSURE, SPECULATIVE, SPINS,
    };
    use crate::canon::Canon;
    use crate::header_copy::SPEED;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// Runs the pass over the function as it stands.
    fn hoist(func: &mut Func, fuel: &mut Fuel) -> Stats {
        LICM.run(func, &mut crate::machine::fixtures::analyses(), fuel)
    }

    /// Insists the function is one the rest of the compiler may believe.
    ///
    /// Moving a definition is the edit that breaks a definition's dominance over its uses, so this
    /// is where most of the strength of these tests is.
    fn sound(func: &Func, names: &mut Interner) {
        checked(func, names, &[]);
    }

    /// The same, in a module that declares those globals.
    fn checked(func: &Func, names: &mut Interner, globals: &[Symbol]) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        for name in globals {
            module.add_global(Global::new(*name, 16, 8));
        }
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    /// The instruction that worked that value out.
    fn made(func: &Func, value: Value) -> Inst {
        match func[value].def {
            Def::Result { inst, .. } => inst,
            other => panic!("{other:?} is not something an instruction worked out"),
        }
    }

    /// Which block that value is worked out in now.
    fn lives_in(func: &Func, value: Value) -> Block {
        func.block_of(made(func, value)).expect("it is in a block")
    }

    /// Where in its block that value is worked out, counting from the top.
    fn position(func: &Func, value: Value) -> usize {
        let inst = made(func, value);
        let block = func.block_of(inst).expect("it is in a block");
        func.insts(block).position(|other| other == inst).expect("it is in that block")
    }

    /// Moves whatever the caller appended after a block's terminator to in front of it.
    ///
    /// A builder appends, and a block that already ends in a jump has nowhere to append to that is
    /// legal. Writing the loop first and the body second reads better than the other order, so the
    /// tests do that and this puts the instructions back where they belong, in the order they were
    /// written in.
    fn tucked(func: &mut Func, block: Block) {
        let term = func
            .insts(block)
            .find(|inst| func.is_terminator(*inst))
            .expect("the block ends in something");
        let stragglers: Vec<Inst> =
            func.insts(block).skip_while(|inst| *inst != term).skip(1).collect();
        for inst in stragglers {
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
    }

    /// A memory record of that many bytes.
    fn record(size: u64) -> MemInfo {
        MemInfo { size, align: 8, order: MemOrder::NotAtomic, tbaa: None, restrict: Restrict::NONE }
    }

    /// A counted loop that tests at the top, which is what `while (i < n)` lowers to.
    ///
    /// ```text
    /// entry: jump head(0)
    /// head(i): t = i < n; br t -> body, done
    /// body: next = i + 1; jump head(next)
    /// done: ret i + the spares
    /// ```
    ///
    /// The spare parameters are added up after the loop and used nowhere else, which is how a test
    /// makes the loop hold values without putting anything in it. The pointer is there because the
    /// only address the function cannot say anything about is one it was handed.
    struct Counted {
        names: Interner,
        func: Func,
        entry: Block,
        head: Block,
        body: Block,
        limit: Value,
        pointer: Value,
    }

    fn counted(spare: usize) -> Counted {
        let mut names = Interner::new();
        let mut types = vec![Type::int(32); spare + 1];
        types.push(Type::PTR);
        let signature = Signature::new().with_params(&types).with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let handed: Vec<Value> =
            types.iter().map(|ty| func.append_param(entry, *ty)).collect::<Vec<_>>();
        let limit = handed[0];
        let pointer = *handed.last().expect("the pointer is the last of them");
        let i = func.append_param(head, Type::int(32));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);
        let test = Builder::new(&mut func, head).icmp(IntPred::Slt, i, limit);
        Builder::new(&mut func, head).br_if(test, body, &[], done, &[]);
        let one = Builder::new(&mut func, body).iconst(Type::int(32), 1);
        let next = Builder::new(&mut func, body).binary(Opcode::Add, i, one, Flags::NONE);
        Builder::new(&mut func, body).jump(head, &[next]);
        let mut build = Builder::new(&mut func, done);
        let mut total = i;
        for value in &handed[1..=spare] {
            total = build.binary(Opcode::Add, total, *value, Flags::NONE);
        }
        build.ret(&[total]);
        Counted { names, func, entry, head, body, limit, pointer }
    }

    #[test]
    fn an_invariant_computation_moves_in_front_of_the_loop() {
        let mut it = counted(0);
        let product = Builder::new(&mut it.func, it.body).binary(
            Opcode::Mul,
            it.limit,
            it.limit,
            Flags::NONE,
        );
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);
        assert_eq!(lives_in(&it.func, product), it.entry, "it is in front of the loop now");
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_computation_the_loop_changes_stays_where_it_is() {
        let mut it = counted(0);
        let i = it.func[it.head].params[0];
        let square = Builder::new(&mut it.func, it.body).binary(Opcode::Mul, i, i, Flags::NONE);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 0);
        assert_eq!(lives_in(&it.func, square), it.body);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_loop_with_nothing_invariant_in_it_is_left_alone() {
        // The counter, the constant one and the comparison are the whole of the loop, and the
        // constant is the case the cost table gives nothing to, since it is worked out again
        // wherever it is wanted.
        let mut it = counted(0);
        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 0);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_load_the_loop_might_not_reach_stays_where_it_is() {
        let mut it = counted(0);
        let read = Builder::new(&mut it.func, it.body).load(
            Type::int(32),
            it.pointer,
            record(4),
            Flags::NONE,
        );
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 0);
        assert_eq!(stats.count(Kind::Missed, SPECULATIVE), 1);
        assert_eq!(lives_in(&it.func, read), it.body, "the loop may run zero times");
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_same_load_moves_once_the_loop_tests_at_the_bottom() {
        // Section 27.1's claim that header copying is a prerequisite rather than a nicety, run as
        // a test. Nothing about the load changed. What changed is that the body now runs on every
        // entry to the loop, so working it out in front is working it out when it was going to be.
        // The three passes in front of it are the order the pipeline runs them in, and the second
        // canonicalization is not spare: the copy leaves the rotated loop entered from a block
        // that also leaves it, and making a preheader out of that is what canonicalization does.
        let mut it = counted(0);
        let read = Builder::new(&mut it.func, it.body).load(
            Type::int(32),
            it.pointer,
            record(4),
            Flags::NONE,
        );
        tucked(&mut it.func, it.body);
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut it.func, &mut an, &mut Fuel::unlimited());
        SPEED.run(&mut it.func, &mut an, &mut Fuel::unlimited());
        Canon.run(&mut it.func, &mut an, &mut Fuel::unlimited());

        let stats = LICM.run(&mut it.func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);
        assert_ne!(lives_in(&it.func, read), it.body, "it left the body");
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn something_that_could_trap_moves_when_it_runs_on_every_entry() {
        // The header of an unrotated loop is the one block that does, which is why this is the
        // only hoist an uncanonicalized `while` gets out of the pass.
        let mut it = counted(0);
        let share = Builder::new(&mut it.func, it.head).binary(
            Opcode::SDiv,
            it.limit,
            it.limit,
            Flags::NONE,
        );
        tucked(&mut it.func, it.head);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);
        assert_eq!(lives_in(&it.func, share), it.entry);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn something_that_could_trap_stays_behind_a_call_that_might_not_come_back() {
        // The shape of `gcc.c-torture/execute/pr38819.c`, which is what found this. The head runs
        // on every entry to the loop and the division is invariant, and neither of those is the
        // question. The call in front of it may exit, so the division is not something the program
        // was going to work out, and hoisting it crashes a program that returns.
        let mut it = counted(0);
        let callee = it.names.intern("g");
        let mut build = Builder::new(&mut it.func, it.head);
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        let share = Builder::new(&mut it.func, it.head).binary(
            Opcode::SDiv,
            it.limit,
            it.limit,
            Flags::NONE,
        );
        tucked(&mut it.func, it.head);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, SPECULATIVE), 1);
        assert_eq!(lives_in(&it.func, share), it.head, "the call in front is what keeps it there");
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_same_division_moves_when_the_call_is_behind_it() {
        // The other half of the rule, and the reason it is not a count of the calls in the loop.
        // A call after the division says nothing about whether the division ran.
        let mut it = counted(0);
        let callee = it.names.intern("g");
        let share = Builder::new(&mut it.func, it.head).binary(
            Opcode::SDiv,
            it.limit,
            it.limit,
            Flags::NONE,
        );
        let mut build = Builder::new(&mut it.func, it.head);
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        tucked(&mut it.func, it.head);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);
        assert_eq!(lives_in(&it.func, share), it.entry);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_call_in_one_block_keeps_something_in_a_later_one_where_it_is() {
        // The flag has to outlive the block it went false in, because the blocks of one turn round
        // the loop run in the order this walks them and the call is still in front of everything
        // behind it. A `do-while` written out by hand, so that both blocks of the loop post-dominate
        // its header and the division would be a hoist this pass makes if it looked at that alone.
        let mut names = Interner::new();
        let signature =
            Signature::new().with_params(&[Type::int(32)]).with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let callee = names.intern("g");
        let entry = func.create_block();
        let head = func.create_block();
        let rest = func.create_block();
        let done = func.create_block();
        let limit = func.append_param(entry, Type::int(32));
        let i = func.append_param(head, Type::int(32));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);
        let mut build = Builder::new(&mut func, head);
        let taken = build.func().add_signature(Signature::new());
        build.call(callee, taken, &[]);
        Builder::new(&mut func, head).jump(rest, &[]);
        let share = Builder::new(&mut func, rest).binary(Opcode::SDiv, limit, limit, Flags::NONE);
        let one = Builder::new(&mut func, rest).iconst(Type::int(32), 1);
        let next = Builder::new(&mut func, rest).binary(Opcode::Add, i, one, Flags::NONE);
        let test = Builder::new(&mut func, rest).icmp(IntPred::Slt, next, limit);
        Builder::new(&mut func, rest).br_if(test, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[i]);

        let stats = hoist(&mut func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, SPECULATIVE), 1);
        assert_eq!(lives_in(&func, share), rest, "the call is in front of it in the same turn");
        sound(&func, &mut names);
    }

    #[test]
    fn a_division_a_test_inside_the_loop_made_safe_stays_inside_that_test() {
        // The one that looks safe and is not. Inside `if (limit)` the ranges know the divisor is
        // not zero, so asking about the division where it stands gets a yes. The preheader is not
        // inside that test and the same question there gets a no, which is the question the pass
        // has to be asking, because the preheader is where the answer would be used.
        let mut names = Interner::new();
        let signature =
            Signature::new().with_params(&[Type::int(32)]).with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let safe = func.create_block();
        let latch = func.create_block();
        let done = func.create_block();
        let limit = func.append_param(entry, Type::int(32));
        let i = func.append_param(head, Type::int(32));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);
        let test = Builder::new(&mut func, head).icmp(IntPred::Slt, i, limit);
        Builder::new(&mut func, head).br_if(test, body, &[], done, &[]);
        let guard = Builder::new(&mut func, body).icmp(IntPred::Ne, limit, zero);
        Builder::new(&mut func, body).br_if(guard, safe, &[], latch, &[]);
        let share = Builder::new(&mut func, safe).binary(Opcode::SDiv, limit, limit, Flags::NONE);
        Builder::new(&mut func, safe).jump(latch, &[]);
        let one = Builder::new(&mut func, latch).iconst(Type::int(32), 1);
        let next = Builder::new(&mut func, latch).binary(Opcode::Add, i, one, Flags::NONE);
        Builder::new(&mut func, latch).jump(head, &[next]);
        Builder::new(&mut func, done).ret(&[i]);

        let stats = hoist(&mut func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, SPECULATIVE), 1);
        assert_eq!(lives_in(&func, share), safe, "the guard is what made it safe");
        // The test itself is invariant and does come out, which is worth asserting because it is
        // the difference between the pass declining this division and the pass declining the loop.
        assert_eq!(lives_in(&func, guard), entry);
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);
        sound(&func, &mut names);
    }

    #[test]
    fn the_same_division_in_the_body_stays() {
        let mut it = counted(0);
        let share = Builder::new(&mut it.func, it.body).binary(
            Opcode::SDiv,
            it.limit,
            it.limit,
            Flags::NONE,
        );
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, SPECULATIVE), 1);
        assert_eq!(lives_in(&it.func, share), it.body, "the divisor could be zero");
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_volatile_load_stays_even_where_it_runs_on_every_entry() {
        let mut it = counted(0);
        let read = Builder::new(&mut it.func, it.head).load(
            Type::int(32),
            it.pointer,
            record(4),
            Flags::VOLATILE,
        );
        tucked(&mut it.func, it.head);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, EFFECTS), 1);
        assert_eq!(lives_in(&it.func, read), it.head, "one access per iteration is the point");
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_chain_comes_out_in_the_order_it_was_written_in() {
        // Section 27.6's fourth way of getting it wrong. The sum reads the product, so the product
        // has to arrive in front of it and not merely arrive.
        let mut it = counted(0);
        let mut build = Builder::new(&mut it.func, it.body);
        let product = build.binary(Opcode::Mul, it.limit, it.limit, Flags::NONE);
        let sum = build.binary(Opcode::Mul, product, it.limit, Flags::NONE);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 2);
        assert_eq!(lives_in(&it.func, product), it.entry);
        assert_eq!(lives_in(&it.func, sum), it.entry);
        assert!(position(&it.func, product) < position(&it.func, sum));
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_pass_stops_where_the_fuel_runs_out() {
        let mut it = counted(0);
        let mut build = Builder::new(&mut it.func, it.body);
        let product = build.binary(Opcode::Mul, it.limit, it.limit, Flags::NONE);
        build.binary(Opcode::Mul, product, it.limit, Flags::NONE);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::of(1));
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);
        assert_eq!(stats.count(Kind::Missed, NO_FUEL), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_cheap_computation_stays_where_the_loop_is_already_full() {
        // Fourteen values arriving and nothing in the loop to spare, so section 27.2's line is
        // what decides. The add is cheaper than the register it would want and the multiply is
        // not.
        let mut it = counted(14);
        let mut build = Builder::new(&mut it.func, it.body);
        let sum = build.binary(Opcode::Add, it.limit, it.limit, Flags::NONE);
        let product = build.binary(Opcode::Mul, it.limit, it.limit, Flags::NONE);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, PRESSURE), 1);
        assert_eq!(lives_in(&it.func, sum), it.body);
        assert_eq!(lives_in(&it.func, product), it.entry);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_cheap_link_moves_when_it_is_carrying_an_expensive_one_out() {
        // The same full loop and the same add, with the multiply now reading it. On its own the
        // add is not worth a register and the test above is that. Here it is the only thing
        // between the multiply and the front of the loop, and refusing it refuses the multiply
        // too, silently, because an instruction whose operand stayed behind is not invariant.
        let mut it = counted(14);
        let mut build = Builder::new(&mut it.func, it.body);
        let sum = build.binary(Opcode::Add, it.limit, it.limit, Flags::NONE);
        let product = build.binary(Opcode::Mul, sum, it.limit, Flags::NONE);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, PRESSURE), 0);
        assert_eq!(lives_in(&it.func, sum), it.entry, "it is carrying the multiply");
        assert_eq!(lives_in(&it.func, product), it.entry);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_chain_of_cheap_links_that_carries_nothing_stays_where_it_is() {
        // Every link rides along while the plan is being made, since what reads it has not been
        // read yet, and the whole chain comes back out in one walk when the end of it turns out
        // to be nothing. Two links, so the walk has to answer the second one before the first.
        let mut it = counted(14);
        let mut build = Builder::new(&mut it.func, it.body);
        let first = build.binary(Opcode::Add, it.limit, it.limit, Flags::NONE);
        let second = build.binary(Opcode::Add, first, it.limit, Flags::NONE);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, PRESSURE), 2);
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 0);
        assert_eq!(lives_in(&it.func, first), it.body);
        assert_eq!(lives_in(&it.func, second), it.body);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_same_add_moves_when_the_loop_has_room() {
        let mut it = counted(0);
        let sum = Builder::new(&mut it.func, it.body).binary(
            Opcode::Add,
            it.limit,
            it.limit,
            Flags::NONE,
        );
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, PRESSURE), 0);
        assert_eq!(lives_in(&it.func, sum), it.entry);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_loop_with_two_ways_in_is_left_alone() {
        // No preheader means nowhere to put anything, and section 26 owns making one.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::I1, Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let low = func.create_block();
        let high = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let either = func.append_param(entry, Type::I1);
        let n = func.append_param(entry, Type::int(32));
        let i = func.append_param(head, Type::int(32));
        Builder::new(&mut func, entry).br_if(either, low, &[], high, &[]);
        let zero = Builder::new(&mut func, low).iconst(Type::int(32), 0);
        Builder::new(&mut func, low).jump(head, &[zero]);
        let one = Builder::new(&mut func, high).iconst(Type::int(32), 1);
        Builder::new(&mut func, high).jump(head, &[one]);
        let test = Builder::new(&mut func, head).icmp(IntPred::Slt, i, n);
        Builder::new(&mut func, head).br_if(test, body, &[], done, &[]);
        let product = Builder::new(&mut func, body).binary(Opcode::Mul, n, n, Flags::NONE);
        let next = Builder::new(&mut func, body).binary(Opcode::Add, i, product, Flags::NONE);
        Builder::new(&mut func, body).jump(head, &[next]);
        Builder::new(&mut func, done).ret(&[]);

        let stats = hoist(&mut func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, NO_PREHEADER), 1);
        assert_eq!(lives_in(&func, product), body);
        sound(&func, &mut names);
    }

    #[test]
    fn a_loop_with_no_way_out_gets_the_pure_hoist_and_not_the_other_one() {
        // Post-dominance in here is answered against an edge document 06.8's analysis invented, so
        // nothing in the loop counts as running and only what may move anywhere moves.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32), Type::PTR]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let n = func.append_param(entry, Type::int(32));
        let pointer = func.append_param(entry, Type::PTR);
        Builder::new(&mut func, entry).jump(head, &[]);
        let mut build = Builder::new(&mut func, head);
        let product = build.binary(Opcode::Mul, n, n, Flags::NONE);
        let read = build.load(Type::int(32), pointer, record(4), Flags::NONE);
        build.jump(head, &[]);

        let stats = hoist(&mut func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, SPINS), 1);
        assert_eq!(stats.count(Kind::Missed, SPECULATIVE), 1);
        assert_eq!(lives_in(&func, product), entry, "arithmetic is safe anywhere");
        assert_eq!(lives_in(&func, read), head, "the address is still one nobody has vouched for");
        sound(&func, &mut names);
    }

    #[test]
    fn an_invariant_comes_all_the_way_out_of_a_nest_in_one_run() {
        // Innermost first, so the inner loop leaves the product in the outer loop's preheader,
        // which is a block of the outer loop, and the outer loop's turn takes it the rest of the
        // way.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let outer = func.create_block();
        let ready = func.create_block();
        let inner = func.create_block();
        let deep = func.create_block();
        let latch = func.create_block();
        let done = func.create_block();
        let n = func.append_param(entry, Type::int(32));
        let i = func.append_param(outer, Type::int(32));
        let j = func.append_param(inner, Type::int(32));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(outer, &[zero]);
        let outer_test = Builder::new(&mut func, outer).icmp(IntPred::Slt, i, n);
        Builder::new(&mut func, outer).br_if(outer_test, ready, &[], done, &[]);
        let start = Builder::new(&mut func, ready).iconst(Type::int(32), 0);
        Builder::new(&mut func, ready).jump(inner, &[start]);
        let inner_test = Builder::new(&mut func, inner).icmp(IntPred::Slt, j, n);
        Builder::new(&mut func, inner).br_if(inner_test, deep, &[], latch, &[]);
        let mut build = Builder::new(&mut func, deep);
        let product = build.binary(Opcode::Mul, n, n, Flags::NONE);
        let one = build.iconst(Type::int(32), 1);
        let next_j = build.binary(Opcode::Add, j, one, Flags::NONE);
        build.jump(inner, &[next_j]);
        let mut build = Builder::new(&mut func, latch);
        let step = build.iconst(Type::int(32), 1);
        let next_i = build.binary(Opcode::Add, i, step, Flags::NONE);
        build.jump(outer, &[next_i]);
        Builder::new(&mut func, done).ret(&[]);

        let stats = hoist(&mut func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 2, "one level and then the other");
        assert_eq!(lives_in(&func, product), entry);
        sound(&func, &mut names);
    }

    #[test]
    fn a_function_with_no_loop_in_it_is_untouched() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        Builder::new(&mut func, entry).ret(&[]);

        let stats = hoist(&mut func, &mut Fuel::unlimited());
        assert!(!stats.changed());
        sound(&func, &mut names);
    }

    #[test]
    fn the_address_of_a_global_moves_only_when_something_that_reads_it_moves() {
        // The load is what is worth hoisting and the address is free, so the address goes with it
        // and would have gone nowhere on its own. Getting this wrong in the other direction is
        // what refusing a free value up front does: the load reads an address defined in the loop,
        // so refusing the address makes the load look like something the loop changes.
        let mut it = counted(0);
        let grid = it.names.intern("grid");
        let mut build = Builder::new(&mut it.func, it.head);
        let at = build.value(
            InstData { extra: Extra::Symbol(grid), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        );
        let read = build.load(Type::int(32), at, record(4), Flags::NONE);
        tucked(&mut it.func, it.head);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 2, "the load and its address");
        assert_eq!(lives_in(&it.func, at), it.entry);
        assert_eq!(lives_in(&it.func, read), it.entry);
        assert!(position(&it.func, at) < position(&it.func, read));
        checked(&it.func, &mut it.names, &[grid]);
    }

    #[test]
    fn the_address_of_a_global_on_its_own_stays_where_it_is() {
        // Nothing to carry, so it is a register held for the length of the loop to save an
        // instruction that costs what a copy of it costs.
        let mut it = counted(0);
        let grid = it.names.intern("grid");
        let at = Builder::new(&mut it.func, it.body).value(
            InstData { extra: Extra::Symbol(grid), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        );
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 0);
        assert_eq!(lives_in(&it.func, at), it.body);
        checked(&it.func, &mut it.names, &[grid]);
    }

    /// An alloca is here so the load below has an address the function can vouch for.
    #[test]
    fn the_same_load_stays_once_the_loop_writes_anything_at_all() {
        // The address is the same address and the storage is the same four bytes, and the store
        // is to somewhere else entirely. It does not matter: this function does not carry the
        // memory chain, so there is nothing to read that says the store and the load are apart,
        // and a load in a loop that writes is a load that stays. Coarse on purpose, and the
        // remark says which of the reasons it was rather than leaving it to be guessed at.
        let mut it = counted(0);
        let mem = it.func.add_mem(record(4));
        let slot = Builder::new(&mut it.func, it.entry)
            .value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR);
        tucked(&mut it.func, it.entry);
        let mut build = Builder::new(&mut it.func, it.body);
        let read = build.load(Type::int(32), slot, record(4), Flags::NONE);
        build.store(read, it.pointer, record(4), Flags::NONE);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, MEMORY), 1);
        assert_eq!(lives_in(&it.func, read), it.body);
        sound(&it.func, &mut it.names);
    }

    /// Builds `cap_extent` of the function's pointer in `block`, with the `cap_of` it reads.
    fn extent(func: &mut Func, block: Block, pointer: Value) -> Value {
        let mut build = Builder::new(func, block);
        let want = build.iconst(Type::int(64), i128::from(i64::MAX));
        let args = build.func().push_values(&[pointer]);
        let of = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = build.func().push_values(&[of, pointer, want]);
        build.value(InstData { args, ..InstData::new(Opcode::CapExtent) }, Type::int(64))
    }

    #[test]
    fn asking_how_big_an_object_is_moves_out_of_a_loop_that_writes_to_it() {
        // The loop writes through the very pointer being asked about, and the answer is the same
        // every time round all the same: how much room is left from a pointer is a fact about the
        // allocation rather than about what is in it. A load here would stay, and the test above
        // is that load.
        let mut it = counted(0);
        let asked = extent(&mut it.func, it.body, it.pointer);
        let mut build = Builder::new(&mut it.func, it.body);
        let byte = build.iconst(Type::int(32), 0);
        build.store(byte, it.pointer, record(4), Flags::NONE);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, MEMORY), 0);
        assert_eq!(lives_in(&it.func, asked), it.entry, "it is in front of the loop now");
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn asking_how_big_an_object_is_stays_in_a_loop_that_calls_something_that_could_free() {
        // A call the module has nothing to say about could be `free`, and then the answer before
        // the call and the answer after it are different numbers. `crate::split` refuses a loop
        // for the same call and says so in the same words.
        let mut it = counted(0);
        let asked = extent(&mut it.func, it.body, it.pointer);
        let signature = it.func.add_signature(Signature::new().with_params(&[Type::PTR]));
        let callee = it.names.intern("might_free");
        Builder::new(&mut it.func, it.body).call(callee, signature, &[it.pointer]);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, MEMORY), 1);
        assert_eq!(lives_in(&it.func, asked), it.body);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn asking_how_big_an_object_is_moves_past_a_call_the_summary_says_cannot_free() {
        // The same loop with the same call, marked `nofree` by `crate::nofree`. That flag is the
        // whole difference between this test and the one above it, and a loop with a `memcpy` in
        // it is the shape it is about.
        let mut it = counted(0);
        let asked = extent(&mut it.func, it.body, it.pointer);
        let signature = it.func.add_signature(Signature::new().with_params(&[Type::PTR]));
        let callee = it.names.intern("cannot_free");
        let call = Builder::new(&mut it.func, it.body).call(callee, signature, &[it.pointer]);
        it.func[call].flags |= Flags::NOFREE;
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, MEMORY), 0);
        assert_eq!(lives_in(&it.func, asked), it.entry, "it is in front of the loop now");
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_load_of_a_local_the_loop_does_not_write_moves_out_of_the_body() {
        let mut it = counted(0);
        let mem = it.func.add_mem(record(4));
        let slot = Builder::new(&mut it.func, it.entry)
            .value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR);
        tucked(&mut it.func, it.entry);
        let read =
            Builder::new(&mut it.func, it.body).load(Type::int(32), slot, record(4), Flags::NONE);
        tucked(&mut it.func, it.body);

        let stats = hoist(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);
        assert_eq!(lives_in(&it.func, read), it.entry, "four bytes of four are always there");
        sound(&it.func, &mut it.names);
    }
}
