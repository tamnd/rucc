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
