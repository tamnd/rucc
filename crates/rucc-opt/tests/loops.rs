//! The loop forest against the definition in section 7.1 of
//! `spec/optimizer/07-loops-and-scev.md`, on graphs nobody chose.
//!
//! The analysis in `loops.rs` does not find loops the way the design document describes them. The
//! document says a back edge is an edge whose head dominates its tail and the natural loop of a
//! back edge is the set of blocks that reach the tail without passing through the head. The
//! implementation takes strongly connected components and asks which of them have a header. The
//! two are the same set of loops, and this is where that is checked, on random graphs, against
//! the definition written out literally.
//!
//! That is the point of writing the definition out a second time here. A property test that
//! checks an implementation against a paraphrase of itself finds nothing.

// The same builder the unit tests use, read rather than copied, so the two cannot describe
// different graphs.
#[path = "../src/testing.rs"]
#[allow(dead_code)]
mod testing;

use std::collections::BTreeSet;

use rucc_ir::Block;
use rucc_opt::{Cfg, Dominators, Loops};

use crate::testing::graph;

/// Block number `n`, spelled the way the tests read.
fn b(n: usize) -> Block {
    Block::from_usize(n)
}

#[test]
fn every_loop_is_the_natural_loop_of_its_header_on_a_thousand_random_graphs() {
    let mut random = Random::new(0x10ad_5eed_c0ff_ee01);
    let mut with_loops = 0;
    for _ in 0..1000 {
        let edges = random.graph();
        let lists: Vec<&[usize]> = edges.iter().map(Vec::as_slice).collect();
        let func = graph(&lists);
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);

        // The forest checks out on its own terms first, because a malformed forest would make
        // everything below it meaningless rather than failing.
        assert_eq!(loops.problems(&cfg, &doms), Vec::<String>::new(), "in {edges:?}");

        // Every header the analysis reports is a back edge head. Nesting does not weaken this:
        // an inner loop's header has its own back edge, and finding it is the whole reason the
        // search recurses.
        let found: BTreeSet<usize> = loops.all().map(|id| loops.header(id).index()).collect();
        let expected = headers(&edges, &cfg, &doms);
        assert!(found.is_subset(&expected), "headers in {edges:?}: {found:?} against {expected:?}");
        if !found.is_empty() {
            with_loops += 1;
        }

        // And on a graph with no irreducible region in it, the two are the same set. The
        // direction that can fail is a back edge inside an irreducible region, whose head the
        // analysis declines to call a header on purpose, and which is what
        // `loops::tests::a_self_loop_inside_an_irreducible_region_is_declined_along_with_it`
        // pins down.
        if loops.irreducible().is_empty() {
            assert_eq!(found, expected, "headers in {edges:?}");
        } else {
            // Whatever it declined is in the region it declined, so a loop pass that checks the
            // irreducible set does not walk past it by accident.
            for header in expected.difference(&found) {
                assert!(
                    loops.is_irreducible(b(*header)),
                    "back edge head {header} is neither a loop nor irreducible in {edges:?}"
                );
            }
        }

        // And each of them holds exactly the blocks the definition puts in it.
        for id in loops.all() {
            let header = loops.header(id).index();
            let mut body: Vec<usize> = loops.blocks(id).iter().map(|block| block.index()).collect();
            body.sort_unstable();
            let body: BTreeSet<usize> = body.into_iter().collect();
            assert_eq!(body, natural(&edges, &cfg, &doms, header), "loop at {header} in {edges:?}");
        }

        // Nothing that goes round is left unaccounted for. A block in a cycle is in a loop, or
        // it is in a cycle with no header and the analysis says so, and a loop pass that checks
        // both is a loop pass that cannot be handed a region it does not understand.
        for block in cfg.postorder() {
            if !in_a_cycle(&edges, block.index()) {
                continue;
            }
            assert!(
                loops.innermost(*block).is_some() || loops.is_irreducible(*block),
                "block {} goes round and is neither in a loop nor irreducible in {edges:?}",
                block.index()
            );
        }
    }
    assert!(with_loops > 100, "only {with_loops} of a thousand graphs had a loop in them");
}

#[test]
fn the_innermost_loop_of_a_block_is_the_deepest_one_holding_it() {
    let mut random = Random::new(0xdeed_beef_1357_9bdf);
    for _ in 0..1000 {
        let edges = random.graph();
        let lists: Vec<&[usize]> = edges.iter().map(Vec::as_slice).collect();
        let func = graph(&lists);
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);

        for block in cfg.postorder().iter().copied() {
            // Every loop whose recorded body holds the block, taken the slow way.
            let holding: Vec<_> =
                loops.all().filter(|&id| loops.blocks(id).contains(&block)).collect();
            let deepest = holding.iter().copied().max_by_key(|&id| loops.depth(id));
            assert_eq!(loops.innermost(block), deepest, "block {} in {edges:?}", block.index());

            // `contains` walks the parent chain rather than the block list, so it is worth
            // checking that the two agree.
            for id in loops.all() {
                assert_eq!(
                    loops.contains(id, block),
                    holding.contains(&id),
                    "loop {} against block {} in {edges:?}",
                    id.index(),
                    block.index()
                );
            }
        }
    }
}

#[test]
fn a_preheader_is_the_only_way_in_and_leads_nowhere_else() {
    let mut random = Random::new(0x9fe1_dead_1234_5678);
    let mut found = 0;
    for _ in 0..1000 {
        let edges = random.graph();
        let lists: Vec<&[usize]> = edges.iter().map(Vec::as_slice).collect();
        let func = graph(&lists);
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);

        for id in loops.all() {
            let header = loops.header(id);
            let outside: Vec<Block> = cfg
                .predecessors(header)
                .iter()
                .copied()
                .filter(|&pred| !loops.contains(id, pred))
                .collect();
            match loops.preheader(&cfg, id) {
                Some(preheader) => {
                    found += 1;
                    assert_eq!(outside, [preheader], "in {edges:?}");
                    assert_eq!(cfg.successors(preheader), [header], "in {edges:?}");
                }
                // Either more than one way in, or the one way in also goes somewhere else, and
                // both are things the canonicalizer fixes by splitting an edge.
                None => assert!(
                    outside.len() != 1 || cfg.successors(outside[0]).len() != 1,
                    "loop at {} in {edges:?} has a preheader and was told it does not",
                    header.index()
                ),
            }
        }
    }
    assert!(found > 50, "only {found} loops out of a thousand graphs had a preheader");
}

/// Every block with a back edge arriving at it, which is the definition's set of loop headers.
///
/// A back edge is an edge whose head dominates its tail. Unreachable blocks are not in the graph
/// as far as any analysis is concerned, per section 6.5, so they are not in this either.
fn headers(edges: &[Vec<usize>], cfg: &Cfg, doms: &Dominators) -> BTreeSet<usize> {
    let mut found = BTreeSet::new();
    for (tail, targets) in edges.iter().enumerate() {
        if !cfg.reaches(b(tail)) {
            continue;
        }
        for &head in targets {
            if doms.dominates(b(head), b(tail)) {
                found.insert(head);
            }
        }
    }
    found
}

/// The natural loop of the back edges arriving at this header.
///
/// The header, plus every block that reaches one of its latches without passing through it.
/// Written as a walk backwards from the latches that refuses to step onto the header, which is
/// the same thing and does not need paths enumerated.
fn natural(edges: &[Vec<usize>], cfg: &Cfg, doms: &Dominators, header: usize) -> BTreeSet<usize> {
    let mut body = BTreeSet::new();
    body.insert(header);
    let mut stack: Vec<usize> = (0..edges.len())
        .filter(|&tail| cfg.reaches(b(tail)) && edges[tail].contains(&header))
        .filter(|&tail| doms.dominates(b(header), b(tail)))
        .collect();
    let mut seen: Vec<bool> = vec![false; edges.len()];
    for &tail in &stack {
        seen[tail] = true;
        body.insert(tail);
    }
    // A latch that is the header itself is a block branching to itself, and the loop is that one
    // block. Walking backwards from it would step out through the header, which is exactly what
    // "without passing through the head" forbids.
    seen[header] = true;
    stack.retain(|&tail| tail != header);
    while let Some(block) = stack.pop() {
        for (pred, targets) in edges.iter().enumerate() {
            // Unreachable blocks branch into the graph without being part of it, so a walk
            // backwards has to refuse them the way a walk forwards never sees them.
            if pred == header || seen[pred] || !targets.contains(&block) || !cfg.reaches(b(pred)) {
                continue;
            }
            seen[pred] = true;
            body.insert(pred);
            stack.push(pred);
        }
    }
    body
}

/// Whether control can get from this block back to it.
fn in_a_cycle(edges: &[Vec<usize>], from: usize) -> bool {
    let mut seen = vec![false; edges.len()];
    let mut stack = edges[from].clone();
    while let Some(block) = stack.pop() {
        if block == from {
            return true;
        }
        if seen[block] {
            continue;
        }
        seen[block] = true;
        stack.extend_from_slice(&edges[block]);
    }
    false
}

/// The generator, which is here rather than in a dependency because this crate has none.
///
/// The same xorshift as `tests/dominators.rs`, with its own seeds written down so a failure is
/// reproducible from the test name alone.
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
    /// unreachable blocks, self loops, nested loops and irreducible regions without having to be
    /// asked for any of them.
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
