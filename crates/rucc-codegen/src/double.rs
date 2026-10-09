//! A shift of a value two words wide by a count in a register, as the `shld` and `shrd` the machine
//! has for it rather than the bits that cross put together by hand.
//!
//! [`crate::wide`] writes `x << n` on a `long long` on i386, and on a `__int128` on x86-64, as
//! each word moved by the count masked to a word, and the bits that cross moved the other way by
//! one place and then by the mask less the count, so that a count of zero carries nothing over.
//! That is right on every machine and it is eight instructions for the word the bits cross into,
//! three of them shifts by `cl` that each want the count in `ecx`. `shld %cl` is one, and it
//! takes the count as it is, since the machine masks a count to the word before it shifts.
//!
//! Every shift by `cl` whose count was masked to the word reads the count it was masked from
//! afterwards, for the same reason, and the mask goes when nothing else reads it.
//!
//! # Why here
//!
//! Straight after selection, for the reason [`crate::carry`] gives: a virtual register is written
//! once, so each part of the pattern can be followed back to the instruction that wrote it by name
//! and seen to have no other reader. A rule cannot say it, because the pattern is four levels deep
//! and the matcher reaches one.

use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_mir as mir;
use rucc_target::{MachineInsts, OperandDesc, Role};

/// Turns every word of a wide shift by a register that can be one into a `shld` or a `shrd`, and
/// says how many it turned.
pub fn doubles(func: &mut mir::Func, shapes: &MachineInsts, names: &mut Interner) -> usize {
    let mut made = 0;
    // Once for the function. What a rewrite takes away only lowers a count, which keeps every
    // later answer on the safe side, and the counts it raises are of registers nothing asks about.
    let (writer, reads) = registers(func);
    for block in func.blocks().collect::<Vec<_>>() {
        let order: Vec<mir::Inst> = func.insts(block).collect();
        let walk = Walk { func, shapes, names, writer: &writer, reads: &reads };
        let found: Vec<Double> = order.iter().filter_map(|&inst| walk.double(inst)).collect();
        for double in found {
            let opcode =
                mir::Opcode::new(names.intern(&format!("{}{}", shapes.prefix, double.name)));
            let span = func.span(double.or);
            // Each operand where the description wants it, the count in `cl` above all, since the
            // registers came from instructions that wanted them somewhere else.
            let operands = [double.answer, double.into, double.from, double.count];
            let mut build = func.build_loose(opcode).at(span);
            for (operand, desc) in operands.into_iter().zip(double.descs) {
                build = build.operand(operand.with(desc.constraint));
            }
            let shift = build.finish();
            func.insert_before(double.or, shift);
            func.remove_inst(double.or);
            for gone in double.gone {
                func.remove_inst(gone);
            }
            made += 1;
        }
    }
    unmasked(func, shapes, names);
    made
}

/// Every shift by `cl` whose count is the count masked to the width of the shift reads the count
/// itself, and a mask nothing reads any more goes.
fn unmasked(func: &mut mir::Func, shapes: &MachineInsts, names: &Interner) {
    let (writer, reads) = registers(func);
    let mut reads = reads;
    let mut masks: Vec<mir::Inst> = Vec::new();
    for block in func.blocks().collect::<Vec<_>>() {
        for inst in func.insts(block).collect::<Vec<_>>() {
            let walk = Walk { func, shapes, names, writer: &writer, reads: &reads };
            let Some(width) = walk.name(inst).and_then(|name| {
                ["shl_rcl_", "shr_rcl_", "sar_rcl_"]
                    .iter()
                    .find_map(|shift| name.strip_prefix(shift))
                    .and_then(word)
            }) else {
                continue;
            };
            let &[_, _, count] = walk.operands(inst) else { continue };
            let Some((mask, from)) = walk.masked(count, width) else { continue };
            let operands = func[inst].operands;
            func[operands][2].reg = from.reg;
            *reads.entry(from.reg).or_default() += 1;
            if let Some(left) = reads.get_mut(&count.reg) {
                *left -= 1;
                if *left == 0 {
                    masks.push(mask);
                }
            }
        }
    }
    for mask in masks {
        func.remove_inst(mask);
    }
}

/// Which instruction writes each register, and how many places read it.
fn registers(func: &mir::Func) -> (Map<mir::Reg, mir::Inst>, Map<mir::Reg, usize>) {
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
    (writer, reads)
}

/// The width of a word a shift by `cl` masks its count to, which is every width but a byte and a
/// half word, both of which the machine masks to thirty two places as well.
fn word(width: &str) -> Option<u32> {
    match width {
        "32" => Some(32),
        "64" => Some(64),
        _ => None,
    }
}

/// One `or` to rewrite and what goes with it.
struct Double {
    or: mir::Inst,
    name: String,
    answer: mir::Operand,
    /// The word the bits move within, which is the one the instruction writes over.
    into: mir::Operand,
    /// The word the bits that cross come from.
    from: mir::Operand,
    count: mir::Operand,
    descs: &'static [OperandDesc],
    gone: Vec<mir::Inst>,
}

struct Walk<'a> {
    func: &'a mir::Func,
    shapes: &'a MachineInsts,
    names: &'a Interner,
    writer: &'a Map<mir::Reg, mir::Inst>,
    reads: &'a Map<mir::Reg, usize>,
}

impl Walk<'_> {
    fn name(&self, inst: mir::Inst) -> Option<&str> {
        self.names.resolve(self.func[inst].opcode.name()).strip_prefix(self.shapes.prefix)
    }

    fn operands(&self, inst: mir::Inst) -> &[mir::Operand] {
        &self.func[self.func[inst].operands]
    }

    fn imm(&self, inst: mir::Inst) -> Option<i64> {
        self.func[inst].imm.map(|imm| self.func[imm].0)
    }

    /// The instruction in the same block as `user` that writes a register only one instruction
    /// reads, when it is called `name`.
    fn alone(&self, reg: mir::Reg, user: mir::Inst, name: &str) -> Option<mir::Inst> {
        let inst = *self.writer.get(&reg)?;
        (reg.is_virtual()
            && self.reads.get(&reg) == Some(&1)
            && self.func.block_of(inst) == self.func.block_of(user)
            && self.name(inst) == Some(name))
        .then_some(inst)
    }

    /// The count a register holds masked to `width`, and the `and` that masked it.
    fn masked(&self, count: mir::Operand, width: u32) -> Option<(mir::Inst, mir::Operand)> {
        let mask = *self.writer.get(&count.reg)?;
        if !count.reg.is_virtual()
            || self.name(mask) != Some(&format!("and_ri_{width}"))
            || self.imm(mask) != Some(i64::from(width - 1))
        {
            return None;
        }
        let &[_, from] = self.operands(mask) else { return None };
        (from.role == Role::Use && from.reg.is_virtual()).then_some((mask, from))
    }

    /// An `or` of a word shifted by a masked count and the bits that cross into it from the
    /// other word, in the shape [`crate::wide`] writes, in either order.
    fn double(&self, or: mir::Inst) -> Option<Double> {
        let width = word(self.name(or)?.strip_prefix("or_rr_")?)?;
        let &[answer, first, second] = self.operands(or) else { return None };
        [(first, second), (second, first)]
            .into_iter()
            .find_map(|(moved, across)| self.joined(or, width, answer, moved, across))
    }

    fn joined(
        &self,
        or: mir::Inst,
        width: u32,
        answer: mir::Operand,
        moved: mir::Operand,
        across: mir::Operand,
    ) -> Option<Double> {
        // A `shl` of the word the bits cross into and a `shr` of the word they come from, for
        // `shld`, or the other way round for `shrd`.
        let (going, crosses, name) = match self.name(*self.writer.get(&moved.reg)?)? {
            name if name == format!("shl_rcl_{width}") => ("shl", "shr", "shld"),
            name if name == format!("shr_rcl_{width}") => ("shr", "shl", "shrd"),
            _ => return None,
        };
        let moving = self.alone(moved.reg, or, &format!("{going}_rcl_{width}"))?;
        let &[_, into, places] = self.operands(moving) else { return None };
        let (_, count) = self.masked(places, width)?;
        let crossing = self.alone(across.reg, or, &format!("{crosses}_rcl_{width}"))?;
        let &[_, edge, rest] = self.operands(crossing) else { return None };
        let stepped = self.alone(edge.reg, crossing, &format!("{crosses}_ri_{width}"))?;
        let &[_, from] = self.operands(stepped) else { return None };
        if self.imm(stepped) != Some(1) {
            return None;
        }
        let mut gone = vec![moving, crossing, stepped];
        gone.extend(self.rest(rest, crossing, places, width)?);
        let reads = [into, from, count];
        if !reads.iter().all(|operand| operand.role == Role::Use && operand.reg.is_virtual()) {
            return None;
        }
        let name = format!("{name}_rcl_{width}");
        let descs = (self.shapes.operands)(&name)?;
        Some(Double { or, name, answer, into, from, count, descs, gone })
    }

    /// The instructions that work out the mask less the masked count, when that is what `rest`
    /// is, as a constant less the count or as the count with its bits flipped.
    fn rest(
        &self,
        rest: mir::Operand,
        user: mir::Inst,
        places: mir::Operand,
        width: u32,
    ) -> Option<Vec<mir::Inst>> {
        let top = i64::from(width - 1);
        if let Some(flip) = self.alone(rest.reg, user, &format!("xor_ri_{width}")) {
            let &[_, flipped] = self.operands(flip) else { return None };
            return (self.imm(flip) == Some(top) && flipped.reg == places.reg).then(|| vec![flip]);
        }
        let less = self.alone(rest.reg, user, &format!("sub_rr_{width}"))?;
        let &[_, mask, taken] = self.operands(less) else { return None };
        if taken.reg != places.reg {
            return None;
        }
        let constant = *self.writer.get(&mask.reg)?;
        if self.name(constant) != Some(&format!("mov_ri_{width}"))
            || self.imm(constant) != Some(top)
        {
            return None;
        }
        let mut gone = vec![less];
        gone.extend(self.alone(mask.reg, less, &format!("mov_ri_{width}")));
        Some(gone)
    }
}
