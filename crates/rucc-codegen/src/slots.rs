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
//! Unless the program said where the local stops. See the next section.
//!
//! # The end of a lifetime
//!
//! A block scoped object in C stops existing when its block is left, whoever still has its address,
//! and a pointer to it is indeterminate from then on (C11 6.2.4). The front end says so with a
//! `lifetime_end` of the object's slot on every way out of the block it can see, which the lowering
//! turns into a marker in the code and an entry on [`crate::lower::Stack::ends`]. A local that has
//! those can share even after its address went where this pass cannot follow it, because what
//! happens to the bytes after the end is not the program's business any more.
//!
//! Such a local is not asked about the way the others are. The touches this pass can see are still
//! where it starts, but it cannot be asked where the last touch is, because the call it was handed
//! to may have kept the address and any later call may use it. So its area is every point that a
//! touch reaches going forward without meeting an end on the way, see `ended`. That is exact for
//! a straight run, and a loop comes out right too: the declaration is at the top of the body, the
//! end is at the bottom, and the back edge carries nothing because the end stands in the way. What
//! a declaration a `goto` jumps into or out of does is covered by the front end, which writes no
//! end for a local whose scope it cannot see all the ways out of, and a local with no end is left
//! out exactly as before. A value holding the address that is still live across an end is a copy
//! the lowering made for its own use, and those count as touches, so they keep the bytes wanted
//! over the end rather than being cut off by it.
//!
//! # Where a local is wanted is not where its address is live
//!
//! Knowing which instructions reach a local is only half of it. The address that reaches it is a
//! value and the object is not, so an address register that dies right after the store through it
//! says nothing about how long those bytes have to go on holding what was stored. A local written
//! at one point and read at another has to hold its contents through everything in between, however
//! little of what is in between mentions the local at all.
//!
//! So the area of a local is worked out as its own question over the control flow graph: its bytes
//! matter at every point that has a touch behind it and a touch in front of it. A point with
//! nothing in front is one where the object is finished with, and a point with nothing behind is
//! one where it holds nothing anybody may read, since the contents of a local nothing has written
//! yet are not contents. The two halves of that question are reachability over the graph rather
//! than over the line the function was laid out in. Over the line would be wrong for a loop: a
//! local written at the bottom of a body and read at the top of the next turn is one whose bytes
//! matter across the header too, and the header is laid out before either of the two touches.
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
//! # A spill slot is not a variable
//!
//! The two kinds are asked about separately, because the reason a frame ever lays two things out
//! apart that could share is the debugger, and that reason covers one kind and not the other. A
//! local is a variable somebody wrote down and can ask the value of, so two locals sharing bytes
//! means a variable that is out of scope reads as whatever took its place, which is what `-O0`
//! exists not to do and what `-fstack-reuse=none` turns off at every level. A spill slot holds a
//! value the allocator ran out of registers for, it has no name, nothing can ask for it, and the
//! only thing that ever reads it is the instruction the allocator wrote. Laying those out one
//! each buys a debugger nothing and costs a frame everything, since most frames are mostly spill
//! slots.
//!
//! So spill slots share at every level and locals share only where the level says they may. What
//! says which is whether this pass is handed a [`Reach`]: with one, the locals it followed join
//! in, and without one every local gets bytes of its own and the spill slots are fitted around
//! them.
//!
//! # How big it is allowed to get
//!
//! Fitting each thing into the first cell it does not clash with compares it against the cells so
//! far, so a function whose things mostly cannot share costs the square of how many there are.
//! What bounds that is a budget of comparisons rather than a count of things: the fit spends
//! [`BUDGET`] of them and lays out whatever is left one cell each. A function whose things do
//! share never comes near it, because what each one is compared against is the cells and not the
//! things, and the whole point of sharing is that there are far fewer cells than things. lua's
//! interpreter, which is 2802 slots fitted into 144 cells and the largest function in the corpus,
//! spends an eighth of the budget and adds a seventh of a second to the file it is in. A function
//! with that many slots that are all live at once would spend the lot, and it gets the layout it
//! would have got anyway.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rucc_base::Interner;
use rucc_base::hash::{Map, Set};
use rucc_mir::{Func, Inst, Opcode, Reg};
use rucc_regalloc::Allocation;
use rucc_regalloc::assign::Place;
use rucc_regalloc::live::{Area, Live, Range};
use rucc_regalloc::order::{Order, Point};
use rucc_regalloc::rewrite::At;
use rucc_target::FrameInsts;

use crate::frame::Local;

/// How many cells the fit may look at before it stops pairing things up and gives everything left
/// a cell of its own.
///
/// See the note on how big it is allowed to get in the module documentation. One unit is one thing
/// compared against one cell, which is what costs. The largest function in the corpus spends an
/// eighth of this, so the budget is a guard against a generated file rather than something the
/// ordinary path meets.
pub const BUDGET: usize = 1 << 20;

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
    /// Where each local that went in beside something else is wanted, and `None` for the rest.
    shared: Vec<Option<Vec<Range>>>,
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
            shared: vec![None; locals.len()],
            cells,
        }
    }

    /// The frame with everything that can share sharing, worked out from the allocator's liveness.
    ///
    /// `reach` is what [`reach`] said about this function before the allocator ran, or `None` for a
    /// build whose locals keep bytes of their own, which is `-O0` and `-fstack-reuse=none`. The
    /// spill slots share either way, for the reason in the module documentation. `widths` is how
    /// many bytes a slot of each of the allocation's spill slots takes, and `locals` is the
    /// function's own objects in the order the lowering recorded them. `func` is the function the
    /// allocator has finished with, which is asked for the shape of its control flow and nothing
    /// else: the rewrite took the values away but it left every block and every edge where it was.
    #[must_use]
    pub fn share(
        func: &Func,
        reach: Option<&Reach>,
        allocation: &Allocation,
        locals: &[Local],
        widths: &[u32],
    ) -> Self {
        let mut wants = Vec::with_capacity(locals.len() + widths.len());
        let mut reached = reach
            .map(|reach| areas(func, reach, &allocation.live, &allocation.order))
            .unwrap_or_default();
        for (local, &Local { size, align }) in locals.iter().enumerate() {
            let area = reached.get_mut(local).and_then(Option::take);
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
        fit(wants, locals.len(), widths.len(), BUDGET)
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

    /// Where a local is wanted, in the allocator's points, if its cell holds something else too,
    /// and `None` for a local whose bytes are its own.
    ///
    /// The bytes of a local that shares are only its over this area. Outside it they hold
    /// whatever else went in the cell, which is why the debugging information asks: a place given
    /// for the whole function would have a debugger print the other thing under this one's name.
    #[must_use]
    pub fn shared(&self, local: usize) -> Option<&[Range]> {
        self.shared.get(local)?.as_deref()
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
///
/// What `budget` is is comparisons of one thing against one cell, which is what costs. Past that
/// everything left opens a cell of its own, which is the layout a frame had before this pass
/// existed, and the wants are in a settled order so which ones those are is settled too.
fn fit(mut wants: Vec<Want>, locals: usize, slots: usize, mut budget: usize) -> Slots {
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by_key(|&want| {
        let Want { size, align, .. } = wants[want];
        (Reverse(align), Reverse(size), want)
    });

    let mut cells: Vec<Cell> = Vec::new();
    // `None` is a cell nothing else may go in, which is what a thing this pass could not follow
    // opens. A cell with an area is one anything that does not clash with that area may join.
    let mut busy: Vec<Option<Vec<Range>>> = Vec::new();
    let mut of_local = vec![0; locals];
    let mut of_slot = vec![0; slots];
    let mut areas = vec![None; locals];
    let mut held = Vec::new();
    for want in order {
        let Want { what, size, align, area } = std::mem::replace(
            &mut wants[want],
            Want { what: What::Local(0), size: 0, align: 0, area: None },
        );
        let mut into = None;
        if let Some(area) = &area {
            for (cell, held) in busy.iter().enumerate() {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                if held.as_ref().is_some_and(|held| !clashes(held, area)) {
                    into = Some(cell);
                    break;
                }
            }
        }
        // Kept for a local as well as handed to the cell, since whether it shared is only known once
        // everything has been fitted, and one that did is asked about again. See [`Slots::shared`].
        let mine = if let What::Local(_) = what { area.clone() } else { None };
        let cell = match into {
            Some(cell) => {
                cells[cell].size = cells[cell].size.max(size);
                cells[cell].align = cells[cell].align.max(align);
                let held = busy[cell].take().unwrap_or_default();
                busy[cell] = Some(union(&held, area.as_deref().unwrap_or_default()));
                cell
            }
            None => {
                cells.push(Cell { size, align });
                busy.push(area);
                cells.len() - 1
            }
        };
        match what {
            What::Local(local) => {
                of_local[local] = cell;
                areas[local] = mine;
            }
            What::Slot(slot) => of_slot[slot] = cell,
        }
        if held.len() <= cell {
            held.resize(cell + 1, 0);
        }
        held[cell] += 1;
    }
    let shared = areas
        .into_iter()
        .zip(&of_local)
        .map(|(area, &cell)| area.filter(|_| held[cell] > 1))
        .collect();
    Slots { cells, locals: of_local, slots: of_slot, shared }
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
    /// Every point one local is touched at, which is where its address is live and where an
    /// instruction that swallowed the address stands.
    fn touches(&self, local: usize, live: &Live, order: &Order) -> Option<Vec<Range>> {
        let held = self.through.get(local)?.as_ref()?;
        let mut spots: Vec<Range> = Vec::new();
        for &reg in &held.regs {
            spots.extend(live.area(reg).into_iter().flat_map(Area::pieces));
        }
        for &inst in &held.at {
            spots.push(Range { start: order.early(inst), end: order.late(inst) });
        }
        Some(spots)
    }

    /// Whether a local may share its bytes with anything, which is what the tests ask.
    #[must_use]
    pub fn shares(&self, local: usize) -> bool {
        self.through.get(local).is_some_and(Option::is_some)
    }
}

/// Everywhere the bytes of each local have to go on holding what was put in them.
///
/// A point counts if a touch of that local can have happened before it and another can still
/// happen after it. Before is reachability forward through the graph from the blocks that touch
/// the local, after is the same walk backwards, and the bytes matter where the two meet. See the
/// note in the module documentation on why this is asked over the graph and not over the line the
/// function was laid out in.
///
/// A local this pass could not follow the address of comes back `None`, which is the answer that
/// shares with nothing.
fn areas(func: &Func, reach: &Reach, live: &Live, order: &Order) -> Vec<Option<Vec<Range>>> {
    let blocks = order.blocks();
    let count = reach.through.len();
    let words = count.div_ceil(64);

    // Where each block starts, which is ascending, so the block a point is in is a search.
    let starts: Vec<u32> = blocks.iter().map(|&block| order.start(block)).collect();
    let holding = |point: u32| starts.partition_point(|&start| start <= point).saturating_sub(1);

    // Which locals each block touches, as bits for the walk and as a range for the answer. A touch
    // that runs through whole blocks between the two it starts and stops in covers those blocks
    // top to bottom whatever the walk says, so they are one piece of the answer straight away and
    // only the two ends are left for the blocks to decide.
    let mut touched = vec![vec![0u64; words]; blocks.len()];
    let mut inside: Vec<Vec<(usize, Range)>> = vec![Vec::new(); blocks.len()];
    let mut through: Vec<Vec<Range>> = vec![Vec::new(); count];
    // The locals whose areas come from where they end, which are asked about on their own below and
    // are kept out of the rows above so that nothing here mistakes them for the other kind.
    let mut ending = vec![false; count];
    let mut spots_of: Vec<Vec<Range>> = vec![Vec::new(); count];
    for local in 0..count {
        let Some(spots) = reach.touches(local, live, order) else { continue };
        if reach.through[local].as_ref().is_some_and(|held| held.escapes) {
            ending[local] = true;
            spots_of[local] = spots;
            continue;
        }
        for spot in spots {
            let (first, last) = (holding(spot.start), holding(spot.end));
            for row in &mut touched[first..=last] {
                row[local / 64] |= 1 << (local % 64);
            }
            let mut clip = |at: usize| {
                let block = blocks[at];
                let start = spot.start.max(order.start(block));
                let end = spot.end.min(order.end(block));
                inside[at].push((local, Range { start, end }));
            };
            clip(first);
            if last != first {
                clip(last);
            }
            if last > first + 1 {
                let start = order.start(blocks[first + 1]);
                through[local].push(Range { start, end: order.end(blocks[last - 1]) });
            }
        }
    }

    // One range per block per local, from the first touch in the block to the last. A block runs
    // top to bottom, so whatever sits between two touches of the same local is between them in the
    // run as well, and the bytes have to have held what they hold all the way through it.
    for spots in inside.iter_mut() {
        spots.sort_unstable_by_key(|&(local, Range { start, .. })| (local, start));
        let mut kept = 0;
        for at in 1..spots.len() {
            if spots[at].0 == spots[kept].0 {
                spots[kept].1.end = spots[kept].1.end.max(spots[at].1.end);
            } else {
                kept += 1;
                spots[kept] = spots[at];
            }
        }
        spots.truncate(spots.len().min(kept + 1));
    }

    // The graph, by position in the line rather than by block, because everything else here is.
    let mut place = vec![0usize; func.block_count()];
    for (at, &block) in blocks.iter().enumerate() {
        place[block.index()] = at;
    }
    let mut ahead: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    let mut behind: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    for (at, &block) in blocks.iter().enumerate() {
        for call in &func[block].succs {
            let to = place[call.block.index()];
            ahead[at].push(to);
            behind[to].push(at);
        }
    }

    let written = spread(&behind, &touched, None, words, true);
    let read = spread(&ahead, &touched, None, words, false);

    let mut out = vec![None; count];
    for (local, pieces) in out.iter_mut().enumerate() {
        if reach.shares(local) && !ending[local] {
            *pieces = Some(std::mem::take(&mut through[local]));
        }
    }
    if ending.contains(&true) {
        let settled = ended(func, reach, &ending, spots_of, order, &place, &behind);
        for (local, pieces) in settled.into_iter().enumerate() {
            if ending[local] {
                out[local] = Some(pieces);
            }
        }
    }
    for (at, &block) in blocks.iter().enumerate() {
        let whole = Range { start: order.start(block), end: order.end(block) };
        for word in 0..words {
            let mut bits = written[at][word] & read[at][word];
            while bits != 0 {
                let local = word * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if let Some(pieces) = out[local].as_mut() {
                    joined(pieces, whole);
                }
            }
        }
        // A block that touches the local is covered from the touch, or from the top of the block
        // if something above already wrote it, and to the touch, or to the bottom if something
        // below still reads it.
        for &(local, spot) in &inside[at] {
            let held = |bits: &[Vec<u64>]| bits[at][local / 64] & (1 << (local % 64)) != 0;
            let start = if held(&written) { whole.start } else { spot.start };
            let end = if held(&read) { whole.end } else { spot.end };
            if let Some(pieces) = out[local].as_mut() {
                joined(pieces, Range { start, end });
            }
        }
    }
    for pieces in out.iter_mut().flatten() {
        *pieces = merged(std::mem::take(pieces));
    }
    out
}

/// Where each local that escapes and has ends is wanted, which is every point a touch of it can have
/// happened before without an end of it in between.
///
/// `ending` says which locals those are and `spots_of` is where each of them is touched, which is
/// everywhere a value holding its address is live and everywhere an instruction that swallowed the
/// address stands. Every other local comes back with nothing, since [`areas`] asks about those its
/// own way.
///
/// Only forwards. The other half of the question [`areas`] asks, whether a touch can still happen
/// after a point, is no help here, because some of the touches of a local whose address got away
/// are not instructions in this function at all: they are in whatever the address was handed to,
/// and they can happen at any point the address can still be come by. What this function can say
/// is where that stops, which is where the program said the lifetime ends. So the area starts at
/// the first touch on every path and runs until the path meets an end.
///
/// A touch after an end starts the area again, which is a loop coming back round to the
/// declaration, and a value holding the address that is live across an end keeps the local wanted
/// over the whole of it. The second is what the lowering's habit of writing the address once per
/// block needs, since two copies of one loop body unrolled into a block may read the address the
/// first copy worked out.
fn ended(
    func: &Func,
    reach: &Reach,
    ending: &[bool],
    spots_of: Vec<Vec<Range>>,
    order: &Order,
    place: &[usize],
    behind: &[Vec<usize>],
) -> Vec<Vec<Range>> {
    let blocks = order.blocks();
    let count = ending.len();
    let words = count.div_ceil(64);
    let starts: Vec<u32> = blocks.iter().map(|&block| order.start(block)).collect();
    let holding = |point: u32| starts.partition_point(|&start| start <= point).saturating_sub(1);
    let whole = |at: usize| Range { start: order.start(blocks[at]), end: order.end(blocks[at]) };

    // Where each local ends in each block, and which locals get through each block without one.
    let mut kills: Vec<Vec<(usize, u32)>> = vec![Vec::new(); blocks.len()];
    let mut passing = vec![vec![!0u64; words]; blocks.len()];
    for local in (0..count).filter(|&local| ending[local]) {
        let Some(held) = reach.through[local].as_ref() else { continue };
        for &inst in &held.ends {
            let Some(block) = func.block_of(inst) else { continue };
            let at = place[block.index()];
            kills[at].push((local, order.early(inst)));
            passing[at][local / 64] &= !(1 << (local % 64));
        }
    }
    // The first end of a local in a block at or after a point, or the bottom of the block when
    // there is none, which is how far a touch at that point keeps the local wanted.
    let until = |at: usize, local: usize, point: u32| -> u32 {
        kills[at]
            .iter()
            .filter(|&&(one, kill)| one == local && kill >= point)
            .map(|&(_, kill)| kill)
            .min()
            .unwrap_or_else(|| whole(at).end)
    };

    // The touches, clipped to the blocks they start and stop in. A touch that runs through whole
    // blocks between those two covers them top to bottom and reaches the bottom of each, whatever
    // ends in them, so those are one piece of the answer straight away, as they are in [`areas`].
    let mut out: Vec<Vec<Range>> = vec![Vec::new(); count];
    let mut inside: Vec<Vec<(usize, Range)>> = vec![Vec::new(); blocks.len()];
    let mut leaving = vec![vec![0u64; words]; blocks.len()];
    for (local, spots) in spots_of.into_iter().enumerate() {
        for spot in spots {
            let (first, last) = (holding(spot.start), holding(spot.end));
            for at in [first, last] {
                let piece = Range {
                    start: spot.start.max(whole(at).start),
                    end: spot.end.min(whole(at).end),
                };
                inside[at].push((local, piece));
            }
            if last > first + 1 {
                out[local].push(Range { start: whole(first + 1).start, end: whole(last - 1).end });
                for row in &mut leaving[first + 1..last] {
                    row[local / 64] |= 1 << (local % 64);
                }
            }
        }
    }
    // A block a local leaves still wanted is one touching it after the last end of it in there.
    for (at, row) in leaving.iter_mut().enumerate() {
        for &(local, piece) in &inside[at] {
            if until(at, local, piece.end) == whole(at).end {
                row[local / 64] |= 1 << (local % 64);
            }
        }
    }

    let arriving = spread(behind, &leaving, Some(&passing), words, true);
    for (at, row) in arriving.iter().enumerate() {
        let top = whole(at).start;
        for (word, &bits) in row.iter().enumerate() {
            let mut bits = bits;
            while bits != 0 {
                let local = word * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                out[local].push(Range { start: top, end: until(at, local, top) });
            }
        }
        for &(local, piece) in &inside[at] {
            out[local].push(Range { start: piece.start, end: until(at, local, piece.end) });
        }
    }
    for pieces in &mut out {
        *pieces = merged(std::mem::take(pieces));
    }
    out
}

/// Adds a piece to a local's area, stretching the last one instead when the piece starts on the
/// point right after it.
///
/// Blocks are taken in the line's order and a block starts one point after the one before it ends,
/// so a local wanted all the way through a run of blocks is one piece for the run rather than one
/// piece per block. On jtckdint the answer used to be a piece per block for each of the locals
/// whose address stays live across its 16000 blocks, which with the one piece per block pushed for
/// the touches above was 3% of the instructions of an optimized build. The join only ever happens
/// where one block ends and the next starts, so no point is added or lost, and a stretch of debug
/// info is still found block by block from the same points.
fn joined(pieces: &mut Vec<Range>, piece: Range) {
    match pieces.last_mut() {
        Some(last) if last.end.checked_add(1) == Some(piece.start) => last.end = piece.end,
        _ => pieces.push(piece),
    }
}

/// Which locals a touch of can reach the start of each block, following the given edges.
///
/// One walk stands for both directions. Handed the edges into each block it says which locals were
/// touched somewhere above, and handed the edges out of each block it says which are touched
/// somewhere below. Blocks are taken in [`settling`]'s order, so a block with no loop around it is
/// looked at after everything it reads from is final and only once, and a block is only looked at
/// again when a block it reads from changed.
///
/// It used to be rounds over every block in the line's order until one changed nothing, which
/// settles a straight stretch in one round only when the line runs the way the edges do. It does
/// not have to: jtckdint's main has a chain a thousand blocks long laid out against its edges, and
/// the rounds over its 16000 blocks took a thousand passes and five seconds to move the answer down
/// it one block at a time.
///
/// `passing`, where there is one, is which locals get through each block from top to bottom. A
/// local reaching the start of a block it does not get through only goes on from there if the
/// block touches it as well, which is what the end of a lifetime needs and nothing else does. See
/// [`ended`].
///
/// A block is allowed to be its own neighbour, which is what a loop of one block is, and the row it
/// is working on is a copy for that reason. Reading a block's own answer back is a no change either
/// way, since the answer being built is the one being read, but what a block touches does come back
/// to itself around a back edge and that is the half that has to arrive. tamnd/rucc#1207.
fn spread(
    edges: &[Vec<usize>],
    touched: &[Vec<u64>],
    passing: Option<&[Vec<u64>]>,
    words: usize,
    forward: bool,
) -> Vec<Vec<u64>> {
    let count = edges.len();
    let mut readers: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (at, froms) in edges.iter().enumerate() {
        for &from in froms {
            readers[from].push(at);
        }
    }
    let order = settling(&readers, forward);
    let mut rank = vec![0; count];
    for (place, &at) in order.iter().enumerate() {
        rank[at] = place;
    }
    let mut out = vec![vec![0u64; words]; count];
    let mut waiting: BinaryHeap<Reverse<usize>> = (0..count).map(Reverse).collect();
    let mut queued = vec![true; count];
    let mut row = vec![0u64; words];
    while let Some(Reverse(place)) = waiting.pop() {
        let at = order[place];
        queued[at] = false;
        row.copy_from_slice(&out[at]);
        let mut grew = false;
        for &from in &edges[at] {
            for word in 0..words {
                let had = row[word];
                let passes = passing.map_or(!0, |passing| passing[from][word]);
                row[word] |= (out[from][word] & passes) | touched[from][word];
                grew |= row[word] != had;
            }
        }
        if grew {
            out[at].copy_from_slice(&row);
            for &reader in &readers[at] {
                if !queued[reader] {
                    queued[reader] = true;
                    waiting.push(Reverse(rank[reader]));
                }
            }
        }
    }
    out
}

/// The blocks in an order where, loops aside, every block comes after the blocks it reads from.
///
/// That is reverse postorder of a walk along the way the answer flows, from every block in turn so
/// that one nothing reaches is still in it. Any walk's reverse postorder puts a block after all its
/// predecessors once the back edges are left out, whichever block it starts from.
fn settling(readers: &[Vec<usize>], forward: bool) -> Vec<usize> {
    let count = readers.len();
    let mut seen = vec![false; count];
    let mut post = Vec::with_capacity(count);
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let roots: Vec<usize> = if forward { (0..count).collect() } else { (0..count).rev().collect() };
    for root in roots {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        stack.push((root, 0));
        while let Some((at, next)) = stack.last_mut() {
            if let Some(&to) = readers[*at].get(*next) {
                *next += 1;
                if !seen[to] {
                    seen[to] = true;
                    stack.push((to, 0));
                }
            } else {
                post.push(*at);
                stack.pop();
            }
        }
    }
    post.reverse();
    post
}

/// Everywhere one local is reached from.
#[derive(Debug, Clone, Default)]
struct Carried {
    /// The values that hold its address.
    regs: Vec<Reg>,
    /// The instructions that reach it with no value in between, which is what an address folded
    /// into its reader leaves behind.
    at: Vec<Inst>,
    /// Where the program said the local's lifetime ends, which is what the front end's
    /// `lifetime_end` became.
    ends: Vec<Inst>,
    /// Whether its address went somewhere [`follow`] could not see the end of. A local like that
    /// shares only where it has ends, and then its area is worked out from them rather than from
    /// the touches alone. See the note on the end of a lifetime in the module documentation.
    escapes: bool,
}

/// Follows the address of every local of a function as far as it goes.
///
/// `addresses` is the list [`crate::lower`] built and [`crate::fold`] rewrote, which says which
/// instruction carries the address of which local. `ends` is the other list the lowering built,
/// which says where the lifetime of which local ends. `count` is how many locals there are, since a
/// local nothing on the first list names is one this has no account of rather than one nothing
/// touches.
///
/// Run after the fold and before allocation. After the fold because an address that ended up inside
/// its reader is an address no value holds and this has to see it that way. Before allocation
/// because every answer here is about a virtual register, and the rewrite the allocator ends with
/// is what stops there being one.
#[must_use]
pub fn reach(
    func: &Func,
    addresses: &[(Inst, usize)],
    ends: &[(Inst, usize)],
    count: usize,
    insts: &FrameInsts,
    names: &mut Interner,
) -> Reach {
    let lea = Opcode::new(names.intern(&format!("{}{}", insts.prefix, insts.lea)));
    let mut through: Vec<Option<Carried>> = vec![None; count];
    // A local one of whose addresses this could not read at all, which stays out whatever the rest
    // of the list says about it.
    let mut lost = vec![false; count];
    for &(inst, local) in addresses {
        if lost.get(local).is_none_or(|&lost| lost) {
            continue;
        }
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
                    lost[local] = true;
                    continue;
                }
            }
        }
        // On the list either way, so that a local whose address nothing reads is still wanted where
        // the address of it was taken rather than nowhere at all.
        held.at.push(inst);
    }

    // Only an end something still stands at. A pass that took the marker out has taken the end
    // with it, and a local with fewer ends is a local wanted over more of the function, never less.
    for &(inst, local) in ends {
        if func.block_of(inst).is_none() {
            continue;
        }
        if let Some(Some(held)) = through.get_mut(local) {
            held.ends.push(inst);
        }
    }

    let readers = readers(func);
    let crossing = crossing(func);
    for held in &mut through {
        if let Some(carried) = held.take() {
            *held = follow(func, lea, &readers, &crossing, carried)
                .filter(|carried| !carried.escapes || !carried.ends.is_empty());
        }
    }
    Reach { through }
}

/// Follows every address a local is reached through to every value that address becomes.
///
/// Marks as escaping a local whose address is read some way this cannot account for, which is any
/// way but as the base or the index of a memory operand. A call argument is one of those, a value
/// stored into memory is another, and so is a value carried into a block as an argument, which is
/// the one that is not an operand at all. The walk goes on past one of those rather than stopping,
/// because an escaping local that has ends is still asked about, and what it is asked about then is
/// every value holding its address that this can see.
fn follow(
    func: &Func,
    lea: Opcode,
    readers: &Map<Reg, Vec<Inst>>,
    crossing: &Set<Reg>,
    mut held: Carried,
) -> Option<Carried> {
    let mut seen: Set<Reg> = held.regs.iter().copied().collect();
    let mut queue = held.regs.clone();
    while let Some(reg) = queue.pop() {
        if crossing.contains(&reg) {
            held.escapes = true;
        }
        for &inst in readers.get(&reg).map(Vec::as_slice).unwrap_or_default() {
            if !addressed(func, inst, reg) {
                held.escapes = true;
                continue;
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
fn readers(func: &Func) -> Map<Reg, Vec<Inst>> {
    let mut readers: Map<Reg, Vec<Inst>> = Map::default();
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
fn crossing(func: &Func) -> Set<Reg> {
    let mut crossing = Set::default();
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

/// What [`merged`] makes of two lists it made, without sorting them again.
///
/// A cell's list is joined this way every time something goes in it, and sorting the whole of it
/// each time made every join cost more than the last. Both lists are in order, so taking the earlier
/// front each time is the order a sort would have put them in.
fn union(one: &[Range], two: &[Range]) -> Vec<Range> {
    let mut out: Vec<Range> = Vec::with_capacity(one.len() + two.len());
    let (mut mine, mut theirs) = (0, 0);
    while mine < one.len() || theirs < two.len() {
        let first = theirs == two.len()
            || (mine < one.len()
                && (one[mine].start, one[mine].end) <= (two[theirs].start, two[theirs].end));
        let piece = if first {
            mine += 1;
            one[mine - 1]
        } else {
            theirs += 1;
            two[theirs - 1]
        };
        match out.last_mut() {
            Some(last) if piece.start <= last.end => last.end = last.end.max(piece.end),
            _ => out.push(piece),
        }
    }
    out
}

/// Whether two stretches of a function are both wanted anywhere, which is what stops two things
/// sharing a cell.
///
/// Both lists are in order and apart, so this walks them together and stops at the first pair that
/// touches rather than comparing every piece with every other. A cell that many things went into
/// has a long list, and a thing asking about it is often a few pieces far apart, so each step skips
/// every piece that ends before the other list's piece starts rather than going one at a time.
fn clashes(one: &[Range], two: &[Range]) -> bool {
    let (mut mine, mut theirs) = (0, 0);
    while mine < one.len() && theirs < two.len() {
        if one[mine].overlaps(two[theirs]) {
            return true;
        }
        if one[mine].end < two[theirs].end {
            mine += ending_before(&one[mine..], two[theirs].start);
        } else {
            theirs += ending_before(&two[theirs..], one[mine].start);
        }
    }
    false
}

/// How many pieces at the front of a list end before a point. The answer is often one, so this
/// doubles a step until it passes the point and searches only the last step.
fn ending_before(pieces: &[Range], point: Point) -> usize {
    let mut bound = 1;
    while bound < pieces.len() && pieces[bound].end < point {
        bound *= 2;
    }
    let low = bound / 2;
    let high = pieces.len().min(bound + 1);
    low + pieces[low..high].partition_point(|piece| piece.end < point)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{Block, BlockCall, Mem, Operand};
    use rucc_regalloc::assign::Env;
    use rucc_regalloc::order::Point;
    use rucc_target::x86_64::{FRAME, GPR, REGS, SYSV};

    use super::*;
    use crate::frame::{Frame, Layout};

    /// A function being built, with the names and the opcodes a test needs to hand.
    struct Building {
        names: Interner,
        func: Func,
        lea: Opcode,
        nop: Opcode,
        end: Opcode,
        addresses: Vec<(Inst, usize)>,
        ends: Vec<(Inst, usize)>,
    }

    impl Building {
        /// An empty function of one block.
        fn new() -> (Self, Block) {
            let mut names = Interner::new();
            let func = Func::new(names.intern("f"));
            let lea = Opcode::new(names.intern(&format!("{}{}", FRAME.prefix, FRAME.lea)));
            let nop = Opcode::new(names.intern("x64.nop"));
            let end = Opcode::new(names.intern("x64.lifetime_end"));
            let mut building =
                Self { names, func, lea, nop, end, addresses: Vec::new(), ends: Vec::new() };
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

        /// The end of a local's lifetime, the way the lowering writes one.
        fn end(&mut self, block: Block, which: usize) {
            let inst = self.func.build(block, self.end).finish();
            self.ends.push((inst, which));
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
            let reach =
                reach(&self.func, &self.addresses, &self.ends, locals, &FRAME, &mut self.names);
            let env =
                Env::new().with(GPR, &SYSV.int_order[..registers], &SYSV.int_order[registers..]);
            let allocation = rucc_regalloc::run(&mut self.func, &env, "test", true);
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

        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_eq!(plan.cells().len(), 1, "one run of bytes for the two of them");
        assert_eq!(plan.local(0), plan.local(1));
        assert_eq!(plan.saved(), 1);
    }

    #[test]
    fn a_local_that_went_in_beside_another_says_where_it_is_wanted_and_one_alone_does_not() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.through(block, first);
        let second = building.local(block, 1);
        building.through(block, second);
        let (reach, allocation) = building.allocate(2, 4);

        // Each of the two is wanted over a stretch the other is not, and it is those stretches the
        // debugging information gives each of them a place over rather than the whole function.
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        let (one, two) = (plan.shared(0).expect("shares"), plan.shared(1).expect("shares"));
        assert!(!one.is_empty() && !two.is_empty());
        assert!(!clashes(one, two), "wanted apart: {one:?} and {two:?}");

        // The same function with nothing allowed to share, where each local's bytes are its own
        // over the whole of it.
        let plan = Slots::share(&building.func, None, &allocation, &[WORD, WORD], &[]);
        assert_eq!((plan.shared(0), plan.shared(1)), (None, None));
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

        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
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
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD], &[8]);
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
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_eq!(plan.cells().len(), 2);
        assert_ne!(plan.local(0), plan.local(1));
    }

    /// Two locals whose addresses are both handed to something, one after the other, the way two
    /// structs declared in the two arms of an `if` and passed to a call by address are.
    fn handed_one_after_the_other(ended: bool) -> (Building, Reach, Allocation) {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.held(block, first);
        if ended {
            building.end(block, 0);
        }
        let second = building.local(block, 1);
        building.held(block, second);
        if ended {
            building.end(block, 1);
        }
        let (reach, allocation) = building.allocate(2, 4);
        (building, reach, allocation)
    }

    #[test]
    fn two_locals_handed_to_something_share_when_each_ends_before_the_other_starts() {
        let (building, reach, allocation) = handed_one_after_the_other(true);
        assert!(reach.shares(0) && reach.shares(1));
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_eq!(plan.cells().len(), 1, "one run of bytes for the two of them");
        assert_eq!(plan.local(0), plan.local(1));

        // The same two with nothing saying where they stop, which is every one of them before
        // the front end wrote ends, and which stays apart.
        let (building, reach, allocation) = handed_one_after_the_other(false);
        assert!(!reach.shares(0) && !reach.shares(1));
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_ne!(plan.local(0), plan.local(1));
    }

    #[test]
    fn a_local_handed_to_something_is_wanted_until_its_end_and_not_only_its_last_use() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.held(block, first);
        // The second local comes in before the first one ends, and whatever the first was handed
        // to may still be using it here, although nothing in this function mentions it.
        let second = building.local(block, 1);
        building.through(block, second);
        building.end(block, 0);
        let (reach, allocation) = building.allocate(2, 4);

        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_ne!(plan.local(0), plan.local(1));
    }

    #[test]
    fn an_address_still_held_after_the_end_keeps_the_local_wanted_until_it_is_let_go() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.held(block, first);
        building.end(block, 0);
        let second = building.local(block, 1);
        building.through(block, second);
        // The first address again, from the same value, which is what a copy of an address the
        // lowering kept for the rest of the block looks like when the block goes on past an end.
        building.held(block, first);
        let (reach, allocation) = building.allocate(2, 4);

        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_ne!(plan.local(0), plan.local(1));
    }

    #[test]
    fn a_local_that_ends_at_the_bottom_of_a_loop_body_is_not_wanted_around_the_back_edge() {
        let (mut building, block) = Building::new();
        let header = building.func.create_block();
        let body = building.func.create_block();
        let exit = building.func.create_block();
        building.func.build(block, building.nop).finish();
        building.func.succs_mut(block).push(BlockCall::to(header));
        building.func.build(header, building.nop).finish();
        building.func.succs_mut(header).push(BlockCall::to(body));
        building.func.succs_mut(header).push(BlockCall::to(exit));

        // Declared at the top of the body and ended at the bottom, every turn.
        let addr = building.local(body, 0);
        building.held(body, addr);
        building.end(body, 0);
        building.func.build(body, building.nop).finish();
        building.func.succs_mut(body).push(BlockCall::to(header));

        // Another local after the loop, which the first is never wanted beside.
        let after = building.local(exit, 1);
        building.through(exit, after);
        let (reach, allocation) = building.allocate(2, 4);

        let areas = areas(&building.func, &reach, &allocation.live, &allocation.order);
        let (one, two) = (areas[0].as_ref().expect("ends"), areas[1].as_ref().expect("shares"));
        assert!(!clashes(one, two), "wanted apart: {one:?} and {two:?}");
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_eq!(plan.local(0), plan.local(1));
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
    fn a_local_touched_again_later_keeps_its_bytes_over_everything_in_between() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.through(block, first);
        // Another local in the stretch between the two touches of the first one. Nothing mentions
        // the first local in here, which is exactly the case: it is not being read, but what it
        // holds is still wanted below, so these cannot be the same bytes.
        let second = building.local(block, 1);
        building.through(block, second);
        // The first local again, reached through an address worked out a second time.
        let again = building.local(block, 0);
        building.through(block, again);
        let (reach, allocation) = building.allocate(2, 4);

        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_ne!(plan.local(0), plan.local(1));
        assert_eq!(plan.saved(), 0);
    }

    #[test]
    fn a_local_touched_in_a_loop_keeps_its_bytes_over_the_rest_of_the_loop() {
        let (mut building, block) = Building::new();
        let header = building.func.create_block();
        let body = building.func.create_block();
        building.func.build(block, building.nop).finish();
        building.func.succs_mut(block).push(BlockCall::to(header));

        // The header is laid out before the body and touches a local of its own.
        let held = building.local(header, 1);
        building.through(header, held);
        building.func.build(header, building.nop).finish();
        building.func.succs_mut(header).push(BlockCall::to(body));

        // The body touches the other one, every turn of the loop, and the header runs between one
        // turn and the next. So the body's local is wanted over the header as well, which is a
        // thing only the edges say: in the line the function is laid out in, the header is above
        // the only touch there is.
        let addr = building.local(body, 0);
        building.through(body, addr);
        building.func.build(body, building.nop).finish();
        building.func.succs_mut(body).push(BlockCall::to(header));
        let (reach, allocation) = building.allocate(2, 4);

        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_ne!(plan.local(0), plan.local(1));
    }

    #[test]
    fn a_local_wanted_across_a_run_of_blocks_is_one_piece_for_the_run() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.through(block, first);
        let mut last = block;
        for _ in 0..3 {
            let next = building.func.create_block();
            building.func.succs_mut(last).push(BlockCall::to(next));
            building.func.build(next, building.nop).finish();
            last = next;
        }
        let again = building.local(last, 0);
        building.through(last, again);
        let (reach, allocation) = building.allocate(1, 4);

        let areas = areas(&building.func, &reach, &allocation.live, &allocation.order);
        let pieces = areas[0].as_ref().expect("shares");
        assert_eq!(pieces.len(), 1, "one piece from the first touch to the last: {pieces:?}");
    }

    /// A loop of one block, which is a block that is its own predecessor and its own successor.
    /// The walk over the graph has to take that rather than fall over it, and what comes back is
    /// the same answer the two block loop above gets: the body runs again, so a local touched at
    /// the bottom of it is wanted at the top. tamnd/rucc#1207.
    #[test]
    fn a_block_that_is_its_own_neighbour_is_a_loop_like_any_other() {
        let (mut building, block) = Building::new();
        let loops = building.func.create_block();
        building.func.build(block, building.nop).finish();
        building.func.succs_mut(block).push(BlockCall::to(loops));

        // One local touched at the top of the block and the other at the bottom. The edge back to
        // the top is what puts the second one over the first.
        let held = building.local(loops, 1);
        building.through(loops, held);
        let addr = building.local(loops, 0);
        building.through(loops, addr);
        building.func.build(loops, building.nop).finish();
        building.func.succs_mut(loops).push(BlockCall::to(loops));
        let (reach, allocation) = building.allocate(2, 4);

        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
        assert_ne!(plan.local(0), plan.local(1));
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
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
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
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[narrow, wide], &[]);
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
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &[WORD, WORD], &[]);
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
        let plan = Slots::share(&building.func, Some(&reach), &allocation, &locals, &[]);
        let layout = Layout { share: Some(&plan), ..base };
        let together = Frame::of(&building.func, &allocation, &layout);

        assert_ne!(apart.local(0), apart.local(1));
        assert_eq!(together.local(0), together.local(1));
        // Sixty four bytes of frame gone, and eight more in each of them for the word that lands
        // the stack pointer back where a call wants it.
        assert_eq!((apart.size(), together.size()), (136, 72));
    }

    #[test]
    fn a_run_of_bytes_that_ends_part_way_through_its_alignment_costs_the_frame_nothing() {
        let (mut building, block) = Building::new();
        let addr = building.local(block, 0);
        building.through(block, addr);
        let (_, allocation) = building.allocate(1, 4);

        // Twenty four bytes asking for sixteen is what a cell shared by a wide thing and a strict
        // one looks like, and it ends eight bytes into an alignment. Which way round the two are
        // given is not allowed to matter, because the order they are placed in is this pass's
        // business and the order they were declared in is not.
        let ragged = Local { size: 24, align: 16 };
        let whole = Local { size: 32, align: 16 };
        let size = |locals: &[Local]| {
            let layout = Layout { leaf: false, locals, ..Layout::new(&SYSV, REGS) };
            Frame::of(&building.func, &allocation, &layout).size()
        };

        assert_eq!(size(&[ragged, whole]), size(&[whole, ragged]));
        // The two of them end to end with no hole between, which with the return address on top
        // of it is already where a call wants the stack pointer, so nothing is added for that.
        assert_eq!(size(&[ragged, whole]), 56);
    }

    #[test]
    fn a_build_whose_locals_keep_their_own_bytes_still_shares_the_spill_slots() {
        let (mut building, block) = Building::new();
        let first = building.local(block, 0);
        building.through(block, first);
        let second = building.local(block, 1);
        building.through(block, second);
        // Two stretches of three values with two registers to hand out, one after the other, so
        // what goes to the stack in the first is finished with before the second starts.
        for _ in 0..2 {
            let values: Vec<Reg> = (0..3).map(|_| building.value(block)).collect();
            for &reg in &values {
                building.held(block, reg);
            }
        }
        let (_, allocation) = building.allocate(2, 2);

        let widths = vec![8; allocation.assignment.spilled()];
        let plan = Slots::share(&building.func, None, &allocation, &[WORD, WORD], &widths);
        assert_ne!(plan.local(0), plan.local(1), "a variable somebody can ask for keeps its bytes");
        assert_eq!(plan.slot(0), plan.slot(1), "and two spilled values that never meet share");
    }

    /// A spill slot wanting bytes over one stretch of the line.
    fn slot(number: usize, start: Point, end: Point) -> Want {
        Want { what: What::Slot(number), size: 8, align: 8, area: Some(vec![Range { start, end }]) }
    }

    #[test]
    fn what_is_left_when_the_budget_runs_out_gets_bytes_of_its_own() {
        // Three that are never both wanted, which is one run of bytes for the three of them when
        // there is anything to spend on finding that out.
        let three = || vec![slot(0, 0, 10), slot(1, 20, 30), slot(2, 40, 50)];
        assert_eq!(fit(three(), 0, 3, BUDGET).cells().len(), 1);
        // One comparison puts the second beside the first and leaves nothing for the third.
        assert_eq!(fit(three(), 0, 3, 1).cells().len(), 2);
        assert_eq!(fit(three(), 0, 3, 0).cells().len(), 3);
    }

    #[test]
    fn skipping_ahead_finds_every_clash_a_walk_one_piece_at_a_time_would() {
        let piece = |start, end| Range { start, end };
        // A cell many things went into, every other stretch of ten points.
        let cell: Vec<Range> = (0..40).map(|at| piece(at * 20, at * 20 + 9)).collect();
        let asked = [
            vec![piece(10, 19)],
            vec![piece(795, 900)],
            vec![piece(10, 19), piece(330, 339), piece(509, 509)],
            vec![piece(10, 19), piece(330, 339), piece(510, 519)],
            vec![piece(0, 0)],
            vec![piece(801, 802)],
        ];
        for area in &asked {
            let walked = cell.iter().any(|one| area.iter().any(|two| one.overlaps(*two)));
            assert_eq!(clashes(&cell, area), walked, "{area:?}");
            assert_eq!(clashes(area, &cell), walked, "{area:?}");
        }
        assert_eq!(ending_before(&cell, 0), 0);
        assert_eq!(ending_before(&cell, 10), 1);
        assert_eq!(ending_before(&cell, 700), 35);
        assert_eq!(ending_before(&cell, 5000), 40);
    }
}
