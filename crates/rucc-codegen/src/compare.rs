//! Taking out a comparison the machine has already made.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` section 37.4.
//!
//! A comparison produces no value. It sets a few bits nobody named and the instruction behind it
//! reads them, so a comparison that sets the bits that are already there is one nothing could tell
//! had run. There are two ways for that to happen, and both of them are about an instruction a
//! little way in front rather than about a dataflow the whole function takes part in.
//!
//! The same comparison twice. `if (x == y) ... else if (x != y)` and every expression that asks a
//! question and then asks its negation come out as two comparisons of the same two registers with
//! nothing between them but the bytes each one kept. The second asks what the first asked and the
//! answer has not moved.
//!
//! A comparison against zero of something arithmetic has just worked out. `if (a & MASK)` is an
//! `and` and then a comparison of its result against zero, and the `and` set the bits that
//! comparison would have set on its way past. This is the common one by a long way: at `-O2` over
//! the SQLite amalgamation there are 2250 of these and 0 of the other shape.
//!
//! # Why it runs after the layout rather than before
//!
//! Because this is the second pass to work on a pair of instructions whose middle has to stay
//! empty, and the first is the block layout. A branch on a comparison is written there as the
//! comparison with its byte taken off and a jump that reads the condition state, and what is
//! between those two is live and is not a register, so anything that ran afterwards and put an
//! instruction between them would be wrong. Running last is the whole of what makes this safe,
//! which is the sentence section 37.4 uses about the layout itself.
//!
//! It also makes the two shapes one shape. A comparison the layout folded a branch into is a
//! comparison that keeps nothing, one whose byte something else wanted is a comparison that keeps
//! a byte, and after the layout both are sitting in a block to be looked at the same way. Before
//! the layout the first kind does not exist yet, so a pass that ran earlier would have to either
//! leave every branch alone or undo the fusion to get at one.
//!
//! # What a block boundary is
//!
//! The end of everything this knows. The state a comparison leaves is not a register and nothing
//! in this back end carries one from a block to its successors: the layout writes the jump that
//! reads a comparison into the same block as the comparison, which is the only place one is read
//! at all. So the walk starts each block knowing nothing, which is what makes it a walk rather
//! than a dataflow.
//!
//! # What it will not do
//!
//! A comparison with anything between it and the instruction that already made it that writes the
//! condition state. The target says which instructions those are and says it about every name it
//! does not recognise, so an opcode added to a rule set and not to that description makes this
//! find less rather than making it wrong.
//!
//! A comparison of a register something wrote in between. The bits are still the bits the earlier
//! instruction left, but they are about what the register held then and the comparison is about
//! what it holds now. Every definition between the two is checked against the registers the
//! earlier one was about, which are physical by the time this runs and so are the ones the machine
//! will really read.
//!
//! A comparison against zero after arithmetic whose condition reads a part of the condition state
//! the arithmetic did not leave the way a comparison would have. `subl` says whether its answer
//! was zero and a comparison of that answer against zero would agree, and it says whether the
//! subtraction overflowed where the comparison would have said it did not, so a signed `<` after
//! one reads a sign and an overflow that no longer belong together. [`rucc_target::Zeroing`] is
//! where each instruction says which conditions it is good for, and every condition that ends up
//! reading what the arithmetic left has to be one of them, including the ones behind the
//! comparison rather than on it.
//!
//! A comparison against zero after arithmetic that wrote a different number of bits. `andl` leaves
//! a statement about thirty two bits and `cmpq $0` asks about sixty four, and on this machine the
//! upper half is then zero and the two disagree about the sign.
//!
//! A comparison against zero after arithmetic whose condition state nothing is found to read. That
//! is a comparison that is dead rather than redundant, and taking a dead one out is a different
//! question: it needs no earlier instruction at all, so answering it here would mean answering it
//! only where an earlier instruction happened to be.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_mir::{self as mir, Role};
use rucc_target::{Compare, FlagInsts, Reads, RegClass, Zeroing};

/// A register, and the file it is drawn from.
///
/// The class as well as the number, because the two files number from zero and `xmm0` is not
/// `rax`. The width is deliberately not here: `%al` and `%eax` are one register, so a write of
/// either is a write of the other and a statement about what the other held is a statement about
/// a value that has moved.
type Place = (RegClass, mir::Reg);

/// Takes out every comparison whose condition state the instruction in front of it already left.
///
/// Gives back how many went, which the tests read and nothing else does.
pub fn redundant(func: &mut mir::Func, insts: &FlagInsts, names: &mut Interner) -> usize {
    // Every name the rewrite could want, before the walk rather than inside it. The walk holds a
    // name it read out of the interner while it edits the function, and interning a new one there
    // would be the same interner borrowed twice.
    let opcodes: HashMap<&str, mir::Opcode> = insts
        .compares
        .iter()
        .filter_map(|entry| entry.kept)
        .map(|kept| (kept, mir::Opcode::new(names.intern(&format!("{}{kept}", insts.prefix)))))
        .collect();
    let names = &*names;
    let mut gone = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        let sequence: Vec<mir::Inst> = func.insts(block).collect();
        let mut left: Option<Left> = None;
        for at in 0..sequence.len() {
            let inst = sequence[at];
            let Some(name) = opcode(func, insts, names, inst) else {
                left = None;
                continue;
            };
            left = if let Some(entry) = insts.compare(name) {
                let already = left
                    .as_ref()
                    .is_some_and(|had| had.answers(func, insts, names, &sequence, at, entry));
                if already {
                    // What is left of the instruction is asked before the rewrite rather than
                    // after, because one of the two answers is that there is nothing left of it.
                    let after = stale(func, inst, left);
                    take(func, &opcodes, inst, entry);
                    gone += 1;
                    after
                } else {
                    stale(func, inst, Some(Left::made(func, entry, inst)))
                }
            } else if let Some(zeroing) = insts.zeroed(name) {
                // Before the general question of whether the name writes the condition state,
                // because every one of these does and this is what it wrote there.
                Left::zeroed(func, insts, name, zeroing, inst)
            } else if (insts.writes)(name) {
                None
            } else {
                stale(func, inst, left)
            };
        }
    }
    gone
}

/// What the condition state holds, and which registers it is a statement about.
struct Left {
    /// Which of the two ways it got there.
    how: How,
    /// The registers the statement is about, which anything writing one of makes it stale.
    about: Vec<Place>,
}

/// The two ways the condition state comes to hold something this pass can use.
enum How {
    /// A comparison made it, and this is the question it asked.
    Made {
        /// The name of the comparison that keeps nothing, which is what says two are the same.
        asks: &'static str,
        /// What it compared, in the order it read them.
        read: Vec<Place>,
        /// The constant it compared against, if it compared against one.
        imm: Option<i64>,
    },
    /// Arithmetic left it, and this is what a comparison against zero has to look like to be one
    /// the arithmetic already made.
    Zeroed {
        /// How wide the value it wrote is.
        width: u32,
        /// Which conditions may read what it left.
        covers: Zeroing,
    },
}

impl Left {
    /// What a comparison leaves behind.
    fn made(func: &mir::Func, entry: &Compare, inst: mir::Inst) -> Self {
        let read: Vec<Place> = reads(func, inst).into_iter().map(|(_, place)| place).collect();
        Self {
            how: How::Made {
                asks: entry.asks,
                read: read.clone(),
                imm: func[inst].imm.map(|at| func[at].0),
            },
            about: read,
        }
    }

    /// What arithmetic leaves behind, when what it wrote is one register of a width the
    /// description names.
    ///
    /// The statement is about the register it wrote rather than about the ones it read, which is
    /// what makes its own definition not something that makes it stale: what it wrote is the value
    /// the comparison it stands in for is about.
    fn zeroed(
        func: &mir::Func,
        insts: &FlagInsts,
        name: &str,
        zeroing: &Zeroing,
        inst: mir::Inst,
    ) -> Option<Self> {
        let written = writes(func, inst);
        let [(at, def)] = written[..] else { return None };
        let width = (insts.width)(name, at)?;
        Some(Self { how: How::Zeroed { width, covers: *zeroing }, about: vec![def] })
    }

    /// Whether this comparison is one the condition state already answers.
    fn answers(
        &self,
        func: &mir::Func,
        insts: &FlagInsts,
        names: &Interner,
        sequence: &[mir::Inst],
        at: usize,
        entry: &Compare,
    ) -> bool {
        let inst = sequence[at];
        let asked: Vec<Place> = reads(func, inst).into_iter().map(|(_, place)| place).collect();
        let against = func[inst].imm.map(|at| func[at].0);
        match &self.how {
            // The same question about the same values, so every bit of the answer is the bit that
            // is already there and what reads it is not something anyone has to ask.
            How::Made { asks, read, imm } => {
                *asks == entry.asks && *read == asked && *imm == against
            }
            How::Zeroed { width, covers } => {
                if against != Some(0) || self.about != asked {
                    return false;
                }
                let [(index, _)] = reads(func, inst)[..] else { return false };
                let Some(name) = opcode(func, insts, names, inst) else { return false };
                if (insts.width)(name, index) != Some(*width) {
                    return false;
                }
                let conditions = conditions(func, insts, names, sequence, at);
                !conditions.is_empty() && conditions.iter().all(|&reads| covers.covers(reads))
            }
        }
    }
}

/// The conditions that read what an instruction leaves in the condition state.
///
/// Its own first, which is where a comparison that keeps a byte carries the condition it is about,
/// and then the ones behind it as far as whatever writes the condition state next. Both halves
/// matter and for one reason: the rewrite leaves the readers where they are and takes the
/// comparison out from under them, so each of them ends up reading what the instruction further
/// back left instead.
fn conditions(
    func: &mir::Func,
    insts: &FlagInsts,
    names: &Interner,
    sequence: &[mir::Inst],
    at: usize,
) -> Vec<Reads> {
    let mut found = Vec::new();
    let Some(name) = opcode(func, insts, names, sequence[at]) else { return found };
    found.extend(insts.reads(name));
    for &inst in &sequence[at + 1..] {
        let Some(name) = opcode(func, insts, names, inst) else { break };
        if (insts.writes)(name) {
            break;
        }
        found.extend(insts.reads(name));
    }
    found
}

/// The same state, unless the instruction wrote a register it was a statement about.
fn stale(func: &mir::Func, inst: mir::Inst, left: Option<Left>) -> Option<Left> {
    let left = left?;
    let touched = writes(func, inst).iter().any(|&(_, place)| left.about.contains(&place));
    (!touched).then_some(left)
}

/// The registers an instruction reads, each with the index it reads it at.
fn reads(func: &mir::Func, inst: mir::Inst) -> Vec<(u8, Place)> {
    picked(func, inst, Role::Use)
}

/// The registers an instruction writes, each with the index it writes it at.
fn writes(func: &mir::Func, inst: mir::Inst) -> Vec<(u8, Place)> {
    let mut found = picked(func, inst, Role::Def);
    found.extend(picked(func, inst, Role::EarlyDef));
    found
}

/// The operands in that role, each with the index it is at.
fn picked(func: &mir::Func, inst: mir::Inst, role: Role) -> Vec<(u8, Place)> {
    func[func[inst].operands]
        .iter()
        .enumerate()
        .filter(|(_, operand)| operand.role == role)
        .filter_map(|(at, operand)| Some((u8::try_from(at).ok()?, (operand.class, operand.reg))))
        .collect()
}

/// Turns a comparison into what is left of it, which is a byte or nothing at all.
///
/// The byte keeps the register it was going to and the constant goes, because what the constant
/// was for was the comparison and the comparison is the part that is not happening. Nothing else
/// about the instruction moves, which is what keeps this a rewrite of one instruction rather than
/// a rewrite of the block around it.
fn take(
    func: &mut mir::Func,
    opcodes: &HashMap<&str, mir::Opcode>,
    inst: mir::Inst,
    entry: &Compare,
) {
    let Some(kept) = entry.kept else {
        func.remove_inst(inst);
        return;
    };
    let Some(&opcode) = opcodes.get(kept) else { return };
    let byte: Vec<mir::Operand> = func[func[inst].operands]
        .iter()
        .filter(|operand| operand.role != Role::Use)
        .copied()
        .collect();
    let operands = func.push_operands(&byte);
    func[inst].opcode = opcode;
    func[inst].operands = operands;
    func[inst].imm = None;
}

/// The name this target knows an instruction by, for an instruction that is one of this target's.
///
/// The opcode in machine IR carries the target's prefix, because a function in the middle of being
/// compiled holds instructions of one machine and the prefix is what says which. Anything without
/// it is not something this description covers, and the rest of the pass treats that as knowing
/// nothing rather than as knowing it is safe.
fn opcode<'a>(
    func: &mir::Func,
    insts: &FlagInsts,
    names: &'a Interner,
    inst: mir::Inst,
) -> Option<&'a str> {
    names.resolve(func[inst].opcode.name()).strip_prefix(insts.prefix)
}

#[cfg(test)]
mod tests {
    use rucc_target::x86_64::{FLAGS, GPR};

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
        mir::Opcode::new(names.intern(&format!("{}{name}", FLAGS.prefix)))
    }

    /// The pass, over the machine this crate has a backend for.
    fn takes(func: &mut mir::Func, names: &mut Interner) -> usize {
        redundant(func, &FLAGS, names)
    }

    /// What every instruction in a block came to, as opcodes with the target's prefix taken off.
    fn shape(func: &mir::Func, names: &Interner, block: mir::Block) -> Vec<String> {
        func.insts(block)
            .map(|inst| {
                names
                    .resolve(func[inst].opcode.name())
                    .strip_prefix(FLAGS.prefix)
                    .unwrap_or("")
                    .to_owned()
            })
            .collect()
    }

    /// The shape the issue is named after: the same comparison made twice with nothing between the
    /// two but the byte the first one kept. The second asks what the first asked, so what is left
    /// of it is the byte alone.
    #[test]
    fn the_same_comparison_twice_leaves_one_comparison_and_two_bytes() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let ne = op(&mut names, "cmp_set_ne_ri_32");
        let e = op(&mut names, "cmp_set_e_ri_32");
        func.build(block, ne).def(first, GPR).uses(value, GPR).imm(0).finish();
        func.build(block, e).def(second, GPR).uses(value, GPR).imm(0).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["cmp_set_ne_ri_32", "set_e"]);
    }

    /// The same two comparisons with something writing the compared register in between. The bits
    /// are the bits the first one left and they are about a value that has moved on.
    #[test]
    fn a_comparison_of_a_register_something_wrote_in_between_stays() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let ne = op(&mut names, "cmp_set_ne_ri_32");
        let e = op(&mut names, "cmp_set_e_ri_32");
        let copy = op(&mut names, "mov_rr_64");
        func.build(block, ne).def(first, GPR).uses(value, GPR).imm(0).finish();
        func.build(block, copy).def(value, GPR).uses(other, GPR).finish();
        func.build(block, e).def(second, GPR).uses(value, GPR).imm(0).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 3);
    }

    /// The common shape, which is `if (a & MASK)`. The `and` clears the carry and the overflow and
    /// sets the zero and the sign from what it wrote, which is every bit the comparison would have
    /// set and the same values, so every condition may read it.
    #[test]
    fn a_comparison_against_zero_after_a_bitwise_operation_goes() {
        for condition in ["e", "l", "b"] {
            let (mut names, mut func, block) = empty();
            let value = func.new_vreg(GPR);
            let byte = func.new_vreg(GPR);
            let and = op(&mut names, "and_ri_32");
            let cmp = op(&mut names, &format!("cmp_set_{condition}_ri_32"));
            func.build(block, and).def(value, GPR).uses(value, GPR).imm(255).finish();
            func.build(block, cmp).def(byte, GPR).uses(value, GPR).imm(0).finish();

            assert_eq!(takes(&mut func, &mut names), 1, "set{condition}");
            assert_eq!(
                shape(&func, &names, block),
                ["and_ri_32".to_owned(), format!("set_{condition}")]
            );
        }
    }

    /// The same after a subtraction, which is the one that is only half true. The zero bit is what
    /// a comparison of the answer against zero would have set it to, and the overflow is not, so
    /// the conditions built out of the sign and the overflow together have to stay.
    #[test]
    fn a_comparison_against_zero_after_a_subtraction_goes_only_for_the_zero_conditions() {
        for (condition, left) in [("e", 1), ("ne", 1), ("l", 0), ("ge", 0), ("a", 0)] {
            let (mut names, mut func, block) = empty();
            let value = func.new_vreg(GPR);
            let other = func.new_vreg(GPR);
            let byte = func.new_vreg(GPR);
            let sub = op(&mut names, "sub_rr_32");
            let cmp = op(&mut names, &format!("cmp_set_{condition}_ri_32"));
            func.build(block, sub).def(value, GPR).uses(value, GPR).uses(other, GPR).finish();
            func.build(block, cmp).def(byte, GPR).uses(value, GPR).imm(0).finish();

            assert_eq!(takes(&mut func, &mut names), left, "set{condition}");
        }
    }

    /// A comparison the layout already folded a branch into, which keeps no byte at all. There is
    /// nothing left of one of those, and the jump behind it reads what the `and` left.
    #[test]
    fn a_comparison_that_keeps_nothing_is_taken_out_and_the_jump_reads_what_is_there() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let and = op(&mut names, "and_ri_32");
        let cmp = op(&mut names, "cmp_ri_32");
        let jump = op(&mut names, "jcc_l");
        func.build(block, and).def(value, GPR).uses(value, GPR).imm(255).finish();
        func.build(block, cmp).uses(value, GPR).imm(0).finish();
        func.build(block, jump).finish();

        assert_eq!(takes(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["and_ri_32", "jcc_l"]);
    }

    /// The same three instructions with a subtraction in front. The condition is behind the
    /// comparison rather than on it, so finding it means looking at what reads what the comparison
    /// would have left, and a signed `<` is not something a subtraction answers.
    #[test]
    fn a_comparison_that_keeps_nothing_is_refused_on_the_condition_behind_it() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let sub = op(&mut names, "sub_rr_32");
        let cmp = op(&mut names, "cmp_ri_32");
        let jump = op(&mut names, "jcc_l");
        func.build(block, sub).def(value, GPR).uses(value, GPR).uses(other, GPR).finish();
        func.build(block, cmp).uses(value, GPR).imm(0).finish();
        func.build(block, jump).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 3);
    }

    /// Arithmetic that wrote half of what the comparison is asking about. The upper half is zero
    /// because this machine writes it that way, so the two agree about whether the value is zero
    /// and disagree about its sign, and the description has no way to say half of one condition.
    #[test]
    fn a_comparison_wider_than_the_arithmetic_in_front_of_it_stays() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let and = op(&mut names, "and_ri_32");
        let cmp = op(&mut names, "cmp_set_e_ri_64");
        func.build(block, and).def(value, GPR).uses(value, GPR).imm(255).finish();
        func.build(block, cmp).def(byte, GPR).uses(value, GPR).imm(0).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// Something between the two that writes the condition state. A multiply is not in the
    /// description's list because this machine leaves the zero bit undefined after one, so what it
    /// left is not something to read and not something to reason from either.
    #[test]
    fn anything_that_writes_the_condition_state_in_between_makes_the_comparison_stay() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let and = op(&mut names, "and_ri_32");
        let mul = op(&mut names, "imul_rr_32");
        let cmp = op(&mut names, "cmp_set_e_ri_32");
        func.build(block, and).def(value, GPR).uses(value, GPR).imm(255).finish();
        func.build(block, mul).def(other, GPR).uses(other, GPR).uses(other, GPR).finish();
        func.build(block, cmp).def(byte, GPR).uses(value, GPR).imm(0).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 3);
    }

    /// A comparison that keeps nothing and whose condition state nothing is found to read. It is
    /// dead rather than redundant, and this pass is not the one that answers that.
    #[test]
    fn a_comparison_nothing_is_found_to_read_stays() {
        let (mut names, mut func, block) = empty();
        let value = func.new_vreg(GPR);
        let and = op(&mut names, "and_ri_32");
        let cmp = op(&mut names, "cmp_ri_32");
        func.build(block, and).def(value, GPR).uses(value, GPR).imm(255).finish();
        func.build(block, cmp).uses(value, GPR).imm(0).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// The condition state does not cross a block boundary, and neither does this.
    #[test]
    fn a_comparison_in_another_block_is_not_one_the_arithmetic_answers() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let value = func.new_vreg(GPR);
        let byte = func.new_vreg(GPR);
        let and = op(&mut names, "and_ri_32");
        let cmp = op(&mut names, "cmp_set_e_ri_32");
        func.build(block, and).def(value, GPR).uses(value, GPR).imm(255).finish();
        *func.succs_mut(block) = vec![mir::BlockCall::to(next)];
        func.build(next, cmp).def(byte, GPR).uses(value, GPR).imm(0).finish();

        assert_eq!(takes(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, next).len(), 1);
    }
}
