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

use rucc_base::hash::Map;
use rucc_ir::{Block, Func, Inst, Value};

/// How many times each value is used, indexed by [`Value::index`].
///
/// By position rather than by instruction, because `x + x` uses `x` twice and a reader who wanted
/// to know whether removing one use leaves any needs both of them counted.
#[must_use]
pub fn count(func: &Func) -> Vec<u32> {
    let mut uses = vec![0u32; func.counts().values];
    for inst in func.blocks().flat_map(|block| func.insts(block)) {
        operands(func, inst, |value| uses[value.index()] += 1);
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
pub fn substitute(func: &mut Func, forward: &Map<Value, Value>) {
    let with = |value: Value| chase(forward, value);
    // One list of instructions and one of edges, filled again for each block and each terminator,
    // where a list was made for each of them. tamnd/rucc#3052.
    let mut insts = Vec::new();
    let mut calls = Vec::new();
    for block in func.blocks().collect::<Vec<Block>>() {
        insts.clear();
        insts.extend(func.insts(block));
        for &inst in &insts {
            let args = func[inst].args;
            func.rewrite(args, with);
            calls.clear();
            calls.extend(func.successors(inst));
            for call in &calls {
                func.rewrite(call.args, with);
            }
        }
    }
    rename(func, forward);
}

/// [`substitute`] for a caller that will not put back an instruction it took out of a block.
///
/// One pass over the function's runs of operands with nothing allocated, where [`substitute`]
/// walks every block and collects each one first. The inliner points the results of each call it
/// replaces at the block after it, and a caller that takes hundreds of calls walked itself once
/// for every one of them. The runs of instructions already out of their blocks are rewritten too,
/// which nothing sees.
pub fn substitute_all(func: &mut Func, forward: &Map<Value, Value>) {
    // The results of one instruction are numbered one after another, so the keys are a short range
    // and almost every operand is outside it, which is a compare rather than a lookup.
    let (Some(&low), Some(&high)) = (forward.keys().min(), forward.keys().max()) else {
        return;
    };
    let within = low..=high;
    func.rewrite_all(|value| if within.contains(&value) { chase(forward, value) } else { value });
    rename(func, forward);
}

/// Moves the names of the values in the map to the values they point at.
///
/// The second half of [`substitute`], for a pass that points the readers at the new values itself
/// because it knows who they are and walking the whole function to find them would cost more.
pub fn rename(func: &mut Func, forward: &Map<Value, Value>) {
    // The names go where the readers went. A declaration that was spelled by the value pointed
    // away from is spelled by the one left, and a build asked for debugging information would
    // otherwise have a name on a value nothing computes. Sorted first so that a map walked in
    // whatever order it hashes in still leaves the same function behind.
    let mut moving: Vec<Value> = forward.keys().copied().collect();
    moving.sort_unstable();
    for from in moving {
        func.rename_value(from, chase(forward, from));
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
pub fn chase(forward: &Map<Value, Value>, value: Value) -> Value {
    let mut value = value;
    while let Some(&next) = forward.get(&value) {
        value = next;
    }
    value
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_base::hash::Map;
    use rucc_ir::{Builder, Func, Signature, Type, Value};

    use super::substitute;

    /// A function of three constants, returning the first of them.
    ///
    /// Small on purpose: what this test is about is what a substitution does to the names, and the
    /// shape it happens in makes no difference to that.
    fn three(names: &mut Interner) -> (Func, Vec<Value>) {
        let i32_ = Type::int(32);
        let sig = Signature::new().with_returns(&[i32_]);
        let mut func = Func::new(names.intern("three"), sig);
        let entry = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let held: Vec<Value> = (1i128..=3).map(|value| build.iconst(i32_, value)).collect();
        build.ret(&[held[0]]);
        (func, held)
    }

    #[test]
    fn a_substitution_moves_a_name_to_the_value_the_readers_were_pointed_at_from_where_it_was() {
        let mut names = Interner::new();
        let (mut func, held) = three(&mut names);
        func.declare_value(held[0], 7);
        func.declare_value(held[1], 8);

        // A chain, which is what a pass leaves behind when one of its rewrites feeds another.
        let forward: Map<_, _> = [(held[0], held[1]), (held[1], held[2])].into_iter().collect();
        substitute(&mut func, &forward);

        let entry = func.blocks().next().expect("a block");
        let ret = func.insts(entry).last().expect("the return");
        assert_eq!(func[func[ret].args], [held[2]], "the readers went to the end of the chain");
        // Each name holds the value left from where the one it spelled was computed, since that is
        // where the declaration was given it.
        let first = func.insts(entry).next();
        let starts: Vec<(u32, Option<rucc_ir::Inst>)> =
            func.value_starts(held[2]).map(|start| (start.decl, start.after)).collect();
        assert_eq!(starts, vec![(7, None), (8, first)]);
        assert_eq!(func.value_decls(held[2]).count(), 0);
        assert_eq!(func.value_decls(held[0]).count(), 0, "nothing is left on what is read no more");
        assert_eq!(func.value_decls(held[1]).count(), 0);
    }
}
