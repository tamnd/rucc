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
//! No pointer parameters, which are [`crate::params`]' alignment and extent tables, and no return
//! values. A range that is one interval only, the narrower of the unsigned and the signed hull,
//! which is what gcc's `ipa_vr` keeps as well.

use rucc_base::hash::Map;
use rucc_ir::{Facts, FuncId, Module};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::ipa;
use crate::range::Range;
use crate::range::query::Ranges;
use crate::{CallGraph, Stats};

/// What this is called, which is gcc's spelling so that `-fno-ipa-vrp` means the same thing.
pub const NAME: &str = "ipa-vrp";

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
    use super::*;

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
