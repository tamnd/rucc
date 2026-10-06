//! What a body comes to with some of its parameters known, worked out once for the function and
//! read off for each call, which is gcc's `ipa_fn_summary`.
//!
//! A body that switches on a mode parameter is one arm long when a call passes the mode, and the
//! whole switch when it does not:
//!
//! ```c
//! static int pick(int mode, int x)
//! {
//!   switch (mode) {
//!   case 0: return x + 1;
//!   case 1: return x * 3;
//!   default: return x / 7 + x % 5;
//!   }
//! }
//! ```
//!
//! [`super::folded`] finds that by walking the body for every call it weighs, folding as it goes.
//! A summary does the walk once. It keeps each instruction under the condition on the parameters
//! that has to hold for its block to stay, and under the parameters whose being known folds the
//! instruction itself away, and adds up the instructions with the same two of those together. A
//! condition is a branch going to one of its targets, which holds when the branch is not decided
//! by what is known or when it is decided for that target. Answering a call is then working out
//! the branches the call decides and adding up the entries that are still there, with no walk.
//!
//! The rules are [`super::folded`]'s, and the answer is the one it gives, which every debug build
//! checks. A condition on a block is a disjunction of conjunctions of branch conditions, as gcc's
//! predicates are, kept to [`INLINE_SUMMARY_CLAUSES`] clauses of that many conditions each. A body
//! whose conditions would need more, or that has more than 64 parameters, has no exact summary,
//! and its calls are weighed by walking the body as before.

use rucc_base::Interner;
use rucc_base::hash::{Map, Set};
use rucc_cost::heuristics::{INLINE_CALL_TIME, INLINE_SUMMARY_CLAUSES};
use rucc_ir::{Block, Def, Func, Imm, Inst, Opcode, Type, Value};

use super::{computed, goes_to, weight};
use crate::cfg::Cfg;

/// The parameters something needs known, one bit for each, or nothing when no set of them does.
type Mask = Option<u64>;

/// Which conditions have to hold together, by their place in [`Summary::conditions`], for any one
/// of the clauses. A predicate with an empty clause always holds.
type Predicate = Vec<Vec<u32>>;

/// What [`Summary::held`] has worked out, by value.
type Memo = Map<Value, Option<(Imm, Type)>>;

/// The instructions of a body that stay or go together.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// When their blocks stay, by its place in [`Summary::predicates`].
    predicate: u32,
    /// The parameters whose being known folds them away, either of two sets, sorted.
    unless: [Mask; 2],
    /// How many there are.
    plain: usize,
    /// What gcc's `estimate_num_insns` charges for them. See [`weight`].
    weighed: usize,
    /// How long they take, each weighed by how often its block runs.
    time: f64,
}

/// What a body comes to for one call.
#[derive(Debug, Clone, Copy)]
pub(super) struct Estimate {
    /// How many instructions are left, which is [`super::folded_size`] without weights.
    pub plain: usize,
    /// The same with gcc's weights, which is [`super::folded_size`] with them.
    pub weighed: usize,
    /// How long what is left takes.
    pub time: f64,
    /// Whether what the call knows removes anything the body has when nothing is known.
    pub cut: bool,
}

/// A body's size and time as functions of what is known about its parameters.
#[derive(Debug, Clone)]
pub(super) struct Summary {
    /// The parameters, whose places are the bits of a mask.
    params: Vec<Value>,
    /// The parameters each value needs known to be known itself, for the ones some set does.
    masks: Map<Value, u64>,
    /// A branch going to a block.
    conditions: Vec<(Inst, Block)>,
    predicates: Vec<Predicate>,
    /// Whether each predicate holds when nothing is known.
    alone: Vec<bool>,
    entries: Vec<Entry>,
    /// Whether the summary gives [`super::folded`]'s answer. When not, it gives none.
    exact: bool,
}

impl Summary {
    /// A summary that answers nothing.
    fn inexact() -> Self {
        Self {
            params: Vec::new(),
            masks: Map::default(),
            conditions: Vec::new(),
            predicates: Vec::new(),
            alone: Vec::new(),
            entries: Vec::new(),
            exact: false,
        }
    }

    /// The summary of a body, with the time each block takes weighed by how often `frequency`
    /// says it runs, or once each without it.
    pub(super) fn of(func: &Func, names: &Interner, frequency: Option<&Map<Block, f64>>) -> Self {
        let Some(entry) = func.entry() else { return Self::inexact() };
        let params = func[entry].params.clone();
        if params.len() > 64 {
            return Self::inexact();
        }
        let mut summary = Self { params, exact: true, ..Self::inexact() };
        for (at, &param) in summary.params.iter().enumerate() {
            summary.masks.insert(param, 1 << at);
        }
        let readers = readers(func);
        let only = |value: Option<Value>, fits: &dyn Fn(Opcode, usize) -> bool| {
            value
                .and_then(|value| readers.get(&value))
                .is_some_and(|readers| readers.iter().all(|&(opcode, at)| fits(opcode, at)))
        };
        let address = |opcode: Opcode, at: usize| {
            matches!((opcode, at), (Opcode::Load, 0) | (Opcode::Store, 1))
        };
        let cfg = Cfg::new(func);
        let mut reaching: Map<Block, Predicate> = Map::default();
        reaching.insert(entry, vec![Vec::new()]);
        let mut interned: Map<Predicate, u32> = Map::default();
        let mut decisions: Map<(Inst, Block), u32> = Map::default();
        let mut placed: Map<(u32, [Mask; 2]), usize> = Map::default();
        for block in cfg.reverse_postorder() {
            let Some(reach) = reaching.get(&block).cloned() else { continue };
            let predicate = match interned.get(&reach) {
                Some(&id) => id,
                None => {
                    let id = u32::try_from(summary.predicates.len()).unwrap_or(u32::MAX);
                    interned.insert(reach.clone(), id);
                    summary.predicates.push(reach.clone());
                    id
                }
            };
            let often = frequency.and_then(|it| it.get(&block)).copied().unwrap_or(1.0);
            for inst in func.insts(block) {
                let data = &func[inst];
                let args = &func[data.args];
                let mask = |at: usize| args.get(at).and_then(|arg| summary.masks.get(arg)).copied();
                let operands =
                    args.iter().try_fold(0, |all, arg| Some(all | summary.masks.get(arg)?));
                let result = data.results().next();
                let free = match data.opcode {
                    Opcode::IConst
                    | Opcode::FConst
                    | Opcode::GlobalAddr
                    | Opcode::IsConstant
                    | Opcode::Jump
                    | Opcode::UnreachableHint => Some(0),
                    Opcode::Shl
                    | Opcode::LShr
                    | Opcode::AShr
                    | Opcode::Add
                    | Opcode::Sub
                    | Opcode::Mul
                    | Opcode::And
                    | Opcode::Or
                    | Opcode::Xor
                    | Opcode::Ctlz
                    | Opcode::Cttz
                    | Opcode::Ctpop
                    | Opcode::ICmp
                    | Opcode::Select => operands,
                    Opcode::BrIf | Opcode::Switch => mask(0),
                    _ => None,
                };
                let conversion = matches!(
                    data.opcode,
                    Opcode::Trunc
                        | Opcode::SExt
                        | Opcode::ZExt
                        | Opcode::PtrToInt
                        | Opcode::IntToPtr
                        | Opcode::Bitcast
                );
                let costless = match data.opcode {
                    _ if conversion => Some(0),
                    Opcode::PtrAdd if only(result, &address) => Some(0),
                    Opcode::PtrAdd => mask(1),
                    Opcode::Mul | Opcode::Shl
                        if only(result, &|opcode, at| opcode == Opcode::PtrAdd && at == 1) =>
                    {
                        mask(1)
                    }
                    Opcode::ICmp
                        if only(result, &|opcode, at| {
                            matches!((opcode, at), (Opcode::BrIf | Opcode::Expect, 0))
                        }) =>
                    {
                        Some(0)
                    }
                    Opcode::Expect | Opcode::Return | Opcode::LifetimeEnd => Some(0),
                    Opcode::Alloca if args.is_empty() => Some(0),
                    _ => None,
                };
                let settles = matches!(data.opcode, Opcode::BrIf | Opcode::Switch)
                    && args.first().is_some_and(|&test| summary.settles(func, test));
                if let Some(known) = if conversion { operands } else { free } {
                    for result in data.results() {
                        summary.masks.insert(result, known);
                    }
                }
                if func.is_terminator(inst) {
                    let mut seen = Set::default();
                    for call in func.successors(inst) {
                        let to = call.block;
                        let later =
                            cfg.rank(block).zip(cfg.rank(to)).is_some_and(|(at, on)| on > at);
                        if !later || !seen.insert(to) {
                            continue;
                        }
                        let mut taken = reach.clone();
                        if settles {
                            let next = u32::try_from(summary.conditions.len()).unwrap_or(u32::MAX);
                            let condition = *decisions.entry((inst, to)).or_insert(next);
                            if condition == next {
                                summary.conditions.push((inst, to));
                            }
                            for clause in &mut taken {
                                clause.push(condition);
                            }
                        }
                        let into = reaching.entry(to).or_default();
                        into.extend(taken);
                        if !simplify(into) {
                            return Self::inexact();
                        }
                    }
                }
                let unless = either(free, costless);
                if unless[0] == Some(0) {
                    continue;
                }
                let read = data.results().any(|result| readers.contains_key(&result));
                let cost = weight(func, inst, names, read);
                // A call takes longer than its size says, which is gcc's `eni_time_weights`.
                let waits = match data.opcode {
                    Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => {
                        f64::from(rucc_cost::param!(INLINE_CALL_TIME)) - 1.0
                    }
                    _ => 0.0,
                };
                let at = *placed.entry((predicate, unless)).or_insert(summary.entries.len());
                if at == summary.entries.len() {
                    summary.entries.push(Entry {
                        predicate,
                        unless,
                        plain: 0,
                        weighed: 0,
                        time: 0.0,
                    });
                }
                let entry = &mut summary.entries[at];
                entry.plain += 1;
                entry.weighed += cost;
                entry.time += (cost as f64 + waits) * often;
            }
        }
        let truths = summary.truths(func, 0, &Map::default());
        summary.alone = summary.predicates.iter().map(|it| holds(it, &truths)).collect();
        summary
    }

    /// What the body comes to when a call passes the constants in `values` for its parameters, or
    /// nothing when the summary cannot say.
    pub(super) fn estimate(
        &self,
        func: &Func,
        values: &Map<Value, (Imm, Type)>,
    ) -> Option<Estimate> {
        if !self.exact {
            return None;
        }
        let known = self
            .params
            .iter()
            .enumerate()
            .filter(|(_, param)| values.contains_key(param))
            .fold(0, |known, (at, _)| known | 1 << at);
        let truths = self.truths(func, known, values);
        let mut estimate = Estimate { plain: 0, weighed: 0, time: 0.0, cut: false };
        for entry in &self.entries {
            let stays = holds(&self.predicates[entry.predicate as usize], &truths);
            let folds = entry.unless.iter().flatten().any(|&mask| mask & !known == 0);
            if stays && !folds {
                estimate.plain += entry.plain;
                estimate.weighed += entry.weighed;
                estimate.time += entry.time;
            } else if self.alone[entry.predicate as usize] {
                estimate.cut = true;
            }
        }
        Some(estimate)
    }

    /// Whether a branch on `test` is one some set of known parameters decides, which is when the
    /// test is a parameter or is worked out from values that some set makes known.
    fn settles(&self, func: &Func, test: Value) -> bool {
        match func[test].def {
            Def::Result { inst, .. } => {
                func[inst].results == 1
                    && func[func[inst].args].iter().all(|arg| self.masks.contains_key(arg))
            }
            Def::Param { .. } => self.masks.contains_key(&test),
        }
    }

    /// Whether each condition holds when the parameters in `known` are known, with `values` the
    /// numbers of the ones a call passes.
    fn truths(&self, func: &Func, known: u64, values: &Map<Value, (Imm, Type)>) -> Vec<bool> {
        let mut memo = Memo::default();
        self.conditions
            .iter()
            .map(|&(inst, to)| {
                let data = &func[inst];
                let Some(&test) = func[data.args].first() else { return true };
                self.held(func, test, known, values, &mut memo)
                    .and_then(|(value, _)| goes_to(func, data, value))
                    .is_none_or(|call| call.block == to)
            })
            .collect()
    }

    /// What [`super::folded`] holds as the number of `value` once the parameters in `known` are
    /// known, worked out from the bottom up so a long chain of arithmetic is no deeper a call.
    fn held(
        &self,
        func: &Func,
        value: Value,
        known: u64,
        values: &Map<Value, (Imm, Type)>,
        memo: &mut Memo,
    ) -> Option<(Imm, Type)> {
        let mut stack = vec![value];
        while let Some(&top) = stack.last() {
            if memo.contains_key(&top) {
                stack.pop();
                continue;
            }
            let inst = match (values.get(&top), self.computable(func, top, known)) {
                (None, Some(inst)) => inst,
                (found, _) => {
                    memo.insert(top, found.copied());
                    stack.pop();
                    continue;
                }
            };
            let args = &func[func[inst].args];
            let waiting: Vec<Value> =
                args.iter().copied().filter(|arg| !memo.contains_key(arg)).collect();
            if waiting.is_empty() {
                let found = computed(func, inst, &|arg| {
                    memo.get(&arg).copied().flatten().or_else(|| crate::fold::constant(func, arg))
                });
                memo.insert(top, found);
                stack.pop();
            } else {
                stack.extend(waiting);
            }
        }
        memo.get(&value).copied().flatten()
    }

    /// The instruction `value` is the one result of, when everything it reads is known once the
    /// parameters in `known` are, which is when [`super::folded`] works its number out.
    fn computable(&self, func: &Func, value: Value, known: u64) -> Option<Inst> {
        let Def::Result { inst, .. } = func[value].def else { return None };
        let data = &func[inst];
        let needs = func[data.args].iter().try_fold(0, |all, arg| Some(all | self.masks.get(arg)?));
        (data.results == 1 && needs.is_some_and(|needs| needs & !known == 0)).then_some(inst)
    }
}

/// Who reads each value and as which operand, the argument of a jump to a block being read by a
/// jump at no operand. See [`super::folded`].
fn readers(func: &Func) -> Map<Value, Vec<(Opcode, usize)>> {
    let mut readers: Map<Value, Vec<(Opcode, usize)>> = Map::default();
    for inst in func.blocks().flat_map(|block| func.insts(block)) {
        for (at, &arg) in func[func[inst].args].iter().enumerate() {
            readers.entry(arg).or_default().push((func[inst].opcode, at));
        }
        for call in func.successors(inst) {
            for &arg in &func[call.args] {
                readers.entry(arg).or_default().push((Opcode::Jump, usize::MAX));
            }
        }
    }
    readers
}

/// The two sets of parameters either of which folds an instruction away, the one being enough on
/// its own when the other holds it, so the same instructions are summed together.
fn either(free: Mask, costless: Mask) -> [Mask; 2] {
    match (free, costless) {
        (Some(one), Some(other)) if one & other == one => [Some(one), None],
        (Some(one), Some(other)) if one & other == other => [Some(other), None],
        (Some(one), Some(other)) => [Some(one.min(other)), Some(one.max(other))],
        (Some(one), None) | (None, Some(one)) => [Some(one), None],
        (None, None) => [None, None],
    }
}

/// Puts a predicate in its shortest form, a clause that holds whenever a shorter one does being
/// dropped, and says whether it fits in [`INLINE_SUMMARY_CLAUSES`].
fn simplify(predicate: &mut Predicate) -> bool {
    for clause in predicate.iter_mut() {
        clause.sort_unstable();
        clause.dedup();
    }
    predicate
        .sort_unstable_by(|one, other| one.len().cmp(&other.len()).then_with(|| one.cmp(other)));
    predicate.dedup();
    let mut kept: Predicate = Vec::new();
    for clause in predicate.drain(..) {
        if !kept.iter().any(|shorter| shorter.iter().all(|it| clause.binary_search(it).is_ok())) {
            kept.push(clause);
        }
    }
    *predicate = kept;
    let most = rucc_cost::param!(INLINE_SUMMARY_CLAUSES) as usize;
    predicate.len() <= most && predicate.iter().all(|clause| clause.len() <= most)
}

/// Whether a predicate holds, given whether each condition does.
fn holds(predicate: &Predicate, truths: &[bool]) -> bool {
    predicate.iter().any(|clause| clause.iter().all(|&condition| truths[condition as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inline::{folded, folded_size};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// `pick` above: a switch on the mode with one instruction in each of the first two arms and
    /// two in the default.
    const PICK: &str = r#"
func @pick(i32, i32) -> i32, linkage(internal) {
block0(%0: i32, %1: i32):
    switch %0, block3, [0 => block1, 1 => block2]

block1:
    %2 = iconst.i32 1
    %3 = add.i32 %1, %2
    jump block4(%3)

block2:
    %4 = iconst.i32 3
    %5 = mul.i32 %1, %4
    jump block4(%5)

block3:
    %6 = global_addr @table
    %7 = load.i32 %6, align 4
    %8 = add.i32 %7, %1
    jump block4(%8)

block4(%9: i32):
    return %9
}
"#;

    /// Two tests one inside the other, `if (mode > lim) { if (mode < 10) ... }`, with one, two and
    /// three instructions in the three arms.
    const NEST: &str = r#"
func @nest(i32, i32) -> i32, linkage(internal) {
block0(%0: i32, %1: i32):
    %2 = icmp sgt %0, %1
    br_if %2, block1, block4

block1:
    %3 = iconst.i32 10
    %4 = icmp slt %0, %3
    br_if %4, block2, block3

block2:
    %5 = global_addr @table
    %6 = load.i32 %5, align 4
    jump block5(%6)

block3:
    %7 = global_addr @table
    %8 = load.i32 %7, align 4
    %9 = add.i32 %8, %0
    jump block5(%9)

block4:
    %10 = global_addr @table
    %11 = load.i32 %10, align 4
    %12 = add.i32 %11, %0
    %13 = add.i32 %12, %1
    jump block5(%13)

block5(%14: i32):
    return %14
}
"#;

    fn parsed(body: &str) -> (Func, Interner) {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let id = module.funcs().next().expect("the fixture has a function");
        (module[id].clone(), names)
    }

    /// The numbers passed for the parameters at these places.
    fn values(func: &Func, passed: &[(usize, i128)]) -> Map<Value, (Imm, Type)> {
        let entry = func.entry().expect("the fixture has a body");
        passed
            .iter()
            .map(|&(at, number)| {
                let param = func[entry].params[at];
                let ty = func[param].ty;
                (param, (Imm::int(number, ty), ty))
            })
            .collect()
    }

    /// The summary's size with the parameters at these places passed as these numbers, checked
    /// against the walk, and whether it says something was cut.
    fn sized(body: &str, passed: &[(usize, i128)]) -> (usize, bool) {
        let (func, names) = parsed(body);
        let values = values(&func, passed);
        let summary = Summary::of(&func, &names, None);
        let estimate = summary.estimate(&func, &values).expect("the summary is exact");
        assert_eq!(estimate.plain, folded_size(&func, Set::default(), values.clone(), None));
        assert_eq!(estimate.weighed, folded_size(&func, Set::default(), values, Some(&names)));
        (estimate.plain, estimate.cut)
    }

    #[test]
    fn a_switch_on_a_parameter_passed_counts_the_arm_it_takes() {
        assert_eq!(sized(PICK, &[]), (5, false));
        assert_eq!(sized(PICK, &[(0, 0)]), (1, true));
        assert_eq!(sized(PICK, &[(0, 1)]), (1, true));
        assert_eq!(sized(PICK, &[(0, 7)]), (2, true));
        assert_eq!(sized(PICK, &[(1, 5)]), (3, true));
        assert_eq!(sized(PICK, &[(0, 0), (1, 5)]), (0, true));
    }

    #[test]
    fn a_test_inside_a_test_is_decided_by_what_both_need() {
        assert_eq!(sized(NEST, &[]), (8, false));
        assert_eq!(sized(NEST, &[(0, 20), (1, 5)]), (2, true));
        assert_eq!(sized(NEST, &[(0, 7), (1, 5)]), (1, true));
        assert_eq!(sized(NEST, &[(0, 1), (1, 5)]), (3, true));
        assert_eq!(sized(NEST, &[(0, 20)]), (6, true));
        assert_eq!(sized(NEST, &[(1, 5)]), (8, false));
    }

    /// A test worked out from a parameter rather than the parameter itself.
    #[test]
    fn a_test_on_arithmetic_over_a_parameter_is_decided_too() {
        let body = r#"
func @bit(i32) -> i32, linkage(internal) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = and.i32 %0, %1
    br_if %2, block1, block2

block1:
    %3 = global_addr @table
    %4 = load.i32 %3, align 4
    return %4

block2:
    %5 = global_addr @table
    %6 = load.i32 %5, align 4
    %7 = add.i32 %6, %0
    return %7
}
"#;
        assert_eq!(sized(body, &[]), (5, false));
        assert_eq!(sized(body, &[(0, 3)]), (1, true));
        assert_eq!(sized(body, &[(0, 2)]), (2, true));
    }

    /// The time is the walk's, each block weighed by how often it runs.
    #[test]
    fn the_time_is_the_walks() {
        let (func, names) = parsed(NEST);
        let frequency: Map<Block, f64> =
            func.blocks().enumerate().map(|(at, block)| (block, 0.5 + at as f64 * 0.75)).collect();
        let summary = Summary::of(&func, &names, Some(&frequency));
        for passed in [&[][..], &[(0, 20)], &[(0, 7), (1, 5)], &[(1, 5)]] {
            let values = values(&func, passed);
            let estimate = summary.estimate(&func, &values).expect("the summary is exact");
            let walked = folded(&func, Set::default(), values, Some(&names), Some(&frequency)).1;
            assert!((estimate.time - walked).abs() < 1e-9, "{} {walked}", estimate.time);
        }
    }

    #[test]
    fn a_predicate_past_the_clauses_gcc_keeps_is_not_exact() {
        let mut fits: Predicate = (0..8).map(|condition| vec![condition]).collect();
        assert!(simplify(&mut fits));
        let mut over: Predicate = (0..9).map(|condition| vec![condition]).collect();
        assert!(!simplify(&mut over));
        let mut long: Predicate = vec![(0..9).collect()];
        assert!(!simplify(&mut long));
    }

    #[test]
    fn a_clause_a_shorter_one_holds_for_is_dropped() {
        let mut predicate: Predicate = vec![vec![3, 1], vec![1], vec![2, 1, 1], vec![4]];
        assert!(simplify(&mut predicate));
        assert_eq!(predicate, vec![vec![1], vec![4]]);
        let mut always: Predicate = vec![vec![2], Vec::new()];
        assert!(simplify(&mut always));
        assert_eq!(always, vec![Vec::<u32>::new()]);
    }

    #[test]
    fn of_two_sets_one_inside_the_other_the_smaller_is_kept() {
        assert_eq!(either(Some(0b01), Some(0b11)), [Some(0b01), None]);
        assert_eq!(either(Some(0b11), Some(0b10)), [Some(0b10), None]);
        assert_eq!(either(Some(0b01), Some(0b10)), [Some(0b01), Some(0b10)]);
        assert_eq!(either(None, Some(0b10)), [Some(0b10), None]);
        assert_eq!(either(None, None), [None, None]);
    }
}
