//! What a debugger can find out about the arguments of each call while the callee runs.
//!
//! A parameter is in its argument register when the callee starts, and the callee is free to use
//! that register for something else at once. GCC then says the parameter is "the value the register
//! had on entry", which is `DW_OP_entry_value`, and a debugger finds that value in the caller: the
//! call site entry of the call says where the caller still holds it. This is that answer, for each
//! call.
//!
//! The value of an argument is known when the instruction that last wrote its register before the
//! call is one of two:
//!
//! - A copy out of a register the call keeps, with nothing that writes that register between the
//!   copy and the call. The register then holds the value until the call returns.
//! - A constant written into the register.
//! - The instruction an argument of the caller arrives through, or a copy of a register that
//!   instruction wrote with nothing that writes it between. The value is then what that register
//!   held when the caller started, and a debugger finds it at the call one level further out. This
//!   is a parameter of the caller passed on unchanged, and GCC says the same thing for it.
//!
//! Anything else gives no answer for that argument, and a debugger then says the parameter is not
//! available, as it does today. Only the block of the call is searched, since the layout and the
//! passes after the allocator can put anything in front of it.
//!
//! A tail call is not here. The pass that turns it into a jump takes the call out, and the
//! arguments with it, and the caller has no frame left to ask about.

use rucc_base::Interner;
use rucc_mir::{Arg, Call, Constraint, Func, Inst, Opcode, Was};
use rucc_target::{CallRegs, FrameInsts, MachineInsts, PhysReg, RegClass};

/// The calls of a function and what is known about their arguments. See the module.
///
/// `small` is the instruction that writes a constant at thirty two bits, which a selector uses for
/// a small one, as the full name with the target's prefix.
#[must_use]
pub fn sites(
    func: &Func,
    shapes: &MachineInsts,
    frame: &FrameInsts,
    conv: &CallRegs,
    small: &str,
    names: &mut Interner,
) -> Vec<Call> {
    let imm = Opcode::new(names.join(frame.prefix, frame.imm));
    let small = Opcode::new(names.intern(small));
    let moves: Vec<Opcode> = frame
        .classes
        .iter()
        .map(|moves| Opcode::new(names.join(frame.prefix, moves.mov)))
        .collect();
    let mut out = Vec::new();
    for block in func.blocks() {
        let insts: Vec<Inst> = func.insts(block).collect();
        for (at, &inst) in insts.iter().enumerate() {
            if !shapes.calls(names.resolve(func[inst].opcode.name())) {
                continue;
            }
            let mut args = Vec::new();
            for operand in &func[func[inst].operands] {
                let Constraint::Fixed(reg) = operand.constraint else { continue };
                if operand.role.is_def() || !carries(conv, operand.class, reg) {
                    continue;
                }
                let class = operand.class;
                let Some(wrote) =
                    insts[..at].iter().rposition(|&before| writes(func, before, class, reg))
                else {
                    continue;
                };
                let last = insts[wrote];
                let data = &func[last];
                let was = if arrives(func, last, shapes, names) {
                    Was::Entry { reg, class }
                } else if moves.get(usize::from(class.number())) == Some(&data.opcode) {
                    let Some(from) = read(func, last, class) else { continue };
                    let written = |&between: &Inst| writes(func, between, class, from);
                    if keeps(conv, class, from) && !insts[wrote + 1..at].iter().any(written) {
                        Was::Reg { reg: from, class }
                    } else if insts[..wrote]
                        .iter()
                        .rev()
                        .find(|&before| written(before))
                        .is_some_and(|&first| arrives(func, first, shapes, names))
                    {
                        // The copy read the register before anything but the argument wrote it.
                        Was::Entry { reg: from, class }
                    } else {
                        continue;
                    }
                } else if data.opcode == imm && class == conv.int_class {
                    let Some(number) = data.imm.map(|at| func[at].0) else { continue };
                    Was::Constant(number as u64)
                } else if data.opcode == small && class == conv.int_class {
                    let Some(number) = data.imm.map(|at| func[at].0) else { continue };
                    // The instruction writes the bottom half and clears the top, on the one machine
                    // with registers wider than it.
                    Was::Constant(u64::from(number as u32))
                } else {
                    continue;
                };
                args.push(Arg { reg, class, was });
            }
            out.push(Call { inst, callee: func[inst].symbol, args });
        }
    }
    out
}

/// Whether the convention passes an argument in that register.
fn carries(conv: &CallRegs, class: RegClass, reg: PhysReg) -> bool {
    (class == conv.int_class && conv.int_args.contains(&reg))
        || (class == conv.sse_class && conv.sse_args.contains(&reg))
}

/// Whether that instruction is the one an argument arrives through.
///
/// It is at the top of the entry block and encodes to nothing, so the register it writes holds what
/// it held when the function started until the next instruction that writes it.
fn arrives(func: &Func, inst: Inst, shapes: &MachineInsts, names: &Interner) -> bool {
    shapes.arrives(names.resolve(func[inst].opcode.name()))
}

/// Whether a call leaves that register as it found it.
fn keeps(conv: &CallRegs, class: RegClass, reg: PhysReg) -> bool {
    (class == conv.int_class && conv.preserves_int(reg))
        || (class == conv.sse_class && conv.preserves_sse(reg))
}

/// Whether that instruction writes the register.
fn writes(func: &Func, inst: Inst, class: RegClass, reg: PhysReg) -> bool {
    func[func[inst].operands].iter().any(|operand| {
        operand.role.is_def() && operand.class == class && operand.reg.phys() == Some(reg)
    })
}

/// The one register a copy reads.
fn read(func: &Func, inst: Inst, class: RegClass) -> Option<PhysReg> {
    let mut read = func[func[inst].operands].iter().filter(|operand| !operand.role.is_def());
    let operand = read.next().filter(|operand| operand.class == class)?;
    if read.next().is_some() {
        return None;
    }
    operand.reg.phys()
}
