//! One stack slot allocator: every byte a function asks for itself, placed together.
//!
//! Design: `spec/optimizer/36-lowering-and-isel.md` section 36.7.
//!
//! A frame holds two kinds of thing the function asked for. Locals are what an `alloca` becomes and
//! the lowering knows about them before anything else runs. Spill slots are what the allocator
//! gives a value it ran out of registers for, and nothing knows how many of those there are until
//! it has finished. Placed apart, the frame is the sum of the two areas. Placed together it is the
//! most either of them needs at any one moment, because two things that are never both wanted can
//! be the same bytes. That is the same answer the allocator gives about registers and it is the
//! same reason.
//!
//! This runs after allocation and reads the allocator's own liveness rather than working one out.
//! Running before it would mean guessing which values are spilled, and a guess has to be either
//! conservative or wrong. Asking again afterwards would mean two answers about one function that
//! are free to disagree, and the one the machine runs is the allocator's.
//!
//! # What a cell is
//!
//! A [`Cell`] is a run of bytes in the frame, as wide and as aligned as the widest and strictest
//! thing in it. [`Slots`] says which cell every local and every spill slot went in, and
//! [`crate::frame`] is what turns cells into offsets. Nothing else changes: an instruction reading
//! a local still asks the frame where that local is and gets back an offset, and two locals sharing
//! a cell get the same one.
//!
//! # What may share
//!
//! A spill slot holds one value, so where the slot is wanted is where that value is live, and the
//! allocator has already said where that is.
//!
//! A local is harder, because what a local is wanted over is not the live range of anything. The
//! bytes are reached through an address, the address is a value like any other, and the bytes go on
//! meaning something for exactly as long as anything can still come by that address. So the
//! question asked here is where the address gets to, and the answer has to be the whole of it or
//! the local does not share at all. [`reach`] asks it. An address read as the base of a load or a
//! store is a read of the local at that instruction and goes no further. An address read by another
//! address computation is the same local under a second name and is followed. An address read any
//! other way is one this pass cannot follow to the end, and the local it belongs to is left out.
//!
//! Left out is therefore the answer for every local whose address is handed to a call, stored into
//! memory, or carried between blocks as an argument. That is what section 36.7 means by an address
//! taken local: not one the program wrote an `&` in front of, which is a question the types
//! answered and the types are gone by here, but one whose bytes something can reach at a moment
//! liveness does not know about.
//!
//! # The moves count too
//!
//! Where a spilled value is live is not quite everywhere its slot is touched. The store that fills
//! the slot goes after the instruction that wrote the value, the reload that empties it goes before
//! the instruction that reads it, and the moves an edge turns into go at the end of a block or the
//! start of one, none of which is a point the value is live at. The edge moves are the ones that
//! matter: the sequencer put them in an order that works because it was told every place in them
//! was a different place, and two slots it was told apart are two this pass must not put together
//! behind its back.
//!
//! So the moves are read as well as the liveness. Every edit that names a slot puts a point either
//! side of where it stands into that slot's area, which is the gap between two points the edit
//! really sits in, and after that the question is the same question everywhere else in this file.
//!
//! # How big it is allowed to get
//!
//! Fitting each thing into the first cell it does not clash with compares it against the cells so
//! far, so a function with a very large number of them costs the square of that number. Past
//! [`CROWDED`] the frame is laid out the old way, one cell each, because a function with that many
//! slots is rare and a compile that takes a visible pause over one is not worth the bytes.

use std::collections::{HashMap, HashSet};

use rucc_base::Interner;
use rucc_mir::{Func, Inst, Opcode, Reg};
use rucc_regalloc::Allocation;
use rucc_regalloc::assign::Place;
use rucc_regalloc::live::{Area, Live, Range};
use rucc_regalloc::order::Order;
use rucc_regalloc::rewrite::At;
use rucc_target::FrameInsts;

use crate::frame::Local;

/// How many locals and spill slots a function may have before its frame is laid out the old way.
///
/// See the note on crowding in the module documentation. Over the corpus the largest function has
/// far fewer than this, so the limit is a guard against a generated file rather than something the
/// ordinary path meets.
pub const CROWDED: usize = 2048;

/// One run of bytes in the frame, holding one local, one spill slot, or several of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// How many bytes of it there are, which is as many as the largest thing in it needs.
    pub size: u32,
    /// What its address has to be a multiple of, which is the strictest thing in it.
    pub align: u32,
}

/// Which cell of the frame every local and every spill slot of a function is in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Slots {
    cells: Vec<Cell>,
    locals: Vec<usize>,
    slots: Vec<usize>,
}

impl Slots {
    /// The frame with nothing sharing anything: a cell of its own for every local and every spill
    /// slot, in the order the two lists are in.
    ///
    /// This is the layout there was before this pass, and it is what a frame gets when nothing has
    /// asked for sharing and what it falls back to on a function with too many slots to pair up
    /// cheaply.
    #[must_use]
    pub fn apart(locals: &[Local], widths: &[u32]) -> Self {
        let mut cells = Vec::with_capacity(locals.len() + widths.len());
        for &Local { size, align } in locals {
            cells.push(Cell { size, align });
        }
        for &width in widths {
            cells.push(Cell { size: width, align: width });
        }
        Self {
            locals: (0..locals.len()).collect(),
            slots: (locals.len()..cells.len()).collect(),
            cells,
        }
    }

    /// The frame with everything that can share sharing, worked out from the allocator's liveness.
    ///
    /// `reach` is what [`reach`] said about this function before the allocator ran, `widths` is how
    /// many bytes a slot of each of the allocation's spill slots takes, and `locals` is the
    /// function's own objects in the order the lowering recorded them.
    #[must_use]
    pub fn share(reach: &Reach, allocation: &Allocation, locals: &[Local], widths: &[u32]) -> Self {
        if locals.len() + widths.len() > CROWDED {
            return Self::apart(locals, widths);
        }
        let mut wants = Vec::with_capacity(locals.len() + widths.len());
        for (local, &Local { size, align }) in locals.iter().enumerate() {
            let area = reach.area(local, &allocation.live, &allocation.order);
            wants.push(Want { what: What::Local(local), size, align, area });
        }
        let held = spilled(allocation, widths.len());
        let moved = moved(allocation, widths.len());
        for (slot, &width) in widths.iter().enumerate() {
            let area = held[slot]
                .and_then(|reg| allocation.live.area(reg))
                .map(|live| merged(live.pieces().chain(moved[slot].iter().copied())));
            wants.push(Want { what: What::Slot(slot), size: width, align: width, area });
        }
        fit(wants, locals.len(), widths.len())
    }

    /// The cells the frame is made of, which is what [`crate::frame`] places.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Which cell a local is in.
    #[must_use]
    pub fn local(&self, local: usize) -> Option<usize> {
        self.locals.get(local).copied()
    }

    /// Which cell a spill slot is in.
    #[must_use]
    pub fn slot(&self, slot: u32) -> Option<usize> {
        self.slots.get(usize::try_from(slot).ok()?).copied()
    }

    /// How many cells were saved by sharing, which is how many things went in beside something
    /// else.
    ///
    /// This is a count rather than a number of bytes, because how many bytes it saved is the
    /// difference between two frames and a frame is not worked out here.
    #[must_use]
    pub fn saved(&self) -> usize {
        self.locals.len() + self.slots.len() - self.cells.len()
    }
}

/// One thing that wants bytes in the frame, and everywhere it wants them.
#[derive(Debug)]
struct Want {
    what: What,
    size: u32,
    align: u32,
    /// Where it is wanted, or `None` for one this pass could not follow, which shares with nothing.
    area: Option<Vec<Range>>,
}

/// Which of the two lists a want came off.
#[derive(Debug, Clone, Copy)]
enum What {
    Local(usize),
    Slot(usize),
}

/// Fits every want into the fewest cells, largest and strictest first.
///
/// Largest first because a cell only ever grows to hold what goes in it, and starting with the
/// small ones means growing a cell to several times the size of the thing that opened it, which
/// leaves the same bytes taken and a worse chance for everything after. The order is settled
/// entirely by the want rather than partly by which came first, so the same function lays out the
/// same way every time.
fn fit(mut wants: Vec<Want>, locals: usize, slots: usize) -> Slots {
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by_key(|&want| {
        let Want { size, align, .. } = wants[want];
        (std::cmp::Reverse(align), std::cmp::Reverse(size), want)
    });

    let mut cells: Vec<Cell> = Vec::new();
    // `None` is a cell nothing else may go in, which is what a thing this pass could not follow
    // opens. A cell with an area is one anything that does not clash with that area may join.
    let mut busy: Vec<Option<Vec<Range>>> = Vec::new();
    let mut of_local = vec![0; locals];
    let mut of_slot = vec![0; slots];
    for want in order {
        let Want { what, size, align, area } = std::mem::replace(
            &mut wants[want],
            Want { what: What::Local(0), size: 0, align: 0, area: None },
        );
        let into = area.as_ref().and_then(|area| {
            (0..cells.len())
                .find(|&cell| busy[cell].as_ref().is_some_and(|busy| !clashes(busy, area)))
        });
        let cell = match into {
            Some(cell) => {
                cells[cell].size = cells[cell].size.max(size);
                cells[cell].align = cells[cell].align.max(align);
                let held = busy[cell].take().unwrap_or_default();
                busy[cell] = Some(merged(held.into_iter().chain(area.into_iter().flatten())));
                cell
            }
            None => {
                cells.push(Cell { size, align });
                busy.push(area);
                cells.len() - 1
            }
        };
        match what {
            What::Local(local) => of_local[local] = cell,
            What::Slot(slot) => of_slot[slot] = cell,
        }
    }
    Slots { cells, locals: of_local, slots: of_slot }
}

/// Which value the allocator put in each spill slot, by slot number.
///
/// A slot holds one value, because the allocator takes a fresh one every time it spills, so this is
/// the assignment read the other way round.
fn spilled(allocation: &Allocation, slots: usize) -> Vec<Option<Reg>> {
    let mut held = vec![None; slots];
    for (reg, place) in allocation.assignment.placed() {
        if let Place::Slot(slot) = place {
            if let Some(at) = usize::try_from(slot).ok().and_then(|slot| held.get_mut(slot)) {
                *at = Some(reg);
            }
        }
    }
    held
}

/// What carries the address of each of a function's locals, or `None` for one whose address gets
/// away somewhere this pass cannot follow.
///
/// Worked out before allocation, because it is a question about values and a value is written once
/// only until the allocator's rewrite has been through. Read after it, because that is when the
/// liveness these names are looked up in exists.
#[derive(Debug, Clone, Default)]
pub struct Reach {
    through: Vec<Option<Carried>>,
}

impl Reach {
    /// Everywhere a local's bytes may be reached, or `None` for one that shares with nothing.
    fn area(&self, local: usize, live: &Live, order: &Order) -> Option<Vec<Range>> {
        let held = self.through.get(local)?.as_ref()?;
        let mut pieces: Vec<Range> = Vec::new();
        for &reg in &held.regs {
            pieces.extend(live.area(reg).into_iter().flat_map(Area::pieces));
        }
        for &inst in &held.at {
            pieces.push(Range { start: order.early(inst), end: order.late(inst) });
        }
        Some(merged(pieces))
    }

    /// Whether a local may share its bytes with anything, which is what the tests ask.
    #[must_use]
    pub fn shares(&self, local: usize) -> bool {
        self.through.get(local).is_some_and(Option::is_some)
    }
}

/// Everywhere one local is reached from.
#[derive(Debug, Clone, Default)]
struct Carried {
    /// The values that hold its address.
    regs: Vec<Reg>,
    /// The instructions that reach it with no value in between, which is what an address folded
    /// into its reader leaves behind.
    at: Vec<Inst>,
}

/// Follows the address of every local of a function as far as it goes.
///
/// `addresses` is the list [`crate::lower`] built and [`crate::fold`] rewrote, which says which
/// instruction carries the address of which local. `count` is how many locals there are, since a
/// local nothing on that list names is one this has no account of rather than one nothing touches.
///
/// Run after the fold and before allocation. After the fold because an address that ended up inside
/// its reader is an address no value holds and this has to see it that way. Before allocation
/// because every answer here is about a virtual register, and the rewrite the allocator ends with
/// is what stops there being one.
#[must_use]
pub fn reach(
    func: &Func,
    addresses: &[(Inst, usize)],
    count: usize,
    insts: &FrameInsts,
    names: &mut Interner,
) -> Reach {
    let lea = Opcode::new(names.intern(&format!("{}{}", insts.prefix, insts.lea)));
    let mut through: Vec<Option<Carried>> = vec![None; count];
    for &(inst, local) in addresses {
        let Some(held) = through.get_mut(local) else { continue };
        let held = held.get_or_insert_with(Carried::default);
        // Either the `lea` the lowering wrote, whose result is the address and goes on from here,
        // or a reader the fold put the address inside, which touches the local where it stands and
        // hands nothing on. The opcode is the whole of the difference: a reader that is itself a
        // `lea` really does hand an address on, and this reads it as one.
        if func[inst].opcode == lea {
            match def(func, inst) {
                Some(reg) => held.regs.push(reg),
                None => {
                    through[local] = None;
                    continue;
                }
            }
        }
        // On the list either way, so that a local whose address nothing reads is still wanted where
        // the address of it was taken rather than nowhere at all.
        held.at.push(inst);
    }

    let readers = readers(func);
    let crossing = crossing(func);
    for held in &mut through {
        if let Some(carried) = held.take() {
            *held = follow(func, lea, &readers, &crossing, carried);
        }
    }
    Reach { through }
}

/// Follows every address a local is reached through to every value that address becomes.
///
/// Gives back nothing for a local whose address is read some way this cannot account for, which is
/// any way but as the base or the index of a memory operand. A call argument is one of those, a
/// value stored into memory is another, and so is a value carried into a block as an argument,
/// which is the one that is not an operand at all.
fn follow(
    func: &Func,
    lea: Opcode,
    readers: &HashMap<Reg, Vec<Inst>>,
    crossing: &HashSet<Reg>,
    mut held: Carried,
) -> Option<Carried> {
    let mut seen: HashSet<Reg> = held.regs.iter().copied().collect();
    let mut queue = held.regs.clone();
    while let Some(reg) = queue.pop() {
        if crossing.contains(&reg) {
            return None;
        }
        for &inst in readers.get(&reg).map(Vec::as_slice).unwrap_or_default() {
            if !addressed(func, inst, reg) {
                return None;
            }
            if func[inst].opcode == lea {
                let next = def(func, inst)?;
                if seen.insert(next) {
                    held.regs.push(next);
                    queue.push(next);
                }
            }
        }
    }
    Some(held)
}

/// Whether every read of a value by an instruction is as part of the address it works on.
///
/// Anything else is a read this pass cannot follow: the value has gone somewhere that is not an
/// address into this frame any more, and where its bytes are reached from afterwards is no longer a
/// question about liveness.
fn addressed(func: &Func, inst: Inst, reg: Reg) -> bool {
    let data = &func[inst];
    let Some(mem) = data.mem else { return false };
    let amode = func[mem];
    func[data.operands].iter().enumerate().all(|(at, operand)| {
        if operand.reg != reg || operand.role.is_def() {
            return true;
        }
        let at = u8::try_from(at).ok();
        at.is_some() && (amode.base == at || amode.index == at)
    })
}

/// The one virtual register an instruction writes, or nothing when it writes none or several.
fn def(func: &Func, inst: Inst) -> Option<Reg> {
    let mut found = None;
    for operand in &func[func[inst].operands] {
        if !operand.role.is_def() {
            continue;
        }
        if operand.reg.number().is_none() || found.is_some() {
            return None;
        }
        found = Some(operand.reg);
    }
    found
}

/// Which instructions read each virtual register.
fn readers(func: &Func) -> HashMap<Reg, Vec<Inst>> {
    let mut readers: HashMap<Reg, Vec<Inst>> = HashMap::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                if operand.role.is_def() || operand.reg.number().is_none() {
                    continue;
                }
                let at = readers.entry(operand.reg).or_default();
                if at.last() != Some(&inst) {
                    at.push(inst);
                }
            }
        }
    }
    readers
}

/// Every virtual register that goes between blocks, as an argument an edge carries or as a
/// parameter one arrives in.
///
/// These are the reads that are not operands, so the walk above would not see them, and an address
/// that goes round a loop this way is one whose local is left out rather than one followed into a
/// second name.
fn crossing(func: &Func) -> HashSet<Reg> {
    let mut crossing = HashSet::new();
    for block in func.blocks() {
        crossing.extend(func[block].params.iter().map(|param| param.reg));
        for call in &func[block].succs {
            crossing.extend(call.args.iter().copied());
        }
    }
    crossing
}

/// Where the moves the allocator handed back touch each slot of the frame.
///
/// A point either side of where each of them stands, which is the gap between two points the move
/// really goes in. See the note on the moves in the module documentation.
fn moved(allocation: &Allocation, slots: usize) -> Vec<Vec<Range>> {
    let order = &allocation.order;
    let mut moved = vec![Vec::new(); slots];
    for edit in &allocation.edits {
        let at = match edit.at {
            At::Before(inst) => order.early(inst),
            At::After(inst) => order.late(inst),
            At::StartOf(block) => order.start(block),
            At::EndOf(block) => order.end(block),
        };
        let around =
            Range { start: at.saturating_sub(1), end: at.saturating_add(1).min(order.points()) };
        for place in [edit.mov.to, edit.mov.from] {
            if let Place::Slot(slot) = place {
                if let Some(at) = usize::try_from(slot).ok().and_then(|slot| moved.get_mut(slot)) {
                    at.push(around);
                }
            }
        }
    }
    moved
}

/// The same stretches of the function, in order, with everything that touches joined up.
fn merged(pieces: impl IntoIterator<Item = Range>) -> Vec<Range> {
    let mut pieces: Vec<Range> = pieces.into_iter().collect();
    pieces.sort_by_key(|piece| (piece.start, piece.end));
    let mut merged: Vec<Range> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        match merged.last_mut() {
            Some(last) if piece.start <= last.end => last.end = last.end.max(piece.end),
            _ => merged.push(piece),
        }
    }
    merged
}

/// Whether two stretches of a function are both wanted anywhere, which is what stops two things
/// sharing a cell.
///
/// Both lists are in order and neither is long, so this walks them together and stops at the first
/// pair that touches rather than comparing every piece with every other.
fn clashes(one: &[Range], two: &[Range]) -> bool {
    let (mut mine, mut theirs) = (0, 0);
    while mine < one.len() && theirs < two.len() {
        if one[mine].overlaps(two[theirs]) {
            return true;
        }
        if one[mine].end < two[theirs].end {
            mine += 1;
        } else {
            theirs += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{Block, BlockCall, Mem, Operand};
    use rucc_regalloc::assign::Env;
    use rucc_target::x86_64::{FRAME, GPR, REGS, SYSV};

    use super::*;
    use crate::frame::{Frame, Layout};

    /// A function being built, with the names and the opcodes a test needs to hand.
    struct Building {
        names: Interner,
        func: Func,
        lea: Opcode,
        nop: Opcode,
        addresses: Vec<(Inst, usize)>,
    }

    impl Building {
        /// An empty function of one block.
        fn new() -> (Self, Block) {
            let mut names = Interner::new();
            let func = Func::new(names.intern("f"));
            let lea = Opcode::new(names.intern(&format!("{}{}", FRAME.prefix, FRAME.lea)));
            let nop = Opcode::new(names.intern("x64.nop"));
            let mut building = Self { names, func, lea, nop, addresses: Vec::new() };
            let block = building.func.create_block();
            (building, block)
        }

        /// The address of a local, taken the way the lowering takes one: a `lea` off the stack
        /// pointer with nothing in its displacement yet.
        fn local(&mut self, block: Block, which: usize) -> Reg {
            let sp = Operand::read(Reg::physical(SYSV.stack_pointer), GPR);
            let reg = self.func.new_vreg(GPR);
            let inst = self.func.build(block, self.lea).def(reg, GPR).mem(Mem::at(sp)).finish();
            self.addresses.push((inst, which));
            reg
        }

        /// An instruction that reads a local through its address, which is every ordinary use of
        /// one.
        fn through(&mut self, block: Block, addr: Reg) {
            let at = Operand::read(addr, GPR);
            self.func.build(block, self.nop).mem(Mem::at(at)).finish();
        }

        /// An instruction that reads a value as a value, which is what handing an address to a
        /// call looks like from here.
        fn held(&mut self, block: Block, reg: Reg) {
            self.func.build(block, self.nop).uses(reg, GPR).finish();
        }

        /// A value written and then read, which is one more thing wanting a register in between.
        fn value(&mut self, block: Block) -> Reg {
            let reg = self.func.new_vreg(GPR);
            self.func.build(block, self.nop).def(reg, GPR).finish();
            reg
        }

        /// What this pass says about the function, and then what the allocator says, in that
        /// order because the first question is about values and the second takes them away.
        fn allocate(&mut self, locals: usize, registers: usize) -> (Reach, Allocation) {
            let reach = reach(&self.func, &self.addresses, locals, &FRAME, &mut self.names);
            let env =
                Env::new().with(GPR, &SYSV.int_order[..registers], &SYSV.int_order[registers..]);
            let allocation = rucc_regalloc::run(&mut self.func, &env, "test");
            (reach, allocation)
        }
    }

    /// A local of one word, which is what most of them are.
    const WORD: Local = Local { size: 8, align: 8 };

    #[test]
    fn two_locals_that_are_never_both_wanted_are_the_same_bytes() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.through(block, first);
        let second = building.local(block, 1);
        building.through(block, second);
        let (reach, allocation) = building.allocate(2, 4);

        let plan = Slots::share(&reach, &allocation, &[WORD, WORD], &[]);
        assert_eq!(plan.cells().len(), 1, "one run of bytes for the two of them");
        assert_eq!(plan.local(0), plan.local(1));
        assert_eq!(plan.saved(), 1);
    }

    #[test]
    fn two_locals_that_are_both_wanted_at_once_are_not() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        let second = building.local(block, 1);
        // Both addresses are live at this point, which is the whole of the difference from the
        // test above.
        building.through(block, first);
        building.through(block, second);
        let (reach, allocation) = building.allocate(2, 4);

        let plan = Slots::share(&reach, &allocation, &[WORD, WORD], &[]);
        assert_eq!(plan.cells().len(), 2);
        assert_ne!(plan.local(0), plan.local(1));
        assert_eq!(plan.saved(), 0);
    }

    #[test]
    fn a_local_and_a_spilled_value_that_do_not_meet_share_one_run_of_bytes() {
        let (mut building, block) = Building::new();
        let addr = building.local(block, 0);
        building.through(block, addr);
        // Three values wanted at once with two registers to hand out, after the local is finished
        // with, so what spills is spilled over a stretch the local is not wanted over.
        let values: Vec<Reg> = (0..3).map(|_| building.value(block)).collect();
        for &reg in &values {
            building.held(block, reg);
        }
        let (reach, allocation) = building.allocate(1, 2);

        assert_eq!(allocation.assignment.spilled(), 1, "one value went to the stack");
        let plan = Slots::share(&reach, &allocation, &[WORD], &[8]);
        assert_eq!(plan.cells().len(), 1);
        assert_eq!(plan.local(0), plan.slot(0));
    }

    #[test]
    fn a_local_whose_address_is_handed_to_something_shares_with_nothing() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        // Read as a value rather than as an address, which is what a call argument is and is the
        // point past which this pass cannot say where the bytes are reached from.
        building.held(block, first);
        let second = building.local(block, 1);
        building.through(block, second);
        let (reach, allocation) = building.allocate(2, 4);

        assert!(!reach.shares(0), "an address that got away");
        assert!(reach.shares(1));
        let plan = Slots::share(&reach, &allocation, &[WORD, WORD], &[]);
        assert_eq!(plan.cells().len(), 2);
        assert_ne!(plan.local(0), plan.local(1));
    }

    #[test]
    fn a_local_whose_address_is_carried_into_a_block_shares_with_nothing() {
        let (mut building, block) = Building::new();
        let addr = building.local(block, 0);
        let next = building.func.create_block();
        let param = building.func.append_param(next, GPR);
        building.func.build(block, building.nop).finish();
        building.func.succs_mut(block).push(BlockCall::with(next, vec![addr]));
        building.through(next, param);
        let (reach, _) = building.allocate(1, 4);

        assert!(!reach.shares(0), "an address that goes between blocks");
    }

    #[test]
    fn an_address_a_second_address_computation_reads_is_the_same_local_followed_on() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        // `lea` off a `lea`, which is what the address of a field of a local is. The local is
        // wanted wherever the second address is, not only where the first one is.
        let derived = building.func.new_vreg(GPR);
        let at = Operand::read(first, GPR);
        building.func.build(block, building.lea).def(derived, GPR).mem(Mem::at(at)).finish();
        let second = building.local(block, 1);
        building.through(block, second);
        building.through(block, derived);
        let (reach, allocation) = building.allocate(2, 4);

        assert!(reach.shares(0), "a derived address is still an address into this frame");
        let plan = Slots::share(&reach, &allocation, &[WORD, WORD], &[]);
        assert_eq!(plan.cells().len(), 2, "the two locals are wanted at once after all");
    }

    #[test]
    fn a_cell_two_things_share_is_as_wide_and_as_strict_as_both_of_them() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.through(block, first);
        let second = building.local(block, 1);
        building.through(block, second);
        let (reach, allocation) = building.allocate(2, 4);

        let narrow = Local { size: 4, align: 4 };
        let wide = Local { size: 16, align: 16 };
        let plan = Slots::share(&reach, &allocation, &[narrow, wide], &[]);
        assert_eq!(plan.cells(), [Cell { size: 16, align: 16 }]);
        assert_eq!(plan.local(0), plan.local(1));
    }

    #[test]
    fn a_local_nothing_on_the_address_list_names_shares_with_nothing() {
        let (mut building, block) = Building::new();
        let addr = building.local(block, 0);
        building.through(block, addr);
        let (reach, allocation) = building.allocate(2, 4);

        // A list with nothing on it for a local is this pass having no account of it rather than
        // a local nothing touches, so it keeps bytes of its own.
        assert!(!reach.shares(1));
        let plan = Slots::share(&reach, &allocation, &[WORD, WORD], &[]);
        assert_eq!(plan.cells().len(), 2);
    }

    #[test]
    fn the_frame_with_nothing_sharing_gives_every_local_and_every_slot_a_run_of_its_own() {
        let plan = Slots::apart(&[WORD, Local { size: 4, align: 4 }], &[8, 16]);

        assert_eq!(plan.cells().len(), 4);
        assert_eq!(plan.saved(), 0);
        assert_eq!((plan.local(0), plan.local(1)), (Some(0), Some(1)));
        assert_eq!((plan.slot(0), plan.slot(1)), (Some(2), Some(3)));
        assert_eq!(plan.cells()[3], Cell { size: 16, align: 16 });
    }

    #[test]
    fn a_frame_whose_locals_share_is_smaller_and_puts_them_at_the_same_offset() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.through(block, first);
        let second = building.local(block, 1);
        building.through(block, second);
        let (reach, allocation) = building.allocate(2, 4);

        // Not a leaf, so the frame is taken rather than kept in the red zone and its size is a
        // number rather than nothing, and big enough that the convention's alignment does not
        // round the difference away.
        let locals = [Local { size: 64, align: 8 }; 2];
        let base = Layout { leaf: false, locals: &locals, ..Layout::new(&SYSV, REGS) };
        let apart = Frame::of(&building.func, &allocation, &base);
        let plan = Slots::share(&reach, &allocation, &locals, &[]);
        let layout = Layout { share: Some(&plan), ..base };
        let together = Frame::of(&building.func, &allocation, &layout);

        assert_ne!(apart.local(0), apart.local(1));
        assert_eq!(together.local(0), together.local(1));
        // Sixty four bytes of frame gone, and eight more in each of them for the word that lands
        // the stack pointer back where a call wants it.
        assert_eq!((apart.size(), together.size()), (136, 72));
    }

    #[test]
    fn a_function_with_more_slots_than_anything_real_is_laid_out_the_old_way() {
        let (mut building, block) = Building::new();
        let addr = building.local(block, 0);
        building.through(block, addr);
        let (reach, allocation) = building.allocate(1, 4);

        let locals = vec![WORD; CROWDED + 1];
        let plan = Slots::share(&reach, &allocation, &locals, &[]);
        assert_eq!(plan.cells().len(), locals.len());
        assert_eq!(plan.saved(), 0);
    }
}
