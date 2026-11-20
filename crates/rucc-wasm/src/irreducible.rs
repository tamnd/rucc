//! A dispatch node for each loop with more than one entry, which makes any graph reducible.
//!
//! C makes such a loop with a `goto` into it, and Duff's device is the one that is seen most. The
//! structure of [`crate::structure`] needs one header for each loop, and a loop with two entries
//! has none, because neither entry dominates the other. This is section 7.3 of the WebAssembly
//! notes and the method of LLVM's `WebAssemblyFixIrreducibleControlFlow`: each strongly connected
//! component with two or more entries gets a dispatch node, every edge to one of the entries goes
//! to the dispatch node instead, and the edge first writes the number of its entry in the label
//! local of the dispatch node. The dispatch node is then the one entry of the loop, and it goes
//! to the entry by a `br_table` on the label. The components inside a loop, with its header
//! taken out, are made reducible in the same way, so a loop inside a loop is done too.
//!
//! The edges into the region from inside it go through the dispatch node as well. If they went
//! straight to their entries, the cycle through the two entries would not pass the dispatch node
//! and it would still have two entries.
//!
//! No code is copied, and the IR is not changed. Every block parameter is a local in the wasm
//! function, so an edge writes the parameters of the block that it goes to before it branches,
//! wherever it branches to, and the dispatch node has no parameters of its own. A reducible
//! graph is not touched, so its code is the same as before.

use std::collections::hash_map::Entry;

use rucc_base::hash::Map;
use rucc_ir::{Block, Func};

/// A node of the graph that the structure is built from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Node {
    /// A block of the function.
    Block(Block),
    /// The dispatch node of a loop with more than one entry, and the number of its label among
    /// the dispatch nodes of the function.
    Dispatch(usize),
}

/// An edge of the graph.
#[derive(Clone, Debug)]
pub(crate) struct Edge {
    /// The node that the edge goes to.
    pub(crate) to: usize,
    /// The labels that the edge writes before it goes, as the number of the label and the value.
    /// An edge to an entry of a loop that a dispatch node was put in front of writes the number
    /// of the entry, and an edge that passes two dispatch nodes writes two labels.
    pub(crate) labels: Vec<(usize, u32)>,
}

/// The graph of the reachable blocks of a function, with the dispatch nodes that were added.
///
/// The edges of a block are in the order of the targets of its terminator, so that an edge is
/// named by the index of its target, as [`Func::successors`] gives them. The edges of a dispatch
/// node are in the order of the values of its label.
pub(crate) struct Graph {
    pub(crate) nodes: Vec<Node>,
    pub(crate) edges: Vec<Vec<Edge>>,
    /// The node where the function starts, which is the entry block until the entry block is an
    /// entry of a loop with more than one entry.
    pub(crate) start: usize,
    /// How many dispatch nodes there are.
    pub(crate) dispatches: usize,
}

impl Graph {
    /// The graph of the blocks that `entry` reaches.
    pub(crate) fn of(func: &Func, entry: Block) -> Graph {
        let successors = |block: Block| -> Vec<Block> {
            func.terminator(block)
                .map(|term| func.successors(term).map(|call| call.block).collect())
                .unwrap_or_default()
        };
        let mut number: Map<Block, usize> = Map::default();
        let mut nodes = vec![Node::Block(entry)];
        number.insert(entry, 0);
        let mut next = 0;
        while let Some(&Node::Block(block)) = nodes.get(next) {
            next += 1;
            for to in successors(block) {
                let next_number = nodes.len();
                if let Entry::Vacant(slot) = number.entry(to) {
                    slot.insert(next_number);
                    nodes.push(Node::Block(to));
                }
            }
        }
        let edges = nodes
            .iter()
            .map(|&node| {
                let Node::Block(block) = node else { unreachable!("only blocks so far") };
                successors(block)
                    .into_iter()
                    .map(|to| Edge { to: number[&to], labels: Vec::new() })
                    .collect()
            })
            .collect();
        Graph { nodes, edges, start: 0, dispatches: 0 }
    }

    /// Put a dispatch node in front of each loop with more than one entry, at every depth.
    pub(crate) fn untangle(&mut self) {
        let region = vec![true; self.nodes.len()];
        self.untangle_within(region);
    }

    /// The same for the loops that are made of the nodes in `region`.
    fn untangle_within(&mut self, region: Vec<bool>) {
        for component in self.components(&region) {
            let looped = component.len() > 1
                || self.edges[component[0]].iter().any(|edge| edge.to == component[0]);
            if !looped {
                continue;
            }
            let mut inside = vec![false; self.nodes.len()];
            for &node in &component {
                inside[node] = true;
            }
            let entries = self.entries(&inside);
            let header = if let [entry] = entries[..] { entry } else { self.dispatch(&entries) };
            // The loops inside this one are the components of the loop with its header out.
            inside.resize(self.nodes.len(), false);
            inside[header] = false;
            self.untangle_within(inside);
        }
    }

    /// The nodes of `inside` that an edge from outside reaches, and the start when it is inside,
    /// in ascending order.
    fn entries(&self, inside: &[bool]) -> Vec<usize> {
        let mut entry = vec![false; self.nodes.len()];
        if inside[self.start] {
            entry[self.start] = true;
        }
        for (from, edges) in self.edges.iter().enumerate() {
            if inside[from] {
                continue;
            }
            for edge in edges {
                if inside[edge.to] {
                    entry[edge.to] = true;
                }
            }
        }
        (0..self.nodes.len()).filter(|&node| entry[node]).collect()
    }

    /// Add a dispatch node in front of `entries`, and send every edge to one of them through it.
    /// The start of the function goes through it too when the start is one of them, and the
    /// label is then zero, which is the value that wasm gives each local when the function
    /// starts, so the start is always the first of the entries.
    fn dispatch(&mut self, entries: &[usize]) -> usize {
        let label = self.dispatches;
        self.dispatches += 1;
        let node = self.nodes.len();
        for edges in &mut self.edges {
            for edge in edges {
                if let Some(which) = entries.iter().position(|&entry| entry == edge.to) {
                    edge.to = node;
                    let which = u32::try_from(which).expect("fewer than 2^32 entries");
                    edge.labels.push((label, which));
                }
            }
        }
        if entries.contains(&self.start) {
            debug_assert_eq!(entries[0], self.start, "the start is the first entry");
            self.start = node;
        }
        self.nodes.push(Node::Dispatch(label));
        self.edges.push(entries.iter().map(|&to| Edge { to, labels: Vec::new() }).collect());
        node
    }

    /// The strongly connected components of the nodes in `region`, by Tarjan's method with an
    /// explicit stack, so that a long chain of blocks does not use up the stack of the compiler.
    fn components(&self, region: &[bool]) -> Vec<Vec<usize>> {
        let n = self.nodes.len();
        let unseen = usize::MAX;
        let mut index = vec![unseen; n];
        let mut low = vec![0; n];
        let mut on_stack = vec![false; n];
        let mut stack = Vec::new();
        let mut out = Vec::new();
        let mut next = 0;
        for root in 0..n {
            if !region[root] || index[root] != unseen {
                continue;
            }
            index[root] = next;
            low[root] = next;
            next += 1;
            stack.push(root);
            on_stack[root] = true;
            let mut work = vec![(root, 0usize)];
            while let Some(&(v, i)) = work.last() {
                if let Some(edge) = self.edges[v].get(i) {
                    work.last_mut().expect("v is on the work list").1 += 1;
                    let w = edge.to;
                    if !region[w] {
                        continue;
                    }
                    if index[w] == unseen {
                        index[w] = next;
                        low[w] = next;
                        next += 1;
                        stack.push(w);
                        on_stack[w] = true;
                        work.push((w, 0));
                    } else if on_stack[w] {
                        low[v] = low[v].min(index[w]);
                    }
                } else {
                    work.pop();
                    if let Some(&(parent, _)) = work.last() {
                        low[parent] = low[parent].min(low[v]);
                    }
                    if low[v] == index[v] {
                        let mut component = Vec::new();
                        loop {
                            let w = stack.pop().expect("v is on the stack");
                            on_stack[w] = false;
                            component.push(w);
                            if w == v {
                                break;
                            }
                        }
                        out.push(component);
                    }
                }
            }
        }
        out
    }
}
