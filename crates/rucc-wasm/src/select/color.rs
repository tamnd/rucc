//! Coloring: values that are never live at the same time share one local.
//!
//! This is section 8.3 of the WebAssembly notes. Without it, each value has a local of its own, and
//! each argument of an edge is a `local.get` and a `local.set` into the local of the parameter. A
//! function of SQLite then has hundreds of locals, many of them past index 127, where the index
//! takes two bytes, and many edges that are only a copy and a `br`. LLVM's `WebAssemblyRegColoring`
//! shares the locals in the same way after its register coalescer has joined the copies.
//!
//! The liveness is the liveness of the code that the selector writes, and not of the IR. An
//! instruction that stackify moved reads its operands where the root of its tree is written, and a
//! call in a `try_table` reads its operands where the branch after it is written. A value is taken
//! as written at the place of its instruction in the IR, which is never after the place where the
//! code writes its local, so a value can only seem live for longer than it is. The exception is a
//! value that a moved instruction writes with `local.tee`, which is written where the root of its
//! tree is written. It interferes with each value that the tree reads, other than the operands of
//! its own instruction, because the code of the tree can read them after the tee. An instruction
//! that is not sure to read all of its operands before it writes its results gets results that
//! interfere with its operands.
//!
//! Then the argument and the parameter of each edge join into one class when they do not
//! interfere, and the edge needs no copy. Last, each class takes the first local of its type that
//! no class it interferes with has, in the order of how many times the code reads and writes the
//! class, so that the busiest classes have the smallest indices. A parameter of the function keeps
//! its local, and another class can have that local after the parameter is dead.

use rucc_base::hash::Map;
use rucc_ir::{Inst, Opcode, Value};
use rucc_object::wasm::ValType;

use super::{Lower, Result};
use crate::{is_pair, valtype};

/// A set of nodes, one bit for each.
#[derive(Clone, PartialEq, Eq)]
struct Bits(Vec<u64>);

impl Bits {
    fn new(n: usize) -> Self {
        Bits(vec![0; n.div_ceil(64)])
    }

    fn insert(&mut self, i: u32) {
        self.0[i as usize / 64] |= 1 << (i % 64);
    }

    fn remove(&mut self, i: u32) {
        self.0[i as usize / 64] &= !(1 << (i % 64));
    }

    fn union(&mut self, other: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a |= b;
        }
    }

    fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.iter().enumerate().flat_map(|(w, &word)| {
            let mut word = word;
            std::iter::from_fn(move || {
                if word == 0 {
                    return None;
                }
                let bit = word.trailing_zeros();
                word &= word - 1;
                Some(w as u32 * 64 + bit)
            })
        })
    }
}

/// What one block does to the nodes, place by place.
#[derive(Default)]
struct Events {
    /// The nodes that each place writes.
    defs: Vec<Vec<u32>>,
    /// The nodes that each place reads after it writes them, which interfere with what it writes.
    early: Vec<Vec<u32>>,
    /// The nodes that each place reads before it writes anything.
    uses: Vec<Vec<u32>>,
    /// The nodes that a moved instruction writes with `local.tee` in the tree of each place, each
    /// with the operands of its own instruction.
    tees: Vec<Vec<(u32, Vec<u32>)>>,
    /// The parameters of the block.
    params: Vec<u32>,
}

/// The classes of nodes that share a local, as a forest with a root for each class.
struct Classes {
    parent: Vec<u32>,
}

impl Classes {
    fn find(&mut self, mut i: u32) -> u32 {
        while self.parent[i as usize] != i {
            let up = self.parent[self.parent[i as usize] as usize];
            self.parent[i as usize] = up;
            i = up;
        }
        i
    }
}

impl Lower<'_, '_> {
    /// Give each value a local, with the values that are never live at the same time sharing one.
    /// See the module documentation. A pair and the value of a `landing` get a local of their own,
    /// as in [`Self::assign`], because their locals are written in more than one place.
    pub(super) fn color(&mut self) -> Result<()> {
        let func = self.func;
        let blocks = self.blocks();
        let entry = func.entry();
        // The nodes, which are the values that take part, and the type of each.
        let mut nodes: Vec<Value> = Vec::new();
        let mut node: Map<Value, u32> = Map::default();
        let mut types: Vec<ValType> = Vec::new();
        // The local of each node that has one already, which is a parameter of the function.
        let mut fixed: Map<u32, u32> = Map::default();
        let mut own: Vec<Value> = Vec::new();
        for &block in &blocks {
            let params = func[block].params.iter().copied().filter(|&v| !func[v].ty.is_mem());
            let mut next = u32::from(self.sret);
            for value in params {
                let ty = func[value].ty;
                if Some(block) == entry {
                    let local = next;
                    next += if is_pair(ty) { 2 } else { 1 };
                    self.local.insert(value, local);
                    if !is_pair(ty) {
                        fixed.insert(u32::try_from(nodes.len()).expect("fewer nodes"), local);
                        node.insert(value, u32::try_from(nodes.len()).expect("fewer nodes"));
                        nodes.push(value);
                        types.push(valtype(ty)?);
                    }
                } else if is_pair(ty) {
                    own.push(value);
                } else {
                    node.insert(value, u32::try_from(nodes.len()).expect("fewer nodes"));
                    nodes.push(value);
                    types.push(valtype(ty)?);
                }
            }
            for inst in func.insts(block) {
                if matches!(
                    func[inst].opcode,
                    Opcode::IConst | Opcode::FConst | Opcode::GlobalAddr | Opcode::BlockAddr
                ) || self.rematerialized(inst)
                {
                    continue;
                }
                let landing = func[inst].opcode == Opcode::Landing;
                for value in func[inst].results() {
                    let ty = func[value].ty;
                    if ty.is_mem() || ty.is_void() || self.trees.stacked.contains(&value) {
                        continue;
                    }
                    if is_pair(ty) || landing {
                        own.push(value);
                    } else {
                        node.insert(value, u32::try_from(nodes.len()).expect("fewer nodes"));
                        nodes.push(value);
                        types.push(valtype(ty)?);
                    }
                }
            }
        }
        let n = nodes.len();
        let index: Map<_, usize> = blocks.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let mut weight = vec![0u64; n];
        let mut events: Vec<Events> = Vec::with_capacity(blocks.len());
        for &block in &blocks {
            let insts: Vec<Inst> = func.insts(block).collect();
            let at: Map<Inst, usize> =
                insts.iter().enumerate().map(|(i, &inst)| (inst, i)).collect();
            let term = func.terminator(block);
            let caught = term.and_then(|term| self.caught(term));
            let mut e = Events {
                defs: vec![Vec::new(); insts.len()],
                early: vec![Vec::new(); insts.len()],
                uses: vec![Vec::new(); insts.len()],
                tees: vec![Vec::new(); insts.len()],
                params: func[block].params.iter().filter_map(|v| node.get(v).copied()).collect(),
            };
            for (i, &inst) in insts.iter().enumerate() {
                let moved = self.trees.moved.contains(&inst);
                let root = self.trees.roots.get(&inst).map(|root| at[root]);
                let late = moved || self.pushes_once(inst);
                for value in self.results(inst) {
                    if let Some(&d) = node.get(&value) {
                        weight[d as usize] += 1;
                        match root {
                            Some(place) => {
                                let own = match late {
                                    true => self.inputs(inst),
                                    false => Vec::new(),
                                };
                                let own = own.iter().filter_map(|v| node.get(v).copied()).collect();
                                e.tees[place].push((d, own));
                            }
                            None => e.defs[i].push(d),
                        }
                    }
                }
                let place = match (root, caught, term) {
                    (Some(root), _, _) => root,
                    // A `ptr_add` that is not written reads nothing.
                    (None, _, _) if moved => continue,
                    (None, Some((call, unwound)), Some(term))
                        if inst == call || inst == unwound =>
                    {
                        at[&term]
                    }
                    _ => i,
                };
                let edges = func.successors(inst).flat_map(|call| {
                    let params = &func[call.block].params;
                    func[call.args].iter().copied().zip(params.iter().copied())
                });
                let edges: Vec<Value> =
                    edges.filter(|&(arg, param)| arg != param).map(|(arg, _)| arg).collect();
                // The selector writes the results last only for the instructions whose operands
                // stackify knows, which are pushed once each before the instruction is written.
                for value in self.inputs(inst).into_iter().chain(edges) {
                    if self.trees.takers.get(&value) == Some(&inst) {
                        continue;
                    }
                    if let Some(&u) = node.get(&value) {
                        weight[u as usize] += 1;
                        if late || place != i {
                            e.uses[place].push(u);
                        } else {
                            e.early[place].push(u);
                        }
                    }
                }
            }
            events.push(e);
        }
        // The nodes live at the start of each block, to a fixed point, from the last block back.
        let mut live_in: Vec<Bits> = vec![Bits::new(n); blocks.len()];
        let succs: Vec<Vec<usize>> = blocks
            .iter()
            .map(|&block| {
                let Some(term) = func.terminator(block) else { return Vec::new() };
                func.successors(term).filter_map(|call| index.get(&call.block).copied()).collect()
            })
            .collect();
        let live_out = |live_in: &[Bits], b: usize| {
            let mut out = Bits::new(n);
            for &s in &succs[b] {
                out.union(&live_in[s]);
            }
            out
        };
        let mut changed = true;
        while changed {
            changed = false;
            for b in (0..blocks.len()).rev() {
                let mut live = live_out(&live_in, b);
                scan(&events[b], &mut live, |_, _| {});
                if live != live_in[b] {
                    live_in[b] = live;
                    changed = true;
                }
            }
        }
        if self.code.writes.is_some() {
            for (b, live) in live_in.iter().enumerate() {
                for d in live.iter() {
                    self.entering.insert((blocks[b], nodes[d as usize]));
                }
            }
        }
        // Which nodes interfere, between nodes of one type only.
        let mut adjacent: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (b, events) in events.iter().enumerate() {
            let mut live = live_out(&live_in, b);
            scan(events, &mut live, |d, live| {
                for other in live.iter() {
                    if other != d && types[other as usize] == types[d as usize] {
                        adjacent[d as usize].push(other);
                        adjacent[other as usize].push(d);
                    }
                }
            });
        }
        for list in &mut adjacent {
            list.sort_unstable();
            list.dedup();
        }
        // The argument and the parameter of an edge join when no node of one class interferes
        // with a node of the other.
        let mut classes = Classes { parent: (0..n as u32).collect() };
        let mut members: Vec<Vec<u32>> = (0..n as u32).map(|i| vec![i]).collect();
        for &block in &blocks {
            let Some(term) = func.terminator(block) else { continue };
            for call in func.successors(term) {
                let params = &func[call.block].params;
                for (arg, param) in func[call.args].iter().zip(params) {
                    let (Some(&a), Some(&p)) = (node.get(arg), node.get(param)) else { continue };
                    if types[a as usize] != types[p as usize] {
                        continue;
                    }
                    let (ra, rp) = (classes.find(a), classes.find(p));
                    if ra == rp || (fixed.contains_key(&ra) && fixed.contains_key(&rp)) {
                        continue;
                    }
                    let (small, large) = if members[ra as usize].len() < members[rp as usize].len()
                    {
                        (ra, rp)
                    } else {
                        (rp, ra)
                    };
                    let clash = members[small as usize]
                        .iter()
                        .any(|&m| adjacent[m as usize].iter().any(|&o| classes.find(o) == large));
                    if clash {
                        continue;
                    }
                    // The root keeps the local of a parameter of the function.
                    let (from, to) =
                        if fixed.contains_key(&small) { (large, small) } else { (small, large) };
                    classes.parent[from as usize] = to;
                    let moved = std::mem::take(&mut members[from as usize]);
                    members[to as usize].extend(moved);
                }
            }
        }
        // Each class takes a local, the busiest first.
        let mut roots: Vec<u32> = (0..n as u32).filter(|&i| classes.find(i) == i).collect();
        let total =
            |root: u32| -> u64 { members[root as usize].iter().map(|&m| weight[m as usize]).sum() };
        let weights: Map<u32, u64> = roots.iter().map(|&r| (r, total(r))).collect();
        roots.sort_by_key(|&r| (!fixed.contains_key(&r), std::cmp::Reverse(weights[&r]), r));
        // The colors of each type, and the local of a parameter where the color is one.
        let mut colors: Map<ValType, Vec<Option<u32>>> = Map::default();
        let mut color: Map<u32, (ValType, usize)> = Map::default();
        for &root in &roots {
            let ty = types[root as usize];
            let pool = colors.entry(ty).or_default();
            let taken: rucc_base::hash::Set<usize> = members[root as usize]
                .iter()
                .flat_map(|&m| adjacent[m as usize].iter())
                .filter_map(|&o| {
                    let r = classes.find(o);
                    color.get(&r).map(|&(_, c)| c)
                })
                .collect();
            let c = match fixed.get(&root) {
                Some(&local) => {
                    pool.push(Some(local));
                    pool.len() - 1
                }
                None => match (0..pool.len()).find(|c| !taken.contains(c)) {
                    Some(c) => c,
                    None => {
                        pool.push(None);
                        pool.len() - 1
                    }
                },
            };
            color.insert(root, (ty, c));
        }
        // The locals, in the order of the busiest color of each.
        let mut busy: Map<(ValType, usize), u64> = Map::default();
        for &root in &roots {
            *busy.entry(color[&root]).or_default() += weights[&root];
        }
        let mut order: Vec<((ValType, usize), u64)> =
            busy.into_iter().filter(|&((ty, c), _)| colors[&ty][c].is_none()).collect();
        order.sort_by_key(|&((ty, c), w)| (std::cmp::Reverse(w), ty as u8, c));
        let mut local_of: Map<(ValType, usize), u32> = Map::default();
        for ((ty, c), _) in order {
            local_of.insert((ty, c), self.new_local(ty));
        }
        for (i, &value) in nodes.iter().enumerate() {
            let (ty, c) = color[&classes.find(i as u32)];
            let local = match colors[&ty][c] {
                Some(local) => local,
                None => local_of[&(ty, c)],
            };
            self.local.insert(value, local);
        }
        for value in own {
            let local = self.local_for(func[value].ty)?;
            self.local.insert(value, local);
        }
        Ok(())
    }
}

/// Walk the places of one block from the last to the first with `live` the nodes live after it,
/// and leave in `live` the nodes live at its start. `def` sees each node written and the nodes
/// live just after the write.
fn scan(events: &Events, live: &mut Bits, mut def: impl FnMut(u32, &Bits)) {
    for place in (0..events.defs.len()).rev() {
        for &u in &events.early[place] {
            live.insert(u);
        }
        for &d in &events.defs[place] {
            def(d, live);
            live.remove(d);
        }
        let tees = &events.tees[place];
        if !tees.is_empty() {
            for (d, own) in tees {
                // Each read of the tree that is not an operand of the tee's own instruction can come
                // after the tee.
                let mut reads = events.uses[place].clone();
                for u in own {
                    if let Some(at) = reads.iter().position(|r| r == u) {
                        reads.swap_remove(at);
                    }
                }
                let mut after = live.clone();
                for u in reads {
                    after.insert(u);
                }
                def(*d, &after);
            }
            for (d, _) in tees {
                live.remove(*d);
            }
        }
        for &u in &events.uses[place] {
            live.insert(u);
        }
    }
    // The edges write all the parameters, so each one interferes with the others.
    for &p in &events.params {
        live.insert(p);
    }
    for &p in &events.params {
        live.remove(p);
        def(p, live);
        live.insert(p);
    }
    for &p in &events.params {
        live.remove(p);
    }
}
