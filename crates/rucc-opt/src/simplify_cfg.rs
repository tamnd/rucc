//! Control flow simplification: unreachable blocks go, a branch that only ever goes one way
//! becomes a jump, and a block with one way in is folded into the block above it.
//!
//! Design: `spec/optimizer/21-cfg-simplification.md`, and section 6.5 of
//! `spec/optimizer/06-cfg-and-dominators.md`, which states the rule for the whole optimizer, that
//! a block the entry does not reach is invisible to every analysis and is deleted here rather than
//! by whichever pass happened to notice it.
//!
//! # The order
//!
//! Section 21.4, and it is an order rather than a loop. Unreachable removal, then the branches,
//! then merging, each once. Running the three to a fixed point would cost a walk of the function
//! for every pass over it and buy back a case nobody has: what merging leaves behind is a bigger
//! block, and a bigger block does not make a branch foldable that was not foldable before. The
//! pipeline runs this pass more than once anyway, so the second chance is a pass boundary away
//! rather than a loop away, and that is a chance the pass manager can count and print.
//!
//! Two of section 21.1's four transformations are not here. Forwarder removal and redundant block
//! parameter removal are section 21.4's step three, they are interleaved on one worklist because
//! either can make the other possible, and they are their own change. Cross jumping is section
//! 21.1's last paragraph telling us not to: it costs a branch to save a copy, so it belongs at the
//! machine level under `-Os`, which is document 37.
//!
//! # Why this is not only an optimization
//!
//! Issue 359 is a program that does not link:
//!
//! ```c
//! extern void link_error(void);
//! void foo(int x) {
//!     switch (x) {
//!     case 0:
//!         if (0) { link_error(); case 1: bar(); }
//!     }
//! }
//! ```
//!
//! Nothing calls `link_error`, so a compiler that emits the call produces an object file that
//! does not link, and the difference between the two compilers is not how fast the program runs.
//! The file is in a suite of forty years of compiler bugs for the reason the `case 1:` is where
//! it is: control does reach `bar` through the switch, and it reaches it from inside the body of
//! the dead `if`. A compiler that deletes the compound statement gets this as wrong as one that
//! keeps all of it.
//!
//! Doing it in two steps is what makes that come out right without a special case for it. The
//! branch on the constant becomes a jump, which takes the edge into the dead arm away, and then
//! reachability from the entry decides what is left. The block holding `bar` has an edge from the
//! `switch` and stays. The block holding `link_error` has no edges at all and goes.
//!
//! # The condition it can read
//!
//! A constant, and a comparison of two constants. The second is here rather than in
//! [`crate::fold`] because folding a comparison would produce an `i1` standing on its own, which
//! is issue 352 and does not lower, so the pass that folds arithmetic deliberately leaves
//! comparisons alone. Reading one to decide which way a branch goes produces no `i1` at all: the
//! comparison is left exactly where it was, used by nothing, and [`crate::dce`] takes it out.
//!
//! # Fuel
//!
//! Fuel is charged for each branch that folds and for each block that is merged away, and not for
//! the blocks that go because nothing reaches them. Removal is the second half of the
//! transformation that was already paid for rather than a transformation of its own, and a fuel
//! limit that could stop between the two halves would hand the verifier a block nothing reaches.
//! Section 41.5 of `spec/optimizer/41-correctness.md` asks for fuel that is monotonic, which means
//! each step being all of one change and not part of one.
//!
//! That reasoning covers the blocks a fold stranded. It does not cover the ones that arrived
//! unreachable, and those are not charged for either, for a different reason: section 6.5 makes
//! removing them this pass's standing obligation rather than an optimization, everything below
//! reads the graph as though they are not there, and a bisection that turned the obligation off
//! would be bisecting over a function the rest of the optimizer does not believe in.

use std::collections::{HashMap, HashSet};

use rucc_ir::{Block, BlockCall, Def, Extra, Func, Inst, IntPred, Opcode, Value};

use crate::fold::constant;
use crate::{Analyses, Fuel, Pass, Preserved, Stats, uses};

/// Recorded once for each branch that turned into a jump.
const FOLDED: &str = "branch on a condition that is always the same way replaced by a jump";

/// Recorded once for each block that went with it.
const REMOVED: &str = "block nothing reaches removed";

/// Recorded once for each block folded into the one above it.
const MERGED: &str = "block with one way into it merged into the block above it";

/// Recorded for a branch that would have folded if there had been fuel for it.
const NO_FUEL: &str = "branch on a known condition left alone, the pass ran out of fuel";

/// Recorded for a block that would have been merged if there had been fuel for it.
const NO_FUEL_MERGE: &str = "block with one way into it left alone, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimplifyCfg;

impl Pass for SimplifyCfg {
    fn name(&self) -> &'static str {
        "simplify-cfg"
    }

    fn describe(&self) -> &'static str {
        "unreachable blocks go, a branch that only goes one way becomes a jump, and a block with \
         one way in is merged into the one above it"
    }

    fn preserves(&self) -> Preserved {
        // Nothing at all, and this is the pass the declaration exists for. An edge moves, so the
        // graph is a different graph, and everything built on the graph was about the old one.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        // Step one, and it is first for a reason beyond tidiness: a branch in a block nothing
        // reaches is a branch nothing executes, and folding one would spend fuel on a change
        // nobody can see and charge the two steps below for walking blocks that are not there.
        sweep(func, an, &mut stats);
        let mut folded = false;
        for block in func.blocks().collect::<Vec<Block>>() {
            let Some(term) = func.terminator(block) else { continue };
            let Some(taken) = taken(func, term) else { continue };
            if !fuel.take() {
                // Out of fuel stops the transforming and not the looking, the same way the other
                // passes treat it, so that the walk is the same walk at every fuel setting.
                stats.missed(NO_FUEL);
                continue;
            }
            jump_to(func, term, taken);
            stats.optimized(FOLDED);
            folded = true;
        }
        if folded {
            // The second sweep section 21.4 folds into step two. The cache is holding answers
            // about the function as it was a moment ago, and the manager clears it after the pass
            // returns, which is too late for the pass itself.
            an.clear();
            sweep(func, an, &mut stats);
        }
        // Merging reads which blocks have one predecessor, so it has to run on the graph as it is
        // after the stranded ones have gone. A block kept alive only by an edge from a block
        // nothing reaches looks like it has two ways in until that block is out of the function.
        let mut forward = HashMap::new();
        for chain in chains(func, an) {
            for (at, &block) in chain.iter().enumerate().skip(1) {
                if !fuel.take() {
                    // The rest of the chain goes with it. A block is merged into the one at the
                    // head of its chain, and it can only get there once everything between them
                    // has already arrived.
                    for _ in at..chain.len() {
                        stats.missed(NO_FUEL_MERGE);
                    }
                    break;
                }
                merge(func, chain[0], block, &mut forward);
                stats.optimized(MERGED);
            }
        }
        if !forward.is_empty() {
            // Once, for every parameter every merge bound, rather than a walk of the function per
            // block merged.
            uses::substitute(func, &forward);
        }
        stats
    }
}

/// Where this terminator always goes, if it always goes to one place.
///
/// `None` is every reason not to fold and does not say which, because the answer to all of them
/// is to leave the branch alone.
fn taken(func: &Func, term: Inst) -> Option<BlockCall> {
    let data = &func[term];
    let arg = *func[data.args].first()?;
    match data.opcode {
        Opcode::BrIf => {
            let Extra::Targets(targets) = data.extra else { return None };
            if let Some(call) = one_place(func, &func[targets]) {
                return Some(call);
            }
            // The first target is the one taken when the condition is one, which is what
            // `Builder::br_if` writes and what the printer reads back.
            let arm = usize::from(!known(func, arg)?);
            func[targets].get(arm).copied()
        }
        Opcode::Switch => {
            let Extra::Switch(at) = data.extra else { return None };
            let info = func[at];
            if let Some(call) = one_place(func, &func[info.targets]) {
                return Some(call);
            }
            let (value, _) = constant(func, arg)?;
            // The default is the first target and the cases follow it in the order their values
            // are in, so the target for a case that matches is one past the value's own place.
            let case = func[info.cases].iter().position(|it| *it == value);
            func[info.targets].get(case.map_or(0, |case| case + 1)).copied()
        }
        _ => None,
    }
}

/// The one edge every arm of a branch is, when they are all the same edge.
///
/// Section 21.1's branch simplification, the half of it that is not about a constant. A branch
/// whose arms all go to the same block with the same arguments goes there whatever the condition
/// says, so it is a jump, and the condition becomes something nothing reads for
/// [`crate::dce`] to take out.
///
/// The arguments have to match and not only the block. Two edges to one block carrying different
/// arguments are two different edges, and that is the whole reason this IR passes arguments along
/// an edge rather than writing a phi in the block: `if (c) goto L(1); else goto L(2);` is a real
/// program and turning it into a jump would have to pick one of the two numbers.
fn one_place(func: &Func, calls: &[BlockCall]) -> Option<BlockCall> {
    let &first = calls.first()?;
    let same = |call: &BlockCall| call.block == first.block && func[call.args] == func[first.args];
    calls[1..].iter().all(same).then_some(first)
}

/// Rewrites the terminator as a jump to that one of its targets.
///
/// In place, and the target keeps the arguments it already had, because the arguments belong to
/// the edge and the edge is the one that survives.
fn jump_to(func: &mut Func, term: Inst, call: BlockCall) {
    let targets = func.push_block_calls(&[call]);
    let args = func.push_values(&[]);
    let data = &mut func[term];
    data.opcode = Opcode::Jump;
    data.args = args;
    data.extra = Extra::Targets(targets);
}

/// Takes every block the entry does not reach out of the function.
///
/// The cache goes with them, because what it is holding is answers about a function that had them
/// in it, and the pass is not finished asking.
fn sweep(func: &mut Func, an: &mut Analyses, stats: &mut Stats) {
    let gone = stranded(func, an);
    if gone.is_empty() {
        return;
    }
    for block in gone {
        func.remove_block(block);
        stats.optimized(REMOVED);
    }
    an.clear();
}

/// The blocks the entry cannot reach, in block order.
///
/// This is reachability as the verifier counts it, which is over the edges the terminators name
/// and additionally over the blocks a `block_addr` mentions. A block whose address is taken is
/// arrived at by an `indirect_br` somewhere, and that instruction lists every block the address
/// can hold, so the edge is already in the graph from the place control really leaves. What the
/// graph does not carry is the `block_addr` itself, and deleting the block under one would leave
/// an instruction naming a block that is not there.
fn stranded(func: &Func, an: &mut Analyses) -> Vec<Block> {
    let cfg = an.cfg(func);
    let Some(entry) = cfg.entry() else { return Vec::new() };
    let mut seen = vec![false; cfg.capacity()];
    seen[entry.index()] = true;
    let mut stack = vec![entry];
    let mut reached = Vec::new();
    while let Some(block) = stack.pop() {
        for &succ in cfg.successors(block) {
            if !seen[succ.index()] {
                seen[succ.index()] = true;
                stack.push(succ);
            }
        }
        reached.push(block);
    }
    // The addresses in a second walk over the blocks the first one reached, because an address
    // taken in a block nothing reaches is an address nothing takes.
    let mut next = reached;
    while !next.is_empty() {
        let mut found = Vec::new();
        for block in next {
            for inst in func.insts(block) {
                if func[inst].opcode != Opcode::BlockAddr {
                    continue;
                }
                for call in func.successors(inst) {
                    if !seen[call.block.index()] {
                        seen[call.block.index()] = true;
                        found.push(call.block);
                    }
                }
            }
        }
        // Everything the newly kept blocks reach is kept too, which is what makes this a fixed
        // point rather than one extra step.
        let mut stack = found.clone();
        while let Some(block) = stack.pop() {
            for &succ in cfg.successors(block) {
                if !seen[succ.index()] {
                    seen[succ.index()] = true;
                    stack.push(succ);
                    found.push(succ);
                }
            }
        }
        next = found;
    }
    func.blocks().filter(|block| !seen[block.index()]).collect()
}

/// The runs of blocks that are one block written as several, head first.
///
/// Section 21.1's block merging, and the doc calls it a pure win for a reason worth stating: it
/// does not delete an instruction or move one earlier, it takes a boundary out. Every analysis
/// that is cheap inside a block and expensive across one gets more of the cheap kind, which is
/// most of them, and the branch that stops being a branch is the smallest part of it.
///
/// A block goes into the one above it when the one above it ends in a jump and this is the only
/// way in. Both halves are needed. One way in and a `br_if` above means the other arm would lose
/// its terminator, and a jump above with two ways in means the second predecessor would arrive in
/// the middle of a block.
///
/// The refusals are the entry block, which has to stay where control arrives even when one block
/// jumps to it; a block that jumps to itself, whose one predecessor is itself; and a block whose
/// address is taken, which is arrived at by an `indirect_br` the graph reads from the other end.
///
/// The answer is chains rather than pairs because a run of three is ordinary and the middle one
/// stops existing partway through. Each block is the head of at most one of these and the tail of
/// at most one, so what comes out is disjoint paths, and starting only from a head is what leaves
/// a ring of blocks that all point at each other alone rather than walking it forever.
fn chains(func: &Func, an: &mut Analyses) -> Vec<Vec<Block>> {
    let cfg = an.cfg(func);
    let Some(entry) = cfg.entry() else { return Vec::new() };
    let addressed = addressed(func);
    let mut below = HashMap::new();
    let mut is_below = HashSet::new();
    for block in func.blocks() {
        let Some(term) = func.terminator(block) else { continue };
        if func[term].opcode != Opcode::Jump {
            continue;
        }
        let Some(call) = func.successors(term).next() else { continue };
        let into = call.block;
        let preds = cfg.predecessors(into);
        if into == entry || into == block || addressed.contains(&into) {
            continue;
        }
        if preds.len() != 1 || preds[0] != block {
            continue;
        }
        below.insert(block, into);
        is_below.insert(into);
    }
    let heads = func.blocks().filter(|it| below.contains_key(it) && !is_below.contains(it));
    heads
        .map(|head| {
            let mut chain = vec![head];
            let mut at = head;
            while let Some(&next) = below.get(&at) {
                chain.push(next);
                at = next;
            }
            chain
        })
        .collect()
}

/// Every block some `block_addr` names.
fn addressed(func: &Func) -> HashSet<Block> {
    let mut taken = HashSet::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if func[inst].opcode != Opcode::BlockAddr {
                continue;
            }
            for call in func.successors(inst) {
                taken.insert(call.block);
            }
        }
    }
    taken
}

/// Moves everything in a block into the head of its chain and takes the block out of the function.
///
/// The jump is what is really being deleted, and the arguments it carried are what the merged
/// block's parameters were going to be told. Binding each parameter to the argument in its place
/// and pointing every reader at it is exactly what the jump was doing at run time, so the record
/// goes in the map and the whole map is spent in one walk when the pass is done.
fn merge(func: &mut Func, head: Block, block: Block, forward: &mut HashMap<Value, Value>) {
    let term = func.terminator(head).expect("the head of a chain ends in a jump");
    let call = func.successors(term).next().expect("a jump goes somewhere");
    let args = func[call.args].to_vec();
    let params = func[block].params.clone();
    for (param, arg) in params.into_iter().zip(args) {
        // Through whatever the merge above this one already decided, because a chain of three
        // binds the middle block's parameter to something the head passed and then binds the last
        // block's parameter to that same parameter.
        let arg = uses::chase(forward, arg);
        forward.insert(param, arg);
    }
    func.remove_inst(term);
    for inst in func.insts(block).collect::<Vec<Inst>>() {
        func.remove_inst(inst);
        func.append_inst(head, inst);
    }
    func.remove_block(block);
}

/// Whether this condition is always true or always false.
fn known(func: &Func, value: Value) -> Option<bool> {
    if let Some((imm, _)) = constant(func, value) {
        return Some(imm.unsigned() != 0);
    }
    compared(func, value)
}

/// What a comparison of two constants comes out as.
fn compared(func: &Func, value: Value) -> Option<bool> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    if data.opcode != Opcode::ICmp {
        return None;
    }
    let Extra::IntPred(pred) = data.extra else { return None };
    let args = &func[data.args];
    let (lhs, ty) = constant(func, *args.first()?)?;
    let (rhs, _) = constant(func, *args.get(1)?)?;
    Some(match pred {
        IntPred::Eq => lhs == rhs,
        IntPred::Ne => lhs != rhs,
        IntPred::Slt => lhs.signed(ty) < rhs.signed(ty),
        IntPred::Sle => lhs.signed(ty) <= rhs.signed(ty),
        IntPred::Sgt => lhs.signed(ty) > rhs.signed(ty),
        IntPred::Sge => lhs.signed(ty) >= rhs.signed(ty),
        IntPred::Ult => lhs.unsigned() < rhs.unsigned(),
        IntPred::Ule => lhs.unsigned() <= rhs.unsigned(),
        IntPred::Ugt => lhs.unsigned() > rhs.unsigned(),
        IntPred::Uge => lhs.unsigned() >= rhs.unsigned(),
    })
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Def, Func, IntPred, Module, Opcode, Signature, Type, Value};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::SimplifyCfg;
    use crate::stats::Kind;
    use crate::testing::graph;
    use crate::{Analyses, Fuel, Pass, Preserved, Stats};

    /// Runs the pass with as much fuel as it wants.
    fn simplify(func: &mut Func) -> Stats {
        SimplifyCfg.run(func, &mut Analyses::new(), &mut Fuel::unlimited())
    }

    /// The blocks the function still has, by number.
    fn blocks(func: &Func) -> Vec<usize> {
        func.blocks().map(Block::index).collect()
    }

    /// The opcode of a block's terminator.
    fn terminator(func: &Func, block: usize) -> Opcode {
        let block = Block::from_usize(block);
        func[func.terminator(block).expect("every block here has one")].opcode
    }

    /// Where a block's terminator goes, as block numbers.
    fn goes_to(func: &Func, block: usize) -> Vec<usize> {
        let block = Block::from_usize(block);
        let term = func.terminator(block).expect("every block here has one");
        func.successors(term).map(|call| call.block.index()).collect()
    }

    /// The block the instruction that produced a value is in now, if it is in one.
    ///
    /// Which arm of a branch survived is a question about where its code ended up rather than
    /// about the shape of the graph, because the arm that survives is merged into the block above
    /// it in the same run and the two blocks stop being two.
    fn lives_in(func: &Func, value: Value) -> Option<usize> {
        let Def::Result { inst, .. } = func[value].def else { return None };
        func.block_of(inst).map(Block::index)
    }

    /// A function with an entry, a `br_if` on `cond`, two arms and a join.
    ///
    /// The condition is built by the caller out of the builder it is handed, which is what lets
    /// one shape stand for a constant, a comparison and a value nothing knows anything about. Each
    /// arm holds one instruction that does nothing, which is there to be told apart from the one
    /// in the other arm, and the two of them come back with the function.
    fn diamond(cond: impl FnOnce(&mut Builder<'_>) -> Value) -> (Func, [Value; 2]) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let then_block = func.create_block();
        let else_block = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let cond = cond(&mut build);
        build.br_if(cond, then_block, &[], else_block, &[]);
        let mut marks = Vec::new();
        for (arm, mark) in [(then_block, 111), (else_block, 222)] {
            let mut build = Builder::new(&mut func, arm);
            marks.push(build.iconst(Type::int(32), mark));
            build.jump(join, &[]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        (func, [marks[0], marks[1]])
    }

    #[test]
    fn a_branch_on_a_true_constant_becomes_a_jump_to_the_first_arm() {
        let (mut func, [taken, other]) = diamond(|build| build.iconst(Type::int(1), 1));
        let stats = simplify(&mut func);
        assert!(stats.changed());
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 1);
        // The arm it did not take is gone, because nothing else went there, and the arm it did
        // take had one way in and went into the entry along with the join below it.
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 2);
        assert_eq!(lives_in(&func, taken), Some(0));
        assert_eq!(lives_in(&func, other), None);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_branch_on_a_false_constant_becomes_a_jump_to_the_second_arm() {
        let (mut func, [other, taken]) = diamond(|build| build.iconst(Type::int(1), 0));
        assert!(simplify(&mut func).changed());
        assert_eq!(lives_in(&func, taken), Some(0));
        assert_eq!(lives_in(&func, other), None);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn folding_a_branch_and_merging_what_it_leaves_are_two_things_fuel_buys_apart() {
        // The same function as the test above, with fuel for the fold and nothing after it. The
        // jump is there to be seen, which is the shape the merge would otherwise take away.
        let (mut func, _) = diamond(|build| build.iconst(Type::int(1), 1));
        let stats = SimplifyCfg.run(&mut func, &mut Analyses::new(), &mut Fuel::of(1));
        assert_eq!(terminator(&func, 0), Opcode::Jump);
        assert_eq!(goes_to(&func, 0), [1]);
        assert_eq!(blocks(&func), [0, 1, 3]);
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 0);
        // Both blocks of the chain, because a block only reaches the head once the block between
        // them has, so running out before the first one means neither.
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_MERGE), 2);
    }

    #[test]
    fn a_branch_on_a_comparison_of_two_constants_is_read_without_folding_it() {
        // Both ways round on every predicate, which is where a sign error or an inverted
        // comparison would hide. A comparison the pass reads is left standing, because folding
        // it would produce an `i1` on its own and issue 352 says that does not lower.
        let cases: &[(IntPred, i128, i128, bool)] = &[
            (IntPred::Eq, 7, 7, true),
            (IntPred::Eq, 7, 8, false),
            (IntPred::Ne, 7, 8, true),
            (IntPred::Ne, 7, 7, false),
            (IntPred::Slt, -1, 1, true),
            (IntPred::Slt, 1, -1, false),
            (IntPred::Sle, -1, -1, true),
            (IntPred::Sle, 1, -1, false),
            (IntPred::Sgt, 1, -1, true),
            (IntPred::Sgt, -1, 1, false),
            (IntPred::Sge, -1, -1, true),
            (IntPred::Sge, -1, 1, false),
            (IntPred::Ult, 1, -1, true),
            (IntPred::Ult, -1, 1, false),
            (IntPred::Ule, -1, -1, true),
            (IntPred::Ule, -1, 1, false),
            (IntPred::Ugt, -1, 1, true),
            (IntPred::Ugt, 1, -1, false),
            (IntPred::Uge, -1, -1, true),
            (IntPred::Uge, 1, -1, false),
        ];
        for &(pred, lhs, rhs, taken) in cases {
            let (mut func, marks) = diamond(|build| {
                let lhs = build.iconst(Type::int(32), lhs);
                let rhs = build.iconst(Type::int(32), rhs);
                build.icmp(pred, lhs, rhs)
            });
            assert!(simplify(&mut func).changed(), "{pred:?} {lhs} {rhs}");
            let [went, gone] = if taken { [marks[0], marks[1]] } else { [marks[1], marks[0]] };
            assert_eq!(lives_in(&func, went), Some(0), "{pred:?} {lhs} {rhs}");
            assert_eq!(lives_in(&func, gone), None, "{pred:?} {lhs} {rhs}");
            let kept = func.insts(Block::from_usize(0)).any(|it| func[it].opcode == Opcode::ICmp);
            assert!(kept, "the comparison was folded away and issue 352 says it must not be");
        }
    }

    #[test]
    fn a_branch_on_something_nobody_knows_is_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::int(1)]));
        let entry = func.create_block();
        let then_block = func.create_block();
        let else_block = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, then_block, &[], else_block, &[]);
        for arm in [then_block, else_block] {
            let mut build = Builder::new(&mut func, arm);
            build.ret(&[]);
        }
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert!(stats.is_empty(), "a pass with nothing to say should say nothing");
        assert_eq!(terminator(&func, 0), Opcode::BrIf);
        assert_eq!(blocks(&func), [0, 1, 2]);
    }

    /// A function whose entry switches on a constant, with a marker in the default and in each
    /// case, in that order.
    fn switched(on: i128, cases: &[i128]) -> (Func, Vec<Value>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms: Vec<Block> = (0..=cases.len()).map(|_| func.create_block()).collect();
        let mut build = Builder::new(&mut func, entry);
        let value = build.iconst(Type::int(32), on);
        let pairs: Vec<(i128, Block)> =
            cases.iter().enumerate().map(|(at, &case)| (case, arms[at + 1])).collect();
        build.switch(value, arms[0], &pairs);
        let mut marks = Vec::new();
        for (at, &arm) in arms.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            marks.push(build.iconst(Type::int(32), 100 + at as i128));
            build.ret(&[]);
        }
        (func, marks)
    }

    #[test]
    fn a_switch_on_a_constant_takes_the_case_that_matches() {
        let (mut func, marks) = switched(5, &[4, 5]);
        assert!(simplify(&mut func).changed());
        assert_eq!(lives_in(&func, marks[2]), Some(0));
        assert_eq!(lives_in(&func, marks[0]), None);
        assert_eq!(lives_in(&func, marks[1]), None);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_switch_on_a_constant_no_case_names_takes_the_default() {
        let (mut func, marks) = switched(9, &[4]);
        assert!(simplify(&mut func).changed());
        assert_eq!(lives_in(&func, marks[0]), Some(0));
        assert_eq!(lives_in(&func, marks[1]), None);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn the_arguments_travel_with_the_edge_that_survives() {
        // The whole reason there are no phi nodes: the argument is in the branch beside the
        // block it goes to, so the surviving arm brings its own and the other one leaves with
        // the edge it was on. Both arms name the same block, so this is also the case branch
        // simplification has to leave alone: one block and two edges, because the two edges say
        // different things.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 0);
        let taken = build.iconst(Type::int(32), 11);
        let other = build.iconst(Type::int(32), 22);
        build.br_if(cond, join, &[other], join, &[taken]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);
        assert!(simplify(&mut func).changed());
        // The jump took the edge that survived, and then the block below it had one way in and
        // came up, which is where the parameter stopped being a parameter: whatever read it reads
        // the argument that edge was carrying.
        assert_eq!(blocks(&func), [0]);
        let term = func.terminator(entry).expect("the entry has one");
        assert_eq!(func[func[term].args], [taken]);
        assert_ne!(func[func[term].args], [param]);
    }

    #[test]
    fn a_branch_whose_arms_are_the_same_edge_becomes_a_jump() {
        // Section 21.1's branch simplification, which is about the targets rather than about the
        // condition: nothing here knows what `cond` is and it does not matter, because both ways
        // out arrive at the same place carrying the same thing.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let join = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, join, &[], join, &[]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 1);
        assert_eq!(blocks(&func), [0]);
        assert_eq!(terminator(&func, 0), Opcode::Return);
    }

    #[test]
    fn a_switch_whose_cases_all_go_to_one_place_becomes_a_jump() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let join = func.create_block();
        let value = func.append_param(entry, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        build.switch(value, join, &[(4, join), (5, join)]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 1);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_branch_to_one_block_by_two_edges_that_differ_is_left_alone() {
        // One block and two edges. Folding would have to pick one of the two arguments, and
        // whichever it picked would be the wrong one half the time.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let join = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        func.append_param(join, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let first = build.iconst(Type::int(32), 11);
        let second = build.iconst(Type::int(32), 22);
        build.br_if(cond, join, &[first], join, &[second]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(terminator(&func, 0), Opcode::BrIf);
        assert_eq!(blocks(&func), [0, 1]);
    }

    #[test]
    fn a_block_the_dead_arm_shared_with_a_live_one_stays() {
        // Issue 359 in the small. The block holding `bar` is inside the body of the dead `if`
        // and is a `case` of the switch as well, so the arm goes and the block does not.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::int(32)]));
        let entry = func.create_block();
        let dead = func.create_block();
        let shared = func.create_block();
        let exit = func.create_block();
        let x = func.append_param(entry, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let never = build.iconst(Type::int(1), 0);
        build.switch(x, exit, &[(0, dead), (1, shared)]);
        // The `if (0)` inside the first case, whose body is where the second case's label sits.
        let mut build = Builder::new(&mut func, dead);
        build.br_if(never, shared, &[], exit, &[]);
        for arm in [shared, exit] {
            let mut build = Builder::new(&mut func, arm);
            build.ret(&[]);
        }
        let stats = simplify(&mut func);
        assert!(stats.changed());
        // The switch is on a parameter, so it stays. The branch inside the dead arm folds to the
        // exit, and nothing is removed at all, because the shared block is still a case.
        assert_eq!(terminator(&func, 0), Opcode::Switch);
        assert_eq!(goes_to(&func, 1), [3]);
        assert_eq!(blocks(&func), [0, 1, 2, 3]);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 0);
    }

    #[test]
    fn a_block_whose_address_is_taken_is_not_removed() {
        // Reachability here has to be the verifier's reachability. The graph does not carry the
        // edge from a `block_addr` to the block it names, and a pass that removed the block
        // under one would leave an instruction pointing at nothing.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let labelled = func.create_block();
        let arm = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        let addr = build.block_addr(labelled);
        build.br_if(cond, arm, &[], labelled, &[]);
        let mut build = Builder::new(&mut func, arm);
        build.indirect_br(addr, &[labelled]);
        let mut build = Builder::new(&mut func, labelled);
        build.ret(&[]);
        assert!(simplify(&mut func).changed());
        assert!(blocks(&func).contains(&1), "the labelled block went with the arm");
        // The arm had one way in and came up into the entry, which is where the `indirect_br`
        // that reaches the labelled block is now.
        assert_eq!(blocks(&func), [0, 1]);
        assert_eq!(goes_to(&func, 0), [1]);
    }

    #[test]
    fn a_block_only_an_unreachable_block_takes_the_address_of_goes_too() {
        // The other half of the same rule. Once the block holding the `block_addr` is gone, the
        // address is gone with it, and the block it named is reached by nothing.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let dead = func.create_block();
        let labelled = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, entry, &[], dead, &[]);
        let mut build = Builder::new(&mut func, dead);
        let addr = build.block_addr(labelled);
        build.indirect_br(addr, &[labelled]);
        let mut build = Builder::new(&mut func, labelled);
        build.ret(&[]);
        assert!(simplify(&mut func).changed());
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_block_nothing_reaches_goes_even_when_no_branch_folded() {
        // Section 6.5 says this pass is the one that deletes them, and it says so about the
        // blocks the front end handed over as well as the ones a fold here stranded. Nothing in
        // this function folds, and the block still has to go, because every analysis below reads
        // the graph as though it is not there.
        let mut func = graph(&[&[], &[]]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_block_with_one_way_into_it_goes_into_the_block_above_it() {
        let mut func = graph(&[&[1], &[2], &[]]);
        let stats = simplify(&mut func);
        // A run of three is one chain and not two rounds of one pair, because the block in the
        // middle stops being a block partway through.
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 2);
        assert_eq!(blocks(&func), [0]);
        assert_eq!(terminator(&func, 0), Opcode::Return);
    }

    #[test]
    fn a_block_with_two_ways_into_it_stays_where_it_is() {
        // The join of a diamond nobody can fold. Merging it into either arm would leave the other
        // arm branching into the middle of a block.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let then_block = func.create_block();
        let else_block = func.create_block();
        let join = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, then_block, &[], else_block, &[]);
        for arm in [then_block, else_block] {
            let mut build = Builder::new(&mut func, arm);
            build.jump(join, &[]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(blocks(&func), [0, 1, 2, 3]);
    }

    #[test]
    fn a_block_above_one_that_does_not_end_in_a_jump_keeps_it() {
        // One way into the join, and the block above it is a branch. Merging would take the
        // terminator off the other arm.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let arm = func.create_block();
        let exit = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, arm, &[], exit, &[]);
        for block in [arm, exit] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(blocks(&func), [0, 1, 2]);
    }

    #[test]
    fn the_entry_block_is_never_the_one_that_moves() {
        // A loop back to the entry, so the entry has one way in and the block above it ends in a
        // jump, which is every condition but the one that matters. Control arrives at the entry
        // and it has to still be there when it does.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let latch = func.create_block();
        let exit = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, latch, &[], exit, &[]);
        let mut build = Builder::new(&mut func, latch);
        build.jump(entry, &[]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(blocks(&func), [0, 1, 2]);
    }

    #[test]
    fn a_block_whose_address_is_taken_is_not_merged_away_either() {
        // The same rule as the one about deleting it. Merging it into the block above would take
        // the block out of the function, and the `block_addr` would name one that is not there.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let middle = func.create_block();
        let labelled = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        build.block_addr(labelled);
        build.jump(middle, &[]);
        let mut build = Builder::new(&mut func, middle);
        build.jump(labelled, &[]);
        let mut build = Builder::new(&mut func, labelled);
        build.ret(&[]);
        let stats = simplify(&mut func);
        // The middle block had one way in and no address, so it came up. The labelled block has
        // one way in too, and stayed.
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 1);
        assert_eq!(blocks(&func), [0, 2]);
    }

    #[test]
    fn merging_binds_a_block_parameter_to_the_argument_the_jump_carried() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let below = func.create_block();
        let param = func.append_param(below, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let arg = build.iconst(Type::int(32), 7);
        build.jump(below, &[arg]);
        let mut build = Builder::new(&mut func, below);
        build.ret(&[param]);
        assert!(simplify(&mut func).changed());
        assert_eq!(blocks(&func), [0]);
        let term = func.terminator(entry).expect("the entry has one");
        assert_eq!(func[func[term].args], [arg]);
    }

    #[test]
    fn a_chain_of_merges_follows_a_parameter_bound_to_a_parameter() {
        // The middle block passes its own parameter down, so the last block's parameter is bound
        // to something that is on its way to being the entry's constant. Following the map is
        // what makes the second merge worth as much as the first.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let middle = func.create_block();
        let last = func.create_block();
        let carried = func.append_param(middle, Type::int(32));
        let arrived = func.append_param(last, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let arg = build.iconst(Type::int(32), 7);
        build.jump(middle, &[arg]);
        let mut build = Builder::new(&mut func, middle);
        build.jump(last, &[carried]);
        let mut build = Builder::new(&mut func, last);
        build.ret(&[arrived]);
        assert!(simplify(&mut func).changed());
        assert_eq!(blocks(&func), [0]);
        let term = func.terminator(entry).expect("the entry has one");
        assert_eq!(func[func[term].args], [arg]);
    }

    #[test]
    fn out_of_fuel_leaves_the_function_exactly_as_it_was() {
        let (mut func, _) = diamond(|build| build.iconst(Type::int(1), 1));
        let before = blocks(&func);
        let stats = SimplifyCfg.run(&mut func, &mut Analyses::new(), &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert_eq!(terminator(&func, 0), Opcode::BrIf);
        assert_eq!(blocks(&func), before);
    }

    #[test]
    fn what_fuel_buys_is_one_whole_change_and_never_half_of_one() {
        // Two foldable branches and fuel for one. The half that removes the stranded blocks is
        // not charged for, because a limit that could stop between the two halves would leave a
        // block nothing reaches and the verifier would refuse the function.
        let mut func = graph(&[&[1, 2], &[3, 4], &[5], &[5], &[5], &[]]);
        let stats = SimplifyCfg.run(&mut func, &mut Analyses::new(), &mut Fuel::of(1));
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        // The entry folded to its first arm, so the second arm is stranded and goes, and the
        // block only it reached goes with it.
        assert_eq!(blocks(&func), [0, 1, 3, 4, 5]);
    }

    #[test]
    fn the_pass_leaves_the_verifier_nothing_to_complain_about() {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut func = graph(&[&[1, 2], &[3], &[3], &[4, 1], &[]]);
        simplify(&mut func);
        module.add_func(func);
        rucc_ir::verify(&module, &names).expect("the pass left the function verifiable");
    }

    #[test]
    fn the_pass_says_it_preserves_nothing() {
        assert_eq!(SimplifyCfg.preserves(), Preserved::NONE);
    }
}
