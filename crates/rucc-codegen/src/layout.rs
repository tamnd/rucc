//! Putting the blocks in an order, and turning the edges between them into jumps.
//!
//! Design: `spec/10-backend.md` section 10.6.
//!
//! Up to here a function is a set of blocks and a set of edges, and nothing has said which block
//! comes first in memory. A machine has no such thing: it runs the instruction after the one it
//! just ran, so an order is not a presentation detail but the last piece of what the function
//! means. This is what chooses one, and then writes the jumps that make the edges the order did
//! not put next to each other still go where they went.
//!
//! # What the order is
//!
//! Two orders, and which one is used is what `-freorder-blocks` asks about.
//!
//! At `-O0`, reverse postorder over the CFG, with each block's successors walked in reverse, and
//! anything unreachable put at the end in block order. That is the order `spec/10-backend.md`
//! section 10.3 asks for, and it is not arbitrary. Walking the successors in reverse is what
//! makes the first arm of a branch come out first, because a depth-first walk finishes its last
//! child first and reverse postorder then puts that child last. So an `if` with no `else` falls
//! through into its body, and a loop comes out as its header, its body and then whatever follows
//! it, which is the shape where the back edge is the only jump in it.
//!
//! Above it, traces: the software trace cache construction of
//! `spec/optimizer/38-scheduling-and-layout.md` section 38.4, which is `traces` below.
//!
//! Unreachable blocks are laid out rather than deleted. Deleting one is a decision about what the
//! program does and this pass has no business making it, and a block nothing reaches costs the
//! bytes it occupies and nothing else.
//!
//! # What a block looks like afterwards
//!
//! A block still holds where it goes, and it still holds every arm, which is what keeps the
//! control flow graph readable after this has run. What changes is that the order the arms are in
//! now means something it did not mean before:
//!
//! ```text
//!   no arms      it returns
//!   one arm      it falls into that block if that block is next, and jumps to it if not
//!   two arms     a test and a conditional jump to the first, and the second is always next
//! ```
//!
//! So a jump target is a block without an instruction growing a field for one.
//! `rucc_mir::InstData` is twenty four bytes by assertion and a block reference does not fit in
//! it, and every pass over the graph already reads the arms, so putting the target where the
//! graph already is costs nothing and keeps the two from disagreeing.
//!
//! Which arm is which is no longer which way the condition went, because a block that falls into
//! the arm the condition is true for is a block whose jump has to be taken when it is false. That
//! is what the two conditional jumps in [`BranchInsts`] are for, and it is why the arms may come
//! out swapped: what the condition meant is in the opcode afterwards, and what the arms mean is
//! where the jump goes and what comes next.
//!
//! # The block a branch sometimes needs
//!
//! A branch whose second arm cannot be laid out next, because both its arms are blocks the walk
//! has already been to, would need two jumps in one block. Rather than write one, this makes the
//! block it needs: an empty one on the second edge, laid out immediately after the branch, that
//! jumps where the edge went. That is exactly the critical edge splitting in [`crate::split`],
//! done for a different reason, and it costs the same jump the second jump would have cost while
//! leaving every block with at most one.
//!
//! # The test a comparison makes unnecessary
//!
//! Almost every branch a C program writes is on a comparison, and a comparison has already set
//! the flags by the time the byte it wrote is tested against itself. So where the instruction in
//! front of the branch is that comparison, and the branch is the whole of what reads its byte,
//! the byte and the test both go and the jump names the condition the comparison was asked about
//! instead of naming zero. Three instructions become two, and the two are what the machine has a
//! comparison and a conditional jump for.
//!
//! This is where it happens rather than anywhere earlier because of what the flags are. Between
//! the comparison and the jump they are live and they are not a register: no pass could be told
//! about them, so no pass may put an instruction between the two. After this one there is no pass
//! left, which is the whole of the argument, and it is the same argument
//! `rucc_target::x86_64::Form::CmpSet` is one form rather than two under.
//!
//! What this cannot work out for itself is whether the byte has another reader. Every register is
//! physical by the time this runs and a physical register is written many times in a function, so
//! the question has to be asked while they are still virtual and written once. [`fusable`] is that
//! question, asked before allocation, and its answer is one of the arguments to [`blocks`]. The
//! same arrangement, and for the same reason, as the addresses [`crate::finish`] has still to
//! write and [`crate::fold`] is handed.
//!
//! # Why it runs last
//!
//! [`crate::finish`] finds the blocks a function returns from by looking for the ones that go
//! nowhere. Nothing here creates one of those, but everything here reads and writes the arms, and
//! a pass that reorders them is one nothing before it should be looking at. Running the layout
//! after the prologue and the epilogue are in is also what makes the epilogue something it can
//! lay out around rather than something it has to leave room for.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{BranchInsts, Fusion, Role};

/// The scale a weight is in, which is what a share of a block is worked out against.
const SCALE: u128 = mir::Weight::SCALE as u128;

/// Puts a function's blocks in an order and writes the jumps that order needs.
///
/// Run last, after [`crate::finish`].
///
/// # Panics
///
/// Panics on a block with more than two successors, which nothing lowers to yet, and on a block
/// with two whose last instruction is not the conditional branch the target named. Both are a
/// function that was built wrongly somewhere earlier, and both are worth finding here rather than
/// as a jump to the wrong place.
pub fn blocks(
    func: &mut mir::Func,
    insts: &BranchInsts,
    names: &mut Interner,
    fusable: &HashSet<mir::Inst>,
    reorder: bool,
) {
    let table = table(insts, names);
    let mut order = if reorder { traces(func) } else { order(func) };
    let mut writer = Writer { func, insts, names, table, fusable };
    let mut at = 0;
    while at < order.len() {
        // A branch that can fall into neither arm asks for a block to put the second jump in, and
        // that block goes immediately after it, which is where the loop reaches it next.
        if let Some(bridge) = writer.edges(order[at], order.get(at + 1).copied()) {
            order.insert(at + 1, bridge);
        }
        at += 1;
    }
    func.set_block_order(&order);
}

/// The order the blocks are laid out in, which is every block the function has exactly once.
fn order(func: &mir::Func) -> Vec<mir::Block> {
    let mut order = Vec::with_capacity(func.block_count());
    let mut seen = vec![false; func.block_count()];
    if let Some(entry) = func.entry() {
        seen[entry.index()] = true;
        // The walk is explicit rather than recursive because a function with a hundred thousand
        // blocks in it is a function somebody generated, and it should compile rather than run out
        // of stack. Each entry is a block and how many of its arms have been started.
        let mut stack = vec![(entry, 0usize)];
        while let Some((block, next)) = stack.pop() {
            let succs = &func[block].succs;
            let Some(arm) = succs.len().checked_sub(next + 1) else {
                order.push(block);
                continue;
            };
            stack.push((block, next + 1));
            let to = succs[arm].block;
            if !std::mem::replace(&mut seen[to.index()], true) {
                stack.push((to, 0));
            }
        }
        order.reverse();
    }
    // Whatever the walk did not reach, in the order the blocks were made, which is the only order
    // there is anything to be said for when nothing goes to any of them.
    order.extend(func.blocks().filter(|block| !seen[block.index()]));
    order
}

/// The rounds the traces are built in, each asking for less than the one before it.
///
/// Design: `spec/optimizer/38-scheduling-and-layout.md` section 38.4, which quotes
/// `gcc/bb-reorder.cc:32` on why there is more than one round: a first round that only follows
/// the arms almost always taken builds the trunk of the function, and the rounds below it pick up
/// what is left without being able to break the trunk apart. It costs one more pass over the
/// blocks per round and it is the difference between "stc" and "simple".
///
/// A round is a pair. The first number is how likely an arm has to be for the trace to follow it,
/// in parts of [`mir::Weight::SCALE`], which is GCC's branch threshold. The second is how often
/// the block at the end of that arm has to run, in the same parts of how often the function is
/// entered, which is GCC's exec threshold. The last round asks for nothing, which is what makes
/// every block end up somewhere.
///
/// The eight numbers are GCC's own, out of `branch_threshold` and `exec_threshold` in
/// `gcc/bb-reorder.cc`, in ten thousandths where GCC writes thousandths. Two things about them
/// are worth saying out loud because both were got wrong here first.
///
/// The branch threshold is low. Two fifths, not nine tenths: an arm taken half the time is an arm
/// the first round follows, and since one arm of a two way branch always is, the first round walks
/// straight through an unpredicted function the way a depth first walk would. A high threshold
/// stops the trace at every branch nothing predicted, which is most of them, and hands both arms
/// back to the seed list to be laid out by weight, and weight is exactly what has nothing to say
/// about them.
///
/// The exec threshold is against the entry and not against the hottest block. A block that runs
/// once per call is a block in the trunk of the function, and measuring it against a loop that
/// runs twenty times a call makes the whole trunk cold: the preheader of every loop lands at the
/// end of the function behind a jump, which is the opposite of what this is for.
const ROUNDS: [(u64, u64); 4] = [(4_000, 5_000), (2_000, 2_000), (1_000, 500), (0, 0)];

/// The order the blocks are laid out in above `-O0`, which is traces grown from the hottest
/// blocks outwards.
///
/// Design: `spec/optimizer/38-scheduling-and-layout.md` section 38.4.
///
/// A trace is a run of blocks that control is expected to walk straight through. It is grown from
/// a seed by repeatedly taking the arm most likely to be the one taken, stopping when no arm is
/// likely enough for the round or when the likeliest one leads somewhere the layout has already
/// been. Every block is a seed in some round, the hotter ones first, and the traces come out in
/// the order they were grown. So the function's trunk is laid out first and contiguously, its
/// error paths end up behind it, and the branch that leaves the trunk is the one that costs a
/// jump.
///
/// The entry is the first seed whatever its weight, because on this machine a function is entered
/// at its first byte and the block laid out first is the block that runs first. A hotter block
/// inside a loop would otherwise take the seat.
///
/// The traces are then run together by [`connect`], which is what keeps a run of blocks the rounds
/// cut in half from coming out in two places.
///
/// # Which block the next trace starts at
///
/// Not simply the hottest one left. A block something already laid out goes to comes first, and
/// among those the one with the hottest edge into it, which is [`Seed`] and which is GCC's
/// `bb_to_key` in `gcc/bb-reorder.cc`. The reason is the whole of what a layout costs: a block laid
/// out in front of everything that reaches it pays a jump on every one of those paths and saves
/// nothing, and a block laid out behind the trace that reaches it pays nothing on the path that
/// falls into it. Seeding by weight alone gets this wrong on the commonest shape in C, which is two
/// arms that both end at one block: the block both arms join at is the hottest of the three and
/// goes first, and then both arms jump to it.
///
/// # Loop rotation, and where it comes from
///
/// Section 38.4 asks for the loop to be rotated so that its exit is the last block of the trace,
/// and there is no step here that does it. It falls out of the walk instead: a trace that enters
/// a loop header follows the body, reaches the latch, finds that the latch's likeliest arm is the
/// header it has already laid out, and stops. The exit is then a seed of its own and comes next.
/// That is the rotated order, back edge running backwards and exit falling through, arrived at
/// from the greedy rule rather than from a rule about loops.
///
/// What that does not cover is a loop whose header is its exit test and whose body is cold, where
/// GCC would duplicate the header. Section 38.4 says the first version should not copy code and
/// this does not.
fn traces(func: &mir::Func) -> Vec<mir::Block> {
    // Where the shape of the graph would have put each block, which is what decides between two
    // blocks that run equally often. Most branches in most functions have nothing to predict them
    // by and come out even, so without this the seed order between them would be the order the
    // blocks happen to have been made in, and a block that falls into the one after it under
    // [`order`] would be laid out somewhere else for no reason and pay a jump for it.
    let mut place = vec![usize::MAX; func.block_count()];
    for (at, &block) in order(func).iter().enumerate() {
        place[block.index()] = at;
    }

    let mut found: Vec<Vec<mir::Block>> = Vec::new();
    let mut seen = vec![false; func.block_count()];
    // How often the function is entered, which every exec threshold is a share of. A function
    // whose entry says nothing is one nobody wrote a weight on, and then once is the right answer
    // for every block in it and every round behaves the same.
    let entered = func.entry().map_or(mir::Weight::ONCE, |entry| func[entry].weight).raw();
    // The hottest edge into each block out of a block already laid out, which is what the queue is
    // ordered by and what says whether an entry popped off it is out of date. It outlives the
    // round it was written in on purpose: a trace that stops because the next block is below this
    // round's exec threshold leaves that block remembered as reached, and the round that does take
    // it starts its first trace there rather than wherever the weights happen to point. That is
    // how a chain of comparisons whose tail cools off below the threshold stays a straight line.
    let mut reached = vec![0; func.block_count()];

    for (likely, often) in ROUNDS {
        // The exec threshold as a number rather than a fraction. In a hundred and twenty eight
        // bits because a weight saturates at the top of a sixty four bit one and a nest of loops
        // gets there.
        let floor =
            u64::try_from(u128::from(entered) * u128::from(often) / SCALE).unwrap_or(u64::MAX);
        // A round does not start a trace in a block colder than its exec threshold, which is what
        // keeps an error path out of the middle of the trunk: it waits for a round that asks for
        // less. The entry is the exception below, because the block laid out first is the block
        // that runs first and that has to be the entry whatever it weighs.
        let mut queue: BinaryHeap<Seed> = func
            .blocks()
            .filter(|&block| !seen[block.index()] && func[block].weight.raw() >= floor)
            .map(|block| Seed {
                reached: reached[block.index()],
                weight: func[block].weight,
                place: Reverse(place[block.index()]),
                block,
            })
            .collect();
        let mut start = func.entry().filter(|entry| !seen[entry.index()]);

        while let Some(from) = start.take().or_else(|| next_seed(&mut queue, &seen, &reached)) {
            let mut trace = Vec::new();
            let mut block = from;
            loop {
                seen[block.index()] = true;
                trace.push(block);
                let next = along(func, block, &seen, likely, floor);
                // Everything this block goes to and the trace does not, so that the next trace can
                // start at one of them rather than wherever the weights point. A block too cold
                // for this round is still written down as reached, because the round that is cold
                // enough to take it wants to know it hangs off something already laid out.
                for call in &func[block].succs {
                    let to = call.block;
                    if seen[to.index()]
                        || Some(to) == next
                        || call.weight.raw() <= reached[to.index()]
                    {
                        continue;
                    }
                    reached[to.index()] = call.weight.raw();
                    if func[to].weight.raw() >= floor {
                        queue.push(Seed {
                            reached: call.weight.raw(),
                            weight: func[to].weight,
                            place: Reverse(place[to.index()]),
                            block: to,
                        });
                    }
                }
                let Some(next) = next else { break };
                block = next;
            }
            found.push(trace);
        }
    }
    connect(func, found)
}

/// The traces run together into one order, each one followed where possible by the trace control
/// leaves it for.
///
/// Design: `gcc/bb-reorder.cc`, `connect_traces`.
///
/// The rounds cut a straight run of blocks into pieces whenever the run cools below the round's
/// exec threshold, and a chain of comparisons against a constant is exactly that: each comparison
/// is reached only when every one before it failed, so the chain halves in weight at every step and
/// the round that laid the head of it down will not touch the tail. Left alone, the pieces come out
/// in round order with other traces between them, and every piece pays a jump to reach the next.
///
/// So the pieces are put back together. Each trace is followed by the unplaced trace its last block
/// most often goes to, and that one by the trace its last block most often goes to, until there is
/// none, and only then does the next trace in round order start a new run. The rounds still decide
/// which trace is hot and comes first, and this decides what falls in behind it.
fn connect(func: &mir::Func, traces: Vec<Vec<mir::Block>>) -> Vec<mir::Block> {
    // Which trace each block starts, for the blocks that start one. A trace may only be joined at
    // its first block, because joining it anywhere else would mean cutting it in half and the
    // rounds put it together for a reason.
    let mut head = vec![usize::MAX; func.block_count()];
    for (at, trace) in traces.iter().enumerate() {
        if let Some(&first) = trace.first() {
            head[first.index()] = at;
        }
    }

    let mut order = Vec::with_capacity(func.block_count());
    let mut used = vec![false; traces.len()];
    for from in 0..traces.len() {
        if used[from] {
            continue;
        }
        let mut at = from;
        loop {
            used[at] = true;
            order.extend_from_slice(&traces[at]);
            let Some(&last) = traces[at].last() else { break };
            let mut best: Option<(u64, usize)> = None;
            for call in &func[last].succs {
                let to = head[call.block.index()];
                if to == usize::MAX || used[to] {
                    continue;
                }
                let weight = call.weight.raw();
                // Ties go to the trace found first, which is the hotter of the two, because the
                // rounds laid the traces down hottest first.
                if best.is_none_or(|(found, over)| weight > found || (weight == found && to < over))
                {
                    best = Some((weight, to));
                }
            }
            let Some((_, next)) = best else { break };
            at = next;
        }
    }
    order
}

/// A block a trace could start at, ordered so that the greatest is the one to start at next.
///
/// Design: `gcc/bb-reorder.cc`, `bb_to_key`, of which this is the same three answers in the order
/// GCC asks them.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Seed {
    /// How often the hottest edge into this block out of a block already laid out is taken, and
    /// zero while nothing laid out goes here. First, so that a block something reaches beats a
    /// block nothing reaches however hot the second one is.
    reached: u64,
    /// How often the block runs, which decides between two blocks nothing laid out reaches.
    weight: mir::Weight,
    /// Where reverse postorder would have put it, which decides between two blocks that are equal
    /// on both of the above, so that a function with no weights on it comes out in the order the
    /// shape of its graph gives rather than in whatever order the queue settles.
    place: Reverse<usize>,
    /// The block, last, so that two blocks equal on everything else still come out in one order.
    block: mir::Block,
}

/// The next block to start a trace at, out of the queue, or nothing when there is none left.
///
/// An entry whose block has been laid out since it was queued, or which was queued before a hotter
/// edge into the same block was found, is thrown away here rather than found and updated in place
/// when that happens. The queue is a heap and an entry in the middle of one cannot be reached, so
/// the choice is between this and an index beside it, and a stale entry costs one pop.
fn next_seed(queue: &mut BinaryHeap<Seed>, seen: &[bool], reached: &[u64]) -> Option<mir::Block> {
    while let Some(seed) = queue.pop() {
        if !seen[seed.block.index()] && seed.reached >= reached[seed.block.index()] {
            return Some(seed.block);
        }
    }
    None
}

/// The arm the trace follows out of a block, or nothing when no arm is worth following.
///
/// The likeliest arm that has not been laid out already, is taken at least as often as the
/// round's floor, and takes at least the round's share of the times the block runs. Ties go to
/// the arm written first, which is the arm a conditional branch takes when its condition holds,
/// so a function with no weights on it at all comes out following the true arm.
fn along(
    func: &mir::Func,
    block: mir::Block,
    seen: &[bool],
    likely: u64,
    floor: u64,
) -> Option<mir::Block> {
    let whole = func[block].weight;
    let mut best: Option<&mir::BlockCall> = None;
    for call in &func[block].succs {
        if seen[call.block.index()]
            || call.weight.raw() < floor
            || call.weight.out_of(whole) < likely
        {
            continue;
        }
        if best.is_none_or(|found| call.weight > found.weight) {
            best = Some(call);
        }
    }
    best.map(|call| call.block)
}

/// The comparisons a branch may be folded into, which [`blocks`] can then find by opcode.
///
/// One entry per name the target's table holds, interned once for the function rather than once
/// per block, since a block that ends in a branch is most of the blocks there are.
fn table(insts: &BranchInsts, names: &mut Interner) -> HashMap<mir::Opcode, &'static Fusion> {
    insts
        .fused
        .iter()
        .map(|fusion| {
            (mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, fusion.set))), fusion)
        })
        .collect()
}

/// The comparisons a branch on their answer is the whole of what reads, which [`blocks`] may fold
/// the test out of.
///
/// Run before allocation, on the same function [`blocks`] is later given. What it answers is
/// whether anything but the branch reads the byte a comparison wrote, and that is a question about
/// a virtual register: a physical one is written many times in a function and counting its readers
/// would mean asking which of the writes each reader belongs to. So it is asked here, where a
/// register is written once, and the answer is carried to the pass that can use it.
///
/// Being on this list is necessary and not sufficient. Allocation may put a reload between the
/// comparison and the branch, and a comparison that is no longer the instruction in front of the
/// branch is not one the flags survive to, so [`blocks`] checks that again on what it finds.
#[must_use]
pub fn fusable(func: &mir::Func, insts: &BranchInsts, names: &mut Interner) -> HashSet<mir::Inst> {
    let table = table(insts, names);
    let branch = mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, insts.cond)));
    let reads = crate::fold::reads(func);
    let mut found = HashSet::new();
    for block in func.blocks() {
        let insts: Vec<mir::Inst> = func.insts(block).collect();
        let [.., compare, last] = insts[..] else { continue };
        if func[last].opcode != branch || !table.contains_key(&func[compare].opcode) {
            continue;
        }
        let operands = &func[func[compare].operands];
        let Some(byte) = operands.first().filter(|operand| operand.role != Role::Use) else {
            continue;
        };
        if !byte.reg.is_virtual() || reads.get(&byte.reg) != Some(&1) {
            continue;
        }
        // And it is this branch that reads it rather than one in some other block, which the
        // count alone does not say.
        if func[func[last].operands].first().map(|operand| operand.reg) == Some(byte.reg) {
            found.insert(compare);
        }
    }
    found
}

/// The one thing that writes an instruction here, over the function it writes into.
struct Writer<'a> {
    func: &'a mut mir::Func,
    insts: &'a BranchInsts,
    names: &'a mut Interner,
    table: HashMap<mir::Opcode, &'static Fusion>,
    fusable: &'a HashSet<mir::Inst>,
}

impl Writer<'_> {
    /// Writes the jumps one block needs, given the block laid out after it, and gives back the
    /// block that has to go between the two when the branch needed one.
    fn edges(&mut self, block: mir::Block, next: Option<mir::Block>) -> Option<mir::Block> {
        match self.func[block].succs.len() {
            0 => None,
            1 => {
                self.one(block, next);
                None
            }
            2 => self.two(block, next),
            arms => panic!("a block with {arms} arms, and nothing lowers to one"),
        }
    }

    /// A block that goes to one place, which either follows it or has to be jumped to.
    fn one(&mut self, block: mir::Block, next: Option<mir::Block>) {
        if Some(self.func[block].succs[0].block) == next {
            return;
        }
        let opcode = self.opcode(self.insts.jump);
        self.func.build(block, opcode).finish();
    }

    /// A block that goes to two places, which is a test and a jump to one of them.
    ///
    /// The condition is read off the branch the rules selected and the branch is taken out, so the
    /// register the test reads is the one the branch read and no new value is made. That is what
    /// makes this safe to run after allocation: it writes no register that was not already
    /// written and it asks for none that was not already asked for.
    fn two(&mut self, block: mir::Block, next: Option<mir::Block>) -> Option<mir::Block> {
        // Asked before the branch is taken out, because what it looks at is the instruction in
        // front of the branch and taking the branch out would make that the last one.
        let fused = self.fused(block);
        let condition = self.take(block);

        // Whichever arm is laid out next is the one the block falls into, and the jump is then
        // the one taken when the condition sends it the other way. Falling into the arm the
        // condition is false for leaves the jump taken when it holds, and falling into the arm it
        // is true for leaves the other jump and the arms the other way round.
        let (if_true, if_false) = match fused {
            Some((_, fusion)) => (fusion.if_true, fusion.if_false),
            None => (self.insts.if_true, self.insts.if_false),
        };
        let arms: Vec<mir::Block> = self.func[block].succs.iter().map(|arm| arm.block).collect();
        let (name, bridge) = if next == Some(arms[1]) {
            (if_true, None)
        } else if next == Some(arms[0]) {
            self.func.succs_mut(block).swap(0, 1);
            (if_false, None)
        } else {
            (if_true, Some(self.bridge(block)))
        };

        match fused {
            Some((compare, fusion)) => self.keep_only_the_flags(compare, fusion),
            None => {
                let opcode = self.opcode(self.insts.test);
                self.func.build(block, opcode).operand(condition).finish();
            }
        }
        let opcode = self.opcode(name);
        self.func.build(block, opcode).finish();
        bridge
    }

    /// The comparison the block's branch can be folded into, when there is one.
    ///
    /// Three things have to hold and [`fusable`] has already answered the one that cannot be
    /// answered here. What is left is that the comparison is still the instruction in front of the
    /// branch, since allocation may have put a reload between them and the flags do not survive
    /// one, and that the byte the branch reads is the byte that comparison wrote, since the
    /// allocator has since given both of them a physical register and two registers that were
    /// different could have become the same one.
    fn fused(&self, block: mir::Block) -> Option<(mir::Inst, &'static Fusion)> {
        let insts: Vec<mir::Inst> = self.func.insts(block).collect();
        let [.., compare, last] = insts[..] else { return None };
        if !self.fusable.contains(&compare) {
            return None;
        }
        let fusion = *self.table.get(&self.func[compare].opcode)?;
        let byte = self.func[self.func[compare].operands].first()?.reg;
        (self.func[self.func[last].operands].first()?.reg == byte).then_some((compare, fusion))
    }

    /// Turns a comparison that wrote a byte into the same comparison that writes nothing.
    ///
    /// The instruction stays where it is and keeps its immediate, which is the point: what it does
    /// to the flags is what it already did, and the jump written behind it reads those. Only the
    /// operand at the front goes, which is the byte, and the opcode changes to the one that has no
    /// operand there.
    fn keep_only_the_flags(&mut self, compare: mir::Inst, fusion: &Fusion) {
        let read: Vec<mir::Operand> =
            self.func[self.func[compare].operands].iter().skip(1).copied().collect();
        let operands = self.func.push_operands(&read);
        self.func[compare].opcode = self.opcode(fusion.cmp);
        self.func[compare].operands = operands;
    }

    /// Takes the conditional branch off the end of a block and gives back what it read.
    fn take(&mut self, block: mir::Block) -> mir::Operand {
        let branch = self.func.terminator(block).expect("a block with two arms has a branch");
        let cond = self.opcode(self.insts.cond);
        assert_eq!(
            self.func[branch].opcode, cond,
            "a block with two arms whose last instruction is not the branch"
        );
        let operands = self.func[branch].operands;
        let condition = self.func[operands][0];
        self.func.remove_inst(branch);
        condition
    }

    /// Puts an empty block on a branch's second edge, so that the branch has something to fall
    /// into and the jump the edge really needs is in a block of its own.
    fn bridge(&mut self, block: mir::Block) -> mir::Block {
        let bridge = self.func.create_block();
        let edge = self.func[block].succs[1].clone();
        let weight = edge.weight;
        self.func.set_weight(bridge, weight);
        *self.func.succs_mut(bridge) = vec![edge];
        self.func.succs_mut(block)[1] = mir::BlockCall::to(bridge).taken(weight);
        bridge
    }

    /// The opcode of that name on this target, which is the name with the target's prefix in
    /// front of it.
    fn opcode(&mut self, name: &str) -> mir::Opcode {
        mir::Opcode::new(self.names.intern(&format!("{}{name}", self.insts.prefix)))
    }
}

#[cfg(test)]
mod tests {
    use rucc_mir::{BlockCall, Opcode, Operand, Reg};
    use rucc_target::x86_64::{BRANCH, GPR, RAX, RCX, REGS};

    use super::*;

    /// A function with that many blocks, none of which goes anywhere yet.
    fn blank(count: usize) -> (Interner, mir::Func, Vec<mir::Block>) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let blocks = (0..count).map(|_| func.create_block()).collect();
        (names, func, blocks)
    }

    /// Puts a conditional branch at the end of a block, on a register that is already physical
    /// the way one is by the time this pass runs.
    fn branch(func: &mut mir::Func, names: &mut Interner, block: mir::Block, arms: &[mir::Block]) {
        let opcode = Opcode::new(names.intern("x64.br_cond_8"));
        func.build(block, opcode).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        *func.succs_mut(block) = arms.iter().map(|&arm| BlockCall::to(arm)).collect();
    }

    /// Laying the blocks out for the one machine this crate has, and the dump of what came out.
    ///
    /// The dump rather than the function, because where a jump goes is on the block and the dump
    /// is the one place the instruction and the arm are put back together. A test that read the
    /// two separately would pass on a function whose jump and whose edge disagreed, which is the
    /// mistake this pass is most able to make.
    ///
    /// A block is named in the dump by where it is in the layout rather than by the number it was
    /// made with, which is why every expectation below reads that way and why the order is worth
    /// asserting on its own.
    fn laid_out(func: &mut mir::Func, names: &mut Interner) -> Vec<String> {
        // Both halves, in the order the pipeline runs them, so that a test which builds a
        // comparison in front of its branch sees what a compiled function would see.
        let fusable = fusable(func, &BRANCH, names);
        blocks(func, &BRANCH, names, &fusable, false);
        mir::print_func(func, names, &REGS)
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with("mfunc") && *line != "}")
            .map(|line| line.trim().to_string())
            .collect()
    }

    /// The blocks in layout order, by the number each was made with.
    fn order_of(func: &mir::Func) -> Vec<usize> {
        func.blocks().map(mir::Block::index).collect()
    }

    #[test]
    fn a_block_that_falls_into_the_next_one_gets_no_jump_at_all() {
        let (mut names, mut func, made) = blank(2);
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];

        let text = laid_out(&mut func, &mut names);

        // The arm is still on the block, because the graph is still worth reading, and there is
        // no instruction on it because the block it goes to is the one that runs next anyway.
        assert_eq!(text, ["block0:", "block1", "block1:"]);
    }

    #[test]
    fn a_block_that_goes_somewhere_that_is_not_next_gets_a_jump() {
        let (mut names, mut func, made) = blank(2);
        // A loop with nothing in it and no way out, which is the smallest function there is with
        // an edge that runs backwards. Every layout puts the two blocks in this order, so the
        // second one has nothing after it and its edge has to be a jump.
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];
        *func.succs_mut(made[1]) = vec![BlockCall::to(made[0])];

        let text = laid_out(&mut func, &mut names);

        assert_eq!(text, ["block0:", "block1", "block1:", "x64.jmp block0"]);
    }

    #[test]
    fn a_branch_that_falls_into_its_false_arm_jumps_when_the_condition_holds() {
        let (mut names, mut func, made) = blank(3);
        // A loop whose body is the block it came from: the arm taken when the condition holds is
        // a block the walk has already been to, so the other arm is what comes next.
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];
        branch(&mut func, &mut names, made[1], &[made[0], made[2]]);

        let text = laid_out(&mut func, &mut names);

        assert_eq!(order_of(&func), [0, 1, 2]);
        assert_eq!(
            text,
            [
                "block0:",
                "block1",
                "block1:",
                "x64.test_rr_8 $rax",
                "x64.jcc_ne block0, block2",
                "block2:",
            ]
        );
    }

    #[test]
    fn a_branch_that_falls_into_its_true_arm_jumps_when_the_condition_does_not_hold() {
        let (mut names, mut func, made) = blank(3);
        branch(&mut func, &mut names, made[0], &[made[1], made[2]]);

        let text = laid_out(&mut func, &mut names);

        // The arms come out swapped, because after this the first is where the jump goes and the
        // second is what runs next, and the jump is the one taken when the condition failed.
        assert_eq!(order_of(&func), [0, 1, 2]);
        assert_eq!(
            text,
            ["block0:", "x64.test_rr_8 $rax", "x64.jcc_e block2, block1", "block1:", "block2:"]
        );
    }

    #[test]
    fn a_branch_that_can_fall_into_neither_arm_is_given_a_block_to_jump_from() {
        let (mut names, mut func, made) = blank(2);
        // A loop that goes back to the top or round again, so both arms are blocks the walk has
        // already been to and nothing is left to lay out after it.
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];
        branch(&mut func, &mut names, made[1], &[made[0], made[1]]);

        let text = laid_out(&mut func, &mut names);

        // Block two is the one this made. It is empty, it is laid out where the branch falls into
        // it, and the jump the second arm needed is in it rather than being a second jump in the
        // block above.
        assert_eq!(order_of(&func), [0, 1, 2]);
        assert_eq!(
            text,
            [
                "block0:",
                "block1",
                "block1:",
                "x64.test_rr_8 $rax",
                "x64.jcc_ne block0, block2",
                "block2:",
                "x64.jmp block1",
            ]
        );
    }

    #[test]
    fn the_test_reads_the_register_the_branch_read() {
        let (mut names, mut func, made) = blank(3);
        branch(&mut func, &mut names, made[0], &[made[1], made[2]]);

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable, false);

        let test = func.insts(made[0]).next().expect("a test");
        let operands = func[test].operands;
        assert_eq!(func[operands], [Operand::read(Reg::physical(RAX), GPR)]);
    }

    #[test]
    fn a_block_nothing_reaches_is_laid_out_at_the_end_rather_than_deleted() {
        let (mut names, mut func, made) = blank(4);
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[3])];

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable, false);

        // Blocks one and two are reached by nothing, so they go last, in the order they were
        // made. Deleting one would be a decision about what the program does, and this pass has
        // no business making it.
        assert_eq!(order_of(&func), [0, 3, 1, 2]);
    }

    #[test]
    fn a_function_with_no_blocks_is_left_alone() {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable, false);

        assert_eq!(func.block_count(), 0);
    }

    #[test]
    #[should_panic(expected = "a block with 3 arms")]
    fn a_block_with_three_arms_is_refused_rather_than_laid_out_wrongly() {
        let (mut names, mut func, made) = blank(4);
        branch(&mut func, &mut names, made[0], &[made[1], made[2], made[3]]);

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable, false);
    }

    #[test]
    #[should_panic(expected = "whose last instruction is not the branch")]
    fn a_block_with_two_arms_and_no_branch_in_it_is_refused() {
        let (mut names, mut func, made) = blank(3);
        let opcode = Opcode::new(names.intern("x64.nop"));
        func.build(made[0], opcode).finish();
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1]), BlockCall::to(made[2])];

        let fusable = fusable(&func, &BRANCH, &mut names);
        blocks(&mut func, &BRANCH, &mut names, &fusable, false);
    }

    /// Puts a comparison and a branch on its answer at the end of a block.
    ///
    /// The byte is a virtual register, which is what it is when [`fusable`] is asked and is not
    /// what it is when [`blocks`] runs. Nothing in either half cares which it is except the
    /// counting, so a test that runs both over one function has to use the register the counting
    /// wants, and what it costs is that this is one thing the unit tests cannot check about the
    /// two halves running at different times. `crate::pipeline` runs them the real way round.
    fn compare(
        func: &mut mir::Func,
        names: &mut Interner,
        block: mir::Block,
        arms: &[mir::Block],
    ) -> Reg {
        let byte = func.new_vreg(GPR);
        let opcode = Opcode::new(names.intern("x64.cmp_set_l_32"));
        func.build(block, opcode)
            .def(byte, GPR)
            .operand(Operand::read(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RCX), GPR))
            .finish();
        let opcode = Opcode::new(names.intern("x64.br_cond_8"));
        func.build(block, opcode).operand(Operand::read(byte, GPR)).finish();
        *func.succs_mut(block) = arms.iter().map(|&arm| BlockCall::to(arm)).collect();
        byte
    }

    /// A branch on a comparison is the comparison and a jump on what it found.
    ///
    /// Three instructions go in and two come out. The byte goes because nothing reads it, the test
    /// goes because the comparison set the flags the test was going to set, and the jump names the
    /// condition rather than naming zero. Which condition it names is the opposite of the one the
    /// comparison asked about, since the block falls into the arm the comparison is true for.
    #[test]
    fn a_branch_on_a_comparison_is_the_comparison_and_a_jump_on_what_it_found() {
        let (mut names, mut func, made) = blank(3);
        compare(&mut func, &mut names, made[0], &[made[1], made[2]]);

        let text = laid_out(&mut func, &mut names);

        assert_eq!(
            text,
            [
                "block0:",
                "x64.cmp_rr_32 $rax, $rcx",
                "x64.jcc_ge block2, block1",
                "block1:",
                "block2:",
            ]
        );
    }

    /// The same comparison with something else reading its answer, which keeps everything.
    ///
    /// Folding the byte away when a second instruction wants it would be deleting a value the
    /// program computes. This is the whole of what [`fusable`] is asked before allocation, and the
    /// second reader here is in another block so that it is a question about the function rather
    /// than about the block the branch is in.
    #[test]
    fn a_comparison_whose_answer_something_else_reads_keeps_its_byte_and_its_test() {
        let (mut names, mut func, made) = blank(3);
        let byte = compare(&mut func, &mut names, made[0], &[made[1], made[2]]);
        let opcode = Opcode::new(names.intern("x64.mov_rr_64"));
        func.build(made[1], opcode)
            .def(Reg::physical(RAX), GPR)
            .operand(Operand::read(byte, GPR))
            .finish();

        let text = laid_out(&mut func, &mut names);

        assert!(text.contains(&"x64.test_rr_8 %0".to_owned()), "{text:?}");
        assert!(text.contains(&"x64.jcc_e block2, block1".to_owned()), "{text:?}");
    }

    /// A comparison allocation moved away from its branch, which keeps its test.
    ///
    /// [`fusable`] says the byte has one reader and says nothing about where the two instructions
    /// end up, because allocation runs between the two halves and may put a reload in front of the
    /// branch. The flags do not survive one, so the second half looks again, and this is the case
    /// where it finds something and refuses. The instruction is put in between the two calls
    /// because that is when allocation would have put it there.
    #[test]
    fn a_comparison_that_is_no_longer_in_front_of_its_branch_keeps_its_test() {
        let (mut names, mut func, made) = blank(3);
        compare(&mut func, &mut names, made[0], &[made[1], made[2]]);
        let fusable = fusable(&func, &BRANCH, &mut names);
        assert_eq!(fusable.len(), 1, "the comparison is one the byte's count allows");

        let branch = func.terminator(made[0]).expect("a block with two arms has a branch");
        let opcode = Opcode::new(names.intern("x64.mov_rr_64"));
        let reload = func
            .build_loose(opcode)
            .def(Reg::physical(RCX), GPR)
            .operand(Operand::read(Reg::physical(RAX), GPR))
            .finish();
        func.insert_before(branch, reload);
        blocks(&mut func, &BRANCH, &mut names, &fusable, false);
        let text = mir::print_func(&func, &names, &REGS);

        assert!(text.contains("x64.cmp_set_l_32"), "{text}");
        assert!(text.contains("x64.test_rr_8"), "{text}");
        assert!(!text.contains("x64.cmp_rr_32"), "{text}");
    }

    /// Laying the blocks out along the traces the weights say, which is what every level above
    /// `-O0` asks for.
    fn traced(func: &mut mir::Func, names: &mut Interner) -> Vec<String> {
        let fusable = fusable(func, &BRANCH, names);
        blocks(func, &BRANCH, names, &fusable, true);
        mir::print_func(func, names, &REGS)
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with("mfunc") && *line != "}")
            .map(|line| line.trim().to_string())
            .collect()
    }

    /// Says how often a block runs and how often each of its arms is taken, in parts of ten
    /// thousand, the way `crate::weights` would have.
    fn runs(func: &mut mir::Func, block: mir::Block, weight: u64, arms: &[u64]) {
        func.set_weight(block, mir::Weight::parts(weight));
        for (index, &taken) in arms.iter().enumerate() {
            func.succs_mut(block)[index].weight = mir::Weight::parts(taken);
        }
    }

    /// The arm almost always taken is the one laid out next, whichever of the two it is.
    ///
    /// Same function twice, with the two arms weighted the two ways round. At `-O0` the order is
    /// the shape of the graph and the first arm always comes next; here it is the weights, so the
    /// block that hardly ever runs goes behind the one that nearly always does and the jump is
    /// spent on it rather than on the common path.
    #[test]
    fn the_arm_that_is_nearly_always_taken_is_the_one_laid_out_next() {
        let (mut names, mut func, made) = blank(3);
        branch(&mut func, &mut names, made[0], &[made[1], made[2]]);
        runs(&mut func, made[0], 10_000, &[200, 9_800]);
        runs(&mut func, made[1], 200, &[]);
        runs(&mut func, made[2], 9_800, &[]);

        traced(&mut func, &mut names);

        assert_eq!(order_of(&func), [0, 2, 1]);

        let (mut names, mut func, made) = blank(3);
        branch(&mut func, &mut names, made[0], &[made[1], made[2]]);
        runs(&mut func, made[0], 10_000, &[9_800, 200]);
        runs(&mut func, made[1], 9_800, &[]);
        runs(&mut func, made[2], 200, &[]);

        traced(&mut func, &mut names);

        assert_eq!(order_of(&func), [0, 1, 2]);
    }

    /// A loop comes out as its header, its body and then its exit, with the back edge backwards.
    ///
    /// Nothing here rotates anything. The trace walks out of the header into the body because the
    /// body is where the header nearly always goes, stops at the latch because the header it
    /// wants next is already laid out, and the exit is picked up as the next seed. That is the
    /// order a branch predictor's static guess expects and it is what the greedy rule gives.
    #[test]
    fn a_loop_is_laid_out_with_its_exit_behind_it_and_its_back_edge_running_backwards() {
        let (mut names, mut func, made) = blank(4);
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];
        branch(&mut func, &mut names, made[1], &[made[2], made[3]]);
        *func.succs_mut(made[2]) = vec![BlockCall::to(made[1])];
        runs(&mut func, made[0], 10_000, &[10_000]);
        runs(&mut func, made[1], 100_000, &[90_000, 10_000]);
        runs(&mut func, made[2], 90_000, &[90_000]);
        runs(&mut func, made[3], 10_000, &[]);

        let text = traced(&mut func, &mut names);

        assert_eq!(order_of(&func), [0, 1, 2, 3]);
        assert_eq!(
            text,
            [
                "block0:",
                "block1",
                "block1:",
                "x64.test_rr_8 $rax",
                "x64.jcc_e block3, block2",
                "block2:",
                "x64.jmp block1",
                "block3:",
            ]
        );
    }

    /// A block reached only from the cold arm is laid out behind everything the trunk reaches.
    ///
    /// The shape is `if (unlikely) handle(); rest();`, where the handler and the rest of the
    /// function are both reached from the branch. Reverse postorder puts the handler between the
    /// branch and the rest of the function; the trace puts the rest of the function next, because
    /// that is where the branch nearly always goes, and the handler ends up last.
    #[test]
    fn a_block_only_the_cold_arm_reaches_goes_behind_the_rest_of_the_function() {
        let (mut names, mut func, made) = blank(4);
        branch(&mut func, &mut names, made[0], &[made[1], made[2]]);
        *func.succs_mut(made[1]) = vec![BlockCall::to(made[2])];
        *func.succs_mut(made[2]) = vec![BlockCall::to(made[3])];
        runs(&mut func, made[0], 10_000, &[100, 9_900]);
        runs(&mut func, made[1], 100, &[100]);
        runs(&mut func, made[2], 10_000, &[10_000]);
        runs(&mut func, made[3], 10_000, &[]);

        assert_eq!(order(&func), [made[0], made[1], made[2], made[3]]);

        traced(&mut func, &mut names);

        assert_eq!(order_of(&func), [0, 2, 3, 1]);
    }

    /// A block nothing reaches is still laid out, since the last round asks for nothing.
    #[test]
    fn the_last_round_picks_up_a_block_nothing_reaches() {
        let (mut names, mut func, made) = blank(3);
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[2])];
        runs(&mut func, made[0], 10_000, &[10_000]);
        runs(&mut func, made[1], 0, &[]);
        runs(&mut func, made[2], 10_000, &[]);

        traced(&mut func, &mut names);

        assert_eq!(order_of(&func), [0, 2, 1]);
    }

    /// The entry is laid out first however cold it is against the rest of the function.
    ///
    /// A function is entered at its first byte, so the block that runs first has to be the block
    /// that is written first, and the seed order is what makes that true rather than any check
    /// afterwards. Here the loop body runs ten times for every call and would otherwise have been
    /// the first seed.
    #[test]
    fn the_entry_is_the_first_seed_even_when_something_else_runs_more_often() {
        let (mut names, mut func, made) = blank(3);
        *func.succs_mut(made[0]) = vec![BlockCall::to(made[1])];
        branch(&mut func, &mut names, made[1], &[made[1], made[2]]);
        runs(&mut func, made[0], 10_000, &[10_000]);
        runs(&mut func, made[1], 100_000, &[90_000, 10_000]);
        runs(&mut func, made[2], 10_000, &[]);

        traced(&mut func, &mut names);

        assert_eq!(func.blocks().next().map(mir::Block::index), Some(0));
    }

    /// A branch whose arms are even still falls into one of them rather than jumping to both.
    ///
    /// Nothing predicts a range check, so both arms come out at half, and half is under every
    /// branch threshold above the last round. The trace therefore ends at the branch, and what
    /// decides the layout is where the next one starts: at the likeliest arm out of the block the
    /// trace stopped in, which is a fall-through, and not at whichever of the two blocks was made
    /// first, which would have cost a jump on both paths out of an even branch.
    #[test]
    fn a_branch_whose_arms_are_even_is_still_laid_out_next_to_one_of_them() {
        let (mut names, mut func, made) = blank(3);
        // The second arm is the block made first, so a layout that fell back to the seed list
        // would lay that one out next and leave the arm written first to be jumped to.
        branch(&mut func, &mut names, made[0], &[made[2], made[1]]);
        runs(&mut func, made[0], 10_000, &[5_000, 5_000]);
        runs(&mut func, made[1], 5_000, &[]);
        runs(&mut func, made[2], 5_000, &[]);

        traced(&mut func, &mut names);

        assert_eq!(order_of(&func), [0, 2, 1]);
    }

    /// A run of blocks the rounds cut in half comes back out in one piece.
    ///
    /// Two comparisons against a constant, one behind the other, which is what a switch over
    /// scattered labels is lowered to. The second comparison is only reached when the first one
    /// failed, so it runs half as often as the function is entered and the first round will not
    /// touch it: the trace stops at the first comparison and the block that was about to fall
    /// through it is left for a later round. What puts it back is [`connect`], and without it the
    /// body of the first case would sit between the two comparisons and both would pay a jump.
    #[test]
    fn a_chain_the_rounds_cut_in_half_is_run_back_together() {
        let (mut names, mut func, made) = blank(5);
        branch(&mut func, &mut names, made[0], &[made[2], made[1]]);
        branch(&mut func, &mut names, made[2], &[made[4], made[3]]);
        runs(&mut func, made[0], 10_000, &[5_000, 5_000]);
        runs(&mut func, made[1], 5_000, &[]);
        runs(&mut func, made[2], 5_000, &[3_000, 2_000]);
        runs(&mut func, made[3], 2_000, &[]);
        runs(&mut func, made[4], 3_000, &[]);

        traced(&mut func, &mut names);

        assert_eq!(order_of(&func), [0, 2, 4, 1, 3]);
    }
}
