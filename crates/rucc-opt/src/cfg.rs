//! The control flow graph, which is the shape of a function with the instructions taken out.
//!
//! Design: `spec/optimizer/06-cfg-and-dominators.md`.
//!
//! `Func` stores successors and only successors. A terminator names the blocks it goes to and
//! no block records who arrives at it, which is the right storage, because an argument that
//! travels on an edge cannot then go out of step with a predecessor list kept somewhere else.
//! It is the wrong question to ask afresh in six passes, so this is the answer computed once:
//! the predecessors, both adjacency lists, a postorder, and which blocks the entry reaches.
//!
//! Everything here is recomputed from nothing after any change to the shape of the function.
//! There is no incremental update and section 6.3 of the design says why: a CFG edit already
//! invalidates almost every other analysis, so a graph that survived one would be the single
//! survivor of a clearing that took the rest, and a stale dominator tree is the hardest kind of
//! compiler bug to find.

use rucc_ir::{Block, Func};

/// Who goes where in a function, and in what order to walk it.
///
/// Built with [`Cfg::new`] and read only. Nothing here holds a borrow of the function, so a
/// pass can compute the graph, then edit the code, and the compiler will not stop it. What
/// stops it is the pass manager, which throws this away when a pass says it changed the shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cfg {
    /// The blocks each block branches to, indexed by block number.
    succs: Lists,
    /// The blocks that branch to each block, indexed by block number.
    preds: Lists,
    /// The blocks the entry reaches, children before parents.
    postorder: Vec<Block>,
    /// Where each block sits in reverse postorder, and `None` for one the entry misses.
    rank: Vec<Option<u32>>,
    /// Where control arrives, which a function that is only declared does not have.
    entry: Option<Block>,
}

impl Cfg {
    /// Reads the graph out of the function.
    ///
    /// Linear in the blocks and the edges between them. A function with no blocks gives an
    /// empty graph rather than an error, because a declaration is a perfectly ordinary thing
    /// for a pipeline to be handed and refusing it here would put the check in every caller.
    #[must_use]
    pub fn new(func: &Func) -> Self {
        let counts = func.counts();
        // Every edge once, in the order the blocks are laid out, and how many leave and arrive at
        // each block, so both lists can be laid end to end in one allocation each. The graph is
        // built again after every pass that moves an edge, and two allocations per block was most
        // of what building it cost.
        let mut edges: Vec<(Block, Block)> = Vec::new();
        let mut leaving = vec![0u32; counts.blocks + 1];
        let mut arriving = vec![0u32; counts.blocks + 1];

        // The terminator and nothing else, which is where the invariant the verifier proves
        // gets spent: a block has exactly one terminator and it is the last instruction, so
        // this reads one instruction per block instead of all of them.
        //
        // It also means `block_addr` is not read here, and it must not be. The verifier counts
        // a block whose address is taken as a successor of the block that took it, which can
        // only add predecessors and so only take dominators away, which makes the verifier's
        // check stricter and never looser. That is not the graph. Control arrives at such a
        // block from an `indirect_br`, that instruction is a terminator, and it lists every
        // block the address can hold, so the edge is already here from the place control
        // really leaves.
        let mut stamp = vec![usize::MAX; counts.blocks];
        for block in func.blocks() {
            let Some(term) = func.terminator(block) else { continue };
            for call in func.successors(term) {
                // A `switch` with two labels on one arm names that block twice, and so does a
                // `br_if` whose arms agree. Twice is right for an edge, because the two edges
                // can carry different arguments, and wrong for a graph, where it would make a
                // block look like it had two predecessors when it has one. The edge list lives
                // in `Func::target_list` and that is what edge splitting reads. This answers
                // which blocks, so it says each one once.
                if stamp[call.block.index()] == block.index() {
                    continue;
                }
                stamp[call.block.index()] = block.index();
                edges.push((block, call.block));
                leaving[block.index() + 1] += 1;
                arriving[call.block.index() + 1] += 1;
            }
        }
        let succs = Lists::new(leaving, &edges, |(from, to)| (from, to));
        let preds = Lists::new(arriving, &edges, |(from, to)| (to, from));

        let entry = func.entry();
        let postorder = match entry {
            Some(entry) => postorder(&succs, entry, counts.blocks),
            None => Vec::new(),
        };
        let mut rank = vec![None; counts.blocks];
        for (index, &block) in postorder.iter().rev().enumerate() {
            rank[block.index()] = Some(index as u32);
        }

        Self { succs, preds, postorder, rank, entry }
    }

    /// Where control arrives, which is `None` for a function that is only declared.
    #[must_use]
    pub fn entry(&self) -> Option<Block> {
        self.entry
    }

    /// The blocks this one branches to, each named once however many edges go to it.
    #[must_use]
    pub fn successors(&self, block: Block) -> &[Block] {
        self.succs.of(block)
    }

    /// The blocks that branch to this one, each named once however many edges come from it.
    #[must_use]
    pub fn predecessors(&self, block: Block) -> &[Block] {
        self.preds.of(block)
    }

    /// Every block the entry reaches, children before parents.
    ///
    /// This is the order to run a backwards analysis in, and reversing it is the order to run a
    /// forwards one in. It is computed here rather than in each pass that wants it, which is
    /// how a compiler avoids acquiring six traversals that differ in ways nobody wrote down.
    #[must_use]
    pub fn postorder(&self) -> &[Block] {
        &self.postorder
    }

    /// Every block the entry reaches, parents before children.
    ///
    /// A block appears after at least one of its predecessors, and after all of them when the
    /// graph has no back edges. That is what makes it the order a forwards fixed point settles
    /// in fastest.
    pub fn reverse_postorder(&self) -> impl DoubleEndedIterator<Item = Block> + use<'_> {
        self.postorder.iter().rev().copied()
    }

    /// Where a block sits in reverse postorder, and `None` for one the entry does not reach.
    #[must_use]
    pub fn rank(&self, block: Block) -> Option<u32> {
        self.rank[block.index()]
    }

    /// Whether control can arrive at this block at all.
    ///
    /// An unreachable block is not an error and the front end makes them constantly: the block
    /// after a `return`, the arm of an `if` on a constant, the code after
    /// `__builtin_unreachable`. Section 6.5 of the design states the rule for the whole
    /// optimizer, which is that such a block is invisible to every analysis and every
    /// transformation, and is deleted by CFG simplification rather than by whoever noticed it.
    /// A pass that deletes blocks as a side effect of doing something else is a pass whose fuel
    /// accounting is wrong and whose dumps cannot be read.
    #[must_use]
    pub fn reaches(&self, block: Block) -> bool {
        self.rank[block.index()].is_some()
    }

    /// How many blocks the function has room for, counting the removed ones.
    ///
    /// This is the length of every array indexed by block number, and is what somebody sizing
    /// their own array wants. It is not how many blocks there are.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.rank.len()
    }
}

/// A list of blocks for each block number, laid end to end.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Lists {
    /// Where each block's list starts, with one more on the end for where the last one stops.
    at: Vec<u32>,
    /// The lists.
    blocks: Vec<Block>,
}

impl Lists {
    /// Files each pair's second block under its first, keeping the order the pairs come in.
    ///
    /// `sizes` is how many each block gets, shifted up one place, which is the start of each list
    /// once it is added up.
    fn new(
        mut sizes: Vec<u32>,
        pairs: &[(Block, Block)],
        pick: impl Fn((Block, Block)) -> (Block, Block),
    ) -> Self {
        for index in 1..sizes.len() {
            sizes[index] += sizes[index - 1];
        }
        let mut next = sizes.clone();
        let mut blocks = vec![Block::from_usize(0); pairs.len()];
        for &pair in pairs {
            let (under, block) = pick(pair);
            blocks[next[under.index()] as usize] = block;
            next[under.index()] += 1;
        }
        Self { at: sizes, blocks }
    }

    fn of(&self, block: Block) -> &[Block] {
        &self.blocks[self.at[block.index()] as usize..self.at[block.index() + 1] as usize]
    }
}

/// Every block the entry reaches, children before parents.
///
/// An explicit stack rather than recursion, because a chain of blocks is as long as the
/// function is and a straight line of ten thousand statements is a real program.
fn postorder(succs: &Lists, entry: Block, blocks: usize) -> Vec<Block> {
    let mut order = Vec::new();
    let mut seen = vec![false; blocks];
    let mut stack = vec![(entry, 0usize)];
    seen[entry.index()] = true;
    while let Some((block, next)) = stack.pop() {
        match succs.of(block).get(next) {
            Some(&target) => {
                stack.push((block, next + 1));
                if !seen[target.index()] {
                    seen[target.index()] = true;
                    stack.push((target, 0));
                }
            }
            None => order.push(block),
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use rucc_ir::{Block, Func, Signature};

    use crate::cfg::Cfg;
    use crate::testing::{computed_goto, graph};

    /// The successors of a block, as block numbers, in the order the graph holds them.
    fn succs(cfg: &Cfg, block: usize) -> Vec<usize> {
        cfg.successors(Block::from_usize(block)).iter().map(|b| b.index()).collect()
    }

    /// The predecessors of a block, as block numbers, sorted so the test can name a set.
    fn preds(cfg: &Cfg, block: usize) -> Vec<usize> {
        let mut list: Vec<usize> =
            cfg.predecessors(Block::from_usize(block)).iter().map(|b| b.index()).collect();
        list.sort_unstable();
        list
    }

    #[test]
    fn a_straight_line_goes_one_way() {
        let func = graph(&[&[1], &[2], &[]]);
        let cfg = Cfg::new(&func);
        assert_eq!(succs(&cfg, 0), [1]);
        assert_eq!(succs(&cfg, 2), []);
        assert_eq!(preds(&cfg, 0), []);
        assert_eq!(preds(&cfg, 2), [1]);
        assert_eq!(cfg.postorder().iter().map(|b| b.index()).collect::<Vec<_>>(), [2, 1, 0]);
    }

    #[test]
    fn a_join_has_both_arms_as_predecessors() {
        let func = graph(&[&[1, 2], &[3], &[3], &[]]);
        let cfg = Cfg::new(&func);
        assert_eq!(preds(&cfg, 3), [1, 2]);
        assert_eq!(cfg.rank(Block::from_usize(0)), Some(0));
    }

    #[test]
    fn two_arms_of_one_branch_to_one_block_is_one_predecessor() {
        // The block is named twice by the terminator and once by the graph. Counting it twice
        // would make a pass that merges a block into its only predecessor decline this one for
        // the wrong reason, and the right reason is that the predecessor has two successors,
        // which it does not.
        let func = graph(&[&[1, 1], &[]]);
        let cfg = Cfg::new(&func);
        assert_eq!(succs(&cfg, 0), [1]);
        assert_eq!(preds(&cfg, 1), [0]);
    }

    #[test]
    fn a_block_nothing_branches_to_is_not_reached() {
        let func = graph(&[&[1], &[], &[2]]);
        let cfg = Cfg::new(&func);
        assert!(cfg.reaches(Block::from_usize(1)));
        // Block 2 branches to itself and nothing branches to it. A postorder walk that started
        // anywhere other than the entry would spin here, which is the whole reason the walk
        // starts at the entry and carries a seen set rather than trusting the shape.
        assert!(!cfg.reaches(Block::from_usize(2)));
        assert!(cfg.rank(Block::from_usize(2)).is_none());
        assert_eq!(cfg.postorder().len(), 2);
    }

    #[test]
    fn a_back_edge_is_an_edge_like_any_other() {
        let func = graph(&[&[1], &[2, 3], &[1], &[]]);
        let cfg = Cfg::new(&func);
        assert_eq!(preds(&cfg, 1), [0, 2]);
        // Reverse postorder puts a block after one of its predecessors, and the header's other
        // predecessor is the latch, which comes later. That is what a back edge is.
        let order: Vec<usize> = cfg.reverse_postorder().map(|b| b.index()).collect();
        assert_eq!(order[0], 0);
        assert!(order.iter().position(|&b| b == 1) < order.iter().position(|&b| b == 2));
    }

    #[test]
    fn taking_the_address_of_a_block_is_not_an_edge_to_it() {
        // Block 0 takes block 2's address and block 1 is the only thing that branches there.
        // The verifier counts the first as an edge on purpose. The graph must not, or a pass
        // would think block 2 had a predecessor that never branches anywhere.
        let func = computed_goto();
        let cfg = Cfg::new(&func);
        assert_eq!(preds(&cfg, 2), [1]);
        assert!(cfg.reaches(Block::from_usize(2)));
    }

    #[test]
    fn a_declaration_has_no_graph_and_says_so() {
        let func = Func::new(rucc_base::Interner::new().intern("f"), Signature::new());
        let cfg = Cfg::new(&func);
        assert!(cfg.entry().is_none());
        assert!(cfg.postorder().is_empty());
        assert_eq!(cfg.capacity(), 0);
    }
}
