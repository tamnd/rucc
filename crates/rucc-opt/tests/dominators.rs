//! The six graphs section 6.6 of `spec/optimizer/06-cfg-and-dominators.md` asks for, with the
//! dominator tree of each written out by hand, and the property test that checks the analysis
//! against the definition rather than against another implementation of itself.
//!
//! These build the graphs through the crate's public API, the way any other caller would, and
//! share only the builder that turns an edge list into a function.

// The builder these are written against is the one the unit tests use, read directly rather
// than copied, so a change to it cannot leave one set of tests describing a different graph
// from the other. It is compiled into the test build of the crate rather than published.
#[path = "../src/testing.rs"]
#[allow(dead_code)]
mod testing;

use rucc_ir::Block;
use rucc_opt::{Cfg, Dominators, PostDominators};

use crate::testing::graph;

/// Block number `n`, spelled the way the tests read.
fn b(n: usize) -> Block {
    Block::from_usize(n)
}

/// The immediate dominator of every block in order, as block numbers.
fn tree(edges: &[&[usize]]) -> Vec<Option<usize>> {
    let func = graph(edges);
    let doms = Dominators::new(&Cfg::new(&func));
    (0..edges.len()).map(|n| doms.immediate_dominator(b(n)).map(|d| d.index())).collect()
}

#[test]
fn a_straight_line() {
    // 0 -> 1 -> 2 -> 3, and the tree is the same line.
    assert_eq!(tree(&[&[1], &[2], &[3], &[]]), [None, Some(0), Some(1), Some(2)]);
}

#[test]
fn a_diamond() {
    // 0 branches to 1 and 2, both of which go to 3. Every one of them is dominated by 0 and
    // nothing else, because either arm can be the one that runs.
    assert_eq!(tree(&[&[1, 2], &[3], &[3], &[]]), [None, Some(0), Some(0), Some(0)]);
}

#[test]
fn a_natural_loop_with_two_predecessors_on_the_header() {
    // 0 -> 1, 1 branches to the body 2 and the exit 3, and 2 goes back to 1. The header has a
    // predecessor from outside the loop and a predecessor from inside it, and is dominated by
    // the one from outside, which is what makes the other one a back edge.
    assert_eq!(tree(&[&[1], &[2, 3], &[1], &[]]), [None, Some(0), Some(1), Some(1)]);
}

#[test]
fn an_irreducible_two_entry_loop() {
    // 0 branches into the cycle at 1 or at 2, and the cycle runs 1 -> 2 -> 1. Neither candidate
    // header dominates the other, because each is reachable without the other, so both hang off
    // block 0 and the loop has no single header at all.
    //
    // Nothing here is a special case in the algorithm. The iterative fixed point converges on
    // any graph, it just takes a second sweep. What does not survive irreducibility is loop
    // analysis, and section 6.4 leaves that to document 07.
    let doms = Dominators::new(&Cfg::new(&graph(&[&[1, 2], &[2], &[1, 3], &[]])));
    assert_eq!(doms.immediate_dominator(b(1)), Some(b(0)));
    assert_eq!(doms.immediate_dominator(b(2)), Some(b(0)));
    assert!(!doms.dominates(b(1), b(2)));
    assert!(!doms.dominates(b(2), b(1)));
}

#[test]
fn an_unreachable_block_that_branches_to_itself() {
    // Block 2 is reached by nothing and goes to itself. A postorder walk that started from the
    // block list rather than from the entry, or one without a seen set, spins here forever.
    let cfg = Cfg::new(&graph(&[&[1], &[], &[2]]));
    let doms = Dominators::new(&cfg);
    assert!(!cfg.reaches(b(2)));
    assert_eq!(cfg.postorder().len(), 2);
    assert!(doms.immediate_dominator(b(2)).is_none());
    // Not dominated by the entry, and not a dominator of anything. The verifier calls an
    // unreachable block vacuously dominated so it is reported once rather than once per value
    // it uses. An optimizer answering that way would let a pass move a use of a value into a
    // place the value does not exist, which is section 6.5.
    assert!(!doms.dominates(b(0), b(2)));
    assert!(!doms.dominates(b(2), b(1)));
}

#[test]
fn an_entry_block_that_is_also_a_loop_header() {
    // The sixth graph, and the design says why it belongs here as a note rather than as an
    // assertion about dominance: a branch back to the entry breaks the invariant at the top of
    // `func.rs`, because the entry block's parameters are the function's arguments and a branch
    // would have to supply them. The verifier is what catches it, in
    // `verify::tests::a_branch_back_to_the_entry_block_is_reported`.
    //
    // Somebody will still write a pass that builds one, so the answer here should be the
    // sensible one rather than a crash or a loop: the entry dominates everything, including the
    // block that branches back to it.
    let doms = Dominators::new(&Cfg::new(&graph(&[&[1], &[0, 2], &[]])));
    assert_eq!(doms.immediate_dominator(b(0)), None);
    assert!(doms.dominates(b(0), b(1)));
    assert!(doms.dominates(b(1), b(2)));
}

#[test]
fn the_analysis_agrees_with_the_definition_on_a_thousand_random_graphs() {
    // The test that would catch a wrong meet, and a wrong meet is a silent wrong answer in
    // every pass downstream.
    //
    // The definition of `a` dominating `b` is that every path from the entry to `b` goes
    // through `a`. Enumerating paths is not needed to check it: taking the cycles out of a walk
    // leaves a path over a subset of its blocks, so a walk that avoids `a` exists exactly when
    // a path that avoids `a` does, and the question is the same as whether `b` is still
    // reachable once `a` is deleted from the graph. That is a different computation from the
    // one under test, which is what makes it worth running.
    let mut random = Random::new(0x5eed_1234_9abc_def0);
    for _ in 0..1000 {
        let edges = random.graph();
        let lists: Vec<&[usize]> = edges.iter().map(Vec::as_slice).collect();
        let func = graph(&lists);
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        for from in 0..edges.len() {
            if !cfg.reaches(b(from)) {
                continue;
            }
            let without = reachable(&edges, Some(from));
            for (to, &still_reached) in without.iter().enumerate() {
                if !cfg.reaches(b(to)) {
                    continue;
                }
                let expected = !still_reached;
                assert_eq!(
                    doms.dominates(b(from), b(to)),
                    expected,
                    "block {from} against block {to} in {edges:?}"
                );
            }
        }
    }
}

#[test]
fn post_dominance_agrees_with_the_definition_where_the_definition_applies() {
    // The same check backwards. `a` post-dominates `b` when every path from `b` out of the
    // function goes through `a`, which is the same as saying that with `a` deleted, `b` reaches
    // no block that leaves.
    //
    // Only graphs where every block does leave, because an infinite loop has no path out and
    // post-dominance for the blocks in it is whatever the fake edges of section 6.3 make it.
    // That part is checked by the unit tests that read `fake_exits`, and mixing it in here
    // would be checking the analysis against its own invention.
    let mut random = Random::new(0xfeed_face_0bad_c0de);
    let mut checked = 0;
    for _ in 0..1000 {
        let edges = random.graph();
        let lists: Vec<&[usize]> = edges.iter().map(Vec::as_slice).collect();
        let func = graph(&lists);
        let cfg = Cfg::new(&func);
        if !cfg.postorder().iter().all(|&block| leaves(&edges, block.index(), None)) {
            continue;
        }
        checked += 1;
        let posts = PostDominators::new(&cfg);
        assert!(posts.fake_exits().is_empty(), "nothing to invent an edge for in {edges:?}");
        for from in 0..edges.len() {
            if !cfg.reaches(b(from)) {
                continue;
            }
            for to in 0..edges.len() {
                if !cfg.reaches(b(to)) {
                    continue;
                }
                let expected = !leaves(&edges, to, Some(from));
                assert_eq!(
                    posts.post_dominates(b(from), b(to)),
                    expected,
                    "block {from} against block {to} in {edges:?}"
                );
            }
        }
    }
    assert!(checked > 100, "only {checked} of a thousand graphs had a way out of every block");
}

/// Which blocks the entry reaches with one of them deleted.
fn reachable(edges: &[Vec<usize>], deleted: Option<usize>) -> Vec<bool> {
    let mut seen = vec![false; edges.len()];
    if deleted == Some(0) {
        return seen;
    }
    let mut stack = vec![0];
    seen[0] = true;
    while let Some(block) = stack.pop() {
        for &next in &edges[block] {
            if !seen[next] && deleted != Some(next) {
                seen[next] = true;
                stack.push(next);
            }
        }
    }
    seen
}

/// Whether control can get out of the function from here with one block deleted.
///
/// Getting out is arriving at a block with no successors, which is what a `return` is in these
/// graphs.
fn leaves(edges: &[Vec<usize>], from: usize, deleted: Option<usize>) -> bool {
    if deleted == Some(from) {
        return false;
    }
    let mut seen = vec![false; edges.len()];
    let mut stack = vec![from];
    seen[from] = true;
    while let Some(block) = stack.pop() {
        if edges[block].is_empty() {
            return true;
        }
        for &next in &edges[block] {
            if !seen[next] && deleted != Some(next) {
                seen[next] = true;
                stack.push(next);
            }
        }
    }
    false
}

/// The generator, which is here rather than in a dependency because this crate has none.
///
/// Xorshift, which is not a good random number generator and is a perfectly good one for
/// picking a few thousand small integers. The seed is written down so a failure is reproducible
/// from the test name alone.
struct Random(u64);

impl Random {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn bits(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number below the bound.
    fn below(&mut self, bound: usize) -> usize {
        (self.bits() % bound as u64) as usize
    }

    /// An edge list of two to seven blocks, each branching to up to three of them.
    ///
    /// Blocks branch anywhere, including to themselves and backwards, so the run contains
    /// unreachable blocks, self loops, irreducible regions and infinite loops without having to
    /// be asked for any of them.
    fn graph(&mut self) -> Vec<Vec<usize>> {
        let blocks = 2 + self.below(6);
        (0..blocks)
            .map(|_| {
                let count = self.below(4);
                (0..count).map(|_| self.below(blocks)).collect()
            })
            .collect()
    }
}
