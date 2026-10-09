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
//! What a block adds to the set passing through it and what it takes out are the same every round,
//! so they are worked out once rather than by walking its instructions each time. A round only
//! looks again at a block when the live-in of something it branches to changed in the round
//! before, since otherwise it would get the same answer. A function of thirty thousand blocks and
//! two thousand loops took seconds when every block was redone every round.
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

use std::cmp::Ordering;

use rucc_ir::{Block, Func, Inst, Value};

use crate::cfg::Cfg;

/// A set of values, kept as the words of a bitmap that have something in them.
///
/// A bitmap because the fixpoint unions one of these per edge per round, and a union of two
/// bitmaps is a loop over words. Only the words with a bit set are kept, because what is live at
/// one place is a few runs of neighbouring values out of the whole function. On jtckdint's `main`,
/// with 200000 values and 30000 blocks, a whole bitmap per block was 1.3 GB for the live-ins and
/// live-outs together, and fewer than one word in a hundred had anything in it. Allocating that,
/// clearing it and copying it round the fixpoint was most of what working out liveness cost.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Set {
    /// Which word of the bitmap each is, and the word. In order and never zero, so two sets with
    /// the same values in them are the same list.
    words: Vec<(u32, u64)>,
}

impl Set {
    /// Where the word holding that value is, or where it would go.
    fn find(&self, value: Value) -> (Result<usize, usize>, u64) {
        let at = value.index();
        let word = u32::try_from(at / 64).expect("a value number fits in 32 bits");
        (self.words.binary_search_by_key(&word, |&(word, _)| word), 1 << (at % 64))
    }

    fn contains(&self, value: Value) -> bool {
        match self.find(value) {
            (Ok(at), bit) => self.words[at].1 & bit != 0,
            (Err(_), _) => false,
        }
    }

    /// Puts it in, and answers whether it was not already there.
    fn insert(&mut self, value: Value) -> bool {
        match self.find(value) {
            (Ok(at), bit) => {
                let word = &mut self.words[at].1;
                let had = *word & bit != 0;
                *word |= bit;
                !had
            }
            (Err(at), bit) => {
                let word = u32::try_from(value.index() / 64).expect("checked by find");
                self.words.insert(at, (word, bit));
                true
            }
        }
    }

    /// Takes it out, and answers whether it was there.
    fn remove(&mut self, value: Value) -> bool {
        let (Ok(at), bit) = self.find(value) else {
            return false;
        };
        let word = &mut self.words[at].1;
        let had = *word & bit != 0;
        *word &= !bit;
        if *word == 0 {
            self.words.remove(at);
        }
        had
    }

    /// Adds everything in the other.
    fn union_with(&mut self, other: &Self) {
        if other.words.is_empty() {
            return;
        }
        if self.words.is_empty() {
            self.words.clone_from(&other.words);
            return;
        }
        let (mine, theirs) = (&self.words, &other.words);
        let mut both = Vec::with_capacity(mine.len() + theirs.len());
        let (mut left, mut right) = (0, 0);
        while left < mine.len() && right < theirs.len() {
            let ((at, word), (other_at, other_word)) = (mine[left], theirs[right]);
            match at.cmp(&other_at) {
                Ordering::Less => {
                    both.push((at, word));
                    left += 1;
                }
                Ordering::Greater => {
                    both.push((other_at, other_word));
                    right += 1;
                }
                Ordering::Equal => {
                    both.push((at, word | other_word));
                    left += 1;
                    right += 1;
                }
            }
        }
        both.extend_from_slice(&mine[left..]);
        both.extend_from_slice(&theirs[right..]);
        self.words = both;
    }

    /// Takes everything out.
    fn clear(&mut self) {
        self.words.clear();
    }

    fn len(&self) -> usize {
        self.words.iter().map(|&(_, word)| word.count_ones() as usize).sum()
    }

    /// Them, in order.
    ///
    /// The set bits of each word are taken one at a time rather than by testing all sixty four.
    fn iter(&self) -> impl Iterator<Item = Value> + use<'_> {
        self.words
            .iter()
            .flat_map(|&(at, word)| Bits(word).map(move |bit| Value::new(at * 64 + bit)))
    }
}

/// The set bits of one word, lowest first.
///
/// `trailing_zeros` finds the next one and clearing the lowest set bit moves past it, so the work
/// is one step per bit that is there rather than one per bit there could be.
struct Bits(u64);

impl Iterator for Bits {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        if self.0 == 0 {
            return None;
        }
        let bit = self.0.trailing_zeros();
        self.0 &= self.0 - 1;
        Some(bit)
    }
}

/// Which values fall in each of a few groups, as one bitmap per group laid out by word.
///
/// Counting a group in a live set is then a mask and a population count for each word the set has
/// rather than a look at every value in it. The pressure model counts what is live at the edges of
/// every block, and reading the type of each of those values one at a time was most of what it
/// cost on a function with thousands of blocks.
#[derive(Debug)]
pub struct Groups<const N: usize> {
    words: Vec<[u64; N]>,
}

impl<const N: usize> Groups<N> {
    /// Puts each value of the function in the group `group` names, or in none.
    ///
    /// # Panics
    ///
    /// Panics if `group` names a group past the last of the `N`.
    #[must_use]
    pub fn of(func: &Func, group: impl Fn(Value) -> Option<usize>) -> Self {
        let mut words = vec![[0; N]; func.values().count().div_ceil(64)];
        for value in func.values() {
            if let Some(group) = group(value) {
                let at = value.index();
                words[at / 64][group] |= 1 << (at % 64);
            }
        }
        Self { words }
    }

    /// How many of the set fall in each group.
    fn count(&self, set: &Set) -> [u32; N] {
        let mut counts = [0; N];
        for &(at, word) in &set.words {
            let Some(masks) = self.words.get(at as usize) else { continue };
            for (count, mask) in counts.iter_mut().zip(masks) {
                *count += (word & mask).count_ones();
            }
        }
        counts
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
        let live_in = vec![Set::default(); blocks];
        let live_out = vec![Set::default(); blocks];

        // What each block reads before it writes, and what it writes, parameters included. Live in
        // is then live out with the second taken out and the first put in, which is what walking
        // the block backwards gives without walking it.
        let order: Vec<Block> = cfg.postorder().to_vec();
        let (reads, writes) = effects(func, &order);
        let mut live = Self { live_in, live_out };
        live.settle(cfg, &order, &reads, &writes);
        live
    }

    /// The same after a change that left what is live at the edges of every block outside
    /// `blocks` as it was, worked out again over those blocks alone.
    ///
    /// For a pass that moved instructions around inside a region and knows nothing outside it can
    /// have a different answer. Loop invariant motion is the one: a hoist takes instructions out
    /// of a loop and into its preheader, and the only blocks where what is live can change are
    /// those. Working the whole function out again for each loop that asks after one was most of
    /// what the pass cost on lz4hc.c at `-O2`.
    ///
    /// The region starts from nothing and is settled against the blocks around it, so a value no
    /// longer live in it goes, which starting from what it had would never find out.
    pub fn refresh(&mut self, func: &Func, cfg: &Cfg, blocks: &[Block]) {
        let mut inside = vec![false; cfg.capacity()];
        for &block in blocks {
            inside[block.index()] = true;
        }
        let order: Vec<Block> =
            cfg.postorder().iter().copied().filter(|block| inside[block.index()]).collect();
        for &block in &order {
            self.live_in[block.index()].clear();
            self.live_out[block.index()].clear();
        }
        let (reads, writes) = effects(func, &order);
        self.settle(cfg, &order, &reads, &writes);
    }

    /// The fixpoint over the blocks in `order`, with what each reads and writes at the same place
    /// in the two lists, and the live-in of every block outside them taken as it stands.
    fn settle(&mut self, cfg: &Cfg, order: &[Block], reads: &[Vec<Value>], writes: &[Vec<Value>]) {
        // Postorder, so a block is reached after the blocks it branches to wherever the graph
        // allows one order to do that. A loop is what makes a second round necessary, and the
        // second round is only the blocks something changed under.
        let mut stale = vec![false; cfg.capacity()];
        for &block in order {
            stale[block.index()] = true;
        }
        let mut set = Set::default();
        let mut again = true;
        while again {
            again = false;
            for (place, &block) in order.iter().enumerate() {
                let at = block.index();
                if !std::mem::take(&mut stale[at]) {
                    continue;
                }
                set.clear();
                for &successor in cfg.successors(block) {
                    set.union_with(&self.live_in[successor.index()]);
                }
                self.live_out[at].clone_from(&set);
                for &value in &writes[place] {
                    set.remove(value);
                }
                for &value in &reads[place] {
                    set.insert(value);
                }
                if self.live_in[at] != set {
                    self.live_in[at].clone_from(&set);
                    for &pred in cfg.predecessors(block) {
                        stale[pred.index()] = true;
                        again = true;
                    }
                }
            }
        }
    }

    /// How many of what is live when control arrives at the block fall in each group.
    #[must_use]
    pub fn grouped_in<const N: usize>(&self, block: Block, groups: &Groups<N>) -> [u32; N] {
        groups.count(&self.live_in[block.index()])
    }

    /// How many of what is live when control leaves the block fall in each group.
    #[must_use]
    pub fn grouped_out<const N: usize>(&self, block: Block, groups: &Groups<N>) -> [u32; N] {
        groups.count(&self.live_out[block.index()])
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
        walk(func, block, &mut set, |inst, set, _| at(inst, &LiveHere { set }));
    }

    /// The same walk, reporting what each instruction changes rather than what is live.
    ///
    /// [`Liveness::through`] hands out the whole set at every instruction, and a caller that only
    /// wants to count what is in it pays the size of the set per instruction. In a function of a
    /// hundred and ninety thousand instructions the set is thousands of values wide and that is
    /// quadratic. What actually changes at an instruction is its results and its operands, so a
    /// caller keeping a running count can be handed those instead and stay linear.
    /// tamnd/rucc#1015.
    pub fn changes(&self, func: &Func, block: Block, mut at: impl FnMut(Inst, &Change)) {
        let mut set = self.live_out[block.index()].clone();
        walk(func, block, &mut set, |inst, _, change| at(inst, change));
    }
}

/// What one instruction does to the live set, seen walking the block backwards.
///
/// Both lists hold each value once, because they record the bits that moved rather than the names
/// the instruction wrote: a value an instruction names twice is one bit and arrives once.
#[derive(Debug, Default)]
pub struct Change {
    /// Values the instruction defines, which are live after it and not before it.
    pub gone: Vec<Value>,
    /// Values it names, which are live before it and were not after it.
    pub arrived: Vec<Value>,
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

/// What each block reads before it writes and what it writes, parameters included, at the place
/// in the two lists the block has in `order`.
fn effects(func: &Func, order: &[Block]) -> (Vec<Vec<Value>>, Vec<Vec<Value>>) {
    let mut reads: Vec<Vec<Value>> = vec![Vec::new(); order.len()];
    let mut writes: Vec<Vec<Value>> = vec![Vec::new(); order.len()];
    let mut defined = Set::default();
    let mut read = Set::default();
    for (at, &block) in order.iter().enumerate() {
        for &param in &func[block].params {
            defined.insert(param);
            writes[at].push(param);
        }
        for inst in func.insts(block) {
            let data = &func[inst];
            let branches = func.successors(inst).flat_map(|call| &func[call.args]);
            for &arg in func[data.args].iter().chain(branches) {
                if !defined.contains(arg) && read.insert(arg) {
                    reads[at].push(arg);
                }
            }
            for result in data.results() {
                defined.insert(result);
                writes[at].push(result);
            }
        }
        for &value in &writes[at] {
            defined.remove(value);
        }
        for &value in &reads[at] {
            read.remove(value);
        }
    }
    (reads, writes)
}

/// Walks one block backwards, taking out what each instruction defines and putting in what it
/// uses, and calling `at` with the set as it stands before each instruction.
///
/// The order matters and is the reason this is one function rather than two loops at each caller.
/// The results go out before the operands come in, so an instruction whose operand is also its
/// result leaves the value live, which is what a use before a redefinition means.
fn walk(func: &Func, block: Block, set: &mut Set, mut at: impl FnMut(Inst, &Set, &Change)) {
    let mut change = Change::default();
    for this in func.insts_backwards(block) {
        change.gone.clear();
        change.arrived.clear();
        let data = &func[this];
        for result in data.results() {
            if set.remove(result) {
                change.gone.push(result);
            }
        }
        for &arg in &func[data.args] {
            if set.insert(arg) {
                change.arrived.push(arg);
            }
        }
        // A branch's arguments are used by the branch, in the block holding it, which is the whole
        // reason block parameters are easier to be right about than phi nodes.
        for call in func.successors(this) {
            for &arg in &func[call.args] {
                if set.insert(arg) {
                    change.arrived.push(arg);
                }
            }
        }
        at(this, set, &change);
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, Func, Opcode, Signature, Type, Value};

    use super::{Groups, Liveness, Set};
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
    fn a_set_keeps_only_the_words_with_something_in_them() {
        let value = Value::new;
        let mut first = Set::default();
        assert!(first.insert(value(3)));
        assert!(first.insert(value(200)));
        assert!(!first.insert(value(3)), "it was already there");
        assert!(first.insert(value(70)));
        assert!(first.remove(value(70)));
        assert!(!first.remove(value(70)), "it went the first time");
        assert!(!first.remove(value(5000)), "nothing was ever near it");
        assert_eq!(first.words.len(), 2, "the word 70 was in went with it");

        let mut second = Set::default();
        second.insert(value(64));
        second.insert(value(200));
        second.insert(value(201));
        second.insert(value(9000));
        first.union_with(&second);
        let all: Vec<u32> = first.iter().map(|value| value.raw()).collect();
        assert_eq!(all, [3, 64, 200, 201, 9000]);
        assert_eq!(first.len(), 5);
        assert!(first.contains(value(201)) && !first.contains(value(202)));

        // Put in the other way round and taken out again, it is the same list, which is what the
        // fixpoint compares to know it is done.
        let mut again = Set::default();
        for number in [9000, 201, 5, 200, 64, 3] {
            again.insert(value(number));
        }
        again.remove(value(5));
        assert_eq!(again, first);
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
    fn a_group_counts_what_counting_one_value_at_a_time_counts() {
        // Enough values that the live set runs over more than one word of the bitmap.
        let (mut func, blocks) = blank(2);
        let mut build = Builder::new(&mut func, blocks[0]);
        let kept: Vec<Value> = (0..150).map(|number| build.iconst(I32, number)).collect();
        build.jump(blocks[1], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        build.ret(&kept[..]);

        // Every third value is in no group, and the rest go by whether their number is even.
        let group = |value: Value| (value.index() % 3 != 0).then_some(value.index() % 2);
        let groups = Groups::<2>::of(&func, group);
        let (_, live) = liveness(&func);
        for &block in &blocks {
            for (grouped, values) in [
                (live.grouped_in(block, &groups), live.live_in(block).collect::<Vec<_>>()),
                (live.grouped_out(block, &groups), live.live_out(block).collect()),
            ] {
                let mut counted = [0; 2];
                for value in values {
                    if let Some(group) = group(value) {
                        counted[group] += 1;
                    }
                }
                assert_eq!(grouped, counted);
            }
        }
        assert_eq!(live.grouped_in(blocks[1], &groups), [50, 50]);
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
