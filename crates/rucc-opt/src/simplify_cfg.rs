//! Control flow simplification: a branch whose condition is already known becomes a jump, and
//! the blocks that leaves stranded are removed.
//!
//! Design: section 6.5 of `spec/optimizer/06-cfg-and-dominators.md`, which states the rule for the
//! whole optimizer, that a block the entry does not reach is invisible to every analysis and is
//! deleted here rather than by whichever pass happened to notice it.
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
//! Fuel is charged for each branch that folds and not for the blocks that go with it. The
//! removal is the second half of the transformation that was already paid for rather than a
//! transformation of its own, and a fuel limit that could stop between the two halves would hand
//! the verifier a block nothing reaches. Section 41.5 of `spec/optimizer/41-correctness.md` asks
//! for fuel that is monotonic, which means each step being all of one change and not part of one.

use rucc_ir::{Block, BlockCall, Def, Extra, Func, Inst, IntPred, Opcode, Value};

use crate::fold::constant;
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// Recorded once for each branch that turned into a jump.
const FOLDED: &str = "branch on a condition that is always the same way replaced by a jump";

/// Recorded once for each block that went with it.
const REMOVED: &str = "block nothing reaches removed";

/// Recorded for a branch that would have folded if there had been fuel for it.
const NO_FUEL: &str = "branch on a known condition left alone, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimplifyCfg;

impl Pass for SimplifyCfg {
    fn name(&self) -> &'static str {
        "simplify-cfg"
    }

    fn describe(&self) -> &'static str {
        "a branch whose condition is known becomes a jump, and unreachable blocks are removed"
    }

    fn preserves(&self) -> Preserved {
        // Nothing at all, and this is the pass the declaration exists for. An edge moves, so the
        // graph is a different graph, and everything built on the graph was about the old one.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
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
        }
        if !stats.changed() {
            // Nothing moved, so nothing can have been stranded. The graph is not computed at
            // all here, which is what keeps this pass free on the functions that have no branch
            // it can read, which is most of them.
            return stats;
        }
        // The cache is holding answers about the function as it was a moment ago. The manager
        // clears it after the pass returns, which is too late for the pass itself.
        an.clear();
        for block in stranded(func, an) {
            func.remove_block(block);
            stats.optimized(REMOVED);
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
            // The first target is the one taken when the condition is one, which is what
            // `Builder::br_if` writes and what the printer reads back.
            let arm = usize::from(!known(func, arg)?);
            func[targets].get(arm).copied()
        }
        Opcode::Switch => {
            let Extra::Switch(at) = data.extra else { return None };
            let (value, _) = constant(func, arg)?;
            let info = func[at];
            // The default is the first target and the cases follow it in the order their values
            // are in, so the target for a case that matches is one past the value's own place.
            let case = func[info.cases].iter().position(|it| *it == value);
            func[info.targets].get(case.map_or(0, |case| case + 1)).copied()
        }
        _ => None,
    }
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
    use rucc_ir::{Block, Builder, Func, IntPred, Module, Opcode, Signature, Type, Value};
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

    /// A function with an entry, a `br_if` on `cond`, two arms and a join.
    ///
    /// The condition is built by the caller out of the builder it is handed, which is what lets
    /// one shape stand for a constant, a comparison and a value nothing knows anything about.
    fn diamond(cond: impl FnOnce(&mut Builder<'_>) -> Value) -> Func {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let then_block = func.create_block();
        let else_block = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let cond = cond(&mut build);
        build.br_if(cond, then_block, &[], else_block, &[]);
        for arm in [then_block, else_block] {
            let mut build = Builder::new(&mut func, arm);
            build.jump(join, &[]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        func
    }

    #[test]
    fn a_branch_on_a_true_constant_becomes_a_jump_to_the_first_arm() {
        let mut func = diamond(|build| build.iconst(Type::int(1), 1));
        let stats = simplify(&mut func);
        assert!(stats.changed());
        assert_eq!(terminator(&func, 0), Opcode::Jump);
        assert_eq!(goes_to(&func, 0), [1]);
        // And the arm it did not take is gone, because nothing else went there.
        assert_eq!(blocks(&func), [0, 1, 3]);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
    }

    #[test]
    fn a_branch_on_a_false_constant_becomes_a_jump_to_the_second_arm() {
        let mut func = diamond(|build| build.iconst(Type::int(1), 0));
        assert!(simplify(&mut func).changed());
        assert_eq!(goes_to(&func, 0), [2]);
        assert_eq!(blocks(&func), [0, 2, 3]);
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
            let mut func = diamond(|build| {
                let lhs = build.iconst(Type::int(32), lhs);
                let rhs = build.iconst(Type::int(32), rhs);
                build.icmp(pred, lhs, rhs)
            });
            assert!(simplify(&mut func).changed(), "{pred:?} {lhs} {rhs}");
            let arm = if taken { 1 } else { 2 };
            assert_eq!(goes_to(&func, 0), [arm], "{pred:?} {lhs} {rhs}");
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

    #[test]
    fn a_switch_on_a_constant_takes_the_case_that_matches() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let default = func.create_block();
        let first = func.create_block();
        let second = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let value = build.iconst(Type::int(32), 5);
        build.switch(value, default, &[(4, first), (5, second)]);
        for arm in [default, first, second] {
            let mut build = Builder::new(&mut func, arm);
            build.ret(&[]);
        }
        assert!(simplify(&mut func).changed());
        assert_eq!(terminator(&func, 0), Opcode::Jump);
        assert_eq!(goes_to(&func, 0), [3]);
        assert_eq!(blocks(&func), [0, 3]);
    }

    #[test]
    fn a_switch_on_a_constant_no_case_names_takes_the_default() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let default = func.create_block();
        let case = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let value = build.iconst(Type::int(32), 9);
        build.switch(value, default, &[(4, case)]);
        for arm in [default, case] {
            let mut build = Builder::new(&mut func, arm);
            build.ret(&[]);
        }
        assert!(simplify(&mut func).changed());
        assert_eq!(goes_to(&func, 0), [1]);
        assert_eq!(blocks(&func), [0, 1]);
    }

    #[test]
    fn the_arguments_travel_with_the_edge_that_survives() {
        // The whole reason there are no phi nodes: the argument is in the branch beside the
        // block it goes to, so the surviving arm brings its own and the other one leaves with
        // the edge it was on.
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
        build.ret(&[]);
        assert!(simplify(&mut func).changed());
        let term = func.terminator(entry).expect("the entry has one");
        let call = func.successors(term).next().expect("a jump goes somewhere");
        assert_eq!(func[call.args], [taken]);
        assert_eq!(func[join].params, [param]);
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
        assert_eq!(goes_to(&func, 0), [2]);
        assert!(blocks(&func).contains(&1), "the labelled block went with the arm");
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
    fn out_of_fuel_leaves_the_function_exactly_as_it_was() {
        let mut func = diamond(|build| build.iconst(Type::int(1), 1));
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
