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
//! Which is a pair a peephole can see. When an instruction reads the register a `lea` wrote as the
//! base of its memory operand, the two addresses compose: the reader's displacement is a constant
//! added to an address the `lea` already worked out, so adding the two displacements together gives
//! the address the reader wanted in the mode the `lea` was using.
//!
//! The question is asked of the readers together rather than one at a time, which is what section
//! 37.4 says the pass is really for. One address read at several offsets is what a structure
//! written field by field comes out as, and what a loop the unroller took apart comes out as, and
//! in neither of those does any one reader own the address. If every reader can take it then
//! nothing reads the `lea` any more and it goes, and the arithmetic moved into addressing modes
//! that were doing an addition anyway. If one reader cannot, folding into the rest buys nothing:
//! the `lea` stays where it is for the one that refused, the address is worked out twice rather
//! than once, and the registers it reads are now live across every reader as well. So it is all of
//! them or none of them, and that is a property of the set rather than of a pair.
//!
//! # What it will not do
//!
//! A set with a reader in it that cannot take the address. Each of the refusals below is one
//! reader's, and any one of them turns down the whole set it belongs to.
//!
//! An address relative to a symbol, with more than one reader. A reader that reads through a
//! register has room in it for a register and a displacement, and an address made of registers and
//! a displacement goes into that room whoever takes it. A symbol does not: the reader has to name
//! the symbol, which is a whole address word rather than a register number, so each reader that
//! takes one grows by the difference and several readers pay it several times while the `lea` is
//! saved once. Taking those as well loses 2643 bytes over the corpus at -O2 and gains 386, and the
//! loss is almost all soft float and bit counting expansions, which read one global thirty or
//! forty times each. One reader keeps the old answer, since there the address word is written once
//! either way and what goes is the whole `lea`.
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
//! A register that something writes between the address and the last of its readers. Machine IR is
//! in SSA form until the allocator has run, so a virtual register cannot be, but a physical one
//! can: the frame pointer and the stack pointer are already physical here, and a call in between
//! writes every register it is allowed to. Rather than ask which registers are the exceptions, the
//! walk below drops a candidate the moment anything writes a register its address reads. The last
//! reader rather than the first is what makes this the set's question too, since a write after the
//! first reader and before the second is a write the one at a time version would never have seen.
//!
//! # The addresses into the frame
//!
//! A local's place in the frame and an argument's place in the caller's area is a distance from the
//! stack pointer, and there is no frame until the allocator has finished, so [`crate::lower`]
//! leaves those instructions with a zero in the displacement and [`crate::finish`] writes the
//! number in later against a list of which instruction is which.
//!
//! This used to refuse them for that reason, and refusing was expensive: it is the shape of every
//! access to a local that has to go through its address, and of every argument that arrives in the
//! caller's area. What it takes to fold one is that the entry moves. The instruction the list names
//! goes away and the ones that took the
//! address arrive, so [`Pending`] rewrites the list as the fold is applied, and `finish` adds the
//! frame's offset to the displacement rather than assigning it, because the reader brought a
//! displacement of its own and the field it is reading is some way past where the object starts.
//! tamnd/rucc#784.
//!
//! What they do not get is the whole of the set rule above. An address into the frame is off the
//! stack pointer and a memory operand based on the stack pointer needs an index byte on this
//! machine whether or not anything is indexed, so a reader that takes one grows by more than a
//! reader that takes an address in an ordinary register does. Past three of them the bytes the
//! readers put on are more than the whole `lea` was, which is the same arithmetic as the symbol
//! above and comes out at a different number. `FRAME_READERS` below has the measurement.
//!
//! # Where it runs
//!
//! After selection and before the allocator, which is the one window where both instructions
//! exist and the registers are still virtual. Running it after allocation would work on the
//! arithmetic and would be reading a register file where the reader's base may have been reused
//! for something else in between.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{FrameInsts, Role};

/// The addresses [`crate::finish`] has still to write a displacement into.
///
/// Two lists, because the frame holds two areas this pass runs before the layout of: a local's
/// address is an offset into this function's own objects and a stack argument's is an offset into
/// the caller's area. What they have in common is the shape, a `lea` off the stack pointer with the
/// displacement left at zero, and what this type is for is that folding one of those away has to
/// move the entry rather than lose it.
///
/// This used to be a set of instructions the pass refused to touch, and refusing was expensive.
/// Every access to a local through its address was a `lea` and then a memory instruction reading
/// through the register it wrote, which is one instruction more than it needs, on the shape any
/// function whose locals have their address taken is full of. tamnd/rucc#784.
#[derive(Debug)]
pub struct Pending<'a> {
    /// Which instruction carries the address of which of this function's stack objects.
    pub addresses: &'a mut Vec<(mir::Inst, usize)>,
    /// Which instruction reads which of the arguments the caller passed on the stack.
    pub arguments: &'a mut Vec<(mir::Inst, u32)>,
}

impl Pending<'_> {
    /// Moves an entry from an address that has gone to the instructions that took it.
    ///
    /// One entry becomes as many as there were readers, because an address every reader has room
    /// for is handed to all of them, and each of those now carries a displacement of its own that
    /// the frame layout has still to be added to.
    ///
    /// An address on either list reads the stack pointer and nothing else, so it never reads a
    /// register another one of them wrote, which is what makes it impossible for a reader to end up
    /// on a list twice and be given two offsets.
    fn moved(&mut self, from: mir::Inst, into: &[mir::Inst]) {
        move_entries(self.addresses, from, into);
        move_entries(self.arguments, from, into);
    }

    /// Whether this instruction is one of the two lists, which is how many readers it may go to.
    fn holds(&self, inst: mir::Inst) -> bool {
        let named = self.addresses.iter().map(|&(at, _)| at);
        named.chain(self.arguments.iter().map(|&(at, _)| at)).any(|at| at == inst)
    }
}

/// How many readers an address into the frame may be handed to.
///
/// There is a limit at all for the same reason a symbol has one, in the list above. An address into
/// the frame is off the stack pointer, and a memory operand whose base is the stack pointer needs
/// an index byte on this machine whether or not anything is indexed, so every reader that takes one
/// grows by that byte and by the displacement while the `lea` is saved once. Reading through a
/// register the `lea` wrote is three or four bytes and reading the same place off the stack pointer
/// is five or eight, against the five or eight the `lea` itself costs, so the readers are ahead of
/// it while there are few of them and behind it once there are enough.
///
/// Three is where they turn, measured. Over the 1838 corpus programs that come out of both
/// compilers at `-O2`, one reader is 757 bytes better than folding none of them, two is 806, three
/// is 868, four is 848 and five is 520. Handing them to every reader with room, which is what every
/// other address gets, is 528 bytes worse than folding none: 97 programs larger by 1117 bytes
/// against 100 smaller by 589. Up to three, only two programs anywhere in the corpus are larger at
/// all, by two bytes each.
///
/// 690 of the 868 are the ten `long-double` programs, which is the shape this is about at its
/// plainest. A `long double` argument arrives in the caller's area and the `fld` that reads it is
/// its only reader, so the address goes and the read costs nothing more than it did.
const FRAME_READERS: usize = 3;

/// The half of [`Pending::moved`] that does not care what the entry says.
fn move_entries<T: Copy>(list: &mut Vec<(mir::Inst, T)>, from: mir::Inst, into: &[mir::Inst]) {
    let Some(at) = list.iter().position(|&(inst, _)| inst == from) else { return };
    let (_, what) = list[at];
    list.splice(at..=at, into.iter().map(|&inst| (inst, what)));
}

/// Folds every address computation that one memory operand reads, and gives back how many.
///
/// `pending` is the addresses [`crate::finish`] has still to write a displacement into, and folding
/// one moves its entry to the instruction that took it. The displacement composed in by the fold
/// stays where it is and the frame's offset is added to it later, which is why that write is an
/// addition rather than an assignment.
///
/// Run after lowering and before allocation. Running it twice can find more than running it once.
/// Folding a `lea` into a second `lea` leaves that second one foldable in turn, and the walk below
/// takes those in the one pass since it goes forwards. What it does not take in the one pass is the
/// other order, where the second `lea` has a reader of its own and goes before the first one's set
/// is complete, and that is a set the next run finds whole.
pub fn addresses(
    func: &mut mir::Func,
    insts: &FrameInsts,
    names: &mut Interner,
    pending: &mut Pending<'_>,
) -> usize {
    let lea = mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, insts.lea)));
    let reads = reads(func);
    let mut folded = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        // One `lea` per register it wrote, along with the folds its readers so far have agreed to.
        // A register leaves the table the moment the set can no longer be all of them: anything
        // writes what the address reads, or a reader turns up that cannot take it.
        let mut open: HashMap<mir::Reg, Open> = HashMap::new();
        for inst in func.insts(block).collect::<Vec<_>>() {
            if let Some(ready) = offer(func, &mut open, inst) {
                for folding in &ready.folds {
                    let operands = func.push_operands(&folding.operands);
                    let mem = func.add_amode(folding.amode);
                    func[folding.into].operands = operands;
                    func[folding.into].mem = Some(mem);
                    folded += 1;
                }
                let took: Vec<mir::Inst> = ready.folds.iter().map(|fold| fold.into).collect();
                pending.moved(ready.from, &took);
                func.remove_inst(ready.from);
                // Anything still open that was going to fold into the instruction just removed is
                // holding a plan for an instruction that is not there any more. That is a chain
                // whose middle went first, and the outer address waits for the next run of the
                // pass rather than being written into a gap.
                open.retain(|_, held| held.folds.iter().all(|fold| fold.into != ready.from));
            }
            for written in written(func, inst) {
                open.retain(|reg, held| *reg != written && !touches(func, held.from, written));
            }
            if func[inst].opcode == lea {
                let room = if pending.holds(inst) { FRAME_READERS } else { usize::MAX };
                match folding_def(func, &reads, inst) {
                    Some((reg, wanted))
                        if wanted <= room && (wanted == 1 || fits_every_reader(func, inst)) =>
                    {
                        open.insert(reg, Open { from: inst, wanted, folds: Vec::new() });
                    }
                    _ => {}
                }
            }
        }
    }
    folded
}

/// An address computation whose readers are still being counted.
struct Open {
    /// The address instruction, which goes once every one of its readers has taken it.
    from: mir::Inst,
    /// How many reads of the register it wrote there are in the whole function.
    wanted: usize,
    /// The folds agreed to so far, which are applied together or not at all.
    folds: Vec<Folding>,
}

/// Offers an instruction the addresses that are open, and gives back the set that is now complete.
///
/// Every open register this instruction reads either takes the address into its own memory operand
/// or ends the chance for the whole set. Reading it any other way is what makes it a reader nothing
/// can fold into, and one of those is enough, so the register is dropped rather than the read being
/// passed over. Reading it twice in the one instruction counts as that too, since only one of the
/// two reads is the memory operand and the other would be left naming a register nothing writes.
fn offer(func: &mir::Func, open: &mut HashMap<mir::Reg, Open>, inst: mir::Inst) -> Option<Open> {
    let folding = candidate(func, open, inst);
    let takes = |reg: mir::Reg| folding.as_ref().is_some_and(|fold| fold.base == reg);
    let refused: Vec<mir::Reg> = open
        .keys()
        .copied()
        .filter(|&reg| {
            let times = times_read(func, inst, reg);
            times > 0 && !(times == 1 && takes(reg))
        })
        .collect();
    for reg in refused {
        open.remove(&reg);
    }
    let folding = folding?;
    let base = folding.base;
    let held = open.get_mut(&base)?;
    held.folds.push(folding);
    if held.folds.len() < held.wanted {
        return None;
    }
    open.remove(&base)
}

/// Whether an address is one every reader can carry in the room it already has, which is what
/// makes handing it to more than one of them free.
///
/// A reader that reads an address through a register has room in it for a register and for a
/// displacement, and an address made of registers and a displacement fits in exactly that room
/// however many readers take it. An address relative to a symbol does not. The reader was naming a
/// register and now has to name the symbol, which is a whole address word rather than a register
/// number, so each reader that takes it grows by the difference and several readers pay it several
/// times over while the `lea` is only saved once.
///
/// The measurement is what settled the size of that: folding symbol relative addresses into every
/// reader as well loses 2643 bytes over the corpus at -O2 against 386 gained, and the 2643 is
/// almost all soft float and bit counting expansions, which read one global thirty or forty times
/// each and are the longest runs of straight line code in the corpus.
///
/// One reader is a different question and keeps the old answer, since there the address word is
/// written once either way and what goes is the whole `lea`.
fn fits_every_reader(func: &mir::Func, inst: mir::Inst) -> bool {
    func[inst].mem.is_some_and(|mem| func[mem].symbol.is_none())
}

/// How many of an instruction's operands read that register.
fn times_read(func: &mir::Func, inst: mir::Inst, reg: mir::Reg) -> usize {
    func[func[inst].operands]
        .iter()
        .filter(|operand| operand.role == Role::Use && operand.reg == reg)
        .count()
}

/// How many times each virtual register is read, counting the arguments an edge carries.
///
/// A `lea` is only worth folding when the instructions folding it are the whole of what reads the
/// register, since folding does not delete the `lea` for anybody else and doing the address twice
/// is not a saving. The count is what says when the set is complete, and it is taken over the whole
/// function rather than over the block, so a read anywhere else is a set that never completes and
/// an address that stays where it is. An argument on an edge is a read like any other and is not in
/// any operand vector, which is the one place this is easy to get wrong.
///
/// [`crate::layout`] asks the same question about the byte a comparison wrote, for the same
/// reason and while the registers are still virtual for the same reason, so it reads this rather
/// than counting again.
pub(crate) fn reads(func: &mir::Func) -> HashMap<mir::Reg, usize> {
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

/// The one virtual register an instruction writes, and how many reads of it there are, when it
/// writes exactly one and something reads it.
///
/// A register nothing reads is left alone rather than folded into nothing, since an address whose
/// answer is never wanted is dead code and belongs to the pass that removes dead code.
fn folding_def(
    func: &mir::Func,
    reads: &HashMap<mir::Reg, usize>,
    inst: mir::Inst,
) -> Option<(mir::Reg, usize)> {
    let operands = &func[func[inst].operands];
    let mut defs = operands.iter().filter(|operand| operand.role != Role::Use);
    let def = defs.next()?;
    if defs.next().is_some() || !def.reg.is_virtual() {
        return None;
    }
    let wanted = *reads.get(&def.reg)?;
    (wanted > 0).then_some((def.reg, wanted))
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
    /// The reader this rewrites, which is not always the instruction being looked at, since the
    /// set is applied when its last reader arrives rather than as each one agrees.
    into: mir::Inst,
    /// The register the address instruction wrote, which is what ties this to its set.
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
fn candidate(func: &mir::Func, open: &HashMap<mir::Reg, Open>, inst: mir::Inst) -> Option<Folding> {
    let base = base_reg(func, inst)?;
    let from = open.get(&base)?.from;
    let address = func[func[from].mem?];
    // The reader holds the base in its last operand, and [`offer`] is what checks that nothing else
    // in the same instruction names it. So the composed address is the `lea`'s with the reader's
    // displacement added, and the only thing that can go wrong is the width of the field it goes
    // in.
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
    Some(Folding { into: inst, base, operands, amode })
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

    /// The pass, run over a function with nothing owed a frame offset, which is most of these.
    ///
    /// The lists are still there because the pass rewrites them, and a test that is about what it
    /// wrote in them builds its own rather than calling this.
    fn folds(func: &mut mir::Func, names: &mut Interner) -> usize {
        let (mut locals, mut arguments) = (Vec::new(), Vec::new());
        addresses(
            func,
            &FRAME,
            names,
            &mut Pending { addresses: &mut locals, arguments: &mut arguments },
        )
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

        assert_eq!(folds(&mut func, &mut names), 1);

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

        assert_eq!(folds(&mut func, &mut names), 1);

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

        assert_eq!(folds(&mut func, &mut names), 1);

        let inst = func.insts(block).next().expect("the store is still there");
        let regs: Vec<mir::Reg> = func[func[inst].operands].iter().map(|op| op.reg).collect();
        assert_eq!(regs, vec![value, array, index], "the value the store writes went missing");
        assert_eq!(func[func[inst].mem.expect("a memory operand")].scale, 8);
    }

    /// One address at three offsets, which is what a structure written field by field comes out
    /// as. Every reader can carry the whole of it in its own mode, so all three take it and the
    /// `lea` has nothing left reading it. This is the case section 37.4 says the pass is for.
    #[test]
    fn an_address_every_reader_can_take_is_folded_into_all_of_them() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let store = op(&mut names, "mov_mr_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        for offset in [0, 12, 28] {
            func.build(block, store)
                .uses(value, GPR)
                .mem(mir::Mem::at(mir::Operand::read(address, GPR)).plus(offset))
                .finish();
        }

        assert_eq!(folds(&mut func, &mut names), 3);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 3, "the address is still worked out on its own: {left:?}");
        let disps: Vec<i32> = left.iter().map(|(_, amode)| amode.disp).collect();
        assert_eq!(disps, vec![16, 28, 44], "each store is at its own offset from the address");
        for inst in func.insts(block).collect::<Vec<_>>() {
            assert_eq!(address_regs(&func, inst), vec![array]);
        }
    }

    /// Three readers and the middle one has an index of its own. Folding into the other two would
    /// leave the `lea` where it is for the third, so the address would be worked out twice rather
    /// than once and the two folds would have bought nothing but a longer live range for what it
    /// reads. All or nothing over the set means none of them.
    #[test]
    fn an_address_one_reader_cannot_take_is_folded_into_none_of_them() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        for at in 0..3 {
            let value = func.new_vreg(GPR);
            let mem = mir::Mem::at(mir::Operand::read(address, GPR));
            let mem = if at == 1 { mem.indexed(mir::Operand::read(index, GPR), 4) } else { mem };
            func.build(block, load).def(value, GPR).mem(mem).finish();
        }

        assert_eq!(folds(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 4);
    }

    /// An indexed address with two readers, which both of them can take. The index goes into the
    /// room the reader already has for one, the same as the base does, so this is the ordinary
    /// case rather than a special one.
    #[test]
    fn an_indexed_address_every_reader_can_take_is_folded_into_all_of_them() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(
                mir::Mem::at(mir::Operand::read(array, GPR))
                    .indexed(mir::Operand::read(index, GPR), 4),
            )
            .finish();
        for offset in [0, 8] {
            let value = func.new_vreg(GPR);
            func.build(block, load)
                .def(value, GPR)
                .mem(mir::Mem::at(mir::Operand::read(address, GPR)).plus(offset))
                .finish();
        }

        assert_eq!(folds(&mut func, &mut names), 2);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 2, "the address is gone and both loads carry it: {left:?}");
        let disps: Vec<i32> = left.iter().map(|(_, amode)| amode.disp).collect();
        assert_eq!(disps, vec![0, 8], "each load is at its own offset from the address");
        for inst in func.insts(block).collect::<Vec<_>>() {
            assert_eq!(address_regs(&func, inst), vec![array, index]);
        }
    }

    /// A symbol relative address with two readers, which both of them could take and which is left
    /// alone anyway. Each reader would have to name the symbol where it names a register now, and
    /// a symbol is a whole address word, so two readers write that word twice to save one `lea`
    /// that wrote it once. The corpus says that is a loss well before the reader count gets large.
    #[test]
    fn a_symbol_address_with_more_than_one_reader_is_left_where_it_is() {
        let (mut names, mut func, block) = empty();
        let address = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        let cell = names.intern("cell");
        func.build(block, lea).def(address, GPR).mem(mir::Mem::of(cell)).finish();
        for offset in [0, 8] {
            let value = func.new_vreg(GPR);
            func.build(block, load)
                .def(value, GPR)
                .mem(mir::Mem::at(mir::Operand::read(address, GPR)).plus(offset))
                .finish();
        }

        assert_eq!(folds(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 3);
    }

    /// Two readers and one of them is in another block, which is the same refusal as the single
    /// reader case and is caught by a different half of the pass. The count of reads is taken over
    /// the whole function, so a set that leaves one out never becomes complete.
    #[test]
    fn an_address_read_outside_the_block_as_well_is_left_where_it_is() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let array = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        for at in [block, next] {
            let value = func.new_vreg(GPR);
            func.build(at, load)
                .def(value, GPR)
                .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
                .finish();
        }
        *func.succs_mut(block) = vec![mir::BlockCall::to(next)];

        assert_eq!(folds(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// A register the address reads, written between the first reader and the second. This is the
    /// one refusal the set adds that the pair version had no way to need, since a write after the
    /// only reader is a write nobody was ever going to fold across.
    #[test]
    fn a_write_between_one_reader_and_the_next_ends_the_chance_for_the_set() {
        let (mut names, mut func, block) = empty();
        let array = mir::Reg::physical(RDI);
        let address = func.new_vreg(GPR);
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        let put = op(&mut names, "mov_ri_64");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        func.build(block, load)
            .def(first, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();
        func.build(block, put).def(array, GPR).imm(7).finish();
        func.build(block, load)
            .def(second, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)).plus(4))
            .finish();

        assert_eq!(folds(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 4);
    }

    /// A reader that is not reading it as an address at all. There is nowhere in an ordinary
    /// operand to put a base and an index and a displacement, so that read is one no fold can take
    /// and it turns down the set the way any other refusal does.
    #[test]
    fn an_address_something_reads_as_a_plain_operand_is_left_where_it_is() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let value = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        let add = op(&mut names, "add_rr_64");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        func.build(block, load)
            .def(value, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();
        func.build(block, add).def(sum, GPR).uses(address, GPR).finish();

        assert_eq!(folds(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 3);
    }

    /// The one instruction reading the address twice, once as the value it stores and once as the
    /// place it stores to. Only one of those two reads is the memory operand, so folding would
    /// leave the other one naming a register nothing writes any more.
    #[test]
    fn an_address_the_one_instruction_reads_twice_is_left_where_it_is() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let store = op(&mut names, "mov_mr_64");
        func.build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        func.build(block, store)
            .uses(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(folds(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// A chain whose middle has a reader of its own, so the inner address is complete while the
    /// outer one is still waiting for its second reader. Folding the inner one away takes with it
    /// the instruction the outer one's plan was written for, and the outer one waits rather than
    /// being written into a gap. The second run is where it lands, which is the whole of what
    /// waiting costs.
    #[test]
    fn a_chain_whose_middle_goes_first_leaves_the_outer_address_for_the_next_run() {
        let (mut names, mut func, block) = empty();
        let array = func.new_vreg(GPR);
        let outer = func.new_vreg(GPR);
        let inner = func.new_vreg(GPR);
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        func.build(block, lea)
            .def(outer, GPR)
            .mem(mir::Mem::at(mir::Operand::read(array, GPR)).plus(16))
            .finish();
        func.build(block, lea)
            .def(inner, GPR)
            .mem(mir::Mem::at(mir::Operand::read(outer, GPR)).plus(4))
            .finish();
        func.build(block, load)
            .def(first, GPR)
            .mem(mir::Mem::at(mir::Operand::read(inner, GPR)))
            .finish();
        func.build(block, load)
            .def(second, GPR)
            .mem(mir::Mem::at(mir::Operand::read(outer, GPR)).plus(8))
            .finish();

        assert_eq!(folds(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block).len(), 3, "the inner address is still there");

        assert_eq!(folds(&mut func, &mut names), 2);
        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 2, "the outer address is still there: {left:?}");
        let disps: Vec<i32> = left.iter().map(|(_, amode)| amode.disp).collect();
        assert_eq!(disps, vec![20, 24], "the two loads are at the two composed offsets");
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

        assert_eq!(folds(&mut func, &mut names), 0);
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

        assert_eq!(folds(&mut func, &mut names), 0);
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

        assert_eq!(folds(&mut func, &mut names), 0);
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

        assert_eq!(folds(&mut func, &mut names), 0);
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

        assert_eq!(folds(&mut func, &mut names), 2);

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

        assert_eq!(folds(&mut func, &mut names), 1);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].1.symbol, Some(global));
        assert_eq!(left[0].1.disp, 12);
    }

    /// An address into the frame, which reads as an address of nothing until `finish` writes the
    /// distance in. It folds like any other and the entry moves to the instruction that took it, so
    /// the distance is still written into something that runs, and into the reader's own
    /// displacement rather than over it.
    #[test]
    fn an_address_whose_displacement_is_still_to_be_written_folds_and_takes_its_entry_with_it() {
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
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)).plus(8))
            .finish();

        let (mut locals, mut arguments) = (vec![(local, 3)], Vec::new());
        let mut pending = Pending { addresses: &mut locals, arguments: &mut arguments };
        assert_eq!(addresses(&mut func, &FRAME, &mut names, &mut pending), 1);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 1, "the address is worked out twice: {left:?}");
        assert_eq!(left[0].1.disp, 8, "the field's offset is what finish adds the frame's to");
        let reader = func.insts(block).next().expect("the load is still there");
        assert_eq!(locals, vec![(reader, 3)], "the offset is owed to whoever took the address");
    }

    /// One address into the frame read at that many offsets, which is a structure written field by
    /// field. Gives back how many folded, which instructions are in the block afterwards, and what
    /// the caller is still owed an offset into.
    fn a_frame_address(readers: u32) -> (usize, Vec<mir::Inst>, Vec<(mir::Inst, u32)>) {
        let (mut names, mut func, block) = empty();
        let sp = mir::Reg::physical(RDI);
        let address = func.new_vreg(GPR);
        let lea = op(&mut names, FRAME.lea);
        let load = op(&mut names, "mov_rm_32");
        let local = func
            .build(block, lea)
            .def(address, GPR)
            .mem(mir::Mem::at(mir::Operand::read(sp, GPR)))
            .finish();
        for at in 0..readers {
            let value = func.new_vreg(GPR);
            func.build(block, load)
                .def(value, GPR)
                .mem(
                    mir::Mem::at(mir::Operand::read(address, GPR))
                        .plus(i32::try_from(at).unwrap_or(0) * 4),
                )
                .finish();
        }

        let (mut locals, mut arguments) = (Vec::new(), vec![(local, 7)]);
        let mut pending = Pending { addresses: &mut locals, arguments: &mut arguments };
        let folded = addresses(&mut func, &FRAME, &mut names, &mut pending);
        assert!(locals.is_empty(), "an argument is owed off the other list");
        (folded, func.insts(block).collect(), arguments)
    }

    /// One entry on the list becomes one per reader, since each of them now carries a displacement
    /// the frame's offset has to be added to and there is no instruction left to add it to instead.
    #[test]
    fn an_address_into_the_frame_that_three_readers_take_is_owed_to_all_of_them() {
        let (folded, left, owed) = a_frame_address(3);
        assert_eq!(folded, 3);
        assert_eq!(left.len(), 3, "the address is not its own instruction any more");
        assert_eq!(owed, vec![(left[0], 7), (left[1], 7), (left[2], 7)]);
    }

    /// And the reader after that is one too many, so none of them takes it. What each of them would
    /// put on is more than what the whole address instruction costs, which is [`FRAME_READERS`].
    #[test]
    fn an_address_into_the_frame_a_fourth_reader_wants_is_left_where_it_is() {
        let (folded, left, owed) = a_frame_address(4);
        assert_eq!(folded, 0);
        assert_eq!(left.len(), 5, "the address and its four readers");
        assert_eq!(owed, vec![(left[0], 7)], "the offset is still owed to the address itself");
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

        assert_eq!(folds(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }
}
