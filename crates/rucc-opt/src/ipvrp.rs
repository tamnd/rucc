//! Interprocedural value ranges: what every call passes an integer parameter, written on it.
//!
//! The kernel is the reason. `str_to_user` in `drivers/input/evdev.c` takes a length its three
//! callers all work out as `_IOC_SIZE(cmd)`, which is at most 16383, and `copy_to_user` inlined
//! into it tests that length against `INT_MAX` and warns. Gcc's `-fipa-vrp` reads the three calls,
//! writes the range on the parameter and the test folds away along with its `__bug_table` entry.
//! Without the same here the range query sees a parameter and says it could be anything.
//!
//! # What is claimed
//!
//! A function [`ipa::closed`] says this unit sees every call to has, for each integer parameter, a
//! range that holds the union of what each call can pass there, read with [`Ranges`] at the call.
//! It goes on as `!range(lo, hi)` and [`Ranges`] reads it back on the entry block's parameters, so
//! every pass that asks the range of a value gets it without being told this exists.
//!
//! # Which way the walk goes
//!
//! Callers before callees, so that a call passing the caller's own parameter reads the fact the
//! caller was given a moment before. Each function is visited once. The start is pessimistic,
//! which is the other way round from [`crate::ipcp`]: nothing is known until it is written, and
//! everything written is true on its own because it was worked out from facts that were already
//! true. That is why one visit is enough and why recursion needs nothing special. A function that
//! calls itself reads no fact about itself at its own call, so that call widens the union rather
//! than holding it up.
//!
//! # What is not here
//!
//! No pointer parameters, which are [`crate::params`]' alignment and extent tables. A range that is
//! one interval only, the narrower of the unsigned and the signed hull, which is what gcc's
//! `ipa_vr` keeps as well. A return value only when it is one number, which is [`returns`], and it
//! runs once every body has been through the passes rather than with the parameters.

use rucc_base::Symbol;
use rucc_base::hash::Map;
use rucc_ir::{
    AttrSet, Def, Extra, Facts, Func, FuncId, Imm, Inst, InstData, Module, Opcode, Type, Value,
};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::ipa;
use crate::range::Range;
use crate::range::query::Ranges;
use crate::{CallGraph, Fuel, Stats};

/// What this is called, which is gcc's spelling so that `-fno-ipa-vrp` means the same thing.
pub const NAME: &str = "ipa-vrp";

/// Which phase of section 42.7 the time this takes is counted under. See
/// [`crate::Pass::phase`].
pub const PHASE: crate::Phase = crate::Phase::Scalar;

/// What it says for a parameter every call keeps inside a range narrower than its type.
const WRITTEN: &str = "range every call agrees on written on a parameter";

/// What it says for an integer parameter the calls between them can pass anything.
const UNBOUNDED: &str = "parameter left unbounded, the calls between them can pass it anything";

/// Writes `!range(lo, hi)` on every integer parameter its callers all keep inside one.
///
/// Gives back what it said about each function it looked at: one line for every parameter it
/// wrote a range on, and one for every integer parameter the calls leave unbounded. Without them
/// the pass is one `-fopt-info` and the corpus count of tamnd/rucc#2967 cannot see, and a pass
/// whose work only shows as a test some other pass folds later reads as one that never fires.
pub fn annotate(module: &mut Module, graph: &CallGraph) -> Vec<(FuncId, Stats)> {
    let closed = ipa::closed(module, graph);
    if closed.is_empty() {
        return Vec::new();
    }
    let sites = ipa::sites(module, &closed);
    let mut graphs: Map<FuncId, (Cfg, Dominators)> = Map::default();
    let mut said = Vec::new();
    for part in ipa::order(graph, &closed) {
        for id in part {
            let Some(site) = sites.get(&id) else { continue };
            if site.ragged || site.calls.is_empty() {
                continue;
            }
            for &(caller, _) in &site.calls {
                graphs.entry(caller).or_insert_with(|| {
                    let cfg = Cfg::new(&module[caller]);
                    let dom = Dominators::new(&cfg);
                    (cfg, dom)
                });
            }
            let Some(entry) = module[id].entry() else { continue };
            let params = module[id][entry].params.clone();
            let mut found = Vec::new();
            let mut stats = Stats::new();
            let mut spoke = false;
            for (index, &param) in params.iter().enumerate() {
                let ty = module[id][param].ty;
                // A fact is one interval, and a vector has one for each lane. A vector parameter
                // is what a wasm unit built with `simd128` passes.
                if !ty.is_int() || ty.is_vector() {
                    continue;
                }
                let mut union = Range::empty(ty.bits());
                for &(caller, inst) in &site.calls {
                    let func = &module[caller];
                    let (cfg, dom) = &graphs[&caller];
                    let Some(&arg) = func[func[inst].args].get(index) else {
                        union = Range::full(ty.bits());
                        break;
                    };
                    if func[arg].ty != ty {
                        union = Range::full(ty.bits());
                        break;
                    }
                    let passed = Ranges::new(func, cfg, dom).at_inst(arg, inst);
                    union = union.union(passed);
                    if union.is_full() {
                        break;
                    }
                }
                if let Some(interval) = interval(union) {
                    found.push((param, interval));
                } else if union.is_full() {
                    stats.missed(UNBOUNDED);
                    spoke = true;
                }
            }
            let func = &mut module[id];
            for (param, (lo, hi)) in found {
                let had = func.facts(param);
                let width = func[param].ty.bits();
                // Both are true, so what holds at once is true too, and a second run over one
                // module only ever narrows what the first one wrote.
                let both = match had.range {
                    Some((was_lo, was_hi)) => Range::between(was_lo, was_hi, width)
                        .intersect(Range::between(lo, hi, width)),
                    None => Range::between(lo, hi, width),
                };
                let Some(range) = interval(both) else { continue };
                func.set_facts(param, Facts { range: Some(range), ..had });
                stats.optimized(WRITTEN);
                spoke = true;
            }
            if spoke {
                said.push((id, stats));
            }
        }
    }
    said
}

/// What it says for a call whose result is the number every return of the callee gives.
const RETURNED: &str = "call result folded to the number every return of the callee gives";

/// What it says for one it could have folded and had no fuel left for.
const NO_FUEL: &str = "call result left alone, out of fuel";

/// Puts the number in place of the result of every direct call to a function whose every return
/// gives that one number.
///
/// Gcc 14's `ipa_return_value_range`, cut down to the case where the range is one number. The
/// kernel is the reason again. `tegra241_vintf_alloc_lvcmdq_user` in
/// `drivers/iommu/arm/arm-smmu-v3/tegra241-cmdqv.c` tests what `tegra241_vcmdq_hw_init_user`
/// gives back and unwinds through a `WARN_ON` when it is not zero, and the callee ends in
/// `return 0` with no other return. Gcc reads that, folds the test and the `__bug_table` entry
/// goes with it. Neither callee is inlined, so without this the test stays.
///
/// Callees before callers, so that a function whose only return gives back what another one does
/// is read once that one is known. Only a body [`CallGraph::trusted_body`] hands back is read,
/// which is the one that runs, and only an integer, so a call through a prototype that disagrees
/// with the definition about the type is left alone. Nothing is claimed for a function held to
/// `-O0` and nothing is rewritten in one.
///
/// Gives back what it said about each function it rewrote, and those are the functions to tidy.
pub fn returns(module: &mut Module, graph: &CallGraph, fuel: &mut Fuel) -> Vec<(FuncId, Stats)> {
    let mut known: Map<Symbol, (Imm, Type)> = Map::default();
    let mut said = Vec::new();
    for part in graph.components() {
        for &node in part {
            let Some(id) = graph.func(node) else { continue };
            if module[id].is_declaration() || module[id].attrs.set.contains(AttrSet::OPTNONE) {
                continue;
            }
            let stats = fold_calls(&mut module[id], &known, fuel);
            if stats.changed() {
                said.push((id, stats));
            }
            if graph.trusted_body(node).is_some() {
                if let Some(number) = gives(&module[id], &known) {
                    known.insert(module[id].name, number);
                }
            }
        }
    }
    said
}

/// The number every return of this body gives, when there is one and it is an integer.
///
/// A return reads it either as a constant or as the result of a direct call to a function already
/// known to give one. A body with no return at all gives nothing, which is a function that does not
/// come back.
fn gives(func: &Func, known: &Map<Symbol, (Imm, Type)>) -> Option<(Imm, Type)> {
    let mut number: Option<(Imm, Type)> = None;
    for block in func.blocks() {
        let Some(term) = func.terminator(block) else { continue };
        let this = match func[term].opcode {
            Opcode::Return => {
                let &[value] = &func[func[term].args] else { return None };
                read(func, value, known)?
            }
            // Gives back whatever the callee does, which is not read here.
            Opcode::TailCall => return None,
            _ => continue,
        };
        if !this.1.is_int() || this.1.is_vector() || number.is_some_and(|had| had != this) {
            return None;
        }
        number = Some(this);
    }
    number
}

/// The number a value is, read as a constant or as a call [`called`] knows the answer to.
fn read(func: &Func, value: Value, known: &Map<Symbol, (Imm, Type)>) -> Option<(Imm, Type)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    match func[inst].opcode {
        Opcode::IConst => {
            let Extra::Imm(at) = func[inst].extra else { return None };
            Some((func[at], func[value].ty))
        }
        Opcode::Call => called(func, inst, known),
        _ => None,
    }
}

/// What a direct call gives back, when the callee is known to give one number of the type the call
/// takes it as.
fn called(func: &Func, inst: Inst, known: &Map<Symbol, (Imm, Type)>) -> Option<(Imm, Type)> {
    let Extra::Call(at) = func[inst].extra else { return None };
    let &(imm, ty) = known.get(&func[at].callee?)?;
    let mut results = func[inst].results();
    match (results.next(), results.next()) {
        (Some(result), None) if func[result].ty == ty => Some((imm, ty)),
        _ => None,
    }
}

/// Writes the number in front of every call in this body [`called`] knows the answer to, and has
/// the result's readers read that instead.
///
/// The call stays, because whatever else it does is still done. A call whose result nothing reads
/// is left alone, since there is nothing to fold.
fn fold_calls(func: &mut Func, known: &Map<Symbol, (Imm, Type)>, fuel: &mut Fuel) -> Stats {
    let mut stats = Stats::new();
    if known.is_empty() {
        return stats;
    }
    let read = ipa::operands(func);
    let mut found = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if func[inst].opcode != Opcode::Call {
                continue;
            }
            let Some((imm, ty)) = called(func, inst, known) else { continue };
            let Some(result) = func[inst].results().next() else { continue };
            if read.contains(&result) {
                found.push((inst, result, imm, ty));
            }
        }
    }
    let mut same: Map<Value, Value> = Map::default();
    for (inst, result, imm, ty) in found {
        if !fuel.take() {
            stats.missed(NO_FUEL);
            continue;
        }
        let span = func.span(inst);
        let at = func.add_imm(imm);
        let number = func.create_inst(
            InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) },
            &[ty],
            span,
        );
        func.insert_before(number, inst);
        same.insert(result, func[number].results().next().expect("one result was asked for"));
        stats.optimized(RETURNED);
    }
    if !same.is_empty() {
        crate::uses::substitute(func, &same);
    }
    stats
}

/// One interval holding every value in the range, read whichever way round makes it narrower.
///
/// `None` for a range that says nothing, which is the full one, and for the empty one, which is a
/// parameter no call that can run reaches and so is better left to the pass that removes it.
fn interval(range: Range) -> Option<(u128, u128)> {
    if range.is_empty() || range.is_full() {
        return None;
    }
    let width = range.width();
    let mask = if width >= 128 { u128::MAX } else { (1 << width) - 1 };
    let (low, high) = range.unsigned_bounds()?;
    let (least, most) = range.signed_bounds()?;
    let (lo, hi) = if most.abs_diff(least) < high - low {
        (least as u128 & mask, most as u128 & mask)
    } else {
        (low, high)
    };
    if lo == 0 && hi == mask {
        return None;
    }
    Some((lo, hi))
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::Pic;

    use super::*;
    use crate::stats::Kind;

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "aarch64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-S128"
func @warn(), linkage(external);
"#;

    /// A caller that warns when what `@init` gives back is not zero, the shape of
    /// `tegra241_vintf_alloc_lvcmdq_user`.
    const CALLER: &str = r#"
func @alloc(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @init(%0) : (i32) -> i32
    %2 = iconst.i32 0
    %3 = icmp ne %1, %2
    br_if %3, block1, block2

block1:
    call @warn() : ()
    jump block2

block2:
    return %1
}
"#;

    /// The text with [`returns`] run over it, and how many call results it folded.
    fn run(body: &str) -> (String, u32) {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}{CALLER}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let graph = CallGraph::of(&module, Pic::Executable);
        let said = returns(&mut module, &graph, &mut Fuel::unlimited());
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the sweep left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        let folded = said.iter().map(|(_, stats)| stats.count(Kind::Optimized, RETURNED)).sum();
        (rucc_ir::print(&module, &names), folded)
    }

    /// What the caller's `icmp` reads first, which is the call or the number put in its place.
    fn compared(text: &str) -> String {
        let alloc = &text[text.find("func @alloc").expect("the caller is printed")..];
        let line = alloc.lines().find(|line| line.contains("icmp ne")).expect("the test is kept");
        let first = line.split("icmp ne ").nth(1).expect("an operand").split(',').next();
        let first = first.expect("an operand").trim().to_string();
        let def = alloc.lines().find(|line| line.trim_start().starts_with(&format!("{first} =")));
        def.expect("the operand is defined").trim().to_string()
    }

    #[test]
    fn a_call_to_a_function_every_return_of_which_gives_zero_reads_zero() {
        let (text, folded) = run(r#"
func @init(i32) -> i32, linkage(internal) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp ne %0, %1
    br_if %2, block1, block2

block1:
    call @warn() : ()
    return %1

block2:
    %3 = iconst.i32 0
    return %3
}
"#);
        assert_eq!(folded, 1, "{text}");
        assert!(compared(&text).contains("iconst.i32 0"), "{text}");
        // The call stays, because the warning in it is still given.
        assert!(text.contains("call @init"), "{text}");
    }

    #[test]
    fn a_function_that_gives_back_what_one_known_to_give_zero_does_is_known_too() {
        let (text, folded) = run(r#"
func @zero() -> i32, linkage(internal) {
block0:
    call @warn() : ()
    %0 = iconst.i32 0
    return %0
}

func @init(i32) -> i32, linkage(internal) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp ne %0, %1
    br_if %2, block1, block2

block1:
    %3 = call @zero() : () -> i32
    return %3

block2:
    return %1
}
"#);
        // The one in `@init` and the one in the caller.
        assert_eq!(folded, 2, "{text}");
        assert!(compared(&text).contains("iconst.i32 0"), "{text}");
    }

    #[test]
    fn two_returns_giving_two_numbers_leave_the_call_alone() {
        let (text, folded) = run(r#"
func @init(i32) -> i32, linkage(internal) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp ne %0, %1
    br_if %2, block1, block2

block1:
    %3 = iconst.i32 -22
    return %3

block2:
    return %1
}
"#);
        assert_eq!(folded, 0, "{text}");
        assert!(compared(&text).contains("call @init"), "{text}");
    }

    #[test]
    fn a_body_another_object_can_win_over_is_not_read() {
        let (text, folded) = run(r#"
func @init(i32) -> i32, linkage(weak) {
block0(%0: i32):
    %1 = iconst.i32 0
    return %1
}
"#);
        assert_eq!(folded, 0, "{text}");
        assert!(compared(&text).contains("call @init"), "{text}");
    }

    #[test]
    fn a_return_that_reads_a_parameter_leaves_the_call_alone() {
        let (text, folded) = run(r#"
func @init(i32) -> i32, linkage(internal) {
block0(%0: i32):
    return %0
}
"#);
        assert_eq!(folded, 0, "{text}");
    }

    #[test]
    fn an_interval_is_read_whichever_way_is_narrower() {
        let small = Range::between(0, 16383, 32);
        assert_eq!(interval(small), Some((0, 16383)));
        // Minus one to four, which read unsigned is nearly everything.
        let around = Range::signed_between(-1, 4, 32);
        assert_eq!(interval(around), Some((0xffff_ffff, 4)));
        assert_eq!(interval(Range::full(32)), None);
        assert_eq!(interval(Range::empty(32)), None);
    }
}
