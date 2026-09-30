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

use std::collections::{HashMap, HashSet};

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

    let mut of: HashMap<Value, Vec<Inst>> = HashMap::new();
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
    readers: HashMap<Value, Vec<(Inst, Option<Value>)>>,
    /// The blocks that jump to each block.
    preds: HashMap<Block, Vec<Block>>,
    /// Where each instruction is in its block, counting from the top.
    places: HashMap<Inst, usize>,
}

impl Graph {
    fn of(func: &Func) -> Self {
        let mut readers: HashMap<Value, Vec<(Inst, Option<Value>)>> = HashMap::new();
        let mut preds: HashMap<Block, Vec<Block>> = HashMap::new();
        let mut places: HashMap<Inst, usize> = HashMap::new();
        for block in func.blocks() {
            for (place, inst) in func.insts(block).enumerate() {
                places.insert(inst, place);
                for &arg in &func[func[inst].args] {
                    readers.entry(arg).or_default().push((inst, None));
                }
            }
            let Some(term) = func.terminator(block) else { continue };
            for call in func.successors(term) {
                preds.entry(call.block).or_default().push(block);
                let params = &func[call.block].params;
                for (&arg, &param) in func[call.args].iter().zip(params) {
                    readers.entry(arg).or_default().push((term, Some(param)));
                }
            }
        }
        Self { readers, preds, places }
    }

    /// Whether one instruction comes before another in the block they are both in.
    fn before(&self, first: Inst, second: Inst) -> bool {
        self.places.get(&first) < self.places.get(&second)
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
    let mut seen: HashSet<Value> = HashSet::from([slot]);
    let mut queue = vec![slot];
    while let Some(value) = queue.pop() {
        for &(inst, param) in graph.readers.get(&value).map(Vec::as_slice).unwrap_or_default() {
            *budget = budget.checked_sub(1)?;
            let next: Vec<Value> = match param {
                Some(param) => vec![param],
                None if matches!(
                    func[inst].opcode,
                    Opcode::Load | Opcode::Call | Opcode::CallIndirect
                ) =>
                {
                    Vec::new()
                }
                None => func[inst].results().collect(),
            };
            for next in next {
                if !func[next].ty.is_mem() && seen.insert(next) {
                    derived.push(next);
                    queue.push(next);
                }
            }
        }
    }

    for value in derived {
        for &end in ends {
            if live_at(func, graph, value, end, budget)? {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// Whether a value is live at an instruction: defined on some path to it and read on some path
/// from it.
fn live_at(func: &Func, graph: &Graph, value: Value, at: Inst, budget: &mut usize) -> Option<bool> {
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
    let mut into: HashSet<Block> = HashSet::new();
    let mut read_after = false;
    let mut walk: Vec<Block> = Vec::new();
    for &(reader, param) in graph.readers.get(&value).map(Vec::as_slice).unwrap_or_default() {
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
        for &pred in graph.preds.get(&block).map(Vec::as_slice).unwrap_or_default() {
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
