//! The stack arguments of an i386 call pushed rather than stored, where that is fewer bytes.
//!
//! The lowering stores each argument a call passes on the stack into the outgoing area at the
//! bottom of the frame, `movl %eax, 4(%esp)` and `movl $.LC0, (%esp)`, which is what gcc writes at
//! -O2. At -Os gcc pushes them instead, last first, and takes them off again after the call:
//!
//! ```text
//!   movl $.LC0, (%esp)      7 bytes          pushl %eax      1 byte
//!   movl %eax, 4(%esp)      4 bytes          pushl $.LC0     5 bytes
//!   call printf                              call printf
//!                                            addl $8, %esp   3 bytes
//! ```
//!
//! A push of a register is one byte where the store is three or four, and a push of a constant is
//! two or five where the store is seven or eight, so all but a call of one register argument comes
//! out shorter, and a callee that takes its own arguments off the stack, which is every
//! non-variadic call under `-mrtd` and every `stdcall` one, also loses the `subl` the lowering
//! writes behind it to put the outgoing area back. FreeBSD's boot2 passes every `printf` argument
//! this way and has to fit in 7.5 KiB. tamnd/rucc-bsd#7.
//!
//! It runs after the allocator, the frame and every pass that moves an instruction, since what it
//! asks is about registers and about where the stack pointer is: each store is one of the call's,
//! nothing between it and the call writes the register it stores or touches the outgoing area, and
//! together the stores write the whole of the area the call passes, which [`crate::lower`] wrote
//! down for each call as it built it. The pushes go straight in front of the call, so nothing else
//! runs while the stack pointer is lower than the frame says it is, and the unwind table gets a row
//! for each push and one for the way back when it counts the frame from the stack pointer.

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{CallRegs, FrameInsts, MachineInsts};

/// The opcode of a push of a constant, which only i386's frame writes.
const PUSH_IMM: &str = "push_i_32";

/// Rewrites the stack arguments of each of those calls as pushes where that is fewer bytes, and
/// gives back how many calls it rewrote.
///
/// `calls` is each call that passes arguments on the stack with how many bytes of the outgoing area
/// they take, which is `passing` on [`crate::lower::Stack`]. A call is left alone when its area is
/// not a number of words the stack pointer may be left at for a call, which is every area under a
/// sixteen byte boundary that is not a multiple of sixteen.
pub fn arguments(
    func: &mut mir::Func,
    calls: &[(mir::Inst, u32)],
    insts: &FrameInsts,
    machine: &MachineInsts,
    conv: &CallRegs,
    names: &mut Interner,
) -> usize {
    let mut opcode = |name: &'static str| mir::Opcode::new(names.join(insts.prefix, name));
    let ops = Ops {
        store: opcode("mov_mr_32"),
        store_imm: opcode("mov_mi_32"),
        push: opcode(insts.push),
        push_imm: opcode(PUSH_IMM),
        add: opcode(insts.add),
        sub: opcode(insts.sub),
    };
    let stack = conv.stack_pointer;
    let sp = mir::Reg::physical(stack);
    let mut pushed = 0;
    for &(call, area) in calls {
        if area == 0 || area % 4 != 0 || area % conv.stack_align != 0 {
            continue;
        }
        let Some(plan) = plan(func, call, area, sp, &ops, machine, names) else { continue };
        let at = cfa(func, call, conv);
        rewrite(func, call, area, &plan, &ops, conv, at);
        pushed += 1;
    }
    pushed
}

/// The opcodes the rewrite reads and writes.
struct Ops {
    store: mir::Opcode,
    store_imm: mir::Opcode,
    push: mir::Opcode,
    push_imm: mir::Opcode,
    add: mir::Opcode,
    sub: mir::Opcode,
}

/// What one call's arguments become.
struct Plan {
    /// The store of each word of the area, from the bottom of it up.
    stores: Vec<mir::Inst>,
    /// The `subl` behind the call that puts back what a callee that cleans up took, which goes.
    popped: Option<mir::Inst>,
}

/// The stores of that call's arguments, when they can be pushes and the pushes are fewer bytes.
fn plan(
    func: &mir::Func,
    call: mir::Inst,
    area: u32,
    sp: mir::Reg,
    ops: &Ops,
    machine: &MachineInsts,
    names: &Interner,
) -> Option<Plan> {
    let block = func.block_of(call)?;
    if !machine.calls(names.resolve(func[call].opcode.name())) {
        return None;
    }
    // A call through an address counted from the stack pointer would read it from four bytes
    // further down for every push. A call also names the stack pointer as an operand it reads
    // and writes, and that is no obstacle: the pushes are in front of it.
    let operands = &func[func[call].operands];
    if let Some(mem) = func[call].mem {
        let amode = func[mem];
        let reads_sp = |at: Option<u8>| {
            at.and_then(|at| operands.get(usize::from(at))).is_some_and(|op| op.reg == sp)
        };
        if reads_sp(amode.base) || reads_sp(amode.index) {
            return None;
        }
    }
    let words = usize::try_from(area / 4).ok()?;
    let mut stores: Vec<Option<mir::Inst>> = vec![None; words];
    let mut found = 0;
    // The registers written between the instruction the walk is at and the call.
    let mut written: Vec<mir::Reg> = Vec::new();
    let order: Vec<mir::Inst> = func.insts(block).collect();
    let at = order.iter().position(|&inst| inst == call)?;
    for &inst in order[..at].iter().rev() {
        if found == words {
            break;
        }
        let data = &func[inst];
        let operands = &func[data.operands];
        if let Some(word) = word_stored(func, inst, area, sp, ops) {
            let source = (data.opcode == ops.store).then(|| operands[0].reg);
            if stores[word].is_some()
                || source.is_some_and(|reg| reg == sp || written.contains(&reg))
                || func.cfi_after(inst).next().is_some()
            {
                return None;
            }
            stores[word] = Some(inst);
            found += 1;
            continue;
        }
        // The arrival of an argument in a register encodes to nothing and only writes that
        // register, which the walk notes like any other write.
        let name = names.resolve(data.opcode.name());
        let described = machine.has(name) || machine.arrives(name);
        if machine.calls(name) || !described || func.cfi_after(inst).next().is_some() {
            return None;
        }
        // The stack pointer as anything but the base of an address above the area: written, read
        // as a number, or the base of an address in the area, which is the address of a word this
        // would move or a read of one before it is written.
        let amode = data.mem.map(|mem| func[mem]);
        for (index, operand) in operands.iter().enumerate() {
            if operand.reg != sp {
                continue;
            }
            let based = amode.is_some_and(|amode| {
                amode.base.map(usize::from) == Some(index)
                    && amode.index.is_none()
                    && i64::from(amode.disp) >= i64::from(area)
            });
            if operand.role.is_def() || !based {
                return None;
            }
        }
        written.extend(operands.iter().filter(|op| op.role.is_def()).map(|op| op.reg));
    }
    let stores: Vec<mir::Inst> = stores.into_iter().collect::<Option<_>>()?;
    // Behind the call, the `subl` that gives a callee that cleans up its area back, or nothing.
    let popped = func.next_inst(call).filter(|&next| {
        func[next].opcode == ops.sub
            && func[func[next].operands].iter().any(|op| op.role.is_def() && op.reg == sp)
    });
    if let Some(sub) = popped
        && (func[sub].imm.map(|imm| func[imm].0) != Some(i64::from(area))
            || func.cfi_after(sub).next().is_some())
    {
        return None;
    }
    let before: usize = stores.iter().map(|&store| stored_bytes(func, store, ops)).sum();
    let after: usize = stores.iter().map(|&store| pushed_bytes(func, store, ops)).sum::<usize>()
        + if popped.is_some() { 0 } else { arith_bytes(area) };
    let gone = if popped.is_some() { arith_bytes(area) } else { 0 };
    (after < before + gone).then_some(Plan { stores, popped })
}

/// Which word of the area that instruction stores, when it is a four byte store of a register or a
/// constant into it.
fn word_stored(
    func: &mir::Func,
    inst: mir::Inst,
    area: u32,
    sp: mir::Reg,
    ops: &Ops,
) -> Option<usize> {
    let data = &func[inst];
    if data.opcode != ops.store && data.opcode != ops.store_imm {
        return None;
    }
    let amode = func[data.mem?];
    let base = func[data.operands].get(usize::from(amode.base?))?;
    let plain = amode.index.is_none()
        && amode.symbol.is_none()
        && amode.block.is_none()
        && amode.table.is_none()
        && amode.segment.is_none();
    let disp = u32::try_from(amode.disp).ok()?;
    (base.reg == sp && plain && disp < area && disp % 4 == 0)
        .then(|| usize::try_from(disp / 4).ok())
        .flatten()
}

/// How many bytes the displacement of an address counted from the stack pointer takes.
fn disp_bytes(disp: i32) -> usize {
    match disp {
        0 => 0,
        _ if i8::try_from(disp).is_ok() => 1,
        _ => 4,
    }
}

/// How many bytes a store into the area is: the opcode, the `ModRM` and `SIB` bytes the stack
/// pointer needs, the displacement and the constant.
fn stored_bytes(func: &mir::Func, store: mir::Inst, ops: &Ops) -> usize {
    let data = &func[store];
    let disp = data.mem.map_or(0, |mem| func[mem].disp);
    let constant = if data.opcode == ops.store_imm { 4 } else { 0 };
    3 + disp_bytes(disp) + constant
}

/// How many bytes the push of what that store writes is.
fn pushed_bytes(func: &mir::Func, store: mir::Inst, ops: &Ops) -> usize {
    let data = &func[store];
    if data.opcode == ops.store {
        return 1;
    }
    let small =
        data.symbol.is_none() && data.imm.is_some_and(|imm| i8::try_from(func[imm].0).is_ok());
    if small { 2 } else { 5 }
}

/// How many bytes an `addl` or `subl` of that many bytes to the stack pointer is.
fn arith_bytes(area: u32) -> usize {
    if area <= 127 { 3 } else { 6 }
}

/// How far above the stack pointer the unwind table says the frame is at the call, or `None` when
/// it counts the frame from another register, which no push changes.
///
/// The rows are read in the order the instructions are laid out, which is the order the unwinder
/// reads them in, from what every function starts with: the frame is the return address above the
/// stack pointer.
fn cfa(func: &mir::Func, call: mir::Inst, conv: &CallRegs) -> Option<i32> {
    let sp = conv.dwarf(conv.int_class, conv.stack_pointer)?;
    let mut reg = sp;
    let mut offset = i32::try_from(conv.return_address).ok()?;
    for block in func.blocks() {
        for inst in func.insts(block) {
            if inst == call {
                return (reg == sp).then_some(offset);
            }
            for op in func.cfi_after(inst) {
                match op {
                    mir::CfiOp::DefCfa { reg: to, offset: by } => (reg, offset) = (to, by),
                    mir::CfiOp::DefCfaOffset(by) => offset = by,
                    mir::CfiOp::DefCfaRegister(to) => reg = to,
                    _ => {}
                }
            }
        }
    }
    None
}

/// The stores taken out, a push of each in front of the call from the top word down, and the area
/// given back behind it.
fn rewrite(
    func: &mut mir::Func,
    call: mir::Inst,
    area: u32,
    plan: &Plan,
    ops: &Ops,
    conv: &CallRegs,
    cfa: Option<i32>,
) {
    let mut below = cfa;
    for &store in plan.stores.iter().rev() {
        let data = func[store];
        let span = func.span(store);
        let push = if data.opcode == ops.store {
            let source = func[data.operands][0];
            func.build_loose(ops.push).at(span).uses(source.reg, source.class).finish()
        } else {
            let imm = data.imm.map_or(0, |imm| func[imm].0);
            let build = func.build_loose(ops.push_imm).at(span).imm(imm);
            match data.symbol {
                Some(symbol) => build.symbol(symbol).finish(),
                None => build.finish(),
            }
        };
        func.insert_before(call, push);
        func.remove_inst(store);
        below = below.map(|offset| offset + 4);
        if let Some(offset) = below {
            func.cfi.push((push, mir::CfiOp::DefCfaOffset(offset)));
        }
    }
    let back = match plan.popped {
        Some(sub) => {
            func.remove_inst(sub);
            call
        }
        None => {
            let sp = mir::Reg::physical(conv.stack_pointer);
            let class = conv.int_class;
            let span = func.span(call);
            let add = func
                .build_loose(ops.add)
                .at(span)
                .def(sp, class)
                .uses(sp, class)
                .imm(i64::from(area))
                .finish();
            func.insert_after(call, add);
            add
        }
    };
    if let Some(offset) = cfa {
        func.cfi.push((back, mir::CfiOp::DefCfaOffset(offset)));
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{CfiOp, Func, Inst, Mem, Opcode, Operand, Reg};
    use rucc_target::CallRegs;
    use rucc_target::x86::{EAX, ECX, ESP, FRAME, GPR, MACHINE, REGS, SYSV};

    use super::arguments;

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> Opcode {
        Opcode::new(names.intern(&format!("x64.{name}")))
    }

    fn sp() -> Operand {
        Operand::read(Reg::physical(ESP), GPR)
    }

    /// i386 System V under `-mpreferred-stack-boundary=2`, which is what boot code is built with.
    fn words() -> &'static CallRegs {
        SYSV.aligned_to(4)
    }

    /// `printf("%d", x)` as the lowering leaves it: the address of the format into the bottom word
    /// of the area, the number in `%eax` into the next, whatever `between` puts in front of the
    /// call, and the call. Gives back the function and the call.
    fn printf(
        names: &mut Interner,
        between: impl FnOnce(&mut Func, rucc_mir::Block, &mut Interner),
    ) -> (Func, Inst) {
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let format = names.intern(".LC0");
        func.build(block, op(names, "mov_mi_32")).mem(Mem::at(sp())).imm(0).symbol(format).finish();
        func.build(block, op(names, "mov_mr_32"))
            .uses(Reg::physical(EAX), GPR)
            .mem(Mem::at(sp()).plus(4))
            .finish();
        between(&mut func, block, names);
        let printf = names.intern("printf");
        // A call names the stack pointer as an operand, as the lowering leaves it.
        let call = func
            .build(block, op(names, "call"))
            .uses(Reg::physical(ESP), GPR)
            .symbol(printf)
            .finish();
        func.build(block, op(names, "ret")).finish();
        (func, call)
    }

    /// The function once rewritten, one instruction to a line from its opcode on, and how many
    /// calls were rewritten.
    fn pushed(
        names: &mut Interner,
        func: &mut Func,
        calls: &[(Inst, u32)],
        conv: &CallRegs,
    ) -> (usize, Vec<String>) {
        let count = arguments(func, calls, &FRAME, &MACHINE, conv, names);
        let lines = rucc_mir::print_func(func, names, &REGS)
            .lines()
            .filter_map(|line| line.find("x64.").map(|at| line[at..].to_owned()))
            .collect();
        (count, lines)
    }

    #[test]
    fn the_arguments_of_a_call_are_pushed_last_first_and_taken_off_after_it() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1, "{lines:#?}");
        assert_eq!(lines.len(), 5, "{lines:#?}");
        assert!(lines[0].starts_with("x64.push_32"), "{lines:#?}");
        assert!(lines[1].starts_with("x64.push_i_32") && lines[1].contains(".LC0"), "{lines:#?}");
        assert!(lines[2].starts_with("x64.call"), "{lines:#?}");
        assert!(lines[3].starts_with("x64.add_ri_32") && lines[3].contains('8'), "{lines:#?}");
    }

    #[test]
    fn a_callee_that_takes_its_arguments_off_keeps_the_stack_pointer_where_the_pushes_left_it() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let sub = op(&mut names, "sub_ri_32");
        let esp = Reg::physical(ESP);
        let back = func.build_loose(sub).def(esp, GPR).uses(esp, GPR).imm(8).finish();
        func.insert_after(call, back);
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines.iter().all(|line| !line.contains("sub_ri") && !line.contains("add_ri")));
        assert!(lines[2].starts_with("x64.call"), "{lines:#?}");
    }

    #[test]
    fn a_register_written_after_its_store_is_not_pushed() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |func, block, names| {
            func.build(block, op(names, "mov_ri_32")).def(Reg::physical(EAX), GPR).imm(1).finish();
        });
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 0, "{lines:#?}");
        assert!(lines.iter().all(|line| !line.contains("push")), "{lines:#?}");
    }

    /// A function that takes its arguments in registers under `-mregparm=3` and passes them on:
    /// each arrives through an `arg_val_32`, which writes the register and encodes to nothing.
    #[test]
    fn the_arrival_of_an_argument_between_the_stores_is_no_obstacle() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let format = names.intern(".LC0");
        func.build(block, op(&mut names, "mov_mi_32"))
            .mem(Mem::at(sp()))
            .imm(0)
            .symbol(format)
            .finish();
        for (reg, at) in [(EAX, 4), (ECX, 8)] {
            func.build(block, op(&mut names, "arg_val_32")).def(Reg::physical(reg), GPR).finish();
            func.build(block, op(&mut names, "mov_mr_32"))
                .uses(Reg::physical(reg), GPR)
                .mem(Mem::at(sp()).plus(at))
                .finish();
        }
        let printf = names.intern("printf");
        let call = func.build(block, op(&mut names, "call")).symbol(printf).finish();
        func.build(block, op(&mut names, "ret")).finish();
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 12)], words());
        assert_eq!(count, 1, "{lines:#?}");
        assert_eq!(lines.iter().filter(|line| line.contains("push")).count(), 3, "{lines:#?}");
    }

    #[test]
    fn a_word_of_the_area_read_before_the_call_keeps_its_store() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |func, block, names| {
            func.build(block, op(names, "mov_rm_32"))
                .def(Reg::physical(ECX), GPR)
                .mem(Mem::at(sp()).plus(4))
                .finish();
        });
        let (count, _) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 0);
    }

    #[test]
    fn a_slot_above_the_area_read_before_the_call_is_no_obstacle() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |func, block, names| {
            func.build(block, op(names, "mov_rm_32"))
                .def(Reg::physical(ECX), GPR)
                .mem(Mem::at(sp()).plus(12))
                .finish();
        });
        let (count, _) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1);
    }

    #[test]
    fn an_area_whose_stores_are_not_all_there_is_left_alone() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let (count, _) = pushed(&mut names, &mut func, &[(call, 12)], words());
        assert_eq!(count, 0);
    }

    #[test]
    fn a_call_through_a_word_counted_from_the_stack_pointer_keeps_its_stores() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        func.build(block, op(&mut names, "mov_mr_32"))
            .uses(Reg::physical(EAX), GPR)
            .mem(Mem::at(sp()))
            .finish();
        let call =
            func.build(block, op(&mut names, "call_mem")).mem(Mem::at(sp()).plus(8)).finish();
        func.build(block, op(&mut names, "ret")).finish();
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 4)], words());
        assert_eq!(count, 0, "{lines:#?}");
    }

    #[test]
    fn a_call_on_a_sixteen_byte_boundary_keeps_its_stores() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let (count, _) = pushed(&mut names, &mut func, &[(call, 8)], &SYSV);
        assert_eq!(count, 0);
    }

    #[test]
    fn one_register_argument_is_shorter_stored() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        func.build(block, op(&mut names, "mov_mr_32"))
            .uses(Reg::physical(EAX), GPR)
            .mem(Mem::at(sp()))
            .finish();
        let callee = names.intern("g");
        let call = func.build(block, op(&mut names, "call")).symbol(callee).finish();
        let (count, _) = pushed(&mut names, &mut func, &[(call, 4)], words());
        assert_eq!(count, 0);
    }

    #[test]
    fn the_unwind_table_follows_each_push_and_the_way_back() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let first = func.insts(func.blocks().next().expect("a block")).next().expect("a store");
        let sub = op(&mut names, "sub_ri_32");
        let esp = Reg::physical(ESP);
        let frame = func.build_loose(sub).def(esp, GPR).uses(esp, GPR).imm(12).finish();
        func.insert_before(first, frame);
        func.cfi.push((frame, CfiOp::DefCfaOffset(16)));
        let (count, _) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1);
        let rows: Vec<CfiOp> = func.cfi.iter().map(|&(_, row)| row).collect();
        assert_eq!(
            rows,
            [16, 20, 24, 16].map(CfiOp::DefCfaOffset),
            "one row for each push and one behind the call"
        );
        let back = func.next_inst(call).expect("the add");
        assert!(func.cfi_after(back).eq([CfiOp::DefCfaOffset(16)]));
    }
}
