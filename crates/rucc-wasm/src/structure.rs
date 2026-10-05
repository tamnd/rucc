//! What the structured control flow of a function is built from.
//!
//! WebAssembly has no jump. It has `block`, `loop` and `if`, and a branch leaves one of them by
//! its depth. The translation from a graph of blocks is the one of Ramsey, "Beyond Relooper"
//! (ICFP 2022), and it needs three facts about the graph, which this module computes: the
//! reverse postorder, the dominator tree, and which blocks are loop headers and which are merge
//! nodes. A loop header is the target of a back edge. A merge node is a block that two or more
//! forward edges reach, counted as edges and not as predecessors, so that a block that both arms
//! of one `br_if` reach is a merge node and its code is written once.
//!
//! The translation is correct for a reducible graph only, which is one where every back edge goes
//! to a block that dominates its source. C with no `goto` into a loop gives such a graph. A graph
//! that is not reducible is refused here with the name of the block, and a later step of #2864
//! splits nodes to make one reducible.

use rucc_base::hash::Map;
use rucc_ir::{Block, Func};

/// The facts about the reachable blocks of one function, by position in reverse postorder.
pub(crate) struct Shape {
    /// The blocks in reverse postorder. The entry is first.
    pub(crate) order: Vec<Block>,
    /// The position of each reachable block in [`Shape::order`].
    pub(crate) position: Map<Block, usize>,
    /// The blocks that each block immediately dominates, in reverse postorder.
    pub(crate) children: Vec<Vec<usize>>,
    pub(crate) loop_header: Vec<bool>,
    pub(crate) merge: Vec<bool>,
}

impl Shape {
    /// The facts about `func`, or the block where the graph stops being reducible.
    pub(crate) fn of(func: &Func) -> Result<Shape, Block> {
        let Some(entry) = func.entry() else {
            return Ok(Shape {
                order: Vec::new(),
                position: Map::default(),
                children: Vec::new(),
                loop_header: Vec::new(),
                merge: Vec::new(),
            });
        };
        let successors = |block: Block| -> Vec<Block> {
            func.terminator(block)
                .map(|term| func.successors(term).map(|call| call.block).collect())
                .unwrap_or_default()
        };

        // A depth first walk with an explicit stack, so that a function with a long chain of
        // blocks does not use up the stack of the compiler.
        let mut post = Vec::new();
        let mut seen: Map<Block, ()> = Map::default();
        let mut stack = vec![(entry, successors(entry), 0usize)];
        seen.insert(entry, ());
        while let Some((block, succ, next)) = stack.last_mut() {
            if let Some(&to) = succ.get(*next) {
                *next += 1;
                if seen.insert(to, ()).is_none() {
                    let more = successors(to);
                    stack.push((to, more, 0));
                }
            } else {
                post.push(*block);
                stack.pop();
            }
        }
        let order: Vec<Block> = post.into_iter().rev().collect();
        let position: Map<Block, usize> = order.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let n = order.len();

        // Every edge, once for each time a terminator names it.
        let mut preds = vec![Vec::new(); n];
        let mut edges = Vec::new();
        for (from, &block) in order.iter().enumerate() {
            for to in successors(block) {
                let to = position[&to];
                preds[to].push(from);
                edges.push((from, to));
            }
        }

        // Cooper, Harvey and Kennedy, "A Simple, Fast Dominance Algorithm", over the positions,
        // where a smaller position is closer to the entry.
        let undefined = usize::MAX;
        let mut idom = vec![undefined; n];
        idom[0] = 0;
        let mut changed = true;
        while changed {
            changed = false;
            for b in 1..n {
                let mut new = undefined;
                for &p in &preds[b] {
                    if idom[p] == undefined {
                        continue;
                    }
                    new = if new == undefined { p } else { intersect(&idom, p, new) };
                }
                if new != idom[b] {
                    idom[b] = new;
                    changed = true;
                }
            }
        }

        let mut children = vec![Vec::new(); n];
        for b in 1..n {
            children[idom[b]].push(b);
        }
        let mut loop_header = vec![false; n];
        let mut forward = vec![0u32; n];
        for &(from, to) in &edges {
            if to <= from {
                if !dominates(&idom, to, from) {
                    return Err(order[to]);
                }
                loop_header[to] = true;
            } else {
                forward[to] += 1;
            }
        }
        let merge = forward.iter().map(|&count| count >= 2).collect();
        Ok(Shape { order, position, children, loop_header, merge })
    }

    /// Whether the edge from `from` to `to` goes back to a loop header.
    pub(crate) fn is_backward(&self, from: usize, to: usize) -> bool {
        to <= from
    }
}

fn intersect(idom: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while a > b {
            a = idom[a];
        }
        while b > a {
            b = idom[b];
        }
    }
    a
}

/// Whether `a` dominates `b`.
fn dominates(idom: &[usize], a: usize, mut b: usize) -> bool {
    loop {
        if b == a {
            return true;
        }
        if b == 0 {
            return false;
        }
        b = idom[b];
    }
}
