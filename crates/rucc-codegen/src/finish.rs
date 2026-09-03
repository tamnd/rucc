//! The prologue, the epilogue, and the moves the allocator asked for.
//!
//! Design: `spec/10-backend.md` sections 10.4 and 10.7.
//!
//! [`crate::frame`] works out what a function's stack looks like and writes nothing. This is what
//! writes it. Three things are still missing from a function the allocator has finished with, and
//! all three of them are instructions no lowering rule chose:
//!
//! ```text
//!   the prologue     takes the frame the layout worked out, and puts away the registers a call
//!                    leaves alone that this function writes anyway
//!   the moves        every spill, every reload and every copy the allocator handed back as an
//!                    edit, in the place it said and in the order it said
//!   the epilogue     gives the frame back and puts the registers back, at the end of every block
//!                    the function returns from
//! ```
//!
//! After this the function is one an encoder can read: every register is physical, every offset
//! into the frame is a constant, and the stack pointer is where the convention says it should be
//! at every instruction that could look.
//!
//! # Why the moves go in first
//!
//! Every offset the frame reports is from the stack pointer as it stands in the body of the
//! function. A spill written before the prologue exists would be written in front of the
//! instruction it belongs to and behind nothing, which is where the prologue then goes, so the
//! prologue ends up in front of it and the offsets stay true. Writing them the other way round
//! would put the first reload above the instruction that takes the frame, and it would read from
//! an address that is one frame out.
//!
//! # Where a return is
//!
//! A block that goes nowhere is a block the function returns from. There is no other kind: a
//! block with no successors and no return would be one that falls off the end of the function,
//! which is a function that was mis-lowered rather than one this has an opinion about. So the
//! epilogue goes at the end of every block with an empty successor list, and there may be several,
//! because nothing here insists a function has one exit.
//!
//! # What is target-specific here
//!
//! The names, and only the names. Which instruction pushes a register and which one moves the
//! stack pointer is [`rucc_target::FrameInsts`], which the target says and this reads, so what
//! is written below is the shape of a prologue rather than any particular machine's. That is
//! `spec/10-backend.md` section 10.8 as it applies to the one pass that would otherwise be full
//! of `x64.` by hand.

use rucc_base::Interner;
use rucc_mir::{Block, Func, Inst, Mem, Opcode, Operand, Reg};
use rucc_regalloc::Allocation;
use rucc_regalloc::assign::Place;
use rucc_regalloc::rewrite::{At, Edit};
use rucc_target::{CallRegs, FrameInsts, PhysReg, RegClass};

use crate::frame::Frame;

/// Writes the moves, the prologue and the epilogue into a function the allocator has finished
/// with.
///
/// # Panics
///
/// Panics on a function with no blocks in it, on a frame whose slots the allocation does not
/// match, and on a move of a class the target did not say how to move. All three are the caller
/// handing it a frame and a function that were not worked out from each other.
pub fn finish(
    func: &mut Func,
    allocation: &Allocation,
    frame: &Frame,
    conv: &CallRegs,
    insts: &FrameInsts,
    names: &mut Interner,
) {
    let entry = func.entry().expect("a function with a block in it");
    let returns: Vec<Block> = func.blocks().filter(|&block| func[block].succs.is_empty()).collect();
    let mut writer = Writer { func, conv, insts, names };

    let mut cursors: Vec<(At, Inst)> = Vec::new();
    for edit in &allocation.edits {
        let inst = writer.mov(edit, frame);
        writer.put(&mut cursors, edit.at, inst);
    }

    let prologue = writer.prologue(frame);
    for &inst in prologue.iter().rev() {
        writer.func.prepend_inst(entry, inst);
    }
    for block in returns {
        let epilogue = writer.epilogue(frame);
        for inst in epilogue {
            writer.func.append_inst(block, inst);
        }
    }
}

/// One function having its frame written into it.
struct Writer<'a> {
    func: &'a mut Func,
    conv: &'a CallRegs,
    insts: &'a FrameInsts,
    names: &'a mut Interner,
}

impl Writer<'_> {
    /// The instructions the prologue is, in the order they run.
    ///
    /// The order is the one the epilogue undoes and it is not free. The frame pointer is saved
    /// before anything else, so that it points at a fixed place whatever else happens. The
    /// registers are pushed before the alignment is forced, so that the epilogue can find them
    /// again from the frame pointer, since after the alignment is forced nothing else can. And the
    /// vector registers are stored last, because until the frame has been taken there is nowhere
    /// to store them.
    fn prologue(&mut self, frame: &Frame) -> Vec<Inst> {
        let sp = self.conv.stack_pointer;
        let fp = self.conv.frame_pointer;
        let mut out = Vec::new();
        if frame.frame_pointer() {
            out.push(self.push(fp));
            let mov = self.opcode(self.insts.moves(self.conv.int_class).expect("a move").mov);
            out.push(self.two(mov, fp, sp));
        }
        for &reg in frame.saved_int() {
            out.push(self.push(reg));
        }
        if let Some(to) = frame.realign() {
            let and = self.opcode(self.insts.align);
            out.push(self.arith(and, -i64::from(to)));
        }
        if frame.size() > 0 {
            let sub = self.opcode(self.insts.sub);
            out.push(self.arith(sub, i64::from(frame.size())));
        }
        for save in frame.saved_sse() {
            out.push(self.store(self.conv.sse_class, save.reg, save.at));
        }
        out
    }

    /// The instructions the epilogue is, in the order they run.
    ///
    /// The vector registers are read back while the stack pointer is still where the body left it,
    /// because that is what their offsets are from. Then the stack pointer goes back to the last
    /// register the prologue pushed, which is arithmetic when the prologue knew how far it had
    /// moved and a read of the frame pointer when it did not.
    fn epilogue(&mut self, frame: &Frame) -> Vec<Inst> {
        let sp = self.conv.stack_pointer;
        let fp = self.conv.frame_pointer;
        let word = self.conv.word;
        let mut out = Vec::new();
        for save in frame.saved_sse() {
            out.push(self.load(self.conv.sse_class, save.reg, save.at));
        }
        let pushed = u32::try_from(frame.saved_int().len()).expect("a frame");
        if frame.frame_pointer() {
            if pushed == 0 {
                let mov = self.opcode(self.insts.moves(self.conv.int_class).expect("a move").mov);
                out.push(self.two(mov, sp, fp));
            } else {
                let lea = self.opcode(self.insts.lea);
                let back = -offset(word * pushed);
                out.push(self.address(lea, sp, fp, back));
            }
        } else if frame.size() > 0 {
            let add = self.opcode(self.insts.add);
            out.push(self.arith(add, i64::from(frame.size())));
        }
        for &reg in frame.saved_int().iter().rev() {
            out.push(self.pop(reg));
        }
        if frame.frame_pointer() {
            out.push(self.pop(fp));
        }
        let ret = self.opcode(self.insts.ret);
        out.push(self.func.build_loose(ret).finish());
        out
    }

    /// One edit as the instruction that makes it true.
    fn mov(&mut self, edit: &Edit, frame: &Frame) -> Inst {
        let moves = self.insts.moves(edit.class).expect("a class the target says how to move");
        match (edit.mov.to, edit.mov.from) {
            (Place::Reg(to), Place::Reg(from)) => {
                let mov = self.opcode(moves.mov);
                self.func
                    .build_loose(mov)
                    .def(Reg::physical(to), edit.class)
                    .uses(Reg::physical(from), edit.class)
                    .finish()
            }
            (Place::Reg(to), Place::Slot(slot)) => {
                let at = self.slot(frame, slot);
                self.load(edit.class, to, at)
            }
            (Place::Slot(slot), Place::Reg(from)) => {
                let at = self.slot(frame, slot);
                self.store(edit.class, from, at)
            }
            // The allocator expands this into two moves through a register of its own, because a
            // machine that could do it in one is not a machine any of this is written for.
            (Place::Slot(_), Place::Slot(_)) => {
                unreachable!("a move from one stack slot straight into another")
            }
        }
    }

    /// Puts an instruction where an edit says it goes, after whatever earlier edits went there.
    ///
    /// The edits at one place are in the order they have to be made in, so each one goes behind
    /// the last, and the first of them is what the place itself means.
    fn put(&mut self, cursors: &mut Vec<(At, Inst)>, at: At, inst: Inst) {
        if let Some(cursor) = cursors.iter_mut().find(|(place, _)| *place == at) {
            self.func.insert_after(cursor.1, inst);
            cursor.1 = inst;
            return;
        }
        match at {
            At::Before(before) => self.func.insert_before(before, inst),
            At::After(after) => self.func.insert_after(after, inst),
            At::StartOf(block) => self.func.prepend_inst(block, inst),
            // In front of the branch the block finishes with, which is the last instruction in it
            // and is what a block with an edge out of it always ends with.
            At::EndOf(block) => match self.func.terminator(block) {
                Some(branch) => self.func.insert_before(branch, inst),
                None => self.func.append_inst(block, inst),
            },
        }
        cursors.push((at, inst));
    }

    /// Where a spill slot is, from the stack pointer in the body of the function.
    fn slot(&self, frame: &Frame, slot: u32) -> i32 {
        frame.slot(slot).expect("a slot the frame was worked out from")
    }

    /// Reads a register out of the frame.
    fn load(&mut self, class: RegClass, reg: PhysReg, at: i32) -> Inst {
        let load = self.opcode(self.insts.moves(class).expect("a class to load").load);
        let base = Operand::read(Reg::physical(self.conv.stack_pointer), self.conv.int_class);
        self.func
            .build_loose(load)
            .def(Reg::physical(reg), class)
            .mem(Mem::at(base).plus(at))
            .finish()
    }

    /// Writes a register into the frame.
    fn store(&mut self, class: RegClass, reg: PhysReg, at: i32) -> Inst {
        let store = self.opcode(self.insts.moves(class).expect("a class to store").store);
        let base = Operand::read(Reg::physical(self.conv.stack_pointer), self.conv.int_class);
        self.func
            .build_loose(store)
            .uses(Reg::physical(reg), class)
            .mem(Mem::at(base).plus(at))
            .finish()
    }

    /// Puts a general purpose register on the stack.
    fn push(&mut self, reg: PhysReg) -> Inst {
        let push = self.opcode(self.insts.push);
        self.func.build_loose(push).uses(Reg::physical(reg), self.conv.int_class).finish()
    }

    /// Takes a general purpose register back off the stack.
    fn pop(&mut self, reg: PhysReg) -> Inst {
        let pop = self.opcode(self.insts.pop);
        self.func.build_loose(pop).def(Reg::physical(reg), self.conv.int_class).finish()
    }

    /// One general purpose register written with another.
    fn two(&mut self, opcode: Opcode, to: PhysReg, from: PhysReg) -> Inst {
        let class = self.conv.int_class;
        self.func
            .build_loose(opcode)
            .def(Reg::physical(to), class)
            .uses(Reg::physical(from), class)
            .finish()
    }

    /// Two-address arithmetic on the stack pointer, which reads it and writes it back.
    fn arith(&mut self, opcode: Opcode, value: i64) -> Inst {
        let class = self.conv.int_class;
        let sp = Reg::physical(self.conv.stack_pointer);
        self.func.build_loose(opcode).def(sp, class).uses(sp, class).imm(value).finish()
    }

    /// One register written with an address rather than with what is at it.
    fn address(&mut self, opcode: Opcode, to: PhysReg, base: PhysReg, disp: i32) -> Inst {
        let class = self.conv.int_class;
        let base = Operand::read(Reg::physical(base), class);
        self.func
            .build_loose(opcode)
            .def(Reg::physical(to), class)
            .mem(Mem::at(base).plus(disp))
            .finish()
    }

    /// The opcode of that name, in the machine IR's spelling, which is the target's prefix and
    /// then the name the target gave.
    fn opcode(&mut self, name: &str) -> Opcode {
        Opcode::new(self.names.intern(&format!("{}{name}", self.insts.prefix)))
    }
}

/// A distance in a frame, as the signed number every offset is.
fn offset(bytes: u32) -> i32 {
    i32::try_from(bytes).expect("a frame under two gigabytes")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, print_func};
    use rucc_regalloc::assign::Env;
    use rucc_target::x86_64::{FRAME, GPR, REGS, SYSV, WIN64, XMM, xmm};

    use super::*;
    use crate::frame::{Layout, Local};

    /// An environment offering that many of the convention's registers, with everything after
    /// them held back as scratch.
    fn env(conv: &CallRegs, count: usize) -> Env {
        Env::new().with(GPR, &conv.int_order[..count], &conv.int_order[count..])
    }

    /// A function of that many values, every one written before any is read, allocated with that
    /// many registers to hand out. The same shape the frame layout's own tests are written
    /// against, so that a frame here is one that has already been checked there.
    fn pressure(conv: &CallRegs, values: usize, count: usize) -> (Func, Allocation, Interner) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let regs: Vec<Reg> = (0..values).map(|_| func.new_vreg(GPR)).collect();
        for &reg in &regs {
            func.build(block, opcode).def(reg, GPR).finish();
        }
        for &reg in &regs {
            func.build(block, opcode).uses(reg, GPR).finish();
        }
        let allocation = rucc_regalloc::run(&mut func, &env(conv, count));
        (func, allocation, names)
    }

    /// The function with its frame written into it, as the lines a dump would show.
    fn written(
        func: &mut Func,
        allocation: &Allocation,
        layout: &Layout<'_>,
        names: &mut Interner,
    ) -> Vec<String> {
        let frame = Frame::of(func, allocation, layout);
        finish(func, allocation, &frame, layout.conv, &FRAME, names);
        print_func(func, names, &REGS)
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.trim().to_string())
            .collect()
    }

    /// Just the lines the frame put in, which is every line that is not the function it was
    /// given and not the shape of the dump around it.
    fn added(lines: &[String]) -> Vec<&str> {
        lines
            .iter()
            .map(String::as_str)
            .filter(|line| !line.contains("x64.nop"))
            .filter(|line| !line.starts_with("mfunc") && !line.starts_with("block") && *line != "}")
            .collect()
    }

    #[test]
    fn a_function_that_needs_no_frame_is_given_a_return_and_nothing_else() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 2, 4);
        let lines = written(&mut func, &allocation, &Layout::new(&SYSV, REGS), &mut names);

        // Two values and four registers, so nothing is spilled, nothing is saved and the stack
        // pointer never moves. A prologue of nothing is the right prologue for that.
        assert_eq!(added(&lines), ["x64.ret"]);
    }

    #[test]
    fn a_spill_is_a_store_and_a_reload_is_a_load() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 4, 2);
        let lines = written(&mut func, &allocation, &Layout::new(&SYSV, REGS), &mut names);

        // Two registers for four values, so two of them go to the stack. The store goes behind the
        // instruction that wrote the value and the load in front of the one that wants it, both at
        // the offsets the frame gave, which are below the stack pointer because a small leaf
        // function is entitled to the red zone.
        assert_eq!(
            lines,
            [
                "mfunc @f {",
                "block0:",
                "$rax = x64.nop",
                "$rcx = x64.nop",
                "$rdx = x64.nop",
                "x64.mov_mr_64 $rdx, [$rsp - 16]",
                "$rdx = x64.nop",
                "x64.mov_mr_64 $rdx, [$rsp - 8]",
                "x64.nop $rax",
                "x64.nop $rcx",
                "$rdx = x64.mov_rm_64 [$rsp - 16]",
                "x64.nop $rdx",
                "$rdx = x64.mov_rm_64 [$rsp - 8]",
                "x64.nop $rdx",
                "x64.ret",
                "}",
            ]
        );
    }

    #[test]
    fn the_frame_the_prologue_takes_is_the_frame_the_epilogue_gives_back() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 4, 2);
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { red_zone: false, ..base };
        let lines = written(&mut func, &allocation, &layout, &mut names);

        // The same function told it may not use the red zone takes sixteen bytes instead, and
        // every offset moves above the stack pointer to match.
        assert_eq!(
            added(&lines),
            [
                "$rsp = x64.sub_ri_64 $rsp, 16",
                "x64.mov_mr_64 $rdx, [$rsp]",
                "x64.mov_mr_64 $rdx, [$rsp + 8]",
                "$rdx = x64.mov_rm_64 [$rsp]",
                "$rdx = x64.mov_rm_64 [$rsp + 8]",
                "$rsp = x64.add_ri_64 $rsp, 16",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn the_registers_the_prologue_pushes_come_back_in_the_opposite_order() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 13, 13);
        let lines = written(&mut func, &allocation, &Layout::new(&SYSV, REGS), &mut names);

        // Four registers a call leaves alone, pushed in the convention's order and popped in the
        // other one, which is the only order that gets each of them its own value back.
        assert_eq!(
            added(&lines),
            [
                "x64.push_64 $rbx",
                "x64.push_64 $r12",
                "x64.push_64 $r13",
                "x64.push_64 $r14",
                "$r14 = x64.pop_64",
                "$r13 = x64.pop_64",
                "$r12 = x64.pop_64",
                "$rbx = x64.pop_64",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn a_function_that_keeps_a_frame_pointer_sets_it_up_and_leaves_by_it() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 4, 2);
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { frame_pointer: true, red_zone: false, ..base };
        let lines = written(&mut func, &allocation, &layout, &mut names);

        // The frame pointer is saved before anything else and points at where it was saved, so the
        // epilogue reaches the stack pointer through it rather than by counting the frame back.
        assert_eq!(
            added(&lines),
            [
                "x64.push_64 $rbp",
                "$rbp = x64.mov_rr_64 $rsp",
                "$rsp = x64.sub_ri_64 $rsp, 16",
                "x64.mov_mr_64 $rdx, [$rsp]",
                "x64.mov_mr_64 $rdx, [$rsp + 8]",
                "$rdx = x64.mov_rm_64 [$rsp]",
                "$rdx = x64.mov_rm_64 [$rsp + 8]",
                "$rsp = x64.mov_rr_64 $rbp",
                "$rbp = x64.pop_64",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn a_realigned_frame_forces_the_alignment_after_it_has_pushed_what_it_saves() {
        let (mut func, allocation, mut names) = pressure(&SYSV, 13, 13);
        let locals = [Local { size: 64, align: 32 }];
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { locals: &locals, ..base };
        let lines = written(&mut func, &allocation, &layout, &mut names);

        // Forcing the alignment throws away how far the stack pointer had moved, so the registers
        // are pushed before it happens and the epilogue counts back from the frame pointer to find
        // them. The frame pointer is required here whatever the flags said.
        assert_eq!(
            added(&lines),
            [
                "x64.push_64 $rbp",
                "$rbp = x64.mov_rr_64 $rsp",
                "x64.push_64 $rbx",
                "x64.push_64 $r12",
                "x64.push_64 $r13",
                "x64.push_64 $r14",
                "$rsp = x64.and_ri_64 $rsp, -32",
                "$rsp = x64.sub_ri_64 $rsp, 64",
                "$rsp = x64.lea_64 [$rbp - 32]",
                "$r14 = x64.pop_64",
                "$r13 = x64.pop_64",
                "$r12 = x64.pop_64",
                "$rbx = x64.pop_64",
                "$rbp = x64.pop_64",
                "x64.ret",
            ]
        );
    }

    #[test]
    fn every_block_the_function_returns_from_gets_an_epilogue() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let left = func.create_block();
        let right = func.create_block();
        func.build(head, opcode).finish();
        *func.succs_mut(head) = vec![BlockCall::to(left), BlockCall::to(right)];
        func.build(left, opcode).finish();
        func.build(right, opcode).finish();
        let allocation = rucc_regalloc::run(&mut func, &env(&SYSV, 4));
        let base = Layout::new(&SYSV, REGS);
        let layout = Layout { leaf: false, ..base };
        let lines = written(&mut func, &allocation, &layout, &mut names);

        // Both ways out get the frame given back, and the block that goes somewhere gets nothing,
        // because a block with an edge out of it is not a block anything returns from.
        assert_eq!(
            lines,
            [
                "mfunc @f {",
                "block0:",
                "$rsp = x64.sub_ri_64 $rsp, 8",
                "x64.nop block1, block2",
                "block1:",
                "x64.nop",
                "$rsp = x64.add_ri_64 $rsp, 8",
                "x64.ret",
                "block2:",
                "x64.nop",
                "$rsp = x64.add_ri_64 $rsp, 8",
                "x64.ret",
                "}",
            ]
        );
    }

    #[test]
    fn a_vector_register_a_windows_call_preserves_is_stored_and_read_back() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        // An instruction that writes one of the vector registers Windows preserves, which is what
        // a rule for something that has to use it produces.
        func.build(block, opcode).operand(Operand::write(Reg::physical(xmm(6)), XMM)).finish();
        let allocation = rucc_regalloc::run(&mut func, &env(&WIN64, 4));
        let lines = written(&mut func, &allocation, &Layout::new(&WIN64, REGS), &mut names);

        // No machine here pushes a vector register, so it is stored into the frame rather than
        // pushed, and the frame has to be taken before there is anywhere to put it.
        assert_eq!(
            added(&lines),
            [
                "$rsp = x64.sub_ri_64 $rsp, 24",
                "x64.movaps_mr $xmm6, [$rsp]",
                "$xmm6 = x64.movaps_rm [$rsp]",
                "$rsp = x64.add_ri_64 $rsp, 24",
                "x64.ret",
            ]
        );
    }
}
