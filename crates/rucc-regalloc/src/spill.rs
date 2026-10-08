//! Which values go to memory before any register is handed out.
//!
//! Design: `spec/optimizer/39-register-allocation.md` section 39.7, and tamnd/rucc#1177.
//!
//! [`crate::pressure`] finds the points where more values are live than there are registers. At
//! each of those some of the values live there have to be in memory, and this picks which: the
//! lightest first, by the weight [`crate::backtrack`] places values by. That is how often a value
//! is read or written, each time counted by how often its block runs, over how much of the
//! function it is live across. A value read in a loop is heavy for the loop's sake, so what goes
//! is one read outside it, and a value live over a long stretch and rarely read is light, since
//! sending it to memory frees a register over the most points for the fewest loads.
//!
//! The points are taken in order along the line, and at each one values go until no more are
//! over the room. A value that goes is taken off every point it was live at, so one long light
//! value can settle several points at once, and a later point may already be settled by the time
//! it is reached.
//!
//! # What it promises
//!
//! It takes off each point no more values than the pressure model says have to go, and that
//! number is a floor, so every value picked here is standing in for one that some value at that
//! point had to be. It does not promise that the rest then fit. A register a fixed operand claims
//! for one value, the answer of a two address instruction live where its source is read, and a
//! value wanted across a call in one of the registers the call keeps are all left to the
//! assignment, which can still evict and spill after this the way it did before.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use rucc_mir::{Func, Reg};
use rucc_target::RegClass;

use crate::assign;
use crate::backtrack;
use crate::live::{Area, Live};
use crate::order::Point;
use crate::pressure::Pressure;

/// One value that could go to memory.
#[derive(Debug, Clone, Copy)]
struct Candidate<'a> {
    reg: Reg,
    area: Area<'a>,
    weight: u128,
    size: u32,
}

/// The values to send to memory before any register is handed out, lightest first at each point
/// where more are live than there is room for.
///
/// # Panics
///
/// Panics on a function with more virtual registers than a `u32` can number, which is one no
/// earlier pass could have built.
#[must_use]
pub fn choose(func: &Func, live: &Live, pressure: &Pressure) -> Vec<Reg> {
    with(func, live, pressure, &backtrack::costs(func), &assign::forced(func))
}

/// The same, with what each value costs on the stack and the values that go there anyway already
/// read off the function, as the allocator has them by the time it asks.
pub(crate) fn with(
    func: &Func,
    live: &Live,
    pressure: &Pressure,
    costs: &[u128],
    stack: &[Reg],
) -> Vec<Reg> {
    let mut forced = vec![false; func.vregs()];
    for &reg in stack {
        forced[index(reg)] = true;
    }
    let mut classes: BTreeMap<RegClass, Vec<Candidate<'_>>> = BTreeMap::new();
    for (number, &forced) in forced.iter().enumerate() {
        let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
        let (Some(area), Some(class)) = (live.area(reg), func.class_of(reg)) else {
            continue;
        };
        if forced {
            continue;
        }
        let size = backtrack::size(area);
        let weight = costs[number] * 1024 / u128::from(size + 8);
        classes.entry(class).or_default().push(Candidate { reg, area, weight, size });
    }

    let mut pressure = pressure.clone();
    let mut chosen = Vec::new();
    for (class, mut candidates) in classes {
        candidates.sort_by_key(|candidate| candidate.area.hull().start);
        let over: Vec<_> = pressure.over(class).collect();
        // The values live at the point being looked at, lightest first, which is the order they
        // go in. A value goes in when one of its pieces starts and comes out when the piece ends,
        // so each piece is looked at twice however many points it is live over. This used to ask
        // every value whose hull had begun whether it covered the point, at every point, and on a
        // function with many long lived values that was most values at most points.
        let mut here: BTreeSet<(u128, Reverse<u32>, usize)> = BTreeSet::new();
        // When each value next goes in or comes out, one at a time, so a value's own changes are
        // always taken in order.
        let mut changes: BinaryHeap<Reverse<(Point, usize)>> = (candidates.iter().enumerate())
            .map(|(one, candidate)| Reverse((candidate.area.hull().start, one)))
            .collect();
        // Which piece each value is at, and whether it is in `here` for it yet.
        let mut at = vec![(0, false); candidates.len()];
        // The values already sent to memory, which never go back in.
        let mut gone = vec![false; candidates.len()];
        for point in over {
            // Values sent to memory earlier often bring a point back under, and then the changes
            // up to it can wait for the next point that is still over.
            if pressure.excess(class, point) == 0 {
                continue;
            }
            while let Some(&Reverse((when, one))) = changes.peek() {
                if when > point {
                    break;
                }
                changes.pop();
                if gone[one] {
                    continue;
                }
                let candidate = &candidates[one];
                let key = (candidate.weight, Reverse(candidate.size), one);
                let (piece, inside) = &mut at[one];
                let range = candidate.area.piece(*piece);
                if *inside {
                    here.remove(&key);
                    *inside = false;
                    *piece += 1;
                    if *piece < candidate.area.count() {
                        changes.push(Reverse((candidate.area.piece(*piece).start, one)));
                    }
                } else {
                    here.insert(key);
                    *inside = true;
                    if let Some(after) = range.end.checked_add(1) {
                        changes.push(Reverse((after, one)));
                    }
                }
            }
            while pressure.excess(class, point) > 0 {
                let Some((_, _, one)) = here.pop_first() else { break };
                gone[one] = true;
                pressure.lift(class, candidates[one].area);
                chosen.push(candidates[one].reg);
            }
        }
    }
    chosen
}

fn index(reg: Reg) -> usize {
    usize::try_from(reg.number().expect("a virtual register")).expect("a register number")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Opcode, Weight};
    use rucc_target::x86_64::{GPR, SYSV};

    use super::*;
    use crate::assign::Env;
    use crate::order::Order;

    fn narrow(count: usize) -> Env {
        Env::new().with(GPR, &SYSV.int_order[..count], &SYSV.int_order[count..count + 1])
    }

    fn chosen(func: &Func, env: &Env) -> Vec<Reg> {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let pressure = Pressure::of(func, &order, &live, env);
        choose(func, &live, &pressure)
    }

    #[test]
    fn nothing_goes_where_everything_fits() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        assert!(chosen(&func, &narrow(2)).is_empty());
    }

    #[test]
    fn the_value_read_least_is_the_one_that_goes() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let busy = func.new_vreg(GPR);
        let once = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        func.build(block, opcode).def(busy, GPR).finish();
        func.build(block, opcode).def(once, GPR).finish();
        func.build(block, opcode).def(other, GPR).finish();
        for _ in 0..3 {
            func.build(block, opcode).uses(busy, GPR).uses(other, GPR).finish();
        }
        func.build(block, opcode).uses(once, GPR).uses(busy, GPR).uses(other, GPR).finish();

        assert_eq!(chosen(&func, &narrow(2)), [once]);
    }

    #[test]
    fn a_value_read_in_a_loop_stays_and_one_read_outside_it_goes() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let body = func.create_block();
        let out = func.create_block();
        let step = func.new_vreg(GPR);
        let cold = func.new_vreg(GPR);
        func.build(entry, opcode).def(cold, GPR).finish();
        func.build(entry, opcode).def(step, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(body)];
        func.build(body, opcode).uses(step, GPR).finish();
        *func.succs_mut(body) = vec![BlockCall::to(body), BlockCall::to(out)];
        func.set_weight(body, Weight::parts(100 * Weight::SCALE));
        func.build(out, opcode).uses(cold, GPR).finish();
        func.build(out, opcode).uses(step, GPR).finish();

        assert_eq!(chosen(&func, &narrow(1)), [cold]);
    }

    #[test]
    fn one_long_light_value_settles_two_points_that_are_each_one_over() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let long = func.new_vreg(GPR);
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(long, GPR).finish();
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).uses(first, GPR).finish();
        for _ in 0..4 {
            func.build(block, opcode).finish();
        }
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(second, GPR).finish();
        func.build(block, opcode).uses(long, GPR).finish();

        // Each short value meets the long one at a point with room for one. Sending the long one
        // away settles both, where sending the short ones would take two.
        assert_eq!(chosen(&func, &narrow(1)), [long]);
    }
}
