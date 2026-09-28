//! Which register each value lives in, decided in the order the values are hardest to place, and
//! undone when a value that would cost more to lose finds its register taken.
//!
//! Design: `spec/optimizer/39-register-allocation.md` section 39.7, and tamnd/rucc#1177.
//!
//! [`crate::assign`] is the `-O0` answer. It walks the line once, and when it runs out of
//! registers the value that goes to the stack is the one whose range ends last. That is a guess
//! about cost made from a fact about length, and it is wrong exactly where it matters: a value read
//! in every turn of a loop and wanted again after the loop ends last, so it is the one that goes,
//! and every turn of the loop pays a load for it.
//!
//! This answers the same question with the cost in it. Every value gets a weight, which is how
//! often it is read or written, each time counted by how often the block it happens in runs, over
//! how much of the function it is live across. A value with a high weight is one that would cost
//! a lot to keep in memory for little register in return, and that is the one to keep.
//!
//! # The order values are placed in
//!
//! Longest first. A long value meets more of the others than a short one does, so it has the
//! fewest registers to choose from, and giving it first choice is what leaves the short ones
//! something to fit into. That is the order LLVM's greedy allocator takes them in, for the same
//! reason.
//!
//! # Backtracking
//!
//! Going longest first means a long cold value takes a register before a short hot one has been
//! looked at. When the hot one comes and finds nothing free, it asks what it would cost to take a
//! register back. For each register the answer is the values in it that are in the way, and the
//! register can be taken when every one of them weighs less than the value asking. Of the
//! registers that can, the one taken is the one whose heaviest value in the way is lightest. The
//! values that lose it go back in the queue and look for another register, and a value that has
//! lost one [`ROUNDS`] times goes to the stack instead of looking again.
//!
//! That rule is also why it stops. A value only ever takes a register from values lighter than
//! itself, and each value is put back a bounded number of times.
//!
//! # What it keeps from the linear scan
//!
//! Everything that says what a register may hold. The registers an instruction insists on, the
//! values an instruction can only read from memory, the two address instructions and the hints
//! are all read the way [`crate::assign`] reads them, from the same functions, so the two
//! allocators cannot disagree about what the machine allows. They only disagree about who gets
//! the register, and [`crate::check`] asks the same questions of either answer.
//!
//! A two address instruction is coalesced from both ends here. The linear scan only ever meets the
//! answer after its source, since the source is written first. Here either can be placed first, so
//! a source looks at where the answer that reuses it went as well as the other way round.
//!
//! # When it gives up
//!
//! Every question of whether two values are both wanted is counted, and a function that asks more
//! than [`BUDGET`] of them is handed to the linear scan instead. The answer is worse and it comes
//! out in time, which is what the section asks of a pathological function. It is a count of work
//! rather than of values because a function with many short values that never meet is cheap
//! however many there are.
//!
//! # What the spill phase adds
//!
//! It runs twice when [`crate::pressure`] finds a point with more values live than registers. Once
//! as above, where a value goes to the stack only when the queue reaches it and it can take no
//! register back, and once with the values [`crate::spill`] picked sent to the stack before the
//! queue starts. The first is better where the pressure is brief and eviction settles it in a few
//! moves. The second is better where it is long, since the values that go are picked by weight
//! across every point that is over rather than by which one the queue met last. Neither wins
//! everywhere, so both are costed.
//!
//! It also gives up when it would lose. The linear scan runs as well, which is cheap next to this,
//! and the answer kept is the one with the lower [`cost`]: the loads and stores of the values on
//! the stack and the copies between the ends of each tie left apart, each counted by how often its
//! block runs. Placing the long values first is right where registers are fought over in a loop,
//! and it can be worse in a long straight run of arithmetic, where the order the linear scan walks
//! in is also the order that lets each answer follow its source.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use rucc_mir::{Func, Inst, Reg};
use rucc_target::{PhysReg, RegClass};

use crate::assign::{self, Assignment, Blocks, Env, Place, Reuse, Want};
use crate::live::{Area, Live, Range};
use crate::order::Order;
use crate::pressure::Pressure;
use crate::spill;

/// How many questions of whether two values are both wanted a function may ask before it is handed
/// to the linear scan.
pub const BUDGET: u64 = 50_000_000;

/// How many times a value may lose its register and look for another before it goes to the stack.
pub const ROUNDS: u32 = 4;

/// One value, as the queue sees it.
#[derive(Debug, Clone, Copy)]
struct Value<'a> {
    reg: Reg,
    class: RegClass,
    area: Area<'a>,
    range: Range,
    weight: u128,
    size: u32,
}

/// Where every value goes, in the order that places the hard ones first and takes a register back
/// when a heavier value wants it.
///
/// A function that asks more than [`BUDGET`] questions gets the linear scan's answer instead, and
/// so does one where the linear scan's answer has the lower [`cost`].
#[must_use]
pub fn assign(func: &Func, order: &Order, live: &Live, env: &Env) -> Assignment {
    let linear = assign::assign(func, order, live, env);
    let mut best = cost(func, order, &linear);
    let mut kept = linear;
    let pressure = Pressure::of(func, order, live, env);
    let spilled = spill::choose(func, live, &pressure);
    // With nothing sent ahead the second try would be the first one again.
    let tries: &[&[Reg]] = if spilled.is_empty() { &[&[]] } else { &[&[], &spilled] };
    for &early in tries {
        let Some(ours) = placed(func, order, live, env, BUDGET, early) else { break };
        let spent = cost(func, order, &ours);
        if spent <= best {
            best = spent;
            kept = ours;
        }
    }
    kept
}

/// What an assignment is expected to cost a function, in instructions each counted by how often
/// its block runs.
///
/// A value on the stack costs a load or a store every time it is read or written. A two address
/// instruction whose answer is not where its source is costs the copy between them, and so does a
/// block parameter that is not where the value passed to it is. Moves the machine needs whatever
/// the assignment, like those into the registers a call insists on, are left out, since they are
/// the same for any answer.
///
/// # Panics
///
/// Panics on a function with more values than a register number can name, as
/// [`Reg::virtual_reg`] does.
#[must_use]
pub fn cost(func: &Func, order: &Order, assignment: &Assignment) -> u128 {
    let costs = costs(func);
    let mut total = 0;
    for (reg, place) in assignment.placed() {
        if !matches!(place, Place::Reg(_)) {
            total += costs[index(reg)];
        }
    }
    let mut weights = HashMap::new();
    for block in func.blocks() {
        let weight = u128::from(func[block].weight.raw().max(1));
        for inst in func.insts(block) {
            weights.insert(inst, weight);
        }
        for call in &func[block].succs {
            for (&arg, param) in call.args.iter().zip(&func[call.block].params) {
                if assignment.place(arg) != assignment.place(param.reg) {
                    total += weight;
                }
            }
        }
    }
    let commuted: HashSet<Inst> = assignment.commuted().iter().copied().collect();
    for (number, reuse) in assign::reuses(func, order).iter().enumerate() {
        let Some(reuse) = reuse else { continue };
        let answer = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
        let tied = if commuted.contains(&reuse.inst) { reuse.second } else { Some(reuse.source) };
        let Some(at) = assignment.place(answer) else { continue };
        if tied.and_then(|tied| assignment.place(tied)) != Some(at) {
            total += weights.get(&reuse.inst).copied().unwrap_or(1);
        }
    }
    total
}

/// The same, with the budget said, or `None` for a function that went over it.
///
/// # Panics
///
/// Panics on a value in a class the environment hands out no registers from, as the linear scan
/// does.
#[must_use]
pub fn within(
    func: &Func,
    order: &Order,
    live: &Live,
    env: &Env,
    budget: u64,
) -> Option<Assignment> {
    placed(func, order, live, env, budget, &[])
}

fn placed(
    func: &Func,
    order: &Order,
    live: &Live,
    env: &Env,
    budget: u64,
    early: &[Reg],
) -> Option<Assignment> {
    let blocked = assign::blocked(func, order);
    let forced = assign::forced(func);
    let reuses = assign::reuses(func, order);
    let hints = assign::hints(func);
    let passed = assign::passed(func);
    let received = received(func);
    let reused = reused(&reuses);
    let costs = costs(func);

    let count = func.vregs();
    let mut values: Vec<Option<Value<'_>>> = vec![None; count];
    let mut queue = BinaryHeap::new();
    for (number, reuse) in reuses.iter().enumerate() {
        let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
        let (Some(mut area), Some(class)) = (live.area(reg), func.class_of(reg)) else {
            continue;
        };
        if let Some(reuse) = reuse {
            area = area.with(reuse.at);
        }
        let size = size(area);
        let weight = costs[number] * 1024 / u128::from(size + 8);
        values[number] = Some(Value { reg, class, area, range: area.hull(), weight, size });
        queue.push((size, Reverse(number)));
    }

    let mut state = State {
        live,
        blocked: &blocked,
        reuses: &reuses,
        values: &values,
        held: Vec::new(),
        at: vec![None; count],
        commuted: vec![None; count],
        work: 0,
        budget,
    };
    let mut assignment = Assignment::empty(count);
    let mut lost = vec![0u32; count];
    while let Some((_, Reverse(number))) = queue.pop() {
        let Some(value) = values[number] else { continue };
        if forced.contains(&value.reg) || early.contains(&value.reg) {
            assignment.spill(value.reg, value.class);
            continue;
        }
        assert!(
            !env.order(value.class).is_empty(),
            "a value in class {}, which the target hands out no registers from",
            value.class.number()
        );
        let chosen = state
            .coalesced(value, &reused[number])
            .or_else(|| state.hinted(value, &hints[number], Want::Clear))
            .or_else(|| {
                let partners = passed[number].iter().chain(&received[number]);
                let partners: Vec<PhysReg> =
                    partners.filter_map(|&other| state.reg_of(other)).collect();
                state.hinted(value, &partners, Want::Clear)
            })
            .or_else(|| {
                let mut ties = reused[number].clone();
                let source = reuses[number].and_then(|reuse| reuse.source.number());
                ties.extend(source.and_then(|source| usize::try_from(source).ok()));
                ties.retain(|&tie| state.at[tie].is_none() && values[tie].is_some());
                let tied = ties.iter().flat_map(|&tie| hints[tie].iter());
                let wanted: Vec<PhysReg> = tied.chain(env.order(value.class)).copied().collect();
                state.together(value, &ties, &wanted)
            })
            .or_else(|| state.hinted(value, env.order(value.class), Want::Clear))
            .or_else(|| state.hinted(value, env.order(value.class), Want::Allowed));
        if state.work > state.budget {
            return None;
        }
        if let Some(at) = chosen {
            state.take(value, at);
            continue;
        }
        match state.cheapest(value, env.order(value.class)) {
            Some(at) => {
                for other in state.evict(value, at) {
                    lost[other] += 1;
                    let Some(evicted) = values[other] else { continue };
                    if lost[other] > ROUNDS {
                        assignment.spill(evicted.reg, evicted.class);
                    } else {
                        queue.push((evicted.size, Reverse(other)));
                    }
                }
                state.take(value, at);
            }
            None => assignment.spill(value.reg, value.class),
        }
        if state.work > state.budget {
            return None;
        }
    }
    state.settle(&reused, &passed, &received);
    if state.work > state.budget {
        return None;
    }
    for (number, at) in state.at.iter().enumerate() {
        let Some(at) = *at else { continue };
        let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
        assignment.put(reg, Place::Reg(at));
    }
    for inst in state.commuted.iter().flatten() {
        assignment.commute(*inst);
    }
    Some(assignment)
}

/// What the allocation knows while it runs.
struct State<'a, 'v> {
    live: &'a Live,
    blocked: &'a Blocks,
    reuses: &'a [Option<Reuse>],
    values: &'v [Option<Value<'a>>],
    /// Which values are in each register of each class, by number.
    held: Vec<((RegClass, PhysReg), Vec<usize>)>,
    /// Which register each value is in now, if one.
    at: Vec<Option<PhysReg>>,
    /// The instruction whose sources were swapped for the answer written by each value, if its
    /// register is the second source's.
    commuted: Vec<Option<Inst>>,
    work: u64,
    budget: u64,
}

impl<'a> State<'a, '_> {
    fn reg_of(&self, reg: Reg) -> Option<PhysReg> {
        self.at.get(usize::try_from(reg.number()?).ok()?).copied().flatten()
    }

    fn slot(&mut self, class: RegClass, at: PhysReg) -> &mut Vec<usize> {
        let found = self.held.iter().position(|(key, _)| *key == (class, at));
        let index = found.unwrap_or_else(|| {
            self.held.push(((class, at), Vec::new()));
            self.held.len() - 1
        });
        &mut self.held[index].1
    }

    /// The values in `at` that are wanted while `value` is, leaving out the one a two address
    /// instruction lets it share the register with.
    fn clashes(&mut self, value: Value<'_>, at: PhysReg) -> Vec<usize> {
        let Some(index) = self.held.iter().position(|(key, _)| *key == (value.class, at)) else {
            return Vec::new();
        };
        let mut clashes = Vec::new();
        for &other in &self.held[index].1 {
            let Some(held) = self.values[other] else { continue };
            self.work += 1;
            if !held.range.overlaps(value.range) || !held.area.overlaps(value.area) {
                continue;
            }
            if !self.shares(value.reg, held.reg) {
                clashes.push(other);
            }
        }
        clashes
    }

    /// Whether two values that are both wanted at one instruction may still be in one register,
    /// which is when that instruction reads one for the last time and writes the other over it.
    fn shares(&self, one: Reg, two: Reg) -> bool {
        let reads = |answer: Reg, source: Reg| {
            let Some(number) = answer.number().and_then(|n| usize::try_from(n).ok()) else {
                return false;
            };
            let Some(reuse) = self.reuses[number] else { return false };
            match self.commuted[number] {
                Some(_) => reuse.second == Some(source),
                None => reuse.source == source,
            }
        };
        (reads(one, two) || reads(two, one)) && assign::apart(self.live, one, two)
    }

    fn free(&mut self, value: Value<'_>, at: PhysReg, want: Want) -> bool {
        !self.blocked.insists(value.reg, value.class, value.area, value.range, at, want)
            && self.clashes(value, at).is_empty()
    }

    /// The first register of `wanted` that is free for `value`.
    fn hinted(&mut self, value: Value<'_>, wanted: &[PhysReg], want: Want) -> Option<PhysReg> {
        wanted.iter().copied().find(|&at| self.work <= self.budget && self.free(value, at, want))
    }

    /// The first register of `order` free for `value` and for each value in `ties`, which are the
    /// values a two address instruction ties it to that have no register yet. Taking one of these
    /// leaves the tied value a register to follow it into when its turn comes. The registers the
    /// tied values are hinted to come first in `order`, so that a parameter's answer waits for it in
    /// the register the parameter arrives in.
    fn together(&mut self, value: Value<'_>, ties: &[usize], order: &[PhysReg]) -> Option<PhysReg> {
        if ties.is_empty() {
            return None;
        }
        for &at in order {
            if self.work > self.budget {
                return None;
            }
            if !self.free(value, at, Want::Clear) {
                continue;
            }
            let values = self.values;
            let mut tied = ties.iter().filter_map(|&tie| values[tie]);
            if tied.all(|other| self.free(other, at, Want::Allowed)) {
                return Some(at);
            }
        }
        None
    }

    /// The register of a value a two address instruction ties this one to, if it can have it.
    ///
    /// Asked of the answer, it is the register of the source, or of the second source if the
    /// instruction reads its sources either way round. Asked of a source, it is the register of an
    /// answer that reuses it and was placed first.
    fn coalesced(&mut self, value: Value<'_>, answers: &[usize]) -> Option<PhysReg> {
        let number = index(value.reg);
        if let Some(reuse) = self.reuses[number] {
            if let Some(at) = self.reg_of(reuse.source) {
                if self.free(value, at, Want::Allowed) {
                    return Some(at);
                }
            }
            if let Some(second) = reuse.second {
                if let Some(at) = self.reg_of(second) {
                    self.commuted[number] = Some(reuse.inst);
                    if self.free(value, at, Want::Allowed) {
                        return Some(at);
                    }
                    self.commuted[number] = None;
                }
            }
        }
        for &answer in answers {
            let Some(at) = self.at[answer] else { continue };
            if self.commuted[answer].is_none() && self.free(value, at, Want::Allowed) {
                return Some(at);
            }
        }
        None
    }

    /// The register that is cheapest to take back for `value`, if any is cheaper than sending
    /// `value` to the stack.
    fn cheapest(&mut self, value: Value<'_>, order: &[PhysReg]) -> Option<PhysReg> {
        let mut best: Option<(u128, u128, PhysReg)> = None;
        for &at in order {
            if self.blocked.insists(
                value.reg,
                value.class,
                value.area,
                value.range,
                at,
                Want::Allowed,
            ) {
                continue;
            }
            let clashes = self.clashes(value, at);
            let weights = clashes.iter().filter_map(|&other| self.values[other]).map(|v| v.weight);
            let (heaviest, total) = weights.fold((0, 0), |(most, sum), w| (most.max(w), sum + w));
            if heaviest >= value.weight {
                continue;
            }
            if best.is_none_or(|(most, sum, _)| (heaviest, total) < (most, sum)) {
                best = Some((heaviest, total, at));
            }
        }
        best.map(|(_, _, at)| at)
    }

    /// Takes `at` back from the values in it that are in the way of `value`, and says which.
    fn evict(&mut self, value: Value<'_>, at: PhysReg) -> Vec<usize> {
        let clashes = self.clashes(value, at);
        self.slot(value.class, at).retain(|other| !clashes.contains(other));
        for &other in &clashes {
            self.at[other] = None;
            self.commuted[other] = None;
        }
        clashes
    }

    /// Moves each value into the register the most values tied to it are in, of those that are free
    /// for it and hold more of them than where it is now.
    ///
    /// A tie is a two address instruction or a block parameter, and each one met is a copy the
    /// rewrite does not have to write. Placing values in priority order can leave the two ends of a
    /// tie apart though the register one of them is in stayed free for the other, because the other
    /// was placed first and had nothing yet to follow. Every move meets more ties than it breaks, so
    /// this ends.
    fn settle(&mut self, reused: &[Vec<usize>], passed: &[Vec<Reg>], received: &[Vec<Reg>]) {
        let mut moved = true;
        while moved && self.work <= self.budget {
            moved = false;
            for number in 0..self.at.len() {
                let (Some(value), Some(now)) = (self.values[number], self.at[number]) else {
                    continue;
                };
                let reuse = self.reuses[number];
                let source = reuse.and_then(|reuse| self.reg_of(reuse.source));
                let second =
                    reuse.and_then(|reuse| reuse.second).and_then(|second| self.reg_of(second));
                let mut wanted: Vec<PhysReg> = source.into_iter().chain(second).collect();
                for &answer in &reused[number] {
                    if self.commuted[answer].is_none() {
                        wanted.extend(self.at[answer]);
                    }
                }
                let partners = passed[number].iter().chain(&received[number]);
                wanted.extend(partners.filter_map(|&other| self.reg_of(other)));
                let met = |at: PhysReg| wanted.iter().filter(|&&reg| reg == at).count();
                let here = met(now);
                let mut better: Vec<(usize, PhysReg)> = Vec::new();
                for &at in &wanted {
                    let count = met(at);
                    if count > here && !better.contains(&(count, at)) {
                        better.push((count, at));
                    }
                }
                if better.is_empty() {
                    continue;
                }
                better.sort_by_key(|&(count, _)| Reverse(count));
                self.slot(value.class, now).retain(|&other| other != number);
                self.at[number] = None;
                let was = self.commuted[number];
                let mut to = now;
                for (_, at) in better {
                    self.commuted[number] = match reuse {
                        Some(reuse) if second == Some(at) && source != Some(at) => Some(reuse.inst),
                        _ => None,
                    };
                    if self.free(value, at, Want::Allowed) {
                        to = at;
                        moved = true;
                        break;
                    }
                }
                if to == now {
                    self.commuted[number] = was;
                }
                self.take(value, to);
            }
        }
    }

    fn take(&mut self, value: Value<'_>, at: PhysReg) {
        let number = index(value.reg);
        self.at[number] = Some(at);
        self.slot(value.class, at).push(number);
    }
}

/// How much of the line a value is live over.
pub(crate) fn size(area: Area<'_>) -> u32 {
    area.pieces().map(|piece| piece.end - piece.start + 1).sum()
}

/// How often each value is read or written, each time counted by how often its block runs.
///
/// A block that says nothing about how often it runs counts as running once, so a function with no
/// weights on it is counted by how many times each value is named.
pub(crate) fn costs(func: &Func) -> Vec<u128> {
    let mut costs = vec![0u128; func.vregs()];
    let mut add = |reg: Reg, weight: u128| {
        let number = reg.number().and_then(|number| usize::try_from(number).ok());
        if let Some(cost) = number.and_then(|number| costs.get_mut(number)) {
            *cost += weight;
        }
    };
    for block in func.blocks() {
        let weight = u128::from(func[block].weight.raw().max(1));
        for param in &func[block].params {
            add(param.reg, weight);
        }
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                add(operand.reg, weight);
            }
        }
        for call in &func[block].succs {
            for &arg in &call.args {
                add(arg, weight);
            }
        }
    }
    costs
}

/// For each parameter of a block, the values the edges into it pass it.
fn received(func: &Func) -> Vec<Vec<Reg>> {
    let mut received = vec![Vec::new(); func.vregs()];
    for block in func.blocks() {
        for call in &func[block].succs {
            for (&arg, param) in call.args.iter().zip(&func[call.block].params) {
                let number = param.reg.number().and_then(|number| usize::try_from(number).ok());
                let Some(from) = number.and_then(|number| received.get_mut(number)) else {
                    continue;
                };
                if !from.contains(&arg) {
                    from.push(arg);
                }
            }
        }
    }
    received
}

/// For each value, the answers of two address instructions that reuse it as their first source.
fn reused(reuses: &[Option<Reuse>]) -> Vec<Vec<usize>> {
    let mut reused = vec![Vec::new(); reuses.len()];
    for (answer, reuse) in reuses.iter().enumerate() {
        let Some(reuse) = reuse else { continue };
        let number = reuse.source.number().and_then(|number| usize::try_from(number).ok());
        if let Some(answers) = number.and_then(|number| reused.get_mut(number)) {
            answers.push(answer);
        }
    }
    reused
}

fn index(reg: Reg) -> usize {
    usize::try_from(reg.number().expect("a virtual register")).expect("a register number")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Constraint, Flags, Opcode, Operand, Param};
    use rucc_target::x86_64::{GPR, REGS, SYSV};

    use super::*;
    use crate::check;

    fn env() -> Env {
        let (order, scratch) = SYSV.int_order.split_at(SYSV.int_order.len() - 3);
        Env::new().with(GPR, order, scratch)
    }

    fn narrow(count: usize) -> Env {
        Env::new().with(GPR, &SYSV.int_order[..count], &SYSV.int_order[count..count + 1])
    }

    fn named(place: Option<Place>) -> String {
        match place {
            Some(Place::Reg(reg)) => REGS.name(GPR, reg).expect("a register").to_string(),
            Some(Place::Slot(_)) => "slot".to_string(),
            None => "nowhere".to_string(),
        }
    }

    /// Where every value went, after asking the checker whether the machine could run it.
    fn places(func: &mut Func, env: &Env) -> Vec<String> {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let assignment = within(func, &order, &live, env, BUDGET).expect("inside the budget");
        // The sources of an instruction that commutes are swapped before the check, as `run_with`
        // swaps them.
        for &inst in assignment.commuted() {
            let list = func[inst].operands;
            func[list].swap(1, 2);
        }
        let problems = check::check(func, &order, &live, &assignment);
        assert!(problems.is_empty(), "{}", check::report(&problems));
        (0..func.vregs())
            .map(|number| {
                let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
                named(assignment.place(reg))
            })
            .collect()
    }

    fn linear(func: &Func, env: &Env) -> Vec<String> {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let assignment = assign::assign(func, &order, &live, env);
        (0..func.vregs())
            .map(|number| {
                let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
                named(assignment.place(reg))
            })
            .collect()
    }

    #[test]
    fn a_value_the_spill_phase_picked_goes_to_the_stack_and_the_rest_fit() {
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

        let env = narrow(2);
        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let pressure = Pressure::of(&func, &order, &live, &env);
        let early = spill::choose(&func, &live, &pressure);
        assert_eq!(early, [once]);
        let assignment = placed(&func, &order, &live, &env, BUDGET, &early).expect("in budget");
        let problems = check::check(&func, &order, &live, &assignment);
        assert!(problems.is_empty(), "{}", check::report(&problems));
        assert_eq!(named(assignment.place(once)), "slot");
        assert_eq!(assignment.spilled(), 1);
    }

    #[test]
    fn two_values_that_are_never_both_wanted_share_a_register() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).uses(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(second, GPR).finish();

        assert_eq!(places(&mut func, &env()), ["rax", "rax"]);
    }

    #[test]
    fn the_value_read_most_often_keeps_its_register_though_it_is_wanted_longest() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let busy = func.new_vreg(GPR);
        let once = func.new_vreg(GPR);
        func.build(block, opcode).def(busy, GPR).finish();
        func.build(block, opcode).def(once, GPR).finish();
        for _ in 0..6 {
            func.build(block, opcode).uses(busy, GPR).finish();
        }
        func.build(block, opcode).uses(once, GPR).finish();
        func.build(block, opcode).uses(busy, GPR).finish();

        // With one register the linear scan sends the value that ends last to the stack, which is
        // the one read seven times. Weighing them keeps that one and sends the one read once.
        assert_eq!(linear(&func, &narrow(1)), ["slot", "rax"]);
        assert_eq!(places(&mut func, &narrow(1)), ["rax", "slot"]);
    }

    #[test]
    fn a_value_read_in_a_loop_keeps_its_register_over_one_read_outside_it() {
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
        func.set_weight(body, rucc_mir::Weight::parts(100 * rucc_mir::Weight::SCALE));
        func.build(out, opcode).uses(cold, GPR).finish();
        func.build(out, opcode).uses(step, GPR).finish();

        // Read once in the loop and once after it, against a value read once after it: the count
        // of reads is the same and it is how often the loop runs that decides.
        assert_eq!(places(&mut func, &narrow(1)), ["rax", "slot"]);
    }

    #[test]
    fn a_long_value_gives_its_register_back_to_a_short_busy_one() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let long = func.new_vreg(GPR);
        let short = func.new_vreg(GPR);
        func.build(block, opcode).def(long, GPR).finish();
        for _ in 0..4 {
            func.build(block, opcode).finish();
        }
        func.build(block, opcode).def(short, GPR).finish();
        for _ in 0..4 {
            func.build(block, opcode).uses(short, GPR).finish();
        }
        func.build(block, opcode).uses(long, GPR).finish();

        // The long one is placed first, since it is the harder to place, and then loses the one
        // register to the short one, which weighs more, and has nowhere else to go.
        assert_eq!(places(&mut func, &narrow(1)), ["slot", "rax"]);
    }

    #[test]
    fn the_answer_of_a_two_address_instruction_goes_where_its_source_ends() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode).def(right, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(right, GPR).finish();
        func.build(block, opcode).uses(sum, GPR).finish();

        let places = places(&mut func, &env());
        assert_eq!(places[2], places[0]);
        assert_ne!(places[1], places[0]);
    }

    #[test]
    fn a_source_placed_after_its_answer_goes_where_the_answer_is() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .finish();
        for _ in 0..6 {
            func.build(block, opcode).uses(sum, GPR).finish();
        }

        // The answer is the longer of the two, so it is placed first, and the source finds it.
        let places = places(&mut func, &env());
        assert_eq!(places[0], places[1]);
    }

    #[test]
    fn an_answer_that_commutes_stays_where_the_loop_passes_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let head = func.create_block();
        let out = func.create_block();
        let seed = func.new_vreg(GPR);
        let total = func.new_vreg(GPR);
        let term = func.new_vreg(GPR);
        let next = func.new_vreg(GPR);
        func.build(entry, opcode).def(seed, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::with(head, vec![seed])];
        func.params_mut(head).push(Param { reg: total, class: GPR });
        func.build(head, opcode).def(term, GPR).finish();
        func.build(head, opcode)
            .flags(Flags::COMMUTES)
            .operand(Operand::write(next, GPR).with(Constraint::Reuse(1)))
            .uses(total, GPR)
            .uses(term, GPR)
            .finish();
        *func.succs_mut(head) = vec![BlockCall::with(head, vec![next]), BlockCall::to(out)];

        let places = places(&mut func, &env());
        assert_eq!(places[3], places[1]);
    }

    #[test]
    fn an_answer_whose_first_source_lives_on_goes_where_the_second_one_ends() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let base = func.new_vreg(GPR);
        let entry = func.new_vreg(GPR);
        let target = func.new_vreg(GPR);
        func.build(block, opcode).def(base, GPR).finish();
        func.build(block, opcode).def(entry, GPR).finish();
        func.build(block, opcode)
            .flags(Flags::COMMUTES)
            .operand(Operand::write(target, GPR).with(Constraint::Reuse(1)))
            .uses(base, GPR)
            .uses(entry, GPR)
            .finish();
        func.build(block, opcode).uses(target, GPR).finish();
        func.build(block, opcode).uses(base, GPR).finish();

        let places = places(&mut func, &env());
        assert_eq!(places[2], places[1]);
        assert_ne!(places[2], places[0]);
    }

    #[test]
    fn a_register_an_instruction_insists_on_is_left_to_the_value_it_names() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let kept = func.new_vreg(GPR);
        let passed = func.new_vreg(GPR);
        func.build(block, opcode).def(kept, GPR).finish();
        func.build(block, opcode).def(passed, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::read(passed, GPR).with(Constraint::Fixed(rucc_target::x86_64::RAX)))
            .finish();
        func.build(block, opcode).uses(kept, GPR).finish();

        let places = places(&mut func, &env());
        assert_eq!(places[1], "rax");
        assert_ne!(places[0], "rax");
    }

    #[test]
    fn a_value_only_memory_can_hold_goes_to_the_stack() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(block, opcode).def(value, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::read(value, GPR).with(Constraint::Stack))
            .finish();

        assert_eq!(places(&mut func, &env()), ["slot"]);
    }

    #[test]
    fn a_function_over_the_budget_gets_the_linear_scan_answer() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let busy = func.new_vreg(GPR);
        let once = func.new_vreg(GPR);
        func.build(block, opcode).def(busy, GPR).finish();
        func.build(block, opcode).def(once, GPR).finish();
        for _ in 0..6 {
            func.build(block, opcode).uses(busy, GPR).finish();
        }
        func.build(block, opcode).uses(once, GPR).finish();
        func.build(block, opcode).uses(busy, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        assert!(within(&func, &order, &live, &narrow(1), 0).is_none());
        let fallen = linear(&func, &narrow(1));
        assert_eq!(fallen, ["slot", "rax"]);
    }

    #[test]
    fn the_cheaper_of_the_two_answers_is_the_one_kept() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let busy = func.new_vreg(GPR);
        let once = func.new_vreg(GPR);
        func.build(block, opcode).def(busy, GPR).finish();
        func.build(block, opcode).def(once, GPR).finish();
        for _ in 0..6 {
            func.build(block, opcode).uses(busy, GPR).finish();
        }
        func.build(block, opcode).uses(once, GPR).finish();
        func.build(block, opcode).uses(busy, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let env = narrow(1);
        let linear = assign::assign(&func, &order, &live, &env);
        let ours = within(&func, &order, &live, &env, BUDGET).expect("inside the budget");
        // The linear scan puts `busy` on the stack, which is its write and its seven reads. This
        // puts `once` there, which is one write and one read.
        let once_through = u128::from(func[block].weight.raw());
        assert_eq!(cost(&func, &order, &linear), 8 * once_through);
        assert_eq!(cost(&func, &order, &ours), 2 * once_through);
        let kept = assign(&func, &order, &live, &env);
        assert_eq!(kept.place(busy), ours.place(busy));
        assert_eq!(kept.place(once), ours.place(once));
    }

    #[test]
    fn a_copy_left_between_a_two_address_answer_and_its_source_is_counted() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode).def(right, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(sum, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let chosen = assign(&func, &order, &live, &env());
        assert_eq!(cost(&func, &order, &chosen), 0);
        let mut apart = chosen.clone();
        apart.put(sum, chosen.place(right).expect("a place for right"));
        assert_eq!(cost(&func, &order, &apart), u128::from(func[block].weight.raw()));
    }

    #[test]
    fn many_values_in_few_registers_come_out_as_something_the_machine_can_run() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let regs: Vec<Reg> = (0..24).map(|_| func.new_vreg(GPR)).collect();
        for &reg in &regs {
            func.build(block, opcode).def(reg, GPR).finish();
        }
        for (at, &reg) in regs.iter().enumerate().rev() {
            for _ in 0..(at % 5) {
                func.build(block, opcode).uses(reg, GPR).finish();
            }
            func.build(block, opcode).uses(reg, GPR).finish();
        }

        let places = places(&mut func, &narrow(4));
        assert_eq!(places.iter().filter(|place| *place != "slot").count(), 4);
    }
}
