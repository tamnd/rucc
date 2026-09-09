//! Chooses which induction variables a loop should keep, and rewrites nothing.
//!
//! Design: `spec/optimizer/28-induction-variables.md`.
//!
//! Section 28.2 is the reason this is a pass rather than a peephole. The naive version replaces
//! each multiply in an address as it finds it, and section 28.2 says plainly that it "produces
//! worse code than doing nothing on real loops". What is actually being asked is a set cover:
//! choose a set of variables to increment each iteration, minimising the total of what the set
//! costs to maintain and what every use in the loop costs to express in terms of it. Which set
//! wins depends on the target's addressing modes, and section 28.2 says there is no
//! target-independent right answer.
//!
//! # Why this half is on its own
//!
//! Section 28.7 lists five ways this goes wrong, and four of them are ways a rewrite is wrong: an
//! exit test that is not equivalent, a derived limit that overflows, a signedness that changed, a
//! set that spills. None of them can happen here, because nothing here writes to the function.
//! What can be wrong here is the answer, and an answer is a thing to look at before acting on.
//!
//! So this is the choosing, and the numbers it produces are checkable against #701's survey of
//! what GCC 16 chooses over the same corpus. The rewriting is the job after it, and it consumes
//! what this decides.
//!
//! # It is not in any pipeline
//!
//! A pass that changes nothing does not belong in a build, so no optimization level names it.
//! What reaches it is `-fenable-ivopts` together with `-fopt-info-all`, which is the same way
//! `crate::nests` is reached and for the same reason.
//!
//! # What a use is
//!
//! Section 28.1 lists three kinds, and the classification here is one rule rather than three.
//! An operand is a use when it moves by a fixed step around the loop and the instruction reading
//! it does not itself move by a fixed step. That second half is what keeps an induction variable's
//! own increment out of the list: `i + 1` moves by the same step `i` does, so it is the variable
//! rather than a use of it. It also keeps the arithmetic inside an address out, because `a + i*4`
//! moves by four, so its operands are not uses and the address itself is.
//!
//! Which of the three kinds it is comes from the reader. An address of a load or a store is an
//! address use, an operand of a comparison is a comparison use, and everything else is the
//! remainder section 28.1 calls a non-linear expression.
//!
//! # What grouping is worth
//!
//! Section 28.3 calls it "the cheapest large win in the pass", and section 28.1 quotes GCC's rule:
//! address uses group when "their iv bases are different in constant offset". `a[i]`, `a[i+1]` and
//! `a[i+2]` are three uses of one variable and cost one increment between them, and a pass that
//! misses that pays for three. Only address uses group, because a constant offset is free inside
//! an addressing mode and costs an add anywhere else.

use rucc_cost::{AddrMode, Cost, CostTable, Cycles, RegClass, Width, heuristics};
use rucc_ir::{Flags, Func, Inst, Opcode, Value};

use crate::loops::{LoopId, Loops};
use crate::machine::Machine;
use crate::scev::{Chrec, Count, Evolution, Invariant, Scev};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

const NO_TARGET: &str =
    "left alone, nobody has priced this machine and the answer is a fact about the machine";
const POPULATION: &str = "loop with at least one induction variable use in it";
const USE_ADDRESS: &str = "address of a read or a write that moves by a fixed step";
const USE_COMPARE: &str = "comparison against something that moves by a fixed step";
const USE_GENERIC: &str = "other use of something that moves by a fixed step";
const GROUPED: &str = "address uses sharing one variable, apart in a constant offset";
const CANDIDATE: &str = "induction variable considered for the loop to keep";
const CHOSEN: &str = "induction variable chosen for the loop to keep";
const KEPT: &str = "loop whose own induction variables are the ones worth keeping";
const CHANGED: &str = "loop that would be cheaper with a different set of induction variables";
const TOO_MANY_USES: &str = "left alone, more uses in it than the search is allowed to look at";
const PRUNED: &str =
    "candidate dropped before the search, no use wants it and it is not the loop's";

/// The selection section 28.3 asks for.
#[derive(Debug)]
pub struct Ivopts;

impl Pass for Ivopts {
    fn name(&self) -> &'static str {
        "ivopts"
    }

    fn describe(&self) -> &'static str {
        "chooses the induction variables a loop should keep, and rewrites nothing"
    }

    fn preserves(&self) -> Preserved {
        // It writes nothing, so everything worked out about the function is still true.
        Preserved::ALL
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, _fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let machine = an.machine();
        let Some(table) = machine.table() else {
            // Section 28.2: which set wins is a fact about the target, so a compiler with no
            // prices for this one has no opinion worth acting on. Saying so is the whole of the
            // response, per `crate::machine`.
            stats.missed(NO_TARGET);
            return stats;
        };
        let cfg = an.cfg(func).clone();
        let loops = an.loops(func).clone();
        let mut scev = Scev::new(func, &cfg, &loops);
        for id in loops.all() {
            consider(func, &loops, &mut scev, machine, table, id, &mut stats);
        }
        stats
    }
}

/// Which of section 28.1's three kinds a use is.
///
/// The order matters to the grouping: only [`Kind::Address`] groups, so the kind is part of what
/// two uses have to agree on before they can share a variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// The address of a load or a store.
    Address,
    /// One side of a comparison.
    Compare,
    /// Section 28.1's non-linear expressions, which is everything else.
    Generic,
}

impl Kind {
    /// The remark this kind is counted under.
    const fn remark(self) -> &'static str {
        match self {
            Self::Address => USE_ADDRESS,
            Self::Compare => USE_COMPARE,
            Self::Generic => USE_GENERIC,
        }
    }
}

/// One place in the loop that wants the value of something moving by a fixed step.
#[derive(Clone, Copy, Debug)]
struct Want {
    /// Which kind it is, which decides what expressing it costs.
    kind: Kind,
    /// How the value it wants moves around this loop.
    chrec: Chrec,
}

/// Uses one variable can serve between them, apart only in a constant offset.
#[derive(Debug)]
struct Group {
    /// What they have in common, taken from the first of them.
    kind: Kind,
    /// The form the group is named by, which is the first use's.
    chrec: Chrec,
    /// How far past the group's own base each use sits.
    offsets: Vec<i128>,
}

impl Group {
    /// Whether a use belongs in this group.
    ///
    /// Section 28.1's rule, and the two halves of it are separate. The kind has to match because
    /// only an address gets a constant offset for free. The bases have to be the same expression
    /// apart from the number added to them, which is what `value` and `scale` agreeing and
    /// `offset` not agreeing means.
    fn takes(&self, want: &Want) -> bool {
        self.kind == Kind::Address
            && want.kind == Kind::Address
            && self.chrec.ty == want.chrec.ty
            && self.chrec.step == want.chrec.step
            && self.chrec.base.value == want.chrec.base.value
            && self.chrec.base.scale == want.chrec.base.scale
    }
}

/// Where a candidate came from, which is what section 28.1's bias towards the original is written
/// against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    /// A variable the loop already increments.
    Original,
    /// One made up to serve a group of uses.
    Derived,
    /// Section 28.4's countdown, which exists to make the exit test a comparison against zero.
    Countdown,
}

/// A variable the loop could keep.
#[derive(Clone, Copy, Debug)]
struct Cand {
    /// How it moves around the loop.
    chrec: Chrec,
    /// Where it came from.
    origin: Origin,
}

impl Cand {
    /// Whether two candidates are the same variable, which is a question about the sequence and
    /// not about where the sequence was found.
    fn same(&self, other: &Self) -> bool {
        self.chrec.ty == other.chrec.ty
            && self.chrec.base == other.chrec.base
            && self.chrec.step == other.chrec.step
    }
}

/// Everything about one loop, from collecting its uses to naming the set it should keep.
fn consider(
    func: &Func,
    loops: &Loops,
    scev: &mut Scev<'_>,
    machine: Machine,
    table: &CostTable,
    id: LoopId,
    stats: &mut Stats,
) {
    let wants = collect(func, loops, scev, id);
    if wants.is_empty() {
        return;
    }
    stats.note(POPULATION);
    for want in &wants {
        stats.note(want.kind.remark());
    }
    if wants.len() > heuristics::IV_MAX_CONSIDERED_USES {
        // Section 28.8: the search is cubic in the uses, and this is the bound that stops it
        // being cubic in a number the program chose.
        stats.missed(TOO_MANY_USES);
        return;
    }

    let groups = group(wants);
    for one in &groups {
        if one.offsets.len() > 1 {
            stats.note(GROUPED);
        }
    }

    let mut cands = candidates(func, loops, scev, id, &groups);
    prune(&mut cands, table, &groups, stats);
    for _ in &cands {
        stats.note(CANDIDATE);
    }

    let room = machine.allocatable(RegClass::Integer).unwrap_or(0);
    let chosen = select(table, &groups, &cands, room);
    for _ in &chosen {
        stats.note(CHOSEN);
    }
    let untouched = chosen.iter().all(|&at| cands[at].origin == Origin::Original)
        && chosen.len() == cands.iter().filter(|c| c.origin == Origin::Original).count();
    stats.note(if untouched { KEPT } else { CHANGED });
}

/// Every use in the loop, per the module documentation's one rule.
fn collect(func: &Func, loops: &Loops, scev: &mut Scev<'_>, id: LoopId) -> Vec<Want> {
    let mut wants = Vec::new();
    for &block in loops.blocks(id) {
        // A block of a loop inside this one belongs to that loop's own question. Section 28.1
        // says all of this is done loop by loop.
        if loops.innermost(block) != Some(id) {
            continue;
        }
        for inst in func.insts(block) {
            let data = func[inst];
            // A terminator's operands are the block arguments that carry the variable from one
            // turn of the loop to the next, and those are the plumbing rather than a use of it:
            // a rewrite replaces them wholesale and never pays to express one. The condition is
            // the other thing a terminator holds, and it came out of a compare that was counted
            // where it stood. What this does miss is a `switch` on an induction variable, which
            // no C loop this pass has been shown writes and which would be a use if one did.
            if data.opcode.is_terminator() {
                continue;
            }
            // The reader moving by a fixed step itself means this instruction is a variable
            // rather than a use of one, which is what keeps an increment and the arithmetic
            // inside an address off the list.
            if moves(func, scev, id, inst) {
                continue;
            }
            let args = &func[data.args];
            for (position, &arg) in args.iter().enumerate() {
                let Some(chrec) = affine(scev, id, arg) else { continue };
                let kind = classify(data.opcode, position);
                wants.push(Want { kind, chrec });
            }
        }
    }
    wants
}

/// Whether the value this instruction computes moves by a fixed step around the loop.
fn moves(func: &Func, scev: &mut Scev<'_>, id: LoopId, inst: Inst) -> bool {
    func[inst].results().any(|result| affine(scev, id, result).is_some())
}

/// The affine form of a value in this loop, when it has one that is worth serving.
///
/// A step of zero is refused. Section 28.7 wants the analysis to say affine or unknown and never
/// guess, and a value that does not move is not an induction variable at all: it is a loop
/// invariant, and moving it is `crate::licm`'s job rather than this one's.
fn affine(scev: &mut Scev<'_>, id: LoopId, value: Value) -> Option<Chrec> {
    match scev.evolution(id, value) {
        Evolution::Affine(chrec) if !chrec.step.is_zero() => Some(chrec),
        _ => None,
    }
}

/// Which of the three kinds a use in this position of this instruction is.
fn classify(opcode: Opcode, position: usize) -> Kind {
    match (opcode, position) {
        (Opcode::Load, 0) | (Opcode::Store, 1) => Kind::Address,
        (Opcode::ICmp, _) => Kind::Compare,
        _ => Kind::Generic,
    }
}

/// The uses gathered into the sets one variable can serve.
fn group(wants: Vec<Want>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for want in wants {
        match groups.iter_mut().find(|one| one.takes(&want)) {
            Some(one) => one.offsets.push(want.chrec.base.offset - one.chrec.base.offset),
            None => groups.push(Group { kind: want.kind, chrec: want.chrec, offsets: vec![0] }),
        }
    }
    groups
}

/// The variables the loop could keep, per section 28.3.
fn candidates(
    func: &Func,
    loops: &Loops,
    scev: &mut Scev<'_>,
    id: LoopId,
    groups: &[Group],
) -> Vec<Cand> {
    let mut cands: Vec<Cand> = Vec::new();
    let mut add = |cand: Cand| {
        if !cands.iter().any(|had| had.same(&cand)) {
            cands.push(cand);
        }
    };

    // The ones the loop already has. A loop's induction variables arrive at its header as block
    // parameters, because that is what `crate::canon` leaves, so this is the whole list.
    for at in 0..func[loops.header(id)].params.len() {
        let param = func[loops.header(id)].params[at];
        if let Some(chrec) = affine(scev, id, param) {
            add(Cand { chrec, origin: Origin::Original });
        }
    }

    // One per distinct form among the uses, which is what makes the set cover solvable: every
    // group has at least one candidate that serves it for nothing.
    for one in groups {
        add(Cand { chrec: one.chrec, origin: Origin::Derived });
    }

    // Section 28.4's countdown, worth a candidate only when the trip count is a number the
    // analysis will stand behind, because a countdown from a guess is a loop that runs the wrong
    // number of times. `under_undefined_overflow` rather than `proven` because C is the language
    // and `crate::scev` documents that `proven` answers nothing for any `for (int i = 0; i < n;
    // i++)` in it.
    if let Some(bound) = scev.bound(id) {
        if let Some(count) = bound.under_undefined_overflow() {
            if let Some(chrec) = countdown(count, groups) {
                add(Cand { chrec, origin: Origin::Countdown });
            }
        }
    }
    cands
}

/// The countdown variable for a loop of this many iterations, in the type the loop counts in.
///
/// It starts at the trip count and steps down by one, so the exit test becomes a comparison
/// against zero, which section 28.4 says every one of rucc's three targets gets cheaply. The type
/// comes from a group rather than from the count, because the count is a number and a number has
/// no width of its own.
fn countdown(count: Count, groups: &[Group]) -> Option<Chrec> {
    let Count::Exact(iterations) = count else { return None };
    let iterations = i128::try_from(iterations).ok()?;
    let ty = groups.iter().map(|one| one.chrec.ty).find(|ty| ty.is_int())?;
    Some(Chrec {
        base: Invariant::number(iterations),
        step: Invariant::number(-1),
        ty,
        flags: Flags::NONE,
    })
}

/// Drops the candidates no group would pick and the loop does not already have.
///
/// Section 28.2's third parameter, `iv-always-prune-cand-set-bound`. Below it the set is small
/// enough that carrying a useless candidate costs nothing, and above it every candidate is a
/// column in a search that is cubic in the count.
fn prune(cands: &mut Vec<Cand>, table: &CostTable, groups: &[Group], stats: &mut Stats) {
    if cands.len() <= heuristics::IV_ALWAYS_PRUNE_CAND_SET_BOUND {
        return;
    }
    let mut wanted = vec![false; cands.len()];
    for one in groups {
        let best = (0..cands.len())
            .filter(|&at| !serve(table, one, &cands[at]).is_infinite())
            .min_by_key(|&at| serve(table, one, &cands[at]));
        if let Some(at) = best {
            wanted[at] = true;
        }
    }
    let mut at = 0;
    cands.retain(|cand| {
        let keep = wanted[at] || cand.origin == Origin::Original;
        at += 1;
        if !keep {
            stats.note(PRUNED);
        }
        keep
    });
}

/// What one use of this group costs when it is expressed in terms of this candidate.
///
/// [`Cost::INFINITE`] means the candidate cannot express it at all, which is not the same as
/// expressing it expensively and is the answer whenever the two sequences are not related by a
/// number this pass can write down.
fn serve(table: &CostTable, group: &Group, cand: &Cand) -> Cost {
    if group.chrec.ty != cand.chrec.ty {
        // A conversion between the two is arithmetic on the sequence, and `crate::scev` refuses
        // to widen a chrec whose ends are symbolic, so the honest answer is that this candidate
        // does not serve this group rather than a price for a conversion nobody checked.
        return Cost::INFINITE;
    }
    let Some(scale) = ratio(cand.chrec.step, group.chrec.step) else { return Cost::INFINITE };
    // What is left over once the candidate has been scaled up to the group's step, which is the
    // part that has to be added on and is the same on every iteration.
    let scaled = match cand.chrec.base.times(Invariant::number(scale)) {
        Some(scaled) => scaled,
        None => return Cost::INFINITE,
    };
    let Some(rest) = group.chrec.base.minus(scaled) else { return Cost::INFINITE };
    match group.kind {
        Kind::Address => address_cost(table, scale, rest),
        Kind::Compare | Kind::Generic => value_cost(table, group.chrec.ty, scale, rest),
    }
}

/// How many of the candidate's steps make one of the group's, when it is a whole number of them.
fn ratio(step: Invariant, wanted: Invariant) -> Option<i128> {
    let (step, wanted) = (step.as_number()?, wanted.as_number()?);
    if step == 0 || wanted % step != 0 {
        return None;
    }
    Some(wanted / step)
}

/// What an address of this shape costs, which is what the target's addressing modes decide.
///
/// Section 28.2's point in one function: the same loop wants a different set of variables on a
/// machine with a scaled index than on one without, and this is where that difference enters.
fn address_cost(table: &CostTable, scale: i128, rest: Invariant) -> Cost {
    let indexed = scale != 1;
    let displaced = rest.offset != 0;
    let symbolic = rest.value.is_some() && rest.scale != 0;
    if indexed && !legal_scale(scale) {
        // A scale no addressing mode holds has to be multiplied out, and then the product is a
        // plain register the other modes can still use.
        let mult = table.mult_of(width(rest)).max(Cycles::ONE);
        return Cost::cycles(mult) + address_cost(table, 1, rest);
    }
    let mode = match (indexed, symbolic, displaced) {
        (false, false, false) => AddrMode::Base,
        (false, false, true) => AddrMode::BaseDisp,
        (false, true, false) => AddrMode::BaseIndex,
        (true, false, false) => AddrMode::BaseIndexScale,
        (true, false, true) => AddrMode::BaseIndexScaleDisp,
        // A symbolic part and an index at once is two registers and a scale, which is one more
        // register than any of the modes hold, so the symbolic part is added in first and what
        // remains is an address of the same shape without it. The displacement survives that add
        // rather than being dropped with the symbol, because it is a number rather than a
        // register and no addressing mode ran out of room for it.
        (_, true, _) => {
            let left = Invariant::number(rest.offset);
            return Cost::cycles(table.add) + address_cost(table, scale, left);
        }
    };
    let priced = table.addr_cost(mode);
    if priced.is_infinite() {
        // The target has no such mode, so the address is computed into a register and the load
        // reads a plain base. That is what an address computation not expressible as an
        // addressing mode costs, which is the second of the three things section 28.3 asks a
        // target for.
        let parts = i64::from(mode.complexity());
        return Cost::cycles(table.lea) * parts + table.addr_cost(AddrMode::Base);
    }
    priced
}

/// What a value of this shape costs when it is wanted in a register rather than in an address.
fn value_cost(table: &CostTable, ty: rucc_ir::Type, scale: i128, rest: Invariant) -> Cost {
    let mut cost = Cost::ZERO;
    if scale != 1 {
        let bits = ty.bits();
        let mult = match Width::from_bits(bits) {
            Some(width) if scale <= 0 || !scale.unsigned_abs().is_power_of_two() => {
                table.mult[width.index()]
            }
            Some(_) => table.shift_const,
            None => return Cost::INFINITE,
        };
        cost += Cost::cycles(mult);
    }
    if rest.offset != 0 || (rest.value.is_some() && rest.scale != 0) {
        cost += Cost::cycles(table.add);
    }
    cost
}

/// Whether an addressing mode can hold this scale, which every one of rucc's targets reads the
/// same way: one, two, four or eight.
fn legal_scale(scale: i128) -> bool {
    matches!(scale, 1 | 2 | 4 | 8)
}

/// The width the arithmetic on an address happens in, which is the width of a pointer.
fn width(_rest: Invariant) -> Width {
    Width::W64
}

/// What keeping this candidate costs per iteration, before anything uses it.
///
/// One increment, plus section 28.1's bias: "the original variables are somewhat preferred". A
/// variable the loop already has is one the rewrite does not have to introduce and one the
/// register allocator has already been living with, and section 28.7 asks for the bias to be a
/// real number rather than a tiebreak so that a target whose costs are untuned falls back on what
/// the program wrote.
fn upkeep(table: &CostTable, cand: &Cand) -> Cost {
    let step = Cost::cycles(table.add);
    match cand.origin {
        Origin::Original => step,
        Origin::Derived | Origin::Countdown => {
            step + Cost::cycles(Cycles::ONE * i64::from(heuristics::IVOPTS_NEW_VARIABLE_BIAS))
        }
    }
}

/// What a set costs: every group served by its cheapest member, every member maintained, and a
/// penalty once the set has more variables in it than the machine has registers to spare.
///
/// Nothing, when some group in it has no server. A set that cannot express a use is not an
/// expensive set, it is not an answer.
fn total(
    table: &CostTable,
    groups: &[Group],
    cands: &[Cand],
    set: &[usize],
    room: u32,
) -> Option<Cost> {
    let mut cost = Cost::ZERO;
    for group in groups {
        let best = set.iter().map(|&at| serve(table, group, &cands[at])).min()?;
        if best.is_infinite() {
            return None;
        }
        cost += best * i64::try_from(group.offsets.len()).unwrap_or(1);
    }
    for &at in set {
        cost += upkeep(table, &cands[at]);
    }
    let spare = room.saturating_sub(heuristics::LOOP_RESERVED_REGS);
    let over = u32::try_from(set.len()).unwrap_or(u32::MAX).saturating_sub(spare);
    cost += Cost::cycles(Cycles::ONE * i64::from(over) * i64::from(heuristics::IVOPTS_SET_PENALTY));
    Some(cost)
}

/// The greedy search section 28.3 describes.
///
/// It starts from the variables the loop already has, because that is the set the program wrote
/// and section 28.7 says it is the safer default whenever the cost model is wrong. Then it takes
/// whichever single change, an addition or a removal, most reduces the total, and stops when
/// nothing does. Not exhaustive even below `iv-consider-all-candidates-bound`, per section 28.3,
/// because what the exhaustive search is worth over the greedy one has never been published.
fn select(table: &CostTable, groups: &[Group], cands: &[Cand], room: u32) -> Vec<usize> {
    let mut set: Vec<usize> =
        (0..cands.len()).filter(|&at| cands[at].origin == Origin::Original).collect();

    // A set that cannot express every use is not a starting point, and the loop's own variables
    // will not always express every use: a walk of two arrays at once has one counter and two
    // addresses. Every group got a candidate made for it in `candidates`, so adding that one back
    // is always enough to make the starting set an answer.
    for group in groups {
        let served = set.iter().any(|&at| !serve(table, group, &cands[at]).is_infinite());
        if served {
            continue;
        }
        let own = (0..cands.len()).find(|&at| {
            cands[at].origin == Origin::Derived && cands[at].chrec.base == group.chrec.base
        });
        if let Some(at) = own {
            set.push(at);
        }
    }
    set.sort_unstable();
    set.dedup();

    let Some(mut best) = total(table, groups, cands, &set, room) else { return set };
    loop {
        let mut moved = None;
        for at in 0..cands.len() {
            let mut tried = set.clone();
            match tried.iter().position(|&had| had == at) {
                Some(there) => {
                    tried.remove(there);
                }
                None => tried.push(at),
            }
            let Some(cost) = total(table, groups, cands, &tried, room) else { continue };
            if cost < best {
                best = cost;
                moved = Some(tried);
            }
        }
        match moved {
            Some(tried) => set = tried,
            None => return set,
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Flags, Func, IntPred, MemInfo, MemOrder, Opcode, Restrict, Signature, Type,
        Value,
    };

    use super::{
        CANDIDATE, CHANGED, CHOSEN, GROUPED, Ivopts, KEPT, NO_TARGET, POPULATION, USE_ADDRESS,
        USE_COMPARE, USE_GENERIC,
    };
    use crate::stats::Kind;
    use crate::{Analyses, Fuel, Pass, Stats};

    /// Runs the choosing over the function as it stands, on a machine somebody priced.
    fn choose(func: &mut Func) -> Stats {
        Ivopts.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// An access that says as little about itself as one may.
    fn plain() -> MemInfo {
        MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        }
    }

    /// A counted loop with a body for the caller to fill and a block for it to leave to.
    struct Counted {
        head: Block,
        body: Block,
        out: Block,
        counter: Value,
    }

    /// Opens a loop, entered from `into`.
    ///
    /// ```text
    /// into:      jump head(0)
    /// head(i):   t = i < limit; br t -> body(i), out
    /// ```
    fn counted(func: &mut Func, into: Block, limit: i128) -> Counted {
        let head = func.create_block();
        let body = func.create_block();
        let out = func.create_block();
        let i = func.append_param(head, Type::int(64));
        let carried = func.append_param(body, Type::int(64));

        let mut build = Builder::new(func, into);
        let zero = build.iconst(Type::int(64), 0);
        build.jump(head, &[zero]);

        let mut build = Builder::new(func, head);
        let stop = build.iconst(Type::int(64), limit);
        let test = build.icmp(IntPred::Slt, i, stop);
        build.br_if(test, body, &[i], out, &[]);

        Counted { head, body, out, counter: carried }
    }

    /// Closes a loop, moving the counter along by one in `at`.
    fn close(func: &mut Func, it: &Counted, at: Block) {
        let mut build = Builder::new(func, at);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, it.counter, one, Flags::NSW);
        build.jump(it.head, &[next]);
    }

    /// A function taking one pointer, with an entry block for a loop to go in.
    fn shell(names: &mut Interner) -> (Func, Block, Value) {
        let signature = Signature::new().with_params(&[Type::PTR]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let base = func.append_param(entry, Type::PTR);
        (func, entry, base)
    }

    /// The address of `base[i + away]`, in the block being built.
    fn element(build: &mut Builder<'_>, base: Value, counter: Value, away: i128) -> Value {
        let four = build.iconst(Type::int(64), 4);
        let by = build.binary(Opcode::Mul, counter, four, Flags::NSW);
        let at = build.binary(Opcode::PtrAdd, base, by, Flags::NONE);
        if away == 0 {
            return at;
        }
        let past = build.iconst(Type::int(64), away * 4);
        build.binary(Opcode::PtrAdd, at, past, Flags::NONE)
    }

    #[test]
    fn a_loop_walking_one_array_has_one_address_use_and_one_comparison() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let addr = element(&mut build, base, it.counter, 0);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Note, POPULATION), 1);
        assert_eq!(stats.count(Kind::Note, USE_ADDRESS), 1);
        assert_eq!(
            stats.count(Kind::Note, USE_COMPARE),
            1,
            "the exit test is a use of the counter"
        );
        assert_eq!(stats.count(Kind::Note, USE_GENERIC), 0, "the increment is the variable itself");
        assert!(!stats.changed(), "the choosing rewrites nothing");
    }

    /// Section 28.3 calls this the cheapest large win in the pass, so it gets the plainest test.
    #[test]
    fn three_accesses_a_constant_apart_are_one_group_wanting_one_variable() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let zero = build.iconst(Type::int(32), 0);
        for away in [0, 1, 2] {
            let addr = element(&mut build, base, it.counter, away);
            build.store(zero, addr, plain(), Flags::NONE);
        }
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Note, USE_ADDRESS), 3);
        assert_eq!(stats.count(Kind::Note, GROUPED), 1, "one group, not three");
        // The whole of what the loop wants is two variables: the counter for the exit test, and
        // one pointer the three writes all reach off. The pointer is one the loop did not have,
        // because the counter is an integer and an address is not, so the answer is a change.
        // What the grouping bought is that it is one pointer rather than three.
        assert_eq!(stats.count(Kind::Note, CHOSEN), 2);
        assert_eq!(stats.count(Kind::Note, CHANGED), 1);
    }

    /// Two arrays walked at once, which is the case the loop's own variables cannot express.
    ///
    /// The counter serves the exit test and neither address, because an address is a pointer and
    /// the counter is an integer, so the choosing has to add something. What it must not do is add
    /// one variable per access when one serves both.
    #[test]
    fn a_walk_of_two_arrays_takes_a_variable_it_did_not_have() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let from = element(&mut build, base, it.counter, 0);
        let read = build.load(Type::int(32), from, plain(), Flags::NONE);
        let into = element(&mut build, base, it.counter, 4096);
        build.store(read, into, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Note, USE_ADDRESS), 2);
        assert_eq!(stats.count(Kind::Note, GROUPED), 1, "a constant apart, so one group");
        assert_eq!(stats.count(Kind::Note, CHOSEN), 2, "the counter and one address variable");
        assert_eq!(stats.count(Kind::Note, CHANGED), 1);
    }

    /// Nothing is a use when nothing in the loop moves, and a loop like that is not counted.
    #[test]
    fn a_loop_that_only_reads_a_fixed_address_is_not_asked_about() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let read = build.load(Type::int(32), base, plain(), Flags::NONE);
        build.store(read, base, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Note, USE_ADDRESS), 0);
        // The exit test still reads the counter, which is a use, so the loop is in the population.
        // What it has no address use for is the point.
        assert_eq!(stats.count(Kind::Note, USE_COMPARE), 1);
        assert_eq!(stats.count(Kind::Note, POPULATION), 1);
        // Its own counter serves the only use it has, so this is the shape that gets left alone.
        assert_eq!(stats.count(Kind::Note, KEPT), 1);
        assert_eq!(stats.count(Kind::Note, CHOSEN), 1);
    }

    /// Section 28.2's claim, and the one thing in this pass that is a fact about the machine.
    #[test]
    fn a_machine_nobody_priced_is_told_so_rather_than_guessed_at() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let addr = element(&mut build, base, it.counter, 0);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);

        let mut an = Analyses::new(crate::machine::Machine::unknown());
        let stats = Ivopts.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, NO_TARGET), 1);
        assert_eq!(stats.count(Kind::Note, POPULATION), 0);
        assert_eq!(stats.count(Kind::Note, CANDIDATE), 0);
    }

    #[test]
    fn a_function_with_no_loop_in_it_is_left_entirely_alone() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, base, plain(), Flags::NONE);
        build.ret(&[]);

        let stats = choose(&mut func);
        assert_eq!(stats.events().len(), 0);
        assert!(!stats.changed());
    }
}
