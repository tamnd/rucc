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
//! Two kinds. The rules of `rules/simplify.rules`, which are matched against every instruction and
//! are where anything new goes, and one rewrite written out by hand below them.
//!
//! ## The rules
//!
//! Tier one of `spec/optimizer/13-rewrite-rules.md` section 13.4: the identities. Adding nothing,
//! multiplying by one, and'ing a value with itself. None of them needs anything known about the
//! operands, each leaves a term strictly smaller than the one it replaced, and every one of them
//! has been proved against `crates/rucc-ir/rules/ir.model` by `rucc-verify` before it may be used.
//!
//! What a rule leaves behind is one of two things. `(value.iN x)` means the result is a value the
//! function already has, so every use of the result is pointed at that value and the instruction
//! is left for [`crate::dce`]. `(iconst.iN k)` means the result is a constant, and the instruction
//! becomes that constant where it stands, which keeps the result value and is why nothing else has
//! to be rewritten for that half.
//!
//! ## The one written by hand
//!
//! An exclusive or of a comparison with an `i1` of all ones is that comparison with the opposite
//! predicate. That is issue 379, and it is worth more than the instruction it saves.
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
//!
//! An identity that produces a value leaves the same kind of litter for the same reason. The
//! instruction it fired on reads what it always read and nothing reads it, so it is dead, and
//! taking it out here would mean deciding whether its operands are still read by anything, which
//! is the question the dead code eliminator answers for the whole function at once.

use std::collections::HashMap;

use rucc_ir::term::{PLAIN, Plan, Shown, Term, Terms};
use rucc_ir::{Block, Def, Extra, Flags, Func, Imm, Inst, Opcode, Type, Value};

use crate::rules::Piece;
use crate::rules::identities::TABLE;
use crate::uses::count;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// Recorded once for each negation folded into the comparison under it.
const FLIPPED: &str = "comparison negated by an exclusive or rewritten as the opposite comparison";

/// Recorded for a negation that would have folded if there had been fuel for it.
const NO_FUEL: &str = "negated comparison left alone, the pass ran out of fuel";

/// Recorded for an identity that would have fired if there had been fuel for it.
const NO_FUEL_RULE: &str = "identity left alone, the pass ran out of fuel";

/// How each operand of an instruction is shown to the matcher, and in what order the ways are
/// tried.
///
/// The two with a constant come first, because a rule about a number is the more specific one and
/// an operand that is not a constant declines it at the first node of the trie. Nothing here
/// expands an operand into the instruction that computed it, since no tier one identity is about
/// two instructions at once.
const PLANS: [Plan; 3] =
    [[Shown::Reg, Shown::Const, Shown::Reg], [Shown::Const, Shown::Reg, Shown::Reg], PLAIN];

/// The pass. It holds nothing, because a peephole needs to know nothing beyond the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Simplify;

impl Pass for Simplify {
    fn name(&self) -> &'static str {
        "simplify"
    }

    fn describe(&self) -> &'static str {
        "the identities, and a negated comparison as the opposite comparison"
    }

    fn preserves(&self) -> Preserved {
        // Everything about the shape of the function. No block is added, none is removed and no
        // edge moves, so the graph and everything built out of it stand.
        //
        // The liveness does not, and that is the whole of the difference. An identity that
        // produces a value points every reader of one value at another, which is one more place
        // the second is live and one fewer the first is, and the same is true of the negation
        // below, which reads the comparison's operands where it used to read its result.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        // What a rule that produced a value decided, applied to the whole function at the end.
        // Rewriting each one where it is found would be a walk over every instruction for every
        // rewrite, and there is nothing to be gained by it: what a pattern asks about is the
        // instruction and its operands, and neither changes under a redirection.
        let mut forward: HashMap<Value, Value> = HashMap::new();
        // Who reads what, so that an instruction nothing reads is left alone. A rule that fires
        // on one changes no program, because what it does is point the readers somewhere else and
        // there are none, and it would still spend fuel and still report having optimized
        // something. That matters here more than it would in a pass that runs once: this pass is
        // named twice in every pipeline above `-O0`, an identity it takes stays in the function
        // until dead code elimination removes it, and without this the second run would rewrite
        // everything the first run did all over again and say so.
        //
        // Stale by design. It is what the function looked like when this run started, and a
        // rewrite below only ever removes readers, so a value this says nothing reads is a value
        // nothing reads.
        let uses = count(func);
        let dead = |func: &Func, inst: Inst| match func[inst].first_result {
            Some(result) => uses[result.index()] == 0,
            None => false,
        };
        for block in func.blocks().collect::<Vec<Block>>() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                if dead(func, inst) {
                    continue;
                }
                if let Some(flip) = negated_comparison(func, inst) {
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
                    continue;
                }
                let Some((rewrite, pattern)) = identity(func, inst) else { continue };
                if !fuel.take() {
                    stats.missed(NO_FUEL_RULE);
                    continue;
                }
                match rewrite {
                    Rewrite::Value(value) => {
                        let result = func[inst].first_result.expect("the rule matched a result");
                        forward.insert(result, value);
                    }
                    Rewrite::Constant(number) => become_constant(func, inst, number),
                }
                stats.optimized(pattern);
            }
        }
        if !forward.is_empty() {
            substitute(func, &forward);
        }
        stats
    }
}

/// What a rule says an instruction's result is instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rewrite {
    /// A value the function already has, which every reader of the result is pointed at.
    Value(Value),
    /// A number, which the instruction becomes where it stands.
    Constant(i128),
}

/// The identity that fires on this instruction, and the pattern of the rule it came from.
///
/// The plans are tried in order and the first that matches wins. A plan is how the operands are
/// shown rather than what they are, so trying three of them is three walks over a trie, each of
/// which fails in its first node or two when the instruction is not one any rule is about.
fn identity(func: &Func, inst: Inst) -> Option<(Rewrite, &'static str)> {
    let result = func[inst].first_result?;
    for plan in PLANS {
        let terms = Terms::new(func, inst, plan);
        let Some(found) = TABLE.find(&terms, Term::Root) else { continue };
        let rule = TABLE.rule(&found);
        let rewrite = match rule.replacement {
            // A value the pattern bound, which is a register because that is the only thing a
            // `value.iN` binds.
            [Piece::App { head, arity: 1 }, Piece::Var { index, .. }]
                if head.starts_with("value.") =>
            {
                match found.bindings.get(*index) {
                    Some(&Term::Reg(value)) => Rewrite::Value(value),
                    _ => continue,
                }
            }
            // A constant written in the rule. Only at a width the instruction's result has, which
            // it always does: an `iconst.iN` names an integer width and a rule is proved at the
            // width it is written at.
            [Piece::App { head, arity: 1 }, Piece::Int(number)]
                if head.starts_with("iconst.") && func[result].ty.is_int() =>
            {
                Rewrite::Constant(*number)
            }
            // Any other shape, which no rule in the file has. A test below says so, because a
            // rule that fell through here would be a rule that never fires and nothing would
            // say it had stopped.
            _ => continue,
        };
        return Some((rewrite, rule.pattern));
    }
    None
}

/// Turns an instruction into the constant a rule says its result is.
///
/// In place, so the result value survives and every reader of it is already right. That is what
/// makes this the half of the pass with nothing to redirect.
fn become_constant(func: &mut Func, inst: Inst, number: i128) {
    let result = func[inst].first_result.expect("the rule matched a result");
    let ty = func[result].ty;
    let imm = func.add_imm(Imm::int(number, ty.lane()));
    let args = func.push_values(&[]);
    let data = &mut func[inst];
    data.opcode = Opcode::IConst;
    data.args = args;
    data.extra = Extra::Imm(imm);
    // The flags go with the instruction that had them. An `nsw` on an add is a promise about an
    // addition, and a constant makes no promise because it performs nothing.
    data.flags = Flags::NONE;
}

/// Where a redirection ends up, following the ones the rest of this run decided.
///
/// A chain forms when one identity feeds another, `x + 0` read by `y * 1`, and following it is
/// what makes the second rewrite worth as much as the first. A rule points a result at one of its
/// own operands and an operand is defined before the instruction that reads it, so every step
/// goes further back and the chain cannot come round to where it started.
fn chase(forward: &HashMap<Value, Value>, value: Value) -> Value {
    let mut value = value;
    while let Some(&next) = forward.get(&value) {
        value = next;
    }
    value
}

/// Points every reader of a rewritten result at what the rule said it is.
///
/// The arguments of each instruction and the arguments of the blocks it branches to, which is the
/// whole of what an instruction can read and is the same pair [`crate::uses::operands`] walks.
fn substitute(func: &mut Func, forward: &HashMap<Value, Value>) {
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
        Block, Builder, Extra, Flags, Float, FloatPred, Func, IntPred, Module, Opcode, Signature,
        Type, Value,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{PLANS, TABLE};
    use crate::rules::Piece;
    use crate::stats::Kind;
    use crate::{Analyses, Fuel, Pass, simplify::Simplify};

    /// A function with one block, ready to have instructions appended to it.
    fn blank() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(1)]));
        let block = func.create_block();
        (names, func, block)
    }

    /// The same, at the width the test is about and taking a parameter of it, since every identity
    /// below needs an operand that is not itself a constant.
    fn one_block(ty: Type) -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let signature = Signature::new().with_params(&[ty]).with_returns(&[ty]);
        let mut func = Func::new(name, signature);
        let block = func.create_block();
        (names, func, block)
    }

    /// Runs the pass with as much fuel as it wants, and says whether it rewrote anything.
    fn simplify(func: &mut Func) -> bool {
        Simplify.run(func, &mut Analyses::new(), &mut Fuel::unlimited()).changed()
    }

    /// The opcode and the predicate the value now comes from.
    fn came_from(func: &Func, value: Value) -> (Opcode, Extra) {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { panic!("not a result") };
        (func[inst].opcode, func[inst].extra)
    }

    /// What the block gives back, which is where every identity test reads its answer. A rule
    /// that produces a value is only worth anything if the readers move, so the readers are what
    /// the test looks at rather than the instruction that fired.
    fn returned(func: &Func, block: Block) -> Value {
        let inst = func.terminator(block).expect("the block has a terminator");
        func[func[inst].args][0]
    }

    /// The number a value is, which panics unless it is a constant.
    fn number(func: &Func, value: Value) -> i128 {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { panic!("not a result") };
        let data = &func[inst];
        assert_eq!(data.opcode, Opcode::IConst, "not a constant");
        let Extra::Imm(at) = data.extra else { panic!("a constant with no number") };
        func[at].signed(func[value].ty)
    }

    /// Every rule in the table leaves one of the two shapes the pass knows how to apply.
    ///
    /// A rule that left anything else would be matched, found to be neither, and skipped, and
    /// nothing at run time would say so: the rewrite would simply stop happening. So it is said
    /// here instead, once, over the whole table.
    #[test]
    fn every_rule_leaves_a_shape_the_pass_knows_what_to_do_with() {
        for rule in TABLE.rules {
            let known = matches!(
                rule.replacement,
                [Piece::App { head, arity: 1 }, Piece::Var { .. }] if head.starts_with("value.")
            ) || matches!(
                rule.replacement,
                [Piece::App { head, arity: 1 }, Piece::Int(_)] if head.starts_with("iconst.")
            );
            assert!(known, "{} leaves a shape the pass would skip", rule.pattern);
        }
    }

    /// And the table holds every rule the file writes. The table is generated, so this is asking
    /// whether the generator saw the whole file, which is the one thing about it worth doubting.
    #[test]
    fn the_table_holds_every_rule_the_file_writes() {
        let text = include_str!("../rules/simplify.rules");
        assert_eq!(TABLE.rules.len(), text.matches("(rule (simplify ").count());
        assert!(TABLE.rules.len() > 100, "tier one is about a hundred rules and there are fewer");
    }

    /// Three ways of showing an operand and no more, since a fourth would be a plan nothing
    /// tries and a rule written for it would never fire.
    #[test]
    fn a_pattern_is_reached_by_one_of_the_plans() {
        assert_eq!(PLANS.len(), 3);
    }

    #[test]
    fn adding_nothing_points_every_reader_at_the_operand() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let sum = build.binary(Opcode::Add, x, zero, Flags::NONE);
        build.ret(&[sum]);
        assert!(simplify(&mut func));
        // The `add` is still there, used by nothing, which is what dead code elimination is for.
        assert_eq!(returned(&func, block), x);
        assert_eq!(came_from(&func, sum).0, Opcode::Add);
    }

    /// The constant on either side, since nothing puts it on the right yet and a rule written one
    /// way round would fire on half the additions it should.
    #[test]
    fn the_constant_is_found_on_either_side_of_an_identity() {
        for swapped in [false, true] {
            let i32 = Type::int(32);
            let (_, mut func, block) = one_block(i32);
            let x = func.append_param(block, i32);
            let mut build = Builder::new(&mut func, block);
            let zero = build.iconst(i32, 0);
            let (lhs, rhs) = if swapped { (zero, x) } else { (x, zero) };
            let sum = build.binary(Opcode::Add, lhs, rhs, Flags::NONE);
            build.ret(&[sum]);
            assert!(simplify(&mut func), "swapped {swapped}");
            assert_eq!(returned(&func, block), x, "swapped {swapped}");
        }
    }

    #[test]
    fn multiplying_by_nothing_becomes_the_constant_where_it_stands() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let product = build.binary(Opcode::Mul, x, zero, Flags::NONE);
        build.ret(&[product]);
        assert!(simplify(&mut func));
        // The result value survives, which is the whole reason this half rewrites in place.
        assert_eq!(returned(&func, block), product);
        assert_eq!(came_from(&func, product).0, Opcode::IConst);
        assert_eq!(number(&func, product), 0);
    }

    /// The two identities a pattern that writes one name twice exists for, at every width they
    /// are written at.
    #[test]
    fn a_value_against_itself() {
        for bits in [8, 16, 32, 64] {
            let ty = Type::int(bits);
            let (_, mut func, block) = one_block(ty);
            let x = func.append_param(block, ty);
            let mut build = Builder::new(&mut func, block);
            let both = build.binary(Opcode::And, x, x, Flags::NONE);
            build.ret(&[both]);
            assert!(simplify(&mut func), "{bits} bits");
            assert_eq!(returned(&func, block), x, "{bits} bits");

            let (_, mut func, block) = one_block(ty);
            let x = func.append_param(block, ty);
            let mut build = Builder::new(&mut func, block);
            let nothing = build.binary(Opcode::Sub, x, x, Flags::NONE);
            build.ret(&[nothing]);
            assert!(simplify(&mut func), "{bits} bits");
            assert_eq!(number(&func, nothing), 0, "{bits} bits");
        }
    }

    /// A remainder by one is nothing, and a division by one is the value. The pair is worth a
    /// test of its own because they are the two identities that produce different shapes from the
    /// same operands.
    #[test]
    fn dividing_by_one_and_the_remainder_that_goes_with_it() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let one = build.iconst(i32, 1);
        let quotient = build.binary(Opcode::SDiv, x, one, Flags::NONE);
        let rest = build.binary(Opcode::SRem, x, one, Flags::NONE);
        let sum = build.binary(Opcode::Add, quotient, rest, Flags::NONE);
        build.ret(&[sum]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, rest), 0);
        // The add reads the value the division was of, which is what the redirection did.
        let rucc_ir::Def::Result { inst, .. } = func[sum].def else { panic!("not a result") };
        assert_eq!(func[func[inst].args][0], x);
    }

    /// All ones at one bit is the `1` the rule file writes, and the front end writes it as `-1`.
    /// The two are the same bit and the rule has to fire on what the front end wrote.
    #[test]
    fn all_ones_at_one_bit_is_the_one_the_front_end_writes() {
        for written in [-1, 1] {
            let bit = Type::int(1);
            let (_, mut func, block) = one_block(bit);
            let x = func.append_param(block, bit);
            let mut build = Builder::new(&mut func, block);
            let ones = build.iconst(bit, written);
            let kept = build.binary(Opcode::And, x, ones, Flags::NONE);
            build.ret(&[kept]);
            assert!(simplify(&mut func), "written as {written}");
            assert_eq!(returned(&func, block), x, "written as {written}");
        }
    }

    /// One identity feeding another is followed all the way, so the second is worth as much as
    /// the first. The redirections are applied once at the end of the run, and this is what says
    /// that costs nothing.
    #[test]
    fn one_identity_feeding_another_is_followed_to_the_end() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let one = build.iconst(i32, 1);
        let sum = build.binary(Opcode::Add, x, zero, Flags::NONE);
        let product = build.binary(Opcode::Mul, sum, one, Flags::NONE);
        let shifted = build.binary(Opcode::Shl, product, zero, Flags::NONE);
        build.ret(&[shifted]);
        assert!(simplify(&mut func));
        assert_eq!(returned(&func, block), x);
    }

    #[test]
    fn an_instruction_no_rule_is_about_is_left_alone() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let two = build.iconst(i32, 2);
        let doubled = build.binary(Opcode::Mul, x, two, Flags::NONE);
        build.ret(&[doubled]);
        assert!(!simplify(&mut func), "a multiply by two is tier two and there is no rule for it");
        assert_eq!(returned(&func, block), doubled);
        assert_eq!(came_from(&func, doubled).0, Opcode::Mul);
    }

    /// The function the pass leaves is still one the verifier accepts. Pointing a reader at a
    /// different value and turning an instruction into a constant are both things a rewrite could
    /// get wrong in a way none of the tests above would notice, because each of those asks about
    /// one instruction and this asks about the function.
    #[test]
    fn the_pass_leaves_the_verifier_nothing_to_complain_about() {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let i32 = Type::int(32);
        let (mut names, mut func, block) = one_block(i32);
        let mut module = Module::new(names.intern("test.c"), &target);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let one = build.iconst(i32, 1);
        let sum = build.binary(Opcode::Add, x, zero, Flags::NONE);
        let product = build.binary(Opcode::Mul, sum, one, Flags::NONE);
        let gone = build.binary(Opcode::Sub, product, product, Flags::NONE);
        let total = build.binary(Opcode::Add, product, gone, Flags::NONE);
        build.ret(&[total]);
        assert!(simplify(&mut func));
        module.add_func(func);
        rucc_ir::verify(&module, &names).expect("the pass left the function verifiable");
    }

    #[test]
    fn fuel_stops_an_identity_and_not_the_walk() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let first = build.binary(Opcode::Add, x, zero, Flags::NONE);
        let second = build.binary(Opcode::Sub, x, zero, Flags::NONE);
        let sum = build.binary(Opcode::Add, first, second, Flags::NONE);
        build.ret(&[sum]);
        let stats = Simplify.run(&mut func, &mut Analyses::new(), &mut Fuel::of(1));
        assert!(stats.changed());
        assert_eq!(stats.total(Kind::Optimized), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_RULE), 1);
        // The first fired and the second did not, and the second is still read by the add.
        let rucc_ir::Def::Result { inst, .. } = func[sum].def else { panic!("not a result") };
        assert_eq!(func[func[inst].args], [x, second]);
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
        let stats = Simplify.run(&mut func, &mut Analyses::new(), &mut Fuel::of(1));
        assert!(stats.changed());
        assert_eq!(stats.count(Kind::Optimized, super::FLIPPED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert_eq!(came_from(&func, first).0, Opcode::ICmp);
        assert_eq!(came_from(&func, second).0, Opcode::Xor);
    }
}
