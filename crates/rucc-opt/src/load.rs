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
//! part that can be written without an alias oracle, and the part whose cost is one walk over each
//! block.
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
//! # What throws the table away
//!
//! Anything that could write anywhere, and what that means is the alias oracle's answer. A call, a
//! store and a safety plane access each take out the entries the oracle says they may write and
//! leave the rest standing. Everything else that touches memory, which is an atomic, a fence, a
//! `memcpy` and anything else [`Opcode::touches_memory`] is true of, empties the table and records
//! nothing. That predicate is the conservative one, so an opcode added to the IR later throws the
//! table away rather than being quietly assumed harmless.
//!
//! The plane access is there because of what it costs to leave it out. On a build with
//! `-fsafety=detect` there is a `meta_` or a `check_` beside almost every access in the program, so
//! a pass that empties the table at each of them has an empty table almost all of the time. None of
//! them is a write to the address it names, which is what [`Opcode::touches_only_planes`] says and
//! what the oracle now answers with.
//!
//! The oracle arrived late and this pass is the first consumer it has ever had. Until tamnd/rucc#1467
//! a call and a store both emptied the whole table, because `crate::alias` wanted the module and a
//! pass is handed one function. What the whole table cost was the second address: `*p = v; total +=
//! *q;` with two locals was refused even where the two cannot be the same object. It is not refused
//! now.
//!
//! What the oracle is worth here rests on the escape analysis more than on anything else in it.
//! `spec/optimizer/08-alias-analysis.md` section 8.4 calls that the cheapest interprocedural
//! flavoured fact there is, and it is what answers the ordinary case: a local whose address never
//! leaves the function cannot be touched by any call in it, whatever the callee does.
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

/// What this pass is called, which the pipeline matches on to decide whether to build the module
/// facts the oracle asks for.
pub const NAME: &str = "load-forward";

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

        // The oracle borrows the function, so the walk that reads it is a scope of its own and
        // every edit happens after it. Built once, because the escape analysis inside it is one
        // walk over the function and every query may ask it.
        {
            let mut alias = Alias::new(func, an.outside());
            for block in func.blocks().collect::<Vec<Block>>() {
                let mut known: HashMap<Value, Held> = HashMap::new();
                for inst in func.insts(block).collect::<Vec<Inst>>() {
                    match act(func, inst) {
                        Act::Ignore => {}
                        Act::Forget => known.clear(),
                        Act::Ask => {
                            known.retain(|_, held| alias.clobbered_by(&held.access, inst).is_no());
                        }
                        Act::Wrote { address, value, ty } => {
                            // The entries this store may have written go, and the rest stay. Its
                            // own goes in after, so that the address it just wrote survives its own
                            // clearing whatever the oracle made of it.
                            let Some(wrote) = alias.writes(inst) else {
                                known.clear();
                                continue;
                            };
                            known.retain(|_, held| alias.query(&held.access, &wrote).is_no());
                            known.insert(address, Held { ty, value, stored: true, access: wrote });
                        }
                        Act::Read { address, result, ty } => {
                            match known.get(&address).copied() {
                                Some(held) if held.ty == ty => {
                                    if fuel.take() {
                                        forward.insert(result, held.value);
                                        gone.push(inst);
                                        stats.optimized(if held.stored {
                                            FORWARDED
                                        } else {
                                            REUSED
                                        });
                                        continue;
                                    }
                                    // Out of fuel, which is a request to stop transforming and not
                                    // to stop looking. The walk goes on so that the count of what
                                    // could have gone is the same at every fuel setting, which is
                                    // what makes a bisection over it monotonic.
                                    stats.missed(NO_FUEL);
                                }
                                Some(_) => stats.missed(WIDTH),
                                None => {}
                            }
                            // A load with no access is a load the oracle could say nothing about
                            // later, so it is not recorded at all rather than recorded as
                            // something no call can be asked about.
                            if let Some(read) = alias.reads(inst) {
                                known.insert(
                                    address,
                                    Held { ty, value: result, stored: false, access: read },
                                );
                            }
                        }
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
    /// Which bytes it is, for asking the oracle whether a call or a store reaches them.
    ///
    /// The access of the instruction that put the entry here, which is the same address and the
    /// same width as any load that will match it, since matching is by address value and by type.
    access: Access,
}

/// What one instruction does to the table.
enum Act {
    /// Nothing. It touches no memory.
    Ignore,
    /// It could write anywhere the oracle cannot rule out, and nothing here says what it wrote.
    Forget,
    /// The oracle is asked what it wrote, because nothing about its shape says.
    Ask,
    /// It writes this value of this type at this address, and may write elsewhere.
    Wrote { address: Value, value: Value, ty: Type },
    /// It reads a value of this type from this address into this result.
    Read { address: Value, result: Value, ty: Type },
}

/// Which of the five an instruction is.
///
/// The two interesting cases are narrow on purpose. A plain non-volatile `Load` with one address
/// and one result, and a plain non-volatile `Store` of one value to one address. `AtomicLoad` and
/// `AtomicStore` are separate opcodes in this IR and are not these, so an ordering never reaches
/// here as something to forward, and neither does a load carrying a memory token, which is what
/// more than one result would mean.
///
/// `Ask` is what an instruction that writes memory without an access saying where gets, and the
/// oracle is asked a different question about one of those: `Alias::clobbered_by` rather than
/// `Alias::query`, since there is no access of its own to hand over. A call is the obvious member
/// and what it may write is what its attributes and its arguments say. The safety instrumentation
/// is the other one, and there the answer is about the opcode rather than about the callee: a
/// plane write is not a write to the address it names, which is what [`Opcode::touches_only_planes`]
/// is for. Sending those to `Forget` instead is what this pass used to do, and it meant that on a
/// build with `-fsafety=detect` the table was emptied beside almost every access and the pass did
/// close to nothing, which is tamnd/rucc#1501.
fn act(func: &Func, inst: Inst) -> Act {
    let data = &func[inst];
    if !data.opcode.touches_memory() {
        return Act::Ignore;
    }
    if data.flags.contains(Flags::VOLATILE) {
        return Act::Forget;
    }
    let args = &func[data.args];
    match data.opcode {
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => Act::Ask,
        other if other.touches_only_planes() => Act::Ask,
        Opcode::Load => {
            let mut results = data.results();
            let (Some(&address), Some(result), None) =
                (args.first(), results.next(), results.next())
            else {
                return Act::Forget;
            };
            Act::Read { address, result, ty: func[result].ty }
        }
        Opcode::Store => {
            let (Some(&value), Some(&address)) = (args.first(), args.get(1)) else {
                return Act::Forget;
            };
            Act::Wrote { address, value, ty: func[value].ty }
        }
        _ => Act::Forget,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rucc_base::Interner;
    use rucc_ir::{
        AttrSet, Attrs, Block, Builder, Extra, Flags, Func, InstData, MemInfo, MemOrder, Module,
        Restrict, Signature, Type, Value,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::*;
    use crate::Fuel;
    use crate::outside::Outside;

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

    /// Runs the pass over the function with as much fuel as it wants and no module facts.
    fn run(func: &mut Func) -> Stats {
        LoadForward.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// The same, with what the oracle would know if this function were in that module.
    fn run_in(func: &mut Func, module: &Module) -> Stats {
        let mut an = crate::machine::fixtures::analyses().about(Arc::new(Outside::of(module)));
        LoadForward.run(func, &mut an, &mut Fuel::unlimited())
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

        // The escape analysis and nothing else. `g` is a name this module has never heard of and
        // it could do anything at all, and it still cannot reach an address it was never given.
        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(loads(&func), 1);
        assert_eq!(returned(&func), vec![first, first]);
    }

    #[test]
    fn a_call_handed_the_address_is_a_write_to_it() {
        let (mut names, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        build.call(names.intern("g"), signature, &[slot]);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // The address escaped into the call, so the callee has it and nothing here says what it
        // did with it. This is the half of the previous test that must not move.
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(loads(&func), 2);
    }

    #[test]
    fn a_store_to_an_address_that_cannot_be_this_one_leaves_it_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let other = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.store(first, other, plain(8), Flags::NONE);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // Two allocas cannot be the same object, which is the oracle's first layer and the one
        // that answers most of what real code asks. Until tamnd/rucc#1467 this was left on the
        // table with a comment saying so.
        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(loads(&func), 1);
        assert_eq!(returned(&func), vec![first, first]);
    }

    #[test]
    fn a_store_the_oracle_cannot_place_takes_the_table_with_it() {
        let (mut names, mut func, entry) = blank();
        let elsewhere = func.append_param(entry, Type::PTR);
        let mut build = Builder::new(&mut func, entry);
        let slot = local(&mut build);
        // Handed out, so the local is one somebody else's pointer could be naming.
        let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        build.call(names.intern("g"), signature, &[slot]);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.store(first, elsewhere, plain(8), Flags::NONE);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // A pointer handed in is an address the walk cannot follow back to an object, and this
        // local's address did leave the function, so the escape layer has nothing to say either.
        // Nothing left says these are two objects, so the store may be to this one.
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(loads(&func), 2);
    }

    #[test]
    fn a_store_through_a_pointer_from_nowhere_cannot_reach_a_local_that_stayed_here() {
        let (_, mut func, entry) = blank();
        let elsewhere = func.append_param(entry, Type::PTR);
        let mut build = Builder::new(&mut func, entry);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.store(first, elsewhere, plain(8), Flags::NONE);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // The other half of the test above. Nothing ever handed this address out, so no pointer
        // this function cannot follow is naming it, whatever that pointer is.
        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(loads(&func), 1);
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
    fn the_safety_instrumentation_between_two_reads_leaves_the_table_standing() {
        // A safety build puts a plane write and then a plane read beside almost every access, and
        // for as long as those emptied the table this pass did close to nothing at
        // `-fsafety=detect`. Neither is a write to the address it names, so the second load is
        // still the first one's value.
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let width = build.iconst(Type::int(64), 8);
        let args = build.func().push_values(&[slot, width]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaInit) }, &[]);
        let args = build.func().push_values(&[slot]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = build.func().push_values(&[capability, slot]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckLive) }, &[]);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        let stats = run(&mut func);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(loads(&func), 1);
        assert_eq!(returned(&func), vec![first, first]);
    }

    #[test]
    fn a_fence_between_two_reads_still_empties_the_table() {
        // The other side of the line above. A fence touches memory, says nothing about where, and
        // is not one of the plane opcodes, so it falls where everything unrecognized falls.
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.inst(InstData::new(Opcode::Fence), &[]);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(loads(&func), 2);
    }

    #[test]
    fn a_volatile_plane_access_is_read_as_volatile_first() {
        // The volatile test comes before the opcode does, and it has to stay that way: an access
        // the program asked to happen exactly as written happens, whatever the opcode would have
        // said about which memory it is.
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let width = build.iconst(Type::int(64), 8);
        let args = build.func().push_values(&[slot, width]);
        build.inst(
            InstData { args, flags: Flags::VOLATILE, ..InstData::new(Opcode::MetaInit) },
            &[],
        );
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(loads(&func), 2);
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
    fn a_call_to_something_declared_to_read_no_memory_writes_none_either() {
        // The module's half of the oracle, which is the one thing a pass handed a function cannot
        // work out for itself. The address escaped into an earlier call, so the escape layer has
        // nothing left to say and what answers is `g` being declared `const`.
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let quiet = names.intern("g");
        let mut callee = Func::new(quiet, Signature::new().with_params(&[Type::PTR]));
        callee.attrs = Attrs { set: AttrSet::READNONE, ..Attrs::default() };
        module.add_func(callee);

        let name = names.intern("f");
        let build = || {
            let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(64)]));
            let block = func.create_block();
            let mut build = Builder::new(&mut func, block);
            let slot = local(&mut build);
            let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
            build.call(quiet, signature, &[slot]);
            let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
            build.call(quiet, signature, &[slot]);
            let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
            build.ret(&[first, second]);
            func
        };

        assert!(!run(&mut build()).changed(), "without the module there is nothing to read");
        let mut func = build();
        let stats = run_in(&mut func, &module);
        assert_eq!(stats.count(crate::stats::Kind::Optimized, REUSED), 1);
        assert_eq!(loads(&func), 1);
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
