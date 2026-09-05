//! Dominance, which is the question of what every path has to go through.
//!
//! Design: `spec/optimizer/06-cfg-and-dominators.md`.
//!
//! Block `a` dominates block `b` when every path from the entry to `b` goes through `a`. That
//! is what makes it safe to say a value defined in `a` is available in `b`, so almost every
//! transformation that moves code asks this and the ones that do not are asking the reverse
//! question, which is post-dominance: whether every path from `b` to the exit goes through `a`.
//!
//! One algorithm answers both, the iterative one of Cooper, Harvey and Kennedy, "A Simple, Fast
//! Dominance Algorithm" (2001). GCC uses Lengauer and Tarjan, which is asymptotically better
//! and, on graphs the size of a function, slower. That is the argument of the paper and it is
//! why the verifier already uses this algorithm. There is no ET-forest here and no incremental
//! update: a change to the shape of a function throws the tree away and the next request builds
//! a new one. Section 6.3 of the design says why, and section 6.7 says what measurement would
//! change it, which is the analyses taking more than three percent of `-O2` time.
//!
//! The one thing to be careful about is that this is the analysis most likely to be quietly
//! wrong, because a tree that is stale rather than absent produces a miscompilation rather than
//! a crash.

use rucc_ir::Block;

use crate::cfg::Cfg;

/// A node of whichever graph is being walked, which is a block number for the function's own
/// graph and a block number or the added exit for the reverse of it.
type Node = u32;

/// No such node, for an immediate dominator that has not been worked out and for a node the
/// root does not reach.
const NONE: Node = Node::MAX;

/// A graph the dominator computation can walk.
///
/// Two implement it, the function's graph and the reverse of the function's graph, and having
/// them behind one trait is what stops post-dominance from being a second copy of the algorithm
/// with the arrows turned around by hand. Static dispatch throughout, so the reverse case pays
/// nothing for the abstraction and the forward case does not copy the adjacency lists to
/// renumber them.
trait Graph {
    /// How many nodes there are, which is the length of every array indexed by node.
    fn nodes(&self) -> usize;

    /// Where the walk starts, which is the entry going forwards and the exit going backwards.
    fn root(&self) -> Node;

    /// Where control arrives at this node from.
    fn preds(&self, node: Node) -> impl Iterator<Item = Node>;

    /// Where control goes from this node.
    fn succs(&self, node: Node) -> impl Iterator<Item = Node>;
}

/// A dominator tree over a graph whose nodes are numbered from zero.
#[derive(Clone, Debug)]
struct Tree {
    /// The immediate dominator of each node, the root's own number for the root, and [`NONE`]
    /// for a node the root does not reach.
    idom: Vec<Node>,
    /// The nodes each node immediately dominates.
    children: Vec<Vec<Node>>,
    /// When a depth first walk of the tree entered each node.
    enter: Vec<u32>,
    /// One past the largest number handed out anywhere in a node's subtree.
    leave: Vec<u32>,
    /// Where the walk started.
    root: Node,
}

impl Tree {
    /// Builds the tree, which is a postorder walk, a fixed point over it, and a second walk to
    /// number the result.
    fn new(graph: &impl Graph) -> Self {
        let nodes = graph.nodes();
        let root = graph.root();

        // Postorder by an explicit stack. The recursive form runs out of stack on a function
        // that is one long chain of blocks, and a ten thousand statement function is a real
        // thing that people write and that generated code writes more of.
        let mut order: Vec<Node> = Vec::new();
        let mut seen = vec![false; nodes];
        seen[root as usize] = true;
        let mut stack = vec![(root, graph.succs(root))];
        while let Some((node, mut walk)) = stack.pop() {
            // Out of the `match` scrutinee, because the closure borrows the seen set and the
            // arm below writes to it.
            let step = walk.find(|&next| !seen[next as usize]);
            match step {
                Some(next) => {
                    stack.push((node, walk));
                    seen[next as usize] = true;
                    stack.push((next, graph.succs(next)));
                }
                None => order.push(node),
            }
        }

        // Reverse postorder is the order this fixed point settles fastest in, because every
        // node other than a loop header is reached after a predecessor that already has an
        // answer. It converges in one sweep over a reducible graph and in two over the ones a
        // `goto` into a loop body produces, which is the entire special case irreducibility
        // needs here.
        let rpo: Vec<Node> = order.iter().rev().copied().collect();
        let mut rank = vec![NONE; nodes];
        for (index, &node) in rpo.iter().enumerate() {
            rank[node as usize] = index as u32;
        }
        let mut preds: Vec<Vec<u32>> = vec![Vec::new(); rpo.len()];
        for (index, &node) in rpo.iter().enumerate() {
            for pred in graph.preds(node) {
                if rank[pred as usize] != NONE {
                    preds[index].push(rank[pred as usize]);
                }
            }
        }

        // The root dominates itself and everything else starts with no answer, which is what
        // the sentinel is. A predecessor with no answer yet is skipped rather than met, because
        // meeting with nothing is not the same as meeting with the root.
        let mut idom_by_rank = vec![NONE; rpo.len()];
        if !rpo.is_empty() {
            idom_by_rank[0] = 0;
        }
        let mut changed = true;
        while changed {
            changed = false;
            for index in 1..rpo.len() {
                let mut new = NONE;
                for &pred in &preds[index] {
                    if idom_by_rank[pred as usize] == NONE {
                        continue;
                    }
                    new = if new == NONE { pred } else { meet(&idom_by_rank, new, pred) };
                }
                if new != NONE && idom_by_rank[index] != new {
                    idom_by_rank[index] = new;
                    changed = true;
                }
            }
        }

        let mut idom = vec![NONE; nodes];
        let mut children: Vec<Vec<Node>> = vec![Vec::new(); nodes];
        for (index, &node) in rpo.iter().enumerate() {
            let parent = rpo[idom_by_rank[index] as usize];
            idom[node as usize] = parent;
            if node != root {
                children[parent as usize].push(node);
            }
        }

        // The numbering that makes a query two comparisons instead of a walk up the tree. The
        // walk is fine when the caller asks once per value definition and it is not fine for
        // GVN or code motion, which ask per pair inside a loop. This is GCC's answer, from
        // `compute_dom_fast_query`, and it costs one linear pass over a tree that is already
        // built.
        let mut enter = vec![0; nodes];
        let mut leave = vec![0; nodes];
        let mut time = 0;
        let mut stack = vec![(root, 0usize)];
        enter[root as usize] = time;
        time += 1;
        while let Some((node, next)) = stack.pop() {
            match children[node as usize].get(next) {
                Some(&child) => {
                    stack.push((node, next + 1));
                    enter[child as usize] = time;
                    time += 1;
                    stack.push((child, 0));
                }
                None => leave[node as usize] = time,
            }
        }

        Self { idom, children, enter, leave, root }
    }

    /// Whether the root reaches this node at all.
    ///
    /// A node number the tree has no room for is one of a function with no blocks, and the
    /// answer is the same: nothing reaches it.
    fn reached(&self, node: Node) -> bool {
        self.idom.get(node as usize).is_some_and(|&parent| parent != NONE)
    }

    /// Whether every path from the root to `node` goes through `of`.
    ///
    /// False when either end is unreachable, which is the one place this differs from the
    /// verifier's copy of the same algorithm. The verifier calls an unreachable block
    /// vacuously dominated so that it is reported once, for being unreachable, rather than
    /// again for every value it uses. An optimizer must not: "defined in a block that dominates
    /// this one" and "defined in a block control reaches" are different questions, and a pass
    /// that answers the first when it meant the second will move a use of a value into a place
    /// the value does not exist.
    fn dominates(&self, of: Node, node: Node) -> bool {
        if !self.reached(of) || !self.reached(node) {
            return false;
        }
        let (enter, leave) = (self.enter[of as usize], self.leave[of as usize]);
        enter <= self.enter[node as usize] && self.enter[node as usize] < leave
    }

    /// The nearest node that dominates both, which is the root at worst.
    fn common(&self, a: Node, b: Node) -> Option<Node> {
        if !self.reached(a) || !self.reached(b) {
            return None;
        }
        let mut walk = a;
        while !self.dominates(walk, b) {
            walk = self.idom[walk as usize];
        }
        Some(walk)
    }
}

/// The nearest node dominating both, walking the two chains towards the root by rank.
///
/// Ranks are reverse postorder positions, so the larger number is the deeper node and stepping
/// the deeper one towards the root is what makes the two meet.
fn meet(idom: &[u32], mut a: u32, mut b: u32) -> u32 {
    while a != b {
        while a > b {
            a = idom[a as usize];
        }
        while b > a {
            b = idom[b as usize];
        }
    }
    a
}

/// The function's own graph, walked forwards.
struct Forward<'a>(&'a Cfg);

impl Graph for Forward<'_> {
    fn nodes(&self) -> usize {
        self.0.capacity()
    }

    fn root(&self) -> Node {
        self.0.entry().map_or(0, |block| block.index() as Node)
    }

    fn preds(&self, node: Node) -> impl Iterator<Item = Node> {
        self.0.predecessors(Block::from_usize(node as usize)).iter().map(|b| b.index() as Node)
    }

    fn succs(&self, node: Node) -> impl Iterator<Item = Node> {
        self.0.successors(Block::from_usize(node as usize)).iter().map(|b| b.index() as Node)
    }
}

/// Which block every path from the entry has to pass through to reach another.
#[derive(Clone, Debug)]
pub struct Dominators {
    tree: Tree,
}

impl Dominators {
    /// Builds the tree from the graph.
    ///
    /// A function with no blocks gives a tree that reaches nothing, and every query against it
    /// answers no, which is what a caller handed a declaration should see.
    #[must_use]
    pub fn new(cfg: &Cfg) -> Self {
        if cfg.entry().is_none() {
            return Self { tree: Tree::empty() };
        }
        Self { tree: Tree::new(&Forward(cfg)) }
    }

    /// Whether every path from the entry to `block` goes through `of`.
    ///
    /// A block dominates itself. Both ends have to be blocks control reaches, and an
    /// unreachable one dominates nothing and is dominated by nothing.
    #[must_use]
    pub fn dominates(&self, of: Block, block: Block) -> bool {
        self.tree.dominates(of.index() as Node, block.index() as Node)
    }

    /// The same, without a block dominating itself.
    #[must_use]
    pub fn strictly_dominates(&self, of: Block, block: Block) -> bool {
        of != block && self.dominates(of, block)
    }

    /// The nearest block that dominates this one and is not it.
    ///
    /// `None` for the entry, which has no dominator above it, and for a block control does not
    /// reach.
    #[must_use]
    pub fn immediate_dominator(&self, block: Block) -> Option<Block> {
        let node = block.index() as Node;
        if !self.tree.reached(node) || node == self.tree.root {
            return None;
        }
        Some(Block::from_usize(self.tree.idom[node as usize] as usize))
    }

    /// The blocks whose immediate dominator is this one.
    ///
    /// Walking these from the entry is how a pass visits a function in dominator tree order,
    /// which is the order that has every definition in hand before any use of it.
    pub fn children(&self, block: Block) -> impl Iterator<Item = Block> + use<'_> {
        self.tree
            .children
            .get(block.index())
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|&node| Block::from_usize(node as usize))
    }

    /// The nearest block that dominates both, which is the entry at worst.
    ///
    /// `None` when either block is one control does not reach.
    #[must_use]
    pub fn nearest_common_dominator(&self, a: Block, b: Block) -> Option<Block> {
        self.tree
            .common(a.index() as Node, b.index() as Node)
            .map(|node| Block::from_usize(node as usize))
    }
}

/// The function's graph with every arrow turned around, and an exit for them all to end at.
///
/// The exit is a node this adds, numbered one past the last block, because the real graph has
/// as many blocks with no successors as it has `return` statements and a dominator computation
/// wants one root. Every block with no successors gets an edge to it, and so does every
/// infinite loop, through [`Reverse::connect`].
struct Reverse {
    /// Where control comes from, by node, which is where it goes in the real graph.
    succs: Vec<Vec<Node>>,
    /// Where control goes, by node, which is where it comes from in the real graph.
    preds: Vec<Vec<Node>>,
    /// The added node, numbered one past the last block.
    exit: Node,
}

impl Graph for Reverse {
    fn nodes(&self) -> usize {
        self.succs.len()
    }

    fn root(&self) -> Node {
        self.exit
    }

    fn preds(&self, node: Node) -> impl Iterator<Item = Node> {
        self.preds[node as usize].iter().copied()
    }

    fn succs(&self, node: Node) -> impl Iterator<Item = Node> {
        self.succs[node as usize].iter().copied()
    }
}

impl Reverse {
    /// Turns the graph around, keeping only the blocks control reaches.
    fn new(cfg: &Cfg) -> Self {
        let exit = cfg.capacity() as Node;
        let mut succs: Vec<Vec<Node>> = vec![Vec::new(); cfg.capacity() + 1];
        let mut preds: Vec<Vec<Node>> = vec![Vec::new(); cfg.capacity() + 1];
        for &block in cfg.postorder() {
            let from = block.index() as Node;
            for &next in cfg.successors(block) {
                succs[next.index()].push(from);
                preds[from as usize].push(next.index() as Node);
            }
            if cfg.successors(block).is_empty() {
                succs[exit as usize].push(from);
                preds[from as usize].push(exit);
            }
        }
        Self { succs, preds, exit }
    }

    /// Adds an edge to the exit from every region that has no path to one.
    ///
    /// `while (1) { }` has no path to the exit, so the reverse graph is disconnected and
    /// post-dominance is undefined for everything in the loop. GCC's answer is
    /// `connect_infinite_loops_to_exit`, which is to add a fake edge from the far end of each
    /// such region, and this is the same thing. The edges are recorded rather than hidden, so a
    /// pass that would treat one as a real path can ask.
    ///
    /// Anything reachable from a block with no path to the exit also has no path to the exit,
    /// which is why walking forwards from one stays inside the region, and why the block this
    /// stops at is always a sensible place to attach.
    fn connect(&mut self, cfg: &Cfg) -> Vec<Block> {
        let mut arrives = vec![false; self.nodes()];
        let mut stack = vec![self.exit];
        arrives[self.exit as usize] = true;
        let mut fake = Vec::new();
        let mut stamp = vec![u32::MAX; self.nodes()];
        let mut round = 0;
        loop {
            while let Some(node) = stack.pop() {
                for &next in &self.succs[node as usize] {
                    if !arrives[next as usize] {
                        arrives[next as usize] = true;
                        stack.push(next);
                    }
                }
            }
            // Outermost first, so the walk to the far end starts above the loop rather than
            // inside it, which is what makes the attachment point the same one a person would
            // pick by hand.
            let Some(stranded) = cfg.reverse_postorder().find(|block| !arrives[block.index()])
            else {
                break;
            };
            let end = far_end(cfg, stranded, &mut stamp, round).index() as Node;
            round += 1;
            self.succs[self.exit as usize].push(end);
            self.preds[end as usize].push(self.exit);
            fake.push(Block::from_usize(end as usize));
            arrives[end as usize] = true;
            stack.push(end);
        }
        fake
    }
}

/// The block a forward walk from here stops at, which is the far end of the region.
///
/// It stops when every successor has already been stepped through, so on a loop it is the
/// deepest block of the loop rather than the header, and on a chain it is the last block.
fn far_end(cfg: &Cfg, from: Block, stamp: &mut [u32], round: u32) -> Block {
    let mut block = from;
    loop {
        stamp[block.index()] = round;
        let next = cfg.successors(block).iter().copied().find(|b| stamp[b.index()] != round);
        match next {
            Some(next) => block = next,
            None => return block,
        }
    }
}

/// Which block every path to the exit has to pass through after leaving another.
#[derive(Clone, Debug)]
pub struct PostDominators {
    tree: Tree,
    exit: Node,
    fake: Vec<Block>,
}

impl PostDominators {
    /// Builds the tree over the reversed graph.
    ///
    /// # Panics
    ///
    /// Panics if a block control reaches still has no path to the exit after the fake edges
    /// have been added, which would mean the answers below were arbitrary. Section 6.8 of the
    /// design asks for exactly this, on the grounds that a wrong answer here is worth turning
    /// into a crash.
    #[must_use]
    pub fn new(cfg: &Cfg) -> Self {
        if cfg.entry().is_none() {
            return Self { tree: Tree::empty(), exit: 0, fake: Vec::new() };
        }
        let mut reverse = Reverse::new(cfg);
        let fake = reverse.connect(cfg);
        let exit = reverse.exit;
        let tree = Tree::new(&reverse);
        for &block in cfg.postorder() {
            assert!(
                tree.reached(block.index() as Node),
                "a block control reaches has no path to the exit, so post-dominance is undefined"
            );
        }
        Self { tree, exit, fake }
    }

    /// Whether every path from `block` to the exit goes through `of`.
    ///
    /// A block post-dominates itself. Both ends have to be blocks control reaches.
    #[must_use]
    pub fn post_dominates(&self, of: Block, block: Block) -> bool {
        self.tree.dominates(of.index() as Node, block.index() as Node)
    }

    /// The same, without a block post-dominating itself.
    #[must_use]
    pub fn strictly_post_dominates(&self, of: Block, block: Block) -> bool {
        of != block && self.post_dominates(of, block)
    }

    /// The nearest block that post-dominates this one and is not it.
    ///
    /// `None` when the next thing on every path out is the end of the function, and for a block
    /// control does not reach.
    #[must_use]
    pub fn immediate_post_dominator(&self, block: Block) -> Option<Block> {
        let node = block.index() as Node;
        if !self.tree.reached(node) {
            return None;
        }
        let parent = self.tree.idom[node as usize];
        if parent == self.exit || parent == node {
            return None;
        }
        Some(Block::from_usize(parent as usize))
    }

    /// The blocks an edge to the exit was invented for, because nothing led there from them.
    ///
    /// A block is in here when it is the far end of an infinite loop. The list is public
    /// because a pass that reasons about paths should be able to tell that one of them was
    /// added by this analysis and is not a path the program can take.
    #[must_use]
    pub fn fake_exits(&self) -> &[Block] {
        &self.fake
    }
}

impl Tree {
    /// The tree of a function that has no blocks, which reaches nothing.
    fn empty() -> Self {
        Self {
            idom: Vec::new(),
            children: Vec::new(),
            enter: Vec::new(),
            leave: Vec::new(),
            root: NONE,
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_ir::Block;

    use crate::cfg::Cfg;
    use crate::dom::{Dominators, PostDominators};
    use crate::testing::{computed_goto, graph};

    /// Block number `n`, spelled the way the tests read.
    fn b(n: usize) -> Block {
        Block::from_usize(n)
    }

    /// The immediate dominator of every block, as block numbers, `None` where there is none.
    fn idoms(doms: &Dominators, blocks: usize) -> Vec<Option<usize>> {
        (0..blocks).map(|n| doms.immediate_dominator(b(n)).map(|d| d.index())).collect()
    }

    #[test]
    fn a_straight_line_is_a_chain() {
        let func = graph(&[&[1], &[2], &[]]);
        let doms = Dominators::new(&Cfg::new(&func));
        assert_eq!(idoms(&doms, 3), [None, Some(0), Some(1)]);
        assert!(doms.dominates(b(0), b(2)));
        assert!(!doms.dominates(b(2), b(0)));
        assert!(doms.dominates(b(1), b(1)));
        assert!(!doms.strictly_dominates(b(1), b(1)));
    }

    #[test]
    fn a_diamond_is_dominated_by_the_block_it_came_from() {
        let func = graph(&[&[1, 2], &[3], &[3], &[]]);
        let doms = Dominators::new(&Cfg::new(&func));
        // The join is dominated by the branch and not by either arm, which is the whole point.
        assert_eq!(idoms(&doms, 4), [None, Some(0), Some(0), Some(0)]);
        assert!(!doms.dominates(b(1), b(3)));
        assert_eq!(doms.nearest_common_dominator(b(1), b(2)), Some(b(0)));
    }

    #[test]
    fn a_loop_header_dominates_its_body_and_its_latch() {
        let func = graph(&[&[1], &[2, 3], &[1], &[]]);
        let doms = Dominators::new(&Cfg::new(&func));
        assert_eq!(idoms(&doms, 4), [None, Some(0), Some(1), Some(1)]);
        assert!(doms.dominates(b(1), b(2)));
        // The header has two predecessors, one of which it dominates. Meeting the answer from
        // the latch with the answer from outside is what a back edge asks of the fixed point.
        assert!(!doms.dominates(b(2), b(1)));
    }

    #[test]
    fn neither_entry_of_an_irreducible_loop_dominates_the_other() {
        // The classic two-entry loop. Block 0 branches into the middle of the cycle either way,
        // so blocks 1 and 2 are each reachable without the other.
        let func = graph(&[&[1, 2], &[2], &[1, 3], &[]]);
        let doms = Dominators::new(&Cfg::new(&func));
        assert_eq!(idoms(&doms, 4), [None, Some(0), Some(0), Some(2)]);
        assert!(!doms.dominates(b(1), b(2)));
        assert!(!doms.dominates(b(2), b(1)));
    }

    #[test]
    fn an_unreachable_block_dominates_nothing_and_nothing_dominates_it() {
        let func = graph(&[&[1], &[], &[2]]);
        let doms = Dominators::new(&Cfg::new(&func));
        assert!(!doms.dominates(b(0), b(2)));
        assert!(!doms.dominates(b(2), b(2)));
        assert!(doms.immediate_dominator(b(2)).is_none());
        assert!(doms.nearest_common_dominator(b(0), b(2)).is_none());
    }

    #[test]
    fn a_block_only_a_computed_goto_reaches_is_dominated_by_the_branch() {
        let doms = Dominators::new(&Cfg::new(&computed_goto()));
        assert_eq!(doms.immediate_dominator(b(2)), Some(b(1)));
    }

    #[test]
    fn a_declaration_answers_no_to_everything() {
        let func =
            rucc_ir::Func::new(rucc_base::Interner::new().intern("f"), rucc_ir::Signature::new());
        let cfg = Cfg::new(&func);
        let doms = Dominators::new(&cfg);
        // A declaration is an ordinary thing for a pipeline to be handed, so asking about a
        // block it does not have answers rather than panicking.
        assert!(!doms.dominates(b(0), b(0)));
        assert!(doms.immediate_dominator(b(0)).is_none());
        assert_eq!(doms.children(b(0)).count(), 0);
        let posts = PostDominators::new(&cfg);
        assert!(posts.fake_exits().is_empty());
        assert!(!posts.post_dominates(b(0), b(0)));
    }

    #[test]
    fn the_children_of_a_block_are_what_it_immediately_dominates() {
        let func = graph(&[&[1, 2], &[3], &[3], &[]]);
        let doms = Dominators::new(&Cfg::new(&func));
        let mut children: Vec<usize> = doms.children(b(0)).map(|c| c.index()).collect();
        children.sort_unstable();
        assert_eq!(children, [1, 2, 3]);
        assert_eq!(doms.children(b(1)).count(), 0);
    }

    #[test]
    fn a_join_is_post_dominated_by_what_comes_after_it() {
        let func = graph(&[&[1, 2], &[3], &[3], &[]]);
        let posts = PostDominators::new(&Cfg::new(&func));
        assert!(posts.post_dominates(b(3), b(0)));
        assert!(!posts.post_dominates(b(1), b(0)));
        assert_eq!(posts.immediate_post_dominator(b(1)), Some(b(3)));
        // Nothing comes after the return, so the next thing on the way out is the end of the
        // function, which is not a block.
        assert!(posts.immediate_post_dominator(b(3)).is_none());
        assert!(posts.fake_exits().is_empty());
    }

    #[test]
    fn an_infinite_loop_gets_an_edge_to_the_exit_and_says_which() {
        // Block 1 is `while (1) { }` and block 2 is the only way out of the function.
        let func = graph(&[&[1, 2], &[1], &[]]);
        let posts = PostDominators::new(&Cfg::new(&func));
        let fake: Vec<usize> = posts.fake_exits().iter().map(|block| block.index()).collect();
        assert_eq!(fake, [1]);
        // Without the invented edge this is undefined rather than false, which is the failure
        // the design asks to be turned into a crash.
        assert!(!posts.post_dominates(b(2), b(0)));
        assert!(posts.post_dominates(b(1), b(1)));
    }

    #[test]
    fn two_infinite_loops_get_an_edge_each() {
        let func = graph(&[&[1, 2, 3], &[1], &[2], &[]]);
        let posts = PostDominators::new(&Cfg::new(&func));
        let mut fake: Vec<usize> = posts.fake_exits().iter().map(|block| block.index()).collect();
        fake.sort_unstable();
        assert_eq!(fake, [1, 2]);
        // Three ways out of block 0 and no block on all three, so the next thing every path
        // shares is the end of the function.
        assert!(posts.immediate_post_dominator(b(0)).is_none());
    }
}
