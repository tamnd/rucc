//! Range switches: a branch on comparisons of one value against constants too far apart for one
//! bit test becomes a `switch` on the value.
//!
//! Section 19.4 of `spec/optimizer/19-reassociation-and-arithmetic.md`, the part `rangetest` leaves
//! out. `rangetest` reads `x == 1260 || x == 1261 || x == 2964 || ...` as a set of values of `x` and
//! writes back one test per interval of it, all of them run every time, with a bit test only when
//! the whole set fits in one word. PostgreSQL's `IsSharedRelation` is three chains like that, forty
//! six object ids between 1213 and 6303, and what came out was a hundred and forty instructions of
//! comparisons and `or`s run on every call. Both gcc and clang make a `switch` out of the chain,
//! which the switch lowering in `crates/rucc-legalize/src/switch.rs` turns into a binary search
//! over the clusters with a bit test for each stretch of values that fits in a word, so a call is a
//! few comparisons and one bit.
//!
//! # What is matched
//!
//! A block that ends in `br_if` on a tree of `and` and `or` that `rangetest`'s `Read` reads whole,
//! as one set of one value, with the tree used by nothing but the branch. Neither arm passes
//! arguments or carries a hint, so the edges of the `switch` are plain copies of the arms. The set
//! has to be wider than one word, since a set inside one word is `rangetest`'s bit test, and it
//! has to have at least `RANGE_TEST_BIT_INTERVALS` intervals.
//!
//! # When it is worth it
//!
//! The rule is gcc's from `gimple-if-to-switch.cc`: the `switch` is written when the lowering would
//! make fewer clusters out of it than there are intervals, which here means some window of a word
//! holds two intervals or more and becomes one bit test. A set of a few values spread out with no
//! two near each other is as many comparisons as a `switch` as it is now, and stays for `rangetest`.
//!
//! Every value of the set is a case, so the set has to have no more than `RANGE_SWITCH_CASES`
//! values in it. When it has more but the values that fail are few, as with `x != 3 && x != 70 &&
//! ...`, those are the cases instead and they go to the arm taken when the tree is false.

use rucc_cost::heuristics::{RANGE_SWITCH_CASES, RANGE_TEST_BIT_INTERVALS};
use rucc_ir::{Block, BlockCall, Extra, Func, Hint, Imm, Inst, Opcode, SwitchInfo, Value};

use crate::rangetest::{Read, complement, max, sweep};
use crate::uses::count;
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "rangeswitch";

/// Recorded for a branch on comparisons of one value rewritten as a `switch`.
const SWITCHED: &str = "branch on comparisons of one value turned into a switch";

/// Recorded for a branch that would have been rewritten if there had been fuel for it.
const NO_FUEL: &str = "branch left as it was, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeSwitch;

impl Pass for RangeSwitch {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a branch on comparisons of one value against scattered constants becomes a switch"
    }

    fn preserves(&self) -> Preserved {
        // The branch goes to the same two places, but over one edge for each case, and anything
        // that counted edges counted the old ones.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let uses = count(func);
        let mut plans = Vec::new();
        for block in func.blocks().collect::<Vec<Block>>() {
            let Some(term) = func.terminator(block) else { continue };
            let Some(plan) = Plan::of(func, &uses, block, term) else { continue };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            plans.push((term, plan));
        }
        if plans.is_empty() {
            return stats;
        }
        let mut dead = Vec::new();
        for (term, plan) in plans {
            plan.build(func, term);
            dead.extend(plan.consumed);
            stats.optimized(SWITCHED);
        }
        sweep(func, dead);
        stats
    }
}

/// A branch as it will be rewritten.
struct Plan {
    /// The value switched on.
    value: Value,
    /// Every case, in order.
    cases: Vec<u64>,
    /// Where the cases go.
    taken: BlockCall,
    /// Where everything else goes.
    default: BlockCall,
    /// The comparisons and the tree over them, which go with the branch.
    consumed: Vec<Inst>,
}

impl Plan {
    /// The `switch` the branch at the end of the block becomes, when it is worth one.
    fn of(func: &Func, uses: &[u32], block: Block, term: Inst) -> Option<Self> {
        let data = &func[term];
        if data.opcode != Opcode::BrIf {
            return None;
        }
        let Extra::Targets(targets) = data.extra else { return None };
        let &[then, otherwise] = &func[targets] else { return None };
        let plain = |call: BlockCall| func[call.args].is_empty() && call.hint == Hint::NONE;
        if !plain(then) || !plain(otherwise) {
            return None;
        }
        let cond = func[data.args][0];
        if uses[cond.index()] != 1 {
            return None;
        }
        let mut read = Read { func, uses, block, consumed: Vec::new(), compares: 0 };
        let (value, set) = read.set(cond)?;
        // A comparison alone is not a tree and has nothing to merge.
        if read.compares < 2 {
            return None;
        }
        let top = max(func[value].ty);
        let (cases, taken, default) =
            if values(&set) <= u128::from(rucc_cost::param!(RANGE_SWITCH_CASES)) {
                (set, then, otherwise)
            } else {
                let outside = complement(&set, top);
                if values(&outside) > u128::from(rucc_cost::param!(RANGE_SWITCH_CASES)) {
                    return None;
                }
                (outside, otherwise, then)
            };
        let (lo, hi) = (cases.first()?.0, cases.last()?.1);
        let one_word = hi - lo < 64; // not a threshold: one bit for each number in a `u64`.
        if one_word
            || cases.len() < rucc_cost::param!(RANGE_TEST_BIT_INTERVALS)
            || windows(&cases) >= cases.len()
        {
            return None;
        }
        let cases = cases.iter().flat_map(|&(from, to)| from..=to).collect();
        Some(Self { value, cases, taken, default, consumed: read.consumed })
    }

    /// Writes the `switch` over the branch.
    fn build(&self, func: &mut Func, term: Inst) {
        let ty = func[self.value].ty;
        let mut calls = vec![self.default];
        calls.extend(self.cases.iter().map(|_| self.taken));
        let imms: Vec<Imm> =
            self.cases.iter().map(|&case| Imm::int(i128::from(case), ty)).collect();
        let targets = func.push_block_calls(&calls);
        let cases = func.push_imms(&imms);
        let info = func.add_switch(SwitchInfo { targets, cases });
        let args = func.push_values(&[self.value]);
        let data = &mut func[term];
        data.opcode = Opcode::Switch;
        data.args = args;
        data.extra = Extra::Switch(info);
    }
}

/// How many values the intervals hold, which can be more than a `u64` counts.
fn values(set: &[(u64, u64)]) -> u128 {
    set.iter().map(|&(from, to)| u128::from(to - from) + 1).sum()
}

/// How many windows of one word the intervals take, each starting at the first interval the one
/// before could not reach, which is how many bit tests and lone comparisons the lowering makes of
/// them.
fn windows(set: &[(u64, u64)]) -> usize {
    let mut count = 0;
    let mut start: Option<u64> = None;
    for &(from, to) in set {
        match start {
            Some(lo) if to - lo < 64 => {} // not a threshold: the bits in a word
            _ => {
                count += 1;
                start = Some(from);
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use rucc_base::Interner;

    use super::{RangeSwitch, windows};
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it.
    fn run(body: &str) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            RangeSwitch.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// A function that branches on `x op1 k1 join x op2 k2 join ...` and answers one when the
    /// branch is taken and zero when it is not.
    fn chain(pred: &str, join: &str, numbers: &[i64]) -> String {
        let mut body = String::from("func @f(i32) -> i32, linkage(external) {\nblock0(%0: i32):\n");
        let mut next = 1;
        let mut acc: Option<usize> = None;
        for &number in numbers {
            let _ = writeln!(body, "    %{next} = iconst.i32 {number}");
            let _ = writeln!(body, "    %{} = icmp {pred} %0, %{next}", next + 1);
            let bit = next + 1;
            next += 2;
            acc = Some(match acc {
                None => bit,
                Some(had) => {
                    let _ = writeln!(body, "    %{next} = {join} %{had}, %{bit}");
                    next += 1;
                    next - 1
                }
            });
        }
        let cond = acc.expect("at least one number");
        let _ = write!(
            body,
            "    br_if %{cond}, block1, block2\n\nblock1:\n    %{next} = iconst.i32 1\n    \
             return %{next}\n\nblock2:\n    %{} = iconst.i32 0\n    return %{}\n}}\n",
            next + 1,
            next + 1
        );
        body
    }

    /// The default and the cases of the one `switch` in the output, if there is one.
    fn switch(out: &str) -> Option<(String, Vec<(i64, String)>)> {
        let line = out.lines().find(|line| line.trim_start().starts_with("switch "))?;
        let (head, list) = line.split_once('[').expect("a switch has a case list");
        let default = head.split(',').nth(1).expect("a default").trim().to_owned();
        let cases = list
            .trim_end_matches(']')
            .split(", ")
            .map(|case| {
                let (number, block) = case.split_once(" => ").expect("a case");
                (number.parse().expect("a number"), block.to_owned())
            })
            .collect();
        Some((default, cases))
    }

    /// The object ids of the first chain in PostgreSQL's `IsSharedRelation`.
    const SHARED: [i64; 11] = [1260, 1261, 1262, 2964, 6243, 6000, 1214, 2396, 3592, 6100, 1213];

    /// Five comparisons against scattered numbers, one of them of a different value.
    const TWO: &str = r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 1260
    %3 = icmp eq %0, %2
    %4 = iconst.i32 1261
    %5 = icmp eq %1, %4
    %6 = or %3, %5
    %7 = iconst.i32 2964
    %8 = icmp eq %0, %7
    %9 = or %6, %8
    %10 = iconst.i32 6000
    %11 = icmp eq %0, %10
    %12 = or %9, %11
    %13 = iconst.i32 1262
    %14 = icmp eq %0, %13
    %15 = or %12, %14
    br_if %15, block1, block2

block1:
    %16 = iconst.i32 1
    return %16

block2:
    %17 = iconst.i32 0
    return %17
}
"#;

    #[test]
    fn a_chain_of_scattered_equalities_becomes_a_switch_on_the_value() {
        let out = run(&chain("eq", "or", &SHARED));
        let (default, cases) = switch(&out).expect("a switch");
        assert_eq!(default, "block2");
        let mut want = SHARED.to_vec();
        want.sort_unstable();
        assert_eq!(cases.iter().map(|case| case.0).collect::<Vec<_>>(), want);
        assert!(cases.iter().all(|case| case.1 == "block1"), "{out}");
        assert!(!out.contains("icmp"), "the comparisons go with the branch\n{out}");
        assert!(!out.contains("br_if"), "{out}");
    }

    #[test]
    fn a_chain_of_scattered_inequalities_switches_to_the_other_arm() {
        let out = run(&chain("ne", "and", &SHARED));
        let (default, cases) = switch(&out).expect("a switch");
        assert_eq!(default, "block1");
        assert_eq!(cases.len(), SHARED.len());
        assert!(cases.iter().all(|case| case.1 == "block2"), "{out}");
    }

    #[test]
    fn a_set_inside_one_word_is_left_for_the_bit_test() {
        let out = run(&chain("eq", "or", &[1, 5, 9, 33, 60]));
        assert!(switch(&out).is_none(), "{out}");
        assert!(out.contains("br_if"), "{out}");
    }

    #[test]
    fn values_too_far_apart_to_share_a_word_are_left_alone() {
        let out = run(&chain("eq", "or", &[100, 1000, 2000, 3000]));
        assert!(switch(&out).is_none(), "{out}");
    }

    #[test]
    fn two_intervals_are_left_alone() {
        let out = run(&chain("eq", "or", &[100, 101, 5000]));
        assert!(switch(&out).is_none(), "{out}");
    }

    #[test]
    fn a_tree_something_else_reads_is_left_alone() {
        // Eleven comparisons and ten `or`s make the tree `%32`, and the taken arm answers it.
        let body = chain("eq", "or", &SHARED).replace("%33 = iconst.i32 1", "%33 = zext.i32 %32");
        let out = run(&body);
        assert!(switch(&out).is_none(), "{out}");
    }

    #[test]
    fn comparisons_of_two_values_are_left_alone() {
        let out = run(TWO);
        assert!(switch(&out).is_none(), "{out}");
    }

    #[test]
    fn a_window_holds_every_interval_that_ends_within_a_word_of_its_first() {
        assert_eq!(windows(&[(1213, 1214), (1260, 1262), (2396, 2396)]), 2);
        assert_eq!(windows(&[(0, 0), (63, 63), (64, 64)]), 2);
        assert_eq!(windows(&[(0, 100), (101, 102)]), 2);
    }
}
