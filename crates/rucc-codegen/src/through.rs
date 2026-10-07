//! A call or a tail jump through a pointer a load has just read, as one instruction that reads the
//! pointer itself.
//!
//! tamnd/rucc#1994. `p->fn(x)` is a load of the pointer and a call through the register it went
//! into, and Postgres makes a call that way everywhere it goes through a table of methods:
//! `pfree` ends in a jump through the method its chunk header picks, and every node of a plan runs
//! through the function pointer it carries. gcc writes each of those as one instruction that reads
//! the pointer out of memory, `call *8(%rdi)` or `jmp *(%rcx,%rax)`, and this is what lets rucc do
//! the same:
//!
//! ```text
//!   movq (%rcx,%rax,1), %rax          jmp *(%rcx,%rax,1)
//!   jmp *%rax
//! ```
//!
//! It runs after the allocator and after [`crate::tail::jumps`], because a tail jump is only a
//! jump once that has made it one, and because the question it asks is about registers: whether
//! anything between the load and the branch writes a register the address names, reads the one
//! the pointer went into, or writes memory. Before the allocator the answer would have to be
//! written into a plan the allocator then honours, and a register freed this late is one nothing
//! wanted anyway, so the instruction is the whole of the saving and this is the cheap place for it.
//!
//! The register the pointer went into has to be finished with at the branch. A tail jump leaves
//! the function, and [`crate::lower`] gave it a register no argument is in, so nothing reads it
//! after. A call has to write it, which a call does to every register the convention lets the
//! callee destroy, and must not pass anything in it. A jump with somewhere in this function to go
//! is a computed `goto` or a table, and the blocks it goes to could read the same register, so it
//! is folded only when no way on from any of them reads the register before something writes it.
//! That is every handler of an interpreter like Postgres' `ExecInterpExpr`, which ends in
//! `goto *op->opcode` and so in `jmp *(%rcx)`, as clang writes it, where it was a load into a
//! register no handler reads before writing it again. A function with a landing pad keeps its
//! jumps as they are, since a pad is reached by no edge and could read a register a call keeps.
//!
//! Nothing is folded when a speculation hardening flag is in force. Those send a branch through a
//! register to a thunk named after the register, and a branch through memory has no register to
//! name.

use rucc_base::Interner;
use rucc_base::hash::{Map, Set};
use rucc_mir as mir;
use rucc_target::{FrameInsts, MachineInsts, PhysReg, RegClass};

/// Folds every load that only puts a pointer in front of a call or a tail jump through it into
/// that call or jump, and gives back how many it folded.
///
/// `stack` is the stack pointer, which a pop between the load and the branch moves without saying
/// so in its operands.
pub fn fold(
    func: &mut mir::Func,
    insts: &FrameInsts,
    machine: &MachineInsts,
    stack: PhysReg,
    names: &mut Interner,
) -> usize {
    let Some(through) = insts.through else { return 0 };
    let mut opcode =
        |name: &str| mir::Opcode::new(names.intern(&format!("{}{name}", insts.prefix)));
    let load = opcode(through.load);
    let pop = opcode(insts.pop);
    let (call, call_mem) = (opcode(through.call.0), opcode(through.call.1));
    let (jump, jump_mem) = (opcode(through.jump.0), opcode(through.jump.1));
    let blocks: Vec<mir::Block> = func.blocks().collect();
    let mut folded = 0;
    // The blocks each register a jump inside the function goes through is wanted on entry to,
    // worked out once for each register, since every handler of an interpreter goes through one
    // of a few and to the same blocks. A fold leaves the answer right, since what it takes out is
    // a write of the register and the jump's read of it straight after, at the end of a block
    // nothing after wants the register in.
    let mut wanted: Map<(mir::Reg, RegClass), Set<mir::Block>> = Map::default();
    for block in blocks {
        let order: Vec<mir::Inst> = func.insts(block).collect();
        for (at, &branch) in order.iter().enumerate() {
            let calls = func[branch].opcode == call;
            let jumps = func[branch].opcode == jump;
            let stays = jumps && !func[block].succs.is_empty();
            if (!calls && !jumps) || (stays && !func.landings.is_empty()) {
                continue;
            }
            let found =
                feeding(func, &order[..at], branch, calls, load, pop, stack, machine, names);
            let Some(from) = found else {
                continue;
            };
            if stays {
                let pointer = func[func[branch].operands][mir::defs(&func[func[branch].operands])];
                let target = (pointer.reg, pointer.class);
                let wanted = wanted.entry(target).or_insert_with(|| live_into(func, target));
                if func[block].succs.iter().any(|to| wanted.contains(&to.block)) {
                    continue;
                }
            }
            join(func, from, branch, if calls { call_mem } else { jump_mem });
            folded += 1;
        }
    }
    folded
}

/// The load whose register the branch goes through, when it can be folded into the branch.
///
/// `before` is every instruction of the block in front of the branch. The walk goes back from the
/// branch and stops at the first instruction that settles the question either way.
#[allow(clippy::too_many_arguments)]
fn feeding(
    func: &mir::Func,
    before: &[mir::Inst],
    branch: mir::Inst,
    calls: bool,
    load: mir::Opcode,
    pop: mir::Opcode,
    stack: PhysReg,
    machine: &MachineInsts,
    names: &Interner,
) -> Option<mir::Inst> {
    let operands = &func[func[branch].operands];
    let defs = mir::defs(operands);
    let pointer = operands.get(defs)?;
    pointer.reg.phys()?;
    // The register and its file together, since the files are numbered from nought alike and a
    // call that destroys `%xmm12` would otherwise be taken for one that destroys `%r12`.
    let target = (pointer.reg, pointer.class);
    let names_target = |operand: &mir::Operand| (operand.reg, operand.class) == target;
    // A call writes the register when the callee may destroy it, which is what says nothing after
    // the call wants what the load put there, and it must not be an argument as well.
    if calls && !operands[..defs].iter().any(names_target) {
        return None;
    }
    if operands[defs + 1..].iter().any(names_target) {
        return None;
    }
    let mut written: Vec<(mir::Reg, RegClass)> = Vec::new();
    let mut popped = false;
    for &inst in before.iter().rev() {
        let data = &func[inst];
        let read = &func[data.operands];
        if data.opcode == load && read.first().is_some_and(names_target) {
            // The address has to be the same address at the branch as it was at the load.
            let address = &read[1..];
            if address.iter().any(|operand| written.contains(&(operand.reg, operand.class))) {
                return None;
            }
            if popped && address.iter().any(|operand| operand.reg.phys() == Some(stack)) {
                return None;
            }
            // A row of the unwind table hung on the load would go with it.
            if func.cfi_after(inst).next().is_some() {
                return None;
            }
            return Some(inst);
        }
        if read.iter().any(names_target) {
            return None;
        }
        // A pop reads the stack and writes nothing in memory, so the word the load read is still
        // there behind it, and the epilogue between a load and a tail jump is made of them.
        let name = names.resolve(data.opcode.name());
        if data.opcode == pop {
            popped = true;
        } else if machine.calls(name) || !machine.has(name) || machine.touches_mem(name) {
            return None;
        }
        let defined = read.iter().filter(|operand| operand.role.is_def());
        written.extend(defined.map(|operand| (operand.reg, operand.class)));
    }
    None
}

/// The blocks that read the register, or lead to a block that reads it, before anything writes
/// it, which are the blocks it is live into.
///
/// What a block hands its successors counts as a read at its end, and a block that leaves the
/// function reads only what its last instructions name, since a register a call keeps is put back
/// by the epilogue in front of the return and the register is this function's to destroy
/// otherwise.
fn live_into(func: &mir::Func, target: (mir::Reg, RegClass)) -> Set<mir::Block> {
    let blocks: Vec<mir::Block> = func.blocks().collect();
    // For each block, whether it reads the register before writing it, writes it first, or names
    // it nowhere at all.
    let first: Vec<Option<bool>> = blocks
        .iter()
        .map(|&block| {
            for inst in func.insts(block) {
                let operands = &func[func[inst].operands];
                let names = |def: bool| {
                    operands.iter().any(|operand| {
                        operand.role.is_def() == def && (operand.reg, operand.class) == target
                    })
                };
                // The reads first, since an instruction reads its sources before it writes.
                if names(false) {
                    return Some(true);
                }
                if names(true) {
                    return Some(false);
                }
            }
            func[block].succs.iter().any(|call| call.args.contains(&target.0)).then_some(true)
        })
        .collect();
    let mut live: Set<mir::Block> = Set::default();
    let mut changed = true;
    while changed {
        changed = false;
        for (&block, &first) in blocks.iter().zip(&first).rev() {
            if live.contains(&block) {
                continue;
            }
            let reads = match first {
                Some(reads) => reads,
                None => func[block].succs.iter().any(|to| live.contains(&to.block)),
            };
            if reads {
                live.insert(block);
                changed = true;
            }
        }
    }
    live
}

/// The branch rewritten to read where it goes out of the address `from` read, and `from` gone.
///
/// The registers the address names take the place of the register the branch went through, in
/// front of whatever the branch read after it, so the positions the mode holds move along from
/// where they were in the load, behind its one definition, to where they are now.
fn join(func: &mut mir::Func, from: mir::Inst, branch: mir::Inst, into: mir::Opcode) {
    let address = func[func[from].operands][1..].to_vec();
    let mut amode = func[func[from].mem.expect("a load carries an address")];
    let operands = func[func[branch].operands].to_vec();
    let defs = mir::defs(&operands);
    let along = |at: u8| at - 1 + u8::try_from(defs).expect("a handful of operands");
    amode.base = amode.base.map(along);
    amode.index = amode.index.map(along);
    let rest = operands[..defs].iter().chain(&address).chain(&operands[defs + 1..]);
    let rest: Vec<mir::Operand> = rest.copied().collect();
    func[branch].operands = func.push_operands(&rest);
    func[branch].mem = Some(func.add_amode(amode));
    func[branch].opcode = into;
    func.remove_inst(from);
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Func, Mem, Opcode, Operand, Reg};
    use rucc_target::x86_64::{FRAME, GPR, MACHINE, R11, R12, RAX, RBX, RCX, RDI, RSI, RSP, XMM};

    use super::fold;

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> Opcode {
        Opcode::new(names.intern(&format!("x64.{name}")))
    }

    fn phys(reg: rucc_target::PhysReg) -> Reg {
        Reg::physical(reg)
    }

    /// The function once folded, one instruction to a line, and how many were folded.
    fn folded(names: &mut Interner, mut func: Func) -> (usize, Vec<String>) {
        let count = fold(&mut func, &FRAME, &MACHINE, RSP, names);
        let lines = rucc_mir::print_func(&func, names, &rucc_target::x86_64::REGS)
            .lines()
            .map(str::trim)
            .filter(|line| line.contains("x64."))
            .map(str::to_owned)
            .collect();
        (count, lines)
    }

    /// `movq 8(%rdi), %rax` in front of whatever `then` puts there and a call through `%rax` that
    /// passes `%rdi` and may destroy `%rax` and `%rcx`.
    fn call(
        names: &mut Interner,
        then: impl FnOnce(&mut Func, rucc_mir::Block, &mut Interner),
    ) -> Func {
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let load = op(names, "mov_rm_64");
        func.build(block, load)
            .def(phys(RAX), GPR)
            .mem(Mem { disp: 8, ..Mem::at(Operand::read(phys(RDI), GPR)) })
            .finish();
        then(&mut func, block, names);
        let call = op(names, "call_reg");
        func.build(block, call)
            .def(phys(RAX), GPR)
            .def(phys(RCX), GPR)
            .uses(phys(RAX), GPR)
            .uses(phys(RDI), GPR)
            .finish();
        func.build(block, op(names, "ret")).finish();
        func
    }

    #[test]
    fn a_call_through_a_pointer_just_loaded_reads_the_pointer_itself() {
        let mut names = Interner::new();
        let func = call(&mut names, |_, _, _| {});
        let (count, lines) = folded(&mut names, func);
        assert_eq!(count, 1, "{lines:#?}");
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(lines[0].contains("x64.call_mem"), "{lines:#?}");
        // The address register comes in where the pointer's was, ahead of the argument.
        assert!(lines[0].contains("[$rdi + 8]"), "{lines:#?}");
    }

    #[test]
    fn a_copy_that_leaves_the_address_alone_is_stepped_over() {
        let mut names = Interner::new();
        let func = call(&mut names, |func, block, names| {
            let mov = op(names, "mov_rr_64");
            func.build(block, mov).def(phys(RSI), GPR).uses(phys(RBX), GPR).finish();
        });
        let (count, lines) = folded(&mut names, func);
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines[0].starts_with("$rsi = x64.mov_rr_64"), "{lines:#?}");
        assert!(lines[1].contains("x64.call_mem"), "{lines:#?}");
    }

    #[test]
    fn nothing_is_folded_past_a_write_of_the_address_or_of_memory() {
        let mut names = Interner::new();
        // The argument is put where the address was read from.
        let func = call(&mut names, |func, block, names| {
            let mov = op(names, "mov_rr_64");
            func.build(block, mov).def(phys(RDI), GPR).uses(phys(RBX), GPR).finish();
        });
        assert_eq!(folded(&mut names, func).0, 0);
        // A store between them could be a store to the pointer.
        let func = call(&mut names, |func, block, names| {
            let store = op(names, "mov_mr_64");
            func.build(block, store)
                .uses(phys(RSI), GPR)
                .mem(Mem::at(Operand::read(phys(RBX), GPR)))
                .finish();
        });
        assert_eq!(folded(&mut names, func).0, 0);
    }

    #[test]
    fn a_pointer_the_call_leaves_alone_is_left_in_its_register() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let load = op(&mut names, "mov_rm_64");
        func.build(block, load)
            .def(phys(RBX), GPR)
            .mem(Mem::at(Operand::read(phys(RDI), GPR)))
            .finish();
        // `%rbx` survives the call, so something after it may still want the pointer.
        let call = op(&mut names, "call_reg");
        func.build(block, call).def(phys(RAX), GPR).uses(phys(RBX), GPR).finish();
        func.build(block, op(&mut names, "ret")).finish();
        assert_eq!(folded(&mut names, func).0, 0);
    }

    #[test]
    fn a_call_that_destroys_the_vector_register_of_the_same_number_leaves_the_pointer_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let load = op(&mut names, "mov_rm_64");
        func.build(block, load)
            .def(phys(R12), GPR)
            .mem(Mem::at(Operand::read(phys(RSP), GPR)))
            .finish();
        // Every call destroys `%xmm12`, which is numbered as `%r12` is, and `%r12` survives it.
        let call = op(&mut names, "call_reg");
        func.build(block, call)
            .def(phys(RAX), GPR)
            .def(phys(R12), XMM)
            .uses(phys(R12), GPR)
            .finish();
        func.build(block, op(&mut names, "ret")).finish();
        assert_eq!(folded(&mut names, func).0, 0);
    }

    /// Where a [`jump`] goes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Then {
        /// Out of the function.
        Leaves,
        /// To a block that returns.
        Returns,
        /// To a block that reads `%rax` and returns.
        Reads,
        /// Back to the top of its own block, which reads `%rax` as the index.
        Loops,
    }

    /// `movq (%rcx,%rax,1), %rax`, then `pops`, then `jmp *%rax`, in a block that goes where
    /// `then` says.
    fn jump(names: &mut Interner, base: rucc_target::PhysReg, pops: usize, then: Then) -> Func {
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let load = op(names, "mov_rm_64");
        func.build(block, load)
            .def(phys(RAX), GPR)
            .mem(Mem {
                index: Some(Operand::read(phys(RAX), GPR)),
                scale: 1,
                ..Mem::at(Operand::read(phys(base), GPR))
            })
            .finish();
        for _ in 0..pops {
            let pop = op(names, "pop_64");
            func.build(block, pop).def(phys(R11), GPR).finish();
        }
        let jump = op(names, "jmp_reg");
        func.build(block, jump).uses(phys(RAX), GPR).finish();
        if then == Then::Loops {
            func.succs_mut(block).push(BlockCall::to(block));
        } else if then != Then::Leaves {
            let next = func.create_block();
            if then == Then::Reads {
                let test = op(names, "test_rr_64");
                func.build(next, test).uses(phys(RAX), GPR).uses(phys(RAX), GPR).finish();
            }
            func.build(next, op(names, "ret")).finish();
            func.succs_mut(block).push(BlockCall::to(next));
        }
        func
    }

    /// [`jump`] once folded.
    fn jumped(base: rucc_target::PhysReg, pops: usize, then: Then) -> (usize, Vec<String>) {
        let mut names = Interner::new();
        let func = jump(&mut names, base, pops, then);
        folded(&mut names, func)
    }

    #[test]
    fn a_tail_jump_reads_the_pointer_past_the_epilogue() {
        let (count, lines) = jumped(RCX, 0, Then::Leaves);
        assert_eq!(count, 1, "{lines:#?}");
        assert_eq!(lines.len(), 1, "{lines:#?}");
        assert!(lines[0].starts_with("x64.jmp_mem"), "{lines:#?}");
        let (count, lines) = jumped(RCX, 2, Then::Leaves);
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines[2].starts_with("x64.jmp_mem"), "{lines:#?}");
    }

    #[test]
    fn a_jump_past_a_pop_that_moved_the_stack_it_read_is_left_alone() {
        assert_eq!(jumped(RSP, 1, Then::Leaves).0, 0);
        // With nothing between them, the address is the same one the load read.
        assert_eq!(jumped(RSP, 0, Then::Leaves).0, 1);
    }

    #[test]
    fn a_jump_inside_the_function_reads_the_pointer_when_nowhere_it_goes_wants_the_register() {
        let (count, lines) = jumped(RCX, 0, Then::Returns);
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines[0].starts_with("x64.jmp_mem"), "{lines:#?}");
        assert!(lines[0].contains("[$rcx + $rax]"), "{lines:#?}");
        // The block it goes to reads what the load put in `%rax`, and so does the top of a block
        // that goes back to itself, which reads it as the index.
        assert_eq!(jumped(RCX, 0, Then::Reads).0, 0);
        assert_eq!(jumped(RCX, 0, Then::Loops).0, 0);
    }

    #[test]
    fn a_jump_inside_a_function_with_a_landing_pad_is_left_alone() {
        let mut names = Interner::new();
        let mut func = jump(&mut names, RCX, 0, Then::Returns);
        let block = func.entry().expect("a block");
        let first = func.insts(block).next().expect("the load");
        let pad = func[block].succs[0].block;
        func.landings.push((first, pad));
        assert_eq!(folded(&mut names, func).0, 0);
    }
}
