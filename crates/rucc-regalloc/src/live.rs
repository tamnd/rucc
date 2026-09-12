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
//! A list of pieces per virtual register, one for each run of blocks the value is live over, and
//! the interval around them for anyone who only wants to know where a value starts and stops.
//!
//! The pieces are what it takes to say that a value live in one loop and live again in a later one
//! is not live in between. Both loops are in the same line of points, so an interval that covered
//! them both would cover everything laid out between them and every value in there would look like
//! it was competing for a register with one it never meets. Twelve such values in a row are twelve
//! registers gone on a machine that has twelve, which is how a function using half the machine
//! ended up spilling. tamnd/rucc#982.
//!
//! Being dead in a piece's hole means dead for good rather than dead for a while. A value is live
//! in a block when a use of it can still be reached from there, so a block it is not live in is
//! one that no execution reaching it ever reads the value again. That is what makes a hole safe to
//! hand to somebody else without splitting anything: whoever gets the register in there is not
//! borrowing it, and nothing has to be put back afterwards.
//!
//! Physical registers in the operands are not in the answer. Nothing writes one before allocation
//! except an instruction that must, and what a call destroys is a separate question that the ABI
//! lowering asks, so a pass that reads this is reading about the values the allocator places.
//!
//! # How it is computed
//!
//! Which values arrive live in each block and which leave live is a fixpoint over the blocks, run
//! backwards because liveness flows backwards, and it is a fixpoint rather than one pass because
//! a loop carries a value from the end of a block round to a block in front of it. The pieces then
//! come from one walk over the instructions, a block at a time.
//!
//! What each block arrives holding is kept as the register numbers rather than as a bit each, and
//! `Rows` in this module says why. The short of it is that a block is live in a handful of values
//! whatever the function has in it, so a bit per value per block is the size of the function
//! squared for an answer that is not.
//!
//! Inside one block a value's live points are one stretch and never two, because the machine IR is
//! in SSA form and a value is written once. The stretch runs from the start of the block if the
//! value arrives live and from where it is written otherwise, and to the end of the block if it
//! leaves live and to its last read otherwise. Two stretches join into one piece when the blocks
//! they are in are next to each other in the line, which is what makes a value carried round a loop
//! one piece over the whole loop rather than one per block in it.

use std::cmp::Ordering;

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

/// Everywhere one value is live, which is one or more pieces and at most one more point in front
/// of the piece that follows it.
///
/// That one extra point is the only thing about a live area anybody adjusts. A value a two address
/// instruction writes into a register it read is really live from where that instruction reads its
/// operands, which is one point in front of where it is written, and both the allocator and the
/// checker add that point before asking anything. It is one point rather than a new start because
/// a value can be live in several pieces and the one to stretch is the piece the instruction
/// writes, which is not always the first. Reading an area this way only ever makes it bigger, so
/// it is still an area and every answer below still holds of it.
#[derive(Debug, Clone, Copy)]
pub struct Area<'a> {
    pieces: &'a [Range],
    also: Option<Point>,
}

impl<'a> Area<'a> {
    /// The same area with one more point in it, joined to the piece that starts just after it.
    ///
    /// A point already inside a piece changes nothing, which is what a value a loop carries round
    /// looks like: it is live on the way into the instruction that writes it anyway.
    #[must_use]
    pub fn with(self, point: Point) -> Self {
        Self { also: Some(point), ..self }
    }

    /// The interval around the whole area, holes and all, which is what a sweep in the order
    /// values start reads.
    #[must_use]
    pub fn hull(self) -> Range {
        Range { start: self.piece(0).start, end: self.pieces[self.pieces.len() - 1].end }
    }

    /// Whether the value is live at that point.
    #[must_use]
    pub fn covers(self, point: Point) -> bool {
        (0..self.pieces.len()).any(|piece| self.piece(piece).covers(point))
    }

    /// Whether two values are both live somewhere, which is what stops them sharing a register.
    ///
    /// Both lists are in order and neither is long, so this walks them together and stops at the
    /// first pair that touches rather than comparing every piece with every other.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        let (mut mine, mut theirs) = (0, 0);
        while mine < self.pieces.len() && theirs < other.pieces.len() {
            let (one, two) = (self.piece(mine), other.piece(theirs));
            if one.overlaps(two) {
                return true;
            }
            // Whichever stops first cannot reach anything further along the other list.
            if one.end < two.end {
                mine += 1;
            } else {
                theirs += 1;
            }
        }
        false
    }

    /// The pieces themselves, in order.
    pub fn pieces(self) -> impl Iterator<Item = Range> + 'a {
        (0..self.pieces.len()).map(move |piece| self.piece(piece))
    }

    /// One piece, stretched down over the extra point when that point is the one just in front of
    /// it.
    fn piece(self, index: usize) -> Range {
        let piece = self.pieces[index];
        match self.also {
            Some(also) if also + 1 == piece.start => Range { start: also, end: piece.end },
            _ => piece,
        }
    }
}

/// What is live where.
#[derive(Debug, Clone)]
pub struct Live {
    live_in: Rows,
    live_out: Rows,
    /// Every value's pieces end to end, since a vector per value would be a vector per value.
    pieces: Vec<Range>,
    /// Where each value's pieces are in that vector, by register number.
    spans: Vec<(usize, usize)>,
}

impl Live {
    /// Works it out for a function laid out in that order.
    #[must_use]
    pub fn of(func: &Func, order: &Order) -> Self {
        let vregs = func.vregs();
        let (used, defined) = exposed(func, order);
        let (live_in, live_out) = flow(func, order, &used, &defined);
        let (pieces, spans) = carve(func, order, &live_in, &live_out, vregs);
        Self { live_in, live_out, pieces, spans }
    }

    /// Everywhere a virtual register is live, or `None` for one this function never mentions and
    /// for a physical register.
    #[must_use]
    pub fn area(&self, reg: Reg) -> Option<Area<'_>> {
        let pieces = self.pieces(reg);
        if pieces.is_empty() {
            return None;
        }
        Some(Area { pieces, also: None })
    }

    /// The interval a virtual register is live over, holes and all.
    #[must_use]
    pub fn range(&self, reg: Reg) -> Option<Range> {
        self.area(reg).map(Area::hull)
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

    /// Everywhere a virtual register is live, as it is stored.
    fn pieces(&self, reg: Reg) -> &[Range] {
        let number = reg.number().and_then(|number| usize::try_from(number).ok());
        let Some(&(from, to)) = number.and_then(|number| self.spans.get(number)) else {
            return &[];
        };
        &self.pieces[from..to]
    }
}

/// The pieces, from the blocks and from the instructions in them.
///
/// One block at a time, because a value's live points inside one block are one stretch and the
/// whole job is working out where one stretch stops and the next begins. What comes back is every
/// value's pieces end to end, and where each value's are.
fn carve(
    func: &Func,
    order: &Order,
    live_in: &Rows,
    live_out: &Rows,
    vregs: usize,
) -> (Vec<Range>, Vec<(usize, usize)>) {
    let mut lists: Vec<Vec<Range>> = vec![Vec::new(); vregs];
    let mut here: Vec<Option<Range>> = vec![None; vregs];
    let mut touched: Vec<usize> = Vec::new();

    for &block in order.blocks() {
        // A block a value arrives in and leaves is one it is live through, whether or not
        // anything in it says the value's name.
        for reg in live_in.iter(block.index()) {
            note(&mut here, &mut touched, reg, order.start(block));
        }
        for reg in live_out.iter(block.index()) {
            note(&mut here, &mut touched, reg, order.end(block));
        }
        for param in &func[block].params {
            note(&mut here, &mut touched, param.reg, order.start(block));
        }
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                match operand.role {
                    Role::Use => note(&mut here, &mut touched, operand.reg, order.early(inst)),
                    Role::Def => note(&mut here, &mut touched, operand.reg, order.late(inst)),
                    // A register written early is taken from before the operands are read, which
                    // is the whole of what makes it different from a plain definition, and it is
                    // still taken when the instruction is done. Both ends have to be said. Saying
                    // only the first would leave a value nothing reads live at a point in front of
                    // everything else the instruction writes, and the register it went to would
                    // look free to them.
                    Role::EarlyDef => {
                        note(&mut here, &mut touched, operand.reg, order.early(inst));
                        note(&mut here, &mut touched, operand.reg, order.late(inst));
                    }
                }
            }
        }
        for call in &func[block].succs {
            for &arg in &call.args {
                note(&mut here, &mut touched, arg, order.end(block));
            }
        }

        for &number in &touched {
            let Some(piece) = here[number].take() else { continue };
            match lists[number].last_mut() {
                // The points run on from one block into the next, so a stretch that begins where
                // the last one stopped is the same run of blocks carried on. A gap of even one
                // point means a block in between that the value is not live in.
                Some(last) if last.end + 1 == piece.start => last.end = piece.end,
                _ => lists[number].push(piece),
            }
        }
        touched.clear();
    }

    let mut pieces = Vec::new();
    let mut spans = Vec::with_capacity(vregs);
    for list in &lists {
        let from = pieces.len();
        pieces.extend_from_slice(list);
        spans.push((from, pieces.len()));
    }
    (pieces, spans)
}

/// Says that a value is live at a point of the block being carved.
fn note(here: &mut [Option<Range>], touched: &mut Vec<usize>, reg: Reg, point: Point) {
    let Some(number) = reg.number().and_then(|number| usize::try_from(number).ok()) else {
        return;
    };
    let Some(slot) = here.get_mut(number) else { return };
    match slot {
        Some(range) => *range = range.with(point),
        None => {
            *slot = Some(Range { start: point, end: point });
            touched.push(number);
        }
    }
}

/// What each block reads before writing, and what it writes.
///
/// The first is read backwards, because a value a block writes and then reads is one it does not
/// want from anybody, while one it reads and then writes is.
fn exposed(func: &Func, order: &Order) -> (Rows, Rows) {
    let vregs = func.vregs();
    let mut used = Rows::new(func.block_count());
    let mut defined = Rows::new(func.block_count());
    let mut reads = Building::new(vregs);
    let mut writes = Building::new(vregs);
    for &block in order.blocks() {
        let row = block.index();
        for call in &func[block].succs {
            for &arg in &call.args {
                reads.insert(arg);
            }
        }
        let insts: Vec<_> = func.insts(block).collect();
        for &inst in insts.iter().rev() {
            let operands = &func[func[inst].operands];
            for operand in operands.iter().filter(|operand| operand.role.is_def()) {
                reads.remove(operand.reg);
                writes.insert(operand.reg);
            }
            for operand in operands.iter().filter(|operand| !operand.role.is_def()) {
                reads.insert(operand.reg);
            }
        }
        for param in &func[block].params {
            reads.remove(param.reg);
            writes.insert(param.reg);
        }
        used.set(row, &reads.take());
        defined.set(row, &writes.take());
    }
    (used, defined)
}

/// The fixpoint: what arrives live in each block, and what leaves live.
fn flow(func: &Func, order: &Order, used: &Rows, defined: &Rows) -> (Rows, Rows) {
    let mut live_in = Rows::new(func.block_count());
    let mut live_out = Rows::new(func.block_count());
    let (mut out, mut scratch, mut rest, mut next) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut changed = true;
    while changed {
        changed = false;
        for &block in order.blocks().iter().rev() {
            let row = block.index();
            out.clear();
            for call in &func[block].succs {
                union(&out, live_in.row(call.block.index()), &mut scratch);
                std::mem::swap(&mut out, &mut scratch);
            }
            without(&out, defined.row(row), &mut rest);
            union(used.row(row), &rest, &mut next);
            if live_in.row(row) != next.as_slice() {
                live_in.set(row, &next);
                changed = true;
            }
            live_out.set(row, &out);
        }
    }
    (live_in, live_out)
}

/// Everything in either list, in order, into a buffer the caller keeps.
///
/// Both are sorted and neither holds a number twice, so this is one walk of the two together
/// rather than a concatenation and a sort.
fn union(one: &[u32], two: &[u32], out: &mut Vec<u32>) {
    out.clear();
    let (mut here, mut there) = (0, 0);
    while here < one.len() && there < two.len() {
        match one[here].cmp(&two[there]) {
            Ordering::Less => {
                out.push(one[here]);
                here += 1;
            }
            Ordering::Greater => {
                out.push(two[there]);
                there += 1;
            }
            Ordering::Equal => {
                out.push(one[here]);
                here += 1;
                there += 1;
            }
        }
    }
    out.extend_from_slice(&one[here..]);
    out.extend_from_slice(&two[there..]);
}

/// Everything in the first list that is not in the second, in order.
fn without(one: &[u32], two: &[u32], out: &mut Vec<u32>) {
    out.clear();
    let mut there = 0;
    for &number in one {
        while there < two.len() && two[there] < number {
            there += 1;
        }
        if there < two.len() && two[there] == number {
            continue;
        }
        out.push(number);
    }
}

/// A set of virtual registers for each block, held as the numbers in it.
///
/// A bit per register per block is the obvious way to hold this and is what it was. The trouble is
/// that a row is then as wide as the function has values however few of them the block is about,
/// and every step of the fixpoint reads and writes every word of every row. A function with a lot
/// of values in it has a lot of blocks too, so that is the size of the function squared, in memory
/// as well as in time: jtckdint from the real corpus has one function with 190084 instructions and
/// 22000 blocks, and four of these rows came to about two gigabytes of the compiler's footprint,
/// with the fixpoint over them taking a third of the whole compile at `-O1`.
///
/// What is actually true of the answer is that a block is live in a handful of values and not in
/// the other two hundred thousand, so the numbers themselves are smaller than the bits. They are
/// kept in order, which is what makes the union and the difference the fixpoint needs one walk of
/// two lists rather than a search per element, and it is the order a register number sorts in
/// rather than any order of the program. tamnd/rucc#1072.
#[derive(Debug, Clone)]
struct Rows {
    rows: Vec<Vec<u32>>,
}

impl Rows {
    fn new(rows: usize) -> Self {
        Self { rows: vec![Vec::new(); rows] }
    }

    fn row(&self, row: usize) -> &[u32] {
        &self.rows[row]
    }

    /// Puts the numbers in the row, keeping whatever the row had already allocated, since the
    /// fixpoint writes every row once a round and a set that grew by one would otherwise be a set
    /// that allocated again.
    fn set(&mut self, row: usize, numbers: &[u32]) {
        let row = &mut self.rows[row];
        row.clear();
        row.extend_from_slice(numbers);
    }

    fn iter(&self, row: usize) -> impl Iterator<Item = Reg> + '_ {
        self.rows[row].iter().copied().map(Reg::virtual_reg)
    }
}

/// One block's set while it is being worked out, as a flag per register and a list of which to
/// look at.
///
/// A row is held as the numbers in it, so putting a register into one twice would be a search and
/// a shift of everything above it, and taking one out again would be another. Here both are a load
/// and a store. What makes it affordable is the clear: the flags are as many as the function has
/// values and the blocks are as many as it has blocks, so clearing all of the first for each of
/// the second would be the cost this whole representation is here to avoid, and instead only what
/// was set is walked. tamnd/rucc#1072.
struct Building {
    flags: Vec<bool>,
    /// Every number set since the last [`Building::take`], which may name one twice when a
    /// register was taken out and put back. The take drops the repeat rather than the caller
    /// having to care.
    touched: Vec<u32>,
}

impl Building {
    fn new(vregs: usize) -> Self {
        Self { flags: vec![false; vregs], touched: Vec::new() }
    }

    /// The register's number, or nothing for a physical register, which this does not track, and
    /// nothing for a number this function has no value at, which cannot happen and is not worth a
    /// panic if it does.
    fn number(&self, reg: Reg) -> Option<usize> {
        let number = usize::try_from(reg.number()?).ok()?;
        (number < self.flags.len()).then_some(number)
    }

    fn insert(&mut self, reg: Reg) {
        let Some(number) = self.number(reg) else { return };
        if !self.flags[number] {
            self.flags[number] = true;
            self.touched.push(u32::try_from(number).expect("a register number"));
        }
    }

    fn remove(&mut self, reg: Reg) {
        if let Some(number) = self.number(reg) {
            self.flags[number] = false;
        }
    }

    /// What is in the set, in order, leaving it empty for the next block.
    fn take(&mut self) -> Vec<u32> {
        let flags = &mut self.flags;
        let mut out: Vec<u32> = self
            .touched
            .drain(..)
            .filter(|&number| std::mem::replace(&mut flags[number as usize], false))
            .collect();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Constraint, Opcode, Operand};
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
    fn a_block_the_value_never_reaches_is_a_hole_between_two_pieces() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let arm = func.create_block();
        let tail = func.create_block();
        let value = func.new_vreg(GPR);
        let write = func.build(entry, opcode).def(value, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(arm), BlockCall::to(tail)];
        let idle = func.build(arm, opcode).finish();
        let read = func.build(tail, opcode).uses(value, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let area = live.area(value).expect("live somewhere");
        // The arm is written between the two blocks the value is live in, so the interval around
        // it covers the arm and the pieces do not. Both are true and they answer different
        // questions, and it is the pieces that decide who may have a register.
        assert!(live.range(value).expect("live somewhere").covers(order.early(idle)));
        assert!(!area.covers(order.early(idle)));
        assert_eq!(
            area.pieces().collect::<Vec<_>>(),
            vec![
                Range { start: order.late(write), end: order.end(entry) },
                Range { start: order.start(tail), end: order.early(read) },
            ]
        );
    }

    #[test]
    fn a_value_in_a_hole_of_another_may_have_its_register() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let arm = func.create_block();
        let tail = func.create_block();
        let value = func.new_vreg(GPR);
        let inside = func.new_vreg(GPR);
        func.build(entry, opcode).def(value, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(arm), BlockCall::to(tail)];
        func.build(arm, opcode).def(inside, GPR).finish();
        func.build(arm, opcode).uses(inside, GPR).finish();
        func.build(tail, opcode).uses(value, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let value = live.area(value).expect("live somewhere");
        let inside = live.area(inside).expect("live somewhere");
        // Nothing in the arm can reach the read in the tail, so whichever register the first value
        // is in is a register the arm may take for as long as it likes. The intervals say the two
        // are on top of each other and they are not.
        assert!(value.hull().overlaps(inside.hull()));
        assert!(!value.overlaps(inside));
        assert!(!inside.overlaps(value));
    }

    #[test]
    fn one_point_added_in_front_of_a_piece_is_part_of_the_area() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let write = func.build(block, opcode).def(first, GPR).finish();
        let both = func.build(block, opcode).def(second, GPR).uses(first, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let first = live.area(first).expect("live somewhere");
        let second = live.area(second).expect("live somewhere");
        // A two address instruction writes its answer into the register it read, so the answer is
        // really in that register from the moment the instruction starts. Read that way the two
        // values are on top of each other, and read the plain way they are not, which is the whole
        // reason the extra point is the caller's to add.
        assert!(!first.overlaps(second));
        assert!(first.overlaps(second.with(order.early(both))));
        assert!(second.with(order.early(both)).covers(order.early(both)));
        assert_eq!(second.with(order.early(both)).hull().start, order.early(both));
        assert_eq!(first.hull().start, order.late(write));
    }

    #[test]
    fn the_point_added_in_front_joins_the_piece_it_belongs_to_and_not_the_first_one() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let add = Opcode::new(names.intern("x64.add"));
        let entry = func.create_block();
        let head = func.create_block();
        let arm = func.create_block();
        let latch = func.create_block();
        let out = func.create_block();
        let seed = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        let inside = func.new_vreg(GPR);
        let loaded = func.new_vreg(GPR);
        func.build(entry, nop).def(seed, GPR).finish();
        func.build(entry, nop).def(sum, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(head)];
        func.build(head, nop).uses(sum, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(arm), BlockCall::to(latch)];
        func.build(arm, nop).def(inside, GPR).finish();
        func.build(arm, nop).uses(inside, GPR).finish();
        *func.succs_mut(arm) = vec![BlockCall::to(out)];
        func.build(latch, nop).def(loaded, GPR).finish();
        let carry = func
            .build(latch, add)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(seed, GPR)
            .uses(loaded, GPR)
            .finish();
        *func.succs_mut(latch) = vec![BlockCall::to(head), BlockCall::to(out)];

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let sum = live.area(sum).expect("live somewhere");
        let loaded = live.area(loaded).expect("live somewhere");
        // The answer is live in the entry and the head as well, which the arm is a hole in, so the
        // piece the addition writes is the second one. Adding the point in front of the first piece
        // instead would leave the addition reading a register the answer is about to be written to
        // and nothing saying the two are on top of each other. tamnd/rucc#982.
        assert_eq!(sum.pieces().count(), 2);
        assert!(!sum.covers(order.early(carry)));
        assert!(sum.with(order.early(carry)).covers(order.early(carry)));
        assert!(!loaded.overlaps(sum));
        assert!(loaded.overlaps(sum.with(order.early(carry))));
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
