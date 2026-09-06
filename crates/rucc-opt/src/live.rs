//! Which values are live where, which is what the pressure model counts and what a scheduler
//! has to know before it moves anything.
//!
//! Design: section 40.6 of `spec/optimizer/40-cost-models.md`, which needs this before it can
//! count anything, and document 39.5, which is where the count becomes meaningful.
//!
//! # Live means used later, and in this IR that is exact
//!
//! A value is live at a point when some path from that point reaches a use of it. The IR is in
//! SSA with block parameters rather than phi nodes, so the awkward case other compilers have here
//! does not arise: a phi's operand is used in the predecessor and not in the block holding the
//! phi, which every liveness implementation over phi nodes has to special case and half of them
//! get wrong. Here the argument travels on the branch, the branch is an instruction in the
//! predecessor, and the ordinary rule that an instruction uses its operands already says the right
//! thing.
//!
//! # The fixpoint
//!
//! Backwards, over the reverse of reverse postorder, until nothing changes. A block's live-in is
//! what is live at its first instruction with its own parameters taken out, since a parameter is
//! defined by arriving. Its live-out is the union of the live-ins of its successors. Postorder
//! means a block is visited after the blocks it branches to wherever the graph allows, so the
//! usual function settles in one round and a loop costs one more.
//!
//! A value passed as a branch argument is live at the branch and not on the edge, because what
//! crosses the edge is the parameter it becomes. [`Liveness::through`] is where a caller sees it,
//! and it is the walk the pressure model counts along, so the argument is counted where it is
//! actually held.
//!
//! # What is not counted
//!
//! Values of type `mem` are the memory dependence chain and are not data. They are live in the
//! same sense as anything else and [`Liveness`] reports them, because a pass asking whether a
//! store is still needed wants them. The pressure model is what drops them, because memory is not
//! held in a register, and that decision belongs where the registers are being counted rather than
//! here.

use rucc_ir::{Block, Func, Inst, Value};

use crate::cfg::Cfg;

/// A dense set of values.
///
/// One bit per value rather than a hash set, because the fixpoint unions one of these per edge
/// per round and a union of two bitmaps is a loop over words.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Set {
    words: Vec<u64>,
}

impl Set {
    /// An empty set with room for that many values.
    fn with_room_for(values: usize) -> Self {
        Self { words: vec![0; values.div_ceil(64)] }
    }

    fn contains(&self, value: Value) -> bool {
        let at = value.index();
        match self.words.get(at / 64) {
            Some(word) => word & (1 << (at % 64)) != 0,
            None => false,
        }
    }

    fn insert(&mut self, value: Value) {
        let at = value.index();
        self.words[at / 64] |= 1 << (at % 64);
    }

    fn remove(&mut self, value: Value) {
        let at = value.index();
        self.words[at / 64] &= !(1 << (at % 64));
    }

    /// Adds everything in the other, and answers whether that changed anything.
    fn union_with(&mut self, other: &Self) -> bool {
        let mut changed = false;
        for (mine, theirs) in self.words.iter_mut().zip(&other.words) {
            let before = *mine;
            *mine |= theirs;
            changed |= *mine != before;
        }
        changed
    }

    fn len(&self) -> usize {
        self.words.iter().map(|word| word.count_ones() as usize).sum()
    }

    fn iter(&self) -> impl Iterator<Item = Value> + use<'_> {
        self.words.iter().enumerate().flat_map(|(at, &word)| {
            (0..64)
                .filter(move |bit| word & (1 << bit) != 0)
                .map(move |bit| Value::new((at * 64 + bit) as u32))
        })
    }
}

/// What is live at the edges of every block.
///
/// Per block rather than per instruction, because the sets inside a block are recoverable from the
/// live-out by walking the block backwards and nothing wants to pay for storing them.
/// [`Liveness::through`] is that walk, and the pressure model is its first caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Liveness {
    live_in: Vec<Set>,
    live_out: Vec<Set>,
}

impl Liveness {
    /// Works out what is live where.
    #[must_use]
    pub fn of(func: &Func, cfg: &Cfg) -> Self {
        let blocks = cfg.capacity();
        let values = func.counts().values;
        let empty = Set::with_room_for(values);
        let mut live_in = vec![empty.clone(); blocks];
        let mut live_out = vec![empty; blocks];

        // Postorder, so a block is reached after the blocks it branches to wherever the graph
        // allows one order to do that. A loop is what makes a second round necessary.
        let order: Vec<Block> = cfg.postorder().to_vec();
        let mut again = true;
        while again {
            again = false;
            for &block in &order {
                let mut out = Set::with_room_for(values);
                for &successor in cfg.successors(block) {
                    out.union_with(&live_in[successor.index()]);
                }
                let mut set = out.clone();
                walk(func, block, &mut set, |_, _| {});
                for &param in &func[block].params {
                    set.remove(param);
                }
                again |= live_out[block.index()].union_with(&out);
                again |= live_in[block.index()].union_with(&set);
            }
        }

        Self { live_in, live_out }
    }

    /// What is live when control arrives at the block, which excludes its own parameters.
    pub fn live_in(&self, block: Block) -> impl Iterator<Item = Value> + use<'_> {
        self.live_in[block.index()].iter()
    }

    /// What is live when control leaves it.
    pub fn live_out(&self, block: Block) -> impl Iterator<Item = Value> + use<'_> {
        self.live_out[block.index()].iter()
    }

    /// Whether that value is live on the way in.
    #[must_use]
    pub fn is_live_in(&self, block: Block, value: Value) -> bool {
        self.live_in[block.index()].contains(value)
    }

    /// Whether that value is live on the way out.
    #[must_use]
    pub fn is_live_out(&self, block: Block, value: Value) -> bool {
        self.live_out[block.index()].contains(value)
    }

    /// How many values are live on the way in.
    #[must_use]
    pub fn count_in(&self, block: Block) -> usize {
        self.live_in[block.index()].len()
    }

    /// How many are live on the way out.
    #[must_use]
    pub fn count_out(&self, block: Block) -> usize {
        self.live_out[block.index()].len()
    }

    /// Walks the block backwards from its live-out, calling `at` before each instruction with what
    /// is live there.
    ///
    /// This is where the per instruction sets come from, for the callers that want them. The set
    /// handed to `at` is what is live just before that instruction runs, so it holds the
    /// instruction's operands and not its results.
    pub fn through(&self, func: &Func, block: Block, mut at: impl FnMut(Inst, &LiveHere<'_>)) {
        let mut set = self.live_out[block.index()].clone();
        walk(func, block, &mut set, |inst, set| at(inst, &LiveHere { set }));
    }
}

/// What is live at one point inside a block.
///
/// A borrowed view rather than a set the caller keeps, because the walk reuses one set and handing
/// out a copy per instruction is the whole cost of the walk.
#[derive(Debug)]
pub struct LiveHere<'a> {
    set: &'a Set,
}

impl LiveHere<'_> {
    /// Whether that value is live here.
    #[must_use]
    pub fn contains(&self, value: Value) -> bool {
        self.set.contains(value)
    }

    /// How many values are live here.
    #[must_use]
    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// Whether nothing is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Them, in order.
    pub fn iter(&self) -> impl Iterator<Item = Value> + use<'_> {
        self.set.iter()
    }
}

/// Walks one block backwards, taking out what each instruction defines and putting in what it
/// uses, and calling `at` with the set as it stands before each instruction.
///
/// The order matters and is the reason this is one function rather than two loops at each caller.
/// The results go out before the operands come in, so an instruction whose operand is also its
/// result leaves the value live, which is what a use before a redefinition means.
fn walk(func: &Func, block: Block, set: &mut Set, mut at: impl FnMut(Inst, &Set)) {
    for this in func.insts_backwards(block) {
        let data = &func[this];
        for result in data.results() {
            set.remove(result);
        }
        for &arg in &func[data.args] {
            set.insert(arg);
        }
        // A branch's arguments are used by the branch, in the block holding it, which is the whole
        // reason block parameters are easier to be right about than phi nodes.
        for call in func.successors(this) {
            for &arg in &func[call.args] {
                set.insert(arg);
            }
        }
        at(this, set);
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, Func, Opcode, Signature, Type};

    use super::Liveness;
    use crate::cfg::Cfg;

    const I32: Type = Type::int(32);

    fn blank(count: usize) -> (Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let blocks: Vec<Block> = (0..count).map(|_| func.create_block()).collect();
        (func, blocks)
    }

    fn liveness(func: &Func) -> (Cfg, Liveness) {
        let cfg = Cfg::new(func);
        let live = Liveness::of(func, &cfg);
        (cfg, live)
    }

    #[test]
    fn a_value_made_and_read_in_one_block_never_crosses_an_edge() {
        let (mut func, blocks) = blank(1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let one = build.iconst(I32, 1);
        let two = build.iconst(I32, 2);
        let sum = build.binary(Opcode::Add, one, two, Flags::NONE);
        build.ret(&[sum]);

        let (_, live) = liveness(&func);
        assert_eq!(live.count_in(blocks[0]), 0);
        assert_eq!(live.count_out(blocks[0]), 0);
    }

    #[test]
    fn a_value_read_in_a_later_block_is_live_on_the_edge_between_them() {
        let (mut func, blocks) = blank(2);
        let mut build = Builder::new(&mut func, blocks[0]);
        let kept = build.iconst(I32, 7);
        build.jump(blocks[1], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.ret(&[kept]);

        let (_, live) = liveness(&func);
        assert!(live.is_live_out(blocks[0], kept), "it is read after the branch");
        assert!(live.is_live_in(blocks[1], kept), "and it has to arrive there to be read");
        assert!(!live.is_live_in(blocks[0], kept), "it does not exist before it is made");
    }

    #[test]
    fn a_value_passed_on_the_branch_is_used_by_the_branch_and_not_by_the_block_it_arrives_at() {
        // The whole reason block parameters are easier to be right about than phi nodes. The
        // argument is live in the predecessor, and the parameter it becomes is defined by
        // arriving, so it is not live-in of the block that holds it.
        let (mut func, blocks) = blank(2);
        let param = func.append_param(blocks[1], I32);
        let mut build = Builder::new(&mut func, blocks[0]);
        let sent = build.iconst(I32, 7);
        build.jump(blocks[1], &[sent]);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.ret(&[param]);

        let (_, live) = liveness(&func);
        // It is live at the branch and dead on the edge, which is the point. Live-out is what
        // survives the edge, and what the argument becomes on the other side is the parameter.
        let mut at_the_jump = false;
        live.through(&func, blocks[0], |inst, here| {
            if func[inst].opcode == Opcode::Jump {
                at_the_jump = here.contains(sent);
            }
        });
        assert!(at_the_jump, "the branch uses it");
        assert!(!live.is_live_out(blocks[0], sent), "and it does not survive the edge");
        assert!(!live.is_live_in(blocks[1], param), "a parameter is defined by arriving");
        assert!(!live.is_live_in(blocks[1], sent), "nor does it arrive under its own name");
        assert_eq!(live.count_in(blocks[1]), 0);
    }

    #[test]
    fn a_value_read_on_one_arm_only_is_live_on_that_arm_and_not_the_other() {
        let (mut func, blocks) = blank(4);
        let mut build = Builder::new(&mut func, blocks[0]);
        let kept = build.iconst(I32, 7);
        let cond = build.iconst(Type::I1, 1);
        build.br_if(cond, blocks[1], &[], blocks[2], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.jump(blocks[3], &[]);
        let mut build = Builder::new(&mut func, blocks[2]);
        build.ret(&[kept]);
        let mut build = Builder::new(&mut func, blocks[3]);
        build.ret(&[]);

        let (_, live) = liveness(&func);
        assert!(live.is_live_out(blocks[0], kept), "one arm reads it, so it survives the branch");
        assert!(live.is_live_in(blocks[2], kept));
        assert!(!live.is_live_in(blocks[1], kept), "this arm never mentions it");
    }

    #[test]
    fn a_value_read_after_the_loop_stays_live_all_the_way_round_it() {
        // Block 0 makes it, block 1 is the loop and does not touch it, block 2 reads it. The
        // fixpoint is what gets this right: one backwards pass over the blocks in postorder puts
        // it live-in of the loop, and the second round is what carries that back to the latch.
        let (mut func, blocks) = blank(3);
        let mut build = Builder::new(&mut func, blocks[0]);
        let kept = build.iconst(I32, 7);
        let cond = build.iconst(Type::I1, 1);
        build.jump(blocks[1], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.br_if(cond, blocks[1], &[], blocks[2], &[]);
        let mut build = Builder::new(&mut func, blocks[2]);
        build.ret(&[kept]);

        let (_, live) = liveness(&func);
        assert!(live.is_live_in(blocks[1], kept), "it has to survive the loop to be read after it");
        assert!(live.is_live_out(blocks[1], kept), "including round the back edge");
        assert!(live.is_live_in(blocks[2], kept));
    }

    #[test]
    fn nothing_is_live_in_a_block_control_never_reaches() {
        let (mut func, blocks) = blank(2);
        let mut build = Builder::new(&mut func, blocks[0]);
        let kept = build.iconst(I32, 7);
        build.ret(&[kept]);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.ret(&[]);

        let (cfg, live) = liveness(&func);
        assert!(!cfg.reaches(blocks[1]));
        assert_eq!(live.count_in(blocks[1]), 0);
        assert_eq!(live.count_out(blocks[1]), 0);
    }

    #[test]
    fn the_walk_through_a_block_says_what_is_live_before_each_instruction() {
        let (mut func, blocks) = blank(2);
        let mut build = Builder::new(&mut func, blocks[0]);
        let one = build.iconst(I32, 1);
        let two = build.iconst(I32, 2);
        let sum = build.binary(Opcode::Add, one, two, Flags::NONE);
        let jump = build.jump(blocks[1], &[sum]);
        let param = func.append_param(blocks[1], I32);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.ret(&[param]);

        let (_, live) = liveness(&func);
        let mut counts = Vec::new();
        live.through(&func, blocks[0], |inst, here| counts.push((inst, here.len())));
        // Backwards: before the jump only the sum is live, before the add both operands are,
        // before the second constant only the first is, and before the first nothing is.
        assert_eq!(counts.len(), 4);
        assert_eq!(counts[0], (jump, 1));
        assert_eq!(counts[1].1, 2, "the add's two operands");
        assert_eq!(counts[2].1, 1);
        assert_eq!(counts[3].1, 0);
        assert!(counts[0].1 <= counts[1].1, "the sum replaces the two it was made from");
    }

    #[test]
    fn a_value_that_is_its_own_operand_stays_live_across_the_instruction_that_redefines_nothing() {
        // Results go out before operands come in, which is what makes a use of a value the
        // instruction also produces read as a use rather than as a definition.
        let (mut func, blocks) = blank(1);
        let mut build = Builder::new(&mut func, blocks[0]);
        let start = build.iconst(I32, 1);
        let doubled = build.binary(Opcode::Add, start, start, Flags::NONE);
        build.ret(&[doubled]);

        let (_, live) = liveness(&func);
        let mut most = 0;
        live.through(&func, blocks[0], |_, here| most = most.max(here.len()));
        assert_eq!(most, 1, "one value used twice is one value");
    }
}
