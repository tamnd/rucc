//! A value the allocator sent to the stack, given a register again in a block that reads it more
//! than once.
//!
//! The allocator places a value for the whole of its life or not at all. A value that lives across
//! a loop where every register is wanted loses its register for the loop, and with it the register
//! it had everywhere else, so a block that reads it ten times in a row with half the registers idle
//! reads it from the frame ten times. `super_90_sync` in drivers/md/md.c is the case that showed
//! it: the device and the superblock pointer were both read back into `esi` before every store in
//! the first block, one after the other, because the loop over the member disks further down had
//! more values than i386 has registers.
//!
//! So every block that reads such a value twice or more gets a copy of it of its own, made in
//! front of the first of those reads, and the reads in the block read the copy. The copy is a new
//! value with a short life, and the allocator is asked again. Where the block has a register to
//! spare the copy gets it, and the reads after the first come out of that register. The value
//! itself still lives in its slot, which is where the copy is read from.
//!
//! # When it is kept
//!
//! Only when it costs less. The second answer is weighed by [`crate::backtrack::cost`] against the
//! first, and when it does not come out cheaper every copy is taken out again and the function is
//! the one the first answer was for. The copy is written as a two address move, which is what makes
//! the allocator try to put the copy where the value is, and the weighing counts that as a move the
//! copy would not have made where the value is in its slot. Each of those is taken back off before
//! the two answers are compared, since the load of the slot is the whole of what such a copy costs.

use rucc_base::hash::Map;
use rucc_mir::{Block, Constraint, Func, Inst, Operand, Reg, Role};

use crate::assign::{Assignment, Place};

/// Builds the move that writes the first operand with what the second holds, in no block yet, or
/// says the target has none for their class.
pub type Copy<'a> = &'a dyn Fn(&mut Func, Operand, Operand) -> Option<Inst>;

/// What one copy changed, so that it can be taken out again.
#[derive(Debug)]
struct Piece {
    copy: Inst,
    block: Block,
    value: Reg,
    into: Reg,
    /// Each operand that read the value and now reads the copy, by instruction and position.
    renamed: Vec<(Inst, usize)>,
}

/// The copies [`split`] made.
#[derive(Debug, Default)]
pub(crate) struct Pieces {
    pieces: Vec<Piece>,
    vregs: usize,
}

impl Pieces {
    /// Whether nothing was split.
    pub(crate) fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// What the weighing counts for these copies that no instruction pays: a copy whose value is
    /// in its slot and which got a register is the load of the slot, and the two address move is
    /// counted on top of that as a copy between the two.
    pub(crate) fn overcounted(&self, func: &Func, assignment: &Assignment) -> u128 {
        let mut total = 0;
        for piece in &self.pieces {
            let slot = matches!(assignment.place(piece.value), Some(Place::Slot(_)));
            let held = matches!(assignment.place(piece.into), Some(Place::Reg(_)));
            if slot && held {
                total += u128::from(func[piece.block].weight.raw().max(1));
            }
        }
        total
    }

    /// Takes every copy out and gives the reads their value back, which is the function as it was.
    pub(crate) fn undo(self, func: &mut Func) {
        for piece in self.pieces {
            for (inst, at) in piece.renamed {
                let list = func[inst].operands;
                func[list][at].reg = piece.value;
            }
            func.remove_inst(piece.copy);
        }
        func.forget_vregs(self.vregs);
    }
}

/// Gives every block that reads a value on the stack at least twice a copy of it of its own.
pub(crate) fn split(
    func: &mut Func,
    assignment: &Assignment,
    forced: &[Reg],
    copy: Copy<'_>,
) -> Pieces {
    let vregs = func.vregs();
    let mut pieces = Pieces { pieces: Vec::new(), vregs };
    let spilled = |reg: Reg| {
        reg.is_virtual()
            && matches!(assignment.place(reg), Some(Place::Slot(_)))
            && !forced.contains(&reg)
    };
    // A value written more than once is left alone, since a copy made in front of the first read
    // in a block is only the value every read in the block sees when nothing writes it again. So is
    // one read nowhere but in the block that writes it, since the copy would take every read and be
    // the value again under another name, with a move it did not have.
    let mut writes: Map<Reg, (u32, Block)> = Map::default();
    let mut blocks: Map<Reg, (u32, Block)> = Map::default();
    let mut reads: Vec<(Block, Reg, Vec<(Inst, usize)>)> = Vec::new();
    let mut at: Map<(Block, Reg), usize> = Map::default();
    for block in func.blocks() {
        for inst in func.insts(block) {
            for (index, operand) in func[func[inst].operands].iter().enumerate() {
                if !spilled(operand.reg) {
                    continue;
                }
                if operand.role != Role::Use {
                    writes.entry(operand.reg).or_insert((0, block)).0 += 1;
                    continue;
                }
                let entry = *at.entry((block, operand.reg)).or_insert_with(|| {
                    blocks.entry(operand.reg).or_insert((0, block)).0 += 1;
                    reads.push((block, operand.reg, Vec::new()));
                    reads.len() - 1
                });
                reads[entry].2.push((inst, index));
            }
        }
    }
    for (block, value, renamed) in reads {
        let mut readers: Vec<Inst> = renamed.iter().map(|&(inst, _)| inst).collect();
        readers.dedup();
        let written = writes.get(&value).copied();
        if readers.len() < 2 || written.is_some_and(|(count, _)| count > 1) {
            continue;
        }
        let alone = blocks.get(&value).is_some_and(|&(count, _)| count == 1);
        if alone && written.is_some_and(|(_, at)| at == block) {
            continue;
        }
        let Some(class) = func.class_of(value) else { continue };
        let into = func.new_vreg(class);
        let made = copy(
            func,
            Operand::write(into, class).with(Constraint::Reuse(1)),
            Operand::read(value, class),
        );
        let Some(made) = made else {
            func.forget_vregs(func.vregs() - 1);
            continue;
        };
        if let Some(width) = func.width(value) {
            func.set_width(into, u32::from(width));
        }
        let bars: Vec<_> =
            func.bars().iter().filter(|&&(reg, _)| reg == value).map(|&(_, at)| at).collect();
        for at in bars {
            func.bar(into, at);
        }
        func.insert_before(readers[0], made);
        for &(inst, index) in &renamed {
            let list = func[inst].operands;
            func[list][index].reg = into;
        }
        pieces.pieces.push(Piece { copy: made, block, value, into, renamed });
    }
    pieces
}
