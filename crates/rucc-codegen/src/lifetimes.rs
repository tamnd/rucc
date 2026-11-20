//! The ends of lifetimes the front end wrote, kept where they can be trusted and taken out where
//! they cannot.
//!
//! The front end writes a `lifetime_end` of a local's slot on every way out of the block the local
//! was declared in that it can see, and only when the slots are going to be shared, which is
//! tamnd/rucc#2201. What an end says is that the bytes of the local are nobody's business after it
//! until something takes the address of the local again. [`crate::slots`] leans on that to let a
//! local whose address was handed to a call share its bytes with one declared after it, which is
//! most of the frame of a function that fills in a large structure in each arm of an `if` and hands
//! it to something.
//!
//! The one thing the optimizer may have done to spoil that is keep an address worked out from the
//! local in a value across an end, and use it again on the other side before the address is taken
//! again. A loop body whose `&s.field` was hoisted out to the preheader is the plain case, and the
//! preheader is fine there, but a value carried around the loop as a block argument is not. The
//! lowering writes the address of a slot again in every block that reads it, so the slot itself is
//! never the problem. Values computed from it are, and [`settle`] looks for one of those that is
//! live at an end of the slot it came from and throws out every end of that slot when it finds
//! one. A slot with no ends is one [`crate::slots`] treats the way it did before any of this, so
//! that is always an allowed answer.
//!
//! What this cannot see is an address that went through memory. It does not have to: a pointer
//! read back from memory and used after the end of the object it points at is a program reading an
//! object whose lifetime is over, which is what the end means in the first place.

use rucc_base::hash::{Map, Set};
use rucc_ir::{Block, Def, Func, Inst, Opcode, Value};

/// How many steps [`settle`] may take over one function before it gives up and takes every end
/// out, which is what a function it did not look at gets anyway.
const BUDGET: usize = 1 << 22;

/// Keeps the ends of lifetimes the rest of the back end can trust and takes out the rest.
///
/// `keep` is whether the slots are going to be shared at all. When they are not, every end comes
/// out here, so that nothing after this has to know what one is.
pub fn settle(func: &mut Func, keep: bool) {
    let ends: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::LifetimeEnd)
        .collect();
    if ends.is_empty() {
        return;
    }
    if !keep {
        for inst in ends {
            func.remove_inst(inst);
        }
        return;
    }

    let mut of: Map<Value, Vec<Inst>> = Map::default();
    for &inst in &ends {
        match func[func[inst].args].first().copied().filter(|&slot| fixed(func, slot)) {
            Some(slot) => of.entry(slot).or_default().push(inst),
            None => func.remove_inst(inst),
        }
    }

    let mut budget = BUDGET;
    let graph = Graph::of(func);
    let mut wrong = Vec::new();
    let mut slots: Vec<Value> = of.keys().copied().collect();
    slots.sort_unstable_by_key(|slot| slot.index());
    for slot in slots {
        match crosses(func, &graph, slot, &of[&slot], &mut budget) {
            Some(false) => {}
            Some(true) => wrong.push(slot),
            None => {
                wrong = of.keys().copied().collect();
                break;
            }
        }
    }
    for slot in wrong {
        for &inst in &of[&slot] {
            func.remove_inst(inst);
        }
    }
}

/// Whether a value is a slot of the function's own of a size known here, which is the only kind
/// the front end writes an end for and the only kind the frame lays out itself.
fn fixed(func: &Func, value: Value) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    func[inst].opcode == Opcode::Alloca && func[func[inst].args].is_empty()
}

/// Who reads each value, and which blocks come before each block.
struct Graph {
    /// The instructions reading each value, a terminator counted once for each argument it hands a
    /// block, along with which parameter of which block that argument becomes.
    readers: Lists<(Inst, Option<Value>)>,
    /// The blocks that jump to each block.
    preds: Lists<Block>,
    /// Where each instruction is in its block, counting from the top.
    places: Map<Inst, usize>,
}

impl Graph {
    fn of(func: &Func) -> Self {
        let mut readers = Vec::new();
        let mut preds = Vec::new();
        let mut places: Map<Inst, usize> = Map::default();
        for block in func.blocks() {
            for (place, inst) in func.insts(block).enumerate() {
                places.insert(inst, place);
                for &arg in &func[func[inst].args] {
                    readers.push((arg.index(), (inst, None)));
                }
            }
            let Some(term) = func.terminator(block) else { continue };
            for call in func.successors(term) {
                preds.push((call.block.index(), block));
                let params = &func[call.block].params;
                for (&arg, &param) in func[call.args].iter().zip(params) {
                    readers.push((arg.index(), (term, Some(param))));
                }
            }
        }
        Self { readers: Lists::of(readers), preds: Lists::of(preds), places }
    }

    /// Whether one instruction comes before another in the block they are both in.
    fn before(&self, first: Inst, second: Inst) -> bool {
        self.places.get(&first) < self.places.get(&second)
    }
}

/// Lists by index, side by side in one buffer, with where each one starts in it.
///
/// A list of its own for every value read and every block jumped to was an allocation each, for
/// every function with an end in it.
struct Lists<T> {
    starts: Vec<usize>,
    all: Vec<T>,
}

impl<T: Copy> Lists<T> {
    /// Each item under its index, in the order the items came.
    fn of(mut pairs: Vec<(usize, T)>) -> Self {
        // Stable, so each list keeps the order its items came in.
        pairs.sort_by_key(|&(at, _)| at);
        let len = pairs.last().map_or(0, |&(at, _)| at + 1);
        let mut starts = vec![0; len + 1];
        for &(at, _) in &pairs {
            starts[at] += 1;
        }
        let mut total = 0;
        for start in &mut starts {
            let count = *start;
            *start = total;
            total += count;
        }
        Self { starts, all: pairs.into_iter().map(|(_, item)| item).collect() }
    }

    /// The list under one index, which is empty when nothing was put there.
    fn get(&self, at: usize) -> &[T] {
        match (self.starts.get(at), self.starts.get(at + 1)) {
            (Some(&start), Some(&end)) => &self.all[start..end],
            _ => &[],
        }
    }
}

/// Whether some value worked out from a slot is live at one of the slot's ends, or `None` when
/// the question took more than the budget left.
///
/// Worked out from means reached from the slot by anything but a load or a call. A load reads what
/// is in the slot rather than where it is. What a call hands back is a pointer the callee chose,
/// which is the same as one read back out of memory: a program that goes on using it after the end
/// is reading an object that is gone. And a value of the memory type is the order of memory
/// operations rather than an address. Every other result of something that reads the slot, or a
/// value worked out from it, counts, which is more than the addresses and is allowed to be.
fn crosses(
    func: &Func,
    graph: &Graph,
    slot: Value,
    ends: &[Inst],
    budget: &mut usize,
) -> Option<bool> {
    let mut derived: Vec<Value> = Vec::new();
    let mut seen: Set<Value> = std::iter::once(slot).collect();
    let mut queue = vec![slot];
    while let Some(value) = queue.pop() {
        for &(inst, param) in graph.readers.get(value.index()) {
            *budget = budget.checked_sub(1)?;
            let skipped = param.is_some()
                || matches!(func[inst].opcode, Opcode::Load | Opcode::Call | Opcode::CallIndirect);
            let results = (!skipped).then(|| func[inst].results()).into_iter().flatten();
            for next in param.into_iter().chain(results) {
                if !func[next].ty.is_mem() && seen.insert(next) {
                    derived.push(next);
                    queue.push(next);
                }
            }
        }
    }

    let (mut into, mut walk) = (Set::default(), Vec::new());
    for value in derived {
        for &end in ends {
            if live_at(func, graph, value, end, budget, (&mut into, &mut walk))? {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// Whether a value is live at an instruction: defined on some path to it and read on some path
/// from it.
///
/// `scratch` is the set of blocks the value is live into and the blocks still to walk, which the
/// caller keeps from one question to the next.
fn live_at(
    func: &Func,
    graph: &Graph,
    value: Value,
    at: Inst,
    budget: &mut usize,
    scratch: (&mut Set<Block>, &mut Vec<Block>),
) -> Option<bool> {
    let (into, walk) = scratch;
    let Some(here) = func.block_of(at) else { return Some(false) };
    let (home, defined_before) = match func[value].def {
        Def::Result { inst, .. } => {
            let home = func.block_of(inst);
            (home, home == Some(here) && graph.before(inst, at))
        }
        Def::Param { block, .. } => (Some(block), block == here),
    };
    let Some(home) = home else { return Some(false) };

    // The blocks the value is live into, found by walking up from every read to the definition.
    into.clear();
    let mut read_after = false;
    walk.clear();
    for &(reader, param) in graph.readers.get(value.index()) {
        *budget = budget.checked_sub(1)?;
        let Some(block) = func.block_of(reader) else { continue };
        // A block argument is read at the bottom of the block handing it over, and so is every
        // read after the end in the same block.
        if block == here && (param.is_some() || graph.before(at, reader)) {
            read_after = true;
        }
        // Read anywhere but the block it is defined in, which it has to arrive at the top of. The
        // definition dominates every read, so the walk up from one stops at the definition.
        if block != home {
            walk.push(block);
        }
    }
    while let Some(block) = walk.pop() {
        *budget = budget.checked_sub(1)?;
        if !into.insert(block) {
            continue;
        }
        for &pred in graph.preds.get(block.index()) {
            if pred != home {
                walk.push(pred);
            }
        }
    }

    let arrives = defined_before || into.contains(&here);
    if !arrives {
        return Some(false);
    }
    if read_after {
        return Some(true);
    }
    // Live out of the block when some block after it has it coming in, or is its own home and
    // takes it round a loop as a parameter, which the reads above already counted.
    let term = func.terminator(here);
    let leaves =
        term.is_some_and(|term| func.successors(term).any(|call| into.contains(&call.block)));
    Some(leaves)
}
