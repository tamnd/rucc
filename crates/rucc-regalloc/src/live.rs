//! Where every value in a machine function is live.
//!
//! Design: `spec/10-backend.md` section 10.4.
//!
//! A register can be given to two values at once exactly when the two are never both wanted, so
//! this is the question every allocator asks first and the one both of ours will read the answer
//! to from here. It is asked of the machine IR while it is still in SSA form, which is what makes
//! the answer cheap: a value is written once, so its live range is one interval from where it is
//! written to the last place it is read, and there is no need to ask which of several definitions
//! a use is reading from.
//!
//! # What the answer is
//!
//! One interval per virtual register, with no holes in it. A value that is dead in the middle of
//! its range is treated as live there, which costs a register the allocator could have handed out
//! and never claims one is free when it is not. Holes are what the backtracking allocator will
//! want and it will want a different structure to hold them in, since a range it can split is a
//! range with a list of pieces rather than two numbers.
//!
//! Physical registers in the operands are not in the answer. Nothing writes one before allocation
//! except an instruction that must, and what a call destroys is a separate question that the ABI
//! lowering asks, so a pass that reads this is reading about the values the allocator places.
//!
//! # How it is computed
//!
//! Which values arrive live in each block and which leave live is a fixpoint over the blocks, run
//! backwards because liveness flows backwards, and it is a fixpoint rather than one pass because
//! a loop carries a value from the end of a block round to a block in front of it. The intervals
//! then come from one walk over the instructions. A block a value is live through contributes the
//! whole of that block, which is what makes the interval cover the loop rather than stopping at
//! the last instruction that mentions it.

use rucc_mir::{Block, Func, Reg, Role};

use crate::order::{Order, Point};

/// The stretch of the function a value is live over.
///
/// Both ends are included: a value written at a point and read at a later one is live at both,
/// and one written and never read is live where it was written, because the register it was
/// written to is not free at the instant it was written to. A value written early is written
/// before the instruction reads its operands and is still written when the instruction is done,
/// so even one nothing reads covers the whole of the instruction that wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// Where the value is written.
    pub start: Point,
    /// The last place it is read, or where it is written if nothing reads it.
    pub end: Point,
}

impl Range {
    /// Whether the value is live at that point.
    #[must_use]
    pub fn covers(self, point: Point) -> bool {
        self.start <= point && point <= self.end
    }

    /// Whether two values are both live anywhere, which is what stops them sharing a register.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    /// The smallest range covering both, which is how a range grows as more of the function is
    /// read.
    fn with(self, point: Point) -> Self {
        Self { start: self.start.min(point), end: self.end.max(point) }
    }
}

/// What is live where.
#[derive(Debug, Clone)]
pub struct Live {
    live_in: Rows,
    live_out: Rows,
    ranges: Vec<Option<Range>>,
}

impl Live {
    /// Works it out for a function laid out in that order.
    #[must_use]
    pub fn of(func: &Func, order: &Order) -> Self {
        let vregs = func.vregs();
        let (used, defined) = exposed(func, order);
        let (live_in, live_out) = flow(func, order, &used, &defined);
        let ranges = measure(func, order, &live_in, &live_out, vregs);
        Self { live_in, live_out, ranges }
    }

    /// Where a virtual register is live, or `None` for one this function never mentions and for
    /// a physical register.
    #[must_use]
    pub fn range(&self, reg: Reg) -> Option<Range> {
        self.ranges.get(usize::try_from(reg.number()?).ok()?).copied().flatten()
    }

    /// Every virtual register that arrives in a block already holding a value.
    ///
    /// The block's own parameters are not among them. A parameter is written where it arrives,
    /// which makes it a value the block defines rather than one it inherits.
    pub fn live_in(&self, block: Block) -> impl Iterator<Item = Reg> + '_ {
        self.live_in.iter(block.index())
    }

    /// Every virtual register that is still wanted after a block, which is what its successors
    /// and the arguments its terminator carries between them ask for.
    pub fn live_out(&self, block: Block) -> impl Iterator<Item = Reg> + '_ {
        self.live_out.iter(block.index())
    }
}

/// The intervals, from the blocks and from the instructions in them.
fn measure(
    func: &Func,
    order: &Order,
    live_in: &Rows,
    live_out: &Rows,
    vregs: usize,
) -> Vec<Option<Range>> {
    let mut ranges: Vec<Option<Range>> = vec![None; vregs];
    let mut extend = |reg: Reg, point: Point| {
        let Some(number) = reg.number().and_then(|number| usize::try_from(number).ok()) else {
            return;
        };
        let Some(slot) = ranges.get_mut(number) else { return };
        *slot = Some(match *slot {
            Some(range) => range.with(point),
            None => Range { start: point, end: point },
        });
    };

    for &block in order.blocks() {
        // A block a value arrives in and leaves is one it is live through, whether or not
        // anything in it says the value's name.
        for reg in live_in.iter(block.index()) {
            extend(reg, order.start(block));
        }
        for reg in live_out.iter(block.index()) {
            extend(reg, order.end(block));
        }
        for param in &func[block].params {
            extend(param.reg, order.start(block));
        }
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                match operand.role {
                    Role::Use => extend(operand.reg, order.early(inst)),
                    Role::Def => extend(operand.reg, order.late(inst)),
                    // A register written early is taken from before the operands are read, which
                    // is the whole of what makes it different from a plain definition, and it is
                    // still taken when the instruction is done. Both ends have to be said. Saying
                    // only the first would leave a value nothing reads live at a point in front of
                    // everything else the instruction writes, and the register it went to would
                    // look free to them.
                    Role::EarlyDef => {
                        extend(operand.reg, order.early(inst));
                        extend(operand.reg, order.late(inst));
                    }
                }
            }
        }
        for call in &func[block].succs {
            for &arg in &call.args {
                extend(arg, order.end(block));
            }
        }
    }
    ranges
}

/// What each block reads before writing, and what it writes.
///
/// The first is read backwards, because a value a block writes and then reads is one it does not
/// want from anybody, while one it reads and then writes is.
fn exposed(func: &Func, order: &Order) -> (Rows, Rows) {
    let mut used = Rows::new(func.block_count(), func.vregs());
    let mut defined = Rows::new(func.block_count(), func.vregs());
    for &block in order.blocks() {
        let row = block.index();
        for call in &func[block].succs {
            for &arg in &call.args {
                used.insert(row, arg);
            }
        }
        let insts: Vec<_> = func.insts(block).collect();
        for &inst in insts.iter().rev() {
            let operands = &func[func[inst].operands];
            for operand in operands.iter().filter(|operand| operand.role.is_def()) {
                used.remove(row, operand.reg);
                defined.insert(row, operand.reg);
            }
            for operand in operands.iter().filter(|operand| !operand.role.is_def()) {
                used.insert(row, operand.reg);
            }
        }
        for param in &func[block].params {
            used.remove(row, param.reg);
            defined.insert(row, param.reg);
        }
    }
    (used, defined)
}

/// The fixpoint: what arrives live in each block, and what leaves live.
fn flow(func: &Func, order: &Order, used: &Rows, defined: &Rows) -> (Rows, Rows) {
    let mut live_in = Rows::new(func.block_count(), func.vregs());
    let mut live_out = Rows::new(func.block_count(), func.vregs());
    let width = live_in.width;
    let mut next = vec![0u64; width];
    let mut changed = true;
    while changed {
        changed = false;
        for &block in order.blocks().iter().rev() {
            let row = block.index();
            for call in &func[block].succs {
                let successor = call.block.index();
                for (word, &incoming) in
                    live_out.row_mut(row).iter_mut().zip(live_in.row(successor))
                {
                    *word |= incoming;
                }
            }
            for (index, word) in next.iter_mut().enumerate() {
                *word =
                    used.row(row)[index] | (live_out.row(row)[index] & !defined.row(row)[index]);
            }
            if live_in.row(row) != next.as_slice() {
                live_in.row_mut(row).copy_from_slice(&next);
                changed = true;
            }
        }
    }
    (live_in, live_out)
}

/// A set of virtual registers for each block.
#[derive(Debug, Clone)]
struct Rows {
    words: Vec<u64>,
    /// How many words one row is, which is at least one so that a row is a slice rather than
    /// nothing.
    width: usize,
}

impl Rows {
    fn new(rows: usize, columns: usize) -> Self {
        let width = columns.div_ceil(64).max(1);
        Self { words: vec![0; rows * width], width }
    }

    fn row(&self, row: usize) -> &[u64] {
        &self.words[row * self.width..(row + 1) * self.width]
    }

    fn row_mut(&mut self, row: usize) -> &mut [u64] {
        &mut self.words[row * self.width..(row + 1) * self.width]
    }

    /// The column a register is, or nothing for a physical one, which this does not track.
    fn column(&self, reg: Reg) -> Option<usize> {
        let number = usize::try_from(reg.number()?).ok()?;
        (number < self.width * 64).then_some(number)
    }

    fn insert(&mut self, row: usize, reg: Reg) {
        if let Some(column) = self.column(reg) {
            self.row_mut(row)[column / 64] |= 1 << (column % 64);
        }
    }

    fn remove(&mut self, row: usize, reg: Reg) {
        if let Some(column) = self.column(reg) {
            self.row_mut(row)[column / 64] &= !(1 << (column % 64));
        }
    }

    fn iter(&self, row: usize) -> impl Iterator<Item = Reg> + '_ {
        self.row(row).iter().enumerate().flat_map(|(word, &bits)| {
            (0..64).filter(move |bit| bits & (1 << bit) != 0).map(move |bit| {
                Reg::virtual_reg(u32::try_from(word * 64 + bit).expect("a register number"))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Opcode, Operand};
    use rucc_target::x86_64::GPR;

    use super::*;

    /// The registers live in or out of a block, in order, which is what an assertion reads.
    fn regs(of: impl Iterator<Item = Reg>) -> Vec<u32> {
        of.filter_map(Reg::number).collect()
    }

    #[test]
    fn a_value_is_live_from_where_it_is_written_to_where_it_is_last_read() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let value = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let write = func.build(block, opcode).def(value, GPR).finish();
        let idle = func.build(block, opcode).def(other, GPR).finish();
        let read = func.build(block, opcode).uses(value, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let range = live.range(value).expect("the value is live somewhere");
        assert_eq!(range, Range { start: order.late(write), end: order.early(read) });
        assert!(range.covers(order.early(idle)));
        // A value nothing reads is live where it was written and nowhere else, because the
        // register it went to was not free at that instant either.
        assert_eq!(
            live.range(other),
            Some(Range { start: order.late(idle), end: order.late(idle) })
        );
        assert!(!range.overlaps(Range { start: order.late(read), end: order.late(read) }));
        assert_eq!(regs(live.live_in(block)), Vec::<u32>::new());
    }

    #[test]
    fn a_value_read_in_another_block_is_live_between_them() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let middle = func.create_block();
        let tail = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(head, opcode).def(value, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(middle)];
        *func.succs_mut(middle) = vec![BlockCall::to(tail)];
        let read = func.build(tail, opcode).uses(value, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        // The block in between never mentions it and it is live all the way through, which is
        // the whole reason this is a fixpoint over the blocks and not a walk over the code.
        assert_eq!(regs(live.live_in(middle)), vec![0]);
        assert_eq!(regs(live.live_out(middle)), vec![0]);
        assert!(live.range(value).expect("live somewhere").covers(order.start(middle)));
        assert_eq!(live.range(value).expect("live somewhere").end, order.early(read));
    }

    #[test]
    fn a_value_carried_round_a_loop_is_live_round_all_of_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let header = func.create_block();
        let body = func.create_block();
        let carried = func.append_param(header, GPR);
        let next = func.new_vreg(GPR);
        *func.succs_mut(header) = vec![BlockCall::to(body)];
        func.build(body, opcode).def(next, GPR).uses(carried, GPR).finish();
        *func.succs_mut(body) = vec![BlockCall::with(header, vec![next])];

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        // The parameter arrives in the header, so the header does not want it from anybody, and
        // the body does.
        assert_eq!(regs(live.live_in(header)), Vec::<u32>::new());
        assert_eq!(regs(live.live_in(body)), vec![carried.number().expect("virtual")]);
        let range = live.range(next).expect("live somewhere");
        assert_eq!(range.end, order.end(body));
    }

    #[test]
    fn two_values_that_are_never_both_wanted_do_not_overlap() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let write = func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).uses(first, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let first = live.range(first).expect("live somewhere");
        let second = live.range(second).expect("live somewhere");
        // The second instruction reads the first value and writes its own, and it reads before
        // it writes, so the two can be the same register. That is what a two address instruction
        // needs to be true and it is a fact about the points rather than about the opcode.
        assert!(!first.overlaps(second));
        assert!(first.start > order.start(block));
        assert_eq!(first.start, order.late(write));
    }

    #[test]
    fn an_operand_written_early_is_wanted_where_the_operands_are_read() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let source = func.new_vreg(GPR);
        let early = func.new_vreg(GPR);
        func.build(block, opcode).def(source, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write_early(early, GPR))
            .operand(Operand::read(source, GPR))
            .finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let source = live.range(source).expect("live somewhere");
        let early = live.range(early).expect("live somewhere");
        // This is the difference between a division and an addition. The register the answer is
        // going to is destroyed before the divisor is read, so the divisor may not be in it.
        assert!(source.overlaps(early));
    }

    #[test]
    fn a_register_a_memory_operand_names_is_read_like_any_other() {
        use rucc_mir::Mem;

        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let address = func.new_vreg(GPR);
        let write = func.build(block, opcode).def(address, GPR).finish();
        let load = func.build(block, opcode).mem(Mem::at(Operand::read(address, GPR))).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        assert_eq!(
            live.range(address),
            Some(Range { start: order.late(write), end: order.early(load) })
        );
    }
}
