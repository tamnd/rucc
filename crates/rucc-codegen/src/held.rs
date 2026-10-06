//! Taking out a write of a number into a register that holds that number already, on AArch64.
//!
//! A constant is written where it is wanted rather than where the IR defined it, which is what
//! [`crate::lower`] says about `reg_of`, so two blocks that want the same number write it twice.
//! Most of the time that is the right call, since one `mov` that reads nothing is cheaper than a
//! register held live across a branch. It is not right when the allocator then gives both writes
//! the same register and nothing in between touches it. A loop over an array that starts its sum
//! at zero is the common case: the entry block writes `mov x2, #0` for the path that skips the
//! loop, and the block in front of the loop writes `mov x2, #0` again for the path into it.
//!
//! So this walks the blocks with a map from a register to the number it holds, and takes out a
//! `mov` of a number into a register the map says has it. Unlike [`crate::copies`] the map does
//! cross blocks, because the block in front of a loop is the case. A block starts with what every
//! block before it agrees on, and only when all of those have been walked already. A block reached
//! by a back edge, by nothing at all, or with parameters starts with nothing, which is the cheap
//! half of the dataflow problem and the half the case needs.
//!
//! A copy of one register into another carries what the map knows about the first, and is taken
//! out the same way when the second has it already. The block in front of a loop whose counter
//! and sum both start at zero writes the zero once and copies it, which is `mov x2, x3` into an
//! `x2` the entry block set to zero.
//!
//! What clears an entry is what clears one in [`crate::copies`]: a write to the register, and
//! everything at a call or at an instruction the target does not name. The two widths of `mov`
//! both write the whole register, the narrow one with zeros above, so the map holds the whole
//! sixty four bits and a write of the same bits at either width is the same write.

use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_mir::{Block, Func, Inst, Opcode};
use rucc_target::{MachineInsts, PhysReg};

/// Where an instruction in [`A64_NUMBERS`] takes what it writes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    /// Its immediate.
    Imm,
    /// The register it reads.
    Reg,
}

/// The instructions that write a number or a copy of a register on AArch64, with how many bits
/// of it they keep. All four clear the rest.
pub const A64_NUMBERS: [(&str, u32, From); 4] = [
    ("a64.mov_ri_32", 32, From::Imm),
    ("a64.mov_ri_64", 64, From::Imm),
    ("a64.mov_rr_32", 32, From::Reg),
    ("a64.mov_rr_64", 64, From::Reg),
];

/// What each register is known to hold, by its class and its number in the class.
type Known = Map<(u8, PhysReg), u64>;

/// Takes out every write of a number into a register that has it, and gives back how many.
///
/// `numbers` is the list of instructions that write one or copy one, which is [`A64_NUMBERS`] on
/// AArch64 and empty anywhere this has not been looked at.
pub fn again(
    func: &mut Func,
    machine: &MachineInsts,
    numbers: &[(&str, u32, From)],
    names: &mut Interner,
) -> usize {
    if numbers.is_empty() {
        return 0;
    }
    let Some(entry) = func.entry() else { return 0 };
    let numbers: Map<Opcode, (u32, From)> = numbers
        .iter()
        .map(|&(name, bits, from)| (Opcode::new(names.intern(name)), (bits, from)))
        .collect();
    let mut preds: Map<Block, Vec<Block>> = Map::default();
    for block in func.blocks() {
        for call in &func[block].succs {
            preds.entry(call.block).or_default().push(block);
        }
    }
    let mut stops: Map<Opcode, bool> = Map::default();
    let mut left: Map<Block, Known> = Map::default();
    let mut gone: Vec<Inst> = Vec::new();
    for block in order(func, entry) {
        let mut known = if block == entry || !func[block].params.is_empty() {
            Known::default()
        } else {
            agreed(preds.get(&block).map_or(&[][..], Vec::as_slice), &left)
        };
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let data = func[inst];
            if let Some((key, number)) = written(func, inst, &numbers, &known) {
                match number {
                    Some(number) if known.get(&key) == Some(&number) => gone.push(inst),
                    Some(number) => {
                        known.insert(key, number);
                    }
                    None => {
                        known.remove(&key);
                    }
                }
                continue;
            }
            let stop = *stops.entry(data.opcode).or_insert_with(|| {
                let name = names.resolve(data.opcode.name());
                machine.calls(name) || !machine.has(name)
            });
            if stop {
                known.clear();
                continue;
            }
            for operand in &func[data.operands] {
                if let (true, Some(reg)) = (operand.role.is_def(), operand.reg.phys()) {
                    known.remove(&(operand.class.number(), reg));
                }
            }
        }
        left.insert(block, known);
    }
    for &inst in &gone {
        func.remove_inst(inst);
    }
    gone.len()
}

/// The register one of the moves in the list writes and the number it writes there, where it is
/// known. Nothing for an instruction that is not one of them, or is one with more to it than the
/// list says, which the caller treats like any other instruction.
fn written(
    func: &Func,
    inst: Inst,
    numbers: &Map<Opcode, (u32, From)>,
    known: &Known,
) -> Option<((u8, PhysReg), Option<u64>)> {
    let data = func[inst];
    let &(bits, from) = numbers.get(&data.opcode)?;
    if data.mem.is_some() || data.symbol.is_some() {
        return None;
    }
    match (from, &func[data.operands]) {
        (From::Imm, [to]) if to.role.is_def() => {
            let key = (to.class.number(), to.reg.phys()?);
            let number = u64::from_ne_bytes(func[data.imm?].0.to_ne_bytes());
            Some((key, Some(low(number, bits))))
        }
        (From::Reg, [to, read]) if to.role.is_def() && !read.role.is_def() => {
            let key = (to.class.number(), to.reg.phys()?);
            let number = read
                .reg
                .phys()
                .and_then(|reg| known.get(&(read.class.number(), reg)))
                .map(|&number| low(number, bits));
            Some((key, number))
        }
        _ => None,
    }
}

/// The blocks in reverse post order from the entry, so that every block a block is reached from
/// comes before it unless the edge between them is a back edge.
fn order(func: &Func, entry: Block) -> Vec<Block> {
    let mut seen: Map<Block, ()> = Map::default();
    let mut post: Vec<Block> = Vec::new();
    let mut stack: Vec<(Block, usize)> = vec![(entry, 0)];
    seen.insert(entry, ());
    while let Some((block, next)) = stack.pop() {
        if let Some(call) = func[block].succs.get(next) {
            stack.push((block, next + 1));
            if seen.insert(call.block, ()).is_none() {
                stack.push((call.block, 0));
            }
        } else {
            post.push(block);
        }
    }
    post.reverse();
    post
}

/// What every one of those blocks left in the same register, or nothing when one of them has not
/// been walked yet or there are none.
fn agreed(preds: &[Block], left: &Map<Block, Known>) -> Known {
    let Some((first, rest)) = preds.split_first() else { return Known::default() };
    let Some(start) = left.get(first) else { return Known::default() };
    let mut known = start.clone();
    for pred in rest {
        let Some(other) = left.get(pred) else { return Known::default() };
        known.retain(|key, number| other.get(key) == Some(number));
    }
    known
}

/// The low bits of a number with the rest cleared, as a whole register holds it.
fn low(number: u64, bits: u32) -> u64 {
    if bits >= 64 { number } else { number & ((1u64 << bits) - 1) }
}
