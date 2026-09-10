//! Folding an address computation into the memory operand of whatever reads it.
//!
//! Design: `spec/10-backend.md` section 10.9, and `spec/optimizer/37-machine-level-optimization.md`
//! section 37.4.
//!
//! The selector matches one instruction at a time and offers it its operands' operands, which is
//! two levels of term and is exactly what an address needs to become a `lea`: `a + i * 4` is an
//! add at the root with a multiply under it. Put that same address under a load and everything
//! moves down a level, the multiply is at level two, and no plan the selector has reaches it. So
//! an array read comes out of selection as two instructions, the `lea` that works the address out
//! and the `mov` that reads through it, and the second one's addressing mode holds nothing but a
//! base.
//!
//! Which is a pair a peephole can see. When the register a `lea` writes is read by exactly one
//! instruction, and that instruction reads it as the base of its memory operand, the two addresses
//! compose: the reader's displacement is a constant added to an address the `lea` already worked
//! out, so adding the two displacements together gives the address the reader wanted in the mode
//! the `lea` was using. The `lea` then has no reader at all and goes.
//!
//! # What it will not do
//!
//! Two indexes. The reader having an index of its own means the composed address wants two scaled
//! registers and this machine, like every machine, has one. Nothing looks for a way to put them
//! together because there is not one.
//!
//! A displacement that does not fit. The two are added as `i64` and the answer has to be an `i32`,
//! which is what the field holds. It is not a case that comes up in a program anybody wrote, and
//! the check is there because the alternative to checking is wrapping.
//!
//! A reader in another block. Folding moves the work from where the `lea` is to where the reader
//! is, and across a block boundary that can mean moving it into a loop. The same rule and the same
//! reason as `crate::lower::Lowering::foldable`, which is the selector's version of this question.
//!
//! A register that something writes in between. Machine IR is in SSA form until the allocator has
//! run, so a virtual register cannot be, but a physical one can: the frame pointer and the stack
//! pointer are already physical here, and a call in between writes every register it is allowed
//! to. Rather than ask which registers are the exceptions, the walk below drops a candidate the
//! moment anything writes a register its address reads.
//!
//! An address whose displacement is not settled. A local's place in the frame and an argument's
//! place in the caller's is a distance from the stack pointer, and there is no frame until the
//! allocator has finished, so [`crate::lower`] leaves those instructions with a zero in the
//! displacement and [`crate::finish`] writes the number in later against a list of which
//! instruction is which. Folding one of them away would leave that number being written into an
//! instruction nothing runs, and the fold itself would have composed a displacement that was not
//! there yet. So the caller says which instructions those are and this leaves them alone. What it
//! costs is the fold on a local whose address is taken, which is worth having and is not worth
//! having at the price of `finish` and this pass sharing a secret.
//!
//! # Where it runs
//!
//! After selection and before the allocator, which is the one window where both instructions
//! exist and the registers are still virtual. Running it after allocation would work on the
//! arithmetic and would be reading a register file where the reader's base may have been reused
//! for something else in between.

use std::collections::{HashMap, HashSet};

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{FrameInsts, Role};

/// Folds every address computation that one memory operand reads, and gives back how many.
///
/// `waiting` is the instructions whose displacement [`crate::finish`] has still to write, which
/// are the ones this must not touch.
///
/// Run after lowering and before allocation. Running it twice can find more than running it once,
/// because folding a `lea` into a second `lea` leaves that second one foldable in turn, and the
/// walk below takes those in the one pass since it goes forwards.
pub fn addresses(
    func: &mut mir::Func,
    insts: &FrameInsts,
    names: &mut Interner,
    waiting: &HashSet<mir::Inst>,
) -> usize {
    let lea = mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, insts.lea)));
    let reads = reads(func);
    let mut folded = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        // One `lea` per register it wrote, dropped again as soon as anything the address reads is
        // written or the register is read by somebody who is not folding it.
        let mut open: HashMap<mir::Reg, mir::Inst> = HashMap::new();
        for inst in func.insts(block).collect::<Vec<_>>() {
            if let Some(folding) = candidate(func, &open, inst) {
                let operands = func.push_operands(&folding.operands);
                let mem = func.add_amode(folding.amode);
                func[inst].operands = operands;
                func[inst].mem = Some(mem);
                open.remove(&folding.base);
                func.remove_inst(folding.from);
                folded += 1;
            }
            for written in written(func, inst) {
                open.retain(|reg, &mut held| *reg != written && !touches(func, held, written));
            }
            if func[inst].opcode == lea && !waiting.contains(&inst) {
                if let Some(reg) = written_once(func, &reads, inst) {
                    open.insert(reg, inst);
                }
            }
        }
    }
    folded
}

/// How many times each virtual register is read, counting the arguments an edge carries.
///
/// A `lea` is only worth folding when the instruction folding it is the whole of what reads the
/// register, since folding does not delete the `lea` for anybody else and doing the address twice
/// is not a saving. An argument on an edge is a read like any other and is not in any operand
/// vector, which is the one place this is easy to get wrong.
fn reads(func: &mir::Func) -> HashMap<mir::Reg, usize> {
    let mut counts = HashMap::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                if operand.role == Role::Use {
                    *counts.entry(operand.reg).or_insert(0) += 1;
                }
            }
        }
        for call in &func[block].succs {
            for &arg in &call.args {
                *counts.entry(arg).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// The one virtual register an instruction writes, when it writes exactly one and exactly one
/// thing reads it.
fn written_once(
    func: &mir::Func,
    reads: &HashMap<mir::Reg, usize>,
    inst: mir::Inst,
) -> Option<mir::Reg> {
    let operands = &func[func[inst].operands];
    let mut defs = operands.iter().filter(|operand| operand.role != Role::Use);
    let def = defs.next()?;
    if defs.next().is_some() || !def.reg.is_virtual() || reads.get(&def.reg) != Some(&1) {
        return None;
    }
    Some(def.reg)
}

/// The registers an instruction writes.
fn written(func: &mir::Func, inst: mir::Inst) -> Vec<mir::Reg> {
    func[func[inst].operands]
        .iter()
        .filter(|operand| operand.role != Role::Use)
        .map(|operand| operand.reg)
        .collect()
}

/// Whether an address computation reads that register, which is what makes writing it the end of
/// the chance to fold it.
fn touches(func: &mir::Func, inst: mir::Inst, reg: mir::Reg) -> bool {
    let Some(mem) = func[inst].mem else { return false };
    let amode = func[mem];
    let operands = &func[func[inst].operands];
    [amode.base, amode.index]
        .into_iter()
        .flatten()
        .filter_map(|at| operands.get(usize::from(at)))
        .any(|operand| operand.reg == reg)
}

/// The register an instruction's memory operand reads as its base, when that is the whole of what
/// its memory operand is.
///
/// A symbol or an index means the two addresses do not compose, and this is where both are turned
/// down, because the reader is the half of the pair with no room left in it.
fn base_reg(func: &mir::Func, inst: mir::Inst) -> Option<mir::Reg> {
    let amode = func[func[inst].mem?];
    if amode.index.is_some() || amode.symbol.is_some() || amode.got {
        return None;
    }
    Some(func[func[inst].operands].get(usize::from(amode.base?))?.reg)
}

/// A fold that has been checked and not yet done.
///
/// Everything the rewrite needs is worked out here rather than after the decision, so that the
/// decision is the last thing that can go either way and the rewrite itself is three assignments
/// that cannot fail.
struct Folding {
    /// The address instruction that goes, because nothing reads what it wrote any more.
    from: mir::Inst,
    /// The register it wrote, which stops being open the moment this is done.
    base: mir::Reg,
    /// What the reader's operands become.
    operands: Vec<mir::Operand>,
    /// What the reader's addressing mode becomes.
    amode: mir::Amode,
}

/// The `lea` whose address this instruction should read directly, and what reading it directly
/// makes of the instruction.
///
/// The operand vector is rebuilt rather than edited because the registers a memory operand names
/// come last in it, which is the invariant [`mir::InstBuilder::mem`] keeps and the printer and the
/// allocator both read. Dropping the base the reader had and putting the `lea`'s base and index on
/// the end keeps it, and the indices in the new addressing mode are worked out from the length
/// rather than carried over.
fn candidate(
    func: &mir::Func,
    open: &HashMap<mir::Reg, mir::Inst>,
    inst: mir::Inst,
) -> Option<Folding> {
    let base = base_reg(func, inst)?;
    let from = *open.get(&base)?;
    let address = func[func[from].mem?];
    // The reader holds the base in its last operand and nothing else names it, since the register
    // has one read in the whole function and this is it. So the composed address is the `lea`'s
    // with the reader's displacement added, and the only thing that can go wrong is the width of
    // the field it goes in.
    let disp = i64::from(address.disp) + i64::from(func[func[inst].mem?].disp);
    let mut amode = mir::Amode { disp: i32::try_from(disp).ok()?, ..address };

    let taken = &func[func[from].operands];
    let reader = &func[func[inst].operands];
    let mut operands = reader.get(..reader.len().checked_sub(1)?)?.to_vec();
    for (at, into) in [(address.base, &mut amode.base), (address.index, &mut amode.index)] {
        let Some(at) = at else { continue };
        operands.push(*taken.get(usize::from(at))?);
        *into = Some(u8::try_from(operands.len() - 1).ok()?);
    }
    Some(Folding { from, base, operands, amode })
}

#[cfg(test)]
mod tests {
    use rucc_target::x86_64::{FRAME, GPR, RDI};

    use super::*;

    /// A function with one block, and the names it was built with.
    fn empty() -> (Interner, mir::Func, mir::Block) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> mir::Opcode {
        mir::Opcode::new(names.intern(&format!("{}{name}", FRAME.prefix)))
    }

    /// What every instruction in a block came to, as opcodes and addressing modes.
    fn shape(func: &mir::Func, names: &Interner, block: mir::Block) -> Vec<(String, mir::Amode)> {
        func.insts(block)
            .map(|inst| {
                let amode = func[inst].mem.map_or(mir::Amode::NOTHING, |mem| func[mem]);
                (names.resolve(func[inst].opcode.name()).to_owned(), amode)
            })
            .collect()
    }

    /// The registers a memory operand names, in the order the addressing mode names them.
    fn address_regs(func: &mir::Func, inst: mir::Inst) -> Vec<mir::Reg> {
        let amode = func[func[inst].mem.expect("a memory operand")];
        let operands = &func[func[inst].operands];
        [amode.base, amode.index]
            .into_iter()
            .flatten()
            .map(|at| operands[usize::from(at)].reg)
            .collect()
    }

    /// An array read as selection leaves it: a `lea` that scales the index and adds the base, and
    /// a `mov` that reads through the register it wrote.
    #[test]
    fn an_address_a_load_reads_once_becomes_the_load_s_own_addressing_mode() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(
                mir::Mem::at(mir::Operand::read(array, GPR))
                    .indexed(mir::Operand::read(index, GPR), 4),
            )
            .finish();
        func.build(block, load)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 1);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 1, "the address is worked out twice: {left:?}");
        assert_eq!(left[0].0, format!("{}mov_rm_32", FRAME.prefix));
        assert_eq!(left[0].1.scale, 4);
        assert_eq!(left[0].1.disp, 0);
        let inst = func.insts(block).next().expect("the load is still there");
        assert_eq!(address_regs(&func, inst), vec![array, index], "the load reads the wrong pair");
    }

    /// The two displacements are added, which is the whole of what composing them takes when one
    /// of the two addresses has room for an index and the other has none.
    #[test]
    fn the_displacements_of_the_two_addresses_are_added() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        func.build(block, load)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)).plus(8))
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 1);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].1.disp, 24, "the field is at the sum of the two offsets or nowhere");
    }

    /// A store keeps the value it writes, which is the operand the address does not name, and the
    /// rebuilt operand vector has to hold on to it.
    #[test]
    fn a_store_keeps_the_value_it_is_storing() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let store = op(&mut names, "mov_mr_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(
                mir::Mem::at(mir::Operand::read(array, GPR))
                    .indexed(mir::Operand::read(index, GPR), 8),
            )
            .finish();
        func.build(block, store)
            .uses(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 1);

        let inst = func.insts(block).next().expect("the store is still there");
        let regs: Vec<mir::Reg> = func[func[inst].operands].iter().map(|op| op.reg).collect();
        assert_eq!(regs, vec![value, array, index], "the value the store writes went missing");
        assert_eq!(func[func[inst].mem.expect("a memory operand")].scale, 8);
    }

    /// Two readers is not a saving. Folding into either of them leaves the `lea` where it is for
    /// the other, and the address is then worked out twice rather than once.
    #[test]
    fn an_address_two_instructions_read_is_left_where_it_is() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        for _ in 0..2 {
            let value = func.new_vreg(GPR);
            func.build(block, load)
                .def(value, GPR)
                .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
                .finish();
        }

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 0);
        assert_eq!(shape(&func, &names, block).len(), 3);
    }

    /// The reader having an index of its own is the one shape that does not compose, since the
    /// answer would want two scaled registers.
    #[test]
    fn a_reader_that_already_has_an_index_is_left_alone() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        func.build(block, load)
            .def(value, GPR)
            .mem(
                mir::Mem::at(mir::Operand::read(address, GPR))
                    .indexed(mir::Operand::read(index, GPR), 4),
            )
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// The two displacements add up to more than the field holds, so the pair stays a pair. The
    /// program that does this is one nobody wrote, and the point of the test is that the answer is
    /// a refusal rather than a wrap.
    #[test]
    fn two_displacements_that_do_not_fit_together_are_not_put_together() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(i32::MAX))
            .finish();
        func.build(block, load)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)).plus(1))
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// A physical register the address reads, written between the two. Machine IR is in SSA form
    /// here so a virtual register cannot be, and this is why the walk asks anyway.
    #[test]
    fn a_register_the_address_reads_being_written_in_between_ends_the_chance() {
        let (mut names, mut func, block) = empty();
        let array = mir::Reg::physical(RDI);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        let put = op(&mut names, "mov_ri_64");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        func.build(block, put).def(array, GPR).imm(7).finish();
        func.build(block, load)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 0);
        assert_eq!(shape(&func, &names, block).len(), 3);
    }

    /// A reader in another block. Folding would move the address to wherever that block is, and
    /// this pass has no way to know whether that is somewhere it runs more often.
    #[test]
    fn a_reader_in_another_block_is_not_one_this_folds_into() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let array = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        *func.succs_mut(block) = vec![mir::BlockCall::to(next)];
        func.build(next, load)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 0);
    }

    /// A chain of two, which is what an address of a field of an element of an array comes out as.
    /// The walk goes forwards, so the second `lea` is folded into the load and then the first is
    /// folded into what is left of the second, both in the one pass.
    #[test]
    fn a_chain_of_two_addresses_is_folded_the_whole_way_in_one_pass() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let element = func.new_vreg(GPR);
        let field = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(element, GPR)
            .mem(
                mir::Mem::at(mir::Operand::read(array, GPR))
                    .indexed(mir::Operand::read(index, GPR), 8),
            )
            .finish();
        func.build(block, lea)
            .def(field, GPR)
            .mem(mir::Mem::at(mir::Operand::read(element, GPR)).plus(4))
            .finish();
        func.build(block, load)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(field, GPR)))
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 2);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 1, "one of the two addresses is still its own instruction");
        assert_eq!(left[0].1.scale, 8);
        assert_eq!(left[0].1.disp, 4);
        let inst = func.insts(block).next().expect("the load is still there");
        assert_eq!(address_regs(&func, inst), vec![array, index]);
    }

    /// An address of a global, which the `lea` holds as a symbol rather than as a register. It
    /// composes the same way and the reader ends up naming the symbol itself, which is one
    /// instruction rather than two for every read of a global with a constant subscript.
    #[test]
    fn an_address_of_a_global_folds_into_the_reader_symbol_and_all() {
        let (mut names, mut func, block) = empty();
        let global = names.intern("counters");
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea).def(address, GPR).mem(mir::Mem::of(global)).finish();
        func.build(block, load)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)).plus(12))
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 1);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].1.symbol, Some(global));
        assert_eq!(left[0].1.disp, 12);
    }

    /// An address into the frame, which reads as an address of nothing until `finish` writes the
    /// distance in. Folding it would compose a displacement that is not there yet and would leave
    /// `finish` writing the real one into an instruction nothing runs.
    #[test]
    fn an_address_whose_displacement_is_still_to_be_written_is_left_where_it_is() {
        let (mut names, mut func, block) = empty();
        let sp = mir::Reg::physical(RDI);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        let local = func
            .build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(sp, GPR)))
            .finish();
        func.build(block, load)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        let waiting = HashSet::from([local]);
        assert_eq!(addresses(&mut func, &FRAME, &mut names, &waiting), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);

        // And the same function with nothing waiting, so that what the test pins is the list and
        // not some other thing about the pair.
        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 1);
    }

    /// An instruction that is not the target's address instruction, writing a register a load
    /// reads. A load through the result of a load is two loads and folding one into the other
    /// would read the wrong memory, so the opcode is checked rather than the shape.
    #[test]
    fn only_the_target_s_address_instruction_is_one_this_folds() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let load = op(&mut names, "mov_rm_64");
        let read = op(&mut names, "mov_rm_32");
        func.build(block, load)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        func.build(block, read)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(addresses(&mut func, &FRAME, &mut names, &HashSet::new()), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }
}
