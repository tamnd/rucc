//! Which register each value lives in, and which values live on the stack instead.
//!
//! Design: `spec/10-backend.md` section 10.4.
//!
//! This is the `-O0` allocator's decision and nothing else. It is linear scan over the line
//! [`crate::order`] lays the function out in: the values are taken in the order they are written,
//! each is given a register that nothing else live at the same time is in, and when there is no
//! such register one of the values in flight goes to the stack instead. There is no splitting and
//! no coalescing, so a value gets one place for the whole of its range and keeps it. That produces
//! mediocre code quickly, which is what `-O0` is for, and the allocator that produces good code
//! slowly is a separate one, in M4.
//!
//! Which value is sent to the stack is the one whose range ends last, counting the value being
//! placed among the candidates. A value wanted for a long time is the cheapest to spill per
//! instruction it frees a register over, and it is the only heuristic here. What is picked is
//! really a register and not a value, since two values that are never both wanted share one, and
//! then every value in that register which is in this one's way goes.
//!
//! # Where the line is not the function
//!
//! The line is the order the blocks arrived in, and `crate::layout` puts them in a different one
//! afterwards, so being between two blocks on the line says nothing about being between them in
//! the code. A value live in one loop and live again in a later one is written down with
//! everything in between inside the interval around it, and it is not live in any of it.
//!
//! Which is why what decides anything here is the area from `crate::live`, and the interval is
//! only the sweep's bookkeeping: it says which values to compare and the areas say which of them
//! actually collide. Three loops one after another in a function put a dozen values in flight at
//! the same instant of the line and never at the same instant of the program, and asking the
//! interval would spill the one this loop is walking for the sake of eleven values in the other
//! two. tamnd/rucc#982.
//!
//! The same holds for a register an instruction insists on. A call destroys seven registers on
//! x86-64, and a function whose blocks happen to arrive with a call written between the blocks of
//! a loop would otherwise lose all seven for every value in that loop, for a call the loop never
//! reaches, so that question is asked of the area and not of the interval either.
//!
//! Allowed is not the same as free, though, so the registers are offered in two passes. First the
//! ones nothing insists on anywhere the range reaches, then the ones something insists on somewhere
//! the value never goes. The second kind costs: the instruction that insists has to be handed the
//! register in the end, and what hands it over is a move. A function that gives a value back has an
//! operand fixed to `rax` at the end of it, and putting the busiest value in the function in `rax`
//! because no path reaches the return with it live buys one register and pays a move at every
//! return. Ordering the two passes is what keeps the register and drops the moves.
//!
//! The hint below is asked the first question rather than the second for the same reason. A value
//! taking the register its own operand asked for saves a move, and taking one somebody else's
//! operand asked for somewhere it never goes costs one, so a hint is worth following when the
//! register is clear and not worth following when it is merely allowed.
//!
//! # What it does with a register an instruction insists on
//!
//! Two things. It stays out of that register for everybody else, and it tries that register first
//! for the value the operand names. A division wants its dividend in `rax`, so `rax` is
//! unavailable to every other value that is live where the division reads, and it is the first
//! register offered to the dividend itself. When the dividend gets it there is no move on the way
//! in, and when it does not the rewrite writes one and nothing else changes.
//!
//! That second half is the hint, and without it the register an instruction insists on is the one
//! register the value in it can never have, since the value's own operand is what makes the
//! register look busy. The effect is largest on returns, because a function that gives a value
//! back has an operand fixed to `rax` at the end of it and most functions give a value back.
//!
//! What makes the hint safe is asking about the register at each of the instruction's two points
//! rather than across the whole of it. An instruction reads at the first and writes at the second,
//! so a register it insists on is one value's at the first, another value's at the second, and
//! nobody else's at either. A division reads its dividend from `rax` and writes its quotient to
//! `rax`, and those are different values that can both live there. A value passed to a call in
//! `rdi` and wanted again afterwards cannot, because nothing writes `rdi` at the second point and
//! a register the call does not write is a register the call is assumed to destroy.
//!
//! An operand that has to be in memory is the other way round. The value it names goes on the
//! stack whatever else is true of it, because that is the only place the instruction could read it
//! from.
//!
//! # What it does with a two address instruction
//!
//! An `add` on x86-64 writes one of the registers it reads, which the operand says as a reuse of
//! another operand. The rewrite can always make that true by copying the source into the
//! destination first, but only if the destination is a register the instruction does not otherwise
//! read, so a value written by a reuse is treated here as live from where the instruction reads
//! rather than from where it writes. Then the copy is always safe.
//!
//! The copy is also usually unnecessary, and the one place this looks past the interval it is
//! placing is to see that: if the value being reused is read here for the last time and the value
//! being written starts here, the second may have the first's register, and the instruction is
//! already two address without anything being moved anywhere. That is the whole of the coalescing
//! this allocator does, and it is worth the dozen lines, because otherwise every piece of
//! arithmetic in the output carries a move in front of it.
//!
//! Both halves of that are needed. The second is the one a loop breaks: an instruction at the
//! bottom of a loop can write a value the top of the loop reads on the next turn, and such a value
//! is live on the way into the instruction that writes it as well as after. It is then wanted at
//! the same time as the value it reuses, whatever is true of the reuse, and giving it the same
//! register makes an addition read the answer to the last one instead of its own operand.
//!
//! # What it does not do
//!
//! It does not touch the function. What comes out is a table saying where each value went, and the
//! pass that rewrites the operands and writes the moves reads it. Keeping the decision and the
//! rewrite apart is what lets the decision be checked by looking at it, and it is the shape
//! `spec/10-backend.md` section 10.4 asks for: an allocator is a function from a program to an
//! assignment and the moves that make it true.

use std::cmp::Reverse;

use rucc_mir::{Constraint, Flags, Func, Inst, Operand, Reg, Role};
use rucc_target::{PhysReg, RegClass};

use crate::live::{Area, Live, Range};
use crate::order::{Order, Point};

/// Where a value lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Place {
    /// In a register, for the whole of its range.
    Reg(PhysReg),
    /// In a slot of the frame, which is what a value the allocator ran out of registers for gets,
    /// and what a value an instruction can only read from memory gets.
    Slot(u32),
}

/// What the allocator is allowed to use.
///
/// The order is the calling convention's, because which register to hand out first follows from
/// which ones a call destroys, and `rucc-target` is where a convention says so. The scratch
/// registers are held back out of the order and are what a spilled value is read into at each
/// instruction that wants it, so a class needs as many of them as one of its instructions has
/// register operands. Nothing here uses them, since a spilled value is only read once the rewrite
/// is writing the instruction that reads it, but they are held back here because this is what
/// decides what everything else may have.
#[derive(Debug, Clone, Default)]
pub struct Env {
    classes: Vec<Class>,
}

/// What one class of registers offers.
#[derive(Debug, Clone, Default)]
struct Class {
    order: Vec<PhysReg>,
    scratch: Vec<PhysReg>,
}

impl Env {
    /// An environment offering nothing, which is what a target that has said nothing offers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same environment, with that class described.
    #[must_use]
    pub fn with(mut self, class: RegClass, order: &[PhysReg], scratch: &[PhysReg]) -> Self {
        let index = usize::from(class.number());
        if self.classes.len() <= index {
            self.classes.resize(index + 1, Class::default());
        }
        self.classes[index] = Class { order: order.to_vec(), scratch: scratch.to_vec() };
        self
    }

    /// The registers it may hand out in a class, in the order it prefers them.
    #[must_use]
    pub fn order(&self, class: RegClass) -> &[PhysReg] {
        self.classes.get(usize::from(class.number())).map_or(&[], |class| &class.order)
    }

    /// The registers it may hand out, by class number, empty for a class it says nothing about.
    pub(crate) fn offered(&self) -> impl Iterator<Item = &[PhysReg]> + '_ {
        self.classes.iter().map(|class| class.order.as_slice())
    }

    /// The registers held back in a class for reading a spilled value into.
    #[must_use]
    pub fn scratch(&self, class: RegClass) -> &[PhysReg] {
        self.classes.get(usize::from(class.number())).map_or(&[], |class| &class.scratch)
    }
}

/// Where every value in a function went.
#[derive(Debug, Clone)]
pub struct Assignment {
    places: Vec<Option<Place>>,
    slots: Vec<RegClass>,
    commuted: Vec<Inst>,
}

impl Assignment {
    /// Records that the sources of `inst` are to be swapped, for an answer written over the second.
    pub(crate) fn commute(&mut self, inst: Inst) {
        self.commuted.push(inst);
    }

    /// An assignment that says nothing yet about a function with that many values.
    ///
    /// This and [`Assignment::put`] and [`Assignment::take_slot`] are how an allocator says what
    /// it decided. There will be a second one in M4 and it will not reach its answer this way, so
    /// what an assignment is has to be separable from how this file arrives at one, and the
    /// checker in [`crate::check`] reads an assignment without caring which allocator wrote it.
    #[must_use]
    pub fn empty(vregs: usize) -> Self {
        Self { places: vec![None; vregs], slots: Vec::new(), commuted: Vec::new() }
    }

    /// The two address instructions whose answer went into the register of their second source.
    ///
    /// Each has to have its two sources swapped before anything reads the assignment against the
    /// function, which [`crate::run`] does. After that the answer reuses what is then the first
    /// source, as every two address instruction does. tamnd/rucc#1895.
    #[must_use]
    pub fn commuted(&self) -> &[Inst] {
        &self.commuted
    }

    /// Records where a value went.
    ///
    /// # Panics
    ///
    /// Panics on a physical register, which is somewhere already, and on a virtual one the
    /// function never handed out.
    pub fn put(&mut self, reg: Reg, place: Place) {
        self.places[index(reg)] = Some(place);
    }

    /// Takes a slot of the frame, of that class, and gives back which one it is.
    ///
    /// # Panics
    ///
    /// Panics past four billion slots, which is a frame no machine has room for.
    pub fn take_slot(&mut self, class: RegClass) -> u32 {
        let slot = u32::try_from(self.slots.len()).expect("too many spilled values");
        self.slots.push(class);
        slot
    }

    /// Where a value lives, or `None` for a virtual register this function never mentions and for
    /// a physical one, which is already where it is.
    #[must_use]
    pub fn place(&self, reg: Reg) -> Option<Place> {
        self.places.get(usize::try_from(reg.number()?).ok()?).copied().flatten()
    }

    /// The class of each slot of the frame, which is what says how wide it has to be.
    #[must_use]
    pub fn slots(&self) -> &[RegClass] {
        &self.slots
    }

    /// Every value that went somewhere, and where it went.
    ///
    /// The assignment read the other way round, which is what a caller wants when the question is
    /// about the places rather than about the values. The stack slot allocator asks it that way,
    /// since what it needs is which value is in each slot and the assignment is stored by value.
    pub fn placed(&self) -> impl Iterator<Item = (Reg, Place)> + '_ {
        self.places.iter().enumerate().filter_map(|(number, place)| {
            let number = u32::try_from(number).ok()?;
            Some((Reg::virtual_reg(number), (*place)?))
        })
    }

    /// How many values went to the stack.
    #[must_use]
    pub fn spilled(&self) -> usize {
        self.slots.len()
    }

    /// Puts a value on the stack, in a slot of its own.
    pub(crate) fn spill(&mut self, reg: Reg, class: RegClass) {
        let slot = self.take_slot(class);
        self.put(reg, Place::Slot(slot));
    }
}

/// One value waiting for a place.
#[derive(Debug, Clone, Copy)]
struct Interval<'a> {
    reg: Reg,
    class: RegClass,
    /// The interval around the area, which is what the sweep below reads and what says which value
    /// is wanted for longest when one of them has to go.
    range: Range,
    /// Everywhere the value is really live, which is what says whether two of them fit in one
    /// register.
    area: Area<'a>,
}

/// One value that has a register, for as long as it still wants it.
#[derive(Debug, Clone, Copy)]
struct Held<'a> {
    reg: Reg,
    class: RegClass,
    range: Range,
    area: Area<'a>,
    at: PhysReg,
    /// How many values were given a register before this one, which is the order the values in
    /// flight are looked at in when one register has to be taken back.
    since: usize,
}

/// The values that have a register, kept by the register each is in.
///
/// Nearly every question asked of them is about one register: whether it is free for an interval,
/// or whether a value is still in it. They used to be one list walked from the start for every
/// register tried, and on a function with a thousand values in flight that walk was most of the
/// time the allocator took. Keeping them by register means asking about one reads only the values
/// that are in it. Registers are numbered within their class, so two classes can share a list and
/// the class is still checked.
#[derive(Default)]
struct Active<'a> {
    by: Vec<Vec<Held<'a>>>,
    /// For each register, a point no value in it ends before. Values are let go of at the start of
    /// every interval, and most of those times nothing in most registers has ended, so a register
    /// whose values all end at or after the point is not walked at all.
    soonest: Vec<Point>,
    /// How many values have been given a register so far.
    count: usize,
    /// The pieces of the values in each register, by class and then by register number, which is
    /// what [`available`] asks about.
    pieces: Vec<Vec<Pieces>>,
    /// The list [`spill_one`] weighs the registers in, kept so a spill does not build a new one.
    costs: Vec<(usize, PhysReg, usize, Point)>,
}

impl<'a> Active<'a> {
    /// The values in one register, in the order they were given it.
    fn at(&self, at: PhysReg) -> &[Held<'a>] {
        self.by.get(usize::from(at.number())).map_or(&[], Vec::as_slice)
    }

    fn push(&mut self, reg: Reg, class: RegClass, range: Range, area: Area<'a>, at: PhysReg) {
        let slot = usize::from(at.number());
        if self.by.len() <= slot {
            self.by.resize_with(slot + 1, Vec::new);
            self.soonest.resize(slot + 1, Point::MAX);
        }
        self.by[slot].push(Held { reg, class, range, area, at, since: self.count });
        self.soonest[slot] = self.soonest[slot].min(range.end);
        self.count += 1;
        let held = &self.by[slot];
        let pieces = pieces_mut(&mut self.pieces, class, slot);
        if pieces.kept {
            pieces.drop_before(range.start);
            for piece in area.pieces() {
                pieces.insert(piece, reg);
            }
        } else if held.len() > FEW {
            // Enough values to be worth a list, which starts with what is already there.
            pieces.kept = true;
            for held in held.iter().filter(|held| held.class == class) {
                for piece in held.area.pieces() {
                    pieces.insert(piece, held.reg);
                }
            }
        }
    }

    /// Whether a value of the class in `at` other than `except` is live anywhere the area is.
    fn taken(&self, class: RegClass, at: PhysReg, area: Area<'_>, except: Option<Reg>) -> bool {
        let held = self.at(at);
        if held.len() > FEW {
            if let Some(answer) = self.listed(class, at, area, except) {
                return answer;
            }
        }
        held.iter()
            .any(|held| held.class == class && Some(held.reg) != except && held.area.overlaps(area))
    }

    /// What the list of `at`'s pieces says, or nothing when it is not kept or not in order. Out of
    /// line so that the walk above, which is all most registers ever need, stays small enough to
    /// be put inline where it is asked.
    #[inline(never)]
    fn listed(
        &self,
        class: RegClass,
        at: PhysReg,
        area: Area<'_>,
        except: Option<Reg>,
    ) -> Option<bool> {
        let by = self.pieces.get(usize::from(class.number()))?;
        let pieces = by.get(usize::from(at.number()))?;
        (pieces.kept && !pieces.broken).then(|| pieces.touch(area, except))
    }

    /// The values of the class in register number `slot` that touch the area, from its list of
    /// pieces, or nothing when the list is not kept or not in order.
    #[inline(never)]
    fn owners(&self, class: RegClass, slot: usize, area: Area<'_>) -> Option<Vec<Reg>> {
        let pieces = self.pieces.get(usize::from(class.number()))?.get(slot)?;
        if pieces.kept { pieces.owners(area) } else { None }
    }

    /// Takes the values of the class in `at` whose areas `goes` says to, and hands each to `gone`.
    fn evict(
        &mut self,
        class: RegClass,
        at: PhysReg,
        goes: impl Fn(&Held<'a>) -> bool,
        mut gone: impl FnMut(&Held<'a>),
    ) {
        let slot = usize::from(at.number());
        let mut taken = Vec::new();
        self.by[slot].retain(|held| {
            let out = held.class == class && goes(held);
            if out {
                gone(held);
                taken.push((held.reg, held.area));
            }
            !out
        });
        let pieces = pieces_mut(&mut self.pieces, class, slot);
        if pieces.kept {
            for (reg, area) in taken {
                for piece in area.pieces() {
                    pieces.remove(piece, reg);
                }
            }
        }
    }

    /// Lets go of every value whose interval ends before a point.
    ///
    /// Taking a value out of a register anywhere else leaves that register's soonest end where it
    /// was, which is still a point nothing in it ends before, so only this and [`Active::push`]
    /// have to keep it.
    fn expire(&mut self, point: Point) {
        for (held, soonest) in self.by.iter_mut().zip(&mut self.soonest) {
            if *soonest >= point {
                continue;
            }
            held.retain(|held| held.range.end >= point);
            *soonest = held.iter().map(|held| held.range.end).min().unwrap_or(Point::MAX);
        }
    }
}

/// How many values a register can hold before its pieces are kept in a list. A register with no
/// more than this in it, which is most of them without optimization, is quicker to ask about by
/// walking its values, and keeping a list for every one of those cost more than it saved.
pub(crate) const FEW: usize = 4;

/// The list for one class and register number, made when it is first asked for.
fn pieces_mut(pieces: &mut Vec<Vec<Pieces>>, class: RegClass, slot: usize) -> &mut Pieces {
    let class = usize::from(class.number());
    if pieces.len() <= class {
        pieces.resize_with(class + 1, Vec::new);
    }
    let by = &mut pieces[class];
    if by.len() <= slot {
        by.resize_with(slot + 1, Pieces::default);
    }
    &mut by[slot]
}

/// The pieces of every value of one class in one register, sorted by where they start.
///
/// Asking whether a register is free for a value used to compare the value with every other value
/// in the register, a walk over both lists of pieces for each one, and with a few dozen values
/// in each register that was a large part of an optimized build of a large file. Two values in
/// one register are never live at once, bar the one point a value written over the one it reuses
/// shares with it, so in start order the pieces end in order too. Then the only piece that can
/// touch one of the value's is the last one that starts before that piece ends, and asking is a
/// search for each piece of the value rather than a walk over everything in the register.
///
/// Whether the ends really are in order is checked as each piece goes in, and a register where
/// they are not is answered by the walk over its values instead, so the answer is the same either
/// way.
#[derive(Default)]
pub(crate) struct Pieces {
    /// Start, end and whose, sorted by start and then by end.
    list: Vec<(Point, Point, Reg)>,
    /// Whether a piece went in that ends before one in front of it.
    broken: bool,
    /// Whether the list is kept at all, which it is from the first time the register holds more
    /// than [`FEW`] values.
    pub(crate) kept: bool,
}

impl Pieces {
    #[inline(never)]
    pub(crate) fn insert(&mut self, piece: Range, reg: Reg) {
        let key = (piece.start, piece.end);
        let at = self.list.partition_point(|&(start, end, _)| (start, end) <= key);
        let after = at == 0 || self.list[at - 1].1 <= piece.end;
        let before = self.list.get(at).is_none_or(|next| piece.end <= next.1);
        if !(after && before) {
            self.broken = true;
        }
        self.list.insert(at, (piece.start, piece.end, reg));
    }

    pub(crate) fn remove(&mut self, piece: Range, reg: Reg) {
        let from = self.list.partition_point(|&(start, _, _)| start < piece.start);
        let found = self.list[from..]
            .iter()
            .take_while(|&&(start, _, _)| start == piece.start)
            .position(|&(_, end, owner)| end == piece.end && owner == reg);
        if let Some(offset) = found {
            self.list.remove(from + offset);
        }
    }

    /// Lets go of the pieces that end before a point, which no value starting there can touch.
    /// They are the ones at the front while the ends are in order.
    fn drop_before(&mut self, point: Point) {
        if !self.broken {
            let gone = self.list.partition_point(|&(_, end, _)| end < point);
            self.list.drain(..gone);
        }
    }

    /// Whether a piece of a value other than `except` touches the area.
    fn touch(&self, area: Area<'_>, except: Option<Reg>) -> bool {
        area.pieces().any(|piece| {
            let below = self.list.partition_point(|&(start, _, _)| start <= piece.end);
            self.list[..below]
                .iter()
                .rev()
                .find(|&&(_, _, owner)| Some(owner) != except)
                .is_some_and(|&(_, end, _)| end >= piece.start)
        })
    }

    /// Every value with a piece that touches the area, each once and in order, or `None` for a
    /// list whose ends are out of order. With the ends in order the pieces that touch one of the
    /// area's are the last few that start before it ends, back to the first that ends before it
    /// starts.
    pub(crate) fn owners(&self, area: Area<'_>) -> Option<Vec<Reg>> {
        if self.broken {
            return None;
        }
        let mut owners = Vec::new();
        for piece in area.pieces() {
            let below = self.list.partition_point(|&(start, _, _)| start <= piece.end);
            let touching =
                self.list[..below].iter().rev().take_while(|&&(_, end, _)| end >= piece.start);
            owners.extend(touching.map(|&(_, _, owner)| owner));
        }
        owners.sort_unstable();
        owners.dedup();
        Some(owners)
    }
}

/// A register an instruction insists on, and where it insists on it.
#[derive(Debug, Clone, Copy)]
struct Blocked {
    class: RegClass,
    at: PhysReg,
    /// One of the instruction's two points. Every register an instruction insists on has an entry
    /// at each of them, because a register held at one of the two is a register nothing else may
    /// be in across the instruction.
    point: Point,
    /// The one value that may be in it there, which is the value of an operand the instruction
    /// reads at that point or writes at it. `None` means nothing may: an operand naming a physical
    /// register outright claims it against everything, and a point no operand covers is a point
    /// the instruction has the register to itself at.
    by: Option<Reg>,
    /// The byte the instruction writes the register from, when it leaves the bottom of it alone,
    /// which is what a call does to a register AArch64 keeps the low half of. A value that fits
    /// below it is not in the way. See [`Constraint::Above`].
    above: Option<u8>,
}

impl Blocked {
    /// Whether this is in the way of a value of that width, which it is unless it writes only
    /// above everything the value takes.
    fn reaches(&self, width: Option<u8>) -> bool {
        match (self.above, width) {
            (Some(above), Some(width)) => width > above,
            _ => true,
        }
    }
}

/// A value written into the register another operand of the same instruction was read from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reuse {
    /// The value being read, which is the one whose register would do.
    pub(crate) source: Reg,
    /// The other value the instruction reads, when the instruction reads the two either way round
    /// and so could write its answer over this one instead.
    pub(crate) second: Option<Reg>,
    /// Where the instruction reads it.
    pub(crate) at: Point,
    /// The instruction, which is swapped round if the answer takes the second value's register.
    pub(crate) inst: Inst,
}

/// Decides where every value in a function lives.
///
/// # Panics
///
/// Panics if a class has no registers to hand out and something in the function is in that class,
/// since that is a target description that does not describe the target the function is for.
#[must_use]
pub fn assign(func: &Func, order: &Order, live: &Live, env: &Env) -> Assignment {
    let blocked = blocked(func, order);
    let forced = forced(func);
    let reuses = reuses(func, order);
    let hints = hints(func);
    let passed = passed(func);

    let mut intervals = Vec::with_capacity(func.vregs());
    for (number, reuse) in reuses.iter().enumerate() {
        let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
        let (Some(mut area), Some(class)) = (live.area(reg), func.class_of(reg)) else {
            continue;
        };
        if let Some(reuse) = reuse {
            area = area.with(reuse.at);
        }
        intervals.push(Interval { reg, class, range: area.hull(), area });
    }
    intervals.sort_by_key(|interval| (interval.range.start, interval.reg));

    let mut assignment = Assignment::empty(func.vregs());
    let mut active = Active::default();
    for interval in intervals {
        active.expire(interval.range.start);
        if forced.contains(&interval.reg) {
            assignment.spill(interval.reg, interval.class);
            continue;
        }
        // A class with no order is one the target says nothing allocates from, which on x86-64 is
        // the x87 stack. A value of such a class is a mistake at the point it was made rather than
        // a value with nowhere to go: what the target means is that the value lives in memory and
        // that whatever operates on it takes an address. See `ClassInfo::allocatable`.
        assert!(
            !env.order(interval.class).is_empty(),
            "a value in class {}, which the target hands out no registers from",
            interval.class.number()
        );
        let reuse = reuses[index(interval.reg)];
        let coalesced = |source| coalesce(&assignment, &active, &blocked, live, interval, source);
        let first = reuse.and_then(|reuse| coalesced(reuse.source));
        let second = reuse.and_then(|reuse| reuse.second).and_then(coalesced);
        // An instruction that reads its sources either way round can write over the second one
        // instead, which is what it needs when the first is read again later and the second is
        // not. When both would do, the first is kept unless only the second is where something
        // wants the answer, which saves the move in front of that reader.
        //
        // A block the answer is passed to wants it where that block's parameter already is. That
        // has to count as much as an instruction asking for a register. A sum a loop carries is
        // passed back to the parameter it was read from, and taking the register of the other
        // source because the sum is also printed at the end moves the copy onto the back edge,
        // where it runs every turn instead of once.
        let hinted_at = |at: Option<PhysReg>| {
            at.is_some_and(|at| {
                hints[index(interval.reg)].contains(&at)
                    || passed[index(interval.reg)]
                        .iter()
                        .any(|&param| assignment.place(param) == Some(Place::Reg(at)))
            })
        };
        let commute =
            second.is_some() && (first.is_none() || hinted_at(second) && !hinted_at(first));
        let two_address = if commute { second } else { first };
        if let (true, Some(reuse)) = (commute, reuse) {
            assignment.commuted.push(reuse.inst);
        }
        // The reuse comes first, because a two address instruction that has to copy its left
        // operand in pays for the copy whatever the hint says, and taking the hint here would buy
        // one move at the cost of another.
        let hinted = hints[index(interval.reg)].iter().copied().find(|&at| {
            env.order(interval.class).contains(&at)
                && available(&active, &blocked, interval, at, None, Want::Clear)
        });
        // A register nobody else wants anywhere near this value first, and one somebody wants
        // somewhere the value never goes only when there is no other. Both are correct and the
        // second is the worse buy, since the instruction that wants it has to be handed it and
        // whatever this value is doing there has to move out of the way first.
        let scan = |want| {
            env.order(interval.class)
                .iter()
                .copied()
                .find(|&at| available(&active, &blocked, interval, at, None, want))
        };
        let chosen =
            two_address.or(hinted).or_else(|| scan(Want::Clear)).or_else(|| scan(Want::Allowed));
        match chosen {
            Some(at) => {
                assignment.places[index(interval.reg)] = Some(Place::Reg(at));
                active.push(interval.reg, interval.class, interval.range, interval.area, at);
            }
            None => spill_one(&mut assignment, &mut active, &blocked, interval),
        }
    }
    assignment
}

/// How much a register suits an interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    /// No other value is handed it anywhere the range reaches, and nothing writes it where the
    /// value is live, so taking it costs nobody anything.
    ///
    /// A register an instruction only destroys, which is what a call does to seven of them, is
    /// clear for a value that is dead there. There is no value of the instruction's own to move
    /// in, so a loop counter that is passed to a call on the way out may stay in a register the
    /// call destroys. Counting the clobber over the whole range sent such a value to a callee
    /// saved register, which is a push and a pop for nothing. tamnd/rucc#2202.
    Clear,
    /// Something insists on it somewhere the range reaches and nowhere the value is live, so taking
    /// it is allowed and may still cost: the instruction that insists wants the register for a
    /// value of its own, and that value now has to be moved into it.
    Allowed,
}

/// Every register every instruction in the function insists on, arranged to be asked about.
///
/// Built once and never changed afterwards, and there is only one question ever asked of it: of the
/// constraints naming one register of one class, is there one at a point some interval covers. So
/// the entries are ordered by the register they name and then by the point, and the question is a
/// binary search for the start of the interval followed by a walk that stops at its end.
///
/// It used to be a flat list walked from one end for every candidate register of every interval,
/// which is quadratic in the size of a function and is most of the compile on a large one. See
/// tamnd/rucc#1003 for the profile that found it.
///
/// The search is over the points alone and only among the one register's entries. A search over
/// the whole list compares three fields of an entry several times its size at every step, and on a
/// large function that is most of what asking costs, since every candidate register of every
/// interval asks.
pub(crate) struct Blocks {
    /// The constraints, sorted by class, then by register, then by point.
    all: Vec<Blocked>,
    /// The point of each constraint, in the same order, which is what the search reads.
    points: Vec<Point>,
    /// Where each register's constraints start and end in the list, by class times `stride` plus
    /// the register's number.
    spans: Vec<(usize, usize)>,
    /// One more than the highest register number anything insists on.
    stride: usize,
    /// How many bytes of its register each virtual register's value takes, by number, which is
    /// what [`Blocked::reaches`] asks.
    widths: Vec<Option<u8>>,
}

impl Blocks {
    /// Whether an instruction insists on `at` where a value over `area` would be in its way: at any
    /// point the value's range reaches when the register is wanted clear and the instruction wants
    /// it for a value of its own, and otherwise only at a point the value is live at. The value's
    /// own operands never count.
    pub(crate) fn insists(
        &self,
        reg: Reg,
        class: RegClass,
        area: Area<'_>,
        range: Range,
        at: PhysReg,
        want: Want,
    ) -> bool {
        self.over(class, at, range).any(|one| {
            one.by != Some(reg)
                && one.reaches(self.width(reg))
                && ((want == Want::Clear && one.by.is_some()) || area.covers(one.point))
        })
    }

    /// How many bytes of its register a value takes, or `None` for all of it.
    fn width(&self, reg: Reg) -> Option<u8> {
        let number = usize::try_from(reg.number()?).ok()?;
        self.widths.get(number).copied().flatten()
    }

    /// Every register an instruction takes for itself where no value may be in it, with the class
    /// and the point, sorted by class, then by register, then by point.
    ///
    /// Not one it writes only the top of, since a value narrow enough may still be in that.
    pub(crate) fn taken(&self) -> impl Iterator<Item = (RegClass, PhysReg, Point)> + '_ {
        self.all
            .iter()
            .filter(|one| one.by.is_none() && one.above.is_none())
            .map(|one| (one.class, one.at, one.point))
    }

    /// The constraints on one register of one class at the points an interval covers.
    ///
    /// Both ends of the walk come from the ordering rather than from a test, so what comes back is
    /// exactly what the old `covers` call used to keep and in the same order.
    #[inline]
    fn over(
        &self,
        class: RegClass,
        at: PhysReg,
        range: Range,
    ) -> impl Iterator<Item = &Blocked> + '_ {
        let (low, high) = if usize::from(at.number()) < self.stride {
            let key = usize::from(class.number()) * self.stride + usize::from(at.number());
            self.spans.get(key).copied().unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        let first = low + self.points[low..high].partition_point(|&point| point < range.start);
        self.all[first..high].iter().take_while(move |one| one.point <= range.end)
    }
}

/// Whether a register is one this interval could have.
///
/// The exception is the value a reuse is coalescing with, which holds the register right up to the
/// point the new value takes it over and is the one thing that may overlap.
///
/// The sweep only keeps a value in `active` while the interval around it reaches this one, so the
/// areas still have to be compared: two values whose intervals cross can have holes that let them
/// share a register anyway, which on a function with several loops in it is most of them.
fn available(
    active: &Active<'_>,
    blocked: &Blocks,
    interval: Interval<'_>,
    at: PhysReg,
    except: Option<Reg>,
    want: Want,
) -> bool {
    let taken = active.taken(interval.class, at, interval.area, except);
    let width = blocked.width(interval.reg);
    let insisted = blocked.over(interval.class, at, interval.range).any(|one| {
        one.by != Some(interval.reg)
            && one.reaches(width)
            && ((want == Want::Clear && one.by.is_some()) || interval.area.covers(one.point))
    });
    !taken && !insisted
}

/// The register the value being reused is in, when the value being written is never live at the
/// same time as it and the register is otherwise free.
fn coalesce(
    assignment: &Assignment,
    active: &Active<'_>,
    blocked: &Blocks,
    live: &Live,
    interval: Interval<'_>,
    source: Reg,
) -> Option<PhysReg> {
    let Some(Place::Reg(at)) = assignment.place(source) else { return None };
    active.at(at).iter().find(|held| held.reg == source)?;
    // The two have to be apart everywhere, asked of the areas liveness worked out and without the
    // point the reuse adds, since that point is the one they are allowed to share.
    //
    // That covers both ways it can go wrong. A value read again later needs its register after
    // this instruction would have overwritten it. And a value being written that is live where the
    // instruction reads already is what a loop carrying its own result round looks like: the
    // instruction writes it at the bottom and the top of the loop reads what the last turn wrote.
    // Either way the two are wanted at once, and no register holds both.
    //
    // It used to be asked of the end of the interval around the value being read, and that is not
    // the same question. A block laid out after this instruction where the value is still live,
    // such as the default arm of a `switch` that joins back in above it, stretches the interval
    // past this point when nothing past it reads the value at all. The sum a loop carries round
    // then went into a new register and was copied back at the bottom of every turn.
    // tamnd/rucc#1965.
    let free = available(active, blocked, interval, at, Some(source), Want::Allowed);
    (apart(live, source, interval.reg) && free).then_some(at)
}

/// Whether two values are never live at the same time, going by what liveness worked out.
pub(crate) fn apart(live: &Live, first: Reg, second: Reg) -> bool {
    match (live.area(first), live.area(second)) {
        (Some(first), Some(second)) => !first.overlaps(second),
        _ => false,
    }
}

/// Sends values to the stack to free a register: the ones wanted for longest, since a register
/// held that long pays for itself over the most instructions.
///
/// What is chosen is a register rather than a value, because two values whose areas miss each
/// other share one and taking it means every value in it this one is really on top of has to go.
/// A register holding two of those costs twice as much to take as one holding a single value, so
/// the cheap ones are looked at first and the reach only settles ties.
fn spill_one<'a>(
    assignment: &mut Assignment,
    active: &mut Active<'a>,
    blocked: &Blocks,
    interval: Interval<'a>,
) {
    // What each register would cost: how many values would go, and the furthest any of them
    // reaches. The list is one entry per register of the class, so walking it for each value in
    // flight is the same shape as everything else here. The first number is when the earliest of
    // them was given the register, and sorting by it puts the registers in the order the values
    // were given them, which is what settles a tie.
    let mut costs = std::mem::take(&mut active.costs);
    costs.clear();
    for (slot, values) in active.by.iter().enumerate() {
        // A register with a list of its pieces says which values touch the area, which is quicker
        // than asking each value in it when it holds many.
        let owners = if values.len() > FEW {
            active.owners(interval.class, slot, interval.area)
        } else {
            None
        };
        for held in values {
            if held.class != interval.class {
                continue;
            }
            let touches = match &owners {
                Some(owners) => owners.binary_search(&held.reg).is_ok(),
                None => held.area.overlaps(interval.area),
            };
            if !touches {
                continue;
            }
            match costs.iter_mut().find(|(_, at, _, _)| *at == held.at) {
                Some((first, _, count, reach)) => {
                    *first = (*first).min(held.since);
                    *count += 1;
                    *reach = (*reach).max(held.range.end);
                }
                None => costs.push((held.since, held.at, 1, held.range.end)),
            }
        }
    }
    costs.sort_unstable_by_key(|&(first, _, _, _)| first);
    // A register the instructions in the way insist on for themselves is no use, because taking it
    // over would put this value in a register it may not have.
    let none = Active::default();
    let chosen = costs
        .iter()
        .filter(|&&(_, at, _, reach)| {
            reach > interval.range.end
                && available(&none, blocked, interval, at, None, Want::Allowed)
        })
        .min_by_key(|&&(_, _, count, reach)| (count, Reverse(reach)))
        .map(|&(_, at, _, _)| at);
    active.costs = costs;
    match chosen {
        Some(at) => {
            active.evict(
                interval.class,
                at,
                |held| held.area.overlaps(interval.area),
                |held| assignment.spill(held.reg, held.class),
            );
            assignment.places[index(interval.reg)] = Some(Place::Reg(at));
            active.push(interval.reg, interval.class, interval.range, interval.area, at);
        }
        None => assignment.spill(interval.reg, interval.class),
    }
}

/// The registers the instructions insist on, and where.
///
/// A physical register an operand names outright counts the same way. Nothing before allocation
/// writes one except an instruction that has to, and it has to for the length of that one
/// instruction, which is the same statement a fixed constraint makes.
pub(crate) fn blocked(func: &Func, order: &Order) -> Blocks {
    let mut blocked = Vec::new();
    let mut claimed: Vec<(RegClass, PhysReg)> = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            let operands = &func[func[inst].operands];
            claimed.clear();
            for operand in operands {
                if let Some(at) = insisted(operand) {
                    let key = (operand.class, at);
                    if !claimed.contains(&key) {
                        claimed.push(key);
                    }
                }
            }
            for &(class, at) in &claimed {
                // Both points, whether or not an operand is at them. A register an instruction
                // reads and does not write is still gone by the time the instruction is done as far
                // as anything here knows, which is what stops the value a call is passed in `rdi`
                // from staying in `rdi` over the call.
                for (point, role) in [(order.early(inst), Role::Use), (order.late(inst), Role::Def)]
                {
                    let mut named = false;
                    for operand in operands {
                        let mine = insisted(operand) == Some(at) && operand.class == class;
                        if !mine || !(operand.role == role || operand.role == Role::EarlyDef) {
                            continue;
                        }
                        named = true;
                        let by = operand.reg.is_virtual().then_some(operand.reg);
                        let above = match operand.constraint {
                            Constraint::Above(above) => Some(above),
                            _ => None,
                        };
                        blocked.push(Blocked { class, at, point, by, above });
                    }
                    // A register no operand names where the operands are read is one the
                    // instruction writes and does not read, which is what a clobber is, and the
                    // seven registers a call destroys are the whole of why that case is worth
                    // separating. Such a register is free right up to the point it is written, so a
                    // value whose last read is this instruction may sit in one: it is read before
                    // the instruction writes anything, the way any other operand is. Blocking it
                    // where the operands are read as well would take every caller saved register
                    // away from the value a call is passed, which is a value that dies at the call
                    // and pays for a callee saved register it holds for two instructions. Anything
                    // living past the instruction is still refused, by the block below.
                    //
                    // This is where a target's early definitions are paid for. An instruction that
                    // fills a register before it has finished reading has to say so, because that
                    // is the one thing a plain definition here no longer covers: a division on
                    // x86-64 is a sign extension and then the division itself, so `rdx` is gone
                    // before the divisor is read, and a divisor that went there would be read as
                    // the dividend's own sign bits. `rucc_target::x86_64` writes both of them down
                    // as early definitions for exactly that reason.
                    if !named && role == Role::Def {
                        blocked.push(Blocked { class, at, point, by: None, above: None });
                    }
                }
            }
        }
    }
    // Program order already has the points ascending, but the registers one instruction claims are
    // walked outside the two points rather than inside them, so the list arrives in order by
    // instruction and not by register. A sort by the key the lookup searches on is what makes it
    // searchable, and it is stable so two constraints on one register at one point keep the order
    // the instruction wrote them in.
    blocked.sort_by_key(|one: &Blocked| (one.class, one.at, one.point));
    let widths = (0..func.vregs())
        .map(|number| func.width(Reg::virtual_reg(u32::try_from(number).ok()?)))
        .collect();
    let points = blocked.iter().map(|one| one.point).collect();
    let stride = blocked.iter().map(|one| usize::from(one.at.number()) + 1).max().unwrap_or(0);
    let classes = blocked.last().map_or(0, |one| usize::from(one.class.number()) + 1);
    let mut spans = vec![(0, 0); classes * stride];
    for (index, one) in blocked.iter().enumerate() {
        let key = usize::from(one.class.number()) * stride + usize::from(one.at.number());
        let span = &mut spans[key];
        if span.1 == 0 {
            span.0 = index;
        }
        span.1 = index + 1;
    }
    Blocks { all: blocked, points, spans, stride, widths }
}

/// The register an operand has to be in, which is the one a constraint asks for or the one the
/// operand names outright.
fn insisted(operand: &Operand) -> Option<PhysReg> {
    match operand.constraint {
        Constraint::Fixed(at) => Some(at),
        _ => operand.reg.phys(),
    }
}

/// The registers each value would rather be in, which are the ones the operands naming it insist on.
///
/// In the order the function writes them down, so the definition comes first where there is one,
/// since a value written into a fixed register and then moved somewhere else pays for the move at
/// the top of its life rather than at the bottom. The ones after it are worth keeping for the same
/// reason the first one is, and the value a call is passed is where that shows: its definition may
/// insist on the register a parameter arrived in, which the call it is handed to has usually taken
/// back for an argument of its own by then, and behind that is the register the convention passes
/// it in, which is free and is exactly where the value wants to end up.
pub(crate) fn hints(func: &Func) -> Vec<Vec<PhysReg>> {
    let mut hints = vec![Vec::new(); func.vregs()];
    for block in func.blocks() {
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                let Constraint::Fixed(at) = operand.constraint else { continue };
                let number = operand.reg.number().and_then(|number| usize::try_from(number).ok());
                let Some(number) = number else { continue };
                let wanted: &mut Vec<PhysReg> = &mut hints[number];
                if func.class_of(operand.reg) == Some(operand.class) && !wanted.contains(&at) {
                    wanted.push(at);
                }
            }
        }
    }
    hints
}

/// The block parameters each value is passed to, by the virtual register passed.
pub(crate) fn passed(func: &Func) -> Vec<Vec<Reg>> {
    let mut passed = vec![Vec::new(); func.vregs()];
    for block in func.blocks() {
        for call in &func[block].succs {
            for (&arg, param) in call.args.iter().zip(&func[call.block].params) {
                let number = arg.number().and_then(|number| usize::try_from(number).ok());
                let Some(number) = number else { continue };
                let to: &mut Vec<Reg> = &mut passed[number];
                if !to.contains(&param.reg) {
                    to.push(param.reg);
                }
            }
        }
    }
    passed
}

/// The values that have to be on the stack whatever else is true of them.
pub(crate) fn forced(func: &Func) -> Vec<Reg> {
    let mut forced = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                if operand.constraint == Constraint::Stack
                    && operand.reg.is_virtual()
                    && !forced.contains(&operand.reg)
                {
                    forced.push(operand.reg);
                }
            }
        }
    }
    forced
}

/// The value each two address instruction reuses, by the virtual register it writes.
pub(crate) fn reuses(func: &Func, order: &Order) -> Vec<Option<Reuse>> {
    let mut reuses = vec![None; func.vregs()];
    for block in func.blocks() {
        for inst in func.insts(block) {
            let operands = &func[func[inst].operands];
            for operand in operands {
                let Constraint::Reuse(other) = operand.constraint else { continue };
                let number = operand.reg.number().and_then(|number| usize::try_from(number).ok());
                let Some(number) = number else { continue };
                let source = operands[usize::from(other)].reg;
                let second = if func[inst].flags.contains(Flags::COMMUTES) {
                    swappable(operands, usize::from(other))
                } else {
                    None
                };
                reuses[number] = Some(Reuse { source, second, at: order.early(inst), inst });
            }
        }
    }
    reuses
}

/// The second source of an instruction that reads its two sources either way round, when the
/// answer could go over it instead of over the first.
///
/// Only the shape of a two address instruction with two sources, the answer and then the two, with
/// the answer reusing the first. The second has to be a value of the same class that asks for
/// nothing more than a register, since after the swap it is the one the answer reuses.
fn swappable(operands: &[Operand], other: usize) -> Option<Reg> {
    let [answer, first, second] = operands else { return None };
    let same = second.class == first.class && second.class == answer.class;
    let plain = second.role == Role::Use && second.constraint == Constraint::Reg;
    (other == 1 && same && plain && second.reg.is_virtual() && second.reg != first.reg)
        .then_some(second.reg)
}

/// A virtual register's number as a table index.
fn index(reg: Reg) -> usize {
    usize::try_from(reg.number().expect("a virtual register")).expect("a register number")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Opcode, Operand, Param};
    use rucc_target::x86_64::{GPR, R13, R14, R15, RAX, RCX, RDX, REGS, RSI, SYSV};

    use super::*;

    /// The x86-64 environment, with the last three of the allocation order held back as scratch.
    fn env() -> Env {
        let (order, scratch) = SYSV.int_order.split_at(SYSV.int_order.len() - 3);
        Env::new().with(GPR, order, scratch)
    }

    /// An environment with that many general purpose registers, for putting a function under
    /// pressure without writing a hundred instructions.
    fn narrow(count: usize) -> Env {
        Env::new().with(GPR, &SYSV.int_order[..count], &SYSV.int_order[count..count + 1])
    }

    /// What a place is called, which is what an assertion reads.
    fn named(place: Option<Place>) -> String {
        match place {
            Some(Place::Reg(reg)) => REGS.name(GPR, reg).expect("a register").to_string(),
            Some(Place::Slot(slot)) => format!("slot {slot}"),
            None => "nowhere".to_string(),
        }
    }

    /// Where every value in a function went.
    fn places(func: &Func, env: &Env) -> Vec<String> {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let assignment = assign(func, &order, &live, env);
        (0..func.vregs())
            .map(|number| {
                let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
                named(assignment.place(reg))
            })
            .collect()
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

        // The first register in the order, twice, because the first value is finished with before
        // the second one is written.
        assert_eq!(places(&func, &env()), ["rax", "rax"]);
    }

    /// A value held over an instruction that writes the whole of the first register and the top
    /// of the second, which is a call on AArch64 and `v8` in small, with the value as wide as that.
    fn over_the_top(width: u32) -> Func {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let held = func.new_vreg(GPR);
        func.set_width(held, width);
        func.build(block, opcode).def(held, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .operand(Operand::write(Reg::physical(RCX), GPR).with(Constraint::Above(8)))
            .finish();
        func.build(block, opcode).uses(held, GPR).finish();
        func
    }

    #[test]
    fn a_value_that_fits_under_what_an_instruction_writes_stays_in_the_register() {
        assert_eq!(places(&over_the_top(8), &narrow(2)), ["rcx"]);
        assert_eq!(places(&over_the_top(4), &narrow(2)), ["rcx"]);
    }

    #[test]
    fn a_value_wider_than_that_or_of_no_known_width_does_not() {
        assert_eq!(places(&over_the_top(16), &narrow(2)), ["slot 0"]);
        assert_eq!(places(&over_the_top(0), &narrow(2)), ["slot 0"]);
    }

    #[test]
    fn two_values_that_are_both_wanted_do_not() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).finish();
        func.build(block, opcode).uses(second, GPR).finish();

        assert_eq!(places(&func, &env()), ["rax", "rcx"]);
    }

    #[test]
    fn a_value_written_early_that_nothing_reads_still_holds_its_register() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let wanted = func.new_vreg(GPR);
        let spare = func.new_vreg(GPR);
        // A division: a remainder somebody wants, and a quotient nobody does. Both are written by
        // the one instruction and the quotient is written before the operands have been read.
        func.build(block, opcode)
            .def(wanted, GPR)
            .operand(Operand::write_early(spare, GPR))
            .finish();
        func.build(block, opcode).uses(wanted, GPR).finish();

        // Two registers, not one. A value nothing reads is still somewhere, and the instruction
        // that wrote it wrote the other one too, so the two cannot be the same place. Handing them
        // the same register loses the remainder, because the copy that takes the quotient out of
        // the register the machine insisted on goes on top of it. The quotient gets the first
        // register because it is written first, which is the whole of what early means.
        assert_eq!(places(&func, &env()), ["rcx", "rax"]);
    }

    #[test]
    fn the_value_wanted_longest_is_the_one_that_goes_to_the_stack() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let long = func.new_vreg(GPR);
        let short = func.new_vreg(GPR);
        let third = func.new_vreg(GPR);
        func.build(block, opcode).def(long, GPR).finish();
        func.build(block, opcode).def(short, GPR).finish();
        func.build(block, opcode).def(third, GPR).finish();
        func.build(block, opcode).uses(short, GPR).finish();
        func.build(block, opcode).uses(third, GPR).finish();
        func.build(block, opcode).uses(long, GPR).finish();

        // Two registers between three values. The one still wanted at the end of the function is
        // the one whose register is worth the most to everybody else, so it is the one that goes.
        assert_eq!(places(&func, &narrow(2)), ["slot 0", "rcx", "rax"]);
    }

    #[test]
    fn a_register_an_instruction_insists_on_goes_to_the_values_that_asked_for_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let across = func.new_vreg(GPR);
        let dividend = func.new_vreg(GPR);
        let quotient = func.new_vreg(GPR);
        let remainder = func.new_vreg(GPR);
        func.build(block, opcode).def(across, GPR).finish();
        func.build(block, opcode).def(dividend, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(quotient, GPR).with(Constraint::Fixed(RAX)))
            .operand(Operand::write_early(remainder, GPR).with(Constraint::Fixed(RDX)))
            .operand(Operand::read(dividend, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        func.build(block, opcode).uses(across, GPR).finish();

        // The value that has to be across the division is nowhere near `rax` or `rdx`, and each of
        // the three the division names is in the register the division asked for it in. The
        // dividend and the quotient share `rax` because the first is read where the second is
        // written, which is what a division does.
        assert_eq!(places(&func, &env()), ["rcx", "rax", "rax", "rdx"]);
    }

    /// A value read by an instruction that fills a register before it reads is kept out of that
    /// register, even though the read is the last thing the value is wanted for.
    ///
    /// The divisor of a division is the case. What the machine runs is `cltd` and then `idivl`, so
    /// `rdx` holds the top half of the dividend by the time the divisor is read, and a divisor
    /// sitting in `rdx` is read as the dividend's own sign bits. An early definition is how the
    /// target says a register goes before the operands are read, and this is where the allocator
    /// has to hear it, since a value dying at an instruction is otherwise free to sit in a
    /// register that instruction writes. tamnd/rucc#1232.
    #[test]
    fn a_value_that_dies_at_an_instruction_stays_out_of_what_it_fills_first() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let across = func.new_vreg(GPR);
        let dividend = func.new_vreg(GPR);
        let divisor = func.new_vreg(GPR);
        let remainder = func.new_vreg(GPR);
        func.build(block, opcode).def(across, GPR).finish();
        func.build(block, opcode).def(dividend, GPR).finish();
        func.build(block, opcode).def(divisor, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write_early(remainder, GPR).with(Constraint::Fixed(RDX)))
            .operand(Operand::read(dividend, GPR).with(Constraint::Fixed(RAX)))
            .operand(Operand::read(divisor, GPR))
            .finish();
        func.build(block, opcode).uses(across, GPR).uses(remainder, GPR).finish();

        // Four registers for four values, and the divisor takes the fourth. `rdx` is free
        // everywhere in this function except at the instruction that is about to fill it, which is
        // the one instruction the divisor is wanted at.
        assert_eq!(places(&func, &narrow(4)), ["rcx", "rax", "rsi", "rdx"]);
    }

    #[test]
    fn a_value_wanted_after_the_instruction_that_insists_does_not_get_that_register() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let dividend = func.new_vreg(GPR);
        let quotient = func.new_vreg(GPR);
        func.build(block, opcode).def(dividend, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(quotient, GPR).with(Constraint::Fixed(RAX)))
            .operand(Operand::read(dividend, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        func.build(block, opcode).uses(dividend, GPR).finish();

        // The hint is a preference and not a claim. The dividend would rather be in `rax` and
        // cannot be, because the division writes `rax` and the dividend is wanted afterwards, so
        // it takes the next register and the quotient keeps the one it was promised.
        assert_eq!(places(&func, &env()), ["rcx", "rax"]);
    }

    #[test]
    fn a_value_an_instruction_can_only_read_from_memory_is_on_the_stack() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(block, opcode).def(value, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::read(value, GPR).with(Constraint::Stack))
            .finish();

        assert_eq!(places(&func, &env()), ["slot 0"]);
    }

    #[test]
    fn a_two_address_instruction_writes_the_register_it_read_when_it_can() {
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

        // The addition reads the left value for the last time, so the answer goes where that was
        // and the instruction is two address without a move in front of it.
        assert_eq!(places(&func, &env()), ["rax", "rcx", "rax"]);
    }

    #[test]
    fn a_two_address_instruction_that_cannot_gets_a_register_nothing_it_reads_is_in() {
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
        func.build(block, opcode).uses(left, GPR).finish();

        // The left value is wanted afterwards, so the answer cannot have its register. It cannot
        // have the right one's either, because the rewrite is about to write a move into it before
        // the addition has read anything.
        assert_eq!(places(&func, &env()), ["rax", "rcx", "rdx"]);
    }

    #[test]
    fn an_answer_that_commutes_goes_over_the_source_that_is_finished_with() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode).def(right, GPR).finish();
        let add = func
            .build(block, opcode)
            .flags(Flags::COMMUTES)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(left, GPR).uses(sum, GPR).finish();

        // The same shape as the one above where the answer got a register of its own, except that
        // the addition reads its sources either way round, so the answer goes where the right one
        // was and the instruction is marked to be swapped.
        assert_eq!(places(&func, &env()), ["rax", "rcx", "rcx"]);
        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &env());
        assert_eq!(assignment.commuted(), [add]);

        // Once swapped, the instruction is an ordinary reuse of its first source, the checker and
        // the trace agree with it, and nothing has to be moved in front of it. The rewrite has put
        // the registers in by then, so the right one is `rcx` and the left one `rax`.
        let allocation = crate::run(&mut func, &env(), "f", true);
        assert!(allocation.edits.is_empty());
        let operands = &func[func[add].operands];
        let (first, second) = (operands[1].reg.phys(), operands[2].reg.phys());
        assert_eq!((first, second), (Some(RCX), Some(RAX)));
    }

    #[test]
    fn an_answer_that_commutes_takes_the_source_something_after_it_wants() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let left = func.new_vreg(GPR);
        let right = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        func.build(block, opcode).def(left, GPR).finish();
        func.build(block, opcode)
            .operand(Operand::write(right, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        let add = func
            .build(block, opcode)
            .flags(Flags::COMMUTES)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode)
            .operand(Operand::read(sum, GPR).with(Constraint::Fixed(RAX)))
            .finish();

        // Both sources are finished with, so either register would do for the answer. The one
        // reading it wants it in `rax`, which is where the right one already is, so it goes there
        // and nothing is moved in front of that reader.
        let names = places(&func, &env());
        assert_eq!(names[2], "rax");
        assert_ne!(names[0], "rax");
        let allocation = crate::run(&mut func, &env(), "f", true);
        assert!(allocation.edits.is_empty());
        assert_eq!(func[func[add].operands][1].reg.phys(), Some(RAX));
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
        let add = func
            .build(head, opcode)
            .flags(Flags::COMMUTES)
            .operand(Operand::write(next, GPR).with(Constraint::Reuse(1)))
            .uses(total, GPR)
            .uses(term, GPR)
            .finish();
        func.build(head, opcode)
            .operand(Operand::read(next, GPR).with(Constraint::Fixed(RSI)))
            .finish();
        *func.succs_mut(head) = vec![BlockCall::with(head, vec![next]), BlockCall::to(out)];

        // Both sources are finished with and the sum is wanted in `rsi` as well, but the loop
        // passes it back to `total`, so it goes where `total` is and the back edge has nothing to
        // copy.
        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &env());
        assert_eq!(assignment.place(next), assignment.place(total));
        assert!(!assignment.commuted().contains(&add));
    }

    #[test]
    fn an_answer_that_does_not_commute_leaves_its_sources_where_they_are() {
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
            .flags(Flags::COMMUTES)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(left, GPR)
            .uses(right, GPR)
            .finish();
        func.build(block, opcode).uses(right, GPR).finish();

        // The left one is finished with, so the answer goes over it as it always did, and there is
        // nothing to swap.
        assert_eq!(places(&func, &env()), ["rax", "rcx", "rax"]);
        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        assert!(assign(&func, &order, &live, &env()).commuted().is_empty());
    }

    #[test]
    fn a_value_live_across_a_whole_loop_holds_its_register_over_all_of_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let body = func.create_block();
        let carried = func.new_vreg(GPR);
        let inside = func.new_vreg(GPR);
        func.build(head, opcode).def(carried, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(body)];
        func.build(body, opcode).def(inside, GPR).finish();
        func.build(body, opcode).uses(inside, GPR).uses(carried, GPR).finish();
        *func.succs_mut(body) = vec![BlockCall::to(body)];

        // The value inside the loop cannot have the carried one's register, even though nothing
        // between the two definitions says so.
        assert_eq!(places(&func, &env()), ["rax", "rcx"]);
    }

    #[test]
    fn a_two_address_answer_already_live_does_not_take_the_register_it_read() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let head = func.create_block();
        let latch = func.create_block();
        let out = func.create_block();
        let source = func.new_vreg(GPR);
        let carried = func.new_vreg(GPR);
        func.build(head, opcode).def(source, GPR).finish();
        func.build(head, opcode).def(carried, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(latch)];
        // The bottom of the loop adds the source to the carried value and writes the answer back
        // over it, reusing the register the source is in. The next turn round redefines both.
        func.build(latch, opcode)
            .operand(Operand::write(carried, GPR).with(Constraint::Reuse(1)))
            .uses(source, GPR)
            .uses(carried, GPR)
            .finish();
        *func.succs_mut(latch) = vec![BlockCall::to(head), BlockCall::to(out)];
        func.build(out, opcode).uses(carried, GPR).finish();

        // The source is read here for the last time, which on its own is the shape the two address
        // shortcut is for, and taking it would be wrong. The carried value was written by the same
        // instruction on the last turn and is read by this one, so the two are both wanted where
        // the instruction reads and one register cannot hold both.
        assert_eq!(places(&func, &env()), ["rax", "rcx"]);

        // And the checker has to agree, since it excused this pair on the same reasoning and so
        // would have let the answer through.
        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &env());
        assert!(crate::check::check(&func, &order, &live, &assignment).is_empty());
    }

    #[test]
    fn a_two_address_answer_with_a_hole_in_front_of_it_does_not_take_its_other_operand() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let add = Opcode::new(names.intern("x64.add"));
        let entry = func.create_block();
        let head = func.create_block();
        let arm = func.create_block();
        let latch = func.create_block();
        let out = func.create_block();
        let seed = func.new_vreg(GPR);
        let sum = func.new_vreg(GPR);
        let inside = func.new_vreg(GPR);
        let loaded = func.new_vreg(GPR);
        func.build(entry, nop).def(seed, GPR).finish();
        func.build(entry, nop).def(sum, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(head)];
        func.build(head, nop).uses(sum, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(arm), BlockCall::to(latch)];
        func.build(arm, nop).def(inside, GPR).finish();
        func.build(arm, nop).uses(inside, GPR).finish();
        *func.succs_mut(arm) = vec![BlockCall::to(out)];
        func.build(latch, nop).def(loaded, GPR).finish();
        func.build(latch, add)
            .operand(Operand::write(sum, GPR).with(Constraint::Reuse(1)))
            .uses(seed, GPR)
            .uses(loaded, GPR)
            .finish();
        *func.succs_mut(latch) = vec![BlockCall::to(head), BlockCall::to(out)];

        // The answer is live in the entry and the head as well, and the arm between them is a hole
        // in it, so the piece the addition writes is not the first one. The value the addition reads
        // out of memory is still wanted where the addition reads, so it may not be in the register
        // the answer is about to be copied into, holes or no holes. tamnd/rucc#982.
        let places = places(&func, &env());
        assert_ne!(places[index(sum)], places[index(loaded)]);

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &env());
        assert!(crate::check::check(&func, &order, &live, &assignment).is_empty());
    }

    #[test]
    fn a_sum_a_loop_carries_round_keeps_its_register_past_an_arm_laid_out_after_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let nop = Opcode::new(names.intern("x64.nop"));
        let add = Opcode::new(names.intern("x64.add"));
        let entry = func.create_block();
        let head = func.create_block();
        let join = func.create_block();
        let arm = func.create_block();
        let out = func.create_block();
        let seed = func.new_vreg(GPR);
        let term = func.new_vreg(GPR);
        let next = func.new_vreg(GPR);
        func.build(entry, nop).def(seed, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::with(head, vec![seed])];
        let total = func.append_param(head, GPR);
        func.build(head, nop).def(term, GPR).finish();
        *func.succs_mut(head) = vec![BlockCall::to(join), BlockCall::to(arm)];
        func.build(join, add)
            .operand(Operand::write(next, GPR).with(Constraint::Reuse(1)))
            .uses(total, GPR)
            .uses(term, GPR)
            .finish();
        *func.succs_mut(join) =
            vec![BlockCall::with(head, vec![next]), BlockCall::with(out, vec![next])];
        // The default arm of a `switch`, laid out after the addition it joins back in above. The
        // sum is live in it and nothing in it or after it reads the sum again.
        func.build(arm, nop).def(term, GPR).finish();
        *func.succs_mut(arm) = vec![BlockCall::to(join)];
        let result = func.append_param(out, GPR);
        func.build(out, nop).uses(result, GPR).finish();

        // The addition reads the sum for the last time, so the new sum goes where the old one was
        // and the edge back to the top of the loop has nothing to move. tamnd/rucc#1965.
        let places = places(&func, &env());
        assert_eq!(places[index(next)], places[index(total)]);

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &env());
        assert!(crate::check::check(&func, &order, &live, &assignment).is_empty());
    }

    /// Two blocks the entry chooses between, with the one the clobber is in written first. The two
    /// values written in the entry block are read in the other one, so their ranges cover the
    /// clobber whether or not either of them ever reaches it.
    fn arms(reaches: bool) -> Func {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let arm = func.create_block();
        let tail = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(entry, opcode).def(first, GPR).finish();
        func.build(entry, opcode).def(second, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(arm), BlockCall::to(tail)];
        // What a call looks like here: an instruction writing the registers the convention says it
        // destroys, named outright so that nothing else may be in them.
        func.build(arm, opcode).operand(Operand::write(Reg::physical(RAX), GPR)).finish();
        *func.succs_mut(arm) = if reaches { vec![BlockCall::to(tail)] } else { Vec::new() };
        func.build(tail, opcode).uses(first, GPR).uses(second, GPR).finish();
        func
    }

    #[test]
    fn a_register_a_clobber_takes_beats_the_stack_for_a_value_not_live_in_that_block() {
        let func = arms(false);

        // Two registers between two values, and a clobber in the arm that takes the first of them.
        // The intervals around both values cover the clobber, since the arm is written between the
        // two blocks they are live in, and the arm is a hole in both of their areas. So a value has
        // `rax` rather than a stack slot: the arm is a block its own path never goes through.
        // tamnd/rucc#982. It is the first value since tamnd/rucc#2202, because a clobber where a
        // value is dead leaves the register clear for it.
        assert_eq!(places(&func, &narrow(2)), ["rax", "rcx"]);

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &narrow(2));
        assert!(crate::check::check(&func, &order, &live, &assignment).is_empty());
    }

    #[test]
    fn a_register_a_clobber_takes_is_not_free_to_a_value_that_is_live_there() {
        let func = arms(true);

        // The same blocks with an edge from the arm to the tail, which is all it takes: both values
        // now arrive at the read either way, so the clobber is on a path they are live over and the
        // one register left has to do for both of them.
        assert_eq!(places(&func, &narrow(2)), ["rcx", "slot 0"]);
    }

    /// A value and the instruction that destroys a register, written one after the other, with the
    /// value read by that instruction or by the one after it.
    fn dies_at_the_clobber(here: bool) -> Func {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(entry, opcode).def(value, GPR).finish();
        let call = func.build(entry, opcode).operand(Operand::write(Reg::physical(RAX), GPR));
        if here {
            call.uses(value, GPR).finish();
        } else {
            call.finish();
            func.build(entry, opcode).uses(value, GPR).finish();
        }
        func
    }

    /// A value whose last read is the instruction that destroys a register may be in that register,
    /// because the instruction reads what it is handed before it writes anything.
    ///
    /// The call is what this is about, and the value a call is passed is the case: seven registers
    /// on this machine are destroyed by one, every argument dies at the call that reads it, and
    /// refusing all seven to those values left them taking a callee saved register for a life two
    /// instructions long and paying for it in the prologue and the epilogue. tamnd/rucc#1232.
    #[test]
    fn a_value_that_dies_where_a_register_is_destroyed_may_be_in_that_register() {
        let func = dies_at_the_clobber(true);
        assert_eq!(places(&func, &narrow(1)), ["rax"]);

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &narrow(1));
        assert!(crate::check::check(&func, &order, &live, &assignment).is_empty());
    }

    /// And one read later than that is one the instruction really does destroy, which is the same
    /// function with the read moved down by one instruction.
    #[test]
    fn a_value_read_after_the_instruction_that_destroys_a_register_is_not_in_it() {
        let func = dies_at_the_clobber(false);
        assert_eq!(places(&func, &narrow(1)), ["slot 0"]);
    }

    #[test]
    fn a_hint_is_followed_when_the_register_is_clear_and_not_when_it_is_merely_allowed() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let mid = func.create_block();
        let tail = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(entry, opcode).def(first, GPR).finish();
        func.build(entry, opcode).def(second, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(mid), BlockCall::to(tail)];
        // Two arms, each ending in an instruction that wants its own value in `rax`, which is what
        // a return out of either side of a branch looks like.
        func.build(mid, opcode)
            .operand(Operand::read(second, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        func.build(tail, opcode)
            .operand(Operand::read(first, GPR).with(Constraint::Fixed(RAX)))
            .finish();

        // The first value is hinted at `rax` and does not get it, because the other arm wants `rax`
        // for the other value and the first value's range reaches that far. Following the hint here
        // would save a move in the tail and cost one in the middle, and the second value gets `rax`
        // with nothing moved anywhere instead.
        assert_eq!(places(&func, &env()), ["rcx", "rax"]);
    }

    #[test]
    fn a_value_living_in_a_hole_of_another_gets_the_same_register() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let arm = func.create_block();
        let tail = func.create_block();
        let across = func.new_vreg(GPR);
        let inside = func.new_vreg(GPR);
        func.build(entry, opcode).def(across, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(arm), BlockCall::to(tail)];
        func.build(arm, opcode).def(inside, GPR).finish();
        func.build(arm, opcode).uses(inside, GPR).finish();
        func.build(tail, opcode).uses(across, GPR).finish();

        // One register between the two of them, and one register is enough. Nothing in the arm can
        // reach the read in the tail, so the value the arm makes is welcome to the register the
        // value crossing the function is in. The interval around that value covers the arm and the
        // value is nowhere near it, which is what used to send one of the two to the stack.
        // tamnd/rucc#982.
        assert_eq!(places(&func, &narrow(1)), ["rax", "rax"]);

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &narrow(1));
        assert_eq!(assignment.spilled(), 0);
        assert!(crate::check::check(&func, &order, &live, &assignment).is_empty());
    }

    /// The blocks of [`arms`] with no edge from the arm to the tail, where the arm wants `rax` for
    /// a value of its own rather than destroying it.
    fn handed_in_the_arm() -> Func {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let entry = func.create_block();
        let arm = func.create_block();
        let tail = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let own = func.new_vreg(GPR);
        func.build(entry, opcode).def(first, GPR).finish();
        func.build(entry, opcode).def(second, GPR).finish();
        *func.succs_mut(entry) = vec![BlockCall::to(arm), BlockCall::to(tail)];
        func.build(arm, opcode).def(own, GPR).finish();
        func.build(arm, opcode)
            .operand(Operand::read(own, GPR).with(Constraint::Fixed(RAX)))
            .finish();
        func.build(tail, opcode).uses(first, GPR).uses(second, GPR).finish();
        func
    }

    #[test]
    fn a_register_another_value_is_handed_is_the_last_one_offered_rather_than_the_first() {
        // With a register to spare the value takes the spare one. Being allowed a register some
        // instruction insists on is not the same as it being free: the instruction has to be handed
        // it in the end, and what hands it over is a move.
        let func = handed_in_the_arm();
        assert_eq!(places(&func, &narrow(3)), ["rcx", "rdx", "rax"]);
    }

    #[test]
    fn a_register_a_clobber_takes_is_clear_to_a_value_dead_where_it_is_taken() {
        let func = arms(false);

        // A clobber hands the register to nobody, so there is nothing to move in and nothing to
        // pay. The first value takes `rax` though the arm destroys it, since it is dead there, and
        // neither value needs a register past the third. tamnd/rucc#2202.
        assert_eq!(places(&func, &narrow(3)), ["rax", "rcx"]);
    }

    #[test]
    fn a_frame_says_what_each_of_its_slots_is_for() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let assignment = assign(&func, &order, &live, &narrow(1));
        assert_eq!(assignment.spilled(), 1);
        assert_eq!(assignment.slots(), [GPR]);
        // A register that is already a register is where it is, and this has nothing to say about
        // it.
        assert_eq!(assignment.place(Reg::physical(RCX)), None);
        assert_eq!(env().scratch(GPR), [R13, R14, R15]);
    }

    /// Pieces of a value, from pairs of points.
    fn ranges(pairs: &[(Point, Point)]) -> Vec<Range> {
        pairs.iter().map(|&(start, end)| Range { start, end }).collect()
    }

    #[test]
    fn the_pieces_of_a_register_answer_what_a_walk_over_its_values_would() {
        // Three values that take turns in one register, the first with a hole the third sits in.
        let held = [
            (Reg::virtual_reg(0), ranges(&[(0, 4), (20, 30)])),
            (Reg::virtual_reg(1), ranges(&[(5, 9)])),
            (Reg::virtual_reg(2), ranges(&[(10, 19), (31, 40)])),
        ];
        let mut pieces = Pieces::default();
        for (reg, list) in &held {
            for &piece in list {
                pieces.insert(piece, *reg);
            }
        }
        assert!(!pieces.broken);
        let asked = [
            ranges(&[(41, 50)]),
            ranges(&[(40, 50)]),
            ranges(&[(9, 9)]),
            ranges(&[(3, 3), (41, 42)]),
            ranges(&[(50, 60)]),
            ranges(&[(15, 15)]),
        ];
        for list in &asked {
            let area = Area::of_pieces(list);
            for except in [None, Some(Reg::virtual_reg(0)), Some(Reg::virtual_reg(2))] {
                let walked = held.iter().any(|(reg, pieces)| {
                    Some(*reg) != except && Area::of_pieces(pieces).overlaps(area)
                });
                assert_eq!(pieces.touch(area, except), walked, "{list:?} except {except:?}");
            }
        }
    }

    #[test]
    fn the_owners_of_the_pieces_an_area_touches_are_the_values_a_walk_would_find() {
        let held = [
            (Reg::virtual_reg(0), ranges(&[(0, 4), (20, 30)])),
            (Reg::virtual_reg(1), ranges(&[(5, 9)])),
            (Reg::virtual_reg(2), ranges(&[(10, 19), (30, 40)])),
        ];
        let mut pieces = Pieces::default();
        for (reg, list) in &held {
            for &piece in list {
                pieces.insert(piece, *reg);
            }
        }
        let asked = [
            ranges(&[(41, 50)]),
            ranges(&[(30, 30)]),
            ranges(&[(3, 12)]),
            ranges(&[(3, 3), (25, 42)]),
            ranges(&[(0, 50)]),
        ];
        for list in &asked {
            let area = Area::of_pieces(list);
            let walked: Vec<Reg> = held
                .iter()
                .filter(|(_, pieces)| Area::of_pieces(pieces).overlaps(area))
                .map(|&(reg, _)| reg)
                .collect();
            assert_eq!(pieces.owners(area), Some(walked), "{list:?}");
        }
        pieces.insert(Range { start: 2, end: 3 }, Reg::virtual_reg(3));
        assert_eq!(pieces.owners(Area::of_pieces(&asked[0])), None);
    }

    #[test]
    fn pieces_that_end_out_of_order_are_marked() {
        let mut pieces = Pieces::default();
        pieces.insert(Range { start: 0, end: 10 }, Reg::virtual_reg(0));
        pieces.insert(Range { start: 12, end: 20 }, Reg::virtual_reg(1));
        assert!(!pieces.broken);
        // Inside the first, which two values in one register never are.
        pieces.insert(Range { start: 2, end: 3 }, Reg::virtual_reg(2));
        assert!(pieces.broken);
    }

    #[test]
    fn a_value_taken_out_of_a_register_leaves_its_pieces_with_it() {
        let mut pieces = Pieces::default();
        let (first, second) = (Reg::virtual_reg(0), Reg::virtual_reg(1));
        pieces.insert(Range { start: 0, end: 10 }, first);
        pieces.insert(Range { start: 12, end: 20 }, second);
        let asked = ranges(&[(15, 16)]);
        assert!(pieces.touch(Area::of_pieces(&asked), None));
        pieces.remove(Range { start: 12, end: 20 }, second);
        assert!(!pieces.touch(Area::of_pieces(&asked), None));
        pieces.drop_before(11);
        assert!(pieces.list.is_empty());
    }
}
