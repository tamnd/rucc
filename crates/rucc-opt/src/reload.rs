//! A load walks back over memory to the store it sees, and takes the value that store wrote.
//!
//! Design: `spec/optimizer/16-gvn-and-pre.md` section 16.2. That section asks for two redundant
//! load eliminators and this is the second of them. The first is [`crate::load`], which keeps one
//! table per block, compares addresses by identity, and throws everything away at the end of the
//! block, because what reaches a block from its predecessors is a question it does not ask. This is
//! the one that asks it.
//!
//! # What the difference buys
//!
//! A store in a block that dominates the load rather than in the same block, which is every field
//! read after a loop that wrote it. A load reached through a join where every path agrees about
//! which store it sees. A load inside a loop whose body writes nothing that could reach it, where
//! the store is above the loop and the walk cuts the back edge and says so.
//!
//! None of those is something the restricted version could be extended to do. They are the walk.
//!
//! # The walk
//!
//! [`crate::memssa`] is where it lives, and it was written, tested and documented long before
//! anything called it. What stopped anything calling it was tamnd/rucc#1467: the walk needs an alias
//! oracle at every step and an oracle wanted the module, and a pass is handed one function. It does
//! not want the module any more.
//!
//! [`Walk::clobber`] answers with five variants rather than two, and the shape of that is what this
//! pass rests on. [`Clobber::Exact`] is a write that covered exactly the bytes the load reads, and
//! it is the only one worth acting on. [`Clobber::Partial`] covered some of them, which is a shift
//! and a truncate away from being the value and is document 16's decision rather than this one's.
//! [`Clobber::Maybe`] is a write the oracle could not rule out and could not pin down.
//! [`Clobber::Unknown`] is a walk that ran out of budget or a join whose paths disagreed, and it is
//! not a no, which is why it has a name rather than being the absence of an answer.
//!
//! # Why the store it names dominates the load
//!
//! Because the walk only ever gives one instruction back. A join combines the answers from every
//! path into it and a disagreement is [`Clobber::Unknown`], so an `Exact` naming a store is a store
//! on every path from the entry to the load, and an instruction on every path to another one
//! dominates it. That is the whole argument, and it is why this pass does not compute dominance and
//! does not need to: the value the store wrote dominates the store, the store dominates the load,
//! and a use put where the load was is a use inside the value's dominance region.
//!
//! # The width, which is where the miscompilation would be
//!
//! Section 16.6 names a load forwarded from a store of a different size as the most likely wrong
//! answer in that document, and `Exact` is about bytes rather than about types. Two runs that are
//! the same bytes can still be two different readings of them, a four byte integer and a four byte
//! float being the obvious pair, so the types have to be equal as well and a load whose type is not
//! the stored value's stays and is counted.
//!
//! # The same address read twice
//!
//! A load is not a def of memory, so the walk goes straight past an earlier load of the same
//! address and arrives at whatever wrote it last, which is often nothing it can name. That leaves
//! the easiest redundant load of all in place: two loads of the same address at the same version of
//! memory read the same bytes, and the version of memory is exactly the statement that nothing
//! wrote them in between.
//!
//! So there is a table alongside the walk, keyed by the version of memory, the address and the
//! type, holding what the first load of that key is known to be equal to, and a later load whose
//! key is in it takes that value. It is value numbering over memory rather than a walk, which is
//! why it is a table here and not another [`Clobber`] variant in [`crate::memssa`].
//!
//! Two things make it right. Memory is threaded through every instruction that touches it, so two
//! loads carrying the same version have no write between them on any path from the one to the
//! other. That is a statement about paths through the earlier load, so the earlier load has to be
//! on every path to the later one, and that is dominance and is the one thing this pass computes
//! that the walk did not need. On a safety build the two are usually separated by a check, and a
//! check reads the planes and writes nothing, so the version survives it.
//!
//! What goes in the table is the value the load is equal to rather than the load's own result. A
//! load that was itself forwarded is about to be removed, [`substitute`] does not chase a rewrite
//! through another rewrite, and a later load of the same key can still reach the table when its own
//! walk was the one that ran out of budget.
//!
//! # What is not here
//!
//! Phi translation, which is asking about a load whose address is a block parameter in the
//! predecessor's terms. And section 9.2's `translate`, which is what lets a load be followed
//! through a `memcpy` and which [`Walk::clobber_with`] already takes a callback for. Both are on
//! tamnd/rucc#1476.
//!
//! # The chain goes on and comes off again
//!
//! [`memssa::build`] before and [`memssa::strip`] after, per function. The back end has never seen
//! memory SSA and is not going to, and nothing in the pipeline keeps the chain across passes,
//! because that would mean every edit to the control flow graph anywhere in the optimizer had to
//! keep the memory parameters in step with the blocks. Two linear walks per function is the price
//! of not making that claim.
//!
//! It has one consequence worth naming. An instruction cannot grow or lose a result, so putting the
//! chain on and taking it off again replaces every instruction that touches memory with an
//! equivalent one, even in a function where not a single load was forwarded. The shape of the
//! function is untouched, so every control flow answer still stands, and the liveness is about
//! values and does not, which is why this pass drops it whether or not it changed anything.

use std::collections::HashMap;

use rucc_ir::{Block, Flags, Func, Inst, Opcode, Type, Value};

use crate::memssa::{Clobber, Walk};
use crate::uses::substitute;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats, memssa};

/// What this pass is called, which the pipeline matches on to decide whether to build the module
/// facts the oracle asks for.
pub const NAME: &str = "redundant-load";

/// Recorded for a load that took the value of the store the walk said it sees.
const FORWARDED: &str = "load replaced by the value of the store the walk found";

/// Recorded for a load that took the value an earlier load of the same address already had.
const REUSED: &str = "load replaced by the value an earlier load of the same address already had";

/// Recorded for a load the walk placed on a write that covered only part of it.
const PARTIAL: &str = "load kept, what wrote it covers only part of what it reads";

/// Recorded for a load the walk placed on a write it could not pin down.
const MAYBE: &str = "load kept, something that may have written it could not be pinned down";

/// Recorded for a load whose walk ran out of budget or reached a join whose paths disagreed.
const UNKNOWN: &str = "load kept, the walk back over memory established nothing";

/// Recorded for a load whose store covered the same bytes at a different type.
const WIDTH: &str = "load kept, the store that covers it wrote a different type";

/// Recorded for a load the walk placed on a write that is not a store of one value.
const NOT_A_STORE: &str = "load kept, what covers it writes memory without storing one value";

/// Recorded for a load that would have gone if there had been fuel for it.
const NO_FUEL: &str = "redundant load kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug)]
pub struct RedundantLoad;

impl Pass for RedundantLoad {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a load takes the value of the store it sees, wherever in the function that store is"
    }

    fn preserves(&self) -> Preserved {
        // The shape of the function, for the reason `crate::load` gives: no block is added, none
        // is removed, no edge moves, and the instructions that go are loads, which are never
        // terminators. The memory parameters this puts on the joins come back off before the pass
        // returns, so no block ends with a parameter it did not start with.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if !memssa::build(func) {
            return stats;
        }
        // What each removed load's result is read as, applied to the whole function once at the
        // end, for the reason `crate::load` gives: rewriting each one where it is found would be a
        // walk over the function per load and there is nothing to gain by it.
        let mut forward: HashMap<Value, Value> = HashMap::new();
        let mut gone: Vec<Inst> = Vec::new();

        // The walk borrows the function, so it is a scope of its own and every edit happens after
        // it. Built once, because the alias oracle inside it holds an escape analysis that is one
        // walk over the function and every step of every walk may ask it.
        {
            let dom = an.dominators(func);
            let mut walk = Walk::new(func, an.outside());
            // One entry per address read at a version of memory, holding the block the first load
            // of it was in and the value that load is known to be equal to.
            let mut seen: HashMap<(Value, Value, Type), (Block, Value)> = HashMap::new();
            for block in func.blocks().collect::<Vec<Block>>() {
                for inst in func.insts(block).collect::<Vec<Inst>>() {
                    let Some((result, ty)) = reads(func, inst) else {
                        continue;
                    };
                    let key = func.mem_in(inst).map(|mem| (mem, func[func[inst].args][0], ty));
                    let found = match walk.clobber(inst) {
                        Clobber::Exact(wrote) => match stored(func, wrote) {
                            None => Found::Kept(Some(NOT_A_STORE)),
                            Some(value) if func[value].ty != ty => Found::Kept(Some(WIDTH)),
                            Some(value) => Found::Store(value),
                        },
                        Clobber::Partial(_) => Found::Kept(Some(PARTIAL)),
                        Clobber::Maybe(_) => Found::Kept(Some(MAYBE)),
                        Clobber::Unknown => Found::Kept(Some(UNKNOWN)),
                        // Nothing in the function wrote it, so the load reads whatever was there
                        // when the function started. There is no value here to take and nothing
                        // was missed either, so it is not counted as one.
                        Clobber::NoClobber => Found::Kept(None),
                    };
                    // The walk had nothing, so ask whether an earlier load of the same address at
                    // this version of memory had something. The dominance is what the walk did not
                    // have to compute and this does: two arms of the same branch share a version of
                    // memory and neither of them runs before the other.
                    let found = match found {
                        Found::Kept(reason) => match key.and_then(|key| seen.get(&key)) {
                            Some(&(at, value)) if dom.dominates(at, block) => Found::Earlier(value),
                            _ => Found::Kept(reason),
                        },
                        taken => taken,
                    };
                    let (value, why) = match found {
                        Found::Store(value) => (value, FORWARDED),
                        Found::Earlier(value) => (value, REUSED),
                        Found::Kept(reason) => {
                            if let Some(reason) = reason {
                                stats.missed(reason);
                            }
                            remember(&mut seen, key, block, result);
                            continue;
                        }
                    };
                    if !fuel.take() {
                        // Out of fuel is a request to stop transforming and not to stop looking, so
                        // the walk goes on and the count of what could have gone is the same at
                        // every setting, which is what makes a bisection over it monotonic.
                        stats.missed(NO_FUEL);
                        remember(&mut seen, key, block, result);
                        continue;
                    }
                    forward.insert(result, value);
                    gone.push(inst);
                    stats.optimized(why);
                    remember(&mut seen, key, block, value);
                }
            }
            let counts = walk.counts();
            if counts.walks() > 0 {
                stats.record(crate::stats::Kind::Note, WALKS, count(counts.walks()));
                stats.record(crate::stats::Kind::Note, STEPS, count(counts.steps()));
                if counts.exhausted() > 0 {
                    stats.record(crate::stats::Kind::Note, EXHAUSTED, count(counts.exhausted()));
                }
            }
        }

        for inst in gone {
            func.remove_inst(inst);
        }
        if !forward.is_empty() {
            substitute(func, &forward);
        }
        memssa::strip(func);
        // Said here rather than left to `preserves`, because the manager takes a pass that changed
        // nothing to have preserved everything and this one has not: the chain going on and coming
        // off gives every instruction that touches memory a new name whatever the pass did with
        // them.
        an.settle(func, self.preserves(), false);
        stats
    }
}

/// What there is to put in place of a load, and where it came from.
enum Found {
    /// The store the walk arrived at wrote this.
    Store(Value),
    /// An earlier load of the same address at the same version of memory already had this.
    Earlier(Value),
    /// The load stays, with the reason when there is one worth recording.
    Kept(Option<&'static str>),
}

/// Records what a load of this address at this version of memory is equal to.
///
/// The first one of a key wins. A second one is either dominated by the first, in which case it was
/// forwarded and there is nothing left to record, or it is not, and then neither block dominates
/// the other and keeping the one already there is as good as swapping it.
fn remember(
    seen: &mut HashMap<(Value, Value, Type), (Block, Value)>,
    key: Option<(Value, Value, Type)>,
    block: Block,
    value: Value,
) {
    if let Some(key) = key {
        seen.entry(key).or_insert((block, value));
    }
}

/// Recorded as a note: how many walks were made.
const WALKS: &str = "walks back over memory";

/// Recorded as a note: how many defs those walks looked at, which is one alias query each.
const STEPS: &str = "memory defs the walks looked at";

/// Recorded as a note: how many walks gave up rather than answering.
///
/// Section 9.3 of `spec/optimizer/09-memory-ssa.md` says this number decides whether the walk gets
/// a cache. Above one percent of walks and the budget is too small or the alias analysis is too
/// weak, and both of those are better fixed than cached around.
const EXHAUSTED: &str = "walks that ran out of budget";

/// A count as the record holds them, which is narrower than the counters are.
fn count(of: u64) -> u32 {
    u32::try_from(of).unwrap_or(u32::MAX)
}

/// The result and the type of a load worth asking about, and nothing for anything else.
///
/// Narrow on purpose, and the same shape [`crate::load`] uses. A plain non-volatile `Load` with one
/// address and one result. `AtomicLoad` is a separate opcode in this IR and is not this one, so an
/// ordering never reaches here as something to forward.
fn reads(func: &Func, inst: Inst) -> Option<(Value, Type)> {
    let data = &func[inst];
    if data.opcode != Opcode::Load || data.flags.contains(Flags::VOLATILE) {
        return None;
    }
    let mut results = data.results();
    let (Some(result), None) = (results.next(), results.next()) else {
        return None;
    };
    Some((result, func[result].ty))
}

/// What a store wrote, and nothing for anything else that writes memory.
///
/// The walk answers `Exact` for any write that covered exactly the bytes the load reads, and a
/// `memcpy` or a `memset` can do that without there being one value anywhere to take.
fn stored(func: &Func, inst: Inst) -> Option<Value> {
    let data = &func[inst];
    if data.opcode != Opcode::Store || data.flags.contains(Flags::VOLATILE) {
        return None;
    }
    func[data.args].first().copied()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rucc_base::Interner;
    use rucc_ir::{Module, parse, verify_func};

    use super::*;
    use crate::outside::Outside;

    const HEADER: &str = "\
; ModuleID = 'mem.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
";

    fn wrap(signature: &str, body: &str) -> String {
        format!("{HEADER}\nfunc @f{signature}, linkage(external) {{\n{body}}}\n")
    }

    /// Runs the pass over the one function in the text and insists the result verifies, which is
    /// where most of the strength of these tests is: the chain goes on and comes off again, and a
    /// half removed chain is exactly the kind of thing a shape assertion would let through.
    fn run(text: &str) -> (Module, Stats) {
        let mut names = Interner::new();
        let mut module = parse(text, &mut names).expect("the text parses");
        let id = module.funcs().next().expect("one function");
        let outside = Arc::new(Outside::of(&module));
        let mut an = crate::machine::fixtures::analyses().about(outside);
        let stats = RedundantLoad.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("{errors:#?}");
        }
        (module, stats)
    }

    fn one(module: &Module) -> &Func {
        &module[module.funcs().next().expect("one function")]
    }

    /// How many instructions with that opcode the function has left.
    fn count_of(func: &Func, opcode: Opcode) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
            .filter(|&inst| func[inst].opcode == opcode)
            .count()
    }

    /// Nothing anywhere in the function is on the chain.
    fn off(func: &Func) {
        for block in func.blocks() {
            assert!(func[block].params.iter().all(|&param| !func[param].ty.is_mem()));
            for inst in func.insts(block) {
                assert_ne!(func[inst].opcode, Opcode::MemEntry);
                assert!(!func.carries_mem(inst));
            }
        }
    }

    #[test]
    fn a_store_in_a_block_above_the_load_reaches_it() {
        // The case the one block version was built not to handle, and the reason this pass is
        // worth its two walks.
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    %2 = iconst.i32 7
    store %2 -> %0, align 4
    br_if %1, block1, block2

block1:
    jump block2

block2:
    %3 = load.i32 %0, align 4
    return %3
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, FORWARDED), 1);
        let func = one(&module);
        off(func);
        assert_eq!(count_of(func, Opcode::Load), 0, "the load is still there");
        // What the function returns is now the constant the store wrote.
        let ret = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
            .find(|&inst| func[inst].opcode == Opcode::Return)
            .expect("a return");
        let returned = func[func[ret].args][0];
        let seven = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
            .find(|&inst| func[inst].opcode == Opcode::IConst)
            .expect("the constant");
        assert_eq!(returned, func[seven].results().next().expect("a result"));
    }

    #[test]
    fn a_store_down_only_one_arm_is_not_an_answer() {
        // One path into the join wrote it and the other did not, and a disagreement is `Unknown`
        // rather than the weaker of the two, because there is no order on these to act on.
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    br_if %1, block1, block2

block1:
    %2 = iconst.i32 7
    store %2 -> %0, align 4
    jump block3

block2:
    jump block3

block3:
    %3 = load.i32 %0, align 4
    return %3
",
        );
        let (module, stats) = run(&text);
        assert!(!stats.changed(), "a store on one path is not the value on both");
        assert_eq!(stats.count(crate::stats::Kind::Missed, UNKNOWN), 1);
        off(one(&module));
    }

    #[test]
    fn both_arms_storing_the_same_way_is_still_two_stores() {
        // Two stores of the same value are two instructions, so the paths name two different
        // clobbers and disagree. Taking this one wants value numbering over the stores rather
        // than a better walk, and it is worth a test saying which of the two it needs.
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    %2 = iconst.i32 7
    br_if %1, block1, block2

block1:
    store %2 -> %0, align 4
    jump block3

block2:
    store %2 -> %0, align 4
    jump block3

block3:
    %3 = load.i32 %0, align 4
    return %3
",
        );
        let (_, stats) = run(&text);
        assert!(!stats.changed());
    }

    #[test]
    fn a_loop_that_writes_nothing_keeps_the_store_above_it() {
        // The back edge leads to the parameter the walk started from, which is how a cycle is cut
        // and contributes nothing, so what is left is the one path that wrote it.
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    %2 = iconst.i32 7
    store %2 -> %0, align 4
    jump block1

block1:
    %3 = load.i32 %0, align 4
    br_if %1, block1, block2

block2:
    return %3
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, FORWARDED), 1);
        assert_eq!(count_of(one(&module), Opcode::Load), 0);
    }

    #[test]
    fn a_store_inside_the_loop_stops_it() {
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    %2 = iconst.i32 7
    store %2 -> %0, align 4
    jump block1

block1:
    %3 = load.i32 %0, align 4
    %4 = add %3, %3
    store %4 -> %0, align 4
    br_if %1, block1, block2

block2:
    return %3
",
        );
        let (_, stats) = run(&text);
        assert!(!stats.changed(), "the body writes what the load reads");
    }

    #[test]
    fn the_bytes_being_the_same_is_not_the_type_being_the_same() {
        // Section 16.6's most likely wrong answer. Four bytes stored and four bytes read is the
        // same run of memory and two different readings of it, and this pass takes neither.
        let text = wrap(
            "(ptr) -> f32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    %2 = load.f32 %0, align 4
    return %2
",
        );
        let (_, stats) = run(&text);
        assert!(!stats.changed());
        assert_eq!(stats.count(crate::stats::Kind::Missed, WIDTH), 1);
    }

    #[test]
    fn the_same_address_read_twice_over_is_read_once() {
        // Nothing in the function writes memory at all, so the walk says nobody wrote it for both
        // of these and has no value for either. The two of them are still the same value.
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = load.i32 %0, align 4
    %2 = load.i32 %0, align 4
    %3 = add %1, %2
    return %3
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        let func = one(&module);
        off(func);
        assert_eq!(count_of(func, Opcode::Load), 1);
    }

    #[test]
    fn a_check_between_them_is_not_a_write() {
        // What this is worth on a safety build, which is the shape above with the check the
        // instrumentation puts in front of the second access. A check reads the planes and writes
        // nothing, so the version of memory the second load carries is the first one's.
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = load.i32 %0, align 4
    %2 = cap_of %0
    check_bounds %2, %0, size 4, align 4
    %3 = load.i32 %0, align 4
    %4 = add %1, %3
    return %4
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        let func = one(&module);
        assert_eq!(count_of(func, Opcode::Load), 1);
        assert_eq!(count_of(func, Opcode::CheckBounds), 1);
    }

    #[test]
    fn a_write_that_may_be_the_same_address_ends_it() {
        // The two pointers are two parameters and neither of them is restrict, so the store may be
        // to the same place. The store is a def of memory, so the second load carries a version the
        // first one never had and the table cannot match it, which is the check being the version
        // rather than a list of what the pass thinks is in the way.
        let text = wrap(
            "(ptr, ptr) -> i32",
            "block0(%0: ptr, %1: ptr):
    %2 = load.i32 %0, align 4
    %3 = iconst.i32 7
    store %3 -> %1, align 4
    %4 = load.i32 %0, align 4
    %5 = add %2, %4
    return %5
",
        );
        let (module, stats) = run(&text);
        assert!(!stats.changed());
        assert_eq!(count_of(one(&module), Opcode::Load), 2);
    }

    #[test]
    fn one_arm_reading_it_is_not_the_other_arm_having_read_it() {
        // Nothing here writes memory, so both loads carry the version the function started with and
        // the table matches. Neither block runs before the other, which is what the dominance is
        // there to say, and without it this is a use of a value that is not in scope.
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    br_if %1, block1, block2

block1:
    %2 = load.i32 %0, align 4
    jump block3(%2)

block2:
    %3 = load.i32 %0, align 4
    jump block3(%3)

block3(%4: i32):
    return %4
",
        );
        let (module, stats) = run(&text);
        assert!(!stats.changed());
        assert_eq!(count_of(one(&module), Opcode::Load), 2);
    }

    #[test]
    fn a_load_above_the_branch_reaches_both_arms() {
        // The same shape the other way up, where the first load is on every path to the other two.
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    %2 = load.i32 %0, align 4
    br_if %1, block1, block2

block1:
    %3 = load.i32 %0, align 4
    jump block3(%3)

block2:
    %4 = load.i32 %0, align 4
    jump block3(%4)

block3(%5: i32):
    %6 = add %2, %5
    return %6
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 2);
        let func = one(&module);
        off(func);
        assert_eq!(count_of(func, Opcode::Load), 1);
    }

    #[test]
    fn a_volatile_load_has_to_happen() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    %2 = load.i32.volatile %0, align 4
    return %2
",
        );
        let (module, stats) = run(&text);
        assert!(!stats.changed());
        assert_eq!(count_of(one(&module), Opcode::Load), 1);
    }

    #[test]
    fn a_function_with_no_memory_in_it_is_left_alone() {
        let text = wrap(
            "(i32) -> i32",
            "block0(%0: i32):
    %1 = add %0, %0
    return %1
",
        );
        let (module, stats) = run(&text);
        assert!(stats.is_empty(), "there was nothing here to say anything about");
        off(one(&module));
    }

    #[test]
    fn out_of_fuel_keeps_the_load_and_still_counts_it() {
        // The count of what could have gone is the same at every fuel setting, which is what
        // makes a bisection over it monotonic.
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    %2 = load.i32 %0, align 4
    return %2
",
        );
        let mut names = Interner::new();
        let mut module = parse(&text, &mut names).expect("the text parses");
        let id = module.funcs().next().expect("one function");
        let outside = Arc::new(Outside::of(&module));
        let mut an = crate::machine::fixtures::analyses().about(outside);
        let stats = RedundantLoad.run(&mut module[id], &mut an, &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(stats.count(crate::stats::Kind::Missed, NO_FUEL), 1);
        off(&module[id]);
    }

    #[test]
    fn what_the_walks_cost_is_written_down() {
        // Section 9.3 asks for the fraction that ran out of budget by name, and a number nothing
        // reports is a number nobody will look at.
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    %2 = load.i32 %0, align 4
    return %2
",
        );
        let (_, stats) = run(&text);
        assert_eq!(stats.count(crate::stats::Kind::Note, WALKS), 1);
        assert_eq!(stats.count(crate::stats::Kind::Note, STEPS), 1);
        assert_eq!(stats.count(crate::stats::Kind::Note, EXHAUSTED), 0);
    }

    #[test]
    fn the_safety_instrumentation_between_them_does_not_stop_the_forward() {
        // What a safety build looks like by the time the optimizer sees it, which is the shape
        // above with the lifetime plane written and then read between the store and the load.
        // Both of those are on the memory chain and both have `%0` as an operand, so the walk
        // goes through them and has to be told they are not about `%0`.
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    %2 = iconst.i32 7
    store %2 -> %0, align 4
    %3 = iconst.i64 4
    meta_init %0, %3
    br_if %1, block1, block2

block1:
    %4 = cap_of %0
    check_bounds %4, %0, size 4, align 4
    jump block2

block2:
    %5 = load.i32 %0, align 4
    return %5
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, FORWARDED), 1);
        let func = one(&module);
        off(func);
        assert_eq!(count_of(func, Opcode::Load), 0);
        // And the instrumentation is still there, because this pass forwards loads and is not
        // entitled to an opinion about whether a check was worth running.
        assert_eq!(count_of(func, Opcode::MetaInit), 1);
        assert_eq!(count_of(func, Opcode::CheckBounds), 1);
    }
}
