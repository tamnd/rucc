//! The quotient and the remainder of the same division from the one `div` that works out both.
//!
//! A `div` leaves the quotient in `rax` and the remainder in `rdx`, and selection writes `x / y`
//! and `x % y` as an instruction each, which keeps one of the two answers and throws the other
//! away. So a program that asks for both divides twice. gcc divides once and reads both registers,
//! and a loop that prints a number a digit at a time is this: `x % 10` is the digit and `x / 10`
//! is what is left. FreeBSD's `boot2` prints that way and has to fit in 7680 bytes, and the second
//! division is ten bytes of it.
//!
//! # How
//!
//! Each of the two instructions already names both registers, the one it keeps as its answer and
//! the other as a register it destroys. So the earlier one keeps its answer, the register it used
//! to destroy becomes the answer of the later one, and the later one goes. Nothing is put in a new
//! place: each answer is then written no later than it was, and both read the same two registers,
//! which were written before the earlier one.
//!
//! Sixteen, thirty two and sixty four bits. The eight bit division puts its remainder in `ah` and
//! the remainder instruction moves it out, so it is not the same register in both and is left
//! alone.
//!
//! # Why here
//!
//! Straight after selection, for the reason [`crate::carry`] gives: a virtual register is written
//! once, so the two instructions can be seen to read the same two registers by name.

use rucc_base::Interner;
use rucc_base::hash::Set;
use rucc_mir as mir;
use rucc_target::{MachineInsts, Role};

/// Gives every remainder of a division that is also asked for its quotient in the same block,
/// or the other way round, to the one instruction, and says how many it took away.
pub fn pairs(func: &mut mir::Func, shapes: &MachineInsts, names: &Interner) -> usize {
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
    let mut gone = Vec::new();
    for block in func.blocks().collect::<Vec<_>>() {
        let order: Vec<mir::Inst> = func.insts(block).collect();
        let mut taken = Set::default();
        for (n, &first) in order.iter().enumerate() {
            if taken.contains(&first) {
                continue;
            }
            let Some(kind) = division(func, shapes, names, first) else { continue };
            let &[_, other, dividend, divisor] = &func[func[first].operands] else { continue };
            if read.contains(&other.reg) || !dividend.reg.is_virtual() || !divisor.reg.is_virtual()
            {
                continue;
            }
            let later = order[n + 1..].iter().copied().find(|&later| {
                !taken.contains(&later)
                    && division(func, shapes, names, later) == Some(kind.other())
                    && matches!(&func[func[later].operands], &[answer, _, d, s]
                        if answer.reg.is_virtual() && d.reg == dividend.reg && s.reg == divisor.reg)
            });
            let Some(later) = later else { continue };
            taken.insert(later);
            let answer = func[func[later].operands][0].reg;
            let operands = func[first].operands;
            func[operands][1].reg = answer;
            gone.push(later);
        }
    }
    for &inst in &gone {
        func.remove_inst(inst);
    }
    gone.len()
}

/// A division at a width whose two answers are in `rax` and `rdx`, and which of them it keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Kind {
    signed: bool,
    remainder: bool,
    width: &'static str,
}

impl Kind {
    fn other(self) -> Kind {
        Kind { remainder: !self.remainder, ..self }
    }
}

fn division(
    func: &mir::Func,
    shapes: &MachineInsts,
    names: &Interner,
    inst: mir::Inst,
) -> Option<Kind> {
    let name = names.resolve(func[inst].opcode.name()).strip_prefix(shapes.prefix)?;
    let (signed, name) = match name.strip_prefix('i') {
        Some(rest) => (true, rest),
        None => (false, name),
    };
    let (keeps, width) = name.strip_prefix("div_")?.split_once('_')?;
    let remainder = match keeps {
        "quo" => false,
        "rem" => true,
        _ => return None,
    };
    let width = ["16", "32", "64"].into_iter().find(|&known| known == width)?;
    Some(Kind { signed, remainder, width })
}
