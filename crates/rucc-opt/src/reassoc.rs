//! Reassociation: an integer tree of one operation is flattened, simplified and rebuilt in rank
//! order.
//!
//! Section 19.3 of `spec/optimizer/19-reassociation-and-arithmetic.md`, which is steps one to four
//! of the five gcc's `tree-ssa-reassoc.cc` opens with. A sum written in source order keeps that
//! order through every other pass, because each of them looks at one operation at a time and an
//! add whose operands are an add and a constant is not something a local rule can improve unless
//! the other constant happens to be its direct operand. This pass looks at the whole tree.
//!
//! # What a tree is
//!
//! A root is an integer `add`, `sub`, `mul`, `and`, `or` or `xor`, and the tree under it is every
//! operand defined by the same operation, of the same type, in the same block, and used by nothing
//! else. `add` and `sub` are one operation here, with `a - b` read as `a + (-b)`, and the negation
//! exists only inside the pass. Everything else is a leaf, and that includes a value the tree uses
//! that something outside it uses too, since flattening a shared value into two trees would do its
//! work twice.
//!
//! # What happens to it
//!
//! The constants fold into one. In a sum, a term and its negation cancel and a term added `k` times
//! becomes one multiply by `k`. In an `xor` a term twice is no term, and in an `and` or an `or` a
//! term twice is the term once. A constant that absorbs everything, zero in a product or an `and`
//! and all ones in an `or`, is the answer.
//!
//! What is left is sorted by rank and rebuilt left associated with the constant last. The rank of
//! a value is how far down the function it is computed: a function parameter is near zero, a value
//! with effects or read from memory takes its block's place in reverse postorder, and anything else
//! takes the largest rank of its operands, capped at its block's. So the operands computed earliest
//! pair up first, and a group of loop invariant ones becomes one value `licm` can take out whole.
//!
//! The exception is a loop header's parameter, which ranks above everything. That is the
//! accumulator of section 19.2: in `x = x + a + p[i] + b` the recurrence goes through every add
//! when `x` is first and through one when it is last.
//!
//! # Flags
//!
//! A rebuilt tree has no `nsw` or `nuw` on any node, per section 19.7, because a different order
//! of the same additions can overflow where the original did not. A tree already in rank order
//! with nothing to combine is left as it was, flags included, so the pass costs a flag only where
//! it changed something.

use rucc_base::hash::{Map, Set};
use rucc_ir::{Block, Def, Extra, Func, Imm, Inst, InstData, Opcode, Type, Value};

use crate::cfg::Cfg;
use crate::uses::{count, substitute};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "reassoc";

/// Recorded for a tree rebuilt in rank order with nothing combined.
const SORTED: &str = "integer tree rebuilt in rank order";

/// Recorded for a tree where terms cancelled, repeated or folded into one constant.
const COMBINED: &str = "integer tree rebuilt with its terms combined";

/// Recorded for a tree that would have been rebuilt if there had been fuel for it.
const NO_FUEL: &str = "integer tree left as it was, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reassoc;

impl Pass for Reassoc {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "an integer tree of one operation is rebuilt in rank order with its terms combined"
    }

    fn preserves(&self) -> Preserved {
        // Instructions come and go inside blocks and no edge moves.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(entry) = func.entry() else { return stats };
        let rank = ranks(func, an.cfg(func), entry);
        let uses = count(func);
        let mut inside: Set<Inst> = Set::default();
        let mut forward: Map<Value, Value> = Map::default();
        let mut gone: Vec<Inst> = Vec::new();
        for block in func.blocks().collect::<Vec<Block>>() {
            // Bottom up, so that the first instruction of a tree met is its root, and the ones
            // under it are marked before the walk reaches them.
            for inst in func.insts_backwards(block).collect::<Vec<Inst>>() {
                if inside.contains(&inst) {
                    continue;
                }
                let Some(tree) = Tree::of(func, &uses, block, inst) else { continue };
                inside.extend(tree.interior.iter().copied());
                let Some(plan) = tree.plan(func, &rank) else { continue };
                if !fuel.take() {
                    stats.missed(NO_FUEL);
                    continue;
                }
                let root = func[inst].first_result.expect("an arithmetic instruction has a result");
                let value = plan.build(func, inst, tree.op, tree.ty);
                forward.insert(root, value);
                gone.push(inst);
                gone.extend(tree.interior);
                stats.optimized(if plan.combined { COMBINED } else { SORTED });
            }
        }
        if forward.is_empty() {
            return stats;
        }
        substitute(func, &forward);
        for inst in gone {
            func.remove_inst(inst);
        }
        stats
    }
}

/// The operation a tree is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// `add` and `sub`, with a subtraction read as adding the negation.
    Sum,
    Product,
    And,
    Or,
    Xor,
}

impl Op {
    fn of(opcode: Opcode) -> Option<Self> {
        match opcode {
            Opcode::Add | Opcode::Sub => Some(Self::Sum),
            Opcode::Mul => Some(Self::Product),
            Opcode::And => Some(Self::And),
            Opcode::Or => Some(Self::Or),
            Opcode::Xor => Some(Self::Xor),
            _ => None,
        }
    }

    /// The constant that changes nothing.
    const fn identity(self) -> i128 {
        match self {
            Self::Sum | Self::Or | Self::Xor => 0,
            Self::Product => 1,
            Self::And => -1,
        }
    }

    /// The constant that makes the answer itself whatever else is in the tree.
    const fn absorbing(self) -> Option<i128> {
        match self {
            Self::Product | Self::And => Some(0),
            Self::Or => Some(-1),
            Self::Sum | Self::Xor => None,
        }
    }

    fn fold(self, a: i128, b: i128) -> i128 {
        match self {
            Self::Sum => a.wrapping_add(b),
            Self::Product => a.wrapping_mul(b),
            Self::And => a & b,
            Self::Or => a | b,
            Self::Xor => a ^ b,
        }
    }

    const fn opcode(self) -> Opcode {
        match self {
            Self::Sum => Opcode::Add,
            Self::Product => Opcode::Mul,
            Self::And => Opcode::And,
            Self::Or => Opcode::Or,
            Self::Xor => Opcode::Xor,
        }
    }
}

/// A root and what hangs under it.
struct Tree {
    op: Op,
    ty: Type,
    /// Every leaf in the order the tree has them, left to right, and whether it is subtracted.
    leaves: Vec<(Value, bool)>,
    /// The instructions under the root that go when it does.
    interior: Vec<Inst>,
}

impl Tree {
    /// The tree whose root this is, when it is a root with something under it.
    fn of(func: &Func, uses: &[u32], block: Block, root: Inst) -> Option<Self> {
        let op = Op::of(func[root].opcode)?;
        let result = func[root].first_result?;
        let ty = func[result].ty;
        if !ty.is_int() || ty.lanes() != 1 {
            return None;
        }
        let mut leaves = Vec::new();
        let mut interior = Vec::new();
        // Right before left on the stack, so that leaves come off in the order they are written.
        let mut stack = vec![(result, false)];
        while let Some((value, negated)) = stack.pop() {
            let under = match func[value].def {
                Def::Result { inst, .. } if value == result => Some(inst),
                Def::Result { inst, .. }
                    if uses[value.index()] == 1
                        && func.block_of(inst) == Some(block)
                        && Op::of(func[inst].opcode) == Some(op)
                        && func[value].ty == ty =>
                {
                    Some(inst)
                }
                _ => None,
            };
            let Some(inst) = under else {
                leaves.push((value, negated));
                continue;
            };
            if inst != root {
                interior.push(inst);
            }
            let args = &func[func[inst].args];
            let flip = func[inst].opcode == Opcode::Sub;
            stack.push((args[1], negated != flip));
            stack.push((args[0], negated));
        }
        (!interior.is_empty()).then_some(Self { op, ty, leaves, interior })
    }

    /// What the tree becomes, or nothing when it is already that.
    fn plan(&self, func: &Func, rank: &Map<Value, u64>) -> Option<Plan> {
        let mut constant = self.op.identity();
        let mut constants = 0;
        let mut first: Vec<Value> = Vec::new();
        let mut times: Map<Value, i128> = Map::default();
        for &(value, negated) in &self.leaves {
            if let Some(number) = integer(func, value) {
                let number = if negated { number.wrapping_neg() } else { number };
                constant = self.op.fold(constant, number);
                constants += 1;
                continue;
            }
            let count = times.entry(value).or_insert(0);
            if *count == 0 {
                first.push(value);
            }
            *count += if negated { -1 } else { 1 };
        }
        let constant = Imm::int(constant, self.ty).signed(self.ty);
        let mut terms: Vec<(Value, i128)> = Vec::new();
        for &value in &first {
            let count = times[&value];
            match self.op {
                Op::Sum if count != 0 => terms.push((value, count)),
                Op::Product => terms.extend((0..count).map(|_| (value, 1))),
                Op::And | Op::Or => terms.push((value, 1)),
                Op::Xor if count % 2 != 0 => terms.push((value, 1)),
                _ => {}
            }
        }
        let original: Vec<Value> = self
            .leaves
            .iter()
            .map(|&(value, _)| value)
            .filter(|&v| integer(func, v).is_none())
            .collect();
        let absorbed = self.op.absorbing() == Some(constant);
        if absorbed {
            terms.clear();
        }
        let whole = terms.iter().map(|&(_, count)| count.unsigned_abs()).sum::<u128>();
        let combined = absorbed
            || constants > 1
            || (constants == 1 && constant == self.op.identity())
            || whole != original.len() as u128
            || terms.iter().any(|&(_, count)| count.unsigned_abs() > 1);
        terms.sort_by_key(|&(value, _)| (rank.get(&value).copied().unwrap_or(0), value.index()));
        let sorted: Vec<Value> = terms
            .iter()
            .flat_map(|&(value, count)| (0..count.unsigned_abs()).map(move |_| value))
            .collect();
        if !combined && sorted == original {
            return None;
        }
        Some(Plan { terms, constant, combined })
    }
}

/// A tree as it will be rebuilt.
struct Plan {
    /// Each term in rank order, with how many times it is added, or one for the other operations.
    terms: Vec<(Value, i128)>,
    /// The constants folded into one, which is the identity when there were none.
    constant: i128,
    /// Whether anything cancelled or folded, which is what the report says.
    combined: bool,
}

impl Plan {
    /// Writes the tree in front of its old root and answers the value it comes to.
    fn build(&self, func: &mut Func, before: Inst, op: Op, ty: Type) -> Value {
        let mut acc: Option<Value> = None;
        for &(value, count) in &self.terms {
            let magnitude = count.unsigned_abs();
            let term = if op == Op::Sum && magnitude > 1 {
                let times = constant(func, before, ty, magnitude as i128);
                binary(func, before, Opcode::Mul, value, times, ty)
            } else {
                value
            };
            acc = Some(match acc {
                None if count < 0 => {
                    let zero = constant(func, before, ty, 0);
                    binary(func, before, Opcode::Sub, zero, term, ty)
                }
                None => term,
                Some(had) if count < 0 => binary(func, before, Opcode::Sub, had, term, ty),
                Some(had) => binary(func, before, op.opcode(), had, term, ty),
            });
        }
        let identity = op.identity();
        match acc {
            Some(had) if self.constant == identity => had,
            Some(had) => {
                let number = constant(func, before, ty, self.constant);
                binary(func, before, op.opcode(), had, number, ty)
            }
            None => constant(func, before, ty, self.constant),
        }
    }
}

/// The value of an integer constant, read with its own sign.
fn integer(func: &Func, value: Value) -> Option<i128> {
    crate::discharge::constant(func, value)
}

fn constant(func: &mut Func, before: Inst, ty: Type, value: i128) -> Value {
    let at = func.add_imm(Imm::int(value, ty));
    emit(func, before, InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) }, ty)
}

fn binary(func: &mut Func, before: Inst, opcode: Opcode, a: Value, b: Value, ty: Type) -> Value {
    let args = func.push_values(&[a, b]);
    emit(func, before, InstData { args, ..InstData::new(opcode) }, ty)
}

fn emit(func: &mut Func, before: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

/// How far down the function each value is computed, per section 19.2.
///
/// A block's rank is its place in reverse postorder, spaced out so that a value computed in it can
/// sit between it and the next. A parameter of the function ranks by its position, a loop header's
/// parameter ranks above everything, and an instruction ranks as described in the module notes.
fn ranks(func: &Func, cfg: &Cfg, entry: Block) -> Map<Value, u64> {
    let mut rank: Map<Value, u64> = Map::default();
    for (at, block) in cfg.reverse_postorder().enumerate() {
        let base = (at as u64 + 1) << 16;
        let header = cfg.predecessors(block).iter().any(|&pred| cfg.rank(pred) >= cfg.rank(block));
        for (index, &param) in func[block].params.iter().enumerate() {
            let value = if block == entry {
                index as u64 + 1
            } else if header {
                u64::MAX
            } else {
                base
            };
            rank.insert(param, value);
        }
        for inst in func.insts(block) {
            let data = &func[inst];
            let value = if data.opcode.has_effects() {
                base
            } else {
                let most = func[data.args]
                    .iter()
                    .map(|arg| rank.get(arg).copied().unwrap_or(0))
                    .max()
                    .unwrap_or(0);
                most.saturating_add(1).min(base)
            };
            for result in data.results() {
                rank.insert(result, value);
            }
        }
    }
    rank
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::Reassoc;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it under this much fuel.
    fn run(body: &str, fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            Reassoc.run(&mut module[id], &mut an, fuel);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn cleaned(body: &str) -> String {
        run(body, &mut Fuel::unlimited())
    }

    fn count(out: &str, opcode: &str) -> usize {
        out.lines().filter(|line| line.contains(&format!("= {opcode}"))).count()
    }

    const SPREAD: &str = r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 3
    %3 = add.nsw %0, %2
    %4 = sub.nsw %1, %0
    %5 = add.nsw %3, %4
    %6 = iconst.i32 4
    %7 = add.nsw %5, %6
    return %7
}
"#;

    /// `(a + 3) + (b - a) + 4` is `b + 7`: the two constants fold, `a` cancels against its
    /// negation, and the add that is left carries no `nsw`.
    #[test]
    fn constants_fold_and_a_term_cancels_against_its_negation() {
        let out = cleaned(SPREAD);
        assert_eq!(count(&out, "add"), 1, "{out}");
        assert_eq!(count(&out, "sub"), 0, "{out}");
        assert!(out.contains("iconst.i32 7"), "{out}");
        assert!(out.contains("= add %1, "), "the flags went with the old tree, {out}");
    }

    /// `a + b + a` is `b + a * 2`, or rather `a * 2 + b` in rank order, and `a ^ b ^ a` is `b`.
    #[test]
    fn a_repeated_term_is_a_multiply_and_twice_in_an_xor_is_nothing() {
        let out = cleaned(
            r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = add %0, %1
    %3 = add %2, %0
    %4 = xor %0, %1
    %5 = xor %4, %0
    %6 = sub %3, %5
    return %6
}
"#,
        );
        assert_eq!(count(&out, "mul"), 1, "{out}");
        assert!(out.contains("iconst.i32 2"), "{out}");
        assert_eq!(count(&out, "xor"), 0, "{out}");
    }

    /// A value something else also reads is a leaf, so a tree of two leaves is left alone.
    #[test]
    fn a_shared_subexpression_is_a_leaf_and_not_flattened() {
        let body = r#"
func @use(i32), linkage(external);

func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = add.nsw %0, %1
    call @use(%2) : (i32)
    %3 = add.nsw %2, %0
    return %3
}
"#;
        let out = cleaned(body);
        assert!(out.contains("%2 = add.nsw %0, %1"), "{out}");
        assert!(out.contains("%3 = add.nsw %2, %0"), "{out}");
    }

    /// In `x = x + a + i + b` the loop header's parameters go last, so the invariant `a + b` is
    /// one value and the recurrence on `x` is the last add.
    #[test]
    fn a_loop_header_parameter_goes_last_so_the_recurrence_is_one_add() {
        let out = cleaned(
            r#"
func @g(i32, i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32, %2: i32):
    %3 = iconst.i32 0
    jump block1(%3, %3)

block1(%4: i32, %5: i32):
    %6 = add %5, %1
    %7 = add %6, %4
    %8 = add %7, %2
    %9 = iconst.i32 1
    %10 = add %4, %9
    %11 = icmp slt %10, %0
    br_if %11, block1(%10, %8), block2

block2:
    return %8
}
"#,
        );
        assert!(out.contains("= add %1, %2"), "the invariant pair first, {out}");
        assert!(out.lines().any(|line| line.contains("= add ") && line.ends_with(", %5")), "{out}");
    }

    /// A tree already in rank order with nothing to combine keeps its shape and its flags.
    #[test]
    fn a_tree_already_in_order_is_left_alone() {
        let body = r#"
func @f(i32, i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32, %2: i32):
    %3 = add.nsw %0, %1
    %4 = add.nsw %3, %2
    return %4
}
"#;
        let out = cleaned(body);
        assert!(
            out.contains("%3 = add.nsw %0, %1") && out.contains("%4 = add.nsw %3, %2"),
            "{out}"
        );
    }

    /// Zero in an `and` or a product is the answer, and all ones in an `or`.
    #[test]
    fn an_absorbing_constant_is_the_answer() {
        let out = cleaned(
            r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 -1
    %3 = or %0, %2
    %4 = or %3, %1
    return %4
}
"#,
        );
        assert_eq!(count(&out, "or"), 0, "{out}");
        assert!(out.contains("iconst.i32 -1"), "{out}");
    }

    #[test]
    fn fuel_stops_the_rebuilding() {
        let none = run(SPREAD, &mut Fuel::of(0));
        assert_eq!(count(&none, "add"), 3, "{none}");
        let one = run(SPREAD, &mut Fuel::of(1));
        assert_eq!(count(&one, "add"), 1, "{one}");
    }
}
