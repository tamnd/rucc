//! Who reads what, counted by occurrence.
//!
//! Two passes want the same question answered and neither of them wants to be the place the
//! answer is defined. Dead code elimination asks whether anything reads a value, and width
//! narrowing asks whether exactly one thing does, which is the difference between a rewrite that
//! replaces an instruction and a rewrite that adds a second one beside it.
//!
//! This is a count and not an analysis. It is built by walking the function, it goes stale the
//! moment anything is rewritten, and the pass that rewrites is the one that keeps it in step.
//! When there is an analysis manager it will hold a real use list with the instruction on the
//! other end of each use, and this will be the thing that list replaces.

use std::collections::HashMap;

use rucc_ir::{Block, Func, Inst, Value};

/// How many times each value is used, indexed by [`Value::index`].
///
/// By position rather than by instruction, because `x + x` uses `x` twice and a reader who wanted
/// to know whether removing one use leaves any needs both of them counted.
#[must_use]
pub fn count(func: &Func) -> Vec<u32> {
    let mut uses = vec![0u32; func.counts().values];
    for block in func.blocks().collect::<Vec<Block>>() {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            operands(func, inst, |value| uses[value.index()] += 1);
        }
    }
    uses
}

/// Every value this instruction reads, with a repeat for each time it reads it.
///
/// The arguments, and the arguments of the blocks it branches to. That is the whole of what an
/// instruction can use, and it is the same pair the verifier walks, so a use this misses is a use
/// the verifier would already be looking at from the other side.
pub fn operands(func: &Func, inst: Inst, mut each: impl FnMut(Value)) {
    for &value in &func[func[inst].args] {
        each(value);
    }
    for call in func.successors(inst) {
        for &value in &func[call.args] {
            each(value);
        }
    }
}

/// Points every reader of a value at another value, for every pair in the map.
///
/// The arguments of each instruction and the arguments of the blocks it branches to, which is the
/// whole of what an instruction can read and is the same pair [`operands`] walks. It is here
/// rather than in the pass that wanted it first because two passes want it: the peephole points a
/// reader at what a rule said the value is, and control flow simplification points a reader of a
/// block parameter at the argument the one branch to that block passed.
///
/// One walk over the function for the whole map rather than one walk per pair. A pass that
/// rewrote a hundred values would otherwise walk the function a hundred times, and the map is
/// what makes the cost of the walk independent of how much the pass did.
pub fn substitute(func: &mut Func, forward: &HashMap<Value, Value>) {
    let with = |value: Value| chase(forward, value);
    for block in func.blocks().collect::<Vec<Block>>() {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            let args = func[inst].args;
            func.rewrite(args, with);
            for call in func.successors(inst).collect::<Vec<_>>() {
                func.rewrite(call.args, with);
            }
        }
    }
}

/// Where a redirection ends up, following the ones already in the map.
///
/// A chain forms whenever one rewrite feeds another, `x + 0` read by `y * 1` in the peephole, and
/// a block parameter bound to an argument that is itself a parameter of a block merged a moment
/// earlier. Following it is what makes the second rewrite worth as much as the first.
///
/// The caller is what keeps this from running forever, by only ever pointing a value at one that
/// was already defined before it. Both callers do: a rule points a result at one of its own
/// operands, and a merge points a block's parameter at an argument passed by the block above it.
#[must_use]
pub fn chase(forward: &HashMap<Value, Value>, value: Value) -> Value {
    let mut value = value;
    while let Some(&next) = forward.get(&value) {
        value = next;
    }
    value
}
