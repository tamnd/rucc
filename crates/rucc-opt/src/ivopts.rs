//! Chooses which induction variables a loop should keep, and gives a group of addresses its own.
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
//! # What it rewrites, and what it does not
//!
//! Section 28.7 lists five ways this goes wrong and four of them are ways a rewrite is wrong: an
//! exit test that is not equivalent, a derived limit that overflows, a signedness that changed,
//! and a set that spills. Two things are rewritten here, and both are written against that list.
//!
//! The first is a group of address uses that the search says should walk on a pointer of its own.
//! That rewrite is a pointer starting where the group's first address starts and stepping by the
//! group's step, with each use reading off it at the constant offset it already sat at. Nothing is
//! deleted: the addresses the uses used to read become dead and `crate::dce` is what removes them,
//! which is section 28.3's last line.
//!
//! The one new value the loop computes that it did not before is the pointer's last increment,
//! made on the iteration that then leaves. That is one step past the last address the loop
//! touched, which is the address a C program walking the same array with `p++` forms as well, and
//! forming it is an addition on a machine where an addition does not trap.
//!
//! The second is section 28.4's linear function test replacement, and it happens only on top of
//! the first. Once the loop walks a pointer, `i < n` becomes `p != limit` with `limit` worked out
//! before the loop, and then nothing in the loop wants `i` and the counter goes with `crate::dce`
//! as well. Section 28.7 calls a non-equivalent exit test the highest-severity bug in the
//! document, so here is the argument, in the four pieces the section asks for.
//!
//! Where the pointer is. This pass wrote the pointer itself, so its value is known rather than
//! deduced: the preheader hands the header `start`, every latch hands it one step on, and so at
//! the `j`th arrival at the header it holds `start + j * step`.
//!
//! Where the limit is. `crate::scev` indexes every sequence of the loop by that same `j`, and the
//! count it returns is the `j` at which the old test first refuses. So the pointer at the moment
//! the loop used to leave is `start + count * step`, and that number is the limit. It is one
//! addition in the preheader and it is the same value the loop forms on its last turn anyway.
//!
//! Why `!=` is the same test. Over `j` from nothing to the count, the pointer takes a different
//! value each time, because the whole walk fits in a signed sixty four bit number and so no two of
//! those addresses are equal modulo the width of a pointer. A test on `!=` needs that and a test
//! on `<` does not, so that condition is checked rather than assumed. There is no signedness left
//! to get wrong, because `!=` does not have one.
//!
//! Why the test has to be asked every turn. `p != limit` refuses on exactly one turn and `i < n`
//! refuses on every turn from then on, and the two are the same test only if the loop cannot get
//! past that turn. It cannot when the block holding the test dominates every latch, because then
//! every way round goes through it. That is the condition, and the loop is left alone without it.
//!
//! How many turns has to be a number rather than an expression, which is the narrow part of all
//! this: `for (i = 0; i < n; i++)` gets a symbolic count and is left alone. The arithmetic for a
//! symbolic limit is one multiply in the preheader and is not the difficulty. The difficulty is
//! that the argument above rests on a walk that fits in a signed sixty four bit number, and there
//! is no such number to check when the count is an expression. That is #753.
//!
//! What is still not rewritten is a group whose best server is some other candidate: the search
//! says so and this reports it, and expressing one sequence in terms of another is a multiply and
//! an add this has no reason to emit until there is a measurement asking for it. Section 28.4's
//! countdown is a candidate the search will not choose, because the cost table has no price for a
//! comparison against zero and so nothing in the model knows what a countdown buys. That is #751.
//!
//! # It is not in any pipeline
//!
//! Section 28.6 puts ivopts last among the loop passes and after unrolling, and it is not there
//! yet: the numbers it decides on are worth checking against #701's survey of what GCC 16 chooses
//! over the same corpus before a build depends on them. What reaches it is `-fenable-ivopts`,
//! with `-fopt-info-all` to see what it decided.
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
use rucc_ir::{
    Block, BlockCall, Def, Extra, Flags, Func, Inst, InstData, IntPred, Opcode, Type, Value,
};

use crate::analysis::Analysis;
use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::{LoopId, Loops};
use crate::machine::Machine;
use crate::scev::{Chrec, Count, Evolution, Invariant, Plain, Scev};
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
const ADDED: &str = "pointer given to the loop for a group of addresses to walk on";
const REWRITTEN: &str = "address use rewritten to read off a pointer of the loop's own";
const NO_PREHEADER: &str =
    "not rewritten, the loop has no one place outside it to start a pointer from";
const NOT_A_WALK: &str =
    "not rewritten, the group does not step through memory by a number of bytes known here";
const OUT_OF_REACH: &str =
    "not rewritten, what the group is measured from is not available before the loop";
const OUT_OF_FUEL: &str = "not rewritten, the fuel for this compilation ran out first";
const RETARGETED: &str = "exit test asked of the pointer the loop walks, so the counter goes";
const COUNTER_WANTED: &str =
    "exit test left alone, something else in the loop still wants the counter";
const LIMIT_TOO_FAR: &str =
    "exit test left alone, the address it would compare against is further off than fits";
const MANY_EXITS: &str = "exit test left alone, the loop leaves in more than one place";
const NOT_EVERY_TURN: &str =
    "exit test left alone, there is a way round the loop that does not ask it";
const NOT_A_TEST: &str =
    "exit test left alone, the loop does not leave on a comparison of one moving value";
const NOT_A_COUNT: &str =
    "exit test left alone, how many turns the loop takes is not a number known here";

/// The selection section 28.3 asks for.
#[derive(Debug)]
pub struct Ivopts;

impl Pass for Ivopts {
    fn name(&self) -> &'static str {
        "ivopts"
    }

    fn describe(&self) -> &'static str {
        "chooses the induction variables a loop should keep, and gives a group of addresses one"
    }

    fn preserves(&self) -> Preserved {
        // It adds parameters and arithmetic and moves no edge, so the shape of the function is
        // what it was. What is not what it was is which values are live where, because a pointer
        // carried round the loop is live round the whole of it, and the counts per register class
        // that come off that.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
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
        let doms = an.dominators(func).clone();

        // Every loop is decided before any loop is touched. The evolutions are read off the
        // function as it arrived, and a rewrite that ran between two of them would leave the
        // second reading a function the first had already changed. It also means the analysis
        // borrows the function for a while and the rewriting borrows it mutably afterwards,
        // which the two phases make plain rather than fight.
        let mut plans = Vec::new();
        {
            let mut scev = Scev::new(func, &cfg, &loops);
            let it = Loop { func, loops: &loops, doms: &doms, machine, table };
            for id in loops.all() {
                consider(&it, &mut scev, id, &mut stats, &mut plans);
            }
        }
        for plan in plans {
            // The exit test is asked of the pointer, so there is no exit test to rewrite until
            // the pointer is there. A refused rewrite takes the test it was carrying with it.
            let Some(walk) = rewrite(func, &cfg, &loops, &doms, &plan, fuel, &mut stats) else {
                continue;
            };
            if let Some(aim) = plan.aim {
                retarget(func, &walk, &aim, fuel, &mut stats);
            }
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
    /// The instruction reading it, which is where a rewrite of it goes.
    at: Inst,
    /// Which of that instruction's operands it is.
    position: usize,
}

/// One use, once the group it belongs to is known.
#[derive(Clone, Copy, Debug)]
struct Use {
    /// The instruction reading it.
    at: Inst,
    /// Which of that instruction's operands it is.
    position: usize,
    /// How far past the group's own base this one sits, which is the constant offset section
    /// 28.1 groups by and the number a rewrite adds back on.
    offset: i128,
}

/// Uses one variable can serve between them, apart only in a constant offset.
#[derive(Debug)]
struct Group {
    /// What they have in common, taken from the first of them.
    kind: Kind,
    /// The form the group is named by, which is the first use's.
    chrec: Chrec,
    /// Where the uses are and how far past the base each sits.
    uses: Vec<Use>,
    /// Whether this is the comparison the loop leaves on, and one section 28.4 could rewrite.
    ///
    /// It changes what the group costs rather than only what is reported: a test that can be
    /// asked of any variable is a test that is no reason to keep the one it names.
    exit: bool,
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
            && self.chrec.base.alike(want.chrec.base)
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

/// A group of addresses the search says should walk on a pointer of its own, and where they are.
///
/// This is everything the rewriting half needs, taken out of the analysis before the analysis
/// gives the function back. What it is not is a decision: the decision was made in [`select`] and
/// this is it written down.
#[derive(Debug)]
struct Plan {
    /// The loop the pointer goes round.
    id: LoopId,
    /// The sequence it follows, which is the group's own.
    chrec: Chrec,
    /// The uses that read off it.
    uses: Vec<Use>,
    /// The exit test to ask of the pointer once it exists, when there is one worth asking.
    ///
    /// At most one plan of a loop carries it, because a loop has one exit test and rewriting it
    /// twice would be rewriting it against a pointer that is not the one it was measured for.
    aim: Option<Aim>,
}

/// The comparison a loop leaves on, read into the pieces section 28.4's rewrite needs.
#[derive(Clone, Copy, Debug)]
struct Aim {
    /// The comparison itself, which is where the new one goes in front of.
    at: Inst,
    /// The branch reading it, which is the one operand this rewrite repoints.
    branch: Inst,
    /// How many turns the loop takes before that comparison first refuses.
    count: u128,
    /// Whether the loop keeps going when the comparison holds.
    stays: bool,
}

/// What every loop in one function is decided against, which is the same five things each time.
struct Loop<'a> {
    /// The function, as it arrived and before any rewriting.
    func: &'a Func,
    /// Its loops.
    loops: &'a Loops,
    /// Which blocks reach which, for whether the exit test is asked on every turn.
    doms: &'a Dominators,
    /// The machine, for how many registers there are to spare.
    machine: Machine,
    /// Its prices, which section 28.2 says the answer is a fact about.
    table: &'a CostTable,
}

/// Everything about one loop, from collecting its uses to naming the set it should keep.
fn consider(
    it: &Loop<'_>,
    scev: &mut Scev<'_>,
    id: LoopId,
    stats: &mut Stats,
    plans: &mut Vec<Plan>,
) {
    let Loop { func, loops, doms, machine, table } = *it;
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

    // Which group is the exit test, worked out before anything is priced, because a test section
    // 28.4 can move is a test that costs the same whichever variable it is asked of.
    let aimed = aim(func, loops, doms, scev, id);
    let mut groups = group(wants);
    for one in &mut groups {
        let is_exit = |at: Aim| one.kind == Kind::Compare && one.uses.iter().any(|u| u.at == at.at);
        one.exit = aimed.is_ok_and(is_exit);
    }
    let groups = groups;
    for one in &groups {
        if one.uses.len() > 1 {
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

    // The groups the search says should get a pointer of their own. A group is one of those when
    // the candidate that was made for it is in the chosen set, and that candidate serves it for
    // nothing, which is the least any candidate can charge, so it is what the group is served
    // with whenever it is there at all.
    let mut walks = 0;
    for one in &groups {
        if one.kind != Kind::Address {
            continue;
        }
        let own = chosen.iter().any(|&at| {
            cands[at].origin == Origin::Derived
                && cands[at].chrec.base == one.chrec.base
                && cands[at].chrec.step == one.chrec.step
                && cands[at].chrec.ty == one.chrec.ty
        });
        if own {
            walks += 1;
            plans.push(Plan { id, chrec: one.chrec, uses: one.uses.clone(), aim: None });
        }
    }
    if walks == 0 {
        // Nothing walks, so there is no pointer for the exit test to be asked of and nothing to
        // report about it either. Section 28.4's rewrite is only ever on top of section 28.3's.
        return;
    }

    // Section 28.4's rewrite is worth making when nothing else in the loop wants the counter, and
    // the search is what says so: the exit test costs the same either way now, so a counter still
    // in the chosen set is a counter something else is paying for.
    let aimed = aimed.and_then(|at| {
        // No group for it means the test was never priced as free, so the argument below was
        // never made and there is nothing here to claim. It happens when the test sits in a loop
        // inside this one, whose uses belong to that loop's own question.
        let Some(counter) = groups.iter().find(|one| one.exit) else { return Err(NOT_A_TEST) };
        let wanted = chosen
            .iter()
            .any(|&had| cands[had].origin == Origin::Original && counts(counter, &cands[had]));
        if wanted { Err(COUNTER_WANTED) } else { Ok(at) }
    });
    match aimed {
        Ok(at) => {
            let first = plans.len() - walks;
            plans[first].aim = Some(at);
        }
        Err(why) => stats.missed(why),
    }
}

/// Whether the exit test is written in this candidate already, rather than in one it would have
/// to be rewritten to use.
///
/// The same rule [`serve`] used before the exit test became free to move: a whole number of this
/// candidate's steps makes one of the group's, in the type the group is in.
fn counts(group: &Group, cand: &Cand) -> bool {
    group.chrec.ty == cand.chrec.ty && ratio(cand.chrec.step, group.chrec.step).is_some()
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
                wants.push(Want { kind, chrec, at: inst, position });
            }
        }
    }
    wants
}

/// The exit test of a loop, when it is one section 28.4's rewrite could be made against.
///
/// Every condition the module documentation's argument rests on is checked here, before anything
/// is priced and long before anything is written. The error is the reason, so a caller with
/// something to say can say which one it was.
fn aim(
    func: &Func,
    loops: &Loops,
    doms: &Dominators,
    scev: &mut Scev<'_>,
    id: LoopId,
) -> Result<Aim, &'static str> {
    // One exit, because the count `crate::scev` returns is the count from the first exit it can
    // solve, and a second exit is a way out at some other turn that this has not measured.
    let [exit] = loops.exits(id) else { return Err(MANY_EXITS) };
    // Asked on every turn, which is the fourth piece of the argument.
    if !loops.latches(id).iter().all(|&latch| doms.dominates(exit.from, latch)) {
        return Err(NOT_EVERY_TURN);
    }

    let Some(branch) = func.terminator(exit.from) else { return Err(NOT_A_TEST) };
    if func[branch].opcode != Opcode::BrIf {
        return Err(NOT_A_TEST);
    }
    let Some(&cond) = func[func[branch].args].first() else { return Err(NOT_A_TEST) };
    let calls = &func[func.target_list(branch)];
    let (Some(&taken), Some(&other)) = (calls.first(), calls.get(1)) else {
        return Err(NOT_A_TEST);
    };
    let stays = match (loops.contains(id, taken.block), loops.contains(id, other.block)) {
        (true, false) => true,
        (false, true) => false,
        // Both arms in or both arms out is a branch that is not what ends the loop, whatever else
        // it is, which is the reading `crate::scev` takes of the same shape.
        _ => return Err(NOT_A_TEST),
    };

    let Def::Result { inst, .. } = func[cond].def else { return Err(NOT_A_TEST) };
    if func[inst].opcode != Opcode::ICmp || func.block_of(inst) != Some(exit.from) {
        return Err(NOT_A_TEST);
    }
    let operands = &func[func[inst].args];
    let (Some(&lhs), Some(&rhs)) = (operands.first(), operands.get(1)) else {
        return Err(NOT_A_TEST);
    };
    // One side moves and the other does not. Two moving sides is a test whose limit is not
    // something the preheader can work out, and no moving side is not a test about this loop.
    let moving = [affine(scev, id, lhs).is_some(), affine(scev, id, rhs).is_some()];
    if moving[0] == moving[1] {
        return Err(NOT_A_TEST);
    }

    // A number rather than an expression, because the limit is the count multiplied by the step
    // and this has nowhere to emit a multiply. `under_undefined_overflow` rather than `proven`
    // for the reason `candidates` gives.
    let Some(bound) = scev.bound(id) else { return Err(NOT_A_COUNT) };
    let Some(Count::Exact(count)) = bound.under_undefined_overflow() else {
        return Err(NOT_A_COUNT);
    };
    Ok(Aim { at: inst, branch, count, stays })
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
        let (at, position) = (want.at, want.position);
        match groups.iter_mut().find(|one| one.takes(&want)) {
            Some(one) => {
                let offset = want.chrec.base.offset() - one.chrec.base.offset();
                one.uses.push(Use { at, position, offset });
            }
            None => groups.push(Group {
                kind: want.kind,
                chrec: want.chrec,
                uses: vec![Use { at, position, offset: 0 }],
                exit: false,
            }),
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
    // Section 28.4's rewrite, priced. The exit test can be asked of any variable that moves by a
    // step this pass can multiply out, because the limit is that step times the trip count and it
    // is worked out once before the loop. So what the test costs inside the loop is the
    // comparison it already was, which is nothing this set has to pay for, and the variable it
    // happens to name today is no reason to keep that variable.
    if group.exit && cand.chrec.step.as_number().is_some_and(|step| step != 0) {
        return Cost::ZERO;
    }
    // Two sequences in different types mean different things to the two kinds of use, so the
    // question is asked once and answered twice.
    //
    // For a value it is a conversion, which is arithmetic on the sequence, and `crate::scev`
    // refuses to widen a chrec whose ends are symbolic, so the honest answer is that this
    // candidate does not serve this group rather than a price for a conversion nobody checked.
    //
    // For an address it is not a conversion at all. The address is the candidate read as the
    // index of an addressing mode, which is the ordinary `a[i]`: the group's sequence is a
    // pointer, the candidate's is the counter, and the width the counter is read at is the index
    // register's rather than the sequence's. Refusing it here is refusing the only case
    // [`address_cost`] below says it exists to price, and it leaves every address group with its
    // own pointer as the one thing that can serve it, which is a choice made before the cost
    // model is asked rather than by it.
    let indexing = group.kind == Kind::Address && cand.chrec.ty.is_int();
    if group.chrec.ty != cand.chrec.ty && !indexing {
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
    // Two symbols is two registers before the scale is applied, which is a shape no addressing
    // mode holds, so there is no price to quote for it. A symbol read at a wider type than its own
    // is not a register either: it is an extension in front of one, and what the modes hold is the
    // register.
    let Some(rest) = rest.plain().filter(|rest| rest.read.is_none()) else {
        return Cost::INFINITE;
    };
    match group.kind {
        Kind::Address => address_cost(table, scale, rest) + index_cost(table, cand.chrec.ty),
        Kind::Compare | Kind::Generic => value_cost(table, group.chrec.ty, scale, rest),
    }
}

/// What reading this candidate as the index of an addressing mode costs before the mode itself.
///
/// An index register is a pointer wide on every one of rucc's targets, so a candidate counting in
/// something narrower is extended first and the extension is an instruction. A candidate already
/// that wide pays nothing, and neither does a pointer, which is what a group's own candidate is.
///
/// Per use, like everything else `serve` answers, and that is the pessimistic reading: two uses a
/// block apart share one extension in the emitted code and this charges for two. The direction is
/// deliberate. What it overprices is keeping the counter, which is the side section 28.7 says to
/// be careful about being wrong on, and a group large enough for the difference to decide
/// anything is a group where the pointer was going to win regardless.
fn index_cost(table: &CostTable, ty: Type) -> Cost {
    if ty.is_int() && ty.bits() < Width::W64.bits() {
        Cost::cycles(table.movsx)
    } else {
        Cost::ZERO
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
///
/// The address is `cand * scale + rest`, and the mode it lands in follows from reading the two
/// registers the right way round. `rest` is the part that does not change, so it is the base, and
/// the candidate is what the index holds. That is the ordinary `a[i]`: `a` is the base, `i` is the
/// index, and the width of an element is the scale.
fn address_cost(table: &CostTable, scale: i128, rest: Plain) -> Cost {
    let indexed = scale != 1;
    let displaced = rest.offset != 0;
    let symbolic = rest.value.is_some() && rest.scale != 0;
    if indexed && !legal_scale(scale) {
        // A scale no addressing mode holds has to be multiplied out, and then the product is a
        // plain register the other modes can still use.
        let mult = table.mult_of(width(rest)).max(Cycles::ONE);
        return Cost::cycles(mult) + address_cost(table, 1, rest);
    }
    if symbolic && rest.scale != 1 {
        // A base is a register, and this one is a multiple of a register. Multiplying it out
        // leaves an address of the same shape whose invariant part is one of something, which
        // every arm below can then read as a base.
        let mult = table.mult_of(width(rest)).max(Cycles::ONE);
        return Cost::cycles(mult) + address_cost(table, scale, Plain { scale: 1, ..rest });
    }
    let mode = match (indexed, symbolic, displaced) {
        (false, false, false) => AddrMode::Base,
        (false, false, true) => AddrMode::BaseDisp,
        (false, true, false) => AddrMode::BaseIndex,
        // Two registers and a displacement and no scale on the index, which is the one shape the
        // five modes do not name. The scaled mode holds it, because a scale of one is a scale a
        // scaled mode can carry, and it is the smallest of them that does. Reading it as the
        // larger mode prices it one point of complexity above what it is, and that is the
        // direction section 28.7 asks to be wrong in: the loop keeps what the program wrote.
        (false, true, true) => AddrMode::BaseIndexScaleDisp,
        // The candidate is the index whether or not there is a base register to go with it. When
        // there is not, the mode is priced as though there were, which is again the safe
        // direction and is what a target with no base-less mode would charge anyway.
        (true, _, false) => AddrMode::BaseIndexScale,
        (true, _, true) => AddrMode::BaseIndexScaleDisp,
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
fn value_cost(table: &CostTable, ty: Type, scale: i128, rest: Plain) -> Cost {
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
fn width(_rest: Plain) -> Width {
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
        cost += best * i64::try_from(group.uses.len()).unwrap_or(1);
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

/// Gives one group of addresses a pointer of its own, and points the group at it.
///
/// Section 28.1's fourth step, over the one group at a time the third step said should have one.
/// The pointer starts where the group's first address starts, steps by the group's step on every
/// edge back to the header, and each use reads off it at the constant offset it already sat at.
/// The addresses the uses used to read are left where they are, dead, for `crate::dce`.
///
/// Every reason not to do it is checked before anything is written, so a refusal is a refusal
/// rather than a half-finished rewrite. There is no undo here and there should not need to be.
///
/// What it answers with is the walk it wrote, because section 28.4's rewrite is a question about
/// that walk and about nothing else in the function.
fn rewrite(
    func: &mut Func,
    cfg: &Cfg,
    loops: &Loops,
    doms: &Dominators,
    plan: &Plan,
    fuel: &mut Fuel,
    stats: &mut Stats,
) -> Option<Walk> {
    // Section 28.7's last entry: no preheader means the loop is not in the shape section 26.2
    // asks for, and a pass that splits an edge to make one is doing the canonicalizer's job.
    let Some(pre) = loops.preheader(cfg, plan.id) else {
        stats.missed(NO_PREHEADER);
        return None;
    };
    let header = loops.header(plan.id);
    let base = plan.chrec.base;

    // What has to be true for the pointer to be writable: it is a pointer, it moves by a number
    // of bytes this pass knows, and what it is measured from is one value rather than an
    // expression somebody would have to rebuild. Anything else is a group that would need
    // arithmetic emitted for it, and section 28.3's rewrite is not that.
    let step = plan.chrec.step.as_number();
    let Some(base) = base.plain() else {
        stats.missed(NOT_A_WALK);
        return None;
    };
    let (Some(step), Some(from), Type::PTR, 1, None) =
        (step, base.value, plan.chrec.ty, base.scale, base.read)
    else {
        stats.missed(NOT_A_WALK);
        return None;
    };

    // The starting value is computed in the preheader, so what it is computed from has to be
    // available there. A loop invariant used inside the loop dominates the header, and the
    // preheader is the only way in, so this holds for every group that got here. It is checked
    // anyway, because the cost of checking is a dominance query and the cost of being wrong is a
    // function that reads a value before it exists.
    let Some(home) = home(func, from) else {
        stats.missed(OUT_OF_REACH);
        return None;
    };
    if !doms.dominates(home, pre) {
        stats.missed(OUT_OF_REACH);
        return None;
    }
    if !fuel.take() {
        stats.missed(OUT_OF_FUEL);
        return None;
    }

    let term = func.terminator(pre).expect("a preheader ends in a jump to the header");
    let start = past(func, term, from, base.offset);

    let param = func.append_param(header, Type::PTR);
    let mut preds: Vec<Block> = cfg.predecessors(header).to_vec();
    preds.sort_unstable();
    preds.dedup();
    for block in preds {
        let term = func.terminator(block).expect("a block with a successor ends in a branch");
        // From outside the loop the pointer is where the group starts. From inside it is one
        // step on, which is the increment that makes it an induction variable at all.
        let carry = if block == pre { start } else { past(func, term, param, step) };
        for at in func.target_list(term).iter() {
            let call = func[at];
            if call.block != header {
                continue;
            }
            let args = func.append_arg(call.args, carry);
            func.set_block_call(at, BlockCall { block: call.block, args });
        }
    }

    // One address per offset per block. Two uses at the same offset in one block read the same
    // value, and the first of them is where it is computed, so it is available at the second.
    // Two uses at the same offset in different blocks each get their own, because neither block
    // has been asked whether it dominates the other and `crate::simplify` is what merges them.
    let mut ready: Vec<(Block, i128, Value)> = Vec::new();
    for one in &plan.uses {
        let Some(block) = func.block_of(one.at) else { continue };
        let had = ready.iter().find(|&&(at, offset, _)| at == block && offset == one.offset);
        let value = match had {
            Some(&(_, _, value)) => value,
            None => {
                let value = past(func, one.at, param, one.offset);
                ready.push((block, one.offset, value));
                value
            }
        };
        set_arg(func, one.at, one.position, value);
        stats.optimized(REWRITTEN);
    }
    stats.optimized(ADDED);
    Some(Walk { pre, param, start, step })
}

/// A pointer this pass gave a loop, which is what section 28.4's rewrite is written against.
#[derive(Clone, Copy, Debug)]
struct Walk {
    /// The one block outside the loop it starts from, and the block its limit is worked out in.
    pre: Block,
    /// The header parameter it arrives on, which dominates every block of the loop.
    param: Value,
    /// Where it starts, already computed in the preheader.
    start: Value,
    /// How many bytes it moves on every turn.
    step: i128,
}

/// Asks the loop's exit test of the pointer instead of the counter, per section 28.4.
///
/// The module documentation is the argument this rests on and [`aim`] is where the conditions are
/// checked. What is left here is arithmetic: the limit is the count multiplied by the step, added
/// to where the pointer starts, in the preheader.
///
/// Nothing is deleted and nothing is edited in place. A new comparison goes in front of the old
/// one and the branch is repointed at it, so anybody else reading the old comparison still reads
/// what they read before and `crate::dce` is what takes it away when nobody does.
fn retarget(func: &mut Func, walk: &Walk, aim: &Aim, fuel: &mut Fuel, stats: &mut Stats) {
    // The whole walk, which has to be a number an address addition can take. It also has to fit
    // in a signed sixty four bit number for the reason the module documentation gives: that is
    // what makes the addresses along the way all different, and `!=` needs them to be.
    let far = i128::try_from(aim.count).ok().and_then(|count| count.checked_mul(walk.step));
    let Some(far) = far.filter(|&far| i64::try_from(far).is_ok()) else {
        stats.missed(LIMIT_TOO_FAR);
        return;
    };
    if !fuel.take() {
        stats.missed(OUT_OF_FUEL);
        return;
    }

    let term = func.terminator(walk.pre).expect("a preheader ends in a jump to the header");
    let limit = past(func, term, walk.start, far);

    // Not an ordering, so there is no signedness to change and nothing to get wrong at the ends,
    // which is two of section 28.7's five in one choice of predicate.
    let pred = if aim.stays { IntPred::Ne } else { IntPred::Eq };
    let span = func.span(aim.at);
    let args = func.push_values(&[walk.param, limit]);
    let data = InstData { args, extra: Extra::IntPred(pred), ..InstData::new(Opcode::ICmp) };
    let ty = func[walk.param].ty.with_lane(Type::I1);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, aim.at);
    let cond = func[inst].first_result.expect("one result was asked for");
    set_arg(func, aim.branch, 0, cond);
    stats.optimized(RETARGETED);
}

/// The address this many bytes past that one, computed in front of an instruction.
///
/// Nothing is emitted for a step of nothing, which is the common case: a group's own base is at
/// offset zero and so is the first use in it.
fn past(func: &mut Func, before: Inst, from: Value, offset: i128) -> Value {
    if offset == 0 {
        return from;
    }
    let by = number(func, before, Type::int(64), offset);
    let span = func.span(before);
    let args = func.push_values(&[from, by]);
    let data = InstData { args, ..InstData::new(Opcode::PtrAdd) };
    let inst = func.create_inst(data, &[Type::PTR], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

/// A constant, computed in front of an instruction.
fn number(func: &mut Func, before: Inst, ty: Type, value: i128) -> Value {
    let imm = func.add_imm(rucc_ir::Imm::int(value, ty.lane()));
    let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

/// Replaces one operand of an instruction, by position rather than by what is there.
///
/// By position because two operands of one instruction can be the same value and only one of
/// them is the use being rewritten.
fn set_arg(func: &mut Func, at: Inst, position: usize, value: Value) {
    let args = func[at].args;
    let mut seen = 0;
    func.rewrite(args, |had| {
        let here = seen;
        seen += 1;
        if here == position { value } else { had }
    });
}

/// The block a value is defined in.
fn home(func: &Func, value: Value) -> Option<Block> {
    match func[value].def {
        Def::Result { inst, .. } => func.block_of(inst),
        Def::Param { block, .. } => Some(block),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Extra, Flags, Func, IntPred, MemInfo, MemOrder, Module, Opcode, Restrict,
        Signature, Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{
        ADDED, AddrMode, CANDIDATE, CHANGED, CHOSEN, COUNTER_WANTED, Cand, Chrec, Cost, Cycles,
        GROUPED, Group, Invariant, Ivopts, KEPT, LIMIT_TOO_FAR, MANY_EXITS, NO_TARGET,
        NOT_EVERY_TURN, OUT_OF_FUEL, Origin, POPULATION, Plain, RETARGETED, REWRITTEN, USE_ADDRESS,
        USE_COMPARE, USE_GENERIC, address_cost, serve, width,
    };
    use crate::stats::Kind;
    use crate::{Analyses, Fuel, Pass, Stats};

    /// Runs the choosing over the function as it stands, on a machine somebody priced.
    fn choose(func: &mut Func) -> Stats {
        Ivopts.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// Insists the function is one the rest of the compiler may believe.
    ///
    /// Growing a block parameter is the edit that leaves a branch handing a block the wrong
    /// number of arguments, and putting the first value of one in the preheader is the edit that
    /// leaves a use before its definition. Both are what this catches.
    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    /// How many parameters a block takes.
    fn params(func: &Func, block: Block) -> usize {
        func[block].params.len()
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

    /// A counted loop that leaves when its test holds rather than when it fails.
    ///
    /// ```text
    /// into:      jump head(0)
    /// head(i):   t = i >= limit; br t -> out, body(i)
    /// ```
    ///
    /// The same loop as [`counted`] written the other way round, which is the case where the new
    /// comparison has to be `==` rather than `!=`.
    fn counted_leaving(func: &mut Func, into: Block, limit: i128) -> Counted {
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
        let test = build.icmp(IntPred::Sge, i, stop);
        build.br_if(test, out, &[], body, &[i]);

        Counted { head, body, out, counter: carried }
    }

    /// A loop that asks its test at the bottom, which is what a `do` loop leaves.
    ///
    /// The body goes in the header, and [`bottom_close`] writes the increment and the test after
    /// it. The point of the shape is that the value the test compares is one step further along
    /// than the one the body used, so a rewrite that reads the trip count as a number of turns
    /// through the body rather than as `crate::scev` means it lands one element out.
    struct Bottom {
        head: Block,
        out: Block,
        counter: Value,
    }

    fn bottom(func: &mut Func, into: Block) -> Bottom {
        let head = func.create_block();
        let out = func.create_block();
        let counter = func.append_param(head, Type::int(64));
        let mut build = Builder::new(func, into);
        let zero = build.iconst(Type::int(64), 0);
        build.jump(head, &[zero]);
        Bottom { head, out, counter }
    }

    /// Closes a bottom-tested loop, once its body has been written into the header.
    fn bottom_close(func: &mut Func, it: &Bottom, limit: i128) {
        let mut build = Builder::new(func, it.head);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, it.counter, one, Flags::NSW);
        let stop = build.iconst(Type::int(64), limit);
        let test = build.icmp(IntPred::Slt, next, stop);
        build.br_if(test, it.head, &[next], it.out, &[]);
    }

    /// Every address the function writes to, in order, with the pointer it was handed at zero.
    ///
    /// This is the check that matters for section 28.4, which calls an exit test that is not
    /// equivalent the highest-severity bug in the document. Counting instructions says the pass
    /// wrote a comparison. Running the loop and writing down where it wrote says the comparison
    /// is the one the loop had before, at both ends and not only in the middle.
    ///
    /// It interprets exactly what the functions below hold and stops rather than guesses at
    /// anything else, the same as `crate::short_circuit`'s does. A load answers zero, because
    /// nothing here writes anywhere it read.
    fn stores(func: &Func) -> Vec<i128> {
        let mut values: HashMap<Value, i128> = HashMap::new();
        let mut block = func.entry().expect("a function with blocks in it");
        for &param in &func[block].params {
            values.insert(param, 0);
        }
        let mut wrote = Vec::new();
        for _ in 0..10_000 {
            let mut end = None;
            for inst in func.insts(block) {
                if func.is_terminator(inst) {
                    end = Some(inst);
                    break;
                }
                let data = func[inst];
                let args: Vec<i128> = func[data.args].iter().map(|arg| values[arg]).collect();
                if data.opcode == Opcode::Store {
                    wrote.push(args[1]);
                    continue;
                }
                let result = data.first_result.expect("one result");
                let it = match data.opcode {
                    Opcode::IConst => {
                        let (imm, ty) =
                            crate::fold::constant(func, result).expect("a constant is one");
                        imm.signed(ty)
                    }
                    Opcode::ICmp => {
                        let Extra::IntPred(pred) = data.extra else {
                            panic!("a comparison carries its predicate");
                        };
                        i128::from(match pred {
                            IntPred::Slt => args[0] < args[1],
                            IntPred::Sge => args[0] >= args[1],
                            IntPred::Ne => args[0] != args[1],
                            IntPred::Eq => args[0] == args[1],
                            other => panic!("nothing here compares with {other:?}"),
                        })
                    }
                    Opcode::Add | Opcode::PtrAdd => args[0] + args[1],
                    Opcode::Mul => args[0] * args[1],
                    Opcode::Load => 0,
                    other => panic!("nothing here writes a {other:?}"),
                };
                values.insert(result, it);
            }
            let end = end.expect("every block here ends in a terminator");
            let data = func[end];
            let call = match data.opcode {
                Opcode::Jump => func.successors(end).next().expect("a jump has one edge"),
                Opcode::BrIf => {
                    let cond = values[&func[data.args][0]];
                    let mut edges = func.successors(end);
                    let then = edges.next().expect("a branch has two edges");
                    let other = edges.next().expect("a branch has two edges");
                    if cond == 0 { other } else { then }
                }
                Opcode::Return => return wrote,
                other => panic!("nothing here ends a block with a {other:?}"),
            };
            let carried: Vec<i128> = func[call.args].iter().map(|arg| values[arg]).collect();
            for (&param, arg) in func[call.block].params.iter().zip(carried) {
                values.insert(param, arg);
            }
            block = call.block;
        }
        panic!("the loop never ended");
    }

    /// The predicate the branch ending this block is on.
    fn leaves_on(func: &Func, block: Block) -> IntPred {
        let term = func.terminator(block).expect("every block here has one");
        let cond = func[func[term].args][0];
        let rucc_ir::Def::Result { inst, .. } = func[cond].def else {
            panic!("the condition came out of a comparison");
        };
        let Extra::IntPred(pred) = func[inst].extra else {
            panic!("a comparison carries its predicate");
        };
        pred
    }

    /// How wide one element of the array the loops below walk is.
    ///
    /// Sixty four, which is a `struct` of sixteen words rather than a bare `int`, and the choice
    /// decides what the tests underneath are able to be about. No addressing mode on any of rucc's
    /// targets holds a scale of sixty four, so the index has to be multiplied out before every
    /// access, and a pointer that adds sixty four a turn is strictly fewer instructions. That is
    /// the shape the pass is for. An array of `int` is the shape it is not: `(%rax,%rcx,4)` is one
    /// mode and holding an index in it costs almost nothing, so a pointer of its own buys the loop
    /// an increment and a register and saves it next to nothing. The last test in this module is
    /// the one that says so, and it is the only one below that walks an array of `int`.
    const STRIDE: i128 = 64;

    /// The address of `base[i + away]`, in the block being built.
    fn element(build: &mut Builder<'_>, base: Value, counter: Value, away: i128) -> Value {
        strided(build, base, counter, away, STRIDE)
    }

    /// The same address over an array whose elements are `width` bytes wide.
    fn strided(
        build: &mut Builder<'_>,
        base: Value,
        counter: Value,
        away: i128,
        width: i128,
    ) -> Value {
        let each = build.iconst(Type::int(64), width);
        let by = build.binary(Opcode::Mul, counter, each, Flags::NSW);
        let at = build.binary(Opcode::PtrAdd, base, by, Flags::NONE);
        if away == 0 {
            return at;
        }
        let past = build.iconst(Type::int(64), away * width);
        build.binary(Opcode::PtrAdd, at, past, Flags::NONE)
    }

    /// A value to hang a symbolic invariant on, which is all the costing tests below want.
    fn some_value() -> Value {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        func.append_param(entry, Type::PTR)
    }

    /// The table the costing tests read, which is x86-64 optimizing for speed.
    fn priced() -> crate::machine::Machine {
        crate::machine::fixtures::machine()
    }

    #[test]
    fn an_array_read_is_one_addressing_mode_rather_than_an_addition_in_front_of_one() {
        // `a[i]`, which is `a + i * 4`. The array does not change inside the loop so it is the
        // base, the counter is the index, and the width of an element is the scale. x86-64 has
        // that mode, so the cost is what the table says the mode costs and nothing goes in front.
        let machine = priced();
        let table = machine.table().unwrap();
        let array = Plain { value: Some(some_value()), read: None, scale: 1, offset: 0 };
        assert_eq!(address_cost(table, 4, array), table.addr_cost(AddrMode::BaseIndexScale));
        let past = Plain { offset: 8, ..array };
        assert_eq!(address_cost(table, 4, past), table.addr_cost(AddrMode::BaseIndexScaleDisp));
    }

    #[test]
    fn a_pointer_the_loop_walks_costs_a_plain_base_and_that_is_what_it_competes_with() {
        // The other half of the comparison the choosing makes. A candidate that is already the
        // address wants no index and no scale, so it is the cheapest mode there is. What the
        // cost model has to get right is how much cheaper, because the difference is the whole
        // argument for rewriting an indexed read into a walked pointer: the pointer takes the
        // index off every read and pays one increment a turn to do it, so it is worth having
        // when the loop reads through it more than once and not otherwise. Charging an addition
        // in front of the index as well, which is what this used to do, made it worth it always.
        let machine = priced();
        let table = machine.table().unwrap();
        let walked = address_cost(table, 1, Plain { value: None, read: None, scale: 0, offset: 0 });
        assert_eq!(walked, table.addr_cost(AddrMode::Base));
        let indexed = address_cost(
            table,
            4,
            Plain { value: Some(some_value()), read: None, scale: 1, offset: 0 },
        );
        assert!(walked < indexed, "an index costs something or the two would never be compared");
        assert_eq!(
            indexed.cycles,
            walked.cycles + table.add,
            "an index costs exactly what an addition costs here, which is the number the whole \
             trade turns on",
        );
    }

    #[test]
    fn the_counter_serves_an_address_and_pays_for_the_extension_the_index_wants() {
        // The comparison the whole search is about, and the one nothing used to ask. The group is
        // `a[i]`, a pointer sequence stepping by the width of an element, and the candidate is the
        // counter, an `i32`. Reading the counter as an index is what the address already does, so
        // the price is the mode plus what widening the counter to an index register costs. A
        // counter already that wide pays for the mode alone.
        let machine = priced();
        let table = machine.table().unwrap();
        let array = some_value();
        let group = Group {
            kind: super::Kind::Address,
            chrec: Chrec {
                base: Invariant::of(array),
                step: Invariant::number(4),
                ty: Type::PTR,
                flags: Flags::NONE,
            },
            uses: Vec::new(),
            exit: false,
        };
        let counting = |ty| Cand {
            chrec: Chrec {
                base: Invariant::number(0),
                step: Invariant::number(1),
                ty,
                flags: Flags::NONE,
            },
            origin: Origin::Original,
        };
        let mode = table.addr_cost(AddrMode::BaseIndexScale);
        assert_eq!(serve(table, &group, &counting(Type::int(64))), mode);
        assert_eq!(
            serve(table, &group, &counting(Type::int(32))),
            mode + Cost::cycles(table.movsx)
        );
    }

    #[test]
    fn a_base_that_is_a_multiple_of_a_register_is_multiplied_out_first() {
        // `2 * n + i * 4` has an invariant part that is not a register, and a base is a register.
        // So the multiply is charged and what is left is an ordinary indexed address.
        let machine = priced();
        let table = machine.table().unwrap();
        let value = some_value();
        let doubled = Plain { value: Some(value), read: None, scale: 2, offset: 0 };
        let mult = table.mult_of(width(doubled)).max(Cycles::ONE);
        let want = Cost::cycles(mult)
            + address_cost(table, 4, Plain { value: Some(value), read: None, scale: 1, offset: 0 });
        assert_eq!(address_cost(table, 4, doubled), want);
    }

    #[test]
    fn a_scale_no_mode_holds_is_multiplied_out_and_the_rest_is_still_an_address() {
        // Three is not one, two, four or eight, so no addressing mode carries it. What is left
        // once it has been multiplied out is a register, and a register is something the modes
        // still have room for.
        let machine = priced();
        let table = machine.table().unwrap();
        let array = Plain { value: Some(some_value()), read: None, scale: 1, offset: 0 };
        let mult = table.mult_of(width(array)).max(Cycles::ONE);
        let want = Cost::cycles(mult) + address_cost(table, 1, array);
        assert_eq!(address_cost(table, 3, array), want);
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
        let before = params(&func, it.head);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Note, POPULATION), 1);
        assert_eq!(stats.count(Kind::Note, USE_ADDRESS), 1);
        assert_eq!(
            stats.count(Kind::Note, USE_COMPARE),
            1,
            "the exit test is a use of the counter"
        );
        assert_eq!(stats.count(Kind::Note, USE_GENERIC), 0, "the increment is the variable itself");

        // One group, so one pointer, and the write reads off it rather than off a multiply.
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1);
        assert_eq!(stats.count(Kind::Optimized, REWRITTEN), 1);
        assert_eq!(params(&func, it.head), before + 1, "the loop carries the pointer round");
        sound(&func, &mut names);
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
        let before = params(&func, it.head);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Note, USE_ADDRESS), 3);
        assert_eq!(stats.count(Kind::Note, GROUPED), 1, "one group, not three");
        // The whole of what the loop wants is one variable: a pointer the three writes all reach
        // off, with the exit test asked of it as well. The counter is not one of them, because
        // the only thing left wanting it was the test and section 28.4 moves the test.
        assert_eq!(stats.count(Kind::Note, CHOSEN), 1);
        assert_eq!(stats.count(Kind::Note, CHANGED), 1);

        // One pointer added for the three of them, and all three rewritten to read off it. The
        // two that sit past it read off it at their old constant offset, which is the whole
        // reason grouping is worth anything.
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1, "one pointer, not three");
        assert_eq!(stats.count(Kind::Optimized, REWRITTEN), 3);
        assert_eq!(params(&func, it.head), before + 1);
        sound(&func, &mut names);
    }

    /// Two arrays walked at once, which is the case the loop's own variables cannot pay for.
    ///
    /// The counter can serve both addresses, by being the index of a mode, and at this stride that
    /// costs a multiply before every one of them. So the choosing adds something. What it must not
    /// do is add one variable per access when one serves both.
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
        assert_eq!(stats.count(Kind::Note, CHOSEN), 1, "one address variable, and no counter");
        assert_eq!(stats.count(Kind::Note, CHANGED), 1);
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1);
        assert_eq!(stats.count(Kind::Optimized, REWRITTEN), 2, "the read and the write");
        sound(&func, &mut names);
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
        assert!(!stats.changed(), "a loop with nothing to rewrite is not rewritten");
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
        assert!(!stats.changed(), "an unpriced machine gets no rewrite either");
    }

    /// Section 9.10's bisection: the rewrite is fuelled, so a build can be cut in half at it.
    #[test]
    fn the_rewrite_stops_when_the_fuel_does_and_says_so() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let addr = element(&mut build, base, it.counter, 0);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);
        let before = params(&func, it.head);

        let mut an = crate::machine::fixtures::analyses();
        let stats = Ivopts.run(&mut func, &mut an, &mut Fuel::of(0));
        assert_eq!(stats.count(Kind::Missed, OUT_OF_FUEL), 1);
        assert_eq!(stats.count(Kind::Optimized, ADDED), 0);
        assert_eq!(params(&func, it.head), before, "nothing was half done");
        assert_eq!(
            stats.count(Kind::Note, POPULATION),
            1,
            "the choosing still happened and still reported"
        );
        sound(&func, &mut names);
    }

    /// Section 28.4, on the loop the section itself writes down.
    #[test]
    fn a_loop_that_only_walks_an_array_stops_counting_and_tests_the_pointer() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let addr = element(&mut build, base, it.counter, 0);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);
        let before = stores(&func);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1);
        assert_eq!(stats.count(Kind::Optimized, RETARGETED), 1);
        assert_eq!(
            stats.count(Kind::Note, CHOSEN),
            1,
            "the pointer, and nothing that only the test wanted"
        );
        assert_eq!(
            leaves_on(&func, it.head),
            IntPred::Ne,
            "the loop keeps going while the pointer has not landed on the limit"
        );
        assert_eq!(stores(&func), before, "the same hundred addresses, in the same order");
        assert_eq!(before.len(), 100, "and the loop under test really did run a hundred times");
        sound(&func, &mut names);
    }

    /// The same loop written to leave when its test holds, which is the other predicate.
    #[test]
    fn a_loop_that_leaves_when_its_test_holds_gets_the_comparison_the_other_way_round() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted_leaving(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let addr = element(&mut build, base, it.counter, 0);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);
        let before = stores(&func);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Optimized, RETARGETED), 1);
        assert_eq!(leaves_on(&func, it.head), IntPred::Eq, "it leaves when the pointer lands");
        assert_eq!(stores(&func), before);
        assert_eq!(before.len(), 100);
        sound(&func, &mut names);
    }

    /// The off-by-one, which is the whole reason the limit is worked out from a chrec.
    ///
    /// A bottom-tested loop compares a value one step further along than the one its body used,
    /// so its trip count is one less than the number of times the body ran. Reading that count as
    /// a number of turns through the body would put the limit one element short and lose the last
    /// write, and this is the test that says it does not.
    #[test]
    fn a_loop_that_tests_at_the_bottom_gets_a_limit_that_is_one_further_on() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = bottom(&mut func, entry);

        let mut build = Builder::new(&mut func, it.head);
        let addr = element(&mut build, base, it.counter, 0);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        bottom_close(&mut func, &it, 7);
        Builder::new(&mut func, it.out).ret(&[]);
        let before = stores(&func);
        let want: Vec<i128> = (0..7).map(|turn| turn * STRIDE).collect();
        assert_eq!(before, want, "seven turns, and the last one counts");

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1);
        assert_eq!(stats.count(Kind::Optimized, RETARGETED), 1);
        assert_eq!(stores(&func), before, "all seven, not six and not eight");
        sound(&func, &mut names);
    }

    /// Section 28.4's condition: the rewrite is only worth it when nothing else wants the counter.
    #[test]
    fn a_loop_that_still_wants_its_counter_keeps_both_it_and_the_test_it_is_in() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let addr = element(&mut build, base, it.counter, 0);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        // The counter itself written somewhere, which is a use of it that no pointer can serve.
        build.store(it.counter, base, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);
        let before = stores(&func);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Note, USE_GENERIC), 1, "the counter, written out");
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1, "the walk is still worth making");
        assert_eq!(stats.count(Kind::Optimized, RETARGETED), 0);
        assert_eq!(stats.count(Kind::Missed, COUNTER_WANTED), 1);
        assert_eq!(leaves_on(&func, it.head), IntPred::Slt, "the test is the one it arrived as");
        assert_eq!(stores(&func), before);
        sound(&func, &mut names);
    }

    /// A second way out is a second turn count, and only one of them was measured.
    #[test]
    fn a_loop_with_two_ways_out_keeps_the_test_it_has() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);
        let more = func.create_block();
        let carried = func.append_param(more, Type::int(64));

        let mut build = Builder::new(&mut func, it.body);
        let seen = build.load(Type::int(32), base, plain(), Flags::NONE);
        let zero = build.iconst(Type::int(32), 0);
        let done = build.icmp(IntPred::Slt, seen, zero);
        build.br_if(done, it.out, &[], more, &[it.counter]);

        let mut build = Builder::new(&mut func, more);
        let addr = element(&mut build, base, carried, 0);
        let nothing = build.iconst(Type::int(32), 0);
        build.store(nothing, addr, plain(), Flags::NONE);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, carried, one, Flags::NSW);
        build.jump(it.head, &[next]);
        Builder::new(&mut func, it.out).ret(&[]);
        let before = stores(&func);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1);
        assert_eq!(stats.count(Kind::Optimized, RETARGETED), 0);
        assert_eq!(stats.count(Kind::Missed, MANY_EXITS), 1);
        assert_eq!(stores(&func), before);
        sound(&func, &mut names);
    }

    /// A test that is not asked on every turn, which is the condition `!=` needs and `<` does not.
    ///
    /// The loop can come round without going through the block the test is in, so a test that
    /// refuses on exactly one turn is not the same test as one that refuses from a turn onwards.
    #[test]
    fn a_test_the_loop_can_get_past_without_asking_is_left_where_it_is() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let head = func.create_block();
        let check = func.create_block();
        let body = func.create_block();
        let out = func.create_block();
        let counter = func.append_param(head, Type::int(64));

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(64), 0);
        build.jump(head, &[zero]);

        // Whether the test is even asked comes out of memory, so nothing here knows the answer.
        let mut build = Builder::new(&mut func, head);
        let seen = build.load(Type::int(32), base, plain(), Flags::NONE);
        let none = build.iconst(Type::int(32), 0);
        let ask = build.icmp(IntPred::Slt, seen, none);
        build.br_if(ask, check, &[], body, &[]);

        let mut build = Builder::new(&mut func, check);
        let stop = build.iconst(Type::int(64), 100);
        let test = build.icmp(IntPred::Slt, counter, stop);
        build.br_if(test, body, &[], out, &[]);

        let mut build = Builder::new(&mut func, body);
        let addr = element(&mut build, base, counter, 0);
        let nothing = build.iconst(Type::int(32), 0);
        build.store(nothing, addr, plain(), Flags::NONE);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        build.jump(head, &[next]);
        Builder::new(&mut func, out).ret(&[]);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1);
        assert_eq!(stats.count(Kind::Optimized, RETARGETED), 0);
        assert_eq!(stats.count(Kind::Missed, NOT_EVERY_TURN), 1);
        assert_eq!(leaves_on(&func, check), IntPred::Slt);
        sound(&func, &mut names);
    }

    /// The two rewrites are fuelled apart, so a bisection can land between them.
    #[test]
    fn one_unit_of_fuel_buys_the_walk_and_not_the_test() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let addr = element(&mut build, base, it.counter, 0);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);
        let before = stores(&func);

        let mut an = crate::machine::fixtures::analyses();
        let stats = Ivopts.run(&mut func, &mut an, &mut Fuel::of(1));
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1);
        assert_eq!(stats.count(Kind::Optimized, RETARGETED), 0);
        assert_eq!(stats.count(Kind::Missed, OUT_OF_FUEL), 1);
        assert_eq!(leaves_on(&func, it.head), IntPred::Slt, "the counter is still what is tested");
        assert_eq!(stores(&func), before);
        sound(&func, &mut names);
    }

    /// A walk so long that the address at the end of it is not a number an addition can take.
    ///
    /// The limit is the step multiplied by the turn count, and the argument that the addresses
    /// along the way are all different rests on that product fitting in a signed sixty four bit
    /// number. A loop this long is not one anybody runs, and refusing it is a line rather than a
    /// judgement call.
    #[test]
    fn a_walk_too_long_to_measure_leaves_the_test_alone() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, i128::from(i64::MAX) / 2);

        let mut build = Builder::new(&mut func, it.body);
        let addr = element(&mut build, base, it.counter, 0);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Optimized, ADDED), 1);
        assert_eq!(stats.count(Kind::Optimized, RETARGETED), 0);
        assert_eq!(stats.count(Kind::Missed, LIMIT_TOO_FAR), 1);
        sound(&func, &mut names);
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

    /// The array of `int` the rest of this module deliberately does not walk.
    ///
    /// Every loop above steps by [`STRIDE`], which is a width no addressing mode holds, and every
    /// one of them comes out with a pointer. This is the same loop over four byte elements, and it
    /// has to come out the other way. `(%rax,%rcx,4)` is a mode x86-64 has, so the counter is
    /// already the index of it and the only thing a pointer of its own would remove is a scale the
    /// hardware applies for free. The loop keeps what it was written with.
    ///
    /// This is the test that has to fail for the pass to go back to what it did before #918, where
    /// an address group could only ever be served by a pointer made for it and the cost model was
    /// never asked. Getting it wrong costs about a percent of `.text` across the corpus, which is
    /// what kept the pass out of every pipeline until now.
    #[test]
    fn a_walk_the_modes_hold_keeps_the_counter_it_has() {
        let mut names = Interner::new();
        let (mut func, entry, base) = shell(&mut names);
        let it = counted(&mut func, entry, 100);

        let mut build = Builder::new(&mut func, it.body);
        let addr = strided(&mut build, base, it.counter, 0, 4);
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, plain(), Flags::NONE);
        close(&mut func, &it, it.body);
        Builder::new(&mut func, it.out).ret(&[]);
        let before = params(&func, it.head);

        let stats = choose(&mut func);
        assert_eq!(stats.count(Kind::Note, USE_ADDRESS), 1, "the address was looked at");
        assert_eq!(stats.count(Kind::Note, CANDIDATE), 3, "and a pointer for it was costed");
        assert_eq!(stats.count(Kind::Note, KEPT), 1, "and it lost to the counter");
        assert_eq!(stats.count(Kind::Optimized, ADDED), 0);
        assert_eq!(params(&func, it.head), before, "nothing new goes round the loop");
        assert!(!stats.changed());
    }
}
