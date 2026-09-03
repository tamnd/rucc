//! The linear order the allocator works over, and the positions in it.
//!
//! Design: `spec/10-backend.md` section 10.4.
//!
//! A live range is an interval, and an interval needs the program laid out in a line. The line
//! is the function's own block order, because `spec/10-backend.md` section 10.3 says the `-O0`
//! path does no block layout beyond preserving the order it was given, and because that is the
//! order the encoder will write the blocks out in. An allocator that worked over some other
//! order would be allocating for a program nobody emits.
//!
//! # Where the points are
//!
//! Every instruction has two of them. An operand is read at the first and written at the second,
//! which is what makes a two address instruction possible at all: the register a value is read
//! from is free by the time the result is written, so the two can be the same register, and an
//! operand written early is written at the first point instead, where it collides with every
//! operand read there. That is the whole meaning of an early definition and it is the reason the
//! points come in pairs rather than one to an instruction.
//!
//! Every block has two more. The parameters arrive at the point in front of its first
//! instruction, and the arguments its terminator carries away are read at the point after its
//! last, which is where the moves that write the successor's parameters will go. Neither is an
//! instruction, and both are places where something is live.

use rucc_mir::{Block, Func, Inst};

/// A place in the function, counted along the line the blocks make.
pub type Point = u32;

/// The blocks in the order they are emitted in, and the point every part of them is at.
#[derive(Debug, Clone)]
pub struct Order {
    blocks: Vec<Block>,
    /// The point a block's parameters arrive at, by block index.
    start: Vec<Point>,
    /// The point a block's outgoing arguments are read at, by block index.
    end: Vec<Point>,
    /// The point an instruction reads at, by instruction index. It writes at the one after.
    early: Vec<Point>,
    points: Point,
}

impl Order {
    /// Lays a function out.
    #[must_use]
    pub fn of(func: &Func) -> Self {
        let mut order = Self {
            blocks: Vec::with_capacity(func.block_count()),
            start: vec![0; func.block_count()],
            end: vec![0; func.block_count()],
            early: vec![0; func.inst_count()],
            points: 0,
        };
        let mut point = 0;
        for block in func.blocks() {
            order.blocks.push(block);
            order.start[block.index()] = point;
            point += 1;
            for inst in func.insts(block) {
                order.early[inst.index()] = point;
                point += 2;
            }
            order.end[block.index()] = point;
            point += 1;
        }
        order.points = point;
        order
    }

    /// The blocks, in the order they are emitted in.
    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// How many points the function has, which is what a table indexed by point is sized
    /// against.
    #[must_use]
    pub fn points(&self) -> Point {
        self.points
    }

    /// Where a block's parameters arrive.
    #[must_use]
    pub fn start(&self, block: Block) -> Point {
        self.start[block.index()]
    }

    /// Where a block's outgoing arguments are read, which is after everything in it.
    #[must_use]
    pub fn end(&self, block: Block) -> Point {
        self.end[block.index()]
    }

    /// Where an instruction reads its operands, and where it writes the ones it writes early.
    #[must_use]
    pub fn early(&self, inst: Inst) -> Point {
        self.early[inst.index()]
    }

    /// Where an instruction writes its results.
    #[must_use]
    pub fn late(&self, inst: Inst) -> Point {
        self.early[inst.index()] + 1
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Opcode};

    use super::*;

    /// A function of two blocks, the first with two instructions in it and the second with one.
    fn func() -> (Func, Vec<Block>, Vec<Inst>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let tail = func.create_block();
        let first = func.build(head, opcode).finish();
        let second = func.build(head, opcode).finish();
        *func.succs_mut(head) = vec![BlockCall::to(tail)];
        let third = func.build(tail, opcode).finish();
        (func, vec![head, tail], vec![first, second, third])
    }

    #[test]
    fn the_order_is_the_one_the_function_is_written_in() {
        let (func, blocks, _) = func();
        assert_eq!(Order::of(&func).blocks(), blocks);
    }

    #[test]
    fn an_instruction_reads_before_it_writes() {
        let (func, _, insts) = func();
        let order = Order::of(&func);
        for &inst in &insts {
            assert!(order.early(inst) < order.late(inst));
        }
    }

    #[test]
    fn nothing_in_a_function_shares_a_point_with_anything_else() {
        let (func, blocks, insts) = func();
        let order = Order::of(&func);
        let mut points: Vec<Point> = Vec::new();
        for &block in &blocks {
            points.push(order.start(block));
            points.push(order.end(block));
        }
        for &inst in &insts {
            points.push(order.early(inst));
            points.push(order.late(inst));
        }
        points.sort_unstable();
        let count = points.len();
        points.dedup();
        assert_eq!(points.len(), count);
        assert_eq!(order.points(), Point::try_from(count).expect("a small function"));
    }

    #[test]
    fn a_block_holds_everything_in_it() {
        let (func, blocks, insts) = func();
        let order = Order::of(&func);
        for &block in &blocks {
            for inst in func.insts(block) {
                assert!(order.start(block) < order.early(inst));
                assert!(order.late(inst) < order.end(block));
            }
        }
        // And one block ends before the next one starts.
        assert!(order.end(blocks[0]) < order.start(blocks[1]));
        assert!(order.late(insts[1]) < order.early(insts[2]));
    }
}
