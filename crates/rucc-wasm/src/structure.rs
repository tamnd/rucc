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
//! to a block that dominates its source. C with no `goto` into a loop gives such a graph. For a
//! graph that is not reducible, [`crate::irreducible`] puts a dispatch node in front of each loop
//! with more than one entry, and the facts are about the graph with those nodes in it.

use rucc_ir::Func;

use crate::irreducible::{Edge, Graph, Node};

/// The facts about the reachable nodes of one function, by position in reverse postorder.
pub(crate) struct Shape {
    /// The nodes in reverse postorder. The start is first.
    pub(crate) order: Vec<Node>,
    /// The edges of each node, with [`Edge::to`] as a position. The edges of a block are in the
    /// order of the targets of its terminator.
    pub(crate) edges: Vec<Vec<Edge>>,
    /// How many dispatch nodes there are, each of which has a label local.
    pub(crate) dispatches: usize,
    /// The nodes that each node immediately dominates, in reverse postorder.
    pub(crate) children: Vec<Vec<usize>>,
    pub(crate) loop_header: Vec<bool>,
    pub(crate) merge: Vec<bool>,
}

impl Shape {
    /// The facts about `func`, with a dispatch node in front of each loop with more than one
    /// entry when there is such a loop.
    pub(crate) fn of(func: &Func) -> Result<Shape, String> {
        let Some(entry) = func.entry() else {
            return Ok(Shape {
                order: Vec::new(),
                edges: Vec::new(),
                dispatches: 0,
                children: Vec::new(),
                loop_header: Vec::new(),
                merge: Vec::new(),
            });
        };
        let mut graph = Graph::of(func, entry);
        if let Some(shape) = Shape::reducible(&graph) {
            return Ok(shape);
        }
        graph.untangle();
        Shape::reducible(&graph).ok_or_else(|| {
            "the graph of its blocks is not reducible after the dispatch nodes were added".into()
        })
    }

    /// The facts about `graph`, or nothing when it is not reducible.
    fn reducible(graph: &Graph) -> Option<Shape> {
        // A depth first walk with an explicit stack, so that a function with a long chain of
        // blocks does not use up the stack of the compiler.
        let mut post = Vec::new();
        let mut seen = vec![false; graph.nodes.len()];
        let mut stack = vec![(graph.start, 0usize)];
        seen[graph.start] = true;
        while let Some(&(node, next)) = stack.last() {
            if let Some(edge) = graph.edges[node].get(next) {
                stack.last_mut().expect("the node is on the stack").1 += 1;
                if !seen[edge.to] {
                    seen[edge.to] = true;
                    stack.push((edge.to, 0));
                }
            } else {
                post.push(node);
                stack.pop();
            }
        }
        let numbers: Vec<usize> = post.into_iter().rev().collect();
        let mut position = vec![usize::MAX; graph.nodes.len()];
        for (at, &number) in numbers.iter().enumerate() {
            position[number] = at;
        }
        let n = numbers.len();
        let order: Vec<Node> = numbers.iter().map(|&number| graph.nodes[number]).collect();
        let node_edges: Vec<Vec<Edge>> = numbers
            .iter()
            .map(|&number| {
                graph.edges[number]
                    .iter()
                    .map(|edge| Edge { to: position[edge.to], labels: edge.labels.clone() })
                    .collect()
            })
            .collect();

        // Every edge, once for each time a terminator names it.
        let mut preds = vec![Vec::new(); n];
        let mut edges = Vec::new();
        for (from, out) in node_edges.iter().enumerate() {
            for edge in out {
                preds[edge.to].push(from);
                edges.push((from, edge.to));
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
                    return None;
                }
                loop_header[to] = true;
            } else {
                forward[to] += 1;
            }
        }
        let merge = forward.iter().map(|&count| count >= 2).collect();
        Some(Shape {
            order,
            edges: node_edges,
            dispatches: graph.dispatches,
            children,
            loop_header,
            merge,
        })
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
