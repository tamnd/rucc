//! Putting the instructions of a block in the order that finishes soonest.
//!
//! Design: `spec/optimizer/38-scheduling-and-layout.md` sections 38.1, 38.6 and 38.7.
//!
//! Every instruction in a block is going to run, in some order, and the orders that compute the
//! same thing are the ones that keep each instruction behind the ones it reads from. Among those
//! orders, one finishes before the others, because the machine does not answer every instruction in
//! one cycle: a multiply takes three, a load takes five, and an instruction that reads what one of
//! them wrote cannot start until it is done. Putting independent work in those cycles rather than
//! waiting is the whole of this pass.
//!
//! # The algorithm, and where it comes from
//!
//! A list scheduler, which is `gcc/haifa-sched.cc`'s. Build a graph of what depends on what, take
//! the instructions whose dependences are all satisfied, choose one, repeat. Everything a scheduler
//! is is in how it chooses, and `gcc/haifa-sched.cc:55` writes that out as a list of eight
//! tiebreaks. Section 38.1 goes through them and says which are rucc's: one, two, six, seven and
//! eight. Three, four and five are about moving instructions between blocks and about moving them
//! where they might not have run, and this pass does neither.
//!
//! So what this pass chooses by is five numbers in that order:
//!
//! 1. The longest path from here to the end of the run, in cycles. This is the criterion, and the
//!    other four are for when it ties. An instruction on the critical path delays everything behind
//!    it by exactly as much as it is delayed, and one that is not on it is free until it is.
//! 2. How many more registers are live after it than before. Section 38.1 quotes
//!    `gcc/haifa-sched.cc:87` on what this is for: "if an operation requires that constants be
//!    loaded into registers, it is certainly desirable to load those constants as early as
//!    necessary, but no earlier". An instruction that writes a register and reads nothing that dies
//!    is one whose value now has to be kept somewhere, and hoisting it to the top of a block
//!    because it depends on nothing is the classic way a scheduler makes a function worse.
//! 3. Whether it reads what the instruction just scheduled wrote. It does not have to, since the
//!    graph would have stopped it if it were not allowed, but one that does will wait and one that
//!    does not will not.
//! 4. How many instructions depend on it. Scheduling one of these makes more work available to
//!    choose from later, which is what keeps the ready list from running dry.
//! 5. Where it was to start with. This is not a heuristic. It is what makes the output a function
//!    of the input, and it has to be a position rather than anything that comes out of a hash map,
//!    for the reason `spec/10-backend.md` gives about a compiler whose output moves between runs.
//!
//! # Why it runs after the registers are handed out
//!
//! Section 38.6 decides it: "One scheduler, after allocation, before the layout freeze." The
//! argument section 38.7 makes for that placement is the one that matters here. The dominant way a
//! scheduler makes a program worse is by holding more values live at once than there are registers,
//! so the allocator spills, and the spill costs more than the latency the schedule hid. After
//! allocation that cannot happen: every value is already in a register, no reordering this pass can
//! make changes which register anything is in, and nothing is left that could decide to spill.
//!
//! What it costs is that the registers are the constraint instead. Before allocation a value is
//! written once, so the only dependence between two instructions is that one reads what the other
//! wrote. Afterwards the same register holds a dozen different values over a block, so an
//! instruction that writes one has to stay behind everything that reads what was in it, and those
//! orderings are real even though no value passes between the two instructions. That is most of
//! what the graph below is made of, and it is why this pass finds less to do than one before
//! allocation would.
//!
//! How much less has now been measured, and the honest answer is almost all of it. Five programs
//! built with this on and with it off, best of five runs each, on a six core Xeon with gcc 16 as
//! the reference:
//!
//! ```text
//! program    off    on   accurate   gcc-16 -O2
//! ilp         75    75     78          60
//! serial     213   212    215          58
//! mem         47    49     49          40
//! fp         271   267    266         108
//! branchy    119   122    121          81
//! ```
//!
//! Milliseconds, and the run to run spread on this machine is a few of them, so every column here
//! is the same column. That is the measurement section 38.8 asked for and it says this pass is
//! currently worth nothing on these five programs. Two reasons, and the first is the one above: by
//! the time this runs the registers have been handed out, so the same register holds a dozen values
//! over a block and the anti and output edges that creates pin most of the order in place. The
//! second is that the gap to gcc is not a scheduling gap. A factor of three and a half on `serial`
//! and two and a half on `fp` is work gcc did before it got anywhere near an instruction order, and
//! no permutation of the instructions rucc emits closes it.
//!
//! The pass stays, at `-O2` and above, for what it costs rather than for what it currently returns:
//! it is sound, it is cheap, and it is the thing that has to exist before the latencies in
//! [`rucc_target::TimingInsts`] mean anything at all. The column worth watching is `accurate`, which
//! is the same model told to believe its own unit counts, and which is slightly worse on the one
//! program with real instruction level parallelism in it. That is the model being wrong about units
//! in exactly the way [`rucc_target::TimingInsts::accurate`] says it is, and it is why x86-64
//! answers `false`.
//!
//! # What the graph is made of
//!
//! Four kinds of edge, and the first three are `gcc/sched-deps.cc`'s `REG_DEP_TRUE`,
//! `REG_DEP_OUTPUT` and `REG_DEP_ANTI` over registers:
//!
//! - One instruction reads a register another wrote, so it waits for the value.
//! - Two instructions write the same register, so they stay in order or the register ends up
//!   holding the wrong one of them.
//! - One instruction writes a register another read, so the read stays in front of the write.
//!
//! The fourth is the condition state, which on this kind of machine is a register nobody named. It
//! is not in an operand vector, so the three kinds above do not see it, and the target says which
//! instructions write it and which read it. Getting this wrong is a miscompile and the failure
//! looks like a target description that forgot a clobber, which section 38.7 says is the same root
//! cause as every other missing-clobber bug.
//!
//! # Memory, and why it is one chain
//!
//! Every instruction that touches memory or computes an address stays in the order it was in,
//! relative to every other one. That is stronger than it has to be. `gcc/haifa-sched.cc:71` is
//! candid about the trade: "only if we can be certain that memory references are not part of the
//! data dependency graph... can we move operations past memory references. To first approximation,
//! reads can be done independently, while writes introduce dependencies."
//!
//! rucc cannot take the first approximation here. Machine IR does not carry `volatile`, which
//! [`crate::copies`] says at length: a read the program insisted on and an ordinary one are the
//! same instruction with the same operands by the time this runs. So two reads are not
//! interchangeable either, and the only safe answer at this level is to leave the accesses in the
//! order they arrived in. That is also the answer [`crate::combine`] gives, for the same reason and
//! through the same question to the target.
//!
//! Address computation is in the chain as well, and not because an address is a memory access. It
//! is because the stack pointer moves without saying so. A push and a pop change it and name it in
//! no operand, so an address counted from it means different things on either side of one, and
//! anything that carries an addressing mode is something that could be counted from it. Putting
//! them all in one chain costs a little freedom around `lea` and needs no new question of the
//! target.
//!
//! An instruction that writes the stack pointer is in the chain too, for the opposite reason. Moving
//! it up gives back memory the accesses behind it still use, and those may reach it through any
//! register at all rather than the stack pointer. The epilogue of a frame that saved nothing is
//! `movq %rbp, %rsp` and then `popq %rbp`, and without this the move went to the top of the block
//! in a realigned frame, above every store to the frame, which left the frame below the stack
//! pointer and outside the red zone while the body was still writing it.
//!
//! # What nothing moves across
//!
//! A call, because what a call does to memory and to the registers a convention does not preserve
//! is not in its operands. A branch or a return, for the same reason: a `ret` reads the value in
//! `rax` without naming it. One only turns up in the middle of a block when an `asm` template put
//! it there, and a naked function's `movl $42, %eax; ret` is the case that found it, where the
//! `ret` was moved above the `mov`. An instruction the target does not describe, on the same reasoning
//! backwards. An instruction the target describes as doing something the timing model does not
//! cover, which is [`Unit::Fixed`]: a fence, a trap, a landing pad, the padding a patcher was
//! promised. And an instruction that carries a frame rule, because those rules say what the
//! unwinder should believe at each address in the prologue and the epilogue, and an instruction
//! that moves takes its rule with it to an address where it is not true.
//!
//! The last instruction of a block, as well, along with whatever the caller has pinned. What a
//! block leaves on is the last thing in it by the time [`crate::layout`] runs, and the layout is
//! what turns the arms of a block into jumps, so a block whose condition is not at the end of it is
//! a block the layout cannot write. What the caller pins is the comparison the layout is going to
//! fuse with that condition, since the two have to stay next to each other for the fusion to
//! happen and nothing here would otherwise keep them there.
//!
//! Each of those splits the block into runs, and a run is scheduled on its own with everything
//! before and after it left where it was. A block with no barrier in it is one run.
//!
//! # The bound
//!
//! [`READY`] instructions are considered at each step and no more, which is
//! `gcc/params.opt:761`'s `max-sched-ready-insns`, `Init(100)`, and section 38.8 asks for the same
//! bound for the same reason: choosing is linear in the ready list and the ready list can be as
//! long as the block. [`LONGEST`] is the second half of it, a run this pass will not build a graph
//! for at all, because building one is quadratic in the worst case and a block of several thousand
//! machine instructions is a generated table rather than something anybody is waiting on.
//!
//! # What makes it correct
//!
//! The order this writes is a topological order of the graph, and nothing else about the pass is
//! load bearing. The timing model chooses among the orders the graph allows and cannot choose one
//! it does not allow, so a model that is wrong about every number produces a slower program and not
//! a different one, which is what spec 10.5 says the right failure mode is. What has to be right is
//! the graph, and what makes the graph right is that every edge the machine needs is in it.

use std::collections::BTreeSet;
use std::ops::Range;

use rucc_base::hash::{Map, Set};
use rucc_base::{Interner, Symbol};
use rucc_mir::{Block, Func, Inst, Reg, Role};
use rucc_target::{FlagInsts, MachineInsts, PhysReg, RegClass, Timing, TimingInsts, Unit};

/// A register as the graph keys on it: the number and the file it is in.
///
/// The number on its own is not enough. A [`Reg`] that has been through the allocator is a place on
/// the machine, and a machine numbers the places in each of its files from zero, so the first
/// integer register and the first vector register are the same number and not the same place. A
/// graph keyed on the number alone would chain a block's floating point work to the integer work
/// beside it for no reason, which costs a schedule and is not wrong. A virtual register has one
/// class for its whole life, so for anything that has not been through the allocator the pair says
/// exactly what the number alone would.
type Place = (Reg, RegClass);

/// How many instructions are looked at when choosing the next one.
///
/// `gcc/params.opt:761`'s `max-sched-ready-insns`, `Init(100)`, and the same number for the same
/// reason. The ones looked at are the ones that were earliest in the input, so the bound is a
/// function of the input like everything else here.
pub const READY: usize = 100;

/// The longest run of instructions this will schedule.
///
/// Building the graph is quadratic in the worst case, since an instruction that writes a register
/// has to be put behind every instruction that read it. A run longer than this is left exactly as
/// it arrived.
pub const LONGEST: usize = 2000;

/// What one function came to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Scheduled {
    /// Runs of instructions a schedule was chosen for.
    pub runs: usize,
    /// Instructions that came out somewhere other than where they went in.
    pub moved: usize,
}

/// Puts each block's instructions in the order the machine finishes soonest.
///
/// `accurate` is whether the unit counts in the model are worth holding an instruction back over,
/// which is `cycle-accurate-model` of section 38.1. A model that is not cycle accurate is one whose
/// latencies came out of a table and whose picture of the machine's units is a summary, so the
/// latencies are used to order and the units are not used to stall. See [`TimingInsts::accurate`].
///
/// `pinned` is the instructions the caller needs left where they are. The block's own last
/// instruction is always one, and the caller adds the comparisons [`crate::layout`] is going to
/// fuse with a branch, which have to stay next to the branch for the fusion to happen.
///
/// `stack` is the stack pointer and the file it is in, since a write of it is ordered against
/// memory like an access is.
#[allow(clippy::too_many_arguments)]
pub fn insts(
    func: &mut Func,
    stack: (PhysReg, RegClass),
    timing: &TimingInsts,
    machine: &MachineInsts,
    flags: &FlagInsts,
    names: &Interner,
    accurate: bool,
    pinned: &Set<Inst>,
) -> Scheduled {
    let blocks: Vec<Block> = func.blocks().collect();
    let mut done = Scheduled::default();
    let stack = (Reg::physical(stack.0), stack.1);
    let mut known = Known { timing, machine, flags, names, stack, seen: Map::default() };
    let mut scratch = Scratch::default();
    let (mut was, mut now, mut run) = (Vec::new(), Vec::new(), Vec::new());
    for block in blocks {
        was.clear();
        was.extend(func.insts(block));
        if was.len() < 3 {
            continue;
        }
        now.clear();
        run.clear();
        let last = was.last().copied();
        for &inst in &was {
            if Some(inst) == last || pinned.contains(&inst) || known.of(func, inst).barrier {
                let ordered = order(func, &run, &mut known, accurate, &mut now, &mut scratch);
                done.runs += usize::from(ordered);
                run.clear();
                now.push(inst);
            } else {
                run.push(inst);
            }
        }
        done.runs += usize::from(order(func, &run, &mut known, accurate, &mut now, &mut scratch));
        let moved = was.iter().zip(&now).filter(|(before, after)| before != after).count();
        if moved == 0 {
            continue;
        }
        done.moved += moved;
        for &inst in &was {
            func.remove_inst(inst);
        }
        for &inst in &now {
            func.append_inst(block, inst);
        }
    }
    done
}

/// What the target says about one opcode, which is the same for every instruction spelled that
/// way.
///
/// Every one of these is a lookup by the opcode's name, and a target's tables are matches on
/// strings, so asking them for each instruction compared its name against a few hundred others
/// each time. This pass asked seven such questions of every instruction, and on jtckdint's main,
/// with 190000 of them, the comparing came to a twentieth of the `-O2` build.
#[derive(Debug, Clone, Copy)]
struct Facts {
    /// Whether the name alone makes it a barrier. A frame rule after it is about the instruction
    /// rather than the name, so [`Known::of`] asks that one each time.
    barrier: bool,
    /// What it costs, which a barrier by name has none of.
    timing: Option<Timing>,
    reads_flags: bool,
    writes_flags: bool,
    touches_mem: bool,
}

/// The target's tables, and what they have said so far, by opcode.
struct Known<'a> {
    timing: &'a TimingInsts,
    machine: &'a MachineInsts,
    flags: &'a FlagInsts,
    names: &'a Interner,
    stack: Place,
    seen: Map<Symbol, Facts>,
}

impl Known<'_> {
    /// What the target says about this instruction's opcode, and whether nothing may be moved
    /// across the instruction.
    ///
    /// See the module comment. The five barriers are a call, a branch or a return, a name the
    /// target does not have, a name the target has and the timing model does not cover, and an
    /// instruction carrying a frame rule.
    fn of(&mut self, func: &Func, inst: Inst) -> Facts {
        let symbol = func[inst].opcode.name();
        let (timing, machine, flags, names) = (self.timing, self.machine, self.flags, self.names);
        let mut facts = *self.seen.entry(symbol).or_insert_with(|| {
            let name = names.resolve(symbol);
            let bare = name.strip_prefix(flags.prefix).unwrap_or(name);
            let cost = timing.of(name);
            Facts {
                barrier: machine.calls(name)
                    || !machine.has(name)
                    || cost.is_none_or(|cost| matches!(cost.unit, Unit::Fixed | Unit::Branch)),
                timing: cost,
                reads_flags: flags.reads(bare).is_some(),
                writes_flags: (flags.writes)(bare),
                touches_mem: machine.touches_mem(name),
            }
        });
        facts.barrier = facts.barrier || func.cfi_after(inst).next().is_some();
        facts
    }
}

/// Chooses an order for one run and appends it, saying whether there was anything to choose.
fn order(
    func: &Func,
    run: &[Inst],
    known: &mut Known<'_>,
    accurate: bool,
    into: &mut Vec<Inst>,
    scratch: &mut Scratch,
) -> bool {
    if run.len() < 2 || run.len() > LONGEST {
        into.extend_from_slice(run);
        return false;
    }
    graph(func, run, known, scratch);
    list(scratch, known.timing, accurate);
    into.extend(scratch.out.iter().map(|&at| run[at]));
    true
}

/// The lists and maps one run is scheduled with, kept from one run to the next.
///
/// A block is cut into runs at every barrier, and building each run's graph and choosing its order
/// in lists and maps of its own was a dozen allocations a run.
#[derive(Default)]
struct Scratch {
    facts: Vec<Facts>,
    found: Vec<(usize, usize, u32)>,
    wrote: Map<Place, usize>,
    read: Map<Place, usize>,
    readers: Vec<(usize, Option<usize>)>,
    read_flags: Vec<usize>,
    seen: Set<Place>,
    graph: Graph,
    preds: Vec<usize>,
    when: Vec<u32>,
    used: Map<Unit, u32>,
    /// The order chosen for the last run, as positions into it.
    out: Vec<usize>,
}

/// One instruction of a run, and everything the choosing needs to know about it.
#[derive(Debug)]
struct Node {
    /// What it costs, from the target's model.
    timing: Timing,
    /// Where in [`Graph::edges`] the instructions that may not start before it are.
    succs: Range<usize>,
    /// How many instructions it may not start before, counted down as they are scheduled.
    preds: usize,
    /// The longest path from here to the end of the run, in cycles. Criterion one.
    height: u32,
    /// How many more registers are live after it than before. Criterion two.
    ///
    /// Within the run, so a register that is read here and read again in the next block counts as
    /// dying here. Being wrong about that changes which of two instructions with the same critical
    /// path goes first and nothing else, which is what a tiebreak is allowed to be wrong about.
    growth: i32,
}

/// The dependence graph of one run.
#[derive(Debug, Default)]
struct Graph {
    nodes: Vec<Node>,
    /// The instructions that may not start before another, and how long each has to wait, with
    /// those of one instruction side by side.
    ///
    /// The wait is how long the value takes where the edge is one instruction reading what another
    /// wrote, and it is nothing where the edge is only about the two staying in order. One list for
    /// the whole run rather than one per instruction, because a run is built and thrown away for
    /// every stretch between barriers and a list each was an allocation each.
    edges: Vec<(usize, u32)>,
}

impl Graph {
    /// The instructions that may not start before this one, and how long each has to wait.
    fn succs(&self, at: usize) -> &[(usize, u32)] {
        &self.edges[self.nodes[at].succs.clone()]
    }
}

/// Builds the dependence graph of one run into `scratch.graph`.
fn graph(func: &Func, run: &[Inst], known: &mut Known<'_>, scratch: &mut Scratch) {
    let Scratch { facts, found, wrote, read, readers, read_flags, seen, graph, .. } = scratch;
    facts.clear();
    facts.extend(run.iter().map(|&inst| known.of(func, inst)));
    let nodes = &mut graph.nodes;
    nodes.clear();
    nodes.extend(facts.iter().map(|facts| {
        let timing = facts.timing.expect("a barrier otherwise");
        Node { timing, succs: 0..0, preds: 0, height: 0, growth: 0 }
    }));
    // Every edge as it is found, as where it is from, where it goes and the wait. They are found
    // one instruction at a time, as the edges into it, so `since` is where the ones into the
    // instruction being looked at start.
    found.clear();

    // The last instruction to write each register, and every instruction to read one since. The
    // condition state is the same two questions with nowhere to keep the register's number, since
    // it is not an operand on a machine that has one.
    wrote.clear();
    // The readers of a register are a chain through `readers`, newest first, from where `read`
    // says it starts.
    read.clear();
    readers.clear();
    let mut wrote_flags: Option<usize> = None;
    read_flags.clear();
    let mut touched: Option<usize> = None;

    for (at, &inst) in run.iter().enumerate() {
        let facts = facts[at];
        let since = found.len();
        let mut edge = |from: usize, wait: u32| join(found, since, from, at, wait);

        // Reads before writes, because an instruction whose destination is one of its own sources
        // is on both lists and the write it does is not one its own read has to wait for.
        for operand in &func[func[inst].operands] {
            if operand.role == Role::Use {
                let place = (operand.reg, operand.class);
                if let Some(before) = wrote.get(&place) {
                    edge(*before, nodes[*before].timing.latency);
                }
                let next = read.insert(place, readers.len());
                readers.push((at, next));
            }
        }
        if facts.reads_flags {
            if let Some(before) = wrote_flags {
                edge(before, nodes[before].timing.latency);
            }
            read_flags.push(at);
        }
        for operand in &func[func[inst].operands] {
            if operand.role.is_def() {
                let place = (operand.reg, operand.class);
                if let Some(before) = wrote.insert(place, at) {
                    edge(before, after(nodes, before));
                }
                let mut next = read.remove(&place);
                while let Some(link) = next {
                    let (before, older) = readers[link];
                    next = older;
                    if before != at {
                        edge(before, 0);
                    }
                }
            }
        }
        if facts.writes_flags {
            if let Some(before) = wrote_flags.replace(at) {
                edge(before, after(nodes, before));
            }
            for before in read_flags.drain(..) {
                if before != at {
                    edge(before, 0);
                }
            }
        }

        // Memory, addresses and the stack pointer, which are one chain. See the module comment.
        let moves_stack = func[func[inst].operands]
            .iter()
            .any(|operand| operand.role.is_def() && (operand.reg, operand.class) == known.stack);
        if facts.touches_mem || func[inst].mem.is_some() || moves_stack {
            if let Some(before) = touched.replace(at) {
                edge(before, 0);
            }
        }
    }

    // Each instruction's edges side by side, in the order they were found, which is the order of
    // where they go. Counted first and then put in place, rather than sorted.
    for &(from, to, _) in found.iter() {
        nodes[from].succs.end += 1;
        nodes[to].preds += 1;
    }
    let mut start = 0;
    for node in nodes.iter_mut() {
        let count = node.succs.len();
        node.succs = start..start;
        start += count;
    }
    let edges = &mut graph.edges;
    edges.clear();
    edges.resize(found.len(), (0, 0));
    for &(from, to, wait) in found.iter() {
        edges[nodes[from].succs.end] = (to, wait);
        nodes[from].succs.end += 1;
    }
    heights(graph);
    growth(func, run, &mut graph.nodes, seen);
}

/// Says that the second instruction may not start until that many cycles after the first.
///
/// One edge per pair, keeping the longest wait. Two instructions are often joined for several
/// reasons at once, and what the pair costs is the strongest of the reasons rather than the sum of
/// them: a multiply whose result the next instruction reads and whose condition state it also
/// overwrites is one edge of three cycles, not a three cycle edge and a one cycle edge. Keeping one
/// edge per pair is also what makes criterion seven count instructions rather than reasons.
///
/// `since` is where in `found` the edges into `to` start, which are the only ones the pair can
/// already be among.
fn join(found: &mut Vec<(usize, usize, u32)>, since: usize, from: usize, to: usize, wait: u32) {
    if let Some(edge) = found[since..].iter_mut().find(|edge| edge.0 == from) {
        edge.2 = edge.2.max(wait);
        return;
    }
    found.push((from, to, wait));
}

/// How long after one write of somewhere the next write of the same somewhere may start.
///
/// The two have to land in order, and an instruction that takes no time has landed by the time it
/// has started, so this is a cycle for real work and nothing for the instructions that encode to
/// nothing. The ones that encode to nothing are the reason it is worth asking: a machine function
/// opens with an instruction per argument saying which register the argument is already in, each of
/// them writes a register the real work then writes again, and charging a cycle for that held every
/// first use of an argument one cycle behind where it could have been.
fn after(nodes: &[Node], before: usize) -> u32 {
    nodes[before].timing.latency.min(1)
}

/// The longest path from each instruction to the end of the run.
///
/// One pass backwards, which is all it takes because every edge goes from an earlier instruction to
/// a later one: the graph is built by walking the run forwards and only ever putting an edge from
/// something already seen to the instruction being looked at.
fn heights(graph: &mut Graph) {
    for at in (0..graph.nodes.len()).rev() {
        let mut height = graph.nodes[at].timing.latency;
        for &(succ, wait) in graph.succs(at) {
            height = height.max(wait + graph.nodes[succ].height);
        }
        graph.nodes[at].height = height;
    }
}

/// How many more registers are live after each instruction than before it.
///
/// A register a run reads for the last time is one whose value is not wanted afterwards, so the
/// instruction that reads it gives a register back. One that writes a register takes one. The
/// difference is what criterion two compares, and what it is really asking is whether an
/// instruction is doing work or making something that will have to be kept until later.
fn growth(func: &Func, run: &[Inst], nodes: &mut [Node], seen: &mut Set<Place>) {
    seen.clear();
    for (at, &inst) in run.iter().enumerate().rev() {
        for operand in &func[func[inst].operands] {
            if operand.role == Role::Use && seen.insert((operand.reg, operand.class)) {
                nodes[at].growth -= 1;
            }
        }
        for operand in &func[func[inst].operands] {
            if operand.role.is_def() {
                nodes[at].growth += 1;
            }
        }
    }
}

/// The five numbers one instruction is chosen by, in the order they are compared.
///
/// Derived rather than written out, because the order the fields are in is the order section 38.1
/// puts the criteria in and keeping the two the same is the point. Every field is one where smaller
/// is better, so the one that sorts first is the one to schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Pick {
    /// Criterion one, negated: the longest path to the end of the run, longest first.
    path: i64,
    /// Criterion two: how many registers it leaves live that were not, fewest first.
    growth: i32,
    /// Criterion six: whether it reads what was just scheduled, and so has to wait for it.
    waits: bool,
    /// Criterion seven, negated: how many instructions depend on it, most first.
    users: i64,
    /// Criterion eight: where it was in the input, earliest first.
    at: usize,
}

/// Chooses an order for the graph in `scratch`, as positions into the run, into `scratch.out`.
fn list(scratch: &mut Scratch, timing: &TimingInsts, accurate: bool) {
    let Scratch { graph, preds, when, used, out, .. } = scratch;
    let nodes = &graph.nodes;
    preds.clear();
    preds.extend(nodes.iter().map(|node| node.preds));
    when.clear();
    when.resize(nodes.len(), 0);
    let mut ready: BTreeSet<usize> = (0..nodes.len()).filter(|&at| preds[at] == 0).collect();
    out.clear();
    let mut cycle = 0;
    used.clear();
    let mut issued = 0;
    let mut last: Option<usize> = None;

    while !ready.is_empty() {
        let mut best: Option<Pick> = None;
        for &at in ready.iter().take(READY) {
            if when[at] > cycle || (accurate && !fits(nodes[at].timing.unit, used, issued, timing))
            {
                continue;
            }
            let pick = Pick {
                path: -i64::from(nodes[at].height),
                growth: nodes[at].growth,
                waits: last.is_some_and(|last| graph.succs(last).iter().any(|&(to, _)| to == at)),
                users: -(graph.succs(at).len() as i64),
                at,
            };
            if best.is_none_or(|best| pick < best) {
                best = Some(pick);
            }
        }
        let Some(best) = best else {
            // Nothing can start this cycle, either because everything ready is still waiting on a
            // value or because the units it wants are full. Both are answered by the next cycle,
            // and jumping straight to the one something is ready in keeps a long latency from being
            // walked over one cycle at a time.
            let soonest = ready.iter().take(READY).map(|&at| when[at]).min().unwrap_or(cycle);
            cycle = soonest.max(cycle + 1);
            used.clear();
            issued = 0;
            continue;
        };
        let at = best.at;
        ready.remove(&at);
        out.push(at);
        last = Some(at);
        *used.entry(nodes[at].timing.unit).or_default() += 1;
        issued += 1;
        for &(succ, wait) in graph.succs(at) {
            when[succ] = when[succ].max(cycle + wait);
            preds[succ] -= 1;
            if preds[succ] == 0 {
                ready.insert(succ);
            }
        }
    }
}

/// Whether the machine has room this cycle for an instruction on that unit.
fn fits(unit: Unit, used: &Map<Unit, u32>, issued: u32, timing: &TimingInsts) -> bool {
    issued < timing.width.max(1) && used.get(&unit).copied().unwrap_or(0) < timing.slots(unit)
}

#[cfg(test)]
mod tests {
    use rucc_mir::{Constraint, Mem, Opcode, Operand};
    use rucc_target::x86_64::{
        self, FLAGS, GPR, MACHINE, R8, R9, R10, RAX, RBP, RCX, RDI, RDX, RSI, RSP, TIMING, XMM,
    };

    use super::*;

    /// A function with one block, and the names it was built with.
    fn empty() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> Opcode {
        Opcode::new(names.intern(&format!("{}{name}", MACHINE.prefix)))
    }

    /// A register the allocator has already handed out, which is all this pass ever sees.
    fn reg(which: PhysReg) -> Reg {
        Reg::physical(which)
    }

    /// Two address arithmetic writing one of its own sources, which is the shape this machine's
    /// arithmetic has by the time the allocator has been through it.
    fn alu(
        func: &mut Func,
        names: &mut Interner,
        block: Block,
        name: &str,
        into: PhysReg,
        from: PhysReg,
    ) {
        let opcode = op(names, name);
        func.build(block, opcode)
            .operand(Operand::write(reg(into), GPR).with(Constraint::Reuse(1)))
            .uses(reg(into), GPR)
            .uses(reg(from), GPR)
            .finish();
    }

    /// The same, on the vector registers.
    fn vector(
        func: &mut Func,
        names: &mut Interner,
        block: Block,
        name: &str,
        into: PhysReg,
        from: PhysReg,
    ) {
        let opcode = op(names, name);
        func.build(block, opcode)
            .operand(Operand::write(reg(into), XMM).with(Constraint::Reuse(1)))
            .uses(reg(into), XMM)
            .uses(reg(from), XMM)
            .finish();
    }

    /// A move of one register into another.
    fn mov(func: &mut Func, names: &mut Interner, block: Block, into: PhysReg, from: PhysReg) {
        let opcode = op(names, "mov_rr_64");
        func.build(block, opcode).def(reg(into), GPR).uses(reg(from), GPR).finish();
    }

    /// An eight byte read off that register.
    fn load(func: &mut Func, names: &mut Interner, block: Block, into: PhysReg, base: PhysReg) {
        let opcode = op(names, "mov_rm_64");
        func.build(block, opcode)
            .def(reg(into), GPR)
            .mem(Mem::at(Operand::read(reg(base), GPR)))
            .finish();
    }

    /// An instruction of that name with no operands at all, which is what a call, a fence and a
    /// return are on this machine.
    fn bare(func: &mut Func, names: &mut Interner, block: Block, name: &str) {
        let opcode = op(names, name);
        func.build(block, opcode).finish();
    }

    /// What every instruction in a block came to, as opcodes with the target's prefix taken off.
    fn shape(func: &Func, names: &Interner, block: Block) -> Vec<String> {
        func.insts(block)
            .map(|inst| TIMING.bare(names.resolve(func[inst].opcode.name())).to_owned())
            .collect()
    }

    /// The pass, with nothing pinned beyond the block's own last instruction.
    fn schedule(func: &mut Func, names: &Interner) -> Scheduled {
        insts(func, (RSP, GPR), &TIMING, &MACHINE, &FLAGS, names, false, &Set::default())
    }

    /// A chain of three where only one order computes the right answer.
    #[test]
    fn a_block_already_in_the_only_order_it_has_comes_out_unchanged() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RAX, RDX);
        alu(&mut func, &mut names, block, "add_rr_64", RAX, RCX);
        bare(&mut func, &mut names, block, "ret");

        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0, "there was nothing else it could have written");
        assert_eq!(shape(&func, &names, block), ["mov_rr_64", "add_rr_64", "ret"]);
    }

    /// The shape the whole pass is for: a multiply takes three cycles and the instruction that reads
    /// it has to wait for all three, so work that was behind both of them is put in the middle.
    #[test]
    fn work_that_depends_on_nothing_moves_into_a_multiplys_latency() {
        let (mut names, mut func, block) = empty();
        alu(&mut func, &mut names, block, "imul_rr_64", RDI, RSI);
        alu(&mut func, &mut names, block, "add_rr_64", RDI, RCX);
        mov(&mut func, &mut names, block, RAX, RDX);
        bare(&mut func, &mut names, block, "ret");

        let done = schedule(&mut func, &names);
        assert_eq!(done.runs, 1, "one run, since nothing in it is a barrier");
        assert_eq!(
            shape(&func, &names, block),
            ["imul_rr_64", "mov_rr_64", "add_rr_64", "ret"],
            "the move is doing a cycle of the three the addition was going to spend waiting"
        );
    }

    /// A call, which is the barrier the module comment puts first. Without it the multiply below
    /// would be hoisted over the call, since it has the longer path and nothing in its operands says
    /// a call is in the way.
    #[test]
    fn nothing_crosses_a_call() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RAX, RDX);
        bare(&mut func, &mut names, block, "call");
        alu(&mut func, &mut names, block, "imul_rr_64", RDI, RSI);
        alu(&mut func, &mut names, block, "add_rr_64", RDI, RCX);
        bare(&mut func, &mut names, block, "ret");

        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(
            shape(&func, &names, block),
            ["mov_rr_64", "call", "imul_rr_64", "add_rr_64", "ret"]
        );
    }

    /// The epilogue of a frame that saved nothing, behind a store the multiply keeps waiting. The
    /// move of the frame pointer into the stack pointer depends on nothing, so without the chain it
    /// fills the multiply's latency and gives the frame back before the store into it has run.
    #[test]
    fn the_stack_pointer_is_not_given_back_before_a_store_into_the_frame() {
        let (mut names, mut func, block) = empty();
        alu(&mut func, &mut names, block, "imul_rr_64", RAX, RDX);
        let store = op(&mut names, "mov_mr_64");
        func.build(block, store)
            .uses(reg(RAX), GPR)
            .mem(Mem::at(Operand::read(reg(RCX), GPR)))
            .finish();
        mov(&mut func, &mut names, block, RSP, RBP);
        bare(&mut func, &mut names, block, "ret");

        schedule(&mut func, &names);
        assert_eq!(
            shape(&func, &names, block),
            ["imul_rr_64", "mov_mr_64", "mov_rr_64", "ret"],
            "the frame went back while the store into it was still waiting"
        );
    }

    /// Two reads of memory. The second one starts a chain with a longer path than the first, so the
    /// only thing keeping them in order is that they both touch memory.
    #[test]
    fn two_reads_of_memory_keep_the_order_they_arrived_in() {
        let (mut names, mut func, block) = empty();
        load(&mut func, &mut names, block, RAX, RDI);
        load(&mut func, &mut names, block, RCX, RSI);
        alu(&mut func, &mut names, block, "imul_rr_64", RCX, RDX);
        bare(&mut func, &mut names, block, "ret");

        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(
            shape(&func, &names, block),
            ["mov_rm_64", "mov_rm_64", "imul_rr_64", "ret"],
            "the read whose value nothing here wants stayed in front of the one that matters"
        );
    }

    /// Two writes of one register, where the first one's value is never read. What decides the
    /// register's contents afterwards is which of them ran last.
    #[test]
    fn two_writes_of_one_register_keep_the_order_they_arrived_in() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RAX, RDX);
        mov(&mut func, &mut names, block, RAX, RCX);
        alu(&mut func, &mut names, block, "imul_rr_64", RAX, RSI);
        bare(&mut func, &mut names, block, "ret");

        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(shape(&func, &names, block), ["mov_rr_64", "mov_rr_64", "imul_rr_64", "ret"]);
    }

    /// A write of a register something in front of it reads. No value passes between the two, and
    /// the order between them is still the difference between right and wrong.
    #[test]
    fn a_write_stays_behind_the_read_of_what_the_register_held() {
        let (mut names, mut func, block) = empty();
        alu(&mut func, &mut names, block, "add_rr_64", RCX, RAX);
        mov(&mut func, &mut names, block, RAX, RDX);
        alu(&mut func, &mut names, block, "imul_rr_64", RAX, RSI);
        bare(&mut func, &mut names, block, "ret");

        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(
            shape(&func, &names, block),
            ["add_rr_64", "mov_rr_64", "imul_rr_64", "ret"],
            "the addition read what was in the register before the move put something else there"
        );
    }

    /// The condition state, which is in no operand vector. The instruction that reads it has the
    /// longer path of the two ready at the start, so if the target's answer about the flags were not
    /// being used it would be scheduled first.
    #[test]
    fn the_instruction_that_reads_the_condition_state_stays_behind_the_comparison() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RCX, RDX);
        let cmp = op(&mut names, "cmp_rr_64");
        func.build(block, cmp).uses(reg(RDI), GPR).uses(reg(RSI), GPR).finish();
        let set = op(&mut names, "set_e");
        func.build(block, set).def(reg(RAX), GPR).finish();
        alu(&mut func, &mut names, block, "add_rr_64", RAX, R8);
        bare(&mut func, &mut names, block, "ret");

        schedule(&mut func, &names);
        assert_eq!(
            shape(&func, &names, block),
            ["cmp_rr_64", "mov_rr_64", "set_e", "add_rr_64", "ret"],
            "the move went into the cycle the set was waiting for the comparison in"
        );
    }

    /// The block's own last instruction, which [`crate::layout`] needs where it is.
    #[test]
    fn the_last_instruction_of_a_block_never_moves() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RAX, RDX);
        mov(&mut func, &mut names, block, RCX, R8);
        alu(&mut func, &mut names, block, "imul_rr_64", RSI, R9);

        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(
            shape(&func, &names, block),
            ["mov_rr_64", "mov_rr_64", "imul_rr_64"],
            "the multiply has the longest path and is last anyway"
        );
    }

    /// What the caller pins, which is the comparison the layout is going to fuse with a branch.
    #[test]
    fn an_instruction_the_caller_pinned_never_moves() {
        let build = |names: &mut Interner| {
            let mut func = Func::new(names.intern("f"));
            let block = func.create_block();
            mov(&mut func, names, block, RAX, RDX);
            mov(&mut func, names, block, RCX, R8);
            alu(&mut func, names, block, "imul_rr_64", RSI, R9);
            bare(&mut func, names, block, "ret");
            (func, block)
        };

        let mut names = Interner::new();
        let (mut loose, block) = build(&mut names);
        schedule(&mut loose, &names);
        assert_eq!(
            shape(&loose, &names, block),
            ["imul_rr_64", "mov_rr_64", "mov_rr_64", "ret"],
            "with nothing pinned the multiply goes first, since it has the longest path"
        );

        let (mut held, block) = build(&mut names);
        let second = held.insts(block).nth(1).expect("the second move");
        insts(
            &mut held,
            (RSP, GPR),
            &TIMING,
            &MACHINE,
            &FLAGS,
            &names,
            false,
            &[second].into_iter().collect::<Set<_>>(),
        );
        assert_eq!(
            shape(&held, &names, block),
            ["mov_rr_64", "mov_rr_64", "imul_rr_64", "ret"],
            "pinning it splits the block into runs of one, and a run of one has one order"
        );
    }

    /// A name the target does not have, which is the barrier that keeps a rule set growing an opcode
    /// from quietly growing a wrong schedule.
    #[test]
    fn a_name_this_target_does_not_have_is_a_barrier() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RAX, RDX);
        bare(&mut func, &mut names, block, "not_an_instruction_this_machine_has");
        alu(&mut func, &mut names, block, "imul_rr_64", RSI, R9);
        bare(&mut func, &mut names, block, "ret");

        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(
            shape(&func, &names, block),
            ["mov_rr_64", "not_an_instruction_this_machine_has", "imul_rr_64", "ret"]
        );
    }

    /// A trap, which the target has and the timing model deliberately does not describe.
    #[test]
    fn an_instruction_the_model_does_not_describe_is_a_barrier() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RAX, RDX);
        bare(&mut func, &mut names, block, "ud2");
        alu(&mut func, &mut names, block, "imul_rr_64", RSI, R9);
        bare(&mut func, &mut names, block, "ret");

        assert_eq!(TIMING.of("x64.ud2").expect("described").unit, Unit::Fixed);
        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(shape(&func, &names, block), ["mov_rr_64", "ud2", "imul_rr_64", "ret"]);
    }

    /// A return in the middle of a block, which only an `asm` template writes. It reads `rax`
    /// without naming it, so the `mov` in front of it has nothing tying it there but this.
    #[test]
    fn a_return_an_asm_template_wrote_is_a_barrier() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RAX, RDX);
        bare(&mut func, &mut names, block, "ret");
        alu(&mut func, &mut names, block, "imul_rr_64", RSI, R9);
        bare(&mut func, &mut names, block, "ud2");

        assert_eq!(TIMING.of("x64.ret").expect("described").unit, Unit::Branch);
        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(shape(&func, &names, block), ["mov_rr_64", "ret", "imul_rr_64", "ud2"]);
    }

    /// The property that holds whatever the model says, since the model chooses among orders and
    /// does not choose what is in one.
    #[test]
    fn what_comes_out_is_the_instructions_that_went_in_and_no_others() {
        let (mut names, mut func, block) = empty();
        alu(&mut func, &mut names, block, "imul_rr_64", RDI, RSI);
        mov(&mut func, &mut names, block, RAX, RDX);
        load(&mut func, &mut names, block, RCX, R8);
        alu(&mut func, &mut names, block, "add_rr_64", RAX, RCX);
        alu(&mut func, &mut names, block, "sub_rr_64", RDX, R9);
        mov(&mut func, &mut names, block, R10, RDI);
        alu(&mut func, &mut names, block, "imul_rr_64", R10, RAX);
        alu(&mut func, &mut names, block, "add_rr_64", R10, RDX);
        bare(&mut func, &mut names, block, "ret");
        let mut was: Vec<Inst> = func.insts(block).collect();

        schedule(&mut func, &names);
        let mut now: Vec<Inst> = func.insts(block).collect();
        assert_eq!(now.len(), was.len(), "nothing was added or dropped");
        was.sort_unstable();
        now.sort_unstable();
        assert_eq!(now, was, "the same instructions, in some order");
    }

    /// The output is a function of the input. Two hash maps in one process do not agree about the
    /// order they hand their contents back in, so anything in here that walked one would show up
    /// here rather than as a program that comes out differently on somebody else's machine.
    #[test]
    fn the_same_block_twice_gives_the_same_order_twice() {
        let build = |names: &mut Interner| {
            let mut func = Func::new(names.intern("f"));
            let block = func.create_block();
            alu(&mut func, names, block, "imul_rr_64", RDI, RSI);
            mov(&mut func, names, block, RAX, RDX);
            load(&mut func, names, block, RCX, R8);
            alu(&mut func, names, block, "add_rr_64", RAX, RCX);
            alu(&mut func, names, block, "sub_rr_64", RDX, R9);
            mov(&mut func, names, block, R10, RDI);
            alu(&mut func, names, block, "imul_rr_64", R10, RAX);
            bare(&mut func, names, block, "ret");
            (func, block)
        };

        let mut names = Interner::new();
        let (mut first, one) = build(&mut names);
        let (mut second, two) = build(&mut names);
        schedule(&mut first, &names);
        schedule(&mut second, &names);
        assert_eq!(shape(&first, &names, one), shape(&second, &names, two));
    }

    /// Criterion two. Both of these are ready at the start and both are the same distance from the
    /// end, and the one that hands a register back goes first.
    #[test]
    fn a_constant_put_in_a_register_is_not_hoisted_over_work_that_hands_one_back() {
        let (mut names, mut func, block) = empty();
        let load_imm = op(&mut names, "mov_ri_64");
        func.build(block, load_imm).def(reg(RCX), GPR).imm(5).finish();
        alu(&mut func, &mut names, block, "add_rr_64", RAX, RDX);
        alu(&mut func, &mut names, block, "add_rr_64", RAX, RCX);
        bare(&mut func, &mut names, block, "ret");

        schedule(&mut func, &names);
        assert_eq!(
            shape(&func, &names, block),
            ["add_rr_64", "mov_ri_64", "add_rr_64", "ret"],
            "the constant is loaded as early as necessary and no earlier"
        );
    }

    /// What [`TimingInsts::accurate`] is for. Three vector additions want the two floating point
    /// units, and a model worth believing about its units holds the third back and fills the cycle
    /// with the move instead.
    #[test]
    fn a_model_worth_believing_about_its_units_fills_a_full_cycle_with_other_work() {
        let build = |names: &mut Interner| {
            let mut func = Func::new(names.intern("f"));
            let block = func.create_block();
            vector(&mut func, names, block, "addsd_rr", x86_64::xmm(0), x86_64::xmm(1));
            vector(&mut func, names, block, "addsd_rr", x86_64::xmm(2), x86_64::xmm(3));
            vector(&mut func, names, block, "addsd_rr", x86_64::xmm(4), x86_64::xmm(5));
            mov(&mut func, names, block, RAX, RDX);
            bare(&mut func, names, block, "ret");
            (func, block)
        };

        assert_eq!(TIMING.slots(Unit::Float), 2, "the machine this model describes has two");

        let mut names = Interner::new();
        let (mut loose, block) = build(&mut names);
        insts(&mut loose, (RSP, GPR), &TIMING, &MACHINE, &FLAGS, &names, false, &Set::default());
        assert_eq!(
            shape(&loose, &names, block),
            ["addsd_rr", "addsd_rr", "addsd_rr", "mov_rr_64", "ret"],
            "without the units the three additions are the same instruction three times over"
        );

        let (mut tight, block) = build(&mut names);
        insts(&mut tight, (RSP, GPR), &TIMING, &MACHINE, &FLAGS, &names, true, &Set::default());
        assert_eq!(
            shape(&tight, &names, block),
            ["addsd_rr", "addsd_rr", "mov_rr_64", "addsd_rr", "ret"],
            "the third addition has nowhere to go this cycle and the move has"
        );
    }

    /// The bound, and the same block below it as the control. A run of a few thousand machine
    /// instructions is a generated table rather than something anybody is waiting on the schedule
    /// of, and building the graph for one is quadratic in the worst case.
    ///
    /// The moves all write the same register, so they are a chain that has to run in the order it
    /// is in and the first of them is further from the end of the run than a three cycle multiply
    /// is. Below the bound that is what decides the order. Above it nothing decides anything.
    #[test]
    fn a_run_longer_than_the_bound_is_left_alone() {
        let build = |names: &mut Interner, moves: usize| {
            let mut func = Func::new(names.intern("f"));
            let block = func.create_block();
            alu(&mut func, names, block, "imul_rr_64", RSI, R9);
            for _ in 0..moves {
                mov(&mut func, names, block, RAX, RDX);
            }
            bare(&mut func, names, block, "ret");
            (func, block)
        };

        let mut names = Interner::new();
        let (mut short, block) = build(&mut names, 8);
        let done = schedule(&mut short, &names);
        assert!(done.moved > 0, "below the bound a run is looked at");
        assert_eq!(
            shape(&short, &names, block).first().map(String::as_str),
            Some("mov_rr_64"),
            "the chain of moves is the long way round and starts first"
        );

        let (mut long, block) = build(&mut names, LONGEST);
        let done = schedule(&mut long, &names);
        assert_eq!(done.moved, 0, "above it the run is written back exactly as it arrived");
        assert_eq!(shape(&long, &names, block).first().map(String::as_str), Some("imul_rr_64"));
    }

    /// A block too short to have anything to choose, which is the one case the pass skips outright.
    #[test]
    fn a_block_of_two_instructions_is_not_looked_at() {
        let (mut names, mut func, block) = empty();
        alu(&mut func, &mut names, block, "imul_rr_64", RSI, R9);
        bare(&mut func, &mut names, block, "ret");

        let done = schedule(&mut func, &names);
        assert_eq!(done, Scheduled::default());
        assert_eq!(shape(&func, &names, block), ["imul_rr_64", "ret"]);
    }

    /// A shift by a variable amount, which the machine takes out of one particular register and
    /// this target's description names as an operand with that register fixed. The whole of this
    /// pass reads operand vectors, so an instruction whose description left a register it touches
    /// out of one would be reordered around a write of it. This is the check that it does not.
    #[test]
    fn a_shift_by_a_variable_amount_stays_behind_the_write_of_the_register_it_counts() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RCX, R8);
        let shift = op(&mut names, "shl_rcl_64");
        func.build(block, shift)
            .operand(Operand::write(reg(RAX), GPR).with(Constraint::Reuse(1)))
            .uses(reg(RAX), GPR)
            .uses(reg(RCX), GPR)
            .finish();
        alu(&mut func, &mut names, block, "imul_rr_64", RAX, RDX);
        bare(&mut func, &mut names, block, "ret");

        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(shape(&func, &names, block), ["mov_rr_64", "shl_rcl_64", "imul_rr_64", "ret"]);
    }

    /// A divide, which reads and writes two particular registers and names all four of them. It is
    /// twenty six cycles from the end of this run and the move in front of it is one, so the only
    /// thing keeping it where it is is that it said it writes the register the move writes.
    #[test]
    fn a_divide_names_both_of_the_registers_the_machine_makes_it_use() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RDX, R8);
        let divide = op(&mut names, "idiv_quo_64");
        func.build(block, divide)
            .operand(Operand::write(reg(RAX), GPR).with(Constraint::Fixed(RAX)))
            .operand(Operand::write_early(reg(RDX), GPR).with(Constraint::Fixed(RDX)))
            .operand(Operand::read(reg(RAX), GPR).with(Constraint::Fixed(RAX)))
            .uses(reg(RSI), GPR)
            .finish();
        bare(&mut func, &mut names, block, "ret");

        assert!(TIMING.of("x64.idiv_quo_64").expect("described").latency > 1);
        let done = schedule(&mut func, &names);
        assert_eq!(done.moved, 0);
        assert_eq!(shape(&func, &names, block), ["mov_rr_64", "idiv_quo_64", "ret"]);
    }

    /// Every unit the model has, reached through an instruction that is on it, since a unit nothing
    /// can get a slot on is a scheduler that does not finish.
    #[test]
    fn every_unit_a_run_can_ask_for_has_at_least_one_of_it() {
        for &unit in Unit::ALL {
            assert!(TIMING.slots(unit) >= 1, "{unit:?} has none of it");
        }
    }

    /// Two files that each number their registers from zero. The move and the addition here both
    /// write the register numbered nothing, and they are not writing the same register: one is the
    /// first integer register and the other is the first vector register. See [`Place`].
    #[test]
    fn the_first_register_of_each_file_is_not_the_same_register() {
        let (mut names, mut func, block) = empty();
        mov(&mut func, &mut names, block, RAX, RDX);
        vector(&mut func, &mut names, block, "addsd_rr", x86_64::xmm(0), x86_64::xmm(1));
        bare(&mut func, &mut names, block, "ret");

        assert_eq!(reg(RAX), reg(x86_64::xmm(0)), "and a register on its own does not say which");
        schedule(&mut func, &names);
        assert_eq!(
            shape(&func, &names, block),
            ["addsd_rr", "mov_rr_64", "ret"],
            "the addition is four cycles from the end and the move is one, and nothing joins them"
        );
    }
}
