//! Where a dominator stops dominating, forwards and backwards.
//!
//! Design: `spec/optimizer/06-cfg-and-dominators.md` section 6.3, and document 17 for the
//! consumer of the backwards half.
//!
//! The dominance frontier of a block is where its influence ends. Block `a` is in the frontier
//! of block `b` when `b` dominates a predecessor of `a` and does not strictly dominate `a`
//! itself, which is to say `a` is the first place control can arrive without having gone
//! through `b`. Run the same definition on the graph with every arrow turned around and it
//! answers a different question: the post-dominance frontier of a block is the set of branches
//! that decide whether the block runs at all. That is the control dependence relation, and
//! [`ControlDependence`] is the name it goes by, because that is what a pass asking for it
//! wants and nobody wants the frontier of a reversed graph for its own sake.
//!
//! # Why both are here and why they are one algorithm
//!
//! Section 6.3 says the forward frontier is probably not needed, and the reason is sound: the
//! classical consumer is Cytron's SSA construction, and rucc builds SSA during lowering, so that
//! consumer does not exist here. The backwards one is needed, by aggressive dead code
//! elimination in document 17 and by if-conversion in document 22, and it cannot be written
//! without writing the forward one, because they are the same six lines over two graphs. Given
//! that, the forward one costs a wrapper and a doc comment, and it buys a test: the two are
//! checked against a direct reading of the definition, and a mistake in the shared walk shows
//! up twice rather than once.
//!
//! What is deliberately not here is Cytron's iterated frontier. It exists to place phi nodes and
//! nothing else in this compiler places phi nodes.
//!
//! # The invented edges
//!
//! [`PostDominators`] adds an edge to the exit from every block that has no path to one, which
//! is how an infinite loop gets a post-dominator at all. Those edges are in this relation too,
//! so a block at the far end of an infinite loop can come out control dependent on a branch it
//! is not really control dependent on. That is the safe direction for every consumer there is:
//! a pass that keeps something because it might matter is slow, and one that removes an infinite
//! loop because nothing after it runs is wrong. [`PostDominators::fake_exits`] is public so a
//! pass that wants to know can ask.

use rucc_ir::Block;

use crate::{Cfg, Dominators, PostDominators};

/// Where each block stops dominating, by block number.
///
/// The lists are sorted by block number and hold no duplicates, so two of these compare equal
/// when they say the same thing, which is what the analysis cache needs of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontiers {
    of: Vec<Vec<Block>>,
}

impl Frontiers {
    /// Builds the frontier of every block.
    ///
    /// The cost is the size of the answer plus the size of the graph, because the walk from a
    /// predecessor stops at the immediate dominator of the block it started for, and every step
    /// it takes writes one entry.
    #[must_use]
    pub fn new(cfg: &Cfg, doms: &Dominators) -> Self {
        let mut of = vec![Vec::new(); cfg.capacity()];
        for &block in cfg.postorder() {
            // A block with one way in is not a place two paths meet, and the entry is, because
            // control also arrives there from outside the function. Leaving that out is the one
            // mistake in this algorithm that a small test does not catch, since it only shows up
            // on a loop whose back edge goes to the entry itself.
            //
            // A predecessor control never reaches is not a way in. Section 6.5 says such a block
            // is invisible to every analysis, and counting one here would put a frontier on a
            // block that has no dominators for the walk to climb.
            let arriving = || cfg.predecessors(block).iter().copied().filter(|&p| cfg.reaches(p));
            let arrivals = arriving().count() + usize::from(Some(block) == cfg.entry());
            if arrivals < 2 {
                continue;
            }
            let stop = doms.immediate_dominator(block);
            for pred in arriving() {
                let mut runner = pred;
                while Some(runner) != stop {
                    of[runner.index()].push(block);
                    match doms.immediate_dominator(runner) {
                        Some(next) => runner = next,
                        // The entry, which happens when `stop` is `None` because `block` is the
                        // entry as well. The entry is in its own frontier there and that is the
                        // right answer, since it does not strictly dominate itself.
                        None => break,
                    }
                }
            }
        }
        for list in &mut of {
            list.sort_unstable_by_key(|b: &Block| b.index());
            list.dedup();
        }
        Self { of }
    }

    /// The blocks control can first arrive at without having gone through this one.
    ///
    /// Empty for a block the entry does not reach, and for one whose influence covers everything
    /// below it, which is every block on a path with no branches.
    #[must_use]
    pub fn of(&self, block: Block) -> &[Block] {
        self.of.get(block.index()).map_or(&[][..], Vec::as_slice)
    }
}

/// Which branches decide whether a block runs.
///
/// This is the post-dominance frontier under the name a pass would look for. A block is control
/// dependent on a branch when one arm of the branch always reaches it and the other does not
/// have to, so the branch is what a pass has to keep in order to keep the block meaningful.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControlDependence {
    on: Vec<Vec<Block>>,
}

impl ControlDependence {
    /// Builds the relation for every block.
    #[must_use]
    pub fn new(cfg: &Cfg, post: &PostDominators) -> Self {
        let mut on = vec![Vec::new(); cfg.capacity()];
        for &block in cfg.postorder() {
            // The same join test as above, on the reversed graph, where what arrives at a block
            // is what the block branches to. A block with two successors is a branch, and a
            // block with an invented edge to the exit counts that edge, which is what puts an
            // infinite loop in this relation at all.
            let invented = post.fake_exits().contains(&block) || cfg.successors(block).is_empty();
            let arrivals = cfg.successors(block).len() + usize::from(invented);
            if arrivals < 2 {
                continue;
            }
            let stop = post.immediate_post_dominator(block);
            for &succ in cfg.successors(block) {
                let mut runner = succ;
                while Some(runner) != stop {
                    on[runner.index()].push(block);
                    match post.immediate_post_dominator(runner) {
                        Some(next) => runner = next,
                        // The walk arrived at the exit. Nothing above the exit is a block, so
                        // there is nothing further to record whatever `stop` was.
                        None => break,
                    }
                }
            }
        }
        for list in &mut on {
            list.sort_unstable_by_key(|b: &Block| b.index());
            list.dedup();
        }
        Self { on }
    }

    /// The blocks whose terminator decides whether this one runs.
    ///
    /// Empty for a block that runs whenever the function runs, which is the entry and every
    /// block that post-dominates it.
    #[must_use]
    pub fn on(&self, block: Block) -> &[Block] {
        self.on.get(block.index()).map_or(&[][..], Vec::as_slice)
    }

    /// Whether this block runs whatever any branch decides.
    #[must_use]
    pub fn unconditional(&self, block: Block) -> bool {
        self.on(block).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{ControlDependence, Frontiers};
    use crate::testing::graph;
    use crate::{Cfg, Dominators, PostDominators};
    use rucc_ir::{Block, Signature};

    /// The frontier of every block, read straight off the definition.
    ///
    /// Quadratic in the number of blocks and cubic with the predecessor walk, which is why it is
    /// only here. It is the oracle: the algorithm above is an optimization of this and the tests
    /// hold it to exactly this answer.
    fn by_definition(cfg: &Cfg, doms: &Dominators) -> Vec<Vec<Block>> {
        let mut out = vec![Vec::new(); cfg.capacity()];
        for &of in cfg.postorder() {
            for &block in cfg.postorder() {
                // `dominates` is already false for a predecessor nothing reaches, so the filter
                // the algorithm needs is not written again here.
                let reaches_a_pred =
                    cfg.predecessors(block).iter().any(|&pred| doms.dominates(of, pred));
                if reaches_a_pred && !doms.strictly_dominates(of, block) {
                    out[of.index()].push(block);
                }
            }
        }
        for list in &mut out {
            list.sort_unstable_by_key(|b: &Block| b.index());
        }
        out
    }

    /// The same, for post-dominance, which is the control dependence relation with the two ends
    /// the other way round.
    fn control_by_definition(cfg: &Cfg, post: &PostDominators) -> Vec<Vec<Block>> {
        let mut out = vec![Vec::new(); cfg.capacity()];
        for &of in cfg.postorder() {
            for &block in cfg.postorder() {
                let reaches_a_succ =
                    cfg.successors(block).iter().any(|&succ| post.post_dominates(of, succ));
                if reaches_a_succ && !post.strictly_post_dominates(of, block) {
                    out[of.index()].push(block);
                }
            }
        }
        for list in &mut out {
            list.sort_unstable_by_key(|b: &Block| b.index());
        }
        out
    }

    fn frontiers(edges: &[&[usize]]) -> Vec<Vec<usize>> {
        let func = graph(edges);
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        let built = Frontiers::new(&cfg, &doms);
        (0..edges.len())
            .map(|index| built.of(Block::from_usize(index)).iter().map(|b| b.index()).collect())
            .collect()
    }

    fn depends(edges: &[&[usize]]) -> Vec<Vec<usize>> {
        let func = graph(edges);
        let cfg = Cfg::new(&func);
        let post = PostDominators::new(&cfg);
        let built = ControlDependence::new(&cfg, &post);
        (0..edges.len())
            .map(|index| built.on(Block::from_usize(index)).iter().map(|b| b.index()).collect())
            .collect()
    }

    #[test]
    fn a_chain_of_blocks_has_no_frontier_anywhere() {
        // Nothing joins, so no block ever stops dominating what comes after it.
        assert_eq!(frontiers(&[&[1], &[2], &[]]), vec![vec![], vec![], vec![]]);
    }

    #[test]
    fn the_two_arms_of_a_branch_meet_at_the_block_after_it() {
        // 0 branches to 1 and 2, both go to 3. Each arm stops mattering at 3 and the branch
        // itself dominates all of it.
        let df = frontiers(&[&[1, 2], &[3], &[3], &[]]);
        assert_eq!(df, vec![vec![], vec![3], vec![3], vec![]]);
    }

    #[test]
    fn a_loop_header_is_in_its_own_frontier() {
        // 0 -> 1, 1 branches to 1 and 2. The body of the loop stops dominating at the header,
        // which is what makes the header the place a value defined in the body needs a
        // parameter.
        let df = frontiers(&[&[1], &[1, 2], &[]]);
        assert_eq!(df, vec![vec![], vec![1], vec![]]);
    }

    #[test]
    fn a_back_edge_to_the_entry_puts_the_entry_in_its_own_frontier() {
        // The case the arrivals count is written for. Block 0 has one predecessor and is still
        // a join, because control also arrives from outside the function.
        let df = frontiers(&[&[1, 2], &[0], &[]]);
        assert_eq!(df, vec![vec![0], vec![0], vec![]]);
    }

    #[test]
    fn the_frontier_is_what_the_definition_says_on_every_graph_the_design_names() {
        for edges in shapes() {
            let func = graph(edges);
            let cfg = Cfg::new(&func);
            let doms = Dominators::new(&cfg);
            let built = Frontiers::new(&cfg, &doms);
            let wanted = by_definition(&cfg, &doms);
            for &block in cfg.postorder() {
                assert_eq!(
                    built.of(block),
                    wanted[block.index()].as_slice(),
                    "block {} of {edges:?}",
                    block.index()
                );
            }
        }
    }

    #[test]
    fn control_dependence_is_what_the_definition_says_on_every_graph_the_design_names() {
        for edges in shapes() {
            let func = graph(edges);
            let cfg = Cfg::new(&func);
            let post = PostDominators::new(&cfg);
            let built = ControlDependence::new(&cfg, &post);
            let wanted = control_by_definition(&cfg, &post);
            for &block in cfg.postorder() {
                assert_eq!(
                    built.on(block),
                    wanted[block.index()].as_slice(),
                    "block {} of {edges:?}",
                    block.index()
                );
            }
        }
    }

    #[test]
    fn only_the_arms_of_a_branch_depend_on_it() {
        // 0 branches to 1 and 2, both go to 3. The arms run because of the branch and 3 runs
        // whatever the branch decided.
        let cd = depends(&[&[1, 2], &[3], &[3], &[]]);
        assert_eq!(cd, vec![vec![], vec![0], vec![0], vec![]]);
    }

    #[test]
    fn a_block_that_always_runs_depends_on_nothing() {
        let func = graph(&[&[1], &[2], &[]]);
        let cfg = Cfg::new(&func);
        let post = PostDominators::new(&cfg);
        let cd = ControlDependence::new(&cfg, &post);
        for index in 0..3 {
            assert!(cd.unconditional(Block::from_usize(index)), "block {index}");
        }
    }

    #[test]
    fn a_loop_body_depends_on_the_test_that_ends_the_loop() {
        // 0 -> 1, 1 branches to 2 and 3, 2 -> 1, 3 returns. The body and the latch run because
        // the test said so, and the test itself is in the loop, so it depends on itself.
        let cd = depends(&[&[1], &[2, 3], &[1], &[]]);
        assert_eq!(cd, vec![vec![], vec![1], vec![1], vec![]]);
    }

    #[test]
    fn one_arm_falling_through_still_depends_on_the_branch() {
        // 0 branches to 1 and 2, 1 goes to 2, 2 returns. Nothing joins on the other side, so 1
        // is the only block the branch decides.
        let cd = depends(&[&[1, 2], &[2], &[]]);
        assert_eq!(cd, vec![vec![], vec![0], vec![]]);
    }

    #[test]
    fn nothing_after_an_infinite_loop_is_forgotten() {
        // 0 branches to 1 and 2, 1 loops on itself forever, 2 returns. The loop has no path to
        // the exit, so post-dominance is only defined for it through an invented edge, and the
        // relation still has to come out of the walk rather than out of a panic.
        //
        // Block 1 depends on itself as well as on the branch, which is the invented edge
        // showing through: in the reversed graph the loop is a place two ways in meet. That is
        // the conservative direction and it is what keeps a pass from deleting the loop.
        let func = graph(&[&[1, 2], &[1], &[]]);
        let cfg = Cfg::new(&func);
        let post = PostDominators::new(&cfg);
        assert_eq!(post.fake_exits(), [Block::from_usize(1)]);
        assert_eq!(depends(&[&[1, 2], &[1], &[]]), vec![vec![], vec![0, 1], vec![0]]);
    }

    #[test]
    fn a_switch_puts_every_arm_on_the_block_that_chose_it() {
        // Four ways out of block 0, all meeting at 4.
        let cd = depends(&[&[1, 2, 3, 4], &[4], &[4], &[4], &[]]);
        assert_eq!(cd, vec![vec![], vec![0], vec![0], vec![0], vec![]]);
    }

    #[test]
    fn a_declaration_has_a_frontier_like_anything_else() {
        // No blocks, so no answers, and no panic on the way to saying so.
        let func = rucc_ir::Func::new(rucc_base::Interner::new().intern("f"), Signature::new());
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        let post = PostDominators::new(&cfg);
        assert!(Frontiers::new(&cfg, &doms).of(Block::from_usize(0)).is_empty());
        assert!(ControlDependence::new(&cfg, &post).on(Block::from_usize(0)).is_empty());
    }

    /// Shapes that between them have a branch, a join, a loop, a switch, an irreducible region,
    /// a back edge to the entry, an infinite loop and an unreachable block.
    ///
    /// The point of the list is that the two exhaustive tests above run over all of it, so a
    /// shape added here is a shape both relations are checked on.
    fn shapes() -> Vec<&'static [&'static [usize]]> {
        vec![
            &[&[1], &[2], &[]],
            &[&[1, 2], &[3], &[3], &[]],
            &[&[1], &[1, 2], &[]],
            &[&[1, 2], &[0], &[]],
            &[&[1, 2], &[2], &[]],
            &[&[1, 2, 3, 4], &[4], &[4], &[4], &[]],
            &[&[1], &[2, 3], &[1], &[]],
            // Irreducible: two ways into a two block cycle, so neither block of it dominates
            // the other.
            &[&[1, 2], &[2], &[1, 3], &[]],
            // An unreachable block, which every relation here has to ignore rather than trip
            // over.
            &[&[1], &[], &[1]],
            // Nested branches meeting at two different places.
            &[&[1, 4], &[2, 3], &[4], &[4], &[]],
            // An infinite loop, which only has post-dominators through an invented edge.
            &[&[1, 2], &[1], &[]],
            // Two of them, so the walk meets more than one invented edge.
            &[&[1, 2], &[1], &[3, 4], &[3], &[]],
        ]
    }
}
