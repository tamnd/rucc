//! Rewriting at construction with one table for the whole function, the first half of the e-graph.
//!
//! Design: `spec/optimizer/12-egraph.md`. Section 12.1 takes the design apart into four decisions,
//! and section 12.2 says that Cranelift's two percent comes from three of them and not from the
//! e-classes. This pass is those three, which is arm B of section 12.3: hash consing, the rules
//! applied as each instruction is built rather than to a fixpoint, and placement left to
//! [`crate::gcm`]. Section 12.6 puts it before the rest of the experiment, because the full
//! e-graph is this with e-classes added, and if this is most of the win it is what ships.
//!
//! # Building in order
//!
//! The IR is already built when the optimizer sees it, so building is a walk. The blocks are taken
//! in reverse postorder, which puts the definition of every operand of an instruction ahead of it
//! except through a block parameter, and an instruction is looked at once, with its operands read
//! through everything the walk has decided so far. That is what makes it construction rather than a
//! pass over finished code: a rule matching the shape of an operand sees the operand after it was
//! rewritten, and an operand that turned out to equal an earlier value is that value.
//!
//! # The rules, once and then again
//!
//! The rules are the ones [`crate::simplify`] applies, from the same tables and through the same
//! code, so the arms of the experiment differ in how they are driven and in nothing else. Where
//! that pass applies one rule per instruction and is run three times to get the rest, this applies
//! a rule, then the rules again to what the first one left, until none fires or
//! [`heuristics::CONS_CASCADE`] have. The bound is section 12.7's answer to a rule set with a cycle
//! in it, which would otherwise not stop.
//!
//! # One table
//!
//! What an instruction computes is the key [`crate::number`] builds, which has the opcode, the
//! flags, the result type, the predicate or the constant, and the operands, with the operands of a
//! commutative opcode put in one order first. Section 12.7 names the two ways this goes wrong, and
//! both are that key: `a + b` and `b + a` are one key, and two adds where one says it does not wrap
//! are two. The table is a list of where each key was found, and the walk asks it about every
//! instruction.
//!
//! When an earlier instruction with the same key is in a block above this one, this one goes and
//! its readers read the earlier one. That is value numbering, and it is what [`crate::number`]
//! does too.
//!
//! When the earlier one is in a block that does not dominate this one, the two are still one value
//! in an e-graph, because the graph has no blocks in it, and it is placement afterwards that puts
//! the one node where both readers can see it. Here that is the earlier instruction moved to the
//! end of the nearest block that dominates both, and this one going, and [`crate::gcm`] after it
//! is free to move it on. It is done only when nothing about it can go wrong:
//!
//! - The opcode is one [`crate::gcm`] moves, and not a division of integers, which could trap on
//!   a path that never divided.
//! - Every operand is defined in a block that dominates the one it moves to, or is a constant,
//!   which is copied there, because a constant stays in the block that reads it.
//! - The block it moves to runs no more often than the two it was in, by the estimated frequencies.
//!   A value both arms of a rare branch inside a loop compute would otherwise be computed on every
//!   iteration.
//!
//! What [`crate::number`] keeps to one block stays in one here, for the reason it gives: constants,
//! anything made only of constants, and addresses are things the code generator wants beside their
//! reader rather than held in a register.
//!
//! # What it is not
//!
//! It is not the e-graph. A rule replaces what it matched and the old form is gone, so there is
//! nothing to extract and nothing a cost model chooses between. Section 12.3 calls that arm B and
//! it is the arm with no search problem in it. [`crate::egraph`] is arm C, and it is this walk
//! with the classes kept: [`build`] takes them, and does what it does here when there are none.

use rucc_base::hash::{Map, Set};
use rucc_cost::heuristics;
use rucc_ir::{Block, Def, Func, Inst, InstData, Opcode, Value};

use crate::egraph::Classes;
use crate::gcm::movable;
use crate::number::{Key, is_address, key, widened};
use crate::simplify::{Finder, apply};
use crate::uses::{chase, count, operands, substitute};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "cons";

/// Recorded for an instruction an equal one above it already computes.
const MERGED: &str = "instruction removed, an equal one in a block above computes the same thing";

/// Recorded for two equal instructions in blocks neither of which dominates the other.
const RAISED: &str =
    "instruction removed, an equal one moved up to the nearest block that dominates both";

/// Recorded when two equal instructions stay apart because the block above runs more often.
const COLDER: &str =
    "equal instructions kept apart, the block above both runs more often than they do";

/// Recorded for a duplicate that would have gone if there had been fuel for it.
const NO_FUEL: &str = "duplicate instruction kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug)]
pub struct Cons;

impl Pass for Cons {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "the rewrite rules applied as each instruction is reached, again on what they leave, and \
         equal instructions made one value across the whole function"
    }

    fn preserves(&self) -> Preserved {
        // No edge moves and no block appears, so the shape of the function does not change. What
        // changes is where values are live, and what the predictors see: a branch whose condition
        // folded or now reads another value is guessed differently, so the frequencies go too.
        Preserved::ALL.without(Analysis::Liveness).without(Analysis::Frequencies)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        build(func, an, fuel, None)
    }
}

/// The walk, with the classes [`crate::egraph`] keeps or without them.
///
/// Without them a rule replaces what it matched and a duplicate goes, which is this pass. With them
/// a rule adds a form to the class of what it matched, a duplicate joins the class of what it
/// duplicates, and the classes are extracted from at the end.
pub(crate) fn build(
    func: &mut Func,
    an: &mut Analyses,
    fuel: &mut Fuel,
    classes: Option<&mut Classes>,
) -> Stats {
    let mut stats = Stats::new();
    if func.entry().is_none() {
        return stats;
    }
    let mut finder = Finder::new(func, an);
    let dom = an.dominators(func);
    let freq = an.frequencies(func);
    let order: Vec<Block> = an.cfg(func).reverse_postorder().collect();
    let reached: Set<Block> = order.iter().copied().collect();
    let blocks: Vec<Block> = order
        .iter()
        .copied()
        .chain(func.blocks().filter(|block| !reached.contains(block)))
        .collect();
    // Who reads what when the walk started, for the reason `crate::simplify` gives: a rule
    // that fires on what nothing reads changes no program and would still spend fuel.
    let uses = count(func);
    let mut walk = Walk { forward: Map::default(), gone: Vec::new(), classes };
    // Where each key was found outside a block of its own, with the instruction, for moving it
    // up when a second one turns up beside it rather than below it.
    let mut across: Map<Key, Vec<(Block, Inst, Value)>> = Map::default();
    // The first value of each constant in the function, which is what a constant operand is
    // compared as in `across`, the same way `crate::number` does it.
    let mut constants: Map<Key, Value> = Map::default();
    let empty: Map<Value, Value> = Map::default();
    for &block in &blocks {
        let mut seen: Map<Key, Value> = Map::default();
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            if !walk.forward.is_empty() {
                let args = func[inst].args;
                func.rewrite(args, |value| chase(&walk.forward, value));
            }
            if func[inst].first_result.is_some_and(|result| uses[result.index()] == 0) {
                continue;
            }
            walk.look(func, inst);
            let Some(inst) = cascade(func, inst, &mut finder, fuel, &mut stats, &mut walk) else {
                continue;
            };
            // The operands were read through `forward` above, so there is nothing left for
            // the key to look them up through.
            let Some((key, result)) = key(func, &empty, inst) else { continue };
            if let Some(&first) = seen.get(&key) {
                walk.take(fuel, &mut stats, inst, result, first, MERGED);
                continue;
            }
            seen.insert(key, result);
            if key.args[0].is_none() {
                constants.entry(key).or_insert(result);
                continue;
            }
            let constant = |arg: &Option<Value>| arg.is_none_or(|arg| argless(func, arg));
            if is_address(key.opcode) || !reached.contains(&block) || key.args.iter().all(constant)
            {
                continue;
            }
            let found = across.entry(widened(func, &constants, key)).or_default();
            if let Some(&(_, _, first)) = found.iter().find(|&&(at, ..)| dom.dominates(at, block)) {
                walk.take(fuel, &mut stats, inst, result, first, MERGED);
                continue;
            }
            let raised = found.iter().enumerate().find_map(|(index, &(at, first, _))| {
                let to = dom.nearest_common_dominator(at, block)?;
                raisable(func, first, to, &|of, to| dom.dominates(of, to)).then_some((index, to))
            });
            let Some((index, to)) = raised else {
                found.push((block, inst, result));
                continue;
            };
            let (at, first, value) = found[index];
            let both = freq.get(at).raw().saturating_add(freq.get(block).raw());
            if freq.get(to).raw() > both {
                stats.missed(COLDER);
                found.push((block, inst, result));
                continue;
            }
            if !fuel.take() {
                stats.missed(NO_FUEL);
                found.push((block, inst, result));
                continue;
            }
            raise(func, first, to, &|of, to| dom.dominates(of, to));
            found[index].0 = to;
            walk.merge(inst, result, value);
            stats.optimized(RAISED);
        }
    }
    let Walk { forward, gone, classes } = walk;
    if let Some(classes) = classes {
        let table = an.machine().table();
        classes.extract(func, dom, table, &order, &forward, &mut stats);
        // A duplicate stayed as a member for extraction to choose between, and goes now unless
        // something chose it. Left for `crate::dce` it would be one more reader of what it reads to
        // the passes in between, and `crate::narrow` takes only a value read once.
        let mut uses = count(func);
        for inst in gone.into_iter().rev() {
            if func[inst].results().any(|value| uses[value.index()] > 0) {
                continue;
            }
            operands(func, inst, |value| uses[value.index()] -= 1);
            func.remove_inst(inst);
        }
        return stats;
    }
    for inst in gone {
        func.remove_inst(inst);
    }
    if !forward.is_empty() {
        substitute(func, &forward);
    }
    stats
}

/// What the walk has decided, read as it goes and applied once at the end.
struct Walk<'a> {
    /// What each result that went is read as, whether a rule or a duplicate sent it there.
    forward: Map<Value, Value>,
    /// The duplicates on their way out, which with classes go only once extraction is done. What a
    /// rule pointed elsewhere is left for [`crate::dce`], as [`crate::simplify`] leaves it.
    gone: Vec<Inst>,
    /// The classes, when the walk is building them. See [`crate::egraph`].
    classes: Option<&'a mut Classes>,
}

impl Walk<'_> {
    /// Records that this instruction computes what an earlier one computed, or says why it stays.
    fn take(
        &mut self,
        fuel: &mut Fuel,
        stats: &mut Stats,
        inst: Inst,
        result: Value,
        first: Value,
        why: &'static str,
    ) {
        if !fuel.take() {
            stats.missed(NO_FUEL);
            return;
        }
        self.merge(inst, result, first);
        stats.optimized(why);
    }

    /// Reads the result as an equal value from now on. The instruction goes, and with classes it is
    /// first another member of the class for extraction to choose between.
    fn merge(&mut self, inst: Inst, result: Value, first: Value) {
        self.forward.insert(result, first);
        if let Some(classes) = self.classes.as_deref_mut() {
            classes.union(result, first);
        }
        self.gone.push(inst);
    }

    /// Makes what the instruction computes a node of the graph, when there is one being built.
    fn look(&mut self, func: &Func, inst: Inst) {
        let Some(classes) = self.classes.as_deref_mut() else { return };
        if let Some((_, result)) = key(func, &Map::default(), inst) {
            classes.look(result);
        }
    }
}

/// Applies the rules to the instruction, and to what they leave of it, until none fires.
///
/// The instruction holding the last form, which is the same one unless there are classes, and
/// nothing when a rule found the result is a value the function already has, which ends it: the
/// instruction is on its way out and there is nothing left to rewrite or to look up.
fn cascade(
    func: &mut Func,
    inst: Inst,
    finder: &mut Finder,
    fuel: &mut Fuel,
    stats: &mut Stats,
    walk: &mut Walk<'_>,
) -> Option<Inst> {
    let mut at = inst;
    for _ in 0..heuristics::CONS_CASCADE {
        if walk.classes.as_deref_mut().is_some_and(|classes| !classes.open(stats)) {
            return Some(at);
        }
        let Some((found, pattern, no_fuel)) = finder.find(func, at) else { return Some(at) };
        if !fuel.take() {
            stats.missed(no_fuel);
            return Some(at);
        }
        stats.optimized(pattern);
        match walk.classes.as_deref_mut() {
            Some(classes) => at = classes.grow(func, at, found, &mut walk.forward)?,
            None if apply(func, at, found, &mut walk.forward) => return None,
            None => {}
        }
    }
    Some(at)
}

/// Whether the value is the result of an instruction with no operands, which is a constant or the
/// address of a symbol.
fn argless(func: &Func, value: Value) -> bool {
    match func[value].def {
        Def::Result { inst, .. } => func[func[inst].args].is_empty(),
        Def::Param { .. } => false,
    }
}

/// The block a value is defined in, if it is defined anywhere.
fn home(func: &Func, value: Value) -> Option<Block> {
    match func[value].def {
        Def::Result { inst, .. } => func.block_of(inst),
        Def::Param { block, .. } => Some(block),
    }
}

/// Whether the instruction can move to the end of that block, per the module documentation.
fn raisable(func: &Func, inst: Inst, to: Block, dominates: &dyn Fn(Block, Block) -> bool) -> bool {
    if !movable(func, inst)
        || matches!(func[inst].opcode, Opcode::SDiv | Opcode::UDiv | Opcode::SRem | Opcode::URem)
        || func.terminator(to).is_none()
    {
        return false;
    }
    func[func[inst].args]
        .iter()
        .all(|&arg| argless(func, arg) || home(func, arg).is_some_and(|from| dominates(from, to)))
}

/// Moves the instruction to the end of that block, with a copy there of each constant it reads
/// that is not already somewhere above it.
fn raise(func: &mut Func, inst: Inst, to: Block, dominates: &dyn Fn(Block, Block) -> bool) {
    let end = func.terminator(to).expect("raisable asked for a terminator");
    func.remove_inst(inst);
    func.insert_before(inst, end);
    let args: Vec<Value> = func[func[inst].args].to_vec();
    let mut copied: Map<Value, Value> = Map::default();
    for arg in args {
        if home(func, arg).is_some_and(|from| dominates(from, to)) || copied.contains_key(&arg) {
            continue;
        }
        let Def::Result { inst: made, .. } = func[arg].def else { continue };
        let data = &func[made];
        let data = InstData { flags: data.flags, extra: data.extra, ..InstData::new(data.opcode) };
        let ty = func[arg].ty;
        let span = func.span(made);
        let copy = func.create_inst(data, &[ty], span);
        func.insert_before(copy, inst);
        copied.insert(arg, func[copy].first_result.expect("one result was asked for"));
    }
    if !copied.is_empty() {
        let args = func[inst].args;
        func.rewrite(args, |value| copied.get(&value).copied().unwrap_or(value));
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::Cons;
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
            Cons.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// How many lines hold `what`.
    fn count(out: &str, what: &str) -> usize {
        out.lines().filter(|line| line.contains(what)).count()
    }

    /// The label of the block the first line holding `what` is in.
    fn block_of(out: &str, what: &str) -> String {
        let mut block = String::new();
        for line in out.lines() {
            let line = line.trim();
            if line.starts_with("block") && line.ends_with(':') {
                block = line.split(['(', ':']).next().unwrap_or_default().to_string();
            } else if line.contains(what) {
                return block;
            }
        }
        panic!("nothing says {what} in\n{out}");
    }

    #[test]
    fn a_plus_b_and_b_plus_a_are_one_value() {
        let out = run(r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = add %0, %1
    %3 = add %1, %0
    %4 = mul %2, %3
    return %4
}
"#);
        assert_eq!(count(&out, "= add"), 1, "{out}");
        assert!(out.contains("mul %2, %2"), "{out}");
    }

    #[test]
    fn two_adds_that_promise_different_things_stay_two() {
        let out = run(r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = add.nsw %0, %1
    %3 = add %0, %1
    %4 = mul %2, %3
    return %4
}
"#);
        assert_eq!(count(&out, "= add"), 2, "{out}");
    }

    #[test]
    fn a_rule_fires_on_what_the_rule_before_it_left() {
        // `(x + 0) * 1` is `x` twice over, and `x - x` of that is zero. One walk sees all three,
        // because the subtraction is looked at with its operands already read as `x`, and the zero
        // it becomes is then the same constant as the one already in the block.
        let out = run(r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = add %0, %1
    %3 = iconst.i32 1
    %4 = mul %2, %3
    %5 = sub %4, %0
    return %5
}
"#);
        assert!(out.contains("return %1"), "{out}");
        assert_eq!(count(&out, "= sub"), 0, "{out}");
    }

    #[test]
    fn what_a_block_computes_reaches_the_blocks_it_dominates() {
        let out = run(r#"
func @f(i32, i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32, %2: i32):
    %3 = mul %0, %1
    %4 = iconst.i32 0
    %5 = icmp ne %2, %4
    br_if %5, block1, block2

block1:
    %6 = mul %1, %0
    return %6

block2:
    return %3
}
"#);
        assert_eq!(count(&out, "= mul"), 1, "{out}");
        assert_eq!(block_of(&out, "= mul"), "block0", "{out}");
    }

    /// The same multiply in both arms of a branch, given `OP`.
    const ARMS: &str = r#"
func @f(i32, i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32, %2: i32):
    %3 = iconst.i32 0
    %4 = icmp ne %2, %3
    br_if %4, block1, block2

block1:
    %5 = iconst.i32 7
    %6 = OP %0, %5
    jump block3(%6)

block2:
    %7 = iconst.i32 7
    %8 = OP %0, %7
    %9 = add %8, %1
    jump block3(%9)

block3(%10: i32):
    return %10
}
"#;

    #[test]
    fn a_duplicate_in_two_arms_becomes_one_in_the_block_above() {
        let out = run(&ARMS.replace("OP", "mul"));
        assert_eq!(count(&out, "= mul"), 1, "{out}");
        assert_eq!(block_of(&out, "= mul"), "block0", "{out}");
    }

    #[test]
    fn a_division_is_never_moved_to_where_it_was_not_going_to_run() {
        let out = run(&ARMS.replace("OP", "sdiv"));
        assert_eq!(count(&out, "= sdiv"), 2, "{out}");
        assert_eq!(block_of(&out, "= sdiv"), "block1", "{out}");
    }

    #[test]
    fn a_duplicate_in_two_arms_of_a_loop_that_mostly_take_neither_stays_in_the_arms() {
        // The nearest block above both is the loop's header, which runs on every iteration, and
        // the two arms between them run on only some of them.
        let out = run(r#"
func @f(i32, i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32, %2: i32):
    %3 = iconst.i32 0
    jump block1(%3)

block1(%4: i32):
    %5 = icmp eq %4, %2
    br_if %5, block3, block2

block2:
    %6 = icmp eq %4, %1
    br_if %6, block4, block5(%4)

block3:
    %7 = mul %0, %1
    jump block5(%7)

block4:
    %8 = mul %1, %0
    %9 = add %8, %4
    jump block5(%9)

block5(%10: i32):
    %11 = iconst.i32 1
    %12 = add %10, %11
    %13 = icmp slt %12, %0
    br_if %13, block1(%12), block6

block6:
    return %12
}
"#);
        assert_eq!(count(&out, "= mul"), 2, "{out}");
    }
}
