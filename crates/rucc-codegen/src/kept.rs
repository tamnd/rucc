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
//! that reads it, both ends included. The front is one instruction along because a register does
//! not hold a value until the instruction writing it has run, and the back is where it is because
//! nothing reads the value afterwards, so whatever the allocator puts in the register next cannot
//! be seen by anybody asking. A value nothing reads at all gets no stretch, which is the same
//! sentence read the other way: the two ends cross.
//!
//! # What is left out
//!
//! A function whose instructions moved after it was allocated gets nothing. The liveness is
//! counted along the order the allocator laid the function out in, and a pass that reorders blocks
//! or schedules instructions makes that order no longer the order the function is in, so a stretch
//! worked out from it would name two instructions that are no longer either side of the value. The
//! check is the walk below, which notices the moment a surviving instruction is out of order.
//!
//! That is the optimized builds, where nothing here says anything yet, and `-O0` is what M8 is
//! about. Carrying the liveness across a reordering is what would lift it, and the register
//! allocator of M4 will want the same thing, since one that splits a live range has to say where
//! the pieces went too.
//!
//! A value the allocator spilled is in the frame over its stretch rather than in a register, which
//! is as much an answer as the other and is written the same way. A value it spilled in a function
//! whose alignment the prologue had to force has no answer, because the distance from the call
//! frame address is not a constant there, which is what `crate::frame` says about a local in the
//! same function.

use rucc_mir::{Func, Inst, Kept, Where};
use rucc_regalloc::Allocation;
use rucc_regalloc::assign::Place;
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

/// Which declaration is where, over which instructions, or nothing at all for a function the
/// answer cannot be given about. See the module documentation for which those are.
#[must_use]
pub fn of(func: &Func, before: &[Inst], allocation: &Allocation, frame: &Frame) -> Vec<Kept> {
    if func.named.is_empty() {
        return Vec::new();
    }
    let Some(line) = line(func, before, allocation) else { return Vec::new() };
    let mut out = Vec::new();
    for &(decl, reg) in &func.named {
        let at = match allocation.assignment.place(reg) {
            Some(Place::Reg(reg)) => Where::Reg(reg),
            Some(Place::Slot(slot)) => match frame.slot_from_frame_base(slot) {
                Some(at) => Where::Frame(at),
                None => continue,
            },
            None => continue,
        };
        let Some(area) = allocation.live.area(reg) else { continue };
        for piece in area.pieces() {
            // Strictly after where the value is written and up to and including where it is last
            // read. Both ends of a piece are points the value is live at, and the front one is the
            // instruction writing it, which is the one instruction in the piece the register does
            // not hold the value at the start of.
            let lo = line.partition_point(|&(point, _)| point <= piece.start);
            let hi = line.partition_point(|&(point, _)| point <= piece.end);
            if lo >= hi {
                continue;
            }
            out.push(Kept { decl, at, from: line[lo].1, to: line[hi - 1].1 });
        }
    }
    out
}

/// The instructions the function still has that the liveness knows a point for, in the order they
/// are in and paired with that point, or `None` if that is no longer the order the allocator saw.
///
/// The point is where the instruction reads its operands, which is the smaller of its two, so the
/// list is sorted by it and can be searched rather than scanned.
fn line(func: &Func, before: &[Inst], allocation: &Allocation) -> Option<Vec<(Point, Inst)>> {
    let mut known = vec![false; func.inst_count()];
    for &inst in before {
        known[inst.index()] = true;
    }
    let mut out = Vec::with_capacity(before.len());
    for block in func.blocks() {
        for inst in func.insts(block) {
            if !known[inst.index()] {
                continue;
            }
            let point = allocation.order.early(inst);
            if out.last().is_some_and(|&(last, _)| last >= point) {
                return None;
            }
            out.push((point, inst));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{Func, Opcode, Reg};
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
        of(func, line, &allocation, &frame)
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
        assert!(matches!(kept[0].at, Where::Reg(_)), "in a register: {:?}", kept[0].at);
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
    fn a_function_the_front_end_named_nothing_in_says_nothing() {
        let (mut func, line) = three(&[]);
        let kept = about(&mut func, &line);
        assert!(kept.is_empty(), "nothing to say: {kept:?}");
    }

    #[test]
    fn a_function_whose_instructions_moved_after_it_was_allocated_says_nothing() {
        let (mut func, line) = three(&[(41, 0)]);
        let env = Env::new().with(GPR, &SYSV.int_order[..4], &SYSV.int_order[4..]);
        let allocation = rucc_regalloc::run(&mut func, &env, "test", true);
        let frame = Frame::of(&func, &allocation, &Layout::new(&SYSV, REGS));
        assert!(!of(&func, &line, &allocation, &frame).is_empty(), "something to say first");

        // The same function with its first two instructions the other way round, which is what a
        // scheduler leaves behind and is the shape the allocator's liveness can no longer be read
        // against, since it is counted along the order the function was in.
        func.remove_inst(line[0]);
        func.insert_after(line[1], line[0]);
        assert!(of(&func, &line, &allocation, &frame).is_empty(), "no longer the order");
    }
}
