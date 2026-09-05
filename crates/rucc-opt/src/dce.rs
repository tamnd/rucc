//! Dead code elimination: an instruction nothing uses and nothing depends on goes away.
//!
//! The other half of [`crate::fold`]. Folding rewrites an instruction in place and leaves its
//! operands behind, used by nothing, so a function that folds well is a function whose printed IR
//! grows a tail of arithmetic that computes numbers nobody reads. Every later pass will do the
//! same thing, because a rewrite that has to clean up after itself is a rewrite that has to know
//! what else was using what it replaced, and that is the knowledge this pass exists to hold in one
//! place.
//!
//! It is not primarily an optimization. The backend materializes a constant where it is wanted
//! rather than where the IR wrote it, so most of what this removes was already costing nothing in
//! the output. What it buys is that a dump reads like the program, that the passes after it see a
//! function whose size is the size of the work in it, and that a rule which fires on a dead
//! instruction is a rule that fired on nothing rather than a rule that fired.
//!
//! # How it decides
//!
//! An instruction goes when it is not a terminator, when [`Opcode::has_effects`] says no, and when
//! every value it produces is used by nothing. All three are needed and the second is where the
//! argument lives: `has_effects` is the conservative predicate, so a load, an allocation, a call
//! and a `va_arg` all stay whatever their results do. That is stricter than it has to be, since a
//! non-volatile load of a dead value is safe to remove and so is an allocation nothing addresses,
//! but both of those want memory analysis to say so honestly and this pass predates it.
//!
//! # Why it is a worklist
//!
//! Removing an instruction can kill the one that fed it, and that one can kill its own operand, so
//! a single walk in any order finds a fraction of what is there. The counts are built once, and
//! removing an instruction decrements what its operands were used for, and an operand that reaches
//! zero puts its own definition back on the list. That reaches the same fixpoint a repeated walk
//! would and touches each instruction about once.
//!
//! Uses are counted per occurrence rather than per instruction, because `x + x` uses `x` twice and
//! removing one adder should not make `x` look dead.
//!
//! # What it does not remove
//!
//! Not a block parameter. A parameter nothing reads is dead in exactly the same sense, and taking
//! it out means rewriting the argument list of every branch that arrives at the block, which is
//! worth doing and is a different transformation from this one. The loop carried case is the
//! interesting one there and it is the reason to do it separately: a parameter whose only use is
//! the argument it passes to itself is dead, and seeing that needs the cycle broken rather than a
//! count driven to zero.
//!
//! Not an unreachable block. A block no branch names is dead code by any definition, and removing
//! it is control flow work rather than value work. It belongs with the branch folding that creates
//! most of it.

use rucc_ir::{Block, Def, Func, Inst, Opcode, Value};

use crate::{Fuel, Pass};

/// The pass. It holds nothing, because the counts are per function and live in [`Pass::run`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dce;

impl Pass for Dce {
    fn name(&self) -> &'static str {
        "dce"
    }

    fn describe(&self) -> &'static str {
        "an instruction with no effects whose results nothing uses is removed"
    }

    fn run(&self, func: &mut Func, fuel: &mut Fuel) -> bool {
        let mut uses = count(func);
        let mut work: Vec<Inst> = Vec::new();
        for block in func.blocks().collect::<Vec<Block>>() {
            for inst in func.insts(block) {
                if dead(func, inst, &uses) {
                    work.push(inst);
                }
            }
        }
        let mut changed = false;
        while let Some(inst) = work.pop() {
            // A worklist can name the same instruction twice, once from the first walk and once
            // from an operand reaching zero, and the second visit finds it already gone.
            if func.block_of(inst).is_none() {
                continue;
            }
            if !dead(func, inst, &uses) {
                continue;
            }
            if !fuel.take() {
                // Out of fuel, which stops the transforming and not the looking, the same way
                // folding treats it. Draining the rest of the list without removing anything
                // costs one pass over what is left and keeps the walk's shape independent of
                // where the fuel ran out.
                continue;
            }
            operands(func, inst, |value| {
                let count = &mut uses[value.index()];
                *count -= 1;
                if *count == 0 {
                    if let Def::Result { inst: def, .. } = func[value].def {
                        work.push(def);
                    }
                }
            });
            func.remove_inst(inst);
            changed = true;
        }
        changed
    }
}

/// How many times each value is used, by position rather than by instruction.
fn count(func: &Func) -> Vec<u32> {
    let mut uses = vec![0u32; func.counts().values];
    for block in func.blocks().collect::<Vec<Block>>() {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            operands(func, inst, |value| uses[value.index()] += 1);
        }
    }
    uses
}

/// Every value this instruction reads, with a repeat for each time it reads it.
///
/// The arguments and the arguments of the blocks it branches to. That is the whole of what an
/// instruction can use, and it is the same pair the verifier walks, so a use this misses is a use
/// the verifier would already be looking at from the other side.
fn operands(func: &Func, inst: Inst, mut each: impl FnMut(Value)) {
    for &value in &func[func[inst].args] {
        each(value);
    }
    for call in func.successors(inst) {
        for &value in &func[call.args] {
            each(value);
        }
    }
}

/// Whether this instruction can go.
fn dead(func: &Func, inst: Inst, uses: &[u32]) -> bool {
    let data = &func[inst];
    // `is_terminator` on the function rather than on the opcode, because `asm goto` branches
    // and its opcode does not say so. Inline assembly has effects either way, so this is belt
    // and braces, and it is the cheaper of the two mistakes to make.
    if func.is_terminator(inst) || data.opcode.has_effects() {
        return false;
    }
    debug_assert!(
        data.opcode != Opcode::InlineAsm,
        "inline assembly has effects and cannot reach here"
    );
    data.results().all(|value| uses[value.index()] == 0)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, Func, MemInfo, MemOrder, Opcode, Signature, Type};

    use crate::{Fuel, Pass, dce::Dce};

    /// A function with one block, ready to have instructions appended to it.
    fn blank() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(32)]));
        let block = func.create_block();
        (names, func, block)
    }

    /// How many instructions are left in a block.
    fn left(func: &Func, block: Block) -> usize {
        func.insts(block).count()
    }

    #[test]
    fn arithmetic_nothing_reads_goes_away() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let a = build.iconst(Type::int(32), 2);
        let b = build.iconst(Type::int(32), 3);
        build.binary(Opcode::Add, a, b, Flags::NONE);
        build.ret(&[a]);
        assert!(Dce.run(&mut func, &mut Fuel::unlimited()));
        // The add, and then the constant that only it read. A single walk in this order would
        // have removed the add and left the three behind, which is what the worklist is for.
        assert_eq!(left(&func, block), 2);
    }

    #[test]
    fn arithmetic_something_reads_stays() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let a = build.iconst(Type::int(32), 2);
        let b = build.iconst(Type::int(32), 3);
        let sum = build.binary(Opcode::Add, a, b, Flags::NONE);
        build.ret(&[sum]);
        assert!(!Dce.run(&mut func, &mut Fuel::unlimited()));
        assert_eq!(left(&func, block), 4);
    }

    #[test]
    fn a_value_used_twice_is_not_dead_when_one_use_goes() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 7);
        let kept = build.binary(Opcode::Add, x, x, Flags::NONE);
        build.binary(Opcode::Add, x, x, Flags::NONE);
        build.ret(&[kept]);
        assert!(Dce.run(&mut func, &mut Fuel::unlimited()));
        // Only the second add. Counting a use per instruction rather than per position would
        // have driven the constant to zero and taken it out from under the first one.
        assert_eq!(left(&func, block), 3);
    }

    #[test]
    fn a_store_stays_however_dead_it_looks() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let value = build.iconst(Type::int(32), 1);
        let address = build.iconst(Type::int(64), 0);
        let address = build.unary(Opcode::IntToPtr, address, Type::PTR);
        let info = MemInfo { size: 4, align: 4, order: MemOrder::NotAtomic, tbaa: None };
        build.store(value, address, info, Flags::NONE);
        build.ret(&[value]);
        assert!(!Dce.run(&mut func, &mut Fuel::unlimited()));
        assert_eq!(left(&func, block), 5);
    }

    #[test]
    fn a_value_a_branch_passes_on_is_used_by_the_branch() {
        let (_, mut func, block) = blank();
        let target = func.create_block();
        let param = func.append_param(target, Type::int(32));
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 9);
        build.jump(target, &[x]);
        let mut build = Builder::new(&mut func, target);
        build.ret(&[param]);
        assert!(!Dce.run(&mut func, &mut Fuel::unlimited()));
        // The constant is read by nothing in its own block and is not dead, because the only
        // use an instruction can have that its argument list does not hold is this one.
        assert_eq!(left(&func, block), 2);
    }

    #[test]
    fn a_result_a_removed_instruction_read_is_looked_at_again() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let a = build.iconst(Type::int(32), 2);
        let b = build.iconst(Type::int(32), 3);
        let sum = build.binary(Opcode::Add, a, b, Flags::NONE);
        let doubled = build.binary(Opcode::Add, sum, sum, Flags::NONE);
        build.unary(Opcode::SExt, doubled, Type::int(64));
        let kept = build.iconst(Type::int(32), 1);
        build.ret(&[kept]);
        assert!(Dce.run(&mut func, &mut Fuel::unlimited()));
        // A chain five long, dead from the far end, and all of it goes in one run. This is the
        // case a walk in program order finds one instruction of per run.
        assert_eq!(left(&func, block), 2);
    }

    #[test]
    fn fuel_stops_the_removing_and_not_the_looking() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let a = build.iconst(Type::int(32), 2);
        let b = build.iconst(Type::int(32), 3);
        build.binary(Opcode::Add, a, b, Flags::NONE);
        build.ret(&[a]);
        let mut fuel = Fuel::of(1);
        assert!(Dce.run(&mut func, &mut fuel));
        // The add and nothing after it, so the constant the add was keeping alive stays. One
        // unit of fuel is one transformation, which is what makes a bisection over it land on
        // a single site.
        assert_eq!(left(&func, block), 3);
    }
}
