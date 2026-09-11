//! Two instructions in a block that compute the same thing from the same things are one value.
//!
//! Design: `spec/optimizer/16-gvn-and-pre.md` section 16.1. This is the other half of that
//! document, the half [`crate::load`] deliberately did not do. Section 16.2 is candid that value
//! numbering over arithmetic is worth less on C than people expect, because the front end does not
//! generate the same expression twice and the programmer does not write it twice. That is true of
//! the arithmetic somebody wrote. It is not true of the arithmetic the front end emits underneath
//! it, and the address of a subscript is the case that matters: `a[i] = v; total += a[i];` is one
//! subscript written twice in C and two separate runs of the same multiply and add in the IR,
//! because lowering a subscript does not know it has lowered that subscript already.
//!
//! So this pass mostly does not pay for itself in what it removes. It pays for itself in what it
//! lets the pass after it see. [`crate::load`] compares addresses by identity, so until the store's
//! address and the load's address have one name it cannot forward a store to the load that reads it
//! straight back, which is the shape it was written for. Giving them one name is this.
//!
//! # Block local, and why that is the whole of it
//!
//! One table per block, thrown away at the end of it. Inside a block an earlier instruction
//! dominates a later one because there is no other way to reach the later one, so program order is
//! the whole of the dominance question and there is no dominator tree here.
//!
//! The version over the dominator tree finds strictly more, and section 16.1 is where the argument
//! for not writing it lives: under arms B and C of the e-graph experiment, hash-consing gives the
//! acyclic case for nothing, and what is left over is the cyclic case, which wants Tarjan's
//! algorithm over the SSA graph and belongs after the e-graph is built rather than before it. What
//! is wanted before that exists is the part that makes the address of one subscript one value, and
//! both halves of a subscript are in the block the subscript is in.
//!
//! # What counts as the same thing
//!
//! The opcode, the flags, the result type, whatever the instruction carries besides its operands,
//! and the operands. All five, and a difference in any of them is two values.
//!
//! The flags are in the key rather than being merged or intersected. Two adds of the same pair
//! where one of them says its result cannot wrap and the other says nothing are two entries, and
//! the program keeps both. Merging them onto the one that promises more would hand the weaker
//! instruction a promise nobody made about it, and merging them onto the one that promises less
//! throws away something a later pass wanted. Keeping them apart costs an instruction that is
//! rarely there and is the answer that needs no argument.
//!
//! The operands are looked up through what this pass has already decided, so a value that has been
//! redirected onto an earlier one is compared as the earlier one. That is what lets a chain work:
//! once two multiplies are one, the two adds on top of them have the same operands and become one
//! too, and so does the address on top of those. A subscript is three or four instructions deep,
//! so without this the pass would collapse the bottom of it and stop.
//!
//! An operand is a value and not an expression, so `(a + b) + c` and `a + (b + c)` are two values
//! here. Making them one is reassociation, which is document 19 and a different pass.
//!
//! # What is allowed to move
//!
//! [`Opcode::has_effects`] answering no, which the IR defines as exactly the property this pass
//! needs: an instruction that answers no can be deleted when nothing reads it, moved across a call,
//! and merged with another one computing the same thing. It is written as a list of the pure
//! opcodes rather than a list of the impure ones, so an opcode added to the IR later is impure
//! until somebody says otherwise, and this pass leaves it alone.
//!
//! Three things beyond that are refused. `mem_entry` is pure and is not a computation, it is the
//! name of memory on the way in, and merging two of them is a question for [`crate::memssa`] rather
//! than an arithmetic identity. Anything producing other than exactly one result is refused, which
//! is the checked arithmetic, whose second result would need redirecting alongside the first and
//! which is not common enough to be worth the shape. Anything carrying something this pass cannot
//! compare by value is refused, which in practice is `blockaddr` and nothing else, every other
//! payload being on an opcode that has effects anyway.
//!
//! Division is on the allowed list and that is deliberate. Removing the second of two identical
//! divisions is safe for a reason that is only true block locally: the first one is in the same
//! block, so it has already run, and if it was going to trap the second one was never reached.
//!
//! # What it does not do
//!
//! Nothing crosses a block boundary, nothing goes through memory, and no instruction moves. A
//! duplicate is removed where it stands and its readers are pointed at the first one, which is
//! always above it. That means a computation in two arms of a branch stays in two arms: hoisting it
//! to the common predecessor is [`crate::hoist`], and it wants the profitability question this pass
//! does not ask.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_ir::{Block, Extra, Flags, FloatPred, Func, Inst, IntPred, Opcode, Type, Value};

use crate::uses::substitute;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// Recorded for a removed address computation, which is what the pass is here for.
const ADDRESS: &str = "address removed, an earlier one in the block computes the same address";

/// Recorded for any other removed duplicate.
const MERGED: &str = "instruction removed, an earlier one in the block computes the same thing";

/// Recorded for a duplicate that would have gone if there had been fuel for it.
const NO_FUEL: &str = "duplicate instruction kept, the pass ran out of fuel";

/// The most operands any pure opcode has, which is the three of `select` and `fma`.
const OPERANDS: usize = 3;

/// The pass.
#[derive(Debug)]
pub struct Number;

impl Pass for Number {
    fn name(&self) -> &'static str {
        "number"
    }

    fn describe(&self) -> &'static str {
        "two instructions in a block computing the same thing from the same things are one value"
    }

    fn preserves(&self) -> Preserved {
        // The shape of the function. No block is added, none is removed, no edge moves, and what
        // is removed is pure, which no terminator is.
        //
        // The liveness is the one thing that does move, for the reason `crate::simplify` gives:
        // pointing every reader of one value at another is one more place the second is live and
        // one fewer the first is.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        // What each removed instruction's result is read as. It is also what an operand is looked
        // up through while the block is being walked, which is why it is built as the walk goes
        // and applied to the function once at the end rather than either one alone.
        let mut same: HashMap<Value, Value> = HashMap::new();
        let mut gone: Vec<Inst> = Vec::new();

        for block in func.blocks().collect::<Vec<Block>>() {
            let mut seen: HashMap<Key, Value> = HashMap::new();
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                let Some((key, result)) = key(func, &same, inst) else { continue };
                let Some(&first) = seen.get(&key) else {
                    seen.insert(key, result);
                    continue;
                };
                if !fuel.take() {
                    // Out of fuel, which is a request to stop transforming and not to stop
                    // looking. The walk goes on so that the count of what could have gone is the
                    // same at every fuel setting, which is what makes a bisection over it
                    // monotonic. The table is left as it is, so the next duplicate of this same
                    // thing is counted against the same first instruction.
                    stats.missed(NO_FUEL);
                    continue;
                }
                same.insert(result, first);
                gone.push(inst);
                stats.optimized(if is_address(func[inst].opcode) { ADDRESS } else { MERGED });
            }
        }

        for inst in gone {
            func.remove_inst(inst);
        }
        if !same.is_empty() {
            substitute(func, &same);
        }
        stats
    }
}

/// Whether an opcode is one of the two that compute an address.
///
/// Only for the counters, which want the two numbers apart because they answer different
/// questions. The address count is what feeds [`crate::load`] and is the reason the pass exists.
/// The other count is whatever else happened to be written twice, which on real C is not much.
fn is_address(opcode: Opcode) -> bool {
    matches!(opcode, Opcode::PtrAdd | Opcode::GlobalAddr)
}

/// What an instruction computes, as something two instructions can be equal on.
///
/// Fixed size and `Copy`, because a hash table entry per pure instruction in the program is enough
/// work without an allocation for each of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    /// Which instruction it is.
    opcode: Opcode,
    /// What the optimizer was told it may assume about this one, which is not the same question as
    /// what it may assume about another one of the same shape.
    flags: Flags,
    /// The type of its one result, which is what tells two casts of the same value apart.
    ty: Type,
    /// Whatever it carries besides operands, compared by value rather than by where it is stored.
    tag: Tag,
    /// Its operands, resolved, padded with `None`, and put in order if the opcode does not care.
    args: [Option<Value>; OPERANDS],
}

/// An instruction's payload, as far as one can be compared with another.
///
/// [`Extra`] holds most of its payloads as an index into a side table, and two equal payloads
/// written at two times are two indices, so an equality on the index would answer no to a question
/// this pass is asking. This is the payload itself for the shapes a pure opcode has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Tag {
    /// Nothing, which is all of the arithmetic.
    None,
    /// A constant's bits, for `iconst`, `fconst` and `splat`. The immediate table is not interned,
    /// so this is the only reading under which two of the same constant are the same constant.
    Bits(u128),
    /// A name, for `global_addr`.
    Symbol(Symbol),
    /// Which comparison, for `icmp`.
    IntPred(IntPred),
    /// Which comparison, for `fcmp`.
    FloatPred(FloatPred),
}

/// What an instruction computes and where its answer is, or nothing if it is not a candidate.
///
/// `same` is what the block has decided so far, and every operand goes through it, so an operand
/// naming an instruction this pass is removing is compared as the instruction it is being removed
/// in favour of. One lookup is the whole resolution rather than the first step of one: a value is
/// either a key of `same`, meaning it is on its way out, or a value in `seen`, meaning it is
/// staying, and it cannot be both, because a value only enters `same` on a hit and a hit never
/// touches what the table already holds.
fn key(func: &Func, same: &HashMap<Value, Value>, inst: Inst) -> Option<(Key, Value)> {
    let data = &func[inst];
    if data.opcode.has_effects() || data.opcode == Opcode::MemEntry {
        return None;
    }
    let mut results = data.results();
    let (Some(result), None) = (results.next(), results.next()) else { return None };
    let tag = match data.extra {
        Extra::None => Tag::None,
        Extra::Imm(at) => Tag::Bits(func[at].bits()),
        Extra::Symbol(name) => Tag::Symbol(name),
        Extra::IntPred(pred) => Tag::IntPred(pred),
        Extra::FloatPred(pred) => Tag::FloatPred(pred),
        _ => return None,
    };
    let operands = &func[data.args];
    if operands.len() > OPERANDS {
        return None;
    }
    let mut args = [None; OPERANDS];
    for (slot, &arg) in args.iter_mut().zip(operands) {
        *slot = Some(same.get(&arg).copied().unwrap_or(arg));
    }
    // Two operands the opcode reads in either order are put in one order, so that `a + b` written
    // once and `b + a` written once are one add. Sorting is by the position of the value in the
    // function, which is an order that exists for no other reason and is fine because the only
    // thing asked of it is that the two sides agree on it.
    if data.opcode.is_commutative() && operands.len() == 2 {
        args[..2].sort_unstable();
    }
    Some((Key { opcode: data.opcode, flags: data.flags, ty: func[result].ty, tag, args }, result))
}

#[cfg(test)]
mod tests {
    use rucc_ir::{
        Block, Builder, Def, Extra, InstData, MemInfo, MemOrder, Restrict, Signature, Type,
    };

    use super::*;
    use crate::stats::Kind;

    /// An empty function with one block, which is where every test below builds.
    fn blank() -> (Func, Block) {
        let mut names = rucc_base::Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(64)]));
        let block = func.create_block();
        (func, block)
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

    /// An `alloca` of thirty-two bytes, which is an address nothing outside the function knows.
    fn local(build: &mut Builder<'_>) -> Value {
        let mem = build.func().add_mem(MemInfo { size: 32, ..plain(8) });
        build.value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// Runs the pass over the function with as much fuel as it wants.
    fn run(func: &mut Func) -> Stats {
        Number.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// How many instructions of that opcode are left in the function.
    fn count(func: &Func, opcode: Opcode) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
            .filter(|&inst| func[inst].opcode == opcode)
            .count()
    }

    /// What the return statement hands back, after the pass has pointed it somewhere.
    fn returned(func: &Func) -> Vec<Value> {
        let block = func.blocks().last().expect("the function has a block");
        let inst = func.terminator(block).expect("the block has a terminator");
        func[func[inst].args].to_vec()
    }

    /// The operands of whatever instruction produced that value.
    fn operands(func: &Func, value: Value) -> Vec<Value> {
        let Def::Result { inst, .. } = func[value].def else { panic!("not an instruction result") };
        func[func[inst].args].to_vec()
    }

    #[test]
    fn the_same_arithmetic_on_the_same_operands_twice_is_one_instruction() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let left = build.iconst(Type::int(64), 3);
        let right = build.iconst(Type::int(64), 5);
        let first = build.binary(Opcode::Add, left, right, Flags::NONE);
        let second = build.binary(Opcode::Add, left, right, Flags::NONE);
        build.ret(&[first, second]);

        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Optimized, MERGED), 1);
        assert_eq!(count(&func, Opcode::Add), 1);
        assert_eq!(returned(&func), vec![first, first]);
    }

    #[test]
    fn a_commutative_pair_matches_with_its_operands_the_other_way_round() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let left = build.iconst(Type::int(64), 3);
        let right = build.iconst(Type::int(64), 5);
        let first = build.binary(Opcode::Add, left, right, Flags::NONE);
        let second = build.binary(Opcode::Add, right, left, Flags::NONE);
        build.ret(&[first, second]);

        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Optimized, MERGED), 1);
        assert_eq!(returned(&func), vec![first, first]);
    }

    #[test]
    fn a_subtraction_the_other_way_round_is_a_different_answer() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let left = build.iconst(Type::int(64), 3);
        let right = build.iconst(Type::int(64), 5);
        let first = build.binary(Opcode::Sub, left, right, Flags::NONE);
        let second = build.binary(Opcode::Sub, right, left, Flags::NONE);
        build.ret(&[first, second]);

        let stats = run(&mut func);
        assert!(!stats.changed(), "three minus five is not five minus three");
        assert_eq!(count(&func, Opcode::Sub), 2);
    }

    #[test]
    fn two_adds_that_promise_different_things_stay_two_adds() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let left = build.iconst(Type::int(64), 3);
        let right = build.iconst(Type::int(64), 5);
        let first = build.binary(Opcode::Add, left, right, Flags::NSW);
        let second = build.binary(Opcode::Add, left, right, Flags::NONE);
        build.ret(&[first, second]);

        // Merging them onto the first hands the second a promise nobody made about it, and
        // merging them onto the second throws away a promise somebody did make.
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(count(&func, Opcode::Add), 2);
    }

    #[test]
    fn a_chain_collapses_all_the_way_up_and_not_just_at_the_bottom() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let index = build.iconst(Type::int(64), 2);
        let scale = build.iconst(Type::int(64), 8);
        let first = build.binary(Opcode::Mul, index, scale, Flags::NONE);
        let second = build.binary(Opcode::Mul, index, scale, Flags::NONE);
        let up = build.binary(Opcode::Add, first, scale, Flags::NONE);
        let down = build.binary(Opcode::Add, second, scale, Flags::NONE);
        build.ret(&[up, down]);

        // The second add's operand is a value on its way out, so it has to be compared as the
        // value it is on its way out in favour of. Without that the pass takes the multiply and
        // stops, which on a subscript is the bottom instruction of three or four.
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Optimized, MERGED), 2);
        assert_eq!(count(&func, Opcode::Mul), 1);
        assert_eq!(count(&func, Opcode::Add), 1);
        assert_eq!(returned(&func), vec![up, up]);
    }

    #[test]
    fn the_same_constant_written_twice_is_one_constant() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let first = build.iconst(Type::int(64), 7);
        let second = build.iconst(Type::int(64), 7);
        let narrow = build.iconst(Type::int(32), 7);
        build.ret(&[first, second, narrow]);

        // The immediate table is not interned, so the two sevens are two entries in it and only
        // reading the bits back out finds that they are the same seven. The third is the same bits
        // at another width, which is another value.
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Optimized, MERGED), 1);
        assert_eq!(count(&func, Opcode::IConst), 2);
        assert_eq!(returned(&func), vec![first, first, narrow]);
    }

    #[test]
    fn two_allocas_are_two_addresses_however_alike_they_look() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let one = local(&mut build);
        let two = local(&mut build);
        build.ret(&[one, two]);

        // An `alloca` has effects for exactly this reason. Two of them are two objects and the
        // program can tell, by comparing their addresses if by nothing else.
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(count(&func, Opcode::Alloca), 2);
    }

    #[test]
    fn two_loads_of_one_address_are_left_to_the_pass_that_knows_about_memory() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build);
        let first = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        let second = build.load(Type::int(64), slot, plain(8), Flags::NONE);
        build.ret(&[first, second]);

        // A load is not pure, because what it answers depends on what has been written since. It
        // is `crate::load` that knows whether anything has been, and this pass never touches one.
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(count(&func, Opcode::Load), 2);
    }

    #[test]
    fn what_one_block_computes_does_not_reach_the_next_one() {
        let (mut func, entry) = blank();
        let next = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let left = build.iconst(Type::int(64), 3);
        let right = build.iconst(Type::int(64), 5);
        let first = build.binary(Opcode::Add, left, right, Flags::NONE);
        build.jump(next, &[]);
        let mut build = Builder::new(&mut func, next);
        let second = build.binary(Opcode::Add, left, right, Flags::NONE);
        build.ret(&[first, second]);

        // The first add dominates the second and the version over the dominator tree takes it.
        // Section 16.1 is where the argument for this being enough for now lives.
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(count(&func, Opcode::Add), 2);
    }

    #[test]
    fn one_name_for_the_address_is_what_lets_the_load_be_forwarded() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let base = local(&mut build);
        let index = build.iconst(Type::int(64), 2);
        let scale = build.iconst(Type::int(64), 8);
        let wrote = build.iconst(Type::int(64), 7);
        let to = build.binary(Opcode::Mul, index, scale, Flags::NONE);
        let to = build.binary(Opcode::PtrAdd, base, to, Flags::NONE);
        build.store(wrote, to, plain(8), Flags::NONE);
        let from = build.binary(Opcode::Mul, index, scale, Flags::NONE);
        let from = build.binary(Opcode::PtrAdd, base, from, Flags::NONE);
        let read = build.load(Type::int(64), from, plain(8), Flags::NONE);
        build.ret(&[read]);

        // This is `a[2] = 7; total += a[2];` as the front end emits it, with the subscript lowered
        // twice because lowering it does not know it has been lowered already. Before this pass
        // the store's address and the load's address are two values and `crate::load` compares
        // addresses by identity, so it refuses. Afterwards they are one value and it forwards.
        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Optimized, ADDRESS), 1);
        assert_eq!(stats.count(Kind::Optimized, MERGED), 1);
        assert_eq!(count(&func, Opcode::PtrAdd), 1);

        let mut analyses = crate::machine::fixtures::analyses();
        let stats = crate::load::LoadForward.run(&mut func, &mut analyses, &mut Fuel::unlimited());
        assert!(stats.changed(), "the two addresses are one value now");
        assert_eq!(count(&func, Opcode::Load), 0);
        assert_eq!(returned(&func), vec![wrote]);
    }

    #[test]
    fn an_instruction_with_three_operands_is_matched_on_all_three() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let left = build.iconst(Type::int(64), 3);
        let right = build.iconst(Type::int(64), 5);
        let which = build.icmp(IntPred::Slt, left, right);
        let args = build.func().push_values(&[which, left, right]);
        let pick = InstData { args, ..InstData::new(Opcode::Select) };
        let first = build.value(pick, Type::int(64));
        let second = build.value(pick, Type::int(64));
        let args = build.func().push_values(&[which, right, left]);
        let other = InstData { args, ..InstData::new(Opcode::Select) };
        let other = build.value(other, Type::int(64));
        build.ret(&[first, second, other]);

        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Optimized, MERGED), 1, "the arms the other way round differ");
        assert_eq!(count(&func, Opcode::Select), 2);
        assert_eq!(returned(&func), vec![first, first, other]);
    }

    #[test]
    fn a_repeated_global_address_is_counted_as_an_address() {
        let (mut func, block) = blank();
        let mut names = rucc_base::Interner::new();
        let global = names.intern("g");
        let mut build = Builder::new(&mut func, block);
        let named = InstData { extra: Extra::Symbol(global), ..InstData::new(Opcode::GlobalAddr) };
        let first = build.value(named, Type::PTR);
        let second = build.value(named, Type::PTR);
        let offset = build.iconst(Type::int(64), 8);
        let one = build.binary(Opcode::PtrAdd, first, offset, Flags::NONE);
        let two = build.binary(Opcode::PtrAdd, second, offset, Flags::NONE);
        build.ret(&[one, two]);

        let stats = run(&mut func);
        assert_eq!(stats.count(Kind::Optimized, ADDRESS), 2);
        assert_eq!(count(&func, Opcode::GlobalAddr), 1);
        assert_eq!(operands(&func, one), vec![first, offset]);
    }

    #[test]
    fn without_fuel_the_duplicate_stays_and_the_chance_is_still_counted() {
        let (mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let left = build.iconst(Type::int(64), 3);
        let right = build.iconst(Type::int(64), 5);
        let first = build.binary(Opcode::Add, left, right, Flags::NONE);
        let second = build.binary(Opcode::Add, left, right, Flags::NONE);
        build.ret(&[first, second]);

        let stats =
            Number.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, NO_FUEL), 1);
        assert_eq!(count(&func, Opcode::Add), 2);
    }
}
