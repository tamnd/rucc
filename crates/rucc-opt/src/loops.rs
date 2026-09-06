//! The loop forest: what loops there are, how they nest, and what is in a cycle that is not one.
//!
//! Design: `spec/optimizer/07-loops-and-scev.md` sections 7.1 through 7.3 and 7.6.
//!
//! A back edge is an edge whose head dominates its tail, the natural loop of a back edge is the
//! set of blocks that reach the tail without passing through the head, and natural loops with
//! different headers are either disjoint or one contains the other. That is the textbook
//! construction and it needs the dominator tree and nothing else.
//!
//! What is built here is the same set of loops by a different route, because the textbook route
//! answers three questions with three walks and this one answers them with one. Take the
//! strongly connected components of the graph. A component with a cycle in it is either a
//! natural loop or an irreducible region, and which one it is comes down to a single question:
//! whether the nearest block dominating all of it is one of its own blocks. If it is, that block
//! is the header, every way into the component goes through it, and the component is exactly the
//! natural loop of the back edges arriving at it. If it is not, the component has two ways in
//! and is what `goto` into a loop body produces. Taking the header out and doing it again finds
//! what is nested inside, so the nesting falls out of the recursion rather than being worked out
//! afterwards by comparing block sets.
//!
//! Section 7.1's other decision is here by omission: two back edges to one header are two
//! latches of one loop and this does not try to guess whether one of them is really an inner
//! loop's. GCC guesses, from the profile if it has one and from induction variables if it does
//! not. rucc requires a single latch as a canonical form instead, and the canonicalizer in
//! document 26 creates one, which is an edit rather than a guess and is always right.

use rucc_ir::{Block, Def, Func, Value};

use crate::cfg::Cfg;
use crate::dom::Dominators;

/// One loop, by number.
///
/// A handle rather than a reference, because a loop's parent and children are loops and a tree
/// of references to each other is not a thing to hand a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LoopId(u32);

impl LoopId {
    /// Its number, for indexing something the caller keeps alongside.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// An edge that leaves a loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    /// The block inside the loop the edge leaves from.
    pub from: Block,
    /// The block outside the loop it arrives at.
    pub to: Block,
}

/// What is known about one loop.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LoopData {
    header: Block,
    /// Every block in the loop, including the blocks of the loops nested in it.
    blocks: Vec<Block>,
    /// The blocks with a back edge to the header. Canonical form wants exactly one.
    latches: Vec<Block>,
    /// Every edge out of the loop, cached because every loop pass asks and the alternative is
    /// a walk of the body each time.
    exits: Vec<Exit>,
    parent: Option<LoopId>,
    children: Vec<LoopId>,
    depth: u32,
}

/// The loops of a function, nested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loops {
    loops: Vec<LoopData>,
    /// The innermost loop holding each block, indexed by block number.
    innermost: Vec<Option<LoopId>>,
    /// The loops nothing encloses.
    roots: Vec<LoopId>,
    /// Blocks in a cycle that is not a natural loop, in block order.
    irreducible: Vec<Block>,
}

impl Loops {
    /// Finds the loops.
    ///
    /// Linear in the graph per level of nesting, so linear with a small constant on the code
    /// people write. Section 7.8 says this is not the part of loop analysis to worry about.
    #[must_use]
    pub fn new(cfg: &Cfg, doms: &Dominators) -> Self {
        let mut build = Build::new(cfg, doms);
        let region: Vec<Block> = cfg.postorder().to_vec();
        build.region(&region, None);
        build.finish()
    }

    /// How many loops there are, counting nested ones.
    #[must_use]
    pub fn count(&self) -> usize {
        self.loops.len()
    }

    /// Every loop, outer before inner.
    pub fn all(&self) -> impl Iterator<Item = LoopId> + use<> {
        (0..self.loops.len()).map(|index| LoopId(index as u32))
    }

    /// The loops nothing encloses.
    #[must_use]
    pub fn roots(&self) -> &[LoopId] {
        &self.roots
    }

    /// The one block every path into the loop arrives at.
    #[must_use]
    pub fn header(&self, id: LoopId) -> Block {
        self.loops[id.index()].header
    }

    /// Every block in the loop, including the blocks of the loops nested in it.
    #[must_use]
    pub fn blocks(&self, id: LoopId) -> &[Block] {
        &self.loops[id.index()].blocks
    }

    /// The blocks with an edge back to the header.
    ///
    /// Canonical form wants exactly one of these, and section 7.3 says why: it is what makes
    /// "the last thing that happens in an iteration" a place rather than a question.
    #[must_use]
    pub fn latches(&self, id: LoopId) -> &[Block] {
        &self.loops[id.index()].latches
    }

    /// Every edge that leaves the loop.
    #[must_use]
    pub fn exits(&self, id: LoopId) -> &[Exit] {
        &self.loops[id.index()].exits
    }

    /// The loop this one is nested in.
    #[must_use]
    pub fn parent(&self, id: LoopId) -> Option<LoopId> {
        self.loops[id.index()].parent
    }

    /// The loops nested directly in this one.
    #[must_use]
    pub fn children(&self, id: LoopId) -> &[LoopId] {
        &self.loops[id.index()].children
    }

    /// How many loops enclose this one, counting from zero for one nothing encloses.
    #[must_use]
    pub fn depth(&self, id: LoopId) -> u32 {
        self.loops[id.index()].depth
    }

    /// The innermost loop holding this block, if any holds it.
    #[must_use]
    pub fn innermost(&self, block: Block) -> Option<LoopId> {
        *self.innermost.get(block.index()).unwrap_or(&None)
    }

    /// Whether the block is in this loop or in a loop nested in it.
    #[must_use]
    pub fn contains(&self, id: LoopId, block: Block) -> bool {
        let mut walk = self.innermost(block);
        while let Some(inner) = walk {
            if inner == id {
                return true;
            }
            walk = self.parent(inner);
        }
        false
    }

    /// The block outside the loop that every path in comes through, when there is exactly one
    /// such block and its only successor is the header.
    ///
    /// This is the preheader of section 7.3, which is where loop invariant code motion puts
    /// what it hoists. `None` means the loop is not in canonical form yet, and the answer is to
    /// run the canonicalizer rather than to split an edge here.
    #[must_use]
    pub fn preheader(&self, cfg: &Cfg, id: LoopId) -> Option<Block> {
        let header = self.header(id);
        let mut outside = cfg.predecessors(header).iter().filter(|&&pred| !self.contains(id, pred));
        let candidate = *outside.next()?;
        if outside.next().is_some() || cfg.successors(candidate).len() != 1 {
            return None;
        }
        Some(candidate)
    }

    /// Blocks that are in a cycle and in no natural loop.
    ///
    /// These are the irreducible regions of section 7.1, which is what `goto` into a loop body
    /// and some state machines written as a `switch` inside a `for` produce. rucc does not turn
    /// them into reducible ones: node splitting can blow up code size exponentially and the
    /// payoff is a handful of loop optimizations applying to code that is rare and usually
    /// cold. GCC does not do it either. The analysis reports them, the loop passes decline
    /// them, and the value level passes are unaffected because they only need dominance.
    ///
    /// The search stops at an irreducible region rather than looking inside it, so a back edge
    /// buried in one does not become a loop even when its head dominates its tail. That loses a
    /// self loop inside a two entry region and nothing else anyone has produced, and it loses
    /// nothing in practice because a loop pass skips those blocks either way. What it buys is
    /// that "in a loop" and "irreducible" are decided by one walk, so they cannot disagree.
    #[must_use]
    pub fn irreducible(&self) -> &[Block] {
        &self.irreducible
    }

    /// Whether this block is in a cycle that is not a natural loop.
    #[must_use]
    pub fn is_irreducible(&self, block: Block) -> bool {
        self.irreducible.binary_search_by_key(&block.index(), |b| b.index()).is_ok()
    }

    /// Whether the value is the same on every iteration of this loop.
    ///
    /// The second of the four questions section 7.6 says this analysis answers. A value is
    /// invariant when what defines it is outside the loop, which covers the function's
    /// arguments and everything computed before the loop was entered. A value defined inside
    /// can still be invariant, when everything it reads is, and answering that is a walk this
    /// deliberately does not do: the caller that wants it is loop invariant code motion, which
    /// has to walk the body in order anyway and gets the transitive answer for free as it goes.
    #[must_use]
    pub fn is_invariant(&self, func: &Func, id: LoopId, value: Value) -> bool {
        let block = match func[value].def {
            Def::Result { inst, .. } => func.block_of(inst),
            Def::Param { block, .. } => Some(block),
        };
        // A value whose defining instruction is in no block was removed, and nothing should be
        // asking about it. Saying it is not invariant is the answer that stops a caller acting
        // on it.
        block.is_some_and(|block| !self.contains(id, block))
    }

    /// What is wrong with the forest, which on a forest this built is nothing.
    ///
    /// Section 7.2 asks for the equivalent of GCC's `verify_loop_structure`, and it is a
    /// separate thing from document 04.3's check that a pass did not lie about what it
    /// preserved. That one catches a pass claiming to have kept the forest when it changed the
    /// graph under it. This one catches a pass that rebuilt the forest into something malformed,
    /// which is a different mistake and is the one that follows from an edit near a header.
    ///
    /// This does not check canonical form. A preheader, a single latch, loop-closed SSA and a
    /// dedicated exit are what document 26's canonicalizer establishes before the loop pipeline
    /// runs, and a forest read off an arbitrary function has none of them.
    #[must_use]
    pub fn problems(&self, cfg: &Cfg, doms: &Dominators) -> Vec<String> {
        let mut found = Vec::new();
        for id in self.all() {
            let header = self.header(id);
            for &block in self.blocks(id) {
                if !doms.dominates(header, block) {
                    found.push(format!("loop {} holds a block its header does not dominate", id.0));
                    break;
                }
            }
            if self.latches(id).is_empty() {
                found.push(format!("loop {} has no way back to its header", id.0));
            }
            for &latch in self.latches(id) {
                if !cfg.successors(latch).contains(&header) {
                    found.push(format!("loop {} has a latch that does not reach its header", id.0));
                    break;
                }
            }
            for exit in self.exits(id) {
                if !self.contains(id, exit.from) || self.contains(id, exit.to) {
                    found.push(format!("loop {} has an exit that does not leave it", id.0));
                    break;
                }
            }
            if let Some(parent) = self.parent(id) {
                if !self.blocks(id).iter().all(|&block| self.contains(parent, block)) {
                    found.push(format!("loop {} is not inside the loop it says it is in", id.0));
                }
                if self.depth(id) != self.depth(parent) + 1 {
                    found.push(format!("loop {} is not one deeper than its parent", id.0));
                }
            } else if self.depth(id) != 0 {
                found.push(format!("loop {} has a depth and nothing to be deep inside", id.0));
            }
        }
        found
    }
}

/// The state of one construction, which recurses into what it finds.
struct Build<'a> {
    cfg: &'a Cfg,
    doms: &'a Dominators,
    loops: Vec<LoopData>,
    innermost: Vec<Option<LoopId>>,
    roots: Vec<LoopId>,
    irreducible: Vec<Block>,
    /// Whether each block is in the region being looked at, so an edge out of it is skipped in
    /// O(1) rather than by searching the region.
    inside: Vec<bool>,
    /// Tarjan's numbering, and the lowest one reachable from each block.
    index: Vec<u32>,
    low: Vec<u32>,
    stacked: Vec<bool>,
}

/// A block Tarjan's walk has not numbered yet.
const UNVISITED: u32 = u32::MAX;

impl<'a> Build<'a> {
    fn new(cfg: &'a Cfg, doms: &'a Dominators) -> Self {
        let blocks = cfg.capacity();
        Self {
            cfg,
            doms,
            loops: Vec::new(),
            innermost: vec![None; blocks],
            roots: Vec::new(),
            irreducible: Vec::new(),
            inside: vec![false; blocks],
            index: vec![UNVISITED; blocks],
            low: vec![0; blocks],
            stacked: vec![false; blocks],
        }
    }

    /// Finds the loops of one region and then of what is left of each of them.
    ///
    /// The region of the first call is every block control reaches. The region of a later one is
    /// a loop with its header taken out, which is the graph the loops nested in it live in.
    fn region(&mut self, region: &[Block], parent: Option<LoopId>) {
        for &block in region {
            self.inside[block.index()] = true;
            self.index[block.index()] = UNVISITED;
            self.stacked[block.index()] = false;
        }
        let components = self.components(region);
        for &block in region {
            self.inside[block.index()] = false;
        }

        for component in components {
            // The nearest block dominating all of it. If it is one of the component's own
            // blocks then every path in arrives there, because a block outside the component
            // that the header dominates is a block the header reaches and that reaches back
            // into the component, which would put it in the component. So a header inside means
            // one way in, which is what reducible means.
            let Some(header) = component
                .iter()
                .copied()
                .try_fold(component[0], |a, b| self.doms.nearest_common_dominator(a, b))
            else {
                continue;
            };
            if !component.contains(&header) {
                self.irreducible.extend_from_slice(&component);
                continue;
            }
            let id = self.record(header, component, parent);
            let inner: Vec<Block> =
                self.loops[id.index()].blocks.iter().copied().filter(|&b| b != header).collect();
            if !inner.is_empty() {
                self.region(&inner, Some(id));
            }
        }
    }

    /// Adds a loop, and says which blocks are in it.
    fn record(&mut self, header: Block, blocks: Vec<Block>, parent: Option<LoopId>) -> LoopId {
        let id = LoopId(self.loops.len() as u32);
        let mut latches = Vec::new();
        let mut exits = Vec::new();
        for &block in &blocks {
            // Every block of the loop belongs to it until an inner call says otherwise, and an
            // inner call runs after this, so the last writer is the innermost loop.
            self.innermost[block.index()] = Some(id);
            if self.cfg.successors(block).contains(&header) {
                latches.push(block);
            }
            for &next in self.cfg.successors(block) {
                if !blocks.contains(&next) {
                    exits.push(Exit { from: block, to: next });
                }
            }
        }
        let depth = parent.map_or(0, |parent| self.loops[parent.index()].depth + 1);
        self.loops.push(LoopData {
            header,
            blocks,
            latches,
            exits,
            parent,
            children: Vec::new(),
            depth,
        });
        match parent {
            Some(parent) => self.loops[parent.index()].children.push(id),
            None => self.roots.push(id),
        }
        id
    }

    /// The strongly connected components of the region that have a cycle in them.
    ///
    /// Tarjan's algorithm, with the recursion written out, because the depth of the walk is the
    /// length of the longest path in the function and a long C function has one.
    fn components(&mut self, region: &[Block]) -> Vec<Vec<Block>> {
        let mut found = Vec::new();
        let mut next = 0;
        let mut component: Vec<Block> = Vec::new();
        let mut walk: Vec<(Block, usize)> = Vec::new();
        for &start in region {
            if self.index[start.index()] != UNVISITED {
                continue;
            }
            self.enter(start, &mut next, &mut component);
            walk.push((start, 0));
            while let Some((block, step)) = walk.pop() {
                let successors = self.cfg.successors(block);
                if step < successors.len() {
                    let next_block = successors[step];
                    walk.push((block, step + 1));
                    if !self.inside[next_block.index()] {
                        continue;
                    }
                    if self.index[next_block.index()] == UNVISITED {
                        self.enter(next_block, &mut next, &mut component);
                        walk.push((next_block, 0));
                    } else if self.stacked[next_block.index()] {
                        let seen = self.index[next_block.index()];
                        self.low[block.index()] = self.low[block.index()].min(seen);
                    }
                    continue;
                }
                if self.low[block.index()] == self.index[block.index()] {
                    let start = component.iter().rposition(|&b| b == block).expect("on the stack");
                    let members: Vec<Block> = component.split_off(start);
                    for &member in &members {
                        self.stacked[member.index()] = false;
                    }
                    if members.len() > 1 || self.cfg.successors(block).contains(&block) {
                        found.push(members);
                    }
                }
                if let Some(&(above, _)) = walk.last() {
                    let reached = self.low[block.index()];
                    self.low[above.index()] = self.low[above.index()].min(reached);
                }
            }
        }
        found
    }

    /// Numbers a block and puts it on the component stack.
    fn enter(&mut self, block: Block, next: &mut u32, component: &mut Vec<Block>) {
        self.index[block.index()] = *next;
        self.low[block.index()] = *next;
        *next += 1;
        component.push(block);
        self.stacked[block.index()] = true;
    }

    fn finish(mut self) -> Loops {
        self.irreducible.sort_unstable_by_key(|block| block.index());
        self.irreducible.dedup();
        Loops {
            loops: self.loops,
            innermost: self.innermost,
            roots: self.roots,
            irreducible: self.irreducible,
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_ir::Block;

    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::loops::Loops;
    use crate::testing::graph;

    /// Block number `n`, spelled the way the tests read.
    fn b(n: usize) -> Block {
        Block::from_usize(n)
    }

    /// The forest of a graph, along with the graph and the tree it was read from.
    fn forest(edges: &[&[usize]]) -> (Cfg, Loops) {
        let func = graph(edges);
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        assert_eq!(loops.problems(&cfg, &doms), Vec::<String>::new());
        (cfg, loops)
    }

    /// The blocks of a loop, as sorted block numbers.
    fn blocks(loops: &Loops, id: crate::loops::LoopId) -> Vec<usize> {
        let mut list: Vec<usize> = loops.blocks(id).iter().map(|b| b.index()).collect();
        list.sort_unstable();
        list
    }

    #[test]
    fn a_function_with_no_cycle_has_no_loops() {
        let (_, loops) = forest(&[&[1, 2], &[3], &[3], &[]]);
        assert_eq!(loops.count(), 0);
        assert!(loops.innermost(b(1)).is_none());
        assert!(loops.irreducible().is_empty());
    }

    #[test]
    fn a_block_that_branches_to_itself_is_a_loop() {
        let (_, loops) = forest(&[&[1], &[1, 2], &[]]);
        assert_eq!(loops.count(), 1);
        let id = loops.roots()[0];
        assert_eq!(loops.header(id), b(1));
        assert_eq!(blocks(&loops, id), [1]);
        assert_eq!(loops.latches(id), [b(1)]);
        assert_eq!(loops.exits(id), [crate::loops::Exit { from: b(1), to: b(2) }]);
    }

    #[test]
    fn a_loop_holds_its_body_and_names_its_latch() {
        // 0 -> 1, 1 branches to the body 2 and the exit 3, 2 goes back to 1.
        let (cfg, loops) = forest(&[&[1], &[2, 3], &[1], &[]]);
        let id = loops.roots()[0];
        assert_eq!(loops.header(id), b(1));
        assert_eq!(blocks(&loops, id), [1, 2]);
        assert_eq!(loops.latches(id), [b(2)]);
        assert_eq!(loops.depth(id), 0);
        assert_eq!(loops.preheader(&cfg, id), Some(b(0)));
    }

    #[test]
    fn a_loop_inside_a_loop_is_a_child_of_it() {
        // 0 -> 1; the outer header 1 goes to the inner header 2 and to the exit 4; 2 goes to 3
        // and back to 2; 3 goes back to 1.
        let (_, loops) = forest(&[&[1], &[2, 4], &[2, 3], &[1], &[]]);
        assert_eq!(loops.count(), 2);
        let outer = loops.roots()[0];
        assert_eq!(loops.header(outer), b(1));
        assert_eq!(blocks(&loops, outer), [1, 2, 3]);
        assert_eq!(loops.children(outer).len(), 1);
        let inner = loops.children(outer)[0];
        assert_eq!(loops.header(inner), b(2));
        assert_eq!(blocks(&loops, inner), [2]);
        assert_eq!(loops.depth(inner), 1);
        assert_eq!(loops.parent(inner), Some(outer));
        // The innermost loop of a block is the one it is deepest inside, and the block set of
        // the outer loop still holds it.
        assert_eq!(loops.innermost(b(2)), Some(inner));
        assert!(loops.contains(outer, b(2)));
        assert!(!loops.contains(inner, b(3)));
    }

    #[test]
    fn two_back_edges_to_one_header_are_two_latches_of_one_loop() {
        // 1 is the header, 2 and 3 both go back to it. GCC would try to work out whether one of
        // them is really an inner loop's latch. This does not guess: they are two latches, the
        // canonical form wants one, and the canonicalizer is what makes one.
        let (_, loops) = forest(&[&[1], &[2, 3], &[1], &[1, 4], &[]]);
        assert_eq!(loops.count(), 1);
        let id = loops.roots()[0];
        let mut latches: Vec<usize> = loops.latches(id).iter().map(|b| b.index()).collect();
        latches.sort_unstable();
        assert_eq!(latches, [2, 3]);
    }

    #[test]
    fn a_two_entry_loop_is_irreducible_and_is_not_a_loop() {
        // The classic one. Block 0 branches into the cycle at 1 or at 2, so neither of them
        // dominates the other and neither is a header.
        let (_, loops) = forest(&[&[1, 2], &[2], &[1, 3], &[]]);
        assert_eq!(loops.count(), 0);
        assert_eq!(loops.irreducible(), [b(1), b(2)]);
        assert!(loops.is_irreducible(b(1)));
        assert!(!loops.is_irreducible(b(0)));
    }

    #[test]
    fn an_irreducible_region_inside_a_loop_is_found_and_the_loop_is_still_a_loop() {
        // The outer loop 1 -> {2, 3} -> {2, 3} -> 4 -> 1 is a natural loop, and the cycle
        // between 2 and 3 has two ways in. Taking the header out and looking again is what
        // finds the second one, which is the reason the search recurses rather than reporting
        // whatever is left over at the end.
        let (_, loops) = forest(&[&[1], &[2, 3], &[3, 4], &[2, 4], &[1, 5], &[]]);
        assert_eq!(loops.count(), 1);
        let id = loops.roots()[0];
        assert_eq!(loops.header(id), b(1));
        assert_eq!(blocks(&loops, id), [1, 2, 3, 4]);
        assert_eq!(loops.irreducible(), [b(2), b(3)]);
    }

    #[test]
    fn a_self_loop_inside_an_irreducible_region_is_declined_along_with_it() {
        // 0 branches to 1 and 2, 1 goes back to 0 and on to 2, and 2 branches to 1 and to
        // itself. The whole thing is one natural loop headed at 0. Inside it, the cycle between
        // 1 and 2 has two ways in and is irreducible, and block 2's branch to itself is a back
        // edge sitting in the middle of it.
        //
        // The textbook construction would report that back edge as a second loop. This does not,
        // because it stops at the irreducible region rather than looking inside, and the answer
        // it gives instead is that block 2 is irreducible. A loop pass reads that and leaves the
        // block alone, which is what it would have done with a one block loop it was told not to
        // touch. The property test in `tests/loops.rs` is where the difference is pinned down.
        let (_, loops) = forest(&[&[1, 2], &[0, 2], &[1, 2]]);
        assert_eq!(loops.count(), 1);
        let id = loops.roots()[0];
        assert_eq!(loops.header(id), b(0));
        assert_eq!(blocks(&loops, id), [0, 1, 2]);
        assert_eq!(loops.irreducible(), [b(1), b(2)]);
    }

    #[test]
    fn a_loop_with_more_than_one_way_in_has_no_preheader() {
        // Both 0 and 1 branch to the header, so there is no single block to hoist into.
        let (cfg, loops) = forest(&[&[1, 2], &[2], &[2, 3], &[]]);
        let id = loops.roots()[0];
        assert_eq!(loops.header(id), b(2));
        assert!(loops.preheader(&cfg, id).is_none());
    }

    #[test]
    fn a_predecessor_that_goes_two_ways_is_not_a_preheader() {
        // Block 0 branches to the header and to somewhere else, so hoisting into it would run
        // the hoisted code on a path that never enters the loop.
        let (cfg, loops) = forest(&[&[1, 3], &[1, 2], &[], &[]]);
        let id = loops.roots()[0];
        assert_eq!(loops.header(id), b(1));
        assert!(loops.preheader(&cfg, id).is_none());
    }

    #[test]
    fn an_unreachable_cycle_is_not_a_loop() {
        // Blocks 2 and 3 are a cycle nothing reaches. Section 6.5 says an unreachable block is
        // invisible to every analysis, and a loop nothing can enter is not a loop to unroll.
        let (_, loops) = forest(&[&[1], &[], &[3], &[2]]);
        assert_eq!(loops.count(), 0);
        assert!(loops.irreducible().is_empty());
    }

    #[test]
    fn a_value_defined_outside_the_loop_is_invariant_in_it() {
        use rucc_base::Interner;
        use rucc_ir::{Builder, Func, Signature, Type};

        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let header = func.create_block();
        let after = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let outside = build.iconst(Type::int(32), 7);
        build.jump(header, &[]);
        let mut build = Builder::new(&mut func, header);
        let inside = build.iconst(Type::int(32), 9);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, header, &[], after, &[]);
        let mut build = Builder::new(&mut func, after);
        build.ret(&[]);

        let cfg = Cfg::new(&func);
        let loops = Loops::new(&cfg, &Dominators::new(&cfg));
        let id = loops.roots()[0];
        assert!(loops.is_invariant(&func, id, outside));
        assert!(!loops.is_invariant(&func, id, inside));
    }
}
