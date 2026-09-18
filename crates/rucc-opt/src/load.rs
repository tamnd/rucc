//! A load of something the block has already read or written is that value, not a second read.
//!
//! Design: `spec/optimizer/16-gvn-and-pre.md` section 16.2, which calls redundant load elimination
//! the real prize of that document. Value numbering over arithmetic is worth less than people
//! expect on C, because the front end does not generate the same expression twice and the
//! programmer does not write it twice. Loads are different. `p->x` three times in a function is
//! three reads of memory, and if nothing wrote through an aliasing pointer in between, two of them
//! are work the program does not have to do.
//!
//! # The restricted version, and why this is it
//!
//! Section 16.2 asks for two of these. The one at `-O2` walks memory SSA back from each load to
//! its clobbering definition, translates an address backwards through a block's parameters, and
//! sees through a `memcpy`. The one here is the other: same block only, no phi translation, no
//! `memcpy`, and no memory SSA at all. That is the version the section says should be at `-O1`,
//! and it says why: it catches the repeated `p->x` in one basic block, which is the majority of the
//! opportunities, for a fraction of the machinery.
//!
//! The two are not alternatives and this is not a stand-in for the other one. What it is is the
//! part whose cost is one walk over each block, and it is the first pass in the pipeline to ask
//! the alias analysis anything.
//!
//! # What it knows
//!
//! One table per block, from an address to the value that address holds, thrown away at the end of
//! the block because what reaches a block from its predecessors is the question this version does
//! not ask. A store writes what it stored. A load that had to happen writes what it read. Either
//! way the next load of that address is that value.
//!
//! An address is one SSA value, compared by identity. Two pointers that are the same address by
//! arithmetic and not by name are two addresses here, which costs opportunities and no
//! correctness: an address computed twice out of the same parts is tracked twice and neither copy
//! is forwarded to the other. What would give the two one name is value numbering over the
//! arithmetic, which is the other half of document 16 and is not built. It costs more than it
//! sounds like it should. `a[i] = v; total += a[i];` written in C is a store and a load whose
//! addresses are two separate runs of the same multiply and add, because the front end emits the
//! subscript twice, so the shape this pass is most obviously for is one it cannot see until that
//! lands.
//!
//! # What throws the table away, and what only takes entries out of it
//!
//! A store and a call ask [`crate::alias`] which entries they could have written and take those
//! out, leaving the rest. `*p = v; total += *q;` with two locals keeps the entry for `q`, and a
//! call keeps every entry for a local whose address never left the function, which is the escape
//! layer of section 8.4 and the cheapest interprocedural flavoured fact there is. The oracle is
//! built once for the function, since the escape analysis inside it is one walk and every query
//! wants it.
//!
//! Everything else that touches memory still empties the whole table, which is an atomic, a
//! fence, a `memcpy` and a volatile access of either kind. That is [`Opcode::touches_memory`]
//! with the plain store and the call taken out of it, so an opcode added to the IR later empties
//! the table rather than being quietly assumed harmless.
//!
//! The type-based layer is on, and `-fno-strict-aliasing` still works, because the flag is
//! applied where the accesses are made rather than here: `rucc_lower` leaves the type off every
//! access when it is given, so the layer has nothing to answer with.
//!
//! A volatile access empties the table and records nothing either way. Whether a volatile store
//! could be forwarded from is an argument about what `volatile` promises, and this pass does not
//! need to have it.
//!
//! # The width, which is where the miscompilation would be
//!
//! Section 16.6 names it as the single most likely wrong answer in that document: a load forwarded
//! from a store of a different size. Section 09.5 has the three-way distinction, which is that a
//! store covering the load exactly is the value, one covering it partially needs an extract, and
//! one not covering it at all means the walk should continue.
//!
//! This pass only ever takes the first of the three. The address has to be the same SSA value and
//! the type has to be equal, which is the same width and the same reading of the bits, and
//! anything else is left alone and counted. Two-way is what somebody writes first and it is right
//! most of the time, which is what makes it worth being explicit that this is not that.
//!
//! # What it leaves behind
//!
//! The load goes, rather than staying and having its result forwarded. [`crate::dce`] would not
//! remove it: `has_effects` is true of every load, because a pass that removed one would need to
//! know the address is dereferenced anyway, and this is the pass that knows it. The load being
//! removed is safe for a reason nothing else in the pipeline has: something already read or wrote
//! that exact address in this block, so the address is one the program dereferences whatever
//! happens next.
//!
//! The store stays. Removing a store that a later store covers is dead store elimination, which is
//! document 17 and a different pass.

use std::collections::HashMap;

use rucc_ir::{Block, Flags, Func, Inst, Opcode, Type, Value};

use crate::alias::{Access, Alias};
use crate::uses::substitute;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// Recorded for a load that read what a store in the same block had just written.
const FORWARDED: &str = "load replaced by the value a store in the same block wrote there";

/// Recorded for a load of an address an earlier load in the same block had already read.
const REUSED: &str = "load replaced by what an earlier load of the same address read";

/// Recorded for a load whose address is known and whose type is not the type it is known at.
const WIDTH: &str = "load kept, what is known about that address is a different type";

/// Recorded for a load that would have gone if there had been fuel for it.
const NO_FUEL: &str = "redundant load kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug)]
pub struct LoadForward;

impl Pass for LoadForward {
    fn name(&self) -> &'static str {
        "load-forward"
    }

    fn describe(&self) -> &'static str {
        "a load of an address the block has already read or written is that value"
    }

    fn preserves(&self) -> Preserved {
        // The shape of the function. No block is added, none is removed, no edge moves, and the
        // instructions that go are loads, which are never terminators.
        //
        // The liveness is the one thing that does move, for the reason `crate::simplify` gives:
        // pointing every reader of one value at another is one more place the second is live and
        // one fewer the first is.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        // What each removed load's result is read as, applied to the whole function once at the
        // end. Rewriting each one where it is found would be a walk over the function per load,
        // and there is nothing to gain by it: what this pass looks at is the address, and a
        // redirection of a result does not change one.
        let mut forward: HashMap<Value, Value> = HashMap::new();
        let mut gone: Vec<Inst> = Vec::new();
        // Built once for the whole function rather than once per block, because the escape
        // analysis inside it is a walk over the function and the answer does not vary by block.
        let mut alias = Alias::new(func, an.surroundings());

        for block in func.blocks().collect::<Vec<Block>>() {
            let mut known: HashMap<Value, Held> = HashMap::new();
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                match act(func, &alias, inst) {
                    Act::Ignore => {}
                    Act::Forget => known.clear(),
                    Act::Called => {
                        known.retain(|_, held| alias.clobbered_by(&held.access, inst).is_no());
                    }
                    Act::Wrote { address, value, ty, access } => {
                        // The entries this store could have written go first and the store's own
                        // goes in second, so that its address survives the invalidation its own
                        // write caused.
                        known.retain(|_, held| alias.query(&held.access, &access).is_no());
                        known.insert(address, Held { ty, value, stored: true, access });
                    }
                    Act::Read { address, result, ty, access } => {
                        match known.get(&address).copied() {
                            Some(held) if held.ty == ty => {
                                if fuel.take() {
                                    forward.insert(result, held.value);
                                    gone.push(inst);
                                    stats.optimized(if held.stored { FORWARDED } else { REUSED });
                                    continue;
                                }
                                // Out of fuel, which is a request to stop transforming and not to
                                // stop looking. The walk goes on so that the count of what could
                                // have gone is the same at every fuel setting, which is what makes
                                // a bisection over it monotonic.
                                stats.missed(NO_FUEL);
                            }
                            Some(_) => stats.missed(WIDTH),
                            None => {}
                        }
                        known.insert(address, Held { ty, value: result, stored: false, access });
                    }
                }
            }
        }
        for inst in gone {
            func.remove_inst(inst);
        }
        if !forward.is_empty() {
            substitute(func, &forward);
        }
        stats
    }
}

/// What the block knows about one address.
///
/// The value and the type are what the forwarding turns on. Whether it was stored or read is only
/// for the counters, and they want it because the two numbers answer different questions. A
/// forward from a store says the program wrote something and read it straight back, which is what
/// an unrolled loop over an array looks like and what the constant folder can usually finish off.
/// A reuse says the program read the same place twice, which is the `p->x` this pass is named for
/// and which folds into nothing at all.
#[derive(Clone, Copy)]
struct Held {
    /// The type the address is known at, which a load has to match exactly to be that value.
    ty: Type,
    /// What the address holds.
    value: Value,
    /// Whether a store put it there, rather than a load having read it.
    stored: bool,
    /// The reference the access that established this made, which is what a later store or call
    /// is asked about. It is kept rather than rebuilt because the instruction it came from may be
    /// one this pass is about to remove.
    access: Access,
}

/// What one instruction does to the table.
enum Act {
    /// Nothing. It touches no memory.
    Ignore,
    /// It could write anywhere, so nothing the table says is known any more.
    Forget,
    /// It is a call, so what it could have written is a question for the oracle, one entry at a
    /// time.
    Called,
    /// It writes this value of this type at this address, covering these bytes.
    Wrote { address: Value, value: Value, ty: Type, access: Access },
    /// It reads a value of this type from this address into this result, covering these bytes.
    Read { address: Value, result: Value, ty: Type, access: Access },
}

/// Which of the five an instruction is.
///
/// The two interesting cases are narrow on purpose. A plain non-volatile `Load` with one address
/// and one result, and a plain non-volatile `Store` of one value to one address. `AtomicLoad` and
/// `AtomicStore` are separate opcodes in this IR and are not these, so an ordering never reaches
/// here as something to forward, and neither does a load carrying a memory token, which is what
/// more than one result would mean.
///
/// A call that returns by way of a pointer it was handed is a call like any other here, because
/// the handing over is what the escape analysis sees and the answer for that local is then that
/// the call may write it.
fn act(func: &Func, alias: &Alias<'_>, inst: Inst) -> Act {
    let data = &func[inst];
    if !data.opcode.touches_memory() {
        return Act::Ignore;
    }
    if data.flags.contains(Flags::VOLATILE) {
        return Act::Forget;
    }
    let args = &func[data.args];
    match data.opcode {
        Opcode::Load => {
            let mut results = data.results();
            let (Some(&address), Some(result), None, Some(access)) =
                (args.first(), results.next(), results.next(), alias.reads(inst))
            else {
                return Act::Forget;
            };
            Act::Read { address, result, ty: func[result].ty, access }
        }
        Opcode::Store => {
            let (Some(&value), Some(&address), Some(access)) =
                (args.first(), args.get(1), alias.writes(inst))
            else {
                return Act::Forget;
            };
            Act::Wrote { address, value, ty: func[value].ty, access }
        }
        Opcode::Call | Opcode::TailCall => Act::Called,
        _ => Act::Forget,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Extra, Flags, Func, InstData, MemInfo, MemOrder, Restrict, Signature, Type,
        Value,
    };

    use super::*;
    use crate::Fuel;

    /// An empty function with one block, which is where every test below builds.
    fn blank() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(64)]));
        let block = func.create_block();
        (names, func, block)
    }

    /// An ordinary access of that alignment, with nothing said about its type.
    fn plain(align: u32) -> MemInfo {
        MemInfo {
            size: 0,
            align,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        }
    }

    /// An `alloca` of eight bytes, which is an address nothing outside the function knows.
    fn local(build: &mut Builder<'_>) -> Value {
        let mem = build.func().add_mem(MemInfo { size: 8, ..plain(8) });
        build.value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// Runs the pass over the function with as much fuel as it wants.
    fn run(func: &mut Func) -> Stats {
        LoadForward.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// How many loads are left in the function.
    fn loads(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
            .filter(|&inst| func[inst].opcode == Opcode::Load)
            .count()
    }

    /// What the return statement hands back, after the pass has pointed it somewhere.
    fn returned(func: &Func) -> Vec<Value> {
        let block = func.blocks().next().expect("the function has a block");
        let inst = func.terminator(block).expect("the block has a terminator");
        func[func[inst].args].to_vec()
    }

    #[test]
    fn a_load_of_what_a_store_just_wrote_is_the_stored_value() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let wrote = build.iconst(Type::int(64), 7);
        build.store(wrote, slot, plain(8), Flags::NONE);
        let read = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[read]);

        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, FORWARDED), 1);
        assert_eq!(loads(&func), 0, "the load itself has to go, nothing else would remove it");
        assert_eq!(returned(&func), vec![wrote]);
    }

    #[test]
    fn the_second_load_of_an_address_is_what_the_first_one_read() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let sum = build.binary(Opcode::Add, first, second, Flags::NONE);
        build.ret(&[sum]);

        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(loads(&func), 1, "one read of that address has to happen and only one");
        let sum = returned(&func)[0];
        let rucc_ir::Def::Result { inst, .. } = func[sum].def else { panic!("the add is gone") };
        assert_eq!(func[func[inst].args].to_vec(), vec![first, first]);
    }

    #[test]
    fn a_store_of_a_different_width_is_not_forwarded_through() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let wrote = build.iconst(Type::int(32), 7);
        build.store(wrote, slot, plain(4), Flags::NONE);
        let read = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[read]);

        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Missed, WIDTH), 1);
        assert_eq!(loads(&func), 1, "a four byte store does not say what eight bytes hold");
        assert_eq!(returned(&func), vec![read]);
    }

    #[test]
    fn a_call_cannot_touch_a_local_whose_address_never_left_the_function() {
        let (mut names, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let signature = build.func().add_signature(Signature::new());
        build.call(names.intern("g"), signature, &[]);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // Nothing is said about `g` at all, and nothing has to be. The address of the slot is
        // never handed to anything, so there is no way for `g` to have it.
        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(loads(&func), 1);
        assert_eq!(returned(&func), vec![first, first]);
    }

    #[test]
    fn a_call_handed_the_address_of_the_local_can_write_it() {
        let (mut names, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        build.call(names.intern("g"), signature, &[slot]);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // The same function with the one difference that matters. Handing the address over is
        // what the escape analysis is looking for, and after it the honest answer is that the
        // call may have written through it.
        let stats = run(&mut func);
        assert!(!stats.changed(), "the call was given the address, so it may have written there");
        assert_eq!(loads(&func), 2);
    }

    #[test]
    fn a_store_to_a_local_that_cannot_be_the_other_one_leaves_it_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let other = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.store(first, other, plain(8), Flags::NONE);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // Two allocas are two objects, which is the first layer of the oracle and the one that
        // needs nothing analysed.
        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(loads(&func), 1);
        assert_eq!(returned(&func), vec![first, first]);
    }

    #[test]
    fn a_store_through_an_address_that_could_be_the_same_one_is_a_write_to_it() {
        let (_, mut func, block) = blank();
        // Two pointers this function was handed. Neither walks back to an object, and nothing
        // says they are two, so the honest answer is that the store could have been to `p`.
        let p = func.append_param(block, Type::PTR);
        let q = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let first = build.load(Type::int(64), p, plain(8), Flags::NONE);
        build.store(first, q, plain(8), Flags::NONE);
        let second = build.load(Type::int(64), p, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        let stats = run(&mut func);
        assert!(!stats.changed(), "the store may have been to the same place");
        assert_eq!(loads(&func), 2);
    }

    #[test]
    fn a_store_past_the_end_of_what_the_load_read_is_not_a_write_to_it() {
        let (_, mut func, block) = blank();
        let p = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let first = build.load(Type::int(64), p, plain(8), Flags::NONE);
        let eight = build.iconst(Type::int(64), 8);
        let past = build.binary(Opcode::PtrAdd, p, eight, Flags::NONE);
        build.store(first, past, plain(8), Flags::NONE);
        let second = build.load(Type::int(64), p, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // One address, two offsets, and eight bytes from each of them do not meet. That is the
        // offset layer, and it is the one that does not care what either address points at.
        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(loads(&func), 1);
        assert_eq!(returned(&func), vec![first, first]);
    }

    #[test]
    fn a_volatile_load_is_not_reused_and_nothing_before_it_survives_it() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::VOLATILE);
        let second = build.load(Type::int(64), slot, plain(8), Flags::VOLATILE);
        let third = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second, third]);

        let stats = run(&mut func);
        assert!(!stats.changed(), "every volatile read has to happen");
        assert_eq!(loads(&func), 3);
    }

    #[test]
    fn what_one_block_knows_does_not_reach_the_next_one() {
        let (_, mut func, entry) = blank();
        let next = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.jump(next, &[]);
        let mut build = Builder::new(&mut func, next);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // The value is available and the `-O2` version of this pass finds it. Section 16.2 is
        // explicit that this one does not, because reaching it means memory SSA.
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(loads(&func), 2);
    }

    #[test]
    fn a_chain_of_reads_all_come_from_the_first_one() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let third = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[third]);

        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 2);
        assert_eq!(loads(&func), 1);
        assert_eq!(returned(&func), vec![first]);
    }

    #[test]
    fn a_store_of_a_value_the_pass_is_removing_forwards_to_where_that_value_went() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.store(second, slot, plain(8), Flags::NONE);
        let third = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[third]);

        // The store writes a value this pass is in the middle of taking away, so the third load
        // has to land one step further back than what the table says. Landing on the second load's
        // result would leave the function reading an instruction that is no longer in it.
        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, FORWARDED), 1);
        assert_eq!(loads(&func), 1);
        assert_eq!(returned(&func), vec![first]);
    }

    #[test]
    fn without_fuel_the_load_stays_and_the_chance_is_still_counted() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let wrote = build.iconst(Type::int(64), 7);
        build.store(wrote, slot, plain(8), Flags::NONE);
        let read = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[read]);

        let stats =
            LoadForward.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(stats.count(crate::stats::Kind::Missed, NO_FUEL), 1);
        assert_eq!(loads(&func), 1);
    }
}
