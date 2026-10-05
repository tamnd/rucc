//! E-classes and extraction over the hash-consed rewriter, the second half of the e-graph.
//!
//! Design: `spec/optimizer/12-egraph.md`. Section 12.3 calls the full ægraph arm C, and it is arm B,
//! which is [`crate::cons`], with one change: a rule that fires adds its result to the class of the
//! value it matched rather than replacing it. The walk, the table, the cascade and the placement are
//! the ones in [`crate::cons`], run through the same code, so the two arms differ in the classes and
//! in nothing else, which is what section 12.3 asks of the comparison.
//!
//! # The classes
//!
//! A class is a union-find over values. When a rule would rewrite an instruction where it stands,
//! the instruction is copied in front of itself and the rule rewrites the copy, so the old form and
//! the new one are both in the function and their results are one class. When a rule says the result
//! is a value the function already has, or the table finds an equal instruction above, the two values
//! are one class. Every reader the walk reaches after that reads the newest form, which is what arm B
//! would have read, so the rules see the same shapes in both arms.
//!
//! # Extraction
//!
//! Once the walk is done, each value is read as the cheapest member of its class, by the costs of
//! `spec/optimizer/40-cost-models.md`: what the instruction costs on the target, plus what its
//! operands cost as they were extracted. Section 12.4 settles how much effort this gets. It is the
//! bottom-up dynamic program, which costs an operand two readers share twice, and nothing more. Two
//! members that cost the same go to the newest form, so where the cost model has no opinion the
//! answer is arm B's.
//!
//! A member is a candidate for a value only when it is computed above it. That is every form a rule
//! made of the value, since the copies go in front of it, and every earlier value a rule or the
//! table found equal to it, but not a member of the same class computed further down or on another
//! path, which section 12.7 names as the way extraction goes wrong. The walk visits the blocks in
//! the order the rewriter did, so what an operand was extracted as is known by the time its reader
//! is costed. What was not extracted is left for [`crate::dce`].
//!
//! # The budget
//!
//! A rule set with a cycle in it would add forms for ever, and section 12.7 asks for a budget per
//! function. [`heuristics::EGRAPH_NODES`] forms is it. Once it is spent the rules stop and the walk
//! carries on with the table and the placement, as it does for an instruction no rule matches.
//!
//! # What is counted
//!
//! How many forms the classes hold and how many classes there are, as notes, which `-fopt-info-note`
//! prints. Section 12.2 says the average of the two is the number that decides whether the classes
//! are worth having, and Cranelift's is 1.13.

use rucc_base::hash::{Map, Set};
use rucc_cost::{Cost, CostTable, Cycles, Width, heuristics};
use rucc_ir::{Block, Def, Func, Inst, InstData, Opcode, Value};

use crate::dom::Dominators;
use crate::gcm::movable;
use crate::simplify::{Found, Rewrite, apply};
use crate::stats::Kind;
use crate::uses::{chase, substitute};
use crate::{Analyses, Fuel, Pass, Preserved, Stats, cons};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "egraph";

/// Recorded with how many forms of values the classes held.
pub const NODES: &str = "forms of values in the e-classes";

/// Recorded with how many classes those forms were in.
pub const CLASSES: &str = "e-classes";

/// Recorded for a value read as a form other than the newest, because the cost model found that
/// form cheaper.
const EXTRACTED: &str = "value read as an older form of it, which costs less than the newest";

/// Recorded once when the rules stop because the function spent its budget.
const SPENT: &str = "rewrite rules stopped, the function spent its e-graph budget";

/// The pass.
#[derive(Debug)]
pub struct EGraph;

impl Pass for EGraph {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "the hash-consed rewriter with each rewrite added to a class rather than replacing what it \
         matched, and the cheapest form of each value extracted"
    }

    fn preserves(&self) -> Preserved {
        // What the walk preserves, since it is the walk. Extraction only points readers at other
        // values, which is the liveness again.
        cons::Cons.preserves()
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        cons::build(func, an, fuel, Some(&mut Classes::new(heuristics::EGRAPH_NODES)))
    }
}

/// The classes the walk builds, and what extraction needs to know about them.
#[derive(Debug)]
pub(crate) struct Classes {
    /// The union-find. A value points at another in its class, and one that is not here is the
    /// root of its own.
    parent: Map<Value, Value>,
    /// Every value the walk looked at, in the order it did, and every form a rule added.
    nodes: Vec<Value>,
    /// Every value in a class with more than one member, whether or not the walk looked at it. A
    /// parameter a rule found equal to a result is a member and is not a node.
    members: Set<Value>,
    /// The value the walk looked at that each copy is a form of.
    anchor: Map<Value, Value>,
    /// How many more forms the rules may add.
    budget: u32,
    /// Whether the budget has been reported as spent.
    told: bool,
}

impl Classes {
    /// Empty, with that many forms for the rules to add.
    pub(crate) fn new(budget: u32) -> Self {
        Self {
            parent: Map::default(),
            nodes: Vec::new(),
            members: Set::default(),
            anchor: Map::default(),
            budget,
            told: false,
        }
    }

    /// The root of the class the value is in.
    fn find(&mut self, value: Value) -> Value {
        let mut root = value;
        while let Some(&up) = self.parent.get(&root) {
            root = up;
        }
        let mut at = value;
        while let Some(&up) = self.parent.get(&at) {
            if up != root {
                self.parent.insert(at, root);
            }
            at = up;
        }
        root
    }

    /// Makes the two values one class.
    pub(crate) fn union(&mut self, a: Value, b: Value) {
        self.members.insert(a);
        self.members.insert(b);
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent.insert(a, b);
        }
    }

    /// Notes that the walk reached a value, which makes it a node of the graph.
    pub(crate) fn look(&mut self, value: Value) {
        self.nodes.push(value);
    }

    /// Whether the rules may add another form, which says so once when they may not.
    pub(crate) fn open(&mut self, stats: &mut Stats) -> bool {
        if self.budget > 0 {
            return true;
        }
        if !self.told {
            self.told = true;
            stats.missed(SPENT);
        }
        false
    }

    /// Does what a rule found, keeping what it matched.
    ///
    /// A rule that found the result is another value makes the two one class and ends the cascade,
    /// which is nothing. A rule that rewrites the instruction rewrites a copy of it put in front of
    /// it, and the copy is what the cascade goes on with.
    pub(crate) fn grow(
        &mut self,
        func: &mut Func,
        inst: Inst,
        found: Found,
        forward: &mut Map<Value, Value>,
    ) -> Option<Inst> {
        let result = func[inst].first_result.expect("a rule matched a result");
        if matches!(found, Found::Lane(_) | Found::Rule(Rewrite::Value(_))) {
            apply(func, inst, found, forward);
            self.union(result, chase(forward, result));
            return None;
        }
        let copy = copy(func, inst);
        apply(func, copy, found, forward);
        let made = func[copy].first_result.expect("a copy of a result has one");
        let of = self.anchor.get(&result).copied().unwrap_or(result);
        self.anchor.insert(made, of);
        self.union(result, made);
        self.nodes.push(made);
        self.budget -= 1;
        forward.insert(result, made);
        Some(copy)
    }

    /// Points every reader at the cheapest form of what it reads, per the module documentation.
    pub(crate) fn extract(
        &mut self,
        func: &mut Func,
        dom: &Dominators,
        table: Option<&CostTable>,
        order: &[Block],
        forward: &Map<Value, Value>,
        stats: &mut Stats,
    ) {
        let nodes = std::mem::take(&mut self.nodes);
        let mut roots: Set<Value> = Set::default();
        for &node in &nodes {
            roots.insert(self.find(node));
        }
        stats.record(Kind::Note, NODES, u32::try_from(nodes.len()).unwrap_or(u32::MAX));
        stats.record(Kind::Note, CLASSES, u32::try_from(roots.len()).unwrap_or(u32::MAX));
        let mut class: Map<Value, Vec<Value>> = Map::default();
        let mut members: Vec<Value> = self.members.iter().copied().collect();
        members.sort_unstable();
        for member in members {
            let root = self.find(member);
            class.entry(root).or_default().push(member);
        }
        // The forms of each value the walk looked at, which are read as whatever it is read as.
        let mut forms: Map<Value, Vec<Value>> = Map::default();
        for (&made, &of) in &self.anchor {
            forms.entry(of).or_default().push(made);
        }
        let mut cost: Map<Value, Cost> = Map::default();
        let mut chosen: Map<Value, Value> = Map::default();
        let mut visited: Set<Value> = Set::default();
        for &block in order {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                let Some(result) = func[inst].first_result else { continue };
                let paid = if movable(func, inst) {
                    func[func[inst].args].iter().fold(price(func, inst, table), |sum, &arg| {
                        let arg = chosen.get(&arg).copied().unwrap_or(arg);
                        sum + cost.get(&arg).copied().unwrap_or(Cost::ZERO)
                    })
                } else {
                    Cost::ZERO
                };
                cost.insert(result, paid);
                visited.insert(result);
                if self.anchor.contains_key(&result) {
                    continue;
                }
                let Some(candidates) = class.get(&self.find(result)) else { continue };
                let newest = chase(forward, result);
                let above = |member: Value| match func[member].def {
                    Def::Param { block: home, .. } => dom.dominates(home, block),
                    Def::Result { inst: made, .. } => {
                        visited.contains(&member)
                            && func.block_of(made).is_some_and(|home| dom.dominates(home, block))
                    }
                };
                let best = candidates
                    .iter()
                    .copied()
                    .filter(|&member| above(member))
                    .min_by_key(|&member| {
                        (cost.get(&member).copied().unwrap_or(Cost::ZERO), member != newest)
                    })
                    .unwrap_or(result);
                if best != newest {
                    stats.optimized(EXTRACTED);
                }
                for form in
                    std::iter::once(result).chain(forms.get(&result).into_iter().flatten().copied())
                {
                    if form != best {
                        chosen.insert(form, best);
                    }
                }
            }
        }
        if !chosen.is_empty() {
            substitute(func, &chosen);
        }
    }
}

/// A copy of the instruction, in front of it, reading what it reads.
fn copy(func: &mut Func, inst: Inst) -> Inst {
    let data = &func[inst];
    let (opcode, flags, extra) = (data.opcode, data.flags, data.extra);
    let args: Vec<Value> = func[data.args].to_vec();
    let ty = func[data.first_result.expect("a rule matched a result")].ty;
    let span = func.span(inst);
    let args = func.push_values(&args);
    let made =
        func.create_inst(InstData { flags, extra, args, ..InstData::new(opcode) }, &[ty], span);
    func.insert_before(made, inst);
    made
}

/// What the instruction costs on its own, on the target the table is for.
///
/// One cycle for everything without a table, so that the count of instructions decides, and a
/// complexity of one for everything, so that two forms that take as long go to the smaller.
fn price(func: &Func, inst: Inst, table: Option<&CostTable>) -> Cost {
    let data = &func[inst];
    let Some(table) = table else { return Cost::new(Cycles::insns(1), 1) };
    let bits = data.first_result.map_or(64, |result| func[result].ty.bits());
    let width = match bits {
        0..=8 => Width::W8,
        9..=16 => Width::W16,
        17..=32 => Width::W32,
        _ => Width::W64,
    };
    let args = &func[data.args];
    let cycles = match data.opcode {
        _ if args.is_empty() => Cycles::ZERO,
        Opcode::Mul => table.mult_of(width),
        Opcode::SDiv | Opcode::UDiv | Opcode::SRem | Opcode::URem => table.divide_of(width),
        Opcode::Shl | Opcode::LShr | Opcode::AShr => match args.get(1) {
            Some(&by) if constant(func, by) => table.shift_const,
            _ => table.shift_var,
        },
        Opcode::SExt => table.movsx,
        Opcode::ZExt => table.movzx,
        Opcode::Trunc => Cycles::ZERO,
        _ => table.add,
    };
    Cost::new(cycles, 1)
}

/// Whether the value is an integer constant.
fn constant(func: &Func, value: Value) -> bool {
    match func[value].def {
        Def::Result { inst, .. } => func[inst].opcode == Opcode::IConst,
        Def::Param { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_base::hash::Map;
    use rucc_cost::Goal;
    use rucc_target::Arch;

    use super::{CLASSES, Classes, NODES};
    use crate::stats::Kind;
    use crate::{Fuel, Pass, cons, dce::Dce};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass and then dead code elimination run over it, and what
    /// the pass said.
    fn run(body: &str, budget: u32) -> (String, crate::Stats) {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        let mut stats = crate::Stats::new();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            let func = &mut module[id];
            let mut classes = Classes::new(budget);
            stats.merge(&cons::build(func, &mut an, &mut Fuel::unlimited(), Some(&mut classes)));
            Dce.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited());
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        (rucc_ir::print(&module, &names), stats)
    }

    /// How many lines hold `what`.
    fn count(out: &str, what: &str) -> usize {
        out.lines().filter(|line| line.contains(what)).count()
    }

    #[test]
    fn a_cheaper_form_a_rule_added_is_the_one_extracted() {
        // Twice a value is the value added to itself, and the add is what is left.
        let (out, _) = run(
            r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 2
    %2 = mul %0, %1
    return %2
}
"#,
            u32::MAX,
        );
        assert_eq!(count(&out, "= mul"), 0, "{out}");
        assert_eq!(count(&out, "= add %0, %0"), 1, "{out}");
    }

    #[test]
    fn the_rules_stop_when_the_budget_is_spent() {
        let (out, stats) = run(
            r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 2
    %2 = mul %0, %1
    return %2
}
"#,
            0,
        );
        assert_eq!(count(&out, "= mul"), 1, "{out}");
        assert_eq!(stats.count(Kind::Missed, super::SPENT), 1);
    }

    #[test]
    fn the_forms_and_the_classes_are_counted() {
        // `(x + 0) * 1` is three classes before the rules and the two rewrites add a form each to
        // the classes they fired in: the add is `x`, and the multiply is the add.
        let (_, stats) = run(
            r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = add %0, %1
    %3 = iconst.i32 1
    %4 = mul %2, %3
    return %4
}
"#,
            u32::MAX,
        );
        let (nodes, classes) = (stats.count(Kind::Note, NODES), stats.count(Kind::Note, CLASSES));
        assert!(nodes >= classes && classes > 0, "{nodes} forms in {classes} classes");
    }

    /// An add, then a multiply of the same operands, then both read.
    const ABOVE: &str = r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = add %0, %1
    %3 = mul %0, %1
    %4 = sub %3, %2
    return %4
}
"#;

    /// The multiply first and the add after it.
    const BELOW: &str = r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = mul %0, %1
    %3 = add %0, %1
    %4 = sub %2, %3
    return %4
}
"#;

    /// That function with its first two results made one class by hand, which is the way to have
    /// two members no rule would have put together and so to say which one wins, and the
    /// extraction run with the x86-64 costs.
    fn extract(body: &str) -> (String, crate::Stats) {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let id = module.funcs().next().expect("one function");
        let func = &mut module[id];
        let an = crate::machine::fixtures::analyses();
        let dom = an.dominators(func);
        let order: Vec<_> = an.cfg(func).reverse_postorder().collect();
        let entry = func.entry().expect("a body");
        let results: Vec<_> =
            func.insts(entry).filter_map(|inst| func[inst].first_result).collect();
        let mut classes = Classes::new(u32::MAX);
        classes.look(results[0]);
        classes.look(results[1]);
        classes.union(results[0], results[1]);
        let table = rucc_cost::for_arch(Arch::X86_64).map(|costs| costs.table(Goal::Speed));
        let mut stats = crate::Stats::new();
        classes.extract(func, dom, table, &order, &Map::default(), &mut stats);
        (rucc_ir::print(&module, &names), stats)
    }

    #[test]
    fn an_older_form_that_costs_less_is_extracted_over_the_newest() {
        // The multiply is read as the add, which costs less and is computed above it.
        let (out, stats) = extract(ABOVE);
        assert!(out.contains("= sub %2, %2"), "{out}");
        assert_eq!(stats.count(Kind::Optimized, super::EXTRACTED), 1);
    }

    #[test]
    fn a_member_computed_below_a_value_is_not_a_candidate_for_it() {
        // The add costs less, but it is not there yet where the multiply is computed, so the
        // multiply is read as itself.
        let (out, stats) = extract(BELOW);
        assert!(out.contains("= sub %2, %3"), "{out}");
        assert_eq!(stats.count(Kind::Optimized, super::EXTRACTED), 0);
    }
}
