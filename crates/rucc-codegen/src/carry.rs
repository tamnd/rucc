//! The carry between the two halves of a wide add or subtract, as the flag the machine already
//! has rather than a comparison.
//!
//! [`crate::wide`] writes a `long long` sum on i386, and a `__int128` one on x86-64, as an add of
//! the low halves, a comparison that asks whether that add wrapped, the answer widened to a word,
//! and two adds for the high halves. That is correct on every machine, and on this one it is five
//! instructions and a byte register where gcc writes `addl` and `adcl`. The add of the low halves
//! has already left the bit the comparison works out in the carry flag, and `adc` is the
//! instruction that adds it in. The subtract is the same with `sbb` and the borrow.
//!
//! The kernel's 32 bit build is where this shows. Every `u64` in it is a pair, and `ktime_t`,
//! `jiffies_64`, sector numbers and the scheduler's clocks are all sums of them.
//!
//! # Why here
//!
//! Straight after selection, for the reason the folds after it give: a virtual register is
//! written once, so the comparison that reads the low sum can be traced back to the add that
//! wrote it by name, and the byte and the word it was widened into can be seen to have no other
//! reader. After allocation all of that is a question about physical registers.
//!
//! # What keeps the pair a pair
//!
//! Nothing comes between the two when this writes them, since the `adc` goes straight behind the
//! add. What keeps it that way afterwards is [`rucc_target::FlagInsts`], which says the `adc`
//! reads the carry. The scheduler reads it before moving anything, the combine of a load, an
//! operation and a store asks it before moving the add, and the shortening of an add of one into
//! an `inc`, which leaves the carry alone, asks it as well. That is the same thing that keeps the
//! pairs an asm template writes together.
//!
//! # What it will not do
//!
//! A high half whose operands are worked out after the low add, because the `adc` would read them
//! before they were written. A carry anything else reads, since the comparison and its byte are
//! what this takes away. A subtract of a constant written as an add of its negation, whose carry
//! is the other way round from the borrow.

use rucc_base::Interner;
use rucc_base::hash::{Map, Set};
use rucc_mir as mir;
use rucc_target::{MachineInsts, Role};

/// Turns every carry between two halves that can be one into the flag, and says how many it
/// turned.
pub fn carries(func: &mut mir::Func, shapes: &MachineInsts, names: &mut Interner) -> usize {
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
    for block in func.blocks().collect::<Vec<_>>() {
        let order: Vec<mir::Inst> = func.insts(block).collect();
        let at: Map<mir::Inst, usize> = order.iter().enumerate().map(|(n, &i)| (i, n)).collect();
        let walk =
            Walk { func, shapes, names, writer: &writer, reads: &reads, at: &at, order: &order };
        // What an earlier pair takes away or writes again, which a later one may not take as well.
        // A high half that is itself a sum with a carry in it is one pair's high and would be
        // the next pair's half, and the second would read the carry the first took out.
        let mut claimed = Set::default();
        let mut lows = Set::default();
        let mut found = Vec::new();
        for &inst in &order {
            let Some(pair) = walk.pair(inst, &claimed, &lows) else { continue };
            if claimed.contains(&pair.low) || !lows.insert(pair.low) {
                continue;
            }
            claimed.extend(pair.gone.iter().copied());
            found.push(pair);
        }
        for pair in found {
            let span = func.span(pair.high);
            let opcode = mir::Opcode::new(names.intern(&format!("{}{}", shapes.prefix, pair.name)));
            let mut build = func.build_loose(opcode).at(span).operand(pair.answer);
            for &operand in &pair.reads {
                build = build.operand(operand);
            }
            if let Some(imm) = pair.imm {
                build = build.imm(imm);
            }
            let adc = build.finish();
            func.insert_after(pair.low, adc);
            for gone in pair.gone {
                func.remove_inst(gone);
            }
            made += 1;
        }
    }
    made
}

/// One high half to rewrite: the add of the low halves it goes behind, what it becomes, and the
/// instructions that go with the comparison.
struct Pair {
    low: mir::Inst,
    high: mir::Inst,
    name: String,
    answer: mir::Operand,
    reads: Vec<mir::Operand>,
    imm: Option<i64>,
    gone: Vec<mir::Inst>,
}

struct Walk<'a> {
    func: &'a mir::Func,
    shapes: &'a MachineInsts,
    names: &'a Interner,
    writer: &'a Map<mir::Reg, mir::Inst>,
    reads: &'a Map<mir::Reg, usize>,
    at: &'a Map<mir::Inst, usize>,
    order: &'a [mir::Inst],
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

    /// The instruction in this block that writes a register only one instruction reads.
    fn alone(&self, reg: mir::Reg) -> Option<mir::Inst> {
        let inst = *self.writer.get(&reg)?;
        (reg.is_virtual() && self.reads.get(&reg) == Some(&1) && self.at.contains_key(&inst))
            .then_some(inst)
    }

    /// Whether a register holds its value by the time `low` has run.
    fn ready(&self, reg: mir::Reg, low: mir::Inst) -> bool {
        reg.is_virtual()
            && self.writer.get(&reg).is_none_or(|&by| match self.at.get(&by) {
                Some(&n) => n < self.at[&low],
                None => true,
            })
    }

    /// The high half of an add or subtract whose carry comes from a comparison of the low halves.
    fn pair(
        &self,
        high: mir::Inst,
        claimed: &Set<mir::Inst>,
        lows: &Set<mir::Inst>,
    ) -> Option<Pair> {
        let name = self.name(high)?;
        let (add, width) = match name.split_once("_rr_")? {
            ("add", width) => (true, width),
            ("sub", width) => (false, width),
            _ => return None,
        };
        if !matches!(width, "32" | "64") {
            return None;
        }
        let &[answer, first, second] = self.operands(high) else { return None };
        // An add reads the carry from either side and a subtract only takes it away.
        let sides: &[(mir::Operand, mir::Operand)] =
            if add { &[(first, second), (second, first)] } else { &[(first, second)] };
        let taken = |inst: &mir::Inst| claimed.contains(inst) || lows.contains(inst);
        sides.iter().find_map(|&(other, carry)| {
            self.carried(high, add, width, (answer, other, carry), &taken)
        })
    }

    fn carried(
        &self,
        high: mir::Inst,
        add: bool,
        width: &str,
        (answer, other, carry): (mir::Operand, mir::Operand, mir::Operand),
        taken: &dyn Fn(&mir::Inst) -> bool,
    ) -> Option<Pair> {
        let widen = self.alone(carry.reg)?;
        if self.name(widen)? != format!("bit_to_{width}") {
            return None;
        }
        let &[_, bit] = self.operands(widen) else { return None };
        let compare = self.alone(bit.reg)?;
        let low = self.low(compare, add, width)?;
        if self.at[&low] >= self.at[&compare] {
            return None;
        }
        let (op, carrying) = if add { ("add", "adc") } else { ("sub", "sbb") };
        let mut gone = vec![compare, widen, high];
        // The high halves themselves, when nothing else wants what they come to.
        let half = self.alone(other.reg).filter(|inst| !taken(inst)).filter(|&inst| {
            let name = self.name(inst);
            name == Some(&format!("{op}_rr_{width}")) || name == Some(&format!("{op}_ri_{width}"))
        });
        let (form, reads, imm) = match half {
            Some(inst) => {
                gone.push(inst);
                let reads = self.operands(inst)[1..].to_vec();
                match self.imm(inst) {
                    Some(k) => ("ri", reads, Some(k)),
                    None => ("rr", reads, None),
                }
            }
            None => ("ri", vec![other], Some(0)),
        };
        if !reads.iter().all(|operand| operand.role == Role::Use && self.ready(operand.reg, low)) {
            return None;
        }
        let name = format!("{carrying}_{form}_{width}");
        (self.shapes.operands)(&name)?;
        Some(Pair { low, high, name, answer, reads, imm, gone })
    }

    /// The add or subtract of the low halves a comparison asks about, when the comparison asks
    /// exactly what the carry flag it left says.
    ///
    /// An unsigned sum wrapped when it is below an operand, so for an add the comparison is of the
    /// sum against one of the two things added. A difference wrapped when the left operand was
    /// below the right, so for a subtract it is of the same two operands in the same order, and
    /// there is nothing to follow from one to the other but the operands themselves.
    fn low(&self, compare: mir::Inst, add: bool, width: &str) -> Option<mir::Inst> {
        let name = self.name(compare)?;
        let ops = self.operands(compare);
        if add {
            if name != format!("cmp_set_b_{width}") {
                return None;
            }
            let &[_, sum, against] = ops else { return None };
            let low = *self.writer.get(&sum.reg)?;
            self.at.get(&low)?;
            let read = &self.operands(low)[1..];
            let plain = self.name(low)? == format!("add_rr_{width}");
            let constant = self.name(low)? == format!("add_ri_{width}");
            let sources = read.iter().any(|operand| operand.reg == against.reg);
            ((plain || constant) && sources && sum.reg.is_virtual()).then_some(low)
        } else {
            let (form, k) = if name == format!("cmp_set_b_{width}") {
                ("rr", None)
            } else if name == format!("cmp_set_b_ri_{width}") {
                ("ri", Some(self.imm(compare)?))
            } else {
                return None;
            };
            let wanted = format!("sub_{form}_{width}");
            let sources: Vec<mir::Reg> = ops[1..].iter().map(|operand| operand.reg).collect();
            let before = &self.order[..self.at[&compare]];
            before.iter().rev().copied().find(|&inst| {
                self.name(inst) == Some(&wanted)
                    && self.imm(inst) == k
                    && self.operands(inst)[1..]
                        .iter()
                        .map(|operand| operand.reg)
                        .eq(sources.iter().copied())
            })
        }
    }
}
