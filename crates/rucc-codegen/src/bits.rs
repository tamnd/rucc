//! Taking out a conversion whose bits nothing reads.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` section 37.4.
//!
//! A register is one register at every width, and what says how much of it is in play is the
//! instruction naming it. `movzbl %sil, %edi` writes thirty two bits of `rdi` and reads eight of
//! `rsi`, and if the only thing that ever reads `rdi` is a `movb`, then the twenty four bits the
//! widening worked out are bits nobody ever looks at. What is left of the widening once those bits
//! are taken away is a copy of eight bits into a register, which is what the instruction after it
//! was going to read anyway, so the widening goes and its readers read its source instead.
//!
//! That is the bit group liveness of `gcc/ext-dce.cc` at the width this compiler needs it at.
//! Liveness answers whether a register is read at all, this answers how much of it is read, and
//! the second question is the first one asked per group of bits rather than per register. Section
//! 37.4 says to build this one of the two passes it offers, because it is more general than
//! compare elimination and because the analysis is the liveness the allocator already computes
//! with a number on it.
//!
//! # Where the conversions come from
//!
//! Not from code anybody wrote. C promotes nearly every operand of nearly every expression to
//! `int` before doing anything with it, so a program that adds two `char`s widens both of them,
//! adds at thirty two bits and stores eight, and the front end writes every one of those
//! conversions out because each of them is in the language's own description of what the program
//! means. `crate::widths` and tier four of the rewrite rules take the ones that are two
//! instructions next to each other in the same block. What is left for this pass is the ones that
//! are not: a conversion in one block whose readers are in another, and a conversion the selector
//! itself wrote because the machine instruction it picked wanted its operand at a width the value
//! did not arrive at.
//!
//! # What it finds, measured
//!
//! Both directions of conversion are in scope and only one of them turns up, which was not what
//! was expected and is worth writing down rather than rounding off. Over the 1916 programs of
//! tamnd/rucc-corpus at `-O2` this takes out 970 instructions and puts back 45, and not one of
//! the 7040 widenings in that assembly is among them: the count of `movz` and `movs` is the same
//! before and after. What goes is 469 `movl`, 259 `movw` and 242 `movb` between registers, which
//! are the narrowings, and the 45 that come back are `movq`, which is the allocator wanting a
//! plain copy where a narrowing had been doing that job as well as its own.
//!
//! That is tier four of the rewrite rules having already been through the corpus. A widening the
//! rules could not reach is one whose upper bits some reader really does read, and there is
//! nothing here for a bit counter to find in it. A narrowing is the other way round: the machine
//! writes one where a value is put in a register at a width, and whether the bits above it matter
//! is a question about every reader of the result rather than about the pair, which is the
//! question only this pass asks.
//!
//! 2091 bytes of `.text` over the corpus, 76 programs smaller and two larger by a byte each, and
//! 2048 bytes off SQLite's amalgamation at `-O2`. The two that grow are an eight bit division,
//! where every narrowing that goes was also the move that got the answer out of the register the
//! division fixes, so the allocator writes a full width copy of the same pair in its place. Nine
//! of the ten are the same length either way and the tenth is `movb %dl, %bl` becoming
//! `movq %rdx, %rbx`, which is the one byte: the byte names of those two registers need no prefix
//! and the sixty four bit move needs the one that says so.
//!
//! # The analysis
//!
//! One number per register, which is how many of its low bits anything reads. It starts at none
//! and grows, so a register nothing has been seen to read yet is one whose answer is still being
//! worked out rather than one nothing reads.
//!
//! Three things raise it. An instruction reading a register raises it to the width that
//! instruction names the operand at, which is [`rucc_target::BitInsts::width`] and is the target's
//! answer rather than this pass's. An edge carrying a register into a block raises it to whatever
//! the parameter it arrives as needs, which is what carries the answer across a block boundary and
//! is the whole reason this finds anything the rules do not. And an instruction that copies the
//! low bits of its source raises its source only as far as its own result is read, since the bits
//! of the source above that are bits it puts nowhere anything reads.
//!
//! The last of those is what makes the answer a fixpoint rather than a walk: a chain of
//! conversions passes the number back along itself, and how far it passes depends on a number the
//! same pass is still working out. It only ever grows and it is bounded by the widest operand on
//! the machine, so it settles.
//!
//! # What it will not do
//!
//! An operand the target's description does not name at a width. An address register, an operand
//! of an opcode written as no instruction at all, and an opcode from somewhere other than this
//! target all answer that they read everything, which is section 37.7's warning honoured by
//! construction: a store reads every bit of the value it stores because the description says the
//! operand is as wide as the store is, and anything the description is silent about is treated as
//! reading the lot rather than as reading nothing.
//!
//! A physical register on either side. Machine IR is in SSA form until the allocator has run, so a
//! virtual register is written once and the register a reader would be sent to instead still holds
//! what it held. A physical one is not: the frame pointer and the stack pointer are already
//! physical here and a call writes every register it is allowed to, so sending a reader to one of
//! those would be sending it to whatever happened to be there.
//!
//! A conversion whose result is read as wide as it is written. That is a widening whose upper bits
//! somebody does read, which is the whole instruction doing its job.
//!
//! A conversion whose result nothing reads at all. That is an instruction that computes something
//! nobody wants, which is dead code rather than dead bits, and taking it out here would be this
//! pass answering a question it was not asked and reporting a number that says it found widenings
//! it had not. What this is about is a register something reads less of than was put in it.
//!
//! # Where it runs
//!
//! After selection and before allocation, which is the window where the machine instructions exist
//! and the registers are still virtual. Section 37.6 puts it third in the group that runs there,
//! after combining and if-conversion and before compare elimination and addressing-mode folding,
//! and that is where `crate::pipeline` calls it.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{BitInsts, Constraint, Role};

/// How much of a register a read that could be of any of it wants.
///
/// Every operand the target does not describe gets this, and no rewrite fires over a register that
/// has it, since no instruction on any machine writes more bits than this many.
const EVERYTHING: u32 = u32::MAX;

/// Takes out every conversion whose result nothing reads above the width of its source, and gives
/// back how many.
///
/// Each one that goes takes its readers with it: they are pointed at the source instead, which
/// holds the same bits as the result did for as far as anything was looking.
///
/// Run after lowering and before allocation. Running it once is enough, because the analysis is
/// over the whole function at once and a chain of conversions is settled by the fixpoint rather
/// than by a second run.
pub fn dead(func: &mut mir::Func, insts: &BitInsts, names: &Interner) -> usize {
    let wanted = demand(func, insts, names);
    let mut sent: HashMap<mir::Reg, mir::Reg> = HashMap::new();
    let mut gone: Vec<mir::Inst> = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            let Some(name) = opcode(func, insts, names, inst) else { continue };
            if !(insts.copies_low)(name) {
                continue;
            }
            let Some((def, source)) = conversion(func, inst) else { continue };
            let kept = (insts.width)(name, SOURCE).unwrap_or(EVERYTHING);
            let read = wanted.get(&def).copied().unwrap_or(0);
            if read == 0 || read > kept {
                continue;
            }
            sent.insert(def, source);
            gone.push(inst);
        }
    }
    if gone.is_empty() {
        return 0;
    }
    rename(func, &chased(&sent));
    for &inst in &gone {
        func.remove_inst(inst);
    }
    gone.len()
}

/// Where a conversion holds the register it reads.
///
/// A conversion is one definition and one use in that order, which is what [`conversion`] checks
/// rather than assumes, so the source is at one.
const SOURCE: u8 = 1;

/// How many low bits of each register something reads.
///
/// Absent means none, which is a register nothing has been seen to read. That is the right
/// starting point rather than a wrong one to be corrected later: the answer only grows, so a
/// register still absent when the walk settles is one nothing reads at all.
fn demand(func: &mir::Func, insts: &BitInsts, names: &Interner) -> HashMap<mir::Reg, u32> {
    let mut wanted: HashMap<mir::Reg, u32> = HashMap::new();
    loop {
        let mut moved = false;
        for block in func.blocks() {
            for inst in func.insts(block) {
                let name = opcode(func, insts, names, inst);
                // A conversion puts the low bits of its source in its result and nothing else, so
                // the bits of the source above however much of the result is read are bits it
                // takes nowhere. Anything else reads its operand at the width it names it at.
                let copies = name.is_some_and(|name| (insts.copies_low)(name));
                let through = conversion(func, inst)
                    .filter(|_| copies)
                    .map_or(EVERYTHING, |(def, _)| wanted.get(&def).copied().unwrap_or(0));
                let operands = &func[func[inst].operands];
                for (at, operand) in operands.iter().enumerate() {
                    if operand.role != Role::Use {
                        continue;
                    }
                    let Ok(at) = u8::try_from(at) else { continue };
                    let asked = read(name, insts, operands, at).min(through);
                    moved |= raise(&mut wanted, operand.reg, asked);
                }
            }
            for call in &func[block].succs {
                for (arg, param) in call.args.iter().zip(&func[call.block].params) {
                    let asked = wanted.get(&param.reg).copied().unwrap_or(0);
                    moved |= raise(&mut wanted, *arg, asked);
                }
            }
        }
        if !moved {
            return wanted;
        }
    }
}

/// Raises how much of a register is read, and says whether that changed anything.
fn raise(wanted: &mut HashMap<mir::Reg, u32>, reg: mir::Reg, bits: u32) -> bool {
    let had = wanted.entry(reg).or_insert(0);
    if *had >= bits {
        return false;
    }
    *had = bits;
    true
}

/// How many bits of the operand at that index the instruction reads.
///
/// The target's description is asked first and is the answer whenever it has one. Where it has
/// none the operand may still be a tied one, which is the operand an instruction of this shape
/// reads and writes in the one place: the machine writes it once and the assembly names it once,
/// so the description names the definition and says nothing about the use beside it. Those two
/// are the same register at the same width by the time the allocator has finished, so the width
/// of the definition is the width of the use.
///
/// Anything left over reads everything, which is what keeps an address register, an opcode written
/// as no instruction and an opcode from another target from being believed to read nothing.
fn read(name: Option<&str>, insts: &BitInsts, operands: &[mir::Operand], at: u8) -> u32 {
    let Some(name) = name else { return EVERYTHING };
    if let Some(bits) = (insts.width)(name, at) {
        return bits;
    }
    for (index, operand) in operands.iter().enumerate() {
        if operand.role == Role::Use || operand.constraint != Constraint::Reuse(at) {
            continue;
        }
        let Ok(index) = u8::try_from(index) else { continue };
        return (insts.width)(name, index).unwrap_or(EVERYTHING);
    }
    EVERYTHING
}

/// The name this target knows an instruction by, for an instruction that is one of this target's.
///
/// The opcode in machine IR carries the target's prefix, because a function in the middle of being
/// compiled holds instructions of one machine and the prefix is what says which. Anything without
/// it is not something this description covers, and the rest of the pass treats that as knowing
/// nothing rather than as knowing it is safe.
fn opcode<'a>(
    func: &mir::Func,
    insts: &BitInsts,
    names: &'a Interner,
    inst: mir::Inst,
) -> Option<&'a str> {
    names.resolve(func[inst].opcode.name()).strip_prefix(insts.prefix)
}

/// The register a conversion writes and the register it reads, when it is one this may take out.
///
/// One definition and one use, both of them virtual, and no memory operand. The shape is checked
/// rather than taken on trust from the opcode, since what the rewrite does is send every reader of
/// the first register to the second and that is only the same program when there is exactly one of
/// each.
fn conversion(func: &mir::Func, inst: mir::Inst) -> Option<(mir::Reg, mir::Reg)> {
    if func[inst].mem.is_some() {
        return None;
    }
    let operands = &func[func[inst].operands];
    let [def, source] = operands else { return None };
    if def.role == Role::Use || source.role != Role::Use {
        return None;
    }
    if !def.reg.is_virtual() || !source.reg.is_virtual() {
        return None;
    }
    Some((def.reg, source.reg))
}

/// The same map with every chain in it followed to its end.
///
/// A chain is two conversions where the outer one reads what the inner one wrote, and both of them
/// going means a reader of the outer one belongs to the inner one's source rather than to the
/// inner one. The walk ends because machine IR is in SSA form here and every step goes to a
/// register written earlier in the function, and the bound is there so that a map built any other
/// way stops as well.
fn chased(sent: &HashMap<mir::Reg, mir::Reg>) -> HashMap<mir::Reg, mir::Reg> {
    sent.iter()
        .map(|(&from, &first)| {
            let mut into = first;
            for _ in 0..sent.len() {
                match sent.get(&into) {
                    Some(&next) => into = next,
                    None => break,
                }
            }
            (from, into)
        })
        .collect()
}

/// Sends every read of a register that is going to whatever holds the bits it held.
///
/// The arguments an edge carries are reads like any other and are not in any operand vector, which
/// is the one place this is easy to get wrong.
fn rename(func: &mut mir::Func, sent: &HashMap<mir::Reg, mir::Reg>) {
    for block in func.blocks().collect::<Vec<_>>() {
        for inst in func.insts(block).collect::<Vec<_>>() {
            let operands = func[inst].operands;
            for operand in &mut func[operands] {
                if operand.role != Role::Use {
                    continue;
                }
                if let Some(&into) = sent.get(&operand.reg) {
                    operand.reg = into;
                }
            }
        }
        for call in func.succs_mut(block) {
            for arg in &mut call.args {
                if let Some(&into) = sent.get(arg) {
                    *arg = into;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_target::x86_64::{BITS, GPR, RDI};

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
        mir::Opcode::new(names.intern(&format!("{}{name}", BITS.prefix)))
    }

    /// The pass, over the machine this crate has a backend for.
    fn takes(func: &mut mir::Func, names: &Interner) -> usize {
        dead(func, &BITS, names)
    }

    /// What every instruction in a block came to, as opcodes.
    fn shape(func: &mir::Func, names: &Interner, block: mir::Block) -> Vec<String> {
        func.insts(block).map(|inst| names.resolve(func[inst].opcode.name()).to_owned()).collect()
    }

    /// The registers one instruction reads, in the order its operands hold them.
    fn reads(func: &mir::Func, inst: mir::Inst) -> Vec<mir::Reg> {
        func[func[inst].operands]
            .iter()
            .filter(|operand| operand.role == Role::Use)
            .map(|operand| operand.reg)
            .collect()
    }

    /// The shape the whole pass is about, and the one the corpus is full of: a byte widened to a
    /// word because C says to, and then the word written back out as a byte. The twenty four bits
    /// in between are worked out and read by nobody.
    #[test]
    fn a_widening_whose_only_reader_is_as_narrow_as_its_source_goes() {
        let (mut names, mut func, block) = empty();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_32");
        let store = op(&mut names, "mov_mr_8");
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();
        func.build(block, store)
            .uses(wide, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(takes(&mut func, &names), 1);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 1, "the widening is still there: {left:?}");
        let inst = func.insts(block).next().expect("the store is still there");
        assert_eq!(reads(&func, inst)[0], byte, "the store was not sent to the source");
    }

    /// The same widening with a reader that reads the whole of what it wrote. Those upper bits are
    /// read, so the instruction that worked them out is one doing its job.
    #[test]
    fn a_widening_something_reads_the_whole_of_stays() {
        let (mut names, mut func, block) = empty();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let out = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_32");
        let copy = op(&mut names, "mov_rr_64");
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();
        func.build(block, copy).def(out, GPR).uses(wide, GPR).finish();

        assert_eq!(takes(&mut func, &names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// The case no rewrite rule can reach, which is the reason this pass is here at all. The
    /// widening is in one block and the only thing that reads it is in another, so the two are
    /// never operands of one term and no pattern three levels deep sees them both.
    #[test]
    fn a_widening_whose_narrow_reader_is_in_another_block_goes_too() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let arrived = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_32");
        let store = op(&mut names, "mov_mr_8");
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();
        func.params_mut(next).push(mir::Param { reg: arrived, class: GPR });
        *func.succs_mut(block) = vec![mir::BlockCall::with(next, vec![wide])];
        func.build(next, store)
            .uses(arrived, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(takes(&mut func, &names), 1);

        assert!(shape(&func, &names, block).is_empty(), "the widening is still there");
        assert_eq!(func[block].succs[0].args, vec![byte], "the edge still carries the wide one");
    }

    /// And the same edge with a reader on the other side that wants the whole word, which is the
    /// answer coming back across the boundary the other way.
    #[test]
    fn a_widening_whose_reader_in_another_block_is_wide_stays() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let arrived = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_32");
        let store = op(&mut names, "mov_mr_32");
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();
        func.params_mut(next).push(mir::Param { reg: arrived, class: GPR });
        *func.succs_mut(block) = vec![mir::BlockCall::with(next, vec![wide])];
        func.build(next, store)
            .uses(arrived, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(takes(&mut func, &names), 0);
        assert_eq!(shape(&func, &names, block).len(), 1);
        assert_eq!(func[block].succs[0].args, vec![wide]);
    }

    /// A chain, which is what a narrow value widened for one operation and narrowed for the next
    /// comes out as. The middle conversion is what makes the analysis a fixpoint rather than one
    /// walk: how much of it is read depends on how much of the one after it is, and that number is
    /// still being worked out when it is asked for.
    #[test]
    fn a_chain_of_conversions_goes_the_whole_way_and_its_reader_goes_to_the_first_source() {
        let (mut names, mut func, block) = empty();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let narrowed = func.new_vreg(GPR);
        let out = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_64");
        let low = op(&mut names, "low_32");
        let narrow = op(&mut names, "low_8");
        let store = op(&mut names, "mov_mr_8");
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();
        func.build(block, low).def(narrowed, GPR).uses(wide, GPR).finish();
        func.build(block, narrow).def(out, GPR).uses(narrowed, GPR).finish();
        func.build(block, store)
            .uses(out, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(takes(&mut func, &names), 3);

        let left = shape(&func, &names, block);
        assert_eq!(left.len(), 1, "some of the three are still there: {left:?}");
        let inst = func.insts(block).next().expect("the store is still there");
        assert_eq!(reads(&func, inst)[0], byte, "the chain was not followed to its end");
    }

    /// A store of the whole word, which is section 37.7's warning: the bits go to memory and
    /// something reads them from there, so a pass that thought a store read less than it stores
    /// would take out a widening whose answer is in the program's output.
    #[test]
    fn a_store_reads_every_bit_of_what_it_stores() {
        let (mut names, mut func, block) = empty();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_64");
        let store = op(&mut names, "mov_mr_64");
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();
        func.build(block, store)
            .uses(wide, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(takes(&mut func, &names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// A widening whose result is read as an address. The registers a memory operand is made of
    /// are read whole and the description says nothing about their width, so the answer is that
    /// everything is read rather than that nothing is.
    #[test]
    fn a_widening_read_as_an_address_stays() {
        let (mut names, mut func, block) = empty();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let out = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_64");
        let load = op(&mut names, "mov_rm_32");
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();
        func.build(block, load)
            .def(out, GPR)
            .mem(mir::Mem::at(mir::Operand::read(wide, GPR)))
            .finish();

        assert_eq!(takes(&mut func, &names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// The operand an instruction of this shape reads and writes in the one place, which the
    /// assembly names once and the description therefore has no separate width for. It is as wide
    /// as the definition it is tied to, and an eight bit source is not enough for it.
    #[test]
    fn a_tied_operand_reads_as_much_as_the_definition_it_is_tied_to() {
        let (mut names, mut func, block) = empty();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_32");
        let add = op(&mut names, "add_rr_32");
        let store = op(&mut names, "mov_mr_32");
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();
        func.build(block, add)
            .operand(mir::Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(wide, GPR)
            .uses(other, GPR)
            .finish();
        func.build(block, store)
            .uses(sum, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(takes(&mut func, &names), 0);
        assert_eq!(shape(&func, &names, block).len(), 3);
    }

    /// A physical register as the source. Sending the readers there would send them to a register
    /// the convention hands out and a call is free to destroy, which is not what SSA promises
    /// about the virtual one they were reading.
    #[test]
    fn a_widening_of_a_physical_register_stays() {
        let (mut names, mut func, block) = empty();
        let arrived = mir::Reg::physical(RDI);
        let wide = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_32");
        let store = op(&mut names, "mov_mr_8");
        func.build(block, widen).def(wide, GPR).uses(arrived, GPR).finish();
        func.build(block, store)
            .uses(wide, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        assert_eq!(takes(&mut func, &names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// An opcode from somewhere other than this target, which is what an instruction with no
    /// prefix on it is. Nothing is known about how much of its operands it reads, and the answer
    /// to knowing nothing is that it reads everything.
    #[test]
    fn an_opcode_this_target_does_not_describe_reads_everything() {
        let (mut names, mut func, block) = empty();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let out = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_32");
        let foreign = mir::Opcode::new(names.intern("elsewhere.narrow"));
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();
        func.build(block, foreign).def(out, GPR).uses(wide, GPR).finish();

        assert_eq!(takes(&mut func, &names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// A conversion nothing reads at all, which is dead code rather than dead bits. It is left for
    /// whatever removes instructions whose answers nobody wants, so that the number this gives
    /// back is the number of widenings it found and not a count of two different things.
    #[test]
    fn a_conversion_nothing_reads_is_left_for_the_pass_that_owns_dead_code() {
        let (mut names, mut func, block) = empty();
        let byte = func.new_vreg(GPR);
        let wide = func.new_vreg(GPR);
        let widen = op(&mut names, "movzx_8_32");
        func.build(block, widen).def(wide, GPR).uses(byte, GPR).finish();

        assert_eq!(takes(&mut func, &names), 0);
        assert_eq!(shape(&func, &names, block).len(), 1);
    }
}
