//! Peephole rewrites: a small pattern of instructions becomes a smaller one.
//!
//! The third pass, and the one that will eventually not exist. Section 9.3 of
//! `spec/09-optimizer.md` says the value level optimizer is an acyclic e-graph, and that an
//! e-graph replaces what would otherwise be a folding pass, a peephole pass, a GVN pass, a
//! reassociation pass and an instcombine pass, all with a pass ordering problem between them.
//! This is the peephole pass, written now because the e-graph is a milestone away and because
//! there is a rewrite that unblocks twelve lowering rules today.
//!
//! Every rewrite here has to survive being moved into the rule set later, so each one is stated
//! as a pattern and a replacement in its own function and nothing shares state with anything.
//!
//! # The rewrites
//!
//! One so far: an exclusive or of a comparison with an `i1` of all ones is that comparison with
//! the opposite predicate. That is issue 379, and it is worth more than the instruction it saves.
//!
//! C spells eight of the sixteen floating point predicates. The six relational and equality
//! operators give the six ordered ones, `!=` gives `une`, and `__builtin_isunordered` gives `uno`.
//! The other eight are what the negation of one of those means, and the front end writes a
//! negation as an exclusive or rather than as a flipped predicate, so `!(x < y)` lowers to an
//! `fcmp olt` and an `xor` where the machine has an `fcmp uge`. Twelve rules in the x86-64 rule
//! set are written on those predicates and none of them has ever fired, over the whole torture
//! suite at every optimization level, because no IR that reaches selection contains one.
//!
//! The integer case comes with it. `!(a < b)` on integers is the same shape, the same rewrite and
//! the same saving, and leaving it out because the coverage report did not complain about it would
//! be picking the rewrite by what measures it rather than by what it does.
//!
//! # Why it needs dead code elimination after it
//!
//! The rewrite turns the `xor` into the comparison and leaves the original comparison where it
//! was, used by nothing when the negation was its only reader. Rewriting in place keeps the
//! result value, so every use of it is already correct and there is nothing to rewrite, and what
//! is left over is exactly what [`crate::dce`] takes out. That is why the pipeline runs the two in
//! this order, and it is why the pass before the dead code eliminator was written first.

use rucc_ir::{Block, Def, Extra, Flags, Func, Inst, Opcode, Type, Value};

use crate::{Fuel, Pass, Stats};

/// Recorded once for each negation folded into the comparison under it.
const FLIPPED: &str = "comparison negated by an exclusive or rewritten as the opposite comparison";

/// Recorded for a negation that would have folded if there had been fuel for it.
const NO_FUEL: &str = "negated comparison left alone, the pass ran out of fuel";

/// The pass. It holds nothing, because a peephole needs to know nothing beyond the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Simplify;

impl Pass for Simplify {
    fn name(&self) -> &'static str {
        "simplify"
    }

    fn describe(&self) -> &'static str {
        "a negated comparison becomes the comparison with the opposite predicate"
    }

    fn run(&self, func: &mut Func, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        for block in func.blocks().collect::<Vec<Block>>() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                let Some(flip) = negated_comparison(func, inst) else { continue };
                if !fuel.take() {
                    // Out of fuel, which stops the transforming rather than the looking, the
                    // same way the other two passes treat it. The walk is the same walk at
                    // every fuel setting, which is what makes bisecting over it monotonic.
                    stats.missed(NO_FUEL);
                    continue;
                }
                let args = func.push_values(&[flip.lhs, flip.rhs]);
                let data = &mut func[inst];
                data.opcode = flip.opcode;
                data.flags = flip.flags;
                data.args = args;
                data.extra = flip.extra;
                stats.optimized(FLIPPED);
            }
        }
        stats
    }
}

/// What an instruction should become, when it is a comparison written as a negation.
struct Flip {
    /// `ICmp` or `FCmp`, whichever the comparison underneath was.
    opcode: Opcode,
    /// The flags of the comparison, which is where a fast math promise lives.
    flags: Flags,
    /// The opposite predicate.
    extra: Extra,
    /// The comparison's left operand.
    lhs: Value,
    /// Its right operand.
    rhs: Value,
}

/// Whether this instruction is `xor (cmp p a b), true`, and what it becomes if it is.
///
/// The exclusive or is commutative, so the constant is looked for on both sides. Nothing else
/// about the shape is negotiable: the result has to be an `i1`, because an exclusive or with one
/// is a negation only at that width, and the constant has to be all ones, because the front end
/// writes it as `iconst.i1 -1` and a reader who assumed the literal 1 would match nothing.
fn negated_comparison(func: &Func, inst: Inst) -> Option<Flip> {
    let data = &func[inst];
    if data.opcode != Opcode::Xor {
        return None;
    }
    let args = &func[data.args];
    let (&first, &second) = (args.first()?, args.get(1)?);
    if func[first].ty != Type::int(1) {
        return None;
    }
    let cmp = match (all_ones(func, first), all_ones(func, second)) {
        (true, false) => second,
        (false, true) => first,
        // Both, which folding would have turned into a constant, or neither, which is an
        // exclusive or of two comparisons and is not this pattern.
        _ => return None,
    };
    let Def::Result { inst: cmp, .. } = func[cmp].def else { return None };
    let data = &func[cmp];
    let extra = match (data.opcode, data.extra) {
        (Opcode::ICmp, Extra::IntPred(pred)) => Extra::IntPred(pred.inverse()),
        (Opcode::FCmp, Extra::FloatPred(pred)) => Extra::FloatPred(pred.inverse()),
        _ => return None,
    };
    let args = &func[data.args];
    Some(Flip {
        opcode: data.opcode,
        flags: data.flags,
        extra,
        lhs: *args.first()?,
        rhs: *args.get(1)?,
    })
}

/// Whether this value is a constant with every bit of its type set.
fn all_ones(func: &Func, value: Value) -> bool {
    let ty = func[value].ty;
    let Def::Result { inst, .. } = func[value].def else { return false };
    let data = &func[inst];
    let Extra::Imm(at) = data.extra else { return false };
    if data.opcode != Opcode::IConst {
        return false;
    }
    // Read as signed, because an all ones value of any width is minus one that way and reading
    // it unsigned would need the width to build the mask from.
    func[at].signed(ty) == -1
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Extra, Flags, Float, FloatPred, Func, IntPred, Opcode, Signature, Type,
    };

    use crate::stats::Kind;
    use crate::{Fuel, Pass, simplify::Simplify};

    /// A function with one block, ready to have instructions appended to it.
    fn blank() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(1)]));
        let block = func.create_block();
        (names, func, block)
    }

    /// Runs the pass with as much fuel as it wants, and says whether it rewrote anything.
    fn simplify(func: &mut Func) -> bool {
        Simplify.run(func, &mut Fuel::unlimited()).changed()
    }

    /// The opcode and the predicate the value now comes from.
    fn came_from(func: &Func, value: rucc_ir::Value) -> (Opcode, Extra) {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { panic!("not a result") };
        (func[inst].opcode, func[inst].extra)
    }

    #[test]
    fn a_negated_float_comparison_becomes_the_opposite_predicate() {
        // Every ordered predicate and its opposite, which is the table `!(x < y)` is `x >= y`
        // or unordered lives in, and the one place a sign error would hide.
        for pred in FloatPred::all() {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let x = build.iconst(Type::int(64), 0);
            let x = build.unary(Opcode::Bitcast, x, Type::float(Float::F64));
            let cmp = build.fcmp(pred, x, x, Flags::NONE);
            let ones = build.iconst(Type::int(1), -1);
            let not = build.binary(Opcode::Xor, cmp, ones, Flags::NONE);
            build.ret(&[not]);
            assert!(simplify(&mut func), "{pred:?}");
            assert_eq!(
                came_from(&func, not),
                (Opcode::FCmp, Extra::FloatPred(pred.inverse())),
                "{pred:?}"
            );
        }
    }

    #[test]
    fn a_negated_integer_comparison_becomes_the_opposite_predicate() {
        for pred in IntPred::all() {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let x = build.iconst(Type::int(32), 3);
            let cmp = build.icmp(pred, x, x);
            let ones = build.iconst(Type::int(1), -1);
            let not = build.binary(Opcode::Xor, cmp, ones, Flags::NONE);
            build.ret(&[not]);
            assert!(simplify(&mut func), "{pred:?}");
            assert_eq!(
                came_from(&func, not),
                (Opcode::ICmp, Extra::IntPred(pred.inverse())),
                "{pred:?}"
            );
        }
    }

    #[test]
    fn the_constant_is_found_on_either_side() {
        for swapped in [false, true] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let x = build.iconst(Type::int(32), 3);
            let cmp = build.icmp(IntPred::Slt, x, x);
            let ones = build.iconst(Type::int(1), -1);
            let (lhs, rhs) = if swapped { (ones, cmp) } else { (cmp, ones) };
            let not = build.binary(Opcode::Xor, lhs, rhs, Flags::NONE);
            build.ret(&[not]);
            assert!(simplify(&mut func), "swapped {swapped}");
            assert_eq!(came_from(&func, not).1, Extra::IntPred(IntPred::Sge));
        }
    }

    #[test]
    fn an_exclusive_or_of_two_comparisons_is_left_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 3);
        let a = build.icmp(IntPred::Slt, x, x);
        let b = build.icmp(IntPred::Sgt, x, x);
        let differ = build.binary(Opcode::Xor, a, b, Flags::NONE);
        build.ret(&[differ]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, differ).0, Opcode::Xor);
    }

    #[test]
    fn an_exclusive_or_of_something_that_is_not_a_comparison_is_left_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 3);
        let narrow = build.unary(Opcode::Trunc, x, Type::int(1));
        let ones = build.iconst(Type::int(1), -1);
        let not = build.binary(Opcode::Xor, narrow, ones, Flags::NONE);
        build.ret(&[not]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, not).0, Opcode::Xor);
    }

    #[test]
    fn a_wider_exclusive_or_with_one_is_not_a_negation_and_is_left_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 3);
        let cmp = build.icmp(IntPred::Slt, x, x);
        let wide = build.unary(Opcode::ZExt, cmp, Type::int(32));
        let one = build.iconst(Type::int(32), 1);
        let flipped = build.binary(Opcode::Xor, wide, one, Flags::NONE);
        let narrow = build.unary(Opcode::Trunc, flipped, Type::int(1));
        build.ret(&[narrow]);
        assert!(!simplify(&mut func), "an i32 xor 1 flips one bit of thirty two");
        assert_eq!(came_from(&func, flipped).0, Opcode::Xor);
    }

    #[test]
    fn the_comparisons_flags_travel_with_the_predicate() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(64), 0);
        let x = build.unary(Opcode::Bitcast, x, Type::float(Float::F64));
        let cmp = build.fcmp(FloatPred::Olt, x, x, Flags::FAST);
        let ones = build.iconst(Type::int(1), -1);
        let not = build.binary(Opcode::Xor, cmp, ones, Flags::NONE);
        build.ret(&[not]);
        assert!(simplify(&mut func));
        let rucc_ir::Def::Result { inst, .. } = func[not].def else { panic!("not a result") };
        // The promise the original comparison was made under, not the exclusive or's absence of
        // one. Dropping it would be correct and would quietly undo a fast math flag.
        assert_eq!(func[inst].flags, Flags::FAST);
    }

    #[test]
    fn fuel_stops_the_transformation_and_not_the_walk() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 3);
        let a = build.icmp(IntPred::Slt, x, x);
        let b = build.icmp(IntPred::Sgt, x, x);
        let ones = build.iconst(Type::int(1), -1);
        let first = build.binary(Opcode::Xor, a, ones, Flags::NONE);
        let second = build.binary(Opcode::Xor, b, ones, Flags::NONE);
        let both = build.binary(Opcode::And, first, second, Flags::NONE);
        build.ret(&[both]);
        let stats = Simplify.run(&mut func, &mut Fuel::of(1));
        assert!(stats.changed());
        assert_eq!(stats.count(Kind::Optimized, super::FLIPPED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert_eq!(came_from(&func, first).0, Opcode::ICmp);
        assert_eq!(came_from(&func, second).0, Opcode::Xor);
    }
}
