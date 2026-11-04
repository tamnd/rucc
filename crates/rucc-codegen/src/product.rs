//! Both halves of a product from the one `mul` that works them out, where they were an `imul` for
//! the low half and a `mul` for the high one.
//!
//! [`crate::wide`] writes a product two words wide as the low halves multiplied for the low word
//! and the high half of the same product for the high word, and selection gives each its own
//! instruction. The high one is `mulq`, which leaves the low word in `rax` as well, and the low
//! one is `imulq` on the same two registers working the low word out a second time. gcc writes
//! the one `mulq` and reads both halves of it. The kernel does this in `mul_u64_u32_shr`, which is
//! how the scheduler decays its load averages and how a clock turns cycles into nanoseconds, so
//! it is on every tick and every `sched_clock`.
//!
//! A signed high half is the same with `imul`, since the low word of a product is the same bits
//! whichever way the two were signed.
//!
//! # Why here
//!
//! Straight after selection, for the reason [`crate::carry`] gives: a virtual register is written
//! once, so the two instructions can be seen to read the same two registers by name. A rule
//! cannot say it, since a rule replaces one term and this is two terms becoming one instruction
//! with two answers.
//!
//! # Where the one instruction goes
//!
//! Where the earlier of the two was. Both read the same registers, so both were written before
//! that point, and each answer is then written no later than it was, so nothing that read either
//! one is left in front of it.

use rucc_base::Interner;
use rucc_base::hash::Set;
use rucc_mir as mir;
use rucc_target::{MachineInsts, OperandDesc, Role};

/// Turns every low half and high half of the same product in one block into one instruction, and
/// says how many it turned.
pub fn products(func: &mut mir::Func, shapes: &MachineInsts, names: &mut Interner) -> usize {
    let mut read = Set::default();
    for block in func.blocks() {
        for call in &func[block].succs {
            read.extend(call.args.iter().copied());
        }
        for inst in func.insts(block) {
            let operands = &func[func[inst].operands];
            read.extend(operands.iter().filter(|op| op.role == Role::Use).map(|op| op.reg));
        }
    }
    let mut made = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        let order: Vec<mir::Inst> = func.insts(block).collect();
        let mut taken = Set::default();
        let mut found = Vec::new();
        for (n, &high) in order.iter().enumerate() {
            let Some((name, width)) = high_half(func, shapes, names, high) else { continue };
            let &[answer, left, first, second] = &func[func[high].operands] else { continue };
            if !first.reg.is_virtual() || !second.reg.is_virtual() || read.contains(&left.reg) {
                continue;
            }
            let low = order.iter().enumerate().find(|&(_, &low)| {
                !taken.contains(&low)
                    && low_half(func, shapes, names, low, width)
                    && same(&func[func[low].operands], first.reg, second.reg)
            });
            let Some((m, &low)) = low else { continue };
            let name = format!("{name}_wide_{width}");
            let Some(descs) = (shapes.operands)(&name) else { continue };
            taken.insert(low);
            let lower = func[func[low].operands][0];
            let at = if m < n { low } else { high };
            let operands = [lower, answer, first, second];
            found.push(Both { at, high, low, name, descs, operands });
        }
        for both in found {
            let opcode = mir::Opcode::new(names.intern(&format!("{}{}", shapes.prefix, both.name)));
            let span = func.span(both.at);
            let mut build = func.build_loose(opcode).at(span);
            for (operand, desc) in both.operands.into_iter().zip(both.descs) {
                build = build.operand(operand.with(desc.constraint));
            }
            let inst = build.finish();
            func.insert_before(both.at, inst);
            func.remove_inst(both.high);
            func.remove_inst(both.low);
            made += 1;
        }
    }
    made
}

/// One pair of halves to put together, and where.
struct Both {
    at: mir::Inst,
    high: mir::Inst,
    low: mir::Inst,
    name: String,
    descs: &'static [OperandDesc],
    /// The low word, the high word and the two registers multiplied.
    operands: [mir::Operand; 4],
}

fn name<'a>(
    func: &mir::Func,
    shapes: &MachineInsts,
    names: &'a Interner,
    inst: mir::Inst,
) -> Option<&'a str> {
    names.resolve(func[inst].opcode.name()).strip_prefix(shapes.prefix)
}

/// Which multiply a high half is, `mul` or `imul`, and its width.
fn high_half(
    func: &mir::Func,
    shapes: &MachineInsts,
    names: &Interner,
    inst: mir::Inst,
) -> Option<(&'static str, &'static str)> {
    let name = name(func, shapes, names, inst)?;
    let (signed, width) = name.split_once("_high_")?;
    let signed = ["mul", "imul"].into_iter().find(|&known| known == signed)?;
    let width = ["32", "64"].into_iter().find(|&known| known == width)?;
    Some((signed, width))
}

fn low_half(
    func: &mir::Func,
    shapes: &MachineInsts,
    names: &Interner,
    inst: mir::Inst,
    width: &str,
) -> bool {
    name(func, shapes, names, inst).and_then(|name| name.strip_prefix("imul_rr_")) == Some(width)
}

/// Whether a low half writes a virtual register from the two registers, in either order.
fn same(operands: &[mir::Operand], first: mir::Reg, second: mir::Reg) -> bool {
    let &[lower, a, b] = operands else { return false };
    lower.role == Role::Def
        && lower.reg.is_virtual()
        && a.role == Role::Use
        && b.role == Role::Use
        && ((a.reg == first && b.reg == second) || (a.reg == second && b.reg == first))
}
