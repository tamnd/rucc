//! An ordered comparison of two pairs as the borrow out of subtracting one from the other.
//!
//! [`crate::wide`] writes `a < b` on a `long long` on i386, and on an `__int128` on x86-64, as
//! three comparisons of halves put together with an `and` and an `or`: the high halves are below,
//! or they are equal and the low halves are below. That is right on every machine, and here it is
//! three `cmp`, three `setcc`, an `andb` and an `orb`. gcc writes `cmpl` of the low halves, `sbbl`
//! of the high ones into a copy, and a jump or a `setcc` on the flags the `sbbl` left, since the
//! borrow out of the whole subtract is the answer for an unsigned comparison and the sign and the
//! overflow it leaves are the answer for a signed one. The kernel's `u64` sector numbers, times and
//! sizes are compared all over drivers/md/md.c, and it had 350 `setcc` where gcc has a handful.
//!
//! Only the strict comparison is a borrow, so `a <= b` is written as `b < a` and the condition
//! turned round, and `a > b` is `b < a`. What is subtracted has to be in a register, so a
//! comparison that would subtract from a constant is left as it was.
//!
//! # Why here
//!
//! For the reason [`crate::carry`] gives, which runs at the same moment: a virtual register is
//! written once, so the three comparisons can be followed from the `or` by name, and each can be
//! seen to have nothing else reading it.
//!
//! # What it reads
//!
//! Only the shape of the instructions, never which register is the low half of which value. The
//! `or` says `(p ? q) | (p == q & r ? s)`. Each comparison is turned round on its own until it is a
//! `<` or a `<=`, since `p > q` is `q < p`, and then that is a comparison of `p:r` with `q:s`
//! whatever the registers hold, so nothing has to be known about where they came from.

use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_mir as mir;
use rucc_target::{Constraint, MachineInsts, Role};

/// Turns every ordered comparison of two pairs that can be one into a subtract with a borrow, and
/// says how many it turned.
pub fn borrows(func: &mut mir::Func, shapes: &MachineInsts, names: &mut Interner) -> usize {
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
        let found: Vec<Ordered> = {
            let look = Look { func, shapes, names, writer: &writer, reads: &reads, at: &at };
            order.iter().filter_map(|&inst| look.ordered(inst)).collect()
        };
        for ordered in found {
            ordered.write(func, shapes, names);
            made += 1;
        }
    }
    made
}

/// A side of a comparison: a register, or a constant where the instruction had one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Reg(mir::Operand),
    Imm(i64),
}

impl Side {
    fn same(self, other: Self) -> bool {
        match (self, other) {
            (Side::Reg(a), Side::Reg(b)) => a.reg == b.reg,
            (Side::Imm(a), Side::Imm(b)) => a == b,
            _ => false,
        }
    }
}

/// One comparison of halves: the condition and its two sides.
#[derive(Debug, Clone, Copy)]
struct Half {
    condition: &'static str,
    left: Side,
    right: Side,
}

impl Half {
    /// The same comparison with its two sides swapped.
    fn turned(self) -> Self {
        Half { condition: turned(self.condition), left: self.right, right: self.left }
    }
}

/// What an ordered comparison of pairs becomes, and the instructions it replaces.
struct Ordered {
    /// The `or`, which the new instructions go in front of.
    or: mir::Inst,
    answer: mir::Operand,
    width: &'static str,
    /// The pair subtracted from, low and high, and the pair subtracted.
    from: (mir::Operand, mir::Operand),
    by: (Side, Side),
    /// The `setcc` that reads the flags the `sbb` left.
    set: &'static str,
    gone: [mir::Inst; 5],
}

impl Ordered {
    fn write(self, func: &mut mir::Func, shapes: &MachineInsts, names: &mut Interner) {
        let w = self.width;
        let mut op =
            |name: String| mir::Opcode::new(names.intern(&format!("{}{name}", shapes.prefix)));
        let span = func.span(self.or);
        let (from_low, from_high) = self.from;
        let cmp = match self.by.0 {
            Side::Reg(by) => {
                func.build_loose(op(format!("cmp_rr_{w}"))).operand(from_low).operand(by)
            }
            Side::Imm(k) => func.build_loose(op(format!("cmp_ri_{w}"))).operand(from_low).imm(k),
        }
        .at(span)
        .finish();
        let class = func.class_of(from_high.reg).expect("a half is a virtual register");
        let copy = func.new_vreg(class);
        let into = mir::Operand::write(copy, class).with(Constraint::Reuse(1));
        let from_high = mir::Operand::read(from_high.reg, class);
        let sbb = match self.by.1 {
            Side::Reg(by) => func
                .build_loose(op(format!("sbb_rr_{w}")))
                .operand(into)
                .operand(from_high)
                .operand(by),
            Side::Imm(k) => {
                func.build_loose(op(format!("sbb_ri_{w}"))).operand(into).operand(from_high).imm(k)
            }
        }
        .at(span)
        .finish();
        let answer = mir::Operand::write(self.answer.reg, self.answer.class);
        let set = func.build_loose(op(self.set.to_owned())).operand(answer).at(span).finish();
        for inst in [cmp, sbb, set] {
            func.insert_before(self.or, inst);
        }
        for inst in self.gone {
            func.remove_inst(inst);
        }
    }
}

struct Look<'a> {
    func: &'a mir::Func,
    shapes: &'a MachineInsts,
    names: &'a Interner,
    writer: &'a Map<mir::Reg, mir::Inst>,
    reads: &'a Map<mir::Reg, usize>,
    at: &'a Map<mir::Inst, usize>,
}

impl Look<'_> {
    fn name(&self, inst: mir::Inst) -> Option<&str> {
        self.names.resolve(self.func[inst].opcode.name()).strip_prefix(self.shapes.prefix)
    }

    fn operands(&self, inst: mir::Inst) -> &[mir::Operand] {
        &self.func[self.func[inst].operands]
    }

    /// The instruction in this block, ahead of `before`, that writes a register nothing else reads.
    fn alone(&self, reg: mir::Reg, before: mir::Inst) -> Option<mir::Inst> {
        let inst = *self.writer.get(&reg)?;
        let here = *self.at.get(&inst)?;
        (reg.is_virtual() && self.reads.get(&reg) == Some(&1) && here < self.at[&before])
            .then_some(inst)
    }

    /// The comparison of halves an instruction is, with the width it compares at.
    fn half(&self, inst: mir::Inst) -> Option<(Half, &'static str)> {
        let rest = self.name(inst)?.strip_prefix("cmp_set_")?;
        let (condition, rest) = rest.split_once('_')?;
        let condition = CONDITIONS.iter().copied().find(|&c| c == condition)?;
        let (constant, width) = match rest.split_once('_') {
            Some(("ri", width)) => (true, width),
            None => (false, rest),
            _ => return None,
        };
        let width = ["32", "64"].into_iter().find(|&w| w == width)?;
        let ops = self.operands(inst);
        let half = match (constant, ops) {
            (false, &[_, left, right]) => {
                Half { condition, left: Side::Reg(left), right: Side::Reg(right) }
            }
            (true, &[_, left]) => {
                let k = self.func[self.func[inst].imm?].0;
                Half { condition, left: Side::Reg(left), right: Side::Imm(k) }
            }
            _ => return None,
        };
        let reads = [half.left, half.right];
        reads
            .iter()
            .all(|side| match side {
                Side::Reg(operand) => operand.role == Role::Use && operand.reg.is_virtual(),
                Side::Imm(_) => true,
            })
            .then_some((half, width))
    }

    /// The two byte operands of an `and` or an `or` of bytes, when it is one.
    fn bytes(
        &self,
        inst: mir::Inst,
        of: &str,
    ) -> Option<(mir::Operand, mir::Operand, mir::Operand)> {
        if self.name(inst)? != format!("{of}_rr_8") {
            return None;
        }
        let &[answer, first, second] = self.operands(inst) else { return None };
        (first.role == Role::Use && second.role == Role::Use).then_some((answer, first, second))
    }

    fn ordered(&self, or: mir::Inst) -> Option<Ordered> {
        let (answer, first, second) = self.bytes(or, "or")?;
        [(first, second), (second, first)].into_iter().find_map(|(above, tail)| {
            let above_at = self.alone(above.reg, or)?;
            let tail_at = self.alone(tail.reg, or)?;
            let (_, x, y) = self.bytes(tail_at, "and")?;
            [(x, y), (y, x)].into_iter().find_map(|(same, below)| {
                let same_at = self.alone(same.reg, tail_at)?;
                let below_at = self.alone(below.reg, tail_at)?;
                let insts = [above_at, same_at, below_at, tail_at, or];
                self.shape(answer, insts)
            })
        })
    }

    /// Whether the three comparisons are one ordered comparison of pairs, and what it becomes.
    fn shape(&self, answer: mir::Operand, insts: [mir::Inst; 5]) -> Option<Ordered> {
        let [above_at, same_at, below_at, ..] = insts;
        let (mut above, width) = self.half(above_at)?;
        let (same, same_width) = self.half(same_at)?;
        let (mut below, below_width) = self.half(below_at)?;
        if same.condition != "e" || same_width != width || below_width != width {
            return None;
        }
        // The equality is of the two sides the high comparison has, in either order.
        let straight = above.left.same(same.left) && above.right.same(same.right);
        let crossed = above.left.same(same.right) && above.right.same(same.left);
        if !straight && !crossed {
            return None;
        }
        // Strict at the top, both ways round, and the bottom unsigned and the same way round.
        let (less, signed) = match above.condition {
            "b" => (true, false),
            "a" => (false, false),
            "l" => (true, true),
            "g" => (false, true),
            _ => return None,
        };
        if !less {
            above = above.turned();
        }
        if matches!(below.condition, "a" | "ae") {
            below = below.turned();
        }
        if !matches!(below.condition, "b" | "be") {
            return None;
        }
        // Now `above.left:below.left < above.right:below.right` with the bottom strict when
        // `below` is, and its negation `right <= left` written the other way round when it is not.
        let strict = below.condition == "b";
        let (from, by) = if strict {
            ((below.left, above.left), (below.right, above.right))
        } else {
            ((below.right, above.right), (below.left, above.left))
        };
        // A constant is not something to subtract from, so `k < x` is asked as `x >= k + 1` and
        // `x <= k` as `x < k + 1`, which subtract from `x`. Not when `k` is the largest there is.
        let (strict, from, by) = match from {
            (Side::Imm(low), Side::Imm(high)) => {
                let (low, high) = next(low, high, width, signed)?;
                (!strict, by, (Side::Imm(low), Side::Imm(high)))
            }
            _ => (strict, from, by),
        };
        let (Side::Reg(from_low), Side::Reg(from_high)) = from else { return None };
        let set = match (strict, signed) {
            (true, false) => "set_b",
            (true, true) => "set_l",
            (false, false) => "set_ae",
            (false, true) => "set_ge",
        };
        let w = width;
        let needed = [
            format!("cmp_{}_{w}", if matches!(by.0, Side::Imm(_)) { "ri" } else { "rr" }),
            format!("sbb_{}_{w}", if matches!(by.1, Side::Imm(_)) { "ri" } else { "rr" }),
            set.to_owned(),
        ];
        if !needed.iter().all(|name| (self.shapes.operands)(name).is_some()) {
            return None;
        }
        // A constant that does not fit the instruction's immediate is not one it can take.
        let fits = |side: Side| match side {
            Side::Imm(k) => i32::try_from(k).is_ok(),
            Side::Reg(_) => true,
        };
        if !fits(by.0) || !fits(by.1) {
            return None;
        }
        Some(Ordered {
            or: insts[4],
            answer,
            width,
            from: (from_low, from_high),
            by,
            set,
            gone: insts,
        })
    }
}

/// The pair one more than `high:low` at that width, with each half as an immediate of the width
/// says it, or nothing when there is no larger pair.
fn next(low: i64, high: i64, width: &str, signed: bool) -> Option<(i64, i64)> {
    let mask = if width == "32" { u64::from(u32::MAX) } else { u64::MAX };
    let bits = |k: i64| u64::from_ne_bytes(k.to_ne_bytes()) & mask;
    let back = |k: u64| match u32::try_from(k) {
        Ok(k) if width == "32" => i64::from(i32::from_ne_bytes(k.to_ne_bytes())),
        _ => i64::from_ne_bytes(k.to_ne_bytes()),
    };
    let (low, high) = (bits(low), bits(high));
    if low != mask {
        return Some((back(low + 1), back(high)));
    }
    let top = if signed { mask >> 1 } else { mask };
    (high != top).then(|| (back(0), back((high + 1) & mask)))
}

const CONDITIONS: [&str; 10] = ["e", "ne", "l", "le", "g", "ge", "b", "be", "a", "ae"];

/// The condition that holds of `b ? a` when this one holds of `a ? b`.
fn turned(condition: &'static str) -> &'static str {
    match condition {
        "l" => "g",
        "g" => "l",
        "le" => "ge",
        "ge" => "le",
        "b" => "a",
        "a" => "b",
        "be" => "ae",
        "ae" => "be",
        other => other,
    }
}
