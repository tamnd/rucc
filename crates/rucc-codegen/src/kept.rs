//! Where each local the program kept in a value ended up, and over which instructions.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! Selection says which declaration each virtual register holds a value of, and the allocator says
//! where each virtual register went. Putting the two together is all this is, and the only thing
//! that makes it more than a join is the stretch: a frame slot belongs to its local for as long as
//! the frame exists, and a register is handed to the next value the moment this one is done with,
//! so where a register holds a local is a question about part of a function rather than about the
//! whole of it. The allocator's own liveness is the answer, read rather than worked out again for
//! the reason `crate::slots` gives for reading it: two answers about one function are free to
//! disagree, and the one the machine runs is the allocator's.
//!
//! A stretch runs from the instruction after the one that wrote the value to the last instruction
//! that reads it, both ends included, and it stops at the end of the block either way. The front is
//! one instruction along because a register does not hold a value until the instruction writing it
//! has run, and the back is where it is because nothing reads the value afterwards, so whatever the
//! allocator puts in the register next cannot be seen by anybody asking. A value nothing reads at
//! all gets no stretch, which is the same sentence read the other way: the two ends cross.
//!
//! The block is where it stops because the pass that lays the blocks out runs after the allocator
//! and can put them in any order it likes. Inside a block nothing has moved, so a run of
//! instructions there is a run of addresses to come, and a value live from one block into the next
//! gets a stretch in each of them rather than one stretch that would cover whatever the layout
//! happened to put in between.
//!
//! # A block the scheduler reordered
//!
//! The liveness is counted along the order the allocator laid the function out in, and the
//! scheduler runs after it and moves instructions about inside a block, so in a block it touched
//! a run of points is no longer a run of instructions. What is still true there is what the
//! scheduler has to keep true for the program to mean the same thing: it runs after the registers
//! are handed out, so every write of a register stays in order with every read of it and every
//! other write of it, and every access to memory stays in the order it was in. So a value is in
//! its register from the instruction after the one that wrote it to the last one that reads it,
//! wherever the schedule put those two, and a local in the frame is in its bytes from the first
//! touch of them to the last. A stretch in such a block is found by where its two ends went rather
//! than by a search along the points.
//!
//! That needs both ends to be an instruction the liveness knows, or the edge of the block for a
//! value live into it or out of it. A piece that starts or ends at a spill, a reload or an edge
//! move has an end that is neither, and it gets no stretch in that block rather than a guess.
//!
//! # A declaration that took a value part of the way through
//!
//! `int m = a;` computes nothing, so `m` is handed a value `a` already holds, and the value's live
//! range says nothing about where the assignment was. Selection says it instead, as the first
//! instruction after it in [`Func::starts`]. In that instruction's block the stretch starts no
//! earlier than it, and in any other block the declaration holds the value only where every path
//! from the entry goes through the assignment's block first, which is where it is sure to have
//! run. A block the other arm of a branch reaches as well is left out, since there the
//! declaration may never have been given the value at all.
//!
//! # What is left out
//!
//! A value the allocator spilled is in the frame over its stretch rather than in a register, which
//! is as much an answer as the other and is written the same way. A value it spilled in a function
//! whose alignment the prologue had to force has no answer, because the distance from the call
//! frame address is not a constant there, which is what `crate::frame` says about a local in the
//! same function.

use std::collections::HashMap;

use rucc_mir::{Block, Func, Inst, Kept, Reg, Where};
use rucc_regalloc::Allocation;
use rucc_regalloc::assign::Place;
use rucc_regalloc::live::Range;
use rucc_regalloc::order::Point;

use crate::frame::Frame;

/// Every instruction of a function, in the order they are in, which is what the allocator's
/// liveness is counted along while that is still the order.
///
/// Taken before the allocator rewrites the function, because afterwards the spills, the reloads
/// and the edge moves are in among them and none of those is an instruction the liveness knows a
/// point for.
#[must_use]
pub fn before(func: &Func) -> Vec<Inst> {
    func.blocks().flat_map(|block| func.insts(block)).collect()
}

/// Which declaration is where, over which instructions.
///
/// `framed` is the locals in the frame whose bytes they share with something else, as the
/// declaration, how far the bytes are from the call frame address and where the local is wanted.
/// Each of them is in the frame over that area and nowhere outside it, which is the same question
/// as a spilled value and gets the same answer.
#[must_use]
pub fn of(
    func: &Func,
    before: &[Inst],
    allocation: &Allocation,
    frame: &Frame,
    framed: &[(u32, i32, &[Range])],
) -> Vec<Kept> {
    if func.named.is_empty() && func.starts.is_empty() && framed.is_empty() {
        return Vec::new();
    }
    let line = line(func, before, allocation);
    let mut out = Vec::new();
    for &(decl, reg) in &func.named {
        let Some(at) = place(func, allocation, frame, reg) else { continue };
        let Some(area) = allocation.live.area(reg) else { continue };
        over(decl, at, area.pieces(), &line, &mut out);
    }
    for &(decl, at, area) in framed {
        over(decl, Where::Frame(at), area.iter().copied(), &line, &mut out);
    }
    // A declaration that took a value another one already held, from the instruction the
    // assignment became onward. An instruction something took out since is nowhere to start from.
    let mut under: HashMap<Block, Vec<bool>> = HashMap::new();
    for &(decl, reg, first) in &func.starts {
        let Some(block) = func.block_of(first) else { continue };
        let Some(at) = place(func, allocation, frame, reg) else { continue };
        let Some(area) = allocation.live.area(reg) else { continue };
        let dominated = under.entry(block).or_insert_with(|| dominated(func, block));
        for piece in area.pieces() {
            for run in &line {
                let stretch = if run.block == block {
                    run.stretch_from(piece, first)
                } else if dominated[run.block.index()] {
                    run.stretch(piece)
                } else {
                    None
                };
                if let Some((from, to)) = stretch {
                    out.push(Kept { decl, at, from, to });
                }
            }
        }
    }
    out
}

/// Where the allocator put a register, as a place a debugger can read, or `None` for a register
/// it put nowhere or in a slot this frame cannot name.
fn place(func: &Func, allocation: &Allocation, frame: &Frame, reg: Reg) -> Option<Where> {
    let class = func.class_of(reg)?;
    match allocation.assignment.place(reg)? {
        Place::Reg(reg) => Some(Where::Reg { reg, class }),
        Place::Slot(slot) => frame.slot_from_frame_base(slot).map(Where::Frame),
    }
}

/// Which blocks every path from the entry to them goes through `from` on, by block number.
///
/// A block is one of them when the entry reaches it and stops reaching it once `from` is taken
/// away, which is the definition read straight off rather than a dominator tree, since the question
/// is asked of a handful of blocks and a tree would answer it for all of them. `from` itself is
/// not, because what holds in it holds from part of the way through and is asked separately.
fn dominated(func: &Func, from: Block) -> Vec<bool> {
    let reach = |skip: Option<Block>| {
        let mut seen = vec![false; func.block_count()];
        let mut stack: Vec<Block> =
            func.entry().filter(|&entry| Some(entry) != skip).into_iter().collect();
        for &block in &stack {
            seen[block.index()] = true;
        }
        while let Some(block) = stack.pop() {
            for call in &func[block].succs {
                if Some(call.block) != skip && !seen[call.block.index()] {
                    seen[call.block.index()] = true;
                    stack.push(call.block);
                }
            }
        }
        seen
    };
    let all = reach(None);
    let around = reach(Some(from));
    all.iter()
        .zip(&around)
        .enumerate()
        .map(|(index, (&all, &around))| all && !around && index != from.index())
        .collect()
}

/// The stretches one declaration is in one place over, a piece of where it is wanted at a time.
fn over(
    decl: u32,
    at: Where,
    pieces: impl Iterator<Item = Range>,
    line: &[Run],
    out: &mut Vec<Kept>,
) {
    for piece in pieces {
        for run in line {
            if let Some((from, to)) = run.stretch(piece) {
                out.push(Kept { decl, at, from, to });
            }
        }
    }
}

/// One block's instructions the liveness knows a point for, in the order the block is in now.
struct Run {
    /// Which block it is.
    block: Block,
    /// Each instruction, with the point it reads its operands at and the one it writes at.
    insts: Vec<(Point, Point, Inst)>,
    /// Whether the points go up along the block, which is every block the scheduler left alone.
    sorted: bool,
    /// Where the block's parameters arrive, which is before everything in it, and where its
    /// outgoing arguments are read, which is after everything in it. `None` for a block made
    /// after the allocator ran, which has no points of its own.
    bounds: Option<(Point, Point)>,
}

impl Run {
    /// The first and the last instruction of this block a piece of a live range covers, or `None`
    /// for a piece that covers none of them or one this cannot say about.
    fn stretch(&self, piece: Range) -> Option<(Inst, Inst)> {
        let (lo, hi) = self.span(piece)?;
        Some((self.insts[lo].2, self.insts[hi].2))
    }

    /// The same stretch, starting no earlier than `first`, for a declaration that only holds the
    /// value from there on. `None` as well for a `first` the liveness has no point for.
    fn stretch_from(&self, piece: Range, first: Inst) -> Option<(Inst, Inst)> {
        let (lo, hi) = self.span(piece)?;
        let lo = lo.max(self.insts.iter().position(|&(_, _, inst)| inst == first)?);
        (lo <= hi).then(|| (self.insts[lo].2, self.insts[hi].2))
    }

    /// Where in [`Run::insts`] the first and the last instruction of a stretch are.
    fn span(&self, piece: Range) -> Option<(usize, usize)> {
        if self.sorted {
            // Strictly after where the value is written and up to and including where it is last
            // read. Both ends of a piece are points the value is live at, and the front one is the
            // instruction writing it, which is the one instruction in the piece the register does
            // not hold the value at the start of.
            let lo = self.insts.partition_point(|&(early, _, _)| early <= piece.start);
            let hi = self.insts.partition_point(|&(early, _, _)| early <= piece.end);
            return (lo < hi).then(|| (lo, hi - 1));
        }
        let (start, end) = self.bounds?;
        if piece.end < start || piece.start > end {
            return None;
        }
        // The same two ends, found by where the instructions at them went. See the module
        // documentation on a block the scheduler reordered.
        let at = |point: Point| {
            self.insts.iter().position(|&(early, late, _)| early == point || late == point)
        };
        let lo = if piece.start <= start { 0 } else { at(piece.start)? + 1 };
        let hi = if piece.end >= end { self.insts.len().checked_sub(1)? } else { at(piece.end)? };
        (lo <= hi).then_some((lo, hi))
    }
}

/// The instructions the function still has that the liveness knows a point for, one run per
/// block and each in the order that block is in now.
///
/// A block at a time rather than the whole function at once, because the pass that lays the blocks
/// out runs between the allocator and here and is free to put them in any order it likes. A block
/// it moved is still a block whose instructions are contiguous in the addresses to come, so the
/// question the liveness answers is still answerable about each of them on its own. What is not
/// answerable is a stretch that runs from one block into another, which is why a piece of a live
/// range turns into a stretch per block rather than into one stretch.
fn line(func: &Func, before: &[Inst], allocation: &Allocation) -> Vec<Run> {
    let order = &allocation.order;
    let mut known = vec![false; func.inst_count()];
    for &inst in before {
        known[inst.index()] = true;
    }
    let mut out = Vec::with_capacity(func.block_count());
    for block in func.blocks() {
        let insts: Vec<(Point, Point, Inst)> = func
            .insts(block)
            .filter(|inst| known[inst.index()])
            .map(|inst| (order.early(inst), order.late(inst), inst))
            .collect();
        if insts.is_empty() {
            continue;
        }
        let sorted = insts.windows(2).all(|pair| pair[0].0 < pair[1].0);
        out.push(Run { block, insts, sorted, bounds: order.bounds(block) });
    }
    out
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Func, Opcode, Reg};
    use rucc_regalloc::assign::Env;
    use rucc_target::x86_64::{GPR, REGS, SYSV};

    use super::*;
    use crate::frame::Layout;

    /// A function of three instructions: two that write a value and one that reads the first of
    /// them, with the declarations the caller asks for named against its registers.
    ///
    /// Three of them rather than two so that the stretch of the first value has an instruction in
    /// it either side of the one that wrote it, and the second value is one nothing reads.
    fn three(named: &[(u32, u32)]) -> (Func, Vec<Inst>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).finish();
        func.named = named.iter().map(|&(decl, reg)| (decl, Reg::virtual_reg(reg))).collect();
        let line = before(&func);
        (func, line)
    }

    /// That function allocated with enough registers to spill nothing, and what this says about it.
    fn about(func: &mut Func, line: &[Inst]) -> Vec<Kept> {
        let env = Env::new().with(GPR, &SYSV.int_order[..4], &SYSV.int_order[4..]);
        let allocation = rucc_regalloc::run(func, &env, "test", true);
        let frame = Frame::of(func, &allocation, &Layout::new(&SYSV, REGS));
        of(func, line, &allocation, &frame, &[])
    }

    #[test]
    fn a_register_holding_a_local_says_so_from_the_instruction_after_the_one_that_wrote_it() {
        let (mut func, line) = three(&[(41, 0)]);
        let kept = about(&mut func, &line);

        // Written by the first instruction and read by the third, so the stretch is the second and
        // the third: the register does not hold the value until the first has run, and the last
        // instruction that reads it is in the stretch rather than one past the end of it.
        assert_eq!(kept.len(), 1, "one stretch: {kept:?}");
        assert_eq!(kept[0].decl, 41);
        assert_eq!(kept[0].from, line[1], "from the instruction after the one that wrote it");
        assert_eq!(kept[0].to, line[2], "to the last one that reads it");
        assert!(matches!(kept[0].at, Where::Reg { .. }), "in a register: {:?}", kept[0].at);
    }

    #[test]
    fn a_value_nothing_reads_is_nowhere_worth_saying() {
        // The second instruction's result is never read, so the value is live only where it is
        // written and the stretch that would begin after that has nothing in it.
        let (mut func, line) = three(&[(41, 1)]);
        let kept = about(&mut func, &line);
        assert!(kept.is_empty(), "nothing to say: {kept:?}");
    }

    #[test]
    fn a_declaration_two_registers_hold_gets_a_stretch_for_each_of_them() {
        let (mut func, line) = three(&[(41, 0), (41, 1)]);
        let kept = about(&mut func, &line);

        // The one nothing reads still says nothing, so what is left is the one stretch, and the
        // point of the case is that one declaration being asked about twice is allowed.
        assert_eq!(kept.iter().map(|kept| kept.decl).collect::<Vec<u32>>(), vec![41]);
    }

    #[test]
    fn a_local_live_from_one_block_into_the_next_gets_a_stretch_in_each_of_them() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let tail = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(head, opcode).def(value, GPR).finish();
        let across = func.build(head, opcode).finish();
        *func.succs_mut(head) = vec![BlockCall::to(tail)];
        let read = func.build(tail, opcode).uses(value, GPR).finish();
        func.named = vec![(41, value)];
        let line = before(&func);
        let kept = about(&mut func, &line);

        // Live from where it is written to where it is read, and a stretch in each of the two
        // blocks rather than one that would cover whatever the layout later puts in between.
        assert_eq!(kept.len(), 2, "one stretch per block: {kept:?}");
        assert_eq!((kept[0].from, kept[0].to), (across, across), "the rest of the first block");
        assert_eq!((kept[1].from, kept[1].to), (read, read), "and into the second");
    }

    #[test]
    fn a_local_that_shares_its_frame_bytes_is_there_over_its_area_and_nowhere_else() {
        let (mut func, line) = three(&[]);
        let env = Env::new().with(GPR, &SYSV.int_order[..4], &SYSV.int_order[4..]);
        let allocation = rucc_regalloc::run(&mut func, &env, "test", true);
        let frame = Frame::of(&func, &allocation, &Layout::new(&SYSV, REGS));

        // Wanted from the first instruction to the second, so in its bytes over the second only,
        // and the third is where whatever it shares them with may have written over it.
        let order = &allocation.order;
        let area = [Range { start: order.early(line[0]), end: order.late(line[1]) }];
        let kept = of(&func, &line, &allocation, &frame, &[(41, -24, &area)]);
        assert_eq!(kept, [Kept { decl: 41, at: Where::Frame(-24), from: line[1], to: line[1] }]);
    }

    #[test]
    fn a_declaration_that_took_a_value_part_of_the_way_through_holds_it_from_there() {
        // `int m = a;` with the assignment in front of the third instruction: `a` holds the value
        // over the whole of its stretch and `m` only from there.
        let (mut func, line) = three(&[(41, 0)]);
        func.starts = vec![(42, Reg::virtual_reg(0), line[2])];
        let kept = about(&mut func, &line);
        let said: Vec<(u32, Inst, Inst)> =
            kept.iter().map(|kept| (kept.decl, kept.from, kept.to)).collect();
        assert_eq!(said, [(41, line[1], line[2]), (42, line[2], line[2])]);
    }

    #[test]
    fn a_declaration_that_took_a_value_holds_it_in_the_blocks_its_own_dominates_only() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let [head, left, below, right, tail] = std::array::from_fn(|_| func.create_block());
        let value = func.new_vreg(GPR);
        func.build(head, opcode).def(value, GPR).finish();
        let first = func.build(left, opcode).uses(value, GPR).finish();
        let under = func.build(below, opcode).uses(value, GPR).finish();
        func.build(right, opcode).uses(value, GPR).finish();
        func.build(tail, opcode).uses(value, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(left), BlockCall::to(right)];
        *func.succs_mut(left) = vec![BlockCall::to(below)];
        *func.succs_mut(below) = vec![BlockCall::to(tail)];
        *func.succs_mut(right) = vec![BlockCall::to(tail)];
        func.starts = vec![(42, value, first)];
        let line = before(&func);
        let kept = about(&mut func, &line);

        // The block the assignment is in and the one only it leads to. Not the other arm, which
        // never ran the assignment, and not the join, which the other arm reaches too.
        let said: Vec<(Inst, Inst)> = kept.iter().map(|kept| (kept.from, kept.to)).collect();
        assert_eq!(said, [(first, first), (under, under)]);
    }

    #[test]
    fn a_function_the_front_end_named_nothing_in_says_nothing() {
        let (mut func, line) = three(&[]);
        let kept = about(&mut func, &line);
        assert!(kept.is_empty(), "nothing to say: {kept:?}");
    }

    #[test]
    fn a_block_the_scheduler_reordered_is_read_by_where_the_two_ends_went() {
        let (mut func, line) = three(&[(41, 0)]);
        let env = Env::new().with(GPR, &SYSV.int_order[..4], &SYSV.int_order[4..]);
        let allocation = rucc_regalloc::run(&mut func, &env, "test", true);
        let frame = Frame::of(&func, &allocation, &Layout::new(&SYSV, REGS));

        // The first two instructions the other way round, which is what a scheduler leaves behind.
        // The value is written by what is now the second instruction and read by the third, so the
        // register holds it over the third only, and the first is before it was written.
        func.remove_inst(line[0]);
        func.insert_after(line[1], line[0]);
        let kept = of(&func, &line, &allocation, &frame, &[]);
        assert_eq!(kept.len(), 1, "one stretch: {kept:?}");
        assert_eq!((kept[0].from, kept[0].to), (line[2], line[2]));
    }

    #[test]
    fn a_local_live_across_the_whole_of_a_reordered_block_covers_all_of_it() {
        let (mut func, line) = three(&[]);
        let env = Env::new().with(GPR, &SYSV.int_order[..4], &SYSV.int_order[4..]);
        let allocation = rucc_regalloc::run(&mut func, &env, "test", true);
        let frame = Frame::of(&func, &allocation, &Layout::new(&SYSV, REGS));
        func.remove_inst(line[0]);
        func.insert_after(line[1], line[0]);

        // Live into the block and out of it, so its ends are the block's edges rather than any
        // instruction, and the stretch is from whatever is first now to whatever is last.
        let block = func.blocks().next().expect("one block");
        let (start, end) = allocation.order.bounds(block).expect("laid out");
        let area = [Range { start, end }];
        let kept = of(&func, &line, &allocation, &frame, &[(41, -8, &area)]);
        assert_eq!(kept, [Kept { decl: 41, at: Where::Frame(-8), from: line[1], to: line[2] }]);
    }
}
