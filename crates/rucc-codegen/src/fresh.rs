//! A constant that a two address instruction overwrites, written again in front of it.
//!
//! [`crate::lower`] writes a constant once in a block and hands the same register to every reader
//! in the block after it. That is the right answer for a reader that only reads it. A reader that
//! writes its answer over the register it reads, which on x86 is most arithmetic and every `cmov`,
//! cannot take the shared register as it is, so the allocator copies it into the answer's register
//! first. The copy is one instruction, and so is writing the number again, which reads nothing.
//!
//! What differs is what is live. With the copy the shared register is live from its one write to
//! its last reader, across everything in between. A row of `x & bit ? k : 0` on a `long long` on
//! i386 is eight `cmov`s on one zero, and with six registers to go round the zero is the value that
//! goes to the stack and is loaded back for each of them. Written again in front of each one, the
//! zero lives for one instruction and nothing goes to the stack for it.
//!
//! gcc gets the same from its allocator, which puts a constant back by writing it again rather
//! than by loading it. Ours has no such thing, and this is the case of it that costs nothing even
//! when nothing would have been spilled.
//!
//! # Why here
//!
//! Straight after selection, for the reason [`crate::carry`] gives: a virtual register is written
//! once, so the instruction that wrote the constant is found by name.

use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_mir as mir;
use rucc_target::{Constraint, MachineInsts, Role};

/// Writes the constant again in front of every instruction that overwrites one, and says how many
/// times it did.
pub fn constants(func: &mut mir::Func, shapes: &MachineInsts, names: &Interner) -> usize {
    let mut writer: Map<mir::Reg, mir::Inst> = Map::default();
    let mut reads: Map<mir::Reg, usize> = Map::default();
    for block in func.blocks() {
        for call in &func[block].succs {
            for &arg in &call.args {
                *reads.entry(arg).or_default() += 1;
            }
        }
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                if operand.role == Role::Use {
                    *reads.entry(operand.reg).or_default() += 1;
                } else {
                    writer.insert(operand.reg, inst);
                }
            }
        }
    }
    let mut made = 0;
    let mut gone = Vec::new();
    for block in func.blocks().collect::<Vec<_>>() {
        for inst in func.insts(block).collect::<Vec<_>>() {
            let operands = func[func[inst].operands].to_vec();
            for def in operands.iter().filter(|operand| operand.role != Role::Use) {
                let Constraint::Reuse(at) = def.constraint else { continue };
                let Some(&tied) = operands.get(usize::from(at)) else { continue };
                let Some(&number) = writer.get(&tied.reg) else { continue };
                if !tied.reg.is_virtual() || !constant(func, shapes, names, number) {
                    continue;
                }
                // Already the instruction in front with nothing else reading it, which is the
                // shape this would make.
                let left = reads.get(&tied.reg).copied().unwrap_or(0);
                if left == 1 && func.next_inst(number) == Some(inst) {
                    continue;
                }
                let Some(class) = func.class_of(tied.reg) else { continue };
                let fresh = func.new_vreg(class);
                let mut write = func[func[number].operands][0];
                write.reg = fresh;
                let value = func[number].imm.map(|imm| func[imm].0);
                let (opcode, span) = (func[number].opcode, func.span(inst));
                let mut build = func.build_loose(opcode).at(span).operand(write);
                if let Some(value) = value {
                    build = build.imm(value);
                }
                let again = build.finish();
                func.insert_before(inst, again);
                let list = func[inst].operands;
                func[list][usize::from(at)].reg = fresh;
                if let Some(count) = reads.get_mut(&tied.reg) {
                    *count -= 1;
                    if *count == 0 {
                        gone.push(number);
                    }
                }
                made += 1;
            }
        }
    }
    for inst in gone {
        func.remove_inst(inst);
    }
    made
}

/// Whether an instruction writes a number into a register and does nothing else.
fn constant(func: &mir::Func, shapes: &MachineInsts, names: &Interner, inst: mir::Inst) -> bool {
    let data = &func[inst];
    let name = names.resolve(data.opcode.name());
    let moves = name.strip_prefix(shapes.prefix).is_some_and(|name| name.starts_with("mov_ri_"));
    let operands = &func[data.operands];
    moves
        && data.imm.is_some()
        && data.mem.is_none()
        && data.symbol.is_none()
        && operands.len() == 1
        && operands[0].role != Role::Use
}
