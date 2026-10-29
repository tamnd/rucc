//! A branch on an and or an or of two bits, as the two branches gcc writes for it.
//!
//! A branch reaches here after `rucc_opt::short_circuit` has turned `if (a && b)` into one
//! branch on `a & b` with both comparisons worked out in front of it. gcc does the same to the
//! tree in `ifcombine`, and then its expander takes it back apart. `expand_gimple_cond` in
//! `gcc/cfgexpand.cc` turns a branch on a one bit `&` or `|` into `&&` or `||` again whenever
//! `BRANCH_COST` is below 4 and the target has no conditional compare, which is every x86 and
//! RISC-V tuning. Each half is then a compare and a conditional jump of its own.
//!
//! Left as one branch, the and costs two `setcc`, an `and` of the two bytes and a `test`, where
//! gcc writes the two jumps. Over the kernel's `mm` and `kernel` objects that was three hundred
//! more `setne` than gcc has, most of them in this shape.
//!
//! # The shape
//!
//! For `br_if (a & b), yes(..), no(..)` in `head`:
//!
//! ```text
//! head:                         rest:
//!   br_if a, rest(), no(..)       br_if b, yes(..), no(..)
//! ```
//!
//! and for `|` the first branch goes to `yes` when `a` holds and to `rest` when it does not. The
//! arguments the old edges carried are defined in `head` or above it, and `head` is the one way
//! into `rest`, so both branches can carry them.
//!
//! The and has to be read by the branch and by nothing else, and has to be in the same block,
//! which is what gcc's expander asks of a value before it looks through it. What worked out `b`
//! and nothing else goes down into `rest` with it, which is the work the first branch now skips,
//! and the comparison for `a` goes down to the branch, so each comparison sits in front of the
//! jump that reads it.
//!
//! # The weights
//!
//! A hint on the old branch is shared out the way `do_jump_1` in `gcc/dojump.cc` shares it. For an
//! and, half the chance of the whole thing being false goes to each branch, and for an or, half the
//! chance of it being true.

use rucc_base::hash::Map;
use rucc_ir::{
    Block, BlockCall, Builder, Def, Extra, Func, Hint, Inst, IntPred, Opcode, Type, Value,
};

/// Every branch on an and or an or of two bits in the function, as two branches.
pub fn branches(func: &mut Func) {
    let mut uses: Map<Value, u32> = Map::default();
    for block in func.blocks() {
        for inst in func.insts(block) {
            let read = func[func[inst].args].iter().copied();
            let carried = func.successors(inst).flat_map(|call| func[call.args].to_vec());
            for value in read.chain(carried) {
                *uses.entry(value).or_default() += 1;
            }
        }
    }
    let mut work: Vec<Block> = func.blocks().collect();
    while let Some(head) = work.pop() {
        if let Some(rest) = split(func, head, &uses) {
            work.push(head);
            work.push(rest);
        }
    }
}

/// The instruction that defines `value` in `block`, when that is all `value` is read by `uses`
/// times over and moving it changes nothing but where it runs.
fn movable(func: &Func, block: Block, value: Value, uses: &Map<Value, u32>) -> Option<Inst> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let opcode = func[inst].opcode;
    let alone = uses.get(&value) == Some(&1) && func[inst].results == 1;
    let here = func.block_of(inst) == Some(block);
    (alone && here && !opcode.has_effects() && opcode != Opcode::IConst).then_some(inst)
}

/// Splits the branch ending `head`, answering the block its second half went in.
fn split(func: &mut Func, head: Block, uses: &Map<Value, u32>) -> Option<Block> {
    let term = func.terminator(head)?;
    if func[term].opcode != Opcode::BrIf {
        return None;
    }
    let cond = func[func[term].args][0];
    let joined = movable(func, head, cond, uses)?;
    let opcode = func[joined].opcode;
    let bits = matches!(opcode, Opcode::And | Opcode::Or) && func[cond].ty == Type::I1;
    if !bits || pair(func, joined) {
        return None;
    }
    let [left, right] = func[func[joined].args] else { return None };
    let constant = |value: Value| match func[value].def {
        Def::Result { inst, .. } => func[inst].opcode == Opcode::IConst,
        Def::Param { .. } => false,
    };
    if constant(left) || constant(right) {
        return None;
    }
    let arms: Vec<BlockCall> = func.successors(term).collect();
    let [yes, no] = arms[..] else { return None };
    let span = func.span(term);

    // What works out the right half and nothing else, in the order it was written.
    let mut tree = vec![];
    let mut pending = vec![right];
    while let Some(value) = pending.pop() {
        if let Some(inst) = movable(func, head, value, uses) {
            tree.push(inst);
            pending.extend(func[func[inst].args].iter().copied());
        }
    }
    let order: Vec<Inst> = func.insts(head).filter(|inst| tree.contains(inst)).collect();

    let rest = func.create_block();
    func.remove_inst(joined);
    for inst in order {
        func.remove_inst(inst);
        func.append_inst(rest, inst);
    }
    if let Some(inst) = movable(func, head, left, uses) {
        func.remove_inst(inst);
        func.insert_before(inst, term);
    }
    let yes_args = func[yes.args].to_vec();
    let no_args = func[no.args].to_vec();
    let second =
        Builder::new(func, rest).at(span).br_if(right, yes.block, &yes_args, no.block, &no_args);

    // The first branch is rewritten where it stands and the second is built in `rest`. A hint is
    // shared out between the two as the module documentation says.
    let (first, then) = shared(opcode, yes.hint);
    let into = BlockCall::to(rest);
    let (taken, other) = if opcode == Opcode::And { (into, no) } else { (yes, into) };
    let edges =
        [BlockCall { hint: first, ..taken }, BlockCall { hint: first.complement(), ..other }];
    for (at, edge) in func.target_list(term).iter().zip(edges) {
        func.set_block_call(at, edge);
    }
    func.rewrite(func[term].args, |_| left);
    for (at, hint) in func.target_list(second).iter().zip([then, then.complement()]) {
        let call = func[at];
        func.set_block_call(at, BlockCall { hint, ..call });
    }
    Some(rest)
}

/// Whether `inst` is the `(p ? q) | (p == q & r ? s)` that [`crate::wide`] writes for an ordered
/// comparison of two pairs, which [`crate::borrow`] turns into one subtract with a borrow and is
/// better than either branch gcc would write.
fn pair(func: &Func, inst: Inst) -> bool {
    let def = |value: Value| match func[value].def {
        Def::Result { inst, .. } => Some(inst),
        Def::Param { .. } => None,
    };
    let compare = |value: Value| {
        let inst = def(value).filter(|&inst| func[inst].opcode == Opcode::ICmp)?;
        let Extra::IntPred(pred) = func[inst].extra else { return None };
        let [x, y] = func[func[inst].args] else { return None };
        Some((pred, x, y))
    };
    let [left, right] = func[func[inst].args] else { return false };
    func[inst].opcode == Opcode::Or
        && [(left, right), (right, left)].into_iter().any(|(ordered, both)| {
            let Some((_, p, q)) = compare(ordered) else { return false };
            let Some(and) = def(both).filter(|&and| func[and].opcode == Opcode::And) else {
                return false;
            };
            let same = |x: Value, y: Value| x == p && y == q || x == q && y == p;
            func[func[and].args]
                .iter()
                .any(|&half| matches!(compare(half), Some((IntPred::Eq, x, y)) if same(x, y)))
        })
}

/// The hints the first and the second branch take for the yes arm, given the one the whole branch
/// had, shared out as `do_jump_1` does for `&&` and `||`.
fn shared(opcode: Opcode, yes: Hint) -> (Hint, Hint) {
    let Some(taken) = yes.taken() else { return (Hint::NONE, Hint::NONE) };
    let scale = Hint::SCALE;
    // Half of what decides the whole branch goes to the first, and the second takes the other half
    // of the whole, which is more of what is left of it once the first has been passed.
    let half = |part: u32| {
        let first = part / 2;
        (first, part / 2 * scale / (scale - first).max(1))
    };
    if opcode == Opcode::And {
        let (first, second) = half(scale - taken);
        (Hint::parts(scale - first), Hint::parts(scale - second))
    } else {
        let (first, second) = half(taken);
        (Hint::parts(first), Hint::parts(second))
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Flags, Func, Hint, IntPred, Module, Opcode, Signature, Type};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    /// `void f(int a, int b) { if (a == 1 && b == 2) g(); }` once the branch is on an and, built
    /// with the comparison for `b` working out a sum first and a hint that the call is rare.
    fn and_of_two(joined: Opcode) -> (Interner, Func) {
        let mut names = Interner::new();
        let i32 = Type::int(32);
        let signature = Signature::new().with_params(&[i32, i32]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let yes = func.create_block();
        let done = func.create_block();
        let a = func.append_param(entry, i32);
        let b = func.append_param(entry, i32);
        let mut build = Builder::new(&mut func, entry);
        let one = build.iconst(i32, 1);
        let left = build.icmp(IntPred::Eq, a, one);
        let sum = build.binary(Opcode::Add, b, one, Flags::NONE);
        let three = build.iconst(i32, 3);
        let right = build.icmp(IntPred::Eq, sum, three);
        let both = build.binary(joined, left, right, Flags::NONE);
        let branch = build.br_if(both, yes, &[], done, &[]);
        Builder::new(&mut func, yes).jump(done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        let arms: Vec<_> = func.target_list(branch).iter().collect();
        for (at, parts) in arms.into_iter().zip([1_000, 9_000]) {
            let call = func[at];
            func.set_block_call(at, rucc_ir::BlockCall { hint: Hint::parts(parts), ..call });
        }
        (names, func)
    }

    /// Whether what came out is a function the verifier takes.
    fn check(names: &mut Interner, func: &Func) {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let module = Module::new(names.intern("f.c"), &target);
        rucc_ir::verify_func(&module, func, names).expect("the rewrite builds valid IR");
    }

    /// What a block holds, in order.
    fn opcodes(func: &Func, block: rucc_ir::Block) -> Vec<Opcode> {
        func.insts(block).map(|inst| func[inst].opcode).collect()
    }

    #[test]
    fn a_branch_on_an_and_becomes_two_branches_each_after_its_own_comparison() {
        let (mut names, mut func) = and_of_two(Opcode::And);
        super::branches(&mut func);
        let blocks: Vec<_> = func.blocks().collect();
        assert_eq!(blocks.len(), 4);
        let head = [Opcode::IConst, Opcode::IConst, Opcode::ICmp, Opcode::BrIf];
        assert_eq!(opcodes(&func, blocks[0]), head);
        // The sum is only wanted by the second comparison, so it went down with it.
        assert_eq!(opcodes(&func, blocks[3]), [Opcode::Add, Opcode::ICmp, Opcode::BrIf]);
        let first = func.terminator(blocks[0]).expect("a branch");
        let arms: Vec<_> = func.successors(first).collect();
        assert_eq!((arms[0].block, arms[1].block), (blocks[3], blocks[2]));
        // Half of the nine in ten that the whole is false goes to each branch.
        assert_eq!(arms[1].hint.taken(), Some(4_500));
        let second = func.terminator(blocks[3]).expect("a branch");
        let arms: Vec<_> = func.successors(second).collect();
        assert_eq!((arms[0].block, arms[1].block), (blocks[1], blocks[2]));
        assert_eq!(arms[1].hint.taken(), Some(8_181));
        check(&mut names, &func);
    }

    #[test]
    fn a_branch_on_an_or_goes_to_the_second_test_when_the_first_fails() {
        let (mut names, mut func) = and_of_two(Opcode::Or);
        super::branches(&mut func);
        let blocks: Vec<_> = func.blocks().collect();
        let first = func.terminator(blocks[0]).expect("a branch");
        let arms: Vec<_> = func.successors(first).collect();
        assert_eq!((arms[0].block, arms[1].block), (blocks[1], blocks[3]));
        assert_eq!(arms[0].hint.taken(), Some(500));
        assert!(!opcodes(&func, blocks[0]).contains(&Opcode::Or));
        check(&mut names, &func);
    }
}
