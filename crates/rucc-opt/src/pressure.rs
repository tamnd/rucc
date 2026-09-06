//! How many registers the program needs at each point, which is the one number four passes ask
//! for and none of them should compute for itself.
//!
//! Design: section 40.6 of `spec/optimizer/40-cost-models.md`. It discharges document 12.5's
//! obligation for global code motion and document 27.2's for loop invariant motion, which are the
//! same obligation, and document 39.5's finding that the allocator and the scheduler want the same
//! model.
//!
//! # It is the count, not an estimate of it
//!
//! In SSA the number of values live at a point is the number of registers the program needs at
//! that point. That is document 39.5's chordality result and it is what makes this worth computing
//! exactly rather than approximating: the interference graph of an SSA program is chordal, its
//! chromatic number is the size of its largest clique, and the largest clique at a point is
//! exactly what is live there. Everywhere else in a compiler a pressure number is a guess. Here it
//! is not, and the four consumers can be written against a number rather than against a heuristic.
//!
//! # The four consumers
//!
//! Loop invariant motion and global code motion ask whether the pressure inside a loop is already
//! at the allocatable count less a margin, and hoist only division and calls when it is. The
//! scheduler asks, among instructions on equally long critical paths, which one reduces the live
//! count. The spill phase asks for the maximum and reduces it to the register count, and is the
//! consumer that defines the quantity. If conversion asks what merging two arms' live ranges into
//! one block would do to the block it merges them into.
//!
//! # Two classes, and where the register count comes from
//!
//! [`Class`] is integer or floating point, which is the split every target has. A vector lands in
//! the floating point class because on x86-64 the same registers hold both, and a target where
//! that is wrong is a target that needs a third class here rather than a different rule.
//!
//! What this does not hold is how many registers there are. That is the target's, this crate does
//! not see the target, and the comparison belongs where the register file is in hand. So the
//! answer here is a count and [`Pressure::is_tight`] takes the allocatable count from the caller.
//! The margin, which is GCC's `ira-loop-reserved-regs`, is a tuning constant and lives with the
//! others in `rucc_cost::heuristics`.
//!
//! # What is not counted
//!
//! Values of type `mem` are the memory dependence chain rather than data, and nothing holds one in
//! a register. Values of type `void` are not values. Both are dropped here rather than in
//! [`crate::live`], because a pass asking what a store depends on wants the memory chain and only
//! the register counting wants it gone.

use rucc_cost::heuristics::LOOP_RESERVED_REGS;
use rucc_ir::{Block, Func, Type};

use crate::cfg::Cfg;
use crate::live::Liveness;
use crate::loops::{LoopId, Loops};

/// Which bank of registers a value needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// Integers, pointers and capabilities, which the general purpose registers hold.
    Integer,
    /// Floating point and vectors, which on every target rucc targets share a bank.
    Float,
}

impl Class {
    /// Both of them, for a caller that reports each.
    pub const ALL: [Self; 2] = [Self::Integer, Self::Float];

    /// How many there are, for the arrays keyed by one.
    pub const COUNT: usize = Self::ALL.len();

    /// How it reads in a dump.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Float => "float",
        }
    }

    /// Which bank holds a value of that type, and `None` for a type no register holds.
    #[must_use]
    pub const fn of(ty: Type) -> Option<Self> {
        if ty.is_float() {
            return Some(Self::Float);
        }
        if ty.is_vector() {
            // A vector of integers still lives in the vector bank, which is the float one here.
            return Some(Self::Float);
        }
        if ty.is_int() || ty.is_ptr() || ty.is_cap() {
            return Some(Self::Integer);
        }
        // `mem` is the dependence chain and `void` is not a value.
        None
    }

    const fn index(self) -> usize {
        match self {
            Self::Integer => 0,
            Self::Float => 1,
        }
    }
}

impl std::fmt::Display for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A count per register class.
type PerClass = [u32; Class::COUNT];

/// How many values of each class are live, at the places a consumer asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pressure {
    arriving: Vec<PerClass>,
    most_in: Vec<PerClass>,
    most: PerClass,
}

impl Pressure {
    /// Counts what is live at every point of every block.
    #[must_use]
    pub fn of(func: &Func, cfg: &Cfg, live: &Liveness) -> Self {
        let blocks = cfg.capacity();
        let mut arriving = vec![[0; Class::COUNT]; blocks];
        let mut most_in = vec![[0; Class::COUNT]; blocks];
        let mut most = [0; Class::COUNT];

        for block in cfg.reverse_postorder() {
            let at = block.index();
            for value in live.live_in(block) {
                if let Some(class) = Class::of(func[value].ty) {
                    arriving[at][class.index()] += 1;
                }
            }
            // The walk is backwards from the live-out, which is what gives the count just before
            // each instruction without keeping a set per instruction.
            let mut here = [0; Class::COUNT];
            for value in live.live_out(block) {
                if let Some(class) = Class::of(func[value].ty) {
                    here[class.index()] += 1;
                }
            }
            most_in[at] = here;
            live.through(func, block, |_, at_inst| {
                let mut counted = [0; Class::COUNT];
                for value in at_inst.iter() {
                    if let Some(class) = Class::of(func[value].ty) {
                        counted[class.index()] += 1;
                    }
                }
                for class in Class::ALL {
                    let index = class.index();
                    most_in[at][index] = most_in[at][index].max(counted[index]);
                }
            });
            for class in Class::ALL {
                let index = class.index();
                most[index] = most[index].max(most_in[at][index]);
            }
        }

        Self { arriving, most_in, most }
    }

    /// How many are live when control arrives at the block.
    #[must_use]
    pub fn arriving_at(&self, block: Block, class: Class) -> u32 {
        self.arriving[block.index()][class.index()]
    }

    /// The most that are live at any point of the block.
    #[must_use]
    pub fn most_in_block(&self, block: Block, class: Class) -> u32 {
        self.most_in[block.index()][class.index()]
    }

    /// The most that are live at any point of the function.
    ///
    /// The spill phase's number, and the one that says how many registers the function needs.
    #[must_use]
    pub fn most_in_function(&self, class: Class) -> u32 {
        self.most[class.index()]
    }

    /// The most that are live at any point of the loop, including the loops nested in it.
    ///
    /// What loop invariant motion and global code motion ask, since a value hoisted out of a loop
    /// is live across all of it and the pressure it adds is added everywhere inside.
    #[must_use]
    pub fn most_in_loop(&self, loops: &Loops, id: LoopId, class: Class) -> u32 {
        loops.blocks(id).iter().map(|&block| self.most_in_block(block, class)).max().unwrap_or(0)
    }

    /// Whether hoisting into that loop is already too expensive to be worth it.
    ///
    /// The allocatable count is the caller's, because this crate does not see the target. The
    /// margin is `LOOP_RESERVED_REGS`, which is GCC's `ira-loop-reserved-regs` and is two: a hoist
    /// that takes the pressure up to the register count has bought nothing, because the value it
    /// hoisted is now live across the loop and something else gets spilled to make room for it.
    #[must_use]
    pub fn is_tight(&self, loops: &Loops, id: LoopId, class: Class, allocatable: u32) -> bool {
        self.most_in_loop(loops, id, class) >= allocatable.saturating_sub(LOOP_RESERVED_REGS)
    }

    /// What is wrong with these numbers, which should be nothing.
    ///
    /// The one invariant worth checking is that no block's own maximum exceeds the function's,
    /// since the function's is the maximum over the blocks and a consumer comparing against the
    /// wrong one of the two would be making a decision on a number that is too small.
    #[must_use]
    pub fn problems(&self, cfg: &Cfg) -> Vec<String> {
        let mut problems = Vec::new();
        for block in cfg.reverse_postorder() {
            for class in Class::ALL {
                let here = self.most_in_block(block, class);
                let whole = self.most_in_function(class);
                if here > whole {
                    problems.push(format!(
                        "block{} needs {here} {class} registers and the function claims {whole}",
                        block.index()
                    ));
                }
                if self.arriving_at(block, class) > here {
                    problems.push(format!(
                        "block{} has more {class} values arriving than it ever holds",
                        block.index()
                    ));
                }
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, Float, Func, Opcode, Signature, Type};

    use super::{Class, Pressure};
    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::live::Liveness;
    use crate::loops::Loops;

    const I32: Type = Type::int(32);

    fn blank(count: usize) -> (Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let blocks: Vec<Block> = (0..count).map(|_| func.create_block()).collect();
        (func, blocks)
    }

    fn pressure(func: &Func) -> (Cfg, Pressure) {
        let cfg = Cfg::new(func);
        let live = Liveness::of(func, &cfg);
        let of = Pressure::of(func, &cfg, &live);
        assert!(of.problems(&cfg).is_empty(), "{:?}", of.problems(&cfg));
        (cfg, of)
    }

    #[test]
    fn the_most_live_at_once_is_the_number_of_registers_the_block_needs() {
        // Three constants alive together before the first add takes two of them.
        let (mut func, blocks) = blank(1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let one = build.iconst(I32, 1);
        let two = build.iconst(I32, 2);
        let three = build.iconst(I32, 3);
        let first = build.binary(Opcode::Add, one, two, Flags::NONE);
        let second = build.binary(Opcode::Add, first, three, Flags::NONE);
        build.ret(&[second]);

        let (_, of) = pressure(&func);
        assert_eq!(of.most_in_block(blocks[0], Class::Integer), 3);
        assert_eq!(of.most_in_function(Class::Integer), 3);
        assert_eq!(of.most_in_function(Class::Float), 0);
    }

    #[test]
    fn the_two_classes_are_counted_apart_because_they_are_two_banks_of_registers() {
        let (mut func, blocks) = blank(1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let whole = build.iconst(I32, 1);
        let fraction = build.fconst(Type::float(Float::F64), 0);
        let other = build.fconst(Type::float(Float::F64), 1);
        let sum = build.binary(Opcode::FAdd, fraction, other, Flags::NONE);
        build.ret(&[whole, sum]);

        let (_, of) = pressure(&func);
        assert_eq!(of.most_in_function(Class::Integer), 1);
        assert_eq!(of.most_in_function(Class::Float), 2);
    }

    #[test]
    fn nothing_that_is_not_held_in_a_register_is_counted() {
        // The memory chain is a value and it is live, and no register holds one.
        let (mut func, blocks) = blank(1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let mem = build.mem_entry();
        build.ret(&[]);
        let ty = func[mem].ty;

        assert!(ty.is_mem());
        assert_eq!(Class::of(ty), None);
        assert_eq!(Class::of(Type::VOID), None);
        assert_eq!(Class::of(Type::PTR), Some(Class::Integer));
        assert_eq!(Class::of(Type::vector(I32, 4)), Some(Class::Float));

        let (_, of) = pressure(&func);
        assert_eq!(of.most_in_function(Class::Integer), 0);
    }

    #[test]
    fn a_value_read_after_the_loop_costs_a_register_everywhere_inside_it() {
        // Which is the whole reason loop invariant motion asks this question before hoisting.
        let (mut func, blocks) = blank(3);
        let mut build = Builder::new(&mut func, blocks[0]);
        let kept = build.iconst(I32, 7);
        let cond = build.iconst(Type::I1, 1);
        build.jump(blocks[1], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.br_if(cond, blocks[1], &[], blocks[2], &[]);
        let mut build = Builder::new(&mut func, blocks[2]);
        build.ret(&[kept]);

        let (cfg, of) = pressure(&func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        let id = loops.innermost(blocks[1]).expect("block 1 is a loop");
        // The value and the condition both cross the loop, so the loop holds two.
        assert_eq!(of.most_in_loop(&loops, id, Class::Integer), 2);
        assert!(of.is_tight(&loops, id, Class::Integer, 4), "two of four, less a margin of two");
        assert!(!of.is_tight(&loops, id, Class::Integer, 16), "there is room on a real machine");
    }

    #[test]
    fn the_margin_is_what_stops_a_hoist_from_walking_up_to_the_edge() {
        let (mut func, blocks) = blank(2);
        let mut build = Builder::new(&mut func, blocks[0]);
        let cond = build.iconst(Type::I1, 1);
        build.jump(blocks[1], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.br_if(cond, blocks[1], &[], blocks[0], &[]);

        let (cfg, of) = pressure(&func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        let id = loops.innermost(blocks[1]).expect("block 1 is a loop");
        assert_eq!(of.most_in_loop(&loops, id, Class::Integer), 1);
        // One value live, and a machine with three registers has two reserved, so one is already
        // at the line. A machine with four is not.
        assert!(of.is_tight(&loops, id, Class::Integer, 3));
        assert!(!of.is_tight(&loops, id, Class::Integer, 4));
        // A machine with fewer registers than the margin does not underflow into a huge number.
        assert!(of.is_tight(&loops, id, Class::Integer, 1));
    }

    #[test]
    fn a_block_control_never_reaches_needs_nothing() {
        let (mut func, blocks) = blank(2);
        let mut build = Builder::new(&mut func, blocks[0]);
        let one = build.iconst(I32, 1);
        build.ret(&[one]);
        let mut build = Builder::new(&mut func, blocks[1]);
        let two = build.iconst(I32, 2);
        build.ret(&[two]);

        let (cfg, of) = pressure(&func);
        assert!(!cfg.reaches(blocks[1]));
        assert_eq!(of.most_in_block(blocks[1], Class::Integer), 0);
        assert_eq!(of.arriving_at(blocks[1], Class::Integer), 0);
    }

    #[test]
    fn what_arrives_at_a_block_is_never_more_than_the_block_ever_holds() {
        let (mut func, blocks) = blank(3);
        let mut build = Builder::new(&mut func, blocks[0]);
        let kept = build.iconst(I32, 7);
        let cond = build.iconst(Type::I1, 1);
        build.br_if(cond, blocks[1], &[], blocks[2], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.ret(&[kept]);
        let mut build = Builder::new(&mut func, blocks[2]);
        build.ret(&[]);

        let (cfg, of) = pressure(&func);
        assert!(of.problems(&cfg).is_empty());
        for block in cfg.reverse_postorder() {
            for class in Class::ALL {
                assert!(of.arriving_at(block, class) <= of.most_in_block(block, class));
                assert!(of.most_in_block(block, class) <= of.most_in_function(class));
            }
        }
        assert_eq!(of.arriving_at(blocks[1], Class::Integer), 1, "the value it returns");
        assert_eq!(of.arriving_at(blocks[2], Class::Integer), 0, "this arm reads nothing");
    }

    #[test]
    fn every_class_names_itself_and_there_are_only_the_two() {
        assert_eq!(Class::ALL.len(), Class::COUNT);
        for class in Class::ALL {
            assert!(!class.as_str().is_empty());
            assert_eq!(class.to_string(), class.as_str());
        }
        assert_ne!(Class::Integer, Class::Float);
    }
}
