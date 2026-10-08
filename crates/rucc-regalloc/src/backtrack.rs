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
//! a source looks at where the answer that reuses it went as well as the other way round. That
//! goes for the second source of an instruction that reads its sources either way round too: it
//! can follow an answer placed before it by having the sources swapped, when the first source is
//! wanted after the instruction and so can never be where the answer goes. A loaded value added to
//! a base the loop reads again is that case.
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
//! A value sent ahead still gets the offer described next, once every other value has a register.
//! Before it did not, so where the second try won, a value the tuple deforming loop reads in every
//! turn stayed on the stack while a register the `strlen` call destroys sat unused all through the
//! function.
//!
//! # Putting a value away around a call
//!
//! A value wanted on the far side of a call cannot be in a register the call destroys, so it has
//! the callee saved ones or the stack. Where the call is somewhere the loop only goes now and then,
//! like the `strlen` a tuple deforming loop makes for a `cstring` column, every value the loop
//! carries is wanted across it, the callee saved registers run out, and the rest are read from the
//! stack in every turn while the registers the call destroys sit empty. tamnd/rucc#1177.
//!
//! So a value that would go to the stack is first offered a register whose only problem is the
//! instructions that destroy it. It takes the one where that costs least, and the rewrite puts it
//! away in a slot in front of each of those instructions and brings it back behind it. That is a
//! store and a load for each, counted by how often its block runs, and it is only done when that is
//! less than what the value would cost on the stack, which is a load or a store at every read and
//! write. A value read in a hot loop around a cold call is the case it wins, and a value read once
//! around a call in the same loop is the case it does not.
//!
//! It also gives up when it would lose. The linear scan runs as well, which is cheap next to this,
//! and the answer kept is the one with the lower [`cost`]: the loads and stores of the values on
//! the stack and the copies between the ends of each tie left apart, each counted by how often its
//! block runs. Placing the long values first is right where registers are fought over in a loop,
//! and it can be worse in a long straight run of arithmetic, where the order the linear scan walks
//! in is also the order that lets each answer follow its source.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rucc_base::hash::{Map, Set};
use rucc_mir::{Func, Inst, Reg};
use rucc_target::{PhysReg, RegClass};

use crate::assign::{self, Assignment, Blocks, Env, FEW, Pieces, Place, Reuse, Want};
use crate::live::{Area, Live, Range};
use crate::order::{Order, Point};
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
    tries(func, order, live, env, &assign::Facts::of(func, order))
}

/// The same, over what was already read off the function.
pub(crate) fn tries(
    func: &Func,
    order: &Order,
    live: &Live,
    env: &Env,
    facts: &assign::Facts,
) -> Assignment {
    let costs = costs(func);
    let weights = weights(func);
    let linear = assign::scan(func, live, env, facts);
    let mut best = spent(func, &costs, &weights, &facts.reuses, &linear);
    let mut kept = linear;
    let pressure = Pressure::with(func, order, live, env, &facts.forced, &facts.blocked);
    let spilled = spill::with(func, live, &pressure, &costs, &facts.forced);
    // With nothing sent ahead the second try would be the first one again.
    let tries: &[&[Reg]] = if spilled.is_empty() { &[&[]] } else { &[&[], &spilled] };
    for &early in tries {
        let Some(ours) = placed(func, order, live, env, BUDGET, early, facts, &costs) else {
            break;
        };
        let spent = spent(func, &costs, &weights, &facts.reuses, &ours);
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
    spent(func, &costs(func), &weights(func), &assign::reuses(func, order), assignment)
}

/// How often the block of each instruction runs, never less than once.
fn weights(func: &Func) -> Map<Inst, u128> {
    let mut weights = Map::default();
    for block in func.blocks() {
        let weight = u128::from(func[block].weight.raw().max(1));
        for inst in func.insts(block) {
            weights.insert(inst, weight);
        }
    }
    weights
}

/// The same, with what each value costs on the stack, how often each instruction runs and the
/// answers written over a source already read off the function. The weights are the same for
/// every answer, and building them again for each of the three a function is weighed under was
/// a map of every instruction each time. tamnd/rucc#3052.
fn spent(
    func: &Func,
    costs: &[u128],
    weights: &Map<Inst, u128>,
    reuses: &[Option<Reuse>],
    assignment: &Assignment,
) -> u128 {
    let mut total = 0;
    for (reg, place) in assignment.placed() {
        if !matches!(place, Place::Reg(_)) {
            total += costs[index(reg)];
        }
    }
    for block in func.blocks() {
        let weight = u128::from(func[block].weight.raw().max(1));
        for call in &func[block].succs {
            for (&arg, param) in call.args.iter().zip(&func[call.block].params) {
                if assignment.place(arg) != assignment.place(param.reg) {
                    total += weight;
                }
            }
        }
    }
    for save in assignment.saves() {
        total += 2 * weights.get(&save.inst).copied().unwrap_or(1);
    }
    let commuted: Set<Inst> = assignment.commuted().iter().copied().collect();
    for (number, reuse) in reuses.iter().enumerate() {
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
    placed(func, order, live, env, budget, &[], &assign::Facts::of(func, order), &costs(func))
}

#[allow(clippy::too_many_arguments)]
fn placed(
    func: &Func,
    order: &Order,
    live: &Live,
    env: &Env,
    budget: u64,
    early: &[Reg],
    facts: &assign::Facts,
    costs: &[u128],
) -> Option<Assignment> {
    let assign::Facts { blocked, forced, reuses, hints, passed } = facts;
    let received = received(func);
    let reused = reused(reuses);
    let seconds = seconds(reuses);
    let saveable = saveable(func, order);

    let count = func.vregs();
    // By number, since the queue asks about every value it pops and a function with hundreds of
    // values sent ahead searched the list for each of them. tamnd/rucc#3052.
    let mut ahead = vec![false; count];
    for &reg in early {
        ahead[index(reg)] = true;
    }
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
        blocked,
        reuses,
        values: &values,
        held: Vec::new(),
        pieces: Vec::new(),
        at: vec![None; count],
        commuted: vec![None; count],
        saves: vec![Vec::new(); count],
        work: 0,
        budget,
        asked: (None, Vec::new()),
    };
    let mut assignment = Assignment::empty(count);
    let mut lost = vec![0u32; count];
    let mut sent = Vec::new();
    while let Some((_, Reverse(number))) = queue.pop() {
        let Some(value) = values[number] else { continue };
        if forced.contains(&value.reg) {
            assignment.spill(value.reg, value.class);
            continue;
        }
        if ahead[number] {
            sent.push(value);
            continue;
        }
        assert!(
            !env.order(value.class).is_empty(),
            "a value in class {}, which the target hands out no registers from",
            value.class.number()
        );
        // Allowed and not Clear, at each step that asks. A register something insists on only in a
        // hole between this value's pieces costs it nothing, since the value is dead there and the
        // instruction takes the register without a move. Clear is asked over the hull, and it
        // turned a parameter read before an early return that calls away from the register it
        // arrived in and into a saved one, which is a push, a move and a pop nobody needed.
        // Only a register the class hands out. An instruction may insist on one of the scratch
        // registers, which is what an i386 `"S"` operand does with `esi`, and that is a move in
        // front of the instruction. Taking it as a hint put the whole value in the scratch, where
        // the first reload of anything else wrote over it.
        let order = env.order(value.class);
        let handed = |list: &[PhysReg]| -> Vec<PhysReg> {
            list.iter().copied().filter(|at| order.contains(at)).collect()
        };
        let chosen = state
            .coalesced(value, &reused[number])
            .or_else(|| state.hinted(value, &handed(&hints[number]), Want::Allowed))
            .or_else(|| {
                let partners = passed[number].iter().chain(&received[number]);
                let partners: Vec<PhysReg> =
                    partners.filter_map(|&other| state.reg_of(other)).collect();
                state.hinted(value, &partners, Want::Allowed)
            })
            .or_else(|| state.swapped(value, &seconds[number]))
            .or_else(|| {
                let mut ties = reused[number].to_vec();
                let source = reuses[number].and_then(|reuse| reuse.source.number());
                ties.extend(source.and_then(|source| usize::try_from(source).ok()));
                ties.retain(|&tie| state.at[tie].is_none() && values[tie].is_some());
                let tied = ties.iter().flat_map(|&tie| handed(&hints[tie]));
                let wanted: Vec<PhysReg> = tied.chain(order.iter().copied()).collect();
                state.together(value, &ties, &wanted)
            })
            .or_else(|| state.hinted(value, env.order(value.class), Want::Allowed));
        if state.work > state.budget {
            return None;
        }
        if let Some(at) = chosen {
            state.take(value, at);
            continue;
        }
        let mut gone = Vec::new();
        match state.cheapest(value, env.order(value.class)) {
            Some(at) => {
                for other in state.evict(value, at) {
                    lost[other] += 1;
                    let Some(evicted) = values[other] else { continue };
                    if lost[other] > ROUNDS {
                        gone.push(evicted);
                    } else {
                        queue.push((evicted.size, Reverse(other)));
                    }
                }
                state.take(value, at);
            }
            None => gone.push(value),
        }
        // Last, so that a value put away around a call never takes the register the value that
        // evicted it was given.
        for last in gone {
            let spilled = costs[index(last.reg)];
            match state.saved(func, last, env.order(last.class), spilled, &saveable) {
                Some((at, insts)) => {
                    state.take(last, at);
                    state.saves[index(last.reg)] = insts;
                }
                None => assignment.spill(last.reg, last.class),
            }
        }
        if state.work > state.budget {
            return None;
        }
    }
    // A value sent ahead is one the stack was going to take anyway, but the stack is only the
    // cheaper answer where the value is read more often than the calls it is wanted across are
    // made. So once every other value has its register, each is offered one that only a call is in
    // the way of, the most expensive to keep on the stack first, which is what a value that lost
    // its register in the queue is offered too. Nothing is evicted for one here, since what is left
    // is a register nothing else wanted.
    sent.sort_by_key(|value| Reverse(costs[index(value.reg)]));
    for value in sent {
        let spilled = costs[index(value.reg)];
        match state.saved(func, value, env.order(value.class), spilled, &saveable) {
            Some((at, insts)) => {
                state.take(value, at);
                state.saves[index(value.reg)] = insts;
            }
            None => assignment.spill(value.reg, value.class),
        }
    }
    if state.work > state.budget {
        return None;
    }
    state.settle(&reused, passed, &received);
    if state.work > state.budget {
        return None;
    }
    for (number, at) in state.at.iter().enumerate() {
        let Some(at) = *at else { continue };
        let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
        assignment.put(reg, Place::Reg(at));
        if let Some(value) = values[number] {
            assignment.save(reg, value.class, &state.saves[number]);
        }
    }
    for inst in state.commuted.iter().flatten() {
        assignment.commute(*inst);
    }
    Some(assignment)
}

/// A value in a register, by number, with the stretch of the line it covers.
type Held = (usize, Range);

/// What the allocation knows while it runs.
struct State<'a, 'v> {
    live: &'a Live,
    blocked: &'a Blocks,
    reuses: &'a [Option<Reuse>],
    values: &'v [Option<Value<'a>>],
    /// Which values are in each register of each class, by number, each with the stretch of the
    /// line it covers. Most values in a register are nowhere near the one being placed, and having
    /// the stretch here tells so without reading the value itself from wherever it is in `values`.
    held: Vec<((RegClass, PhysReg), Vec<Held>)>,
    /// The pieces of the values in each register of `held`, at the same index, made the first time
    /// a register with more than [`FEW`] values in it is asked about.
    pieces: Vec<Pieces>,
    /// Which register each value is in now, if one.
    at: Vec<Option<PhysReg>>,
    /// The instruction whose sources were swapped for the answer written by each value, if its
    /// register is the second source's.
    commuted: Vec<Option<Inst>>,
    /// The instructions each value is put away around, which is nothing for a value in a register
    /// nothing in its range destroys.
    saves: Vec<Vec<Inst>>,
    work: u64,
    budget: u64,
    /// What [`Blocks::insists`] said about each register for the value it was last asked about, by
    /// the register's number, allowed and then clear. Placing one value asks about the same
    /// registers more than once, for its hints, for its partners' registers and then for every
    /// register in order, and the answer does not change in between.
    asked: (Option<Reg>, Vec<[Option<bool>; 2]>),
}

impl<'a> State<'a, '_> {
    fn reg_of(&self, reg: Reg) -> Option<PhysReg> {
        self.at.get(usize::try_from(reg.number()?).ok()?).copied().flatten()
    }

    fn slot(&mut self, class: RegClass, at: PhysReg) -> usize {
        let found = self.held.iter().position(|(key, _)| *key == (class, at));
        found.unwrap_or_else(|| {
            self.held.push(((class, at), Vec::new()));
            self.pieces.push(Pieces::default());
            self.held.len() - 1
        })
    }

    /// Takes values out of a register, and their pieces with them.
    fn remove(&mut self, index: usize, gone: &[usize]) {
        self.held[index].1.retain(|(other, _)| !gone.contains(other));
        let pieces = &mut self.pieces[index];
        if pieces.kept {
            for value in gone.iter().filter_map(|&other| self.values[other]) {
                for piece in value.area.pieces() {
                    pieces.remove(piece, value.reg);
                }
            }
        }
    }

    /// The values in `at` that are wanted while `value` is, leaving out the one a two address
    /// instruction lets it share the register with.
    fn clashes(&mut self, value: Value<'_>, at: PhysReg) -> Vec<usize> {
        let Some(found) = self.held.iter().position(|(key, _)| *key == (value.class, at)) else {
            return Vec::new();
        };
        let held = &self.held[found].1;
        // Counted as a walk over every value in the register, so that the budget runs out at the
        // same place whichever way the question is answered.
        self.work += held.len() as u64;
        if held.len() > FEW {
            let pieces = &mut self.pieces[found];
            if !pieces.kept {
                pieces.kept = true;
                for other in held.iter().filter_map(|&(other, _)| self.values[other]) {
                    for piece in other.area.pieces() {
                        pieces.insert(piece, other.reg);
                    }
                }
            }
            if let Some(owners) = pieces.owners(value.area) {
                let clash = owners.into_iter().filter(|&other| !self.shares(value.reg, other));
                return clash.map(index).collect();
            }
        }
        let mut clashes = Vec::new();
        for &(other, range) in held {
            if !range.overlaps(value.range) {
                continue;
            }
            let Some(held) = self.values[other] else { continue };
            if !held.area.overlaps(value.area) {
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
        !self.insists(value, at, want) && self.clashes(value, at).is_empty()
    }

    /// Whether an instruction insists on `at` where `value` would be in its way, from what was
    /// worked out the last time this was asked if it was asked about this value already. A
    /// register clear for a value is allowed for it too, and one not allowed is not clear either.
    fn insists(&mut self, value: Value<'_>, at: PhysReg, want: Want) -> bool {
        let (asked, answers) = &mut self.asked;
        if *asked != Some(value.reg) {
            *asked = Some(value.reg);
            answers.fill([None; 2]);
        }
        let number = usize::from(at.number());
        if answers.len() <= number {
            answers.resize(number + 1, [None; 2]);
        }
        let slot = usize::from(want == Want::Clear);
        if let Some(answer) = answers[number][slot] {
            return answer;
        }
        let answer =
            self.blocked.insists(value.reg, value.class, value.area, value.range, at, want);
        let known = &mut answers[number];
        known[slot] = Some(answer);
        match (want, answer) {
            (Want::Clear, false) => known[0] = Some(false),
            (Want::Allowed, true) => known[1] = Some(true),
            _ => {}
        }
        answer
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

    /// The register of an answer placed first that could be written over this value with its
    /// sources swapped, and cannot be written over its first source, because that one is still
    /// wanted after it. Asked after the hints, because the register an instruction wants the value
    /// in saves a move as well, and following the answer would take the value away from it. The
    /// value is one no two address instruction writes, such as a load.
    fn swapped(&mut self, value: Value<'_>, seconds: &[usize]) -> Option<PhysReg> {
        // A value that is itself the answer of a two address instruction is tied to its own source
        // already, and following the answer it is read by would break that tie to save the same
        // copy somewhere else.
        if self.reuses[index(value.reg)].is_some() {
            return None;
        }
        for &answer in seconds {
            let (Some(at), Some(reuse)) = (self.at[answer], self.reuses[answer]) else { continue };
            let Some(written) = self.values[answer] else { continue };
            // An answer whose first source ends where it starts can still go over that one, which
            // saves the same copy without swapping anything, and taking its register here would
            // stop it moving there when the function is settled.
            if self.commuted[answer].is_some()
                || self.reg_of(reuse.source) == Some(at)
                || assign::apart(self.live, written.reg, reuse.source)
            {
                continue;
            }
            self.commuted[answer] = Some(reuse.inst);
            if self.free(value, at, Want::Allowed) {
                return Some(at);
            }
            self.commuted[answer] = None;
        }
        None
    }

    /// The register that is cheapest to take back for `value`, if any is cheaper than sending
    /// `value` to the stack.
    fn cheapest(&mut self, value: Value<'_>, order: &[PhysReg]) -> Option<PhysReg> {
        let mut best: Option<(u128, u128, PhysReg)> = None;
        for &at in order {
            if self.insists(value, at, Want::Allowed) {
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
        let index = self.slot(value.class, at);
        self.remove(index, &clashes);
        for &other in &clashes {
            self.at[other] = None;
            self.commuted[other] = None;
            self.saves[other].clear();
        }
        clashes
    }

    /// The register that is cheapest for `value` to be put away in around every instruction that
    /// destroys it, if one costs less than `spilled`, with those instructions.
    ///
    /// Only a register nothing else is in and nothing insists on for anything but destroying it.
    /// The value itself is never written by one of the instructions, since then there is nothing to
    /// put away in front of it.
    fn saved(
        &mut self,
        func: &Func,
        value: Value<'_>,
        order: &[PhysReg],
        spilled: u128,
        saveable: &Map<Point, (Inst, u128)>,
    ) -> Option<(PhysReg, Vec<Inst>)> {
        let mut best: Option<(u128, PhysReg, Vec<Inst>)> = None;
        for &at in order {
            if self.work > self.budget {
                return None;
            }
            let found = self.blocked.destroyed(
                value.reg,
                value.class,
                value.area,
                value.range,
                at,
                |point| saveable.contains_key(&point),
            );
            let Some(points) = found else { continue };
            let mut insts = Vec::new();
            let mut spent = Some(0u128);
            for point in &points {
                let Some(&(inst, weight)) = saveable.get(point) else { continue };
                let written = func[func[inst].operands]
                    .iter()
                    .any(|operand| operand.reg == value.reg && operand.role.is_def());
                spent = spent.filter(|_| !written).map(|spent| spent + 2 * weight);
                insts.push(inst);
            }
            let Some(spent) = spent else { continue };
            if spent >= spilled || best.as_ref().is_some_and(|&(least, _, _)| least <= spent) {
                continue;
            }
            if self.clashes(value, at).is_empty() {
                best = Some((spent, at, insts));
            }
        }
        best.map(|(_, at, insts)| (at, insts))
    }

    /// Moves each value into the register the most values tied to it are in, of those that are free
    /// for it and hold more of them than where it is now.
    ///
    /// A tie is a two address instruction or a block parameter, and each one met is a copy the
    /// rewrite does not have to write. Placing values in priority order can leave the two ends of a
    /// tie apart though the register one of them is in stayed free for the other, because the other
    /// was placed first and had nothing yet to follow. Every move meets more ties than it breaks, so
    /// this ends.
    fn settle(&mut self, reused: &Answers, passed: &[Vec<Reg>], received: &[Vec<Reg>]) {
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
                let index = self.slot(value.class, now);
                self.remove(index, &[number]);
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
                } else {
                    self.saves[number].clear();
                }
                self.take(value, to);
            }
        }
    }

    fn take(&mut self, value: Value<'_>, at: PhysReg) {
        let number = index(value.reg);
        self.at[number] = Some(at);
        let index = self.slot(value.class, at);
        self.held[index].1.push((number, value.range));
        let pieces = &mut self.pieces[index];
        if pieces.kept {
            for piece in value.area.pieces() {
                pieces.insert(piece, value.reg);
            }
        }
    }
}

/// The late point of every instruction a value may be put away around, with the instruction and how
/// often its block runs.
///
/// Every instruction but the last of a block that leaves more than one way, since what goes behind
/// that one has to go at the start of each block it leaves to and a value brought back there is
/// brought back on edges it may not be live on.
fn saveable(func: &Func, order: &Order) -> Map<Point, (Inst, u128)> {
    let mut saveable = Map::default();
    for block in func.blocks() {
        let weight = u128::from(func[block].weight.raw().max(1));
        let last = if func[block].succs.len() > 1 { func.insts(block).last() } else { None };
        for inst in func.insts(block) {
            if Some(inst) != last {
                saveable.insert(order.late(inst), (inst, weight));
            }
        }
    }
    saveable
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

/// Lists of answers by value, in one buffer, with where each value's list starts in it.
///
/// Most values have no list at all, and a list of its own for each one that does was one
/// allocation per value for every function the allocator placed.
struct Answers {
    starts: Vec<usize>,
    all: Vec<usize>,
}

impl std::ops::Index<usize> for Answers {
    type Output = [usize];

    fn index(&self, number: usize) -> &[usize] {
        &self.all[self.starts[number]..self.starts[number + 1]]
    }
}

/// For each value, the answers of the instructions whose reuse `of` names it, in order.
fn answers(reuses: &[Option<Reuse>], of: impl Fn(Reuse) -> Option<Reg>) -> Answers {
    let number = |reuse: &Option<Reuse>| {
        let reg = reuse.and_then(&of)?;
        let number = usize::try_from(reg.number()?).ok()?;
        (number < reuses.len()).then_some(number)
    };
    // Counted first, then each count turned into where its list ends, and the answers put in from
    // the back so that each end comes down to where its list starts.
    let mut starts = vec![0; reuses.len() + 1];
    for reuse in reuses {
        if let Some(number) = number(reuse) {
            starts[number] += 1;
        }
    }
    let mut total = 0;
    for start in &mut starts {
        total += *start;
        *start = total;
    }
    let mut all = vec![0; total];
    for (answer, reuse) in reuses.iter().enumerate().rev() {
        if let Some(number) = number(reuse) {
            starts[number] -= 1;
            all[starts[number]] = answer;
        }
    }
    Answers { starts, all }
}

/// For each value, the answers of two address instructions that reuse it as their first source.
fn reused(reuses: &[Option<Reuse>]) -> Answers {
    answers(reuses, |reuse| Some(reuse.source))
}

/// The answers that could be written over each value as the second source of an instruction that
/// reads its sources either way round.
fn seconds(reuses: &[Option<Reuse>]) -> Answers {
    answers(reuses, |reuse| reuse.second)
}

fn index(reg: Reg) -> usize {
    usize::try_from(reg.number().expect("a virtual register")).expect("a register number")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Constraint, Flags, Opcode, Operand, Param};
    use rucc_target::x86_64::{GPR, RAX, RCX, REGS, SYSV};

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
        let facts = assign::Facts::of(&func, &order);
        let assignment = placed(&func, &order, &live, &env, BUDGET, &early, &facts, &costs(&func))
            .expect("in budget");
        let problems = check::check(&func, &order, &live, &assignment);
        assert!(problems.is_empty(), "{}", check::report(&problems));
        assert_eq!(named(assignment.place(once)), "slot");
        assert_eq!(assignment.spilled(), 1);
    }

    /// A value sent ahead is put away around a call the loop seldom makes, as one that lost its
    /// register in the queue is, rather than read from the stack in every turn.
    #[test]
    fn a_value_sent_ahead_is_put_away_around_the_call_the_loop_seldom_makes() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let [entry, head, cold, skip, latch, back, out] = [(); 7].map(|()| func.create_block());
        let step = func.new_vreg(GPR);
        func.build(entry, opcode).def(step, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(head)];
        func.build(head, opcode).uses(step, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(cold), BlockCall::to(skip)];
        // A call, as far as the allocator can tell: both registers it hands out are destroyed.
        let call = func
            .build(cold, opcode)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .operand(Operand::write(Reg::physical(RCX), GPR))
            .finish();
        *func.succs_mut(cold) = vec![BlockCall::to(latch)];
        *func.succs_mut(skip) = vec![BlockCall::to(latch)];
        func.build(latch, opcode).uses(step, GPR).finish();
        *func.succs_mut(latch) = vec![BlockCall::to(back), BlockCall::to(out)];
        *func.succs_mut(back) = vec![BlockCall::to(head)];
        func.build(out, opcode).uses(step, GPR).finish();
        for (block, often) in [(head, 100), (skip, 99), (latch, 100), (back, 99)] {
            func.set_weight(block, rucc_mir::Weight::parts(often * rucc_mir::Weight::SCALE));
        }

        let env = narrow(2);
        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let facts = assign::Facts::of(&func, &order);
        let assignment = placed(&func, &order, &live, &env, BUDGET, &[step], &facts, &costs(&func))
            .expect("in budget");
        let problems = check::check(&func, &order, &live, &assignment);
        assert!(problems.is_empty(), "{}", check::report(&problems));
        assert_ne!(named(assignment.place(step)), "slot");
        let saves = assignment.saves();
        assert_eq!(saves.len(), 1);
        assert_eq!((saves[0].reg, saves[0].inst), (step, call));
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
    fn an_offset_added_to_a_base_the_loop_reads_again_goes_where_the_answer_went() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let head = func.create_block();
        let out = func.create_block();
        let base = func.new_vreg(GPR);
        let at = func.new_vreg(GPR);
        let offset = func.new_vreg(GPR);
        let target = func.new_vreg(GPR);
        let later = func.new_vreg(GPR);
        func.build(entry, opcode).def(base, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(head)];
        func.build(head, opcode).def(at, GPR).finish();
        func.build(head, opcode).def(offset, GPR).uses(base, GPR).uses(at, GPR).finish();
        func.build(head, opcode)
            .flags(Flags::COMMUTES)
            .operand(Operand::write(target, GPR).with(Constraint::Reuse(1)))
            .uses(base, GPR)
            .uses(offset, GPR)
            .finish();
        func.build(head, opcode).def(later, GPR).finish();
        func.build(head, opcode).uses(target, GPR).finish();
        for _ in 0..4 {
            func.build(head, opcode).finish();
        }
        func.build(head, opcode).uses(later, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(head), BlockCall::to(out)];

        // The answer is placed before the offset, which is wanted over less of the line, and the
        // base the loop reads again has the only register the answer could have followed. The
        // offset is placed last and finds the register `later` has after the add free before it,
        // which is where it went before it looked at the answer, and the answer could not then be
        // moved to it. tamnd/rucc#2064.
        let places = places(&mut func, &env());
        assert_eq!(places[3], places[2]);
        assert_ne!(places[3], places[0]);
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
            .operand(Operand::read(passed, GPR).with(Constraint::Fixed(RAX)))
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

    /// A value written, then `calls` instructions that destroy both registers there are, then
    /// `reads` reads of the value.
    fn around_calls(calls: usize, reads: usize) -> Vec<String> {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(block, opcode).def(value, GPR).finish();
        for _ in 0..calls {
            func.build(block, opcode)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .operand(Operand::write(Reg::physical(RCX), GPR))
                .finish();
        }
        for _ in 0..reads {
            func.build(block, opcode).uses(value, GPR).finish();
        }
        places(&mut func, &narrow(2))
    }

    #[test]
    fn a_value_is_put_away_around_a_call_only_when_that_is_cheaper_than_the_stack() {
        // One call and ten reads is a store and a load against ten loads.
        assert_eq!(around_calls(1, 10), ["rax"]);
        // Three calls and one read is six against a store and a load.
        assert_eq!(around_calls(3, 1), ["slot"]);
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
