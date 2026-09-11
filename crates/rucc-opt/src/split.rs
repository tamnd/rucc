//! Splits a loop into a run of iterations that needs no checks and the rest of it, which keeps them.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.4, which names this and says what it
//! is for: "Loop splitting is the general form. The checked part and the unchecked part are divided
//! at `min(n, extent / sizeof(T))`."
//!
//! [`crate::hoist`] is the pass next door and it answers a different question. It puts one check in
//! front of a loop that covers every access the loop makes, which needs the loop to make every one
//! of them: an exact count, one way out, and a check every iteration reaches. Most loops in real code
//! are not like that. The census on tamnd/rucc#782 says that of the roughly fifteen hundred checks
//! SQLite still carries at `-O2`, six hundred and ninety two are in loops with a second way out and
//! sixty four are checks an iteration can finish without reaching. Neither is a loop hoisting can say
//! anything about, and both are loops this one can, because it never has to claim the loop reaches
//! the end of what it might read. It only has to know a prefix that is safe.
//!
//! # What the two halves are
//!
//! The loop is copied. The original becomes the fast half and loses its checks, the copy becomes the
//! slow half and keeps them, and a new block in front of the original decides which one runs. That
//! block carries an offset in bytes from the first access, walks it on by the step every time round,
//! and hands over to the slow half once the offset passes a window worked out in the preheader.
//!
//! The offset is a new value rather than one of the loop's own pointers, and the test is against
//! the window rather than against anything the loop compares. That is what makes this work on a loop
//! with several ways out: the fast half keeps every exit the loop had, so leaving early still leaves
//! early, and the extra test is only ever the reason the fast half stops early and never the reason
//! it runs longer.
//!
//! A loop where no address moves gets neither the block nor the offset. Which half runs is settled
//! by an answer that does not change while the loop runs, so the way into the loop is where the two
//! halves are chosen between and there is nothing to carry. Half the loops this takes on SQLite are
//! that shape.
//!
//! # Where the window comes from
//!
//! For a check whose address is `first + delta` reading `reach` bytes each time, every offset with
//! `delta + reach <= extent` is one the check cannot fail on, where `extent` is how many bytes from
//! `first` on belong to whatever owns `first`. So the window is `extent - reach`, and where a loop
//! has several checks that walk by the same amount the window is the smallest of theirs.
//!
//! Bytes rather than iterations, and that is the whole of the arithmetic. An earlier version of this
//! counted iterations against `(extent - reach) / step + 1`, which is the same transformation and a
//! much harder claim: it has a symbolic multiply and a symbolic divide in it at sixty four bits, and
//! z3 does not finish on it in two and a half minutes in any of three formulations, so the pass sat
//! outside the rule table that `spec/safe-memory/07-check-elimination.md` section 7.7 asks every
//! elimination to be inside. In bytes it is `swept.sym.i64`, which is already in that table and
//! already proved, and the pass asks it rather than deciding. `limited` below is where that
//! happens.
//!
//! The extent is the half of that a compiler cannot work out, so it is asked at run time, through the
//! `cap_extent` query that tamnd/rucc#792 added. The query takes a limit on how far to look and the
//! answer is never more than that limit and never more than the truth, so what this pass asks for is
//! as much as the arithmetic carries. That used to be a trip count times a step, on the grounds that
//! what the query walked was work the loop was about to do anyway. tamnd/rucc#861 stopped it walking
//! and tamnd/rucc#871 took the bound off: the query probes the far end of what it was asked for and
//! halves, so the price does not turn on the number, and a limit smaller than the object is a smaller
//! window and so fewer iterations in the half with no checks in it.
//!
//! An address that does not move is the same expression with a step of zero, and its offset is zero
//! on every iteration, so there is nothing to carry and the window question collapses into whether
//! the one access fits.
//! Hoisting would rather have these, and it takes the ones in loops it is willing to touch. What is
//! left over is the ones in loops it refused for one of its own reasons, a second way out or a call
//! inside, and those come back here.
//!
//! # A walk that goes the other way
//!
//! A loop whose address goes down each time round is the same transformation looked at from the
//! other end, and it is written here so that it is the same code. The offset the guard carries
//! counts bytes moved from the first access rather than bytes added to it, so it still goes up by
//! the step every time round and everything built on it is untouched: the guard block, the block
//! parameter, the clamp and the test are the ones above, word for word.
//!
//! What changes is which end of the object the runtime is asked about. The window has to be room
//! below the first access rather than above it, so the query is `cap_extent_back` and it is asked at
//! `first + reach`, the end of the first access rather than its start. The answer is how many bytes
//! ending there belong to whatever owns them, the window is that less the reach as before, and the
//! access on iteration `delta` is the `reach` bytes ending at `first + reach - delta`. That is what
//! `swept.down.sym.i64` in the rule table is written about, and it is asked instead of the ascending
//! rule rather than derived from it.
//!
//! Anchoring at the end is what buys all of that. Anchoring at the lowest address the loop reaches
//! would need a real trip count, since where the verified range starts would then depend on how far
//! the loop goes, and this pass takes loops nobody counted. Not knowing is free for an ascending
//! walk, where asking for too little only costs iterations in the slow half. It is unsound for a
//! descending one, so the query goes the other way instead of the anchor.
//!
//! # A walk nobody could follow
//!
//! Everything above assumes the pass knows how far the address moves each time round. Most of what
//! is left on real code is loops where it does not, and they are not exotic: a scanner that steps by
//! one or by two depending on what it just read, a pointer that comes back round through a join
//! because the body has a branch in it, a walk whose step is a width the caller passed in. None of
//! those is an induction variable and scalar evolution has nothing to say about any of them, so they
//! arrive here as an address that does something unknown.
//!
//! The way through is to stop asking how far the address moves and ask instead where it is. If the
//! check's address is a fixed distance from a pointer the loop's header carries, then the guard can
//! take where that pointer was on the way in from where it is now, and the difference is the
//! displacement itself. It is exact rather than an upper bound on it, so the same window and the same
//! rule apply word for word, and the guard tests it with the same unsigned comparison. It costs a
//! subtract in the guard and saves the block parameter and the add at the latch, so it is not more
//! code than counting.
//!
//! What has to be established is that the pointer is its own former self plus bytes, and the reason
//! is money rather than soundness. The guard compares the difference against the window at run time,
//! so `p = p->next` is safe to measure: a node that landed inside the first one's object passes the
//! comparison and one that did not takes the slow half, and either way the answer is right. It is
//! that a list never passes. The next node of a heap allocated list is its own object, so the guard
//! fails on the second iteration and every one after it, and the split bought a second copy of the
//! loop with every check still in both halves. Letting lists through on SQLite splits 73 more loops,
//! puts 220 more calls to `check_bounds` in the object and adds 139 kilobytes, for 5 liveness checks.
//! So the value the latch hands back has to reach the parameter through `ptr_add`s, block parameters
//! inside the loop and `select`, and a load anywhere on the way is a refusal. `measured` is where
//! that walk is, and it is syntactic because what it is buying is.
//!
//! A fixed distance from a pointer the header carries is not the only address the guard can find its
//! way to, and on real code it is not even the commonest. The one above it is that pointer plus a
//! variable, which is an address that is still a function of what the header carries and of what the
//! loop was handed, and both the guard and the preheader hold every one of those. So the guard writes
//! the arithmetic out again from its own parameters, the preheader writes it out again from the
//! values it passes, and the subtraction between the two is the same subtraction. That is
//! rematerialization rather than measurement, `writable` is where it is decided and `remade` is where
//! it is written, and the fixed distance case is the instance of it that costs nothing to write.
//!
//! What may be written again is a list of opcodes rather than a question about effects, because two
//! things have to hold and neither is what an effect flag answers. The copy has to compute the same
//! number somewhere else, which is what rules out reading memory, and it has to be harmless in the
//! preheader of a loop that turns out to run no iterations, which is what rules out a divide.
//!
//! # Why the fast half may drop a check
//!
//! `check_bounds` asks whether the bytes an access names lie inside one object. Every address in
//! `[first, first + extent)` is inside the object that owns `first`, by what the query answers, and
//! the window is exactly the offsets whose access stays inside that. So no check in the fast half
//! could have failed.
//!
//! `check_live` asks whether anything owns the address right now, and the query answered that too,
//! since a byte belonging to the owner of `first` is a byte with an owner. Right now is the catch,
//! and it is why nothing that could free may be in the loop. A call in the body could free the object
//! between the question and the iteration that reads it, and then the fast half would read freed
//! storage with nothing to say so.
//!
//! That is a question about the callee rather than about calling, and [`crate::nofree`] answers it
//! before the pipeline starts, so a call carrying [`rucc_ir::Flags::NOFREE`] is one the loop may
//! keep. Hoisting refuses every call whatever it does, and the reason is not this one: it needs the
//! loop to reach the end of what its count says, and a call that does not come back leaves it short.
//! Splitting never claims the loop reaches the end, so a call that might not come back costs it
//! nothing.
//!
//! `check_deriv` asks whether a pointer computed from another one stayed inside the capability the
//! first one had, and that is the same containment written about a pointer rather than about the
//! bytes under it. It is the narrower question of the two, since the window document 03 section 3.1
//! allows a derivation runs a stride below the object and up to its end, and the fast half is only
//! ever claiming the address is inside. So a loop whose bounds check the window covers has a
//! derivation check the same window covers, and on the two benchmarks where an index walks a byte
//! at a time that check was all the fast half had left in it.
//!
//! What it needs beyond a walk is that the extent was asked about the object the check names. The
//! query goes to the first iteration's address, so an address a little way along from the pointer
//! the check is about is a question about whatever owns that instead, which past the end of one
//! object is the next object rather than nothing. Two shapes give the right object and `started` and
//! `paired` are the two. Either the walk starts on the pointer the check names, or that pointer
//! walks the loop alongside the new one, in which case the two are a fixed distance apart on every
//! iteration and a window that wide holds the pair: the lower end being inside the object says the
//! capability is that object and the upper end being inside it says the derivation stayed there.
//!
//! The second is the commoner by a long way, because `p = p + k` is what most pointer arithmetic in
//! a loop is, and it is what `bench/safety/a-string-scan` does.
//!
//! Two answers of the query carry the weight and both are argued where the query is implemented. An
//! address no watched region covers gets the whole limit back, so a loop over a local or a global
//! splits into a fast half that runs the whole way, which is right because no check on such an
//! address ever fires under this milestone. An address whose granule nobody owns gets zero, so the
//! limit is zero, the fast half runs no iterations, and the check inside the slow half is what reports
//! the dangling pointer, at the access rather than at the loop.
//!
//! # Which loops
//!
//! One latch, a preheader, nothing in it that could free, and no value defined inside it that
//! anything outside reads. Not a count, unlike hoisting, and not even a step: the count was spent on
//! how far to ask the runtime to look and nothing asks for less than everything any more, and the
//! step was spent on the same thing. The last is loop closed form, which [`crate::canon`]
//! establishes, and it is checked rather than assumed because the copy would otherwise leave a reader
//! outside the loop seeing whichever half happened to define the value.
//!
//! Canonicalization runs a long way in front of this, and `simplify-cfg` between the two undoes some
//! of what it did, so on SQLite the closed form condition once refused 351 of the checks this would
//! otherwise have taken out. Running canonicalization again in front of this gets 156 of them back
//! and costs 17672 bytes of `.text`, which is a bad trade for eleven more checks, so the answer is
//! that this repairs the one loop it is splitting rather than the pipeline repairing every loop in
//! the function. `repaired` is that, and with the repair reaching the joins the exits meet at as
//! well as the exits themselves the condition now refuses none of them.
//!
//! What the repair cannot help with is a name the pass is about to write and has not written yet. A
//! guard is worked out from values the loop was handed, and where the loop before it is one this is
//! also splitting, a value that loop defines stops being one value the moment it has two halves.
//! Those loops are refused, and there are five of them on SQLite against the two hundred and fifty
//! the repair finishes.
//!
//! A loop with a loop inside it is not refused, and there is nothing about an inner loop that would
//! make the copy wrong: the copier takes any set of blocks and the guard goes in front of the outer
//! header either way. What the outer guard cannot speak for is a check inside the inner loop, since
//! it measures where the outer walk has got to at the top of an outer iteration and the inner loop
//! runs its whole way inside that iteration. Those checks stay in both halves and the inner loop's
//! own split is what takes them, so what an outer split is worth is the checks in the outer loop's
//! own blocks. On SQLite that is most of what is there: of the 169 nests the pass used to refuse
//! outright, 162 have a check in the outer loop's own blocks and 113 have more than six.
//!
//! Where a nest plans twice the inner plan wins, because the two plans name blocks in common and
//! applying either moves them. The outer one comes back on the next run of the pipeline. The size
//! limit is the one limit, counted over the whole nest, which is what `heuristics::SPLIT_MAX_INSNS`
//! already counts since a loop's block list holds the blocks of the loops inside it. A second and
//! smaller limit was the obvious guess and the measurement says it is not needed: the outer loop's
//! own blocks are over fifty instructions in 115 of those 169, so a nest that fits inside the limit
//! is mostly the outer loop rather than mostly the inner one, and the limit is already pricing the
//! part that pays.
//!
//! Not every check in the loop has to be one this can size. A check whose address the analysis cannot
//! follow simply stays in both halves, and the fast half is then a loop with fewer checks in it rather
//! than none. That is worth having on its own and it is worth having because it is what a real loop
//! looks like: one sweep the analysis reads and one index that came out of a table.
//!
//! # Which level
//!
//! `-O2` and `-O3`, alongside `crate::unroll` and for the same reason. The loop body is copied, so
//! the function grows by about the size of the loop, and buying speed with code is what those levels
//! are for and what `-Os` and `-Oz` are for declining.

use std::collections::{HashMap, HashSet};

use rucc_cost::heuristics;
use rucc_ir::{
    Block, BlockCall, Builder, Def, Extra, Flags, Func, Inst, InstData, IntPred, Opcode, Type,
    Value,
};

use crate::canon;
use crate::cfg::Cfg;
use crate::copy;
use crate::discharge::{Question, constant, operand_of, yes};
use crate::dom::Dominators;
use crate::frontier::Frontiers;
use crate::loops::{LoopId, Loops};
use crate::rules::safety;
use crate::scev::{Anchor, Evolution, Plain, Reading, Scev};
use crate::trip::inst_of;
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// What is reported when a loop is split.
const SPLIT: &str = "loop split, the iterations in front of the first one that could fail a check \
                     run without them";

/// What is reported when a loop had to be put back into closed form before it could be split.
const CLOSED_HERE: &str = "loop put back into closed form, a value it defines is read after it and both halves define one";

/// What is reported when the pass ran out of fuel with a loop it was about to split.
const NO_FUEL: &str = "loop left alone, the pass ran out of fuel";

/// What is reported for a loop with nowhere to work the limit out.
const NO_PREHEADER: &str = "loop left alone, it has no block in front of it to put a check in";

/// What is reported for a loop whose blocks another loop being split here has already taken.
const NESTED_WITH_ONE: &str = "loop left alone, a loop inside it is being split here instead";

/// What is reported for a check that is not in the blocks of the loop being split.
const INSIDE_A_LOOP: &str =
    "check kept in both halves, it is in a loop inside the one being split and moves with that one";

/// What is reported for a loop with more than one way round.
const MANY_LATCHES: &str = "loop left alone, it goes back to its header from more than one place";

/// What is reported for a loop with a call in it that could free.
const A_CALL_INSIDE: &str = "loop left alone, a call in it might free what the loop is reading";

/// What is reported for a loop holding something that ends a lifetime outright.
const ENDS_A_LIFETIME: &str = "loop left alone, something in it ends a lifetime";

/// What is reported for a loop holding something the copier cannot copy.
const NOT_COPYABLE: &str = "loop left alone, something in it carries a side table this cannot copy";

/// What is reported for a loop whose values are read after it without going through a parameter.
const ESCAPES: &str = "loop left alone, a value it defines is read outside it";

/// What is reported for a loop another loop's guard is about to name a value of.
const WANTED_ELSEWHERE: &str =
    "loop left alone, the guard of another loop being split here names a value it defines";

/// What is reported for a loop whose two halves would be too much code.
const TOO_BIG: &str = "loop left alone, the two halves would be more code than the limit allows";

/// What is reported for a check whose address does not walk the loop.
const NOT_A_SWEEP: &str = "check kept in both halves, its address does not walk the loop by a \
                           constant";

/// What is reported for a check whose address the analysis has nothing to say about.
const NOT_FOLLOWED: &str = "check kept in both halves, what its address does round the loop is not \
                            something the analysis follows";

/// What is reported for a check whose step does not keep its alignment.
const MISALIGNED: &str =
    "check kept in both halves, its step is not a whole number of its alignment";

/// What is reported for a check the guard would have to measure, whose access wants an alignment
/// nothing here can promise.
const MEASURED_ALIGN: &str = "check kept in both halves, the guard would measure how far its \
                              address moved and that is no answer about its alignment";

/// What is reported for a check that already covers a range the program worked out.
const ALREADY_COMPUTED: &str =
    "check kept in both halves, how many bytes it covers is a number only the program has";

/// What is reported for a derivation check whose walk does not start on the pointer it is about.
const NOT_FROM_THE_START: &str = "derivation check kept in both halves, the walk starts along from \
                                  the pointer the check is about rather than on it";

/// What is reported for a check the rule table will not say yes about.
const NOT_PROVED: &str = "check kept in both halves, no rule in the safety namespace says an offset inside the window is \
     an access inside the object";

/// The pass.
#[derive(Debug)]
pub struct Split;

impl Pass for Split {
    fn name(&self) -> &'static str {
        "split"
    }

    fn describe(&self) -> &'static str {
        "a loop becomes a run of iterations with no checks in it and the rest of the loop with them"
    }

    fn preserves(&self) -> Preserved {
        // Blocks appear and edges move, so nothing built on the graph stands.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let cfg = an.cfg(func).clone();
        let loops = an.loops(func).clone();
        if loops.count() == 0 {
            return stats;
        }

        // Worked out first and applied afterwards, because scalar evolution reads the function and
        // the transformation writes it. No two plans share a block, which `planned` sees to, so
        // applying one leaves every other one's blocks where they were.
        let mut plans = planned(func, &cfg, &loops, &mut stats);

        // Closed form put back where it is missing, before anything is copied. The repair adds a
        // block parameter and rewrites uses, so it moves no edge and creates no block, which is why
        // the graph and the loop forest above are both still good after it. What it does move is
        // which value a use inside another loop names, and a plan is a list of values, so a repair
        // means the plans are worked out again rather than trusted. The stats go with them, or the
        // first round's reasons would be counted twice.
        let dom = an.dominators(func).clone();
        let fronts = an.frontiers(func).clone();
        let repairs = repaired(func, &dom, &fronts, &loops, &plans, fuel);
        if repairs.made > 0 {
            stats = Stats::new();
            plans = planned(func, &cfg, &loops, &mut stats);
            for _ in 0..repairs.worked {
                stats.optimized(CLOSED_HERE);
            }
        }
        // A guard names values, and until it is written those uses are in the plans rather than in
        // the function, so the walk that looks for a value read outside the loop cannot see them.
        // They are collected here and the loops they belong to are refused, because a loop that is
        // split stops having one value where another loop's guard expects to find one.
        let named: Vec<(LoopId, Value)> = plans
            .iter()
            .flat_map(|plan| mentions(func, plan).into_iter().map(move |value| (plan.id, value)))
            .collect();
        plans.retain(|plan| {
            if leaving(func, plan) {
                stats.missed(ESCAPES);
                return false;
            }
            if elsewhere(func, plan, &named) {
                stats.missed(WANTED_ELSEWHERE);
                return false;
            }
            true
        });

        let mut changed = false;
        for plan in plans {
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            apply(func, &plan);
            stats.optimized(SPLIT);
            changed = true;
        }
        if changed {
            an.clear();
        }
        stats
    }
}

/// How far past the first access an iteration reads, and who works that out.
///
/// Both are the same number and they differ in who does the arithmetic. `By` is a walk the analysis
/// read, so the guard counts: it carries a byte offset of its own, starts it at zero on the way in
/// and adds the step every time round. `Of` is a walk the analysis could not read, whose address is
/// instead a fixed distance from a pointer the loop's header carries, so the guard measures: it
/// takes where that pointer was on the way in from where it is now, and the difference is the
/// displacement itself rather than a count standing in for it.
///
/// Measuring is what reaches a pointer that moves by an amount nobody wrote down, or by a different
/// amount down each arm of a branch, or that comes back round through a join. None of those is an
/// induction variable and there is nothing for scalar evolution to say about any of them, and
/// between them they are 286 of the checks loop splitting still leaves in place on the SQLite
/// amalgamation, at 44 sites. See tamnd/rucc#810.
///
/// The offset a measured walk produces is exact rather than an upper bound, which is what keeps this
/// inside the rule table. `swept.sym.i64` is asked about it word for word as it is asked about a
/// counted one, because `(p + k) - (first + k)` is `p - first` for whatever fixed `k` the check sits
/// at, so the difference the guard computes is the displacement the rule is written about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Walk {
    /// The address moves this many bytes every time round, either way. Zero is an address that does
    /// not move, which is allowed and puts no limit on the loop. Negative is a walk from high to
    /// low, and what changes for one is which end of the object the runtime is asked about rather
    /// than anything about how the two halves are built.
    By(i128),
    /// The address is a fixed distance from a value the guard can work out for itself, out of the
    /// parameters the header carries and the values the loop was handed. The loop moves it on by an
    /// amount the analysis did not read, so where it is gets measured rather than counted.
    Again {
        /// The value to work out again, which is the check's address with the constant `ptr_add`s
        /// on the front of it taken off. A parameter of the header is the commonest one and costs
        /// nothing to work out, since the guard already carries it.
        at: Value,
    },
}

impl Walk {
    /// Whether the address stays where it is, which is a loop that needs no guard at all.
    fn still(self) -> bool {
        self == Self::By(0)
    }

    /// Whether the address walks from high to low, which asks the runtime about the other end of
    /// the object.
    ///
    /// A measured walk never does. The guard's subtraction is read unsigned, so a pointer that went
    /// below where it started is an enormous displacement and the guard hands the loop to the half
    /// that kept its checks, which is the answer that end of the object would have given anyway.
    fn down(self) -> bool {
        matches!(self, Self::By(step) if step < 0)
    }

    /// Which offset this walk shares with the others in the loop.
    fn key(self) -> Key {
        match self {
            Self::By(step) => Key::Every(step.abs()),
            Self::Again { at, .. } => Key::From(at),
        }
    }
}

/// Which checks are at the same offset from their own first access on every iteration, and so can
/// share one offset in the guard and the smaller of their windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    /// They walk by the same number of bytes each time round, whichever way each of them goes.
    Every(i128),
    /// They are measured from the same value, which the guard works out again for itself. Two
    /// checks a fixed distance from one pointer are the same distance apart on every iteration,
    /// whatever the pointer does, so one subtraction answers for both, and one copy of whatever
    /// arithmetic the pointer took answers for both as well.
    From(Value),
}

/// One check the fast half will not need, and the walk that says so.
#[derive(Debug)]
struct Sweep {
    /// The check itself, which is removed from the fast half and kept in the copy.
    check: Inst,
    /// Where the first iteration's address is computed from. An address rather than a value when
    /// it is a global, since nothing outside the loop computes one of those. See [`Anchor`].
    base: Anchor,
    /// How far past that value the first iteration reads, in bytes. Usually a number, and a value
    /// and a scale beside it when the loop started its counter at something it was handed. See
    /// `spare` for how it is built and #810 for what it is worth.
    apart: Plain,
    /// What the address does round the loop, and so what the guard has to work out.
    walk: Walk,
    /// Everything inside the loop that has to be written again for the guard to have the address,
    /// operands before uses. Empty for a counted walk and for a measured one off a parameter the
    /// header already carries, which is most of them. See [`writable`].
    rebuild: Vec<Value>,
    /// How many bytes one access covers.
    reach: i128,
    /// How far past the window's first byte the walk's first access sits, when that is a distance
    /// the loop works out rather than one written here. Nothing on almost every sweep, because the
    /// two are the same address. See [`trailing`].
    ahead: Option<Plain>,
}

/// One loop to split, worked out before anything is written.
#[derive(Debug)]
struct Plan {
    /// The loop itself, which is read again when its closed form has to be repaired.
    id: LoopId,
    /// Where the limit is worked out.
    preheader: Block,
    /// The block the guard takes over from.
    header: Block,
    /// The block the back edge leaves from, which is where the iteration count goes up.
    latch: Block,
    /// Everything that is copied, which is the whole loop.
    body: Vec<Block>,
    /// The checks the fast half will not need, which is never empty in a plan.
    sweeps: Vec<Sweep>,
}

/// Plans a loop, or counts what stopped it.
///
/// Nothing is reported for a loop with no check in it, because a loop that does no memory access is
/// not a missed opportunity and a report for every one of them would bury the loops that are.
fn sweep(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    scev: &mut Scev<'_>,
    id: LoopId,
    plans: &mut Vec<Plan>,
    stats: &mut Stats,
) {
    let body = loops.blocks(id).to_vec();
    let checks: Vec<Inst> = body
        .iter()
        .flat_map(|&block| func.insts(block).collect::<Vec<Inst>>())
        .filter(|&inst| {
            matches!(
                func[inst].opcode,
                Opcode::CheckBounds | Opcode::CheckLive | Opcode::CheckDeriv
            )
        })
        .collect();
    if checks.is_empty() {
        return;
    }

    let (preheader, latch) = match shaped(func, cfg, loops, id, &body) {
        Ok(shape) => shape,
        Err(why) => {
            stats.missed(why);
            return;
        }
    };
    let mut sweeps = Vec::new();
    for check in checks {
        // A check in a loop inside this one runs many times for each time round this one, at an
        // address that moves with the inner loop rather than with this one. The guard here measures
        // where this loop's walk has got to at the top of an iteration, and that says nothing about
        // how far the inner loop goes before the iteration is over, so the check stays in both
        // halves. What takes it is the inner loop's own split, which is a plan of its own.
        if func.block_of(check).is_none_or(|block| loops.innermost(block) != Some(id)) {
            stats.missed(INSIDE_A_LOOP);
            continue;
        }
        match walked(func, cfg, loops, scev, id, latch, check) {
            Ok(sweep) => sweeps.push(sweep),
            Err(why) => stats.missed(why),
        }
    }
    if sweeps.is_empty() {
        return;
    }
    plans.push(Plan { id, preheader, header: loops.header(id), latch, body, sweeps });
}

/// The preheader and the latch of a loop this pass may copy, or why there is not one.
///
/// The conditions are the module comment's. The one worth restating is freeing, because it is the
/// only one that is about what the fast half is allowed to leave out rather than about whether the
/// copy can be made at all: the extent is asked once before the loop and believed for the whole of
/// the fast half, so anything that could hand the storage back in the middle would make the answer
/// stale, and the fast half has nothing left in it to notice.
///
/// Which is a question about the callee and not about calling, so it is asked of the callee.
/// [`crate::nofree`] settles it before the pipeline starts and writes the answer onto the call site,
/// and a call carrying it reaches nothing that ends a lifetime. Note that this is a weaker
/// requirement than [`crate::hoist`]'s, which refuses every call whatever it does, because hoisting
/// needs the loop to reach the end of what its count says and a call that does not come back leaves
/// it short. Splitting never claims that, so coming back is not something it needs.
fn shaped(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    id: LoopId,
    body: &[Block],
) -> Result<(Block, Block), &'static str> {
    let Some(preheader) = loops.preheader(cfg, id) else {
        return Err(NO_PREHEADER);
    };
    let [latch] = loops.latches(id) else {
        return Err(MANY_LATCHES);
    };
    for &block in body {
        for inst in func.insts(block) {
            match func[inst].opcode {
                Opcode::Call | Opcode::CallIndirect | Opcode::TailCall
                    if !func[inst].flags.contains(Flags::NOFREE) =>
                {
                    return Err(A_CALL_INSIDE);
                }
                // Assembly could do anything and the two meta instructions end a lifetime by
                // definition, which is the same answer `crate::nofree` gives for all three.
                Opcode::InlineAsm | Opcode::MetaEnd | Opcode::MetaTransfer => {
                    return Err(ENDS_A_LIFETIME);
                }
                _ => {}
            }
            if !copy::copyable(func, inst) {
                return Err(NOT_COPYABLE);
            }
        }
    }
    let size = body.iter().map(|&block| func.insts(block).count()).sum::<usize>();
    if size > heuristics::SPLIT_MAX_INSNS as usize {
        return Err(TOO_BIG);
    }
    Ok((preheader, *latch))
}

/// Every loop in the function that is worth copying, and why each of the others is not.
///
/// A nest can plan twice, once for the inner loop and once for the outer one, and the two plans name
/// blocks in common. Applying either of them moves those blocks, so only one may run, and the one
/// kept is the inner one. That is not a coin toss: the outer plan takes checks out of the outer
/// loop's own blocks, which run once per outer iteration, while the inner plan takes checks out of
/// blocks that run once per inner iteration, and the inner loop is also the smaller thing to copy.
/// The outer loop is left for the next run of the pipeline, when the inner one is already split.
fn planned(func: &Func, cfg: &Cfg, loops: &Loops, stats: &mut Stats) -> Vec<Plan> {
    let mut plans = Vec::new();
    let mut scev = Scev::new(func, cfg, loops);
    for id in loops.all() {
        sweep(func, cfg, loops, &mut scev, id, &mut plans, stats);
    }
    plans.sort_by_key(|plan| std::cmp::Reverse(loops.depth(plan.id)));
    let mut taken: HashSet<Block> = HashSet::new();
    plans.retain(|plan| {
        if plan.body.iter().any(|block| taken.contains(block)) {
            stats.missed(NESTED_WITH_ONE);
            return false;
        }
        taken.extend(plan.body.iter().copied());
        true
    });
    plans
}

/// How many loops the closed form repair touched, and how many of those it finished.
///
/// Two numbers rather than one because they answer different questions. Anything touched at all is
/// why the plans have to be worked out again, and only the ones it finished are loops that can now
/// be copied and so are what gets reported.
struct Repairs {
    /// Loops the repair wrote something into.
    made: usize,
    /// Loops that are in closed form afterwards.
    worked: usize,
}

/// Puts the loops that need it back into closed form, before anything is copied.
///
/// [`crate::canon`] establishes closed form a long way in front of this pass and `simplify-cfg`
/// between the two undoes some of what it did. Running the whole of canonicalization again was
/// measured and it costs 17672 bytes of `.text` on the SQLite amalgamation, because it repairs every
/// loop in the function rather than the ones about to be copied. This repairs those, which costs
/// nothing on a function with no loop to split.
///
/// A value read past a join that no single exit dominates gets a parameter at the join as well as
/// at each exit, which is what the iterated dominance frontier in [`canon::leaked`] is for. What is
/// still not repaired is a use the placements do not dominate at all, so the count of what worked
/// is a second look rather than an assumption that the first one did.
fn repaired(
    func: &mut Func,
    dom: &Dominators,
    fronts: &Frontiers,
    loops: &Loops,
    plans: &[Plan],
    fuel: &mut Fuel,
) -> Repairs {
    let mut repairs = Repairs { made: 0, worked: 0 };
    for plan in plans {
        if !leaving(func, plan) {
            continue;
        }
        let mut wrote = false;
        while let Some(job) = canon::leaked(func, dom, fronts, loops, plan.id) {
            if !fuel.take() {
                break;
            }
            canon::close(func, dom, loops, &job);
            wrote = true;
        }
        if !wrote {
            continue;
        }
        repairs.made += 1;
        if !leaving(func, plan) {
            repairs.worked += 1;
        }
    }
    repairs
}

/// Whether anything after this loop reads a value its body defines.
fn leaving(func: &Func, plan: &Plan) -> bool {
    let inside: HashSet<Block> = plan.body.iter().copied().collect();
    escapes(func, &plan.body, &inside)
}

/// Every value a plan's guard will name, which is a use that is not in the function yet.
///
/// The guard runs in front of the loop and works out where its first access is, so what it names is
/// whatever those addresses were built on. Where a check's address has to
/// be written again there is arithmetic to copy as well, and the values that arithmetic rests on are
/// the operands of the instructions being copied, since [`remade`] rewrites the header's parameters
/// and leaves everything else naming what it named inside the loop.
fn mentions(func: &Func, plan: &Plan) -> Vec<Value> {
    let mut found = Vec::new();
    for sweep in &plan.sweeps {
        found.extend(sweep.base.value());
        found.extend(sweep.apart.value);
        if let Walk::Again { at, .. } = sweep.walk {
            found.push(at);
        }
        for &value in &sweep.rebuild {
            found.push(value);
            if let Def::Result { inst, .. } = func[value].def {
                found.extend(func[func[inst].args].iter().copied());
            }
        }
    }
    found
}

/// Whether some other loop being split here has a guard that names a value this one's body defines.
///
/// Splitting a loop is what makes such a name wrong. Before it, the value is defined on the one path
/// out of the loop and so is there to be read in front of the next one. After it, there are two
/// paths out and the value on each belongs to its own half, which is the same thing loop closed form
/// is about and is why the repair in front of this pass exists. The repair cannot help here, because
/// the use it would point at a parameter is one the pass has not written down yet.
fn elsewhere(func: &Func, plan: &Plan, named: &[(LoopId, Value)]) -> bool {
    let defined = defines(func, &plan.body);
    named.iter().any(|&(id, value)| id != plan.id && defined.contains(&value))
}

/// Every value the blocks of a loop define, parameters and results alike.
fn defines(func: &Func, body: &[Block]) -> HashSet<Value> {
    let mut defined: HashSet<Value> = HashSet::new();
    for &block in body {
        defined.extend(func[block].params.iter().copied());
        for inst in func.insts(block) {
            defined.extend(func[inst].results());
        }
    }
    defined
}

/// Whether anything outside the loop reads a value defined inside it.
///
/// Where there is one, the two halves would leave it reading whichever of them happened to define
/// it. Closed form is what makes it not one: the use names a parameter of the block the loop leaves
/// to, and each half fills that parameter in on its own way out.
fn escapes(func: &Func, body: &[Block], inside: &HashSet<Block>) -> bool {
    let defined = defines(func, body);
    for block in func.blocks() {
        if inside.contains(&block) {
            continue;
        }
        for inst in func.insts(block) {
            if func[func[inst].args].iter().any(|value| defined.contains(value)) {
                return true;
            }
            for call in func.successors(inst) {
                if func[call.args].iter().any(|value| defined.contains(value)) {
                    return true;
                }
            }
        }
    }
    false
}

/// What one check's address does round the loop, or why the pass cannot say.
///
/// The counted walk is asked for first and the measured one takes what it could not. That order is
/// the cheaper answer first: a counted walk costs the guard an add on a value it already carries,
/// and a measured one costs it a subtraction of two pointers every time round. It is also the more
/// exact answer first, since a counted walk knows the step and so knows the alignment, which a
/// measured one never does.
#[allow(clippy::too_many_arguments)]
fn walked(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    scev: &mut Scev<'_>,
    id: LoopId,
    latch: Block,
    check: Inst,
) -> Result<Sweep, &'static str> {
    // A derivation check names four operands and the pointer that walks is the third of them, since
    // the capability it carries is the old pointer's rather than the new one's. Everything below is
    // written about the address that moves, so the two are pulled apart here and what the shape
    // needs beyond a walk is asked once the walk is known.
    let (capability, source, pointer) = match (func[check].opcode, &func[func[check].args]) {
        (Opcode::CheckDeriv, &[capability, from, to, _stride]) => (capability, Some(from), to),
        (Opcode::CheckDeriv, _) => return Err(NOT_A_SWEEP),
        // A check that already carries its own extent is one hoisting put somewhere, and how many
        // bytes it covers is not a number this pass can divide by a step.
        (_, args) if args.len() > 2 => return Err(ALREADY_COMPUTED),
        (_, &[capability, pointer]) => (capability, None, pointer),
        _ => return Err(NOT_A_SWEEP),
    };
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(source.unwrap_or(pointer)) {
        return Err(NOT_A_SWEEP);
    }
    // A liveness check reads no bytes, so the window it needs is the one byte its address is in.
    // A bounds check carries how many it reads in its payload. A derivation check reads no bytes
    // either, and the byte its address is in is the narrower of the two windows document 03 section
    // 3.1 allows it, so asking for that one is a smaller claim than the judgement needs.
    let (reach, align) = match func[check].extra {
        Extra::Mem(held) => (i128::from(func[held].size), i128::from(func[held].align)),
        _ => (1, 1),
    };

    let (base, apart, walk, rebuild) = match following(func, scev, id, pointer) {
        Ok((base, apart, step)) => (base, apart, Walk::By(step), Vec::new()),
        // The reason the counted walk gave is what gets reported when the measured one cannot take
        // the check either, so that the census keeps saying what the analysis made of the address
        // rather than collapsing every one of them into this fallback missing.
        Err(why) => match measured(func, cfg, loops, id, latch, pointer) {
            Some(found) => found,
            None => return Err(why),
        },
    };
    match walk {
        // An address a whole number of steps along from an aligned one is aligned, which is the
        // whole of what this condition is. It is [`crate::hoist`]'s and it is here for the reason it
        // is there, that a bounds check carries an alignment as well as a byte count.
        Walk::By(step) if step != 0 && step % align != 0 => return Err(MISALIGNED),
        // A measured walk moves by an amount nobody wrote down, so there is no such number to divide
        // and nothing here can say the second access is as aligned as the first. Refusing on the
        // access wanting any alignment at all is the conservative reading, and it is its own line in
        // the census so that what it costs is a number rather than a guess.
        //
        // Refusing looks like bookkeeping about a payload, because `__rucc_check_bounds` takes an
        // address, a size and a descriptor and the alignment never reaches it. It is not. The
        // alignment is a conjunct of J1 in `spec/safe-memory/04-safety-model.md`, it is bug class S7
        // in document 03, and `tests/safety` has three programs for it that are marked as gaps
        // closing on `tamnd/rucc#431`. What that means here is that the field is going to start
        // being read, and a pass that had quietly stopped preserving it in the meantime would be
        // the reason it could not. So the refusal stays and the sixty odd checks it costs are the
        // price of a claim that is still open rather than a mistake to be tidied away.
        Walk::Again { .. } if align > 1 => return Err(MEASURED_ALIGN),
        _ => {}
    }
    // A derivation check asks about the old pointer's capability, and the window is worked out from
    // the extent of whatever owns the first iteration's address, so those two have to be the same
    // object. Either the walk starts on the old pointer, which [`started`] is, or the old pointer
    // walks the loop alongside the new one and one window holds the pair, which [`paired`] is.
    let (apart, reach, ahead) = match source {
        None => (apart, reach, None),
        Some(from) if started(base, apart, from) => (apart, reach, None),
        Some(from) => match paired(func, scev, id, base, apart, walk, from) {
            Some((apart, reach)) => (apart, reach, None),
            None => match trailing(func, scev, id, base, apart, walk, from) {
                Some((apart, ahead)) => (apart, reach, Some(ahead)),
                None => return Err(NOT_FROM_THE_START),
            },
        },
    };
    // Whether an offset inside the window means an access inside the object, which is what dropping
    // this check rests on and is not something this file decides. The direction goes with it,
    // because a walk from high to low is a different claim about addresses and has its own rule.
    if !windowed(reach, walk.down()) {
        return Err(NOT_PROVED);
    }
    Ok(Sweep { check, base, apart, walk, rebuild, reach, ahead })
}

/// Whether the first iteration's address is a given pointer rather than somewhere along from it.
///
/// [`spare`] asks the runtime about the first iteration's address, which is the base plus however
/// far the first access sits past it, so a window says what it is meant to say about a derivation
/// check only when those two are the same address. The condition is the base being the pointer the
/// check names and the displacement being nothing, which together say the walk starts on it.
///
/// A walk that starts a little way along is not rescued by the guard refusing. An address past the
/// end of one object can be inside the next one, and then the extent comes back positive, the
/// window is real, and what it is about is the wrong object. The one thing that does hold is a
/// walk starting on an address nobody owns, which answers zero and sends every iteration to the
/// slow half, and that is not enough on its own.
///
/// [`paired`] is the other way this can hold, and between the two of them they are most of what a
/// derivation check in a loop looks like.
fn started(base: Anchor, apart: Plain, from: Value) -> bool {
    base == Anchor::Value(from) && flat(apart) == Some(0)
}

/// One window that holds the pointer a derivation check is about and where its walk begins.
///
/// `p = p + k` is the commonest derivation there is and [`started`] refuses every one of them,
/// because the pointer the check names is the one moving and the walk therefore starts wherever the
/// loop was handed rather than on it. On SQLite that refusal is 1546 checks against the 146
/// [`started`] takes, and `bench/safety/a-string-scan` is the shape: a cursor stepped a byte at a
/// time, with the derivation check the only thing the fast half still had in it.
///
/// The way through is to stop asking about one address and ask about both. Both have to follow one
/// anchor, so that the distance between them is a number this can work out, and then a window
/// measured from whichever of them is lower and wide enough to cover the gap holds the pair on the
/// first iteration. That is the same claim [`windowed`] already asks about an access that many
/// bytes wide, written about two pointers instead of about the bytes under one, and the pass asks
/// it in exactly that form rather than inventing a second one.
///
/// What it earns is what a derivation check wants. The lower end is inside the object the query was
/// about, so the capability the check names is that object, and the upper end is inside it too, so
/// the pointer computed from it did not leave. Which of the two is the old pointer does not come
/// into it, which is why a step down needs nothing said separately: `p = p - 1` is the same pair a
/// byte apart with the ends the other way round.
///
/// # The two steps this takes
///
/// The old pointer moving by the same step as the new one is the first, and there the distance
/// between the two is the same number on every iteration, so the one window holds the pair wherever
/// the walk has got to.
///
/// The old pointer not moving at all is the second, and there the pair comes apart as the walk goes
/// on. It is still taken, and what makes it sound is that a pointer which does not move only has to
/// be placed once. The first iteration's window holds it, the first iteration is in the fast half
/// whenever anything is, and a window on a later iteration says where the walk has reached. So the
/// two things a derivation check asks are answered by two askings of the one rule rather than by
/// one, and neither of them is arithmetic this file did quietly. On SQLite these are 370 of the
/// refusals against the 55 where the old pointer moves at a step of its own, and that last case is
/// the one that stays refused: a pointer running away at its own rate is not placed by either
/// window.
///
/// The displacements have to be numbers here, since putting the window on the lower of the two and
/// making it as wide as the gap is arithmetic there is no reason to do at run time when the answer
/// is already known. A gap the loop works out is [`trailing`], which puts the window somewhere else
/// and hands the guard the subtraction.
fn paired(
    func: &Func,
    scev: &mut Scev<'_>,
    id: LoopId,
    base: Anchor,
    apart: Plain,
    walk: Walk,
    from: Value,
) -> Option<(Plain, i128)> {
    let Walk::By(step) = walk else { return None };
    let (anchor, behind, along) = following(func, scev, id, from).ok()?;
    if anchor != base || (along != step && along != 0) {
        return None;
    }
    let (near, far) = (flat(behind)?, flat(apart)?);
    let reach = near.abs_diff(far).checked_add(1)?;
    let apart = Plain { value: None, read: None, scale: 0, offset: near.min(far) };
    Some((apart, i128::try_from(reach).ok()?))
}

/// The same window when the gap between the two pointers is a distance the loop works out.
///
/// [`paired`] needs both displacements to be numbers, because it puts the window on the lower of
/// the two and makes it as wide as the difference, and neither of those is arithmetic worth doing
/// where the answer is already known. A subscript computed in an outer loop is not a number. On
/// SQLite that is 103 of the refusals and `bench/safety/a-strided-column-sum.c` is the shape:
/// `grid[row * COLS + col]` walked down the rows, where the pointer the check names is the
/// allocation itself and the walk begins `col` elements into it.
///
/// What is done instead is to put the window on the pointer the check names, which is the object
/// the check is about and so the object the query has to be about, and hand the guard the gap to
/// take off the window it measured. The preheader of the loop being split is where that happens, it
/// is a multiply and a subtract, and the value being multiplied is one the outer loop already
/// worked out.
///
/// Two things have to hold and both are asked rather than assumed. The pointer the check names has
/// to stand still, for the reason [`paired`] gives. And the gap has to come out at or above zero,
/// since a walk beginning below the pointer the window was measured from is a walk into bytes the
/// extent said nothing about. That second one is not a range the analysis reads, it is a comparison
/// the guard makes, and it is the one extra instruction this costs over [`paired`].
///
/// A walk that goes down is left alone. Its window is measured backwards from the end of the first
/// access, so the gap would be a claim about bytes on the other side of the pointer and it is a
/// different argument rather than this one with a sign changed.
fn trailing(
    func: &Func,
    scev: &mut Scev<'_>,
    id: LoopId,
    base: Anchor,
    apart: Plain,
    walk: Walk,
    from: Value,
) -> Option<(Plain, Plain)> {
    if !matches!(walk, Walk::By(_)) || walk.down() {
        return None;
    }
    let (anchor, behind, along) = following(func, scev, id, from).ok()?;
    if anchor != base || along != 0 {
        return None;
    }
    let near = flat(behind)?;
    // Nothing to hand the guard when the walk's own displacement is a number as well, since that is
    // the case [`paired`] took and this would be a worse answer to it.
    apart.value.filter(|_| apart.scale != 0)?;
    let ahead = Plain { offset: apart.offset.checked_sub(near)?, ..apart };
    Some((Plain { value: None, read: None, scale: 0, offset: near }, ahead))
}

/// The displacement as a number, when it is nothing but one.
///
/// [`displacement`]'s test written the other way round: nothing to add is a value that is not there
/// or is not counted, and no number on top of it.
fn flat(apart: Plain) -> Option<i128> {
    apart.value.filter(|_| apart.scale != 0).is_none().then_some(apart.offset)
}

/// The walk scalar evolution read, as a base to measure from and a step in bytes.
///
/// An address that does not move is a sweep with a step of zero, and the arithmetic downstream takes
/// it without a special case anywhere. Hoisting would rather have these, but hoisting only gets the
/// ones in loops it is willing to touch at all, and a loop it refused for one of its own reasons
/// leaves the check where it is. Splitting is willing to touch more loops, so the same check comes
/// back here and there is no reason to hand it back.
fn following(
    func: &Func,
    scev: &mut Scev<'_>,
    id: LoopId,
    pointer: Value,
) -> Result<(Anchor, Plain, i128), &'static str> {
    let (start, step) = match scev.evolution(id, pointer) {
        Evolution::Affine(chrec) => {
            let Some(step) = chrec.step.as_number() else {
                return Err(NOT_A_SWEEP);
            };
            (chrec.base, step)
        }
        Evolution::Invariant(base) => (base, 0),
        _ => return Err(NOT_FOLLOWED),
    };
    // Scale one because the base is an address. Anything else is a multiple of a pointer, which is
    // not a thing the loop computed, so it is a shape this reads rather than a case to handle.
    //
    // The second arm is `a + 8 * start`, an address the loop reached before it began, which is what
    // a counter the caller handed in looks like once the front end has multiplied the element size
    // through it. The pointer is the side the whole thing is measured from and the index is what is
    // scaled beside it, so anything else with two values in it is refused here rather than turned
    // into an address off whichever value came first.
    match (start.plain(), start.on()) {
        (Some(at @ Plain { value: Some(base), read: None, scale: 1, .. }), _) => Ok((
            Anchor::Value(base),
            Plain { value: None, read: None, scale: 0, offset: at.offset },
            step,
        )),
        (_, Some((base, apart))) if walks(func, base, apart) => Ok((base, apart, step)),
        _ => Err(NOT_A_SWEEP),
    }
}

/// The walk the guard can measure, for an address the guard can work out for itself.
///
/// A syntactic walk rather than an analysis, because what it has to establish is syntactic. The
/// address is peeled of the constant `ptr_add`s on the front of it, and what is under them has to be
/// something the guard could write again out of the parameters the header hands it and the values
/// the loop was handed from outside. The first access is then the same expression written in the
/// preheader out of the values the preheader passes, `k` bytes along, and the displacement on any
/// later iteration is the one less the other. That is a subtraction the guard can do, whatever the
/// loop did to the pointer in between.
///
/// The commonest shape by far is the address being a parameter of the header outright, and that
/// costs nothing to write again: the guard already carries the parameter and the preheader already
/// passes it. Everything past that is [`writable`] and [`remade`], which are what make `p + x` for
/// a variable `x` reachable, and `x` is a variable in a third of what is left here.
///
/// # What the back edge has to look like
///
/// The value the latch hands the parameter has to be that same parameter moved: through `ptr_add`s,
/// through parameters of blocks inside the loop, and through a `select`, which is what a branch that
/// moves the pointer differently down each arm turns into. Anything else is refused.
///
/// That question is asked of every pointer the address is built on that the header carries. One the
/// loop was handed from outside does not move at all and so has nothing to answer.
///
/// The refusal is the point of the walk, and not for the reason it looks like. The subtraction is
/// sound whatever the pointer did, because the guard compares the difference against the window at
/// run time: a pointer that landed inside the first one's object passes and one that did not takes
/// the slow half. What the refusal is about is profit. A list is `p = p->next`, where the value on
/// the back edge is a load, and the next node of a heap allocated list is its own object, so the
/// guard fails on the second iteration and every one after it and both halves keep every check.
/// Measured on SQLite, taking lists as well splits 73 more loops, puts 220 more calls to
/// `check_bounds` in the object and adds 139 kilobytes, and removes 5 liveness checks.
fn measured(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    id: LoopId,
    latch: Block,
    pointer: Value,
) -> Option<(Anchor, Plain, Walk, Vec<Value>)> {
    let (at, offset) = peeled(func, pointer);
    if !func[at].ty.is_ptr() {
        return None;
    }
    let mut rebuild = Vec::new();
    let mut leaves = Vec::new();
    let mut seen = HashSet::new();
    if !writable(func, loops, id, at, &mut rebuild, &mut leaves, &mut seen) {
        return None;
    }
    if rebuild.len() > heuristics::SPLIT_REMADE_INSNS {
        return None;
    }
    if !leaves.iter().all(|&leaf| carried(func, cfg, loops, id, latch, leaf)) {
        return None;
    }
    // The base is where the first access is measured from, and it is written in the preheader by
    // `limited` rather than named here, since for anything but a bare parameter no such value exists
    // yet. `Anchor::Value(at)` says which expression to write, and `limited` is where it is written.
    let apart = Plain { value: None, read: None, scale: 0, offset };
    Some((Anchor::Value(at), apart, Walk::Again { at }, rebuild))
}

/// Whether the guard could write the expression that works this address out somewhere else, and in
/// what order.
///
/// The two places it would be written are the guard, out of the parameters the header carries, and
/// the preheader, out of the values the preheader passes the header. So a value stops the walk when
/// both of those already have it, and there are two ways that happens. A value defined outside the
/// loop is the same number wherever it is read, so it is written again by being read again. A
/// parameter of the header is carried by the guard and passed by the preheader, so each of them has
/// its own in hand. Both kinds are leaves, and a pointer leaf is reported to the caller because
/// whether the address is worth measuring turns on what the loop does to it.
///
/// Everything else in the loop has to be an instruction this may write a second copy of. A parameter
/// of a block inside the loop is not: it is a join, and which value arrived depends on which way the
/// iteration went, which neither the guard nor the preheader is in a position to know. Nor is
/// anything that reads memory, because the second copy would read it at a different moment.
///
/// The order is a post order, so operands come out in front of the uses that want them, which is
/// what [`remade`] needs to write them in one pass. It may hold junk when this refuses, and the
/// caller throws it away.
fn writable(
    func: &Func,
    loops: &Loops,
    id: LoopId,
    value: Value,
    order: &mut Vec<Value>,
    leaves: &mut Vec<Value>,
    seen: &mut HashSet<Value>,
) -> bool {
    // A value reached twice is written once, and its place in the order is the first one, which is
    // in front of both uses. Returning true here is safe because a refusal anywhere refuses the
    // whole address, so a value already seen is one already accepted.
    if !seen.insert(value) {
        return true;
    }
    let at = match func[value].def {
        Def::Result { inst, .. } => func.block_of(inst),
        Def::Param { block, .. } => Some(block),
    };
    if at.is_none_or(|at| !loops.contains(id, at)) {
        if func[value].ty.is_ptr() {
            leaves.push(value);
        }
        return true;
    }
    // A value defined in a loop inside this one is neither of those. It is not the same number
    // wherever it is read, so reading it again in the preheader is not writing it again, and it is
    // not a parameter of the header, so neither block has it in hand. The two questions look alike
    // and the answers are opposite, which is why this arm is separate from the one above rather
    // than folded into it as "not in this loop's own blocks".
    if at.is_some_and(|at| loops.innermost(at) != Some(id)) {
        return false;
    }
    match func[value].def {
        Def::Param { block, .. } => {
            if block != loops.header(id) {
                return false;
            }
            if func[value].ty.is_ptr() {
                leaves.push(value);
            }
            true
        }
        Def::Result { inst, index } => {
            if index != 0 || !plain(func[inst].opcode) {
                return false;
            }
            let args = func[func[inst].args].to_vec();
            if !args.iter().all(|&arg| writable(func, loops, id, arg, order, leaves, seen)) {
                return false;
            }
            order.push(value);
            true
        }
    }
}

/// Whether an instruction is one the guard may write a second copy of.
///
/// A list rather than a question about effects, and deliberately. What has to hold is that a second
/// copy in another block computes the same number, which rules out anything that reads memory and
/// anything that depends on where it is, and that writing it in the preheader is harmless on a loop
/// that turns out to run no iterations at all, which rules out anything that can fault. A division
/// is the one that catches people out: it has no effects to speak of and it traps on a zero the
/// first iteration would never have reached. Naming what is allowed makes an opcode added later
/// refused until somebody looks at it, which is the right way round for this.
fn plain(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::IConst
            | Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::SExt
            | Opcode::ZExt
            | Opcode::Trunc
            | Opcode::ICmp
            | Opcode::Select
            | Opcode::PtrAdd
            | Opcode::GlobalAddr
    )
}

/// Writes the expression that works an address out into the block a builder is on, with the header's
/// parameters replaced by whatever that block has in their place.
///
/// The order is [`writable`]'s, so every operand has been written by the time the use of it is
/// reached and one pass over the list is enough. A value not in the map is one from outside the loop,
/// which is itself wherever it is read.
///
/// Flags come off. `nsw` on an add in the loop is a promise about an address the loop was going to
/// compute, and the copy in the preheader is computed whether the loop runs or not, so a promise that
/// held there does not obviously hold here. Dropping it costs nothing, since what is built is a
/// question for the runtime rather than an address anything reads through.
fn remade(
    build: &mut Builder<'_>,
    made: &mut Vec<Value>,
    order: &[Value],
    at: Value,
    swap: &HashMap<Value, Value>,
) -> Value {
    let mut swap = swap.clone();
    for &value in order {
        let Def::Result { inst, .. } = build.func()[value].def else {
            unreachable!("the order holds nothing but instruction results")
        };
        let data = build.func()[inst];
        let args: Vec<Value> = build.func()[data.args]
            .iter()
            .map(|arg| swap.get(arg).copied().unwrap_or(*arg))
            .collect();
        let args = build.func().push_values(&args);
        let ty = build.func()[value].ty;
        let copy =
            build.value(InstData { args, extra: data.extra, ..InstData::new(data.opcode) }, ty);
        made.push(copy);
        swap.insert(value, copy);
    }
    swap.get(&at).copied().unwrap_or(at)
}

/// Whether the loop moves a pointer it carries in a way this is willing to measure.
///
/// A pointer the loop was handed from outside does not move at all and is nothing to refuse. One the
/// header carries is handed back round the latch, and what comes back has to be that same pointer
/// moved, which is [`moving`] and is where the linked list refusal lives.
fn carried(func: &Func, cfg: &Cfg, loops: &Loops, id: LoopId, latch: Block, leaf: Value) -> bool {
    let header = loops.header(id);
    let Def::Param { block, index } = func[leaf].def else { return true };
    if block != header {
        return true;
    }
    let Some(term) = func.terminator(latch) else { return false };
    let round = copy::edge_args(func, term, header);
    let Some(&next) = round.get(index as usize) else { return false };
    let mut seen = HashSet::new();
    moving(func, cfg, loops, id, leaf, next, &mut seen)
}

/// A pointer with the constant `ptr_add`s on the front of it taken off, and how many bytes they came
/// to between them.
fn peeled(func: &Func, pointer: Value) -> (Value, i128) {
    let mut at = pointer;
    let mut offset = 0;
    while let Some(by) = operand_of(func, at, Opcode::PtrAdd, 1) {
        let (Some(step), Some(of)) = (constant(func, by), operand_of(func, at, Opcode::PtrAdd, 0))
        else {
            break;
        };
        offset += step;
        at = of;
    }
    (at, offset)
}

/// Whether a value is a header parameter moved by some number of bytes.
///
/// The conditions are [`measured`]'s and the walk is the obvious one. False is a value that is not
/// the parameter moved, which is a refusal, and true is the parameter moved by amounts this does not
/// need to know. It used to hand back the largest step it saw, which sized how far the runtime was
/// asked to look, and nothing is sized by a step any more.
fn moving(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    id: LoopId,
    param: Value,
    value: Value,
    seen: &mut HashSet<Value>,
) -> bool {
    if value == param {
        return true;
    }
    // A value already on the way back is one this has been through, and coming back round to it is
    // what a walk through a join looks like. Not a refusal, because this path holds nothing that has
    // not been looked at.
    if !seen.insert(value) {
        return true;
    }
    let at = match func[value].def {
        Def::Result { inst, .. } => match func.block_of(inst) {
            Some(block) => block,
            None => return false,
        },
        Def::Param { block, .. } => block,
    };
    // Anything defined outside the loop is something the loop was handed rather than the parameter
    // moved, and it is where the walk stops as well as what it refuses. A value defined in a loop
    // inside this one is refused by the same test and it is refused for a stronger reason: what it
    // does is a question about the inner loop's iterations rather than about this one's.
    if loops.innermost(at) != Some(id) {
        return false;
    }
    match func[value].def {
        Def::Result { inst, .. } => {
            let args = &func[func[inst].args];
            match func[inst].opcode {
                Opcode::PtrAdd => match (args.first(), args.get(1)) {
                    (Some(&of), Some(_)) => moving(func, cfg, loops, id, param, of, seen),
                    _ => false,
                },
                // Both arms have to be the parameter moved, since either of them may be the one
                // taken. The condition is not looked at, because how the loop chose is not something
                // the displacement depends on.
                Opcode::Select => match (args.get(1), args.get(2)) {
                    (Some(&one), Some(&two)) => {
                        moving(func, cfg, loops, id, param, one, seen)
                            && moving(func, cfg, loops, id, param, two, seen)
                    }
                    _ => false,
                },
                _ => false,
            }
        }
        // A parameter of a block inside the loop is a join, and every way into it has to be the
        // parameter moved. The header is not one of them: its other parameters are other values and
        // the parameter itself was the base case above.
        Def::Param { block, index } => {
            if block == loops.header(id) {
                return false;
            }
            let mut moved = true;
            for &pred in cfg.predecessors(block) {
                let Some(term) = func.terminator(pred) else { return false };
                let args = copy::edge_args(func, term, block);
                let Some(&came) = args.get(index as usize) else { return false };
                moved = moved && moving(func, cfg, loops, id, param, came, seen);
            }
            moved
        }
    }
}

/// Whether a pointer and a byte displacement beside it are the two the address is really built out
/// of, rather than two values an expression happened to end up holding.
///
/// The displacement has to end up as wide as the arithmetic, because what is built from it here is
/// a `ptr_add` in a preheader. It gets there one of three ways: it is a plain number, or it is
/// already sixty four bits, or it is narrower and the invariant says which extension it is read
/// through, which is what an index the caller handed in looks like in C, where the index is an
/// `int`.
fn walks(func: &Func, base: Anchor, apart: Plain) -> bool {
    let word = Type::int(64);
    if !base.value().is_none_or(|base| func[base].ty.is_ptr()) {
        return false;
    }
    // A global with nothing but a number beside it, which is what a walk over a file scope array
    // from a fixed place in it looks like. A number is as wide as it needs to be.
    let Some(value) = apart.value.filter(|_| apart.scale != 0) else { return true };
    match apart.read {
        None => func[value].ty == word,
        Some(read) => read.to == word && func[value].ty.is_int() && func[value].ty.bits() < 64,
    }
}

/// Makes the two halves and the block that chooses between them.
///
/// The order matters in two places. The copy is made before anything is rewired, so the copy's back
/// edge is remapped to the copy's own header rather than to a guard that did not exist yet. The
/// checks come out of the fast half last, so the copy still has them.
fn apply(func: &mut Func, plan: &Plan) {
    // The slow half, which is the loop as it stands, under a substitution that renames everything it
    // defines. Nothing is seeded, so its header gets parameters of its own, which is what a copy
    // reached from a block that also reaches the original needs.
    let mut renamed: HashMap<Value, Value> = HashMap::new();
    let copies = copy::blocks(func, &plan.body, &mut renamed);
    let slow = copies[&plan.header];

    let Choice { ok, windows } = limited(func, plan);

    // Nothing in the loop moves, so which half runs is settled in the preheader and settled for
    // good. There is no guard block and nothing carried round: the way into the loop is the choice.
    if windows.is_empty() {
        let term = func.terminator(plan.preheader).expect("a preheader ends in a jump");
        let args = copy::edge_args(func, term, plan.header);
        func.remove_inst(term);
        Builder::new(func, plan.preheader).br_if(ok, plan.header, &args, slow, &args);
        take(func, plan);
        return;
    }

    // The guard, which takes over the header's place: the preheader arrives here, the back edge
    // comes back to here, and the header is reached from here and nowhere else. Its first
    // parameters are offsets of its own, one per distinct step, because where the loop's own
    // pointers are is not something this pass has to find and a loop with several ways out may
    // have nothing that walks in step with what its checks are about.
    //
    // A measured offset gets no parameter and nothing carried round. Where its pointer is now is
    // worked out from the parameters below, which are the ones the header carries, either by being
    // one of them outright or by the guard writing the arithmetic out again.
    let word = Type::int(64);
    let counting: Vec<i128> =
        windows.iter().filter(|window| window.from.is_none()).map(|w| stepped(w.key)).collect();
    let types: Vec<Type> = func[plan.header].params.iter().map(|&param| func[param].ty).collect();
    let guard = func.create_block();
    let offsets: Vec<Value> = counting.iter().map(|_| func.append_param(guard, word)).collect();
    let carried: Vec<Value> = types.iter().map(|&ty| func.append_param(guard, ty)).collect();

    // Unsigned, because the window is a byte count and so is the offset, and because unsigned is
    // what the rule the removal rests on is written in. That is what makes the subtraction below
    // safe as well: a pointer that went under where it started comes out as a displacement no
    // window is ever going to hold, so the loop goes to the half that kept its checks.
    let held: HashMap<Value, Value> =
        func[plan.header].params.iter().copied().zip(carried.iter().copied()).collect();
    // Nothing built here has to be moved afterwards, unlike in the preheader: the guard is a block
    // this pass just made and it has no terminator yet, so appending puts things in the order they
    // were built and the branch at the end goes on last. `spent` is where the builder drops what it
    // made and nothing reads it back.
    let mut build = Builder::new(func, guard);
    let mut spent = Vec::new();
    let mut inside: Option<Value> = None;
    let mut counted = 0;
    for window in &windows {
        let offset = match window.from {
            None => {
                let offset = offsets[counted];
                counted += 1;
                offset
            }
            Some(from) => {
                let Key::From(at) = window.key else {
                    unreachable!("only a measured window holds where its pointer began")
                };
                let here = remade(&mut build, &mut spent, &window.rebuild, at, &held);
                let now = build.unary(Opcode::PtrToInt, here, word);
                build.binary(Opcode::Sub, now, from, Flags::NONE)
            }
        };
        let under = build.icmp(IntPred::Ule, offset, window.bound);
        inside = Some(match inside {
            None => under,
            Some(so_far) => build.binary(Opcode::And, so_far, under, Flags::NONE),
        });
    }
    let inside = inside.expect("a plan with a window has at least one of them");
    build.br_if(inside, plan.header, &carried, slow, &carried);

    // The way in, which tests whether the fast half may run at all and starts every offset at the
    // first access. A loop nothing fits in never reaches the guard.
    let term = func.terminator(plan.preheader).expect("a preheader ends in a jump to the header");
    let args = copy::edge_args(func, term, plan.header);
    func.remove_inst(term);
    let mut build = Builder::new(func, plan.preheader);
    let zero = build.iconst(word, 0);
    let mut into: Vec<Value> = offsets.iter().map(|_| zero).collect();
    into.extend_from_slice(&args);
    build.br_if(ok, guard, &into, slow, &args);

    // The way round, which walks each counted offset on by its step. The offsets are the guard's
    // parameters and the guard dominates every block in the fast half, so the latch may read them.
    // `nuw` rather than `nsw` because [`bounded`] held the window short of where this could wrap,
    // and it held it there in unsigned terms. A measured offset has nothing here: the guard reads
    // the pointer the loop already hands round.
    let term = func.terminator(plan.latch).expect("a latch ends in a branch back to the header");
    let mut build = Builder::new(func, plan.latch);
    let mut made = Vec::new();
    let mut next = Vec::new();
    for (&offset, &step) in offsets.iter().zip(&counting) {
        let by = build.iconst(word, step);
        made.push(by);
        let walked = build.binary(Opcode::Add, offset, by, Flags::NUW);
        made.push(walked);
        next.push(walked);
    }
    for value in made {
        let inst = inst_of(func, value);
        func.remove_inst(inst);
        func.insert_before(inst, term);
    }
    route(func, term, plan.header, guard, &next);
    take(func, plan);
}

/// Takes the checks the fast half does not need out of it.
///
/// The `cap_of` each one was reading is left where it is, for `dce` after this pass to take away,
/// which is the arrangement [`crate::hoist`] and [`crate::discharge`] are both in.
fn take(func: &mut Func, plan: &Plan) {
    for sweep in &plan.sweeps {
        func.remove_inst(sweep.check);
    }
}

/// Sends every edge this terminator has to `from` to `to` instead, with more arguments in front.
fn route(func: &mut Func, term: Inst, from: Block, to: Block, first: &[Value]) {
    for at in func.target_list(term).iter() {
        let call = func[at];
        if call.block != from {
            continue;
        }
        let mut args = first.to_vec();
        args.extend_from_slice(&func[call.args]);
        let args = func.push_values(&args);
        func.set_block_call(at, BlockCall { block: to, args });
    }
}

/// One offset the guard works out every time round, and how far it may get.
struct Window {
    /// Which checks share it, which for a counted offset is how far the address moves each time
    /// round. That is a magnitude, because the offset counts bytes from the first access and counts
    /// them the same way whichever direction the address walks.
    key: Key,
    /// The highest offset an access may start at and still be inside what the extent covers.
    bound: Value,
    /// Where the pointer was on the way into the loop, as an integer, for an offset the guard
    /// measures. `None` for one it counts, which starts at zero and needs nothing to measure from.
    from: Option<Value>,
    /// What the guard writes again to know where the pointer is now, operands before uses. Empty
    /// for a counted offset, and empty for a measured one off a parameter the header carries, since
    /// the guard carries that parameter itself. See [`writable`].
    rebuild: Vec<Value>,
}

/// How the two halves are chosen between, which depends on whether any address in the loop moves.
struct Choice {
    /// Whether every check in the loop fits at all, which the preheader tests before it enters the
    /// fast half. It is false for a dangling pointer or an object smaller than the thing being read
    /// out of it, and then the fast half runs no iterations and the check in the slow half reports
    /// the fault at the access rather than at the loop.
    ok: Value,
    /// One per distinct step, and empty when no address in the loop moves. A loop like that needs
    /// no guard block and nothing carried round it, because `ok` is the whole answer and it does
    /// not change while the loop runs.
    windows: Vec<Window>,
}

/// Builds what the preheader has to work out before either half can run.
///
/// One `cap_extent` per check and what it leaves room for, all of it in the preheader in front of
/// the jump into the loop. A builder appends to the end of a block, which in a block that already
/// has its terminator is after it, so everything is built first and then moved in front of the
/// terminator in the order it was built.
///
/// # Why the window is bytes and not iterations
///
/// This used to work out how many iterations a check allows, which is `(extent - reach) / step + 1`
/// clamped at zero, and count iterations against it. The claim that has to hold for the fast half
/// to be allowed to drop its checks was then that `i * step + reach <= extent` for every `i` below
/// that limit, which has a symbolic multiply and a symbolic divide in it at sixty four bits, and
/// z3 does not finish on it in two and a half minutes in any of three formulations. So the whole
/// transformation sat outside the rule table that `spec/safe-memory/07-check-elimination.md`
/// section 7.7 asks every elimination to be inside, and it sat there for a solver reason rather
/// than a design one, which is the worst kind.
///
/// Counting bytes instead of iterations takes the arithmetic out. The offset the loop is at moves
/// by `step` each time round exactly as the address does, the window is `extent - reach`, and the
/// claim is that an offset at or below that plus the reach is inside the extent. No multiply and no
/// divide, and it is the claim `swept.sym.i64` in `crates/rucc-opt/rules/safety.rules` already
/// makes, which [`windowed`] asks. The pass earns that rule's hypotheses rather than assuming them:
/// `ok` is where `extent` is held to be at least `reach`, so the window cannot have wrapped, and
/// [`bounded`] is where the offset is held short of where adding one more step would.
///
/// It is also less code. A loop with one step in it loses a divide from its preheader and carries
/// the same one value round that it did before.
///
/// # Why one window per step and not one per check
///
/// Two checks that walk by the same amount are at the same offset on every iteration, so they can
/// share the offset and the smaller of their two windows. On SQLite 127 of the 268 loops this
/// splits have one distinct step and five have two, so this is one value round the loop almost
/// always and two occasionally.
fn limited(func: &mut Func, plan: &Plan) -> Choice {
    let word = Type::int(64);
    let term = func.terminator(plan.preheader).expect("a preheader ends in a jump to the header");
    // What the preheader hands the header, which is where a measured offset is measured from. Read
    // before the builder exists, because reading it borrows the function.
    let entering = copy::edge_args(func, term, plan.header);
    // What the preheader has in place of each parameter the header carries, which is what a measured
    // address is written again out of to get the first iteration's.
    let swap: HashMap<Value, Value> =
        func[plan.header].params.iter().copied().zip(entering.iter().copied()).collect();
    let mut made = Vec::new();
    let mut build = Builder::new(func, plan.preheader);

    let mut ok: Option<Value> = None;
    let mut windows: Vec<Window> = Vec::new();
    // Every measured address written once, since the same expression under the same substitution is
    // the same value and two checks off one pointer are the commonest thing here.
    let mut begun: HashMap<Value, Value> = HashMap::new();
    // Every question asked once, for the same reason. See [`Asked`].
    let mut asked = Asked::default();
    for sweep in &plan.sweeps {
        let base = match sweep.walk {
            Walk::By(_) => anchored(&mut build, &mut made, sweep.base),
            Walk::Again { at, .. } => match begun.get(&at) {
                Some(&had) => had,
                None => {
                    let first = remade(&mut build, &mut made, &sweep.rebuild, at, &swap);
                    begun.insert(at, first);
                    first
                }
            },
        };
        let (window, zero, also) = spare(&mut build, &mut made, sweep, base, &mut asked);
        // Every check has to fit for the fast half to be the one that runs, and this is where the
        // hypothesis the rule is asked under is earned: a window worked out from an extent smaller
        // than the reach is one that wrapped, and none of what follows would mean anything.
        let fits = build.icmp(IntPred::Sge, window, zero);
        made.push(fits);
        // What the sweep asked for beside that, which is nothing on all but the ones [`trailing`]
        // took and is the gap being at or above zero on those.
        let fits = match also {
            None => fits,
            Some(more) => {
                let both = build.binary(Opcode::And, fits, more, Flags::NONE);
                made.push(both);
                both
            }
        };
        ok = Some(match ok {
            None => fits,
            Some(so_far) => {
                let both = build.binary(Opcode::And, so_far, fits, Flags::NONE);
                made.push(both);
                both
            }
        });
        if sweep.walk.still() {
            continue;
        }
        // Two checks that walk by the same amount are at the same offset on every iteration, so
        // they share the offset and the smaller of their two windows. The amount is a magnitude,
        // which is what lets a walk up and a walk down by eight share one offset: the offset counts
        // bytes from the first access and both of them are eight bytes further along each time
        // round. Which way they went is in the window each of them worked out, and taking the
        // smaller of two windows is no different for being about two directions.
        //
        // Two checks the guard measures share for the same reason and by the other key. A fixed
        // distance from one pointer is a fixed distance from it on every iteration, so both of them
        // moved by whatever that pointer moved by and one subtraction answers for the pair.
        let key = sweep.walk.key();
        match windows.iter().position(|held| held.key == key) {
            Some(at) => {
                let bound = windows[at].bound;
                let smaller = build.icmp(IntPred::Ult, window, bound);
                made.push(smaller);
                let least = build.select(smaller, window, bound);
                made.push(least);
                windows[at].bound = least;
            }
            None => {
                // Where a measured offset is measured from, worked out once in the preheader
                // because it is the same address on every iteration by definition.
                let from = match key {
                    Key::Every(_) => None,
                    Key::From(_) => {
                        let from = build.unary(Opcode::PtrToInt, base, word);
                        made.push(from);
                        Some(from)
                    }
                };
                let rebuild = if from.is_some() { sweep.rebuild.clone() } else { Vec::new() };
                windows.push(Window { key, bound: window, from, rebuild });
            }
        }
    }
    let ok = ok.expect("a plan holds at least one check");

    for window in &mut windows {
        window.bound = bounded(&mut build, &mut made, stepped(window.key), window.bound);
    }

    for value in made {
        let inst = inst_of(func, value);
        func.remove_inst(inst);
        func.insert_before(inst, term);
    }
    Choice { ok, windows }
}

/// How much the offset goes up by between one test and the next, which is nothing for one the guard
/// measures.
///
/// A measured offset is worked out from the pointer every time round rather than added to, so it is
/// never one step past anything and there is no step to leave room for. What it can be is enormous,
/// when the pointer went below where it started and the subtraction came out as a huge unsigned
/// number, and that is the answer wanted: the guard is meant to hand a loop like that to the half
/// that kept its checks.
fn stepped(key: Key) -> i128 {
    match key {
        Key::Every(step) => step,
        Key::From(_) => 0,
    }
}

/// Holds a window short of where one more step would take the offset out of sixty four bits.
///
/// The offset goes up by the step every time round and is tested afterwards, so it reaches one step
/// past the window before the guard sends the loop to the other half. Nothing else here bounds the
/// window: `cap_extent` answers with no more than it was asked for, and what it was asked for is a
/// trip count times a step, which saturates rather than refusing. An offset that wrapped would come
/// back small, the guard would let it through, and the fast half would read past the end of the
/// object with nothing left in it to say so.
///
/// One comparison and one select in the preheader, and the value it clamps to is so far past any
/// object a program allocates that this never fires. It is here because the failure it stops is
/// silent.
fn bounded(build: &mut Builder<'_>, made: &mut Vec<Value>, step: i128, bound: Value) -> Value {
    let word = Type::int(64);
    let room = build.iconst(word, i128::from(i64::MAX) - step);
    made.push(room);
    let over = build.icmp(IntPred::Ugt, bound, room);
    made.push(over);
    let held = build.select(over, room, bound);
    made.push(held);
    held
}

/// Whether one offset at or below the window is one whose access is inside the extent.
///
/// This function decides nothing. It builds the term `swept.sym.i64` is written about and asks the
/// table, which is section 7.7's split: the pass established the window and carries the offset, and
/// whether an offset inside the window means an access inside the object is somebody's proof rather
/// than this file's opinion. It is the same rule [`crate::hoist`] asks about a loop whose extent the
/// program works out, and it is the same question, since a window is a hoisted check's far end under
/// another name.
///
/// Four of its five arguments are opaque. The address, the extent and the window are values the pass
/// does not have as numbers, and the offset is whichever iteration the reader cares about, which is
/// how one question comes to be about all of them. The rule's three hypotheses about that pair are
/// what [`limited`] and [`bounded`] earn.
///
/// A walk from high to low asks `swept.down.sym.i64` instead, which is the same claim written about
/// addresses that go the other way. Asking the ascending rule and subtracting somewhere in the pass
/// would be arithmetic on the thing being proved, which is what section 7.7 exists to stop, so the
/// direction picks a term and the table answers about that term or does not.
fn windowed(reach: i128, down: bool) -> bool {
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let span = question.opaque();
    let span = question.app("value.i64", &[span]);
    let far = question.opaque();
    let far = question.app("value.i64", &[far]);
    let reach = question.number(reach);
    let reach = question.app("iconst.i64", &[reach]);
    let delta = question.opaque();
    let delta = question.app("value.i64", &[delta]);
    let head = if down { "swept.down.sym.i64" } else { "swept.sym.i64" };
    let term = question.app(head, &[at, span, far, reach, delta]);
    match safety::TABLE.find(&question, term) {
        Some(found) => yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// The base as a value here, writing the address of a global out again when that is what it is.
///
/// One instruction, and the same one the loop has inside it. Working it out again is why
/// [`crate::licm`] leaves the one in the loop alone, and it is why the address can be described
/// rather than named in the first place.
fn anchored(build: &mut Builder<'_>, made: &mut Vec<Value>, base: Anchor) -> Value {
    match base {
        Anchor::Value(value) => value,
        Anchor::Address(symbol) => {
            let extra = Extra::Symbol(symbol);
            let at =
                build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
            made.push(at);
            at
        }
    }
}

/// How far the first access sits past the base, as a value, or `None` when it sits on it.
///
/// Wrapping arithmetic throughout, because this is the address the loop was going to compute
/// anyway. The flags a `nsw` would put on it would be a promise about the caller's index, and what
/// this is building is a question for the runtime rather than an address anything reads.
fn displacement(build: &mut Builder<'_>, made: &mut Vec<Value>, apart: Plain) -> Option<Value> {
    let word = Type::int(64);
    let mut sum = match apart.value.filter(|_| apart.scale != 0) {
        None => {
            return (apart.offset != 0).then(|| {
                let by = build.iconst(word, apart.offset);
                made.push(by);
                by
            });
        }
        Some(value) => value,
    };
    // The extension the invariant describes, emitted before anything is done with the value. It
    // comes first because everything after it is arithmetic at the wide type and the value is not
    // that width yet.
    if let Some(read) = apart.read {
        let widen = match read.reading {
            Reading::Signed => Opcode::SExt,
            Reading::Unsigned => Opcode::ZExt,
        };
        sum = build.unary(widen, sum, read.to);
        made.push(sum);
    }
    if apart.scale != 1 {
        let by = build.iconst(word, apart.scale);
        made.push(by);
        sum = build.binary(Opcode::Mul, sum, by, Flags::NONE);
        made.push(sum);
    }
    if apart.offset != 0 {
        let by = build.iconst(word, apart.offset);
        made.push(by);
        sum = build.binary(Opcode::Add, sum, by, Flags::NONE);
        made.push(sum);
    }
    Some(sum)
}

/// How many bytes past the first access belong to whatever owns it, and a zero to compare that with.
///
/// The question both callers rest on. `extent - reach` is negative when the first access does not
/// fit at all, zero when exactly one fits, and how much room there is for further ones otherwise.
///
/// # A walk from high to low
///
/// The offset the guard carries is a magnitude, so a loop whose address goes down is a loop whose
/// offset goes up in exactly the same way and everything built around the offset is untouched. What
/// changes is which end of the object is asked about. An ascending walk starts at the first access
/// and runs off the top of it, so `cap_extent` at the first address is the question. A descending
/// one starts at the first access and runs off the bottom, so the question is `cap_extent_back` at
/// the end of the first access, which is `first + reach`.
///
/// Anchoring at the end rather than at `first` is what makes the two the same shape. The answer is
/// then how many bytes below the end of the first access belong to the same thing, the window is
/// that less the reach exactly as above, and the access on iteration `delta` is the `reach` bytes
/// ending at `first + reach - delta`. That is the claim `swept.down.sym.i64` is written about, with
/// `at` being the end of the first access, and it is a claim about every iteration for the same
/// reason the ascending one is.
///
/// # Asking once
///
/// [`spare`] runs once per sweep, and a loop that walks one pointer has a bounds check, a liveness
/// check and a derivation check on it, so the same question about the same address used to be asked
/// three times over and after tamnd/rucc#869 more often than that. On SQLite that came to 4184 calls
/// to the runtime for 591 split loops, which is seven per loop, and `a-string-scan` had five in one
/// preheader at one address. [`Asked`] is what makes it one. Two questions built out of the same
/// pieces are the same question here, because the whole of what this builds sits in one block that
/// has no call in it, so nothing between two of them can change what the second one would answer.
///
/// [`crate::number`] would say the same thing about the arithmetic and cannot say it about the query,
/// which has effects, and in any case it runs before this pass rather than after it, so there is
/// nothing behind this that would tidy up after it.
fn spare(
    build: &mut Builder<'_>,
    made: &mut Vec<Value>,
    sweep: &Sweep,
    base: Value,
    asked: &mut Asked,
) -> (Value, Value, Option<Value>) {
    let word = Type::int(64);
    let first = match asked.first(base, sweep.apart) {
        Some(had) => had,
        None => {
            let first = match displacement(build, made, sweep.apart) {
                None => base,
                Some(by) => {
                    let args = build.func().push_values(&[base, by]);
                    let data = InstData::new(Opcode::PtrAdd);
                    let sum = build.value(InstData { args, ..data }, Type::PTR);
                    made.push(sum);
                    sum
                }
            };
            asked.firsts.push(((base, sweep.apart), first));
            first
        }
    };
    // How far the runtime is asked to look, which is as far as the arithmetic carries. The answer
    // is a true count of the bytes that belong to the object, never more than the truth and never
    // more than what was asked for, so a smaller ask is a smaller window and a smaller window is
    // fewer iterations in the half that has no checks in it. There is nothing on the other side of
    // that trade any more. The query probes the far end of what it was asked for and halves rather
    // than walking, about twenty five reads of the plane whatever the number is, so the price does
    // not turn on the number and the largest ask is the right one.
    let want = asked.number(build, made, i128::from(i64::MAX));

    // Where the question is asked from, which for a walk that goes down is the end of the first
    // access rather than its start. The arithmetic wraps, in the way [`displacement`] wraps and for
    // the same reason: this is an address the loop was going to reach anyway and the value is a
    // question for the runtime rather than something anything reads through.
    let (query, at) = if sweep.walk.down() {
        let end = match asked.end(first, sweep.reach) {
            Some(had) => had,
            None => {
                let by = asked.number(build, made, sweep.reach);
                let args = build.func().push_values(&[first, by]);
                let data = InstData::new(Opcode::PtrAdd);
                let end = build.value(InstData { args, ..data }, Type::PTR);
                made.push(end);
                asked.ends.push(((first, sweep.reach), end));
                end
            }
        };
        (Opcode::CapExtentBack, end)
    } else {
        (Opcode::CapExtent, first)
    };

    let extent = match asked.extent(query, at, want) {
        Some(had) => had,
        None => {
            let args = build.func().push_values(&[at]);
            let data = InstData::new(Opcode::CapOf);
            let capability = build.value(InstData { args, ..data }, Type::CAP);
            made.push(capability);
            let args = build.func().push_values(&[capability, at, want]);
            let extent = build.value(InstData { args, ..InstData::new(query) }, word);
            made.push(extent);
            asked.extents.push(((query, at, want), extent));
            extent
        }
    };

    let reach = asked.number(build, made, sweep.reach);
    let left = build.binary(Opcode::Sub, extent, reach, Flags::NSW);
    made.push(left);
    let zero = asked.number(build, made, 0);

    // The gap [`trailing`] left for the guard to work out, which is how far into the window the
    // walk's first access sits. Taking it off the window is what makes the window one about the
    // walk again, and asking it to be at or above zero is what says the walk begins inside the
    // object the window was measured in rather than somewhere below it.
    let Some(ahead) = sweep.ahead.and_then(|ahead| displacement(build, made, ahead)) else {
        return (left, zero, None);
    };
    let short = build.binary(Opcode::Sub, left, ahead, Flags::NSW);
    made.push(short);
    let above = build.icmp(IntPred::Sge, ahead, zero);
    made.push(above);
    (short, zero, Some(above))
}

/// What the preheader has worked out already, so that one question is asked once.
///
/// Association lists rather than maps, because a plan holds a handful of sweeps and the keys are
/// what scalar evolution hands out, which is `Eq` and not `Hash`. Looking a key up walks the list,
/// and the longest list on SQLite is a dozen entries.
#[derive(Default)]
struct Asked {
    /// Numbers written down, by the number.
    numbers: Vec<(i128, Value)>,
    /// Where the first access is, by the base it is measured from and how far past it it sits.
    firsts: Vec<((Value, Plain), Value)>,
    /// The end of a first access, by where it starts and how many bytes it is.
    ends: Vec<((Value, i128), Value)>,
    /// What the runtime answered, by which end was asked, about which address and how far.
    extents: Vec<((Opcode, Value, Value), Value)>,
}

impl Asked {
    /// A number written down in the preheader, once per number.
    fn number(&mut self, build: &mut Builder<'_>, made: &mut Vec<Value>, imm: i128) -> Value {
        if let Some(&(_, had)) = self.numbers.iter().find(|&&(seen, _)| seen == imm) {
            return had;
        }
        let value = build.iconst(Type::int(64), imm);
        made.push(value);
        self.numbers.push((imm, value));
        value
    }

    /// The first access off this base and this far past it, if it has been worked out.
    fn first(&self, base: Value, apart: Plain) -> Option<Value> {
        self.firsts.iter().find(|&&(key, _)| key == (base, apart)).map(|&(_, had)| had)
    }

    /// The end of this first access, if it has been worked out.
    fn end(&self, first: Value, reach: i128) -> Option<Value> {
        self.ends.iter().find(|&&(key, _)| key == (first, reach)).map(|&(_, had)| had)
    }

    /// What the runtime said about this address, if it has been asked.
    fn extent(&self, query: Opcode, at: Value, want: Value) -> Option<Value> {
        self.extents.iter().find(|&&(key, _)| key == (query, at, want)).map(|&(_, had)| had)
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Extra, Flags, Func, Inst, InstData, IntPred, MemInfo, MemOrder, Module,
        Opcode, Restrict, Signature, Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{SPLIT, Split};
    use crate::canon::Canon;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// How many times the loop goes round, and how wide each element of the walk is.
    const TRIPS: i128 = 16;
    const WIDTH: i128 = 4;

    /// A counted loop that reads one element each time round and can stop on what it read.
    ///
    /// ```text
    /// entry(a): jump head(0)
    /// head(i):  p = a + i*4; check_bounds cap_of(p), p; v = load p
    ///           br v == 0 -> done, more
    /// more:     next = i + 1; br next < 16 -> head(next), done
    /// done:     ret
    /// ```
    ///
    /// The second way out is the point. Hoisting refuses this loop, because a loop that can stop in
    /// the middle reads fewer bytes than its count says and one check in front of it for all of them
    /// would refuse a program that was right. Splitting does not care, because the count it reads is
    /// only ever an upper limit on how far to look.
    fn leaving() -> (Interner, Func, Vec<Block>) {
        walking(Some(TRIPS), Flags::NSW)
    }

    /// The same loop, with how many times it goes round handed in rather than written down.
    ///
    /// A loop whose count is an expression rather than a number, which this pass no longer reads and
    /// which is still worth a test of its own: the shape has to split like any other and the guard
    /// has to come out the same as the one a written down count gets.
    fn counting() -> (Interner, Func, Vec<Block>) {
        walking(None, Flags::NSW)
    }

    /// The same loop again, with an increment that promises nothing, so nobody counts it.
    ///
    /// What `-fwrapv` produces, and the shape a great deal of real code is in. Hoisting refuses it,
    /// because a count that rests on the counter not wrapping is not a count it may size a check
    /// with. This pass sizes nothing with a count, so it takes it.
    fn uncounted() -> (Interner, Func, Vec<Block>) {
        walking(Some(TRIPS), Flags::NONE)
    }

    /// The same loop, reading from an index the caller handed in rather than from zero.
    ///
    /// `a[start + i]`, whose first address is `a + 4 * start`: a pointer and a displacement, with a
    /// number for neither of them. This is the shape the pass used to give up on, and it is a
    /// common one, because a loop over part of an array is written this way and so is every walk
    /// that begins where the last one stopped. See #810.
    fn from_an_index() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let params = [Type::PTR, Type::int(64)];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let start = func.append_param(entry, Type::int(64));
        let counter = func.append_param(head, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let index = build.binary(Opcode::Add, counter, start, Flags::NSW);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, index, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let read = build.load(Type::int(32), pointer, mem(), Flags::NONE);
        let nothing = build.iconst(Type::int(32), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, done, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = build.iconst(Type::int(64), TRIPS);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, done])
    }

    /// The same loop over a file scope array, with the `global_addr` inside the loop.
    ///
    /// Which is where one sits, because working the address out again costs a single instruction
    /// and `crate::licm` would rather do that than hold it in a register the whole way round. So
    /// the address of the array is not a value defined outside the loop and never will be, and the
    /// pass has to take it from where it is or not at all. See #810.
    fn over_a_global() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let tab = names.intern("tab");
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let done = func.create_block();
        let counter = func.append_param(head, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let extra = Extra::Symbol(tab);
        let array = build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let read = build.load(Type::int(32), pointer, mem(), Flags::NONE);
        let nothing = build.iconst(Type::int(32), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, done, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = build.iconst(Type::int(64), TRIPS);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, done])
    }

    /// The same loop again, with the index in `int` and sign extended, which is what C gives.
    ///
    /// `a[start + i]` with `start` and `i` both `int`. The front end adds them at thirty two bits
    /// and sign extends the sum before scaling it, so the first thing scalar evolution meets is the
    /// extension of a chrec whose base is a value rather than a number. Splitting takes it because
    /// the widened base is described rather than named, and this pass emits the extension in the
    /// preheader. See #810.
    fn from_a_narrow_index() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let params = [Type::PTR, Type::int(32)];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let start = func.append_param(entry, Type::int(32));
        let counter = func.append_param(head, Type::int(32));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let index = build.binary(Opcode::Add, counter, start, Flags::NSW);
        let wide = build.unary(Opcode::SExt, index, Type::int(64));
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, wide, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let read = build.load(Type::int(32), pointer, mem(), Flags::NONE);
        let nothing = build.iconst(Type::int(32), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, done, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = build.iconst(Type::int(32), TRIPS);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, done])
    }

    /// The same loop again, walking from the end of the array down to the start of it.
    ///
    /// ```text
    /// entry(a): jump head(15)
    /// head(i):  p = a + i*4; check_bounds cap_of(p), p; v = load p
    ///           br v == 0 -> done, more
    /// more:     next = i - 1; br next >= 0 -> head(next), done
    /// done:     ret
    /// ```
    ///
    /// The step is minus four, so the first access is the highest address the loop touches and every
    /// later one is below it. What the pass has to ask about is room under the first access rather
    /// than over it, which is `cap_extent_back` at the end of that access. See #680.
    fn downwards() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let counter = func.append_param(head, Type::int(64));

        let last = Builder::new(&mut func, entry).iconst(Type::int(64), TRIPS - 1);
        Builder::new(&mut func, entry).jump(head, &[last]);

        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let read = build.load(Type::int(32), pointer, mem(), Flags::NONE);
        let nothing = build.iconst(Type::int(32), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, done, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Sub, counter, one, Flags::NSW);
        let floor = build.iconst(Type::int(64), 0);
        let again = build.icmp(IntPred::Sge, next, floor);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, done])
    }

    /// A scanner whose pointer moves by one byte or by two, depending on what it just read.
    ///
    /// ```text
    /// entry(a): jump head(a)
    /// head(p):  check_bounds cap_of(p), p; v = load p
    ///           br v == 0 -> done, more
    /// more:     br v < 0 -> two, one
    /// one:      jump back(p + 1)
    /// two:      jump back(p + 2)
    /// back(q):  jump head(q)
    /// done:     ret
    /// ```
    ///
    /// What a UTF-8 walk looks like, and what half of SQLite's text handling looks like. There is no
    /// step to speak of, so scalar evolution says nothing and the guard has to measure how far the
    /// pointer got rather than count how far it should have got. See #810.
    fn by_what_it_read() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let one = func.create_block();
        let two = func.create_block();
        let back = func.create_block();
        let done = func.create_block();
        let text = func.append_param(entry, Type::PTR);
        let at = func.append_param(head, Type::PTR);
        let next = func.append_param(back, Type::PTR);

        Builder::new(&mut func, entry).jump(head, &[text]);

        let mut build = Builder::new(&mut func, head);
        checking(&mut build, at, byte());
        let read = build.load(Type::int(8), at, byte(), Flags::NONE);
        let nothing = build.iconst(Type::int(8), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, done, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let wide = build.icmp(IntPred::Slt, read, nothing);
        build.br_if(wide, two, &[], one, &[]);

        for (block, step) in [(one, 1), (two, 2)] {
            let mut build = Builder::new(&mut func, block);
            let by = build.iconst(Type::int(64), step);
            let args = build.func().push_values(&[at, by]);
            let far = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
            build.jump(back, &[far]);
        }

        Builder::new(&mut func, back).jump(head, &[next]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, one, two, back, done])
    }

    /// A walk whose address is a pointer the header carries plus an index it also carries.
    ///
    /// ```text
    /// entry(a, n): jump head(a, 0)
    /// head(p, i):  x = i & 7; q = p + x
    ///              check_bounds cap_of(q), q
    ///              j = i + 1; f = p + 8
    ///              br j < n -> head(f, j), done
    /// done:        ret
    /// ```
    ///
    /// The `and` is what stops scalar evolution: `i` walks by one and `i & 7` does not walk by
    /// anything, so the address is not an induction variable and nothing counts it. It is still a
    /// function of what the header carries, so the guard can write the two instructions out again
    /// from its own parameters and the preheader can write them out again from what it passes. See
    /// #810.
    ///
    /// A `load` in place of the `and` is the same fixture with the answer the other way, which is
    /// `x_came_out_of_memory` below.
    fn from_what_it_carries(reading: bool) -> (Interner, Func, Vec<Block>) {
        let word = Type::int(64);
        let mut names = Interner::new();
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR, word]));
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        let text = func.append_param(entry, Type::PTR);
        let count = func.append_param(entry, word);
        let at = func.append_param(head, Type::PTR);
        let index = func.append_param(head, word);

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(word, 0);
        build.jump(head, &[text, zero]);

        let mut build = Builder::new(&mut func, head);
        let spread = if reading {
            build.load(word, at, mem(), Flags::NONE)
        } else {
            let mask = build.iconst(word, 7);
            build.binary(Opcode::And, index, mask, Flags::NONE)
        };
        let args = build.func().push_values(&[at, spread]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        checking(&mut build, pointer, byte());
        let one = build.iconst(word, 1);
        let next = build.binary(Opcode::Add, index, one, Flags::NSW);
        let by = build.iconst(word, 8);
        let args = build.func().push_values(&[at, by]);
        let far = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        let again = build.icmp(IntPred::Slt, next, count);
        build.br_if(again, head, &[far, next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, done])
    }

    /// A walk down a linked list, where the next pointer is read out of the current node.
    ///
    /// ```text
    /// entry(a): jump head(a)
    /// head(p):  check_bounds cap_of(p), p; v = load p
    ///           br v == 0 -> done, more
    /// more:     q = load p + 8; jump head(q)
    /// done:     ret
    /// ```
    ///
    /// The case measuring does not take. Not because subtracting the two nodes would be wrong, but
    /// because the second one is its own object, so the guard would send every iteration after the
    /// first to the slow half and the split would be two copies of the loop for nothing. See #810.
    fn down_a_list() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let done = func.create_block();
        let list = func.append_param(entry, Type::PTR);
        let at = func.append_param(head, Type::PTR);

        Builder::new(&mut func, entry).jump(head, &[list]);

        let mut build = Builder::new(&mut func, head);
        checking(&mut build, at, byte());
        let read = build.load(Type::int(8), at, byte(), Flags::NONE);
        let nothing = build.iconst(Type::int(8), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, done, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let by = build.iconst(Type::int(64), 8);
        let args = build.func().push_values(&[at, by]);
        let field = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        let next = build.load(Type::PTR, field, mem(), Flags::NONE);
        build.jump(head, &[next]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, done])
    }

    /// Builds the loop, with the exit test against a number or against a second parameter.
    /// The same loop as [`walking`], with the counter starting at a number the caller handed in.
    ///
    /// `a[start + i]` for `i` from nothing up to `TRIPS`, which is the shape an inner loop over a
    /// row of a matrix has once the outer loop's subscript is folded into the start. What it gives
    /// the pass is a walk whose displacement off the array is a value rather than a number.
    fn offsetting(flags: Flags) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let word = Type::int(64);
        let params = vec![Type::PTR, word];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let start = func.append_param(entry, word);
        let counter = func.append_param(head, word);

        Builder::new(&mut func, entry).jump(head, &[start]);

        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(word, WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let read = build.load(Type::int(32), pointer, mem(), Flags::NONE);
        let nothing = build.iconst(Type::int(32), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, done, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let one = build.iconst(word, 1);
        let next = build.binary(Opcode::Add, counter, one, flags);
        let times = build.iconst(word, TRIPS);
        let limit = build.binary(Opcode::Add, start, times, Flags::NSW);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, done])
    }

    fn walking(times: Option<i128>, flags: Flags) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut params = vec![Type::PTR];
        params.extend(times.is_none().then_some(Type::int(64)));
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let handed = times.is_none().then(|| func.append_param(entry, Type::int(64)));
        let counter = func.append_param(head, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let read = build.load(Type::int(32), pointer, mem(), Flags::NONE);
        let nothing = build.iconst(Type::int(32), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, done, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, flags);
        let limit = match (times, handed) {
            (Some(times), _) => build.iconst(Type::int(64), times),
            (None, handed) => handed.expect("a loop with no number for a limit was handed one"),
        };
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, done])
    }

    /// Builds a loop with two ways out that meet again, so neither way out dominates the meeting.
    ///
    /// A parameter at each exit is what section 26.4 asks for and it does not reach this on its own.
    /// Both exits grow one and a use at the join still names the value the loop defined, because a
    /// parameter is only a name where its block dominates.
    fn joining() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let params = [Type::PTR, Type::int(64)];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let left = func.create_block();
        let right = func.create_block();
        let join = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let handed = func.append_param(entry, Type::int(64));
        let counter = func.append_param(head, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let read = build.load(Type::int(32), pointer, mem(), Flags::NONE);
        let nothing = build.iconst(Type::int(32), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, left, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let again = build.icmp(IntPred::Slt, next, handed);
        build.br_if(again, head, &[next], right, &[]);

        Builder::new(&mut func, left).jump(join, &[]);
        Builder::new(&mut func, right).jump(join, &[]);
        Builder::new(&mut func, join).ret(&[]);
        (names, func, vec![entry, head, more, left, right, join])
    }

    /// Builds two loops one after the other, the second starting from where the first stopped.
    ///
    /// The guard the second one gets is worked out from where its walk starts, which is a value the
    /// first loop defines, and splitting the first loop is what stops that being one value.
    fn one_after_another() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let params = [Type::PTR, Type::int(64)];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let over = func.create_block();
        let next = func.create_block();
        let again = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let limit = func.append_param(entry, Type::int(64));
        let first = func.append_param(head, Type::int(64));
        let second = func.append_param(next, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let args = build.func().push_values(&[array, first]);
        let at = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        checking(&mut build, at, byte());
        let read = build.load(Type::int(8), at, byte(), Flags::NONE);
        let nothing = build.iconst(Type::int(8), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, over, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let one = build.iconst(Type::int(64), 1);
        let step = build.binary(Opcode::Add, first, one, Flags::NSW);
        build.jump(head, &[step]);

        Builder::new(&mut func, over).jump(next, &[first]);

        let mut build = Builder::new(&mut func, next);
        let args = build.func().push_values(&[array, second]);
        let here = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        checking(&mut build, here, byte());
        let seen = build.load(Type::int(8), here, byte(), Flags::NONE);
        let blank = build.iconst(Type::int(8), 32);
        let over_too = build.icmp(IntPred::Eq, seen, blank);
        build.br_if(over_too, done, &[], again, &[]);

        let mut build = Builder::new(&mut func, again);
        let one = build.iconst(Type::int(64), 1);
        let onward = build.binary(Opcode::Add, second, one, Flags::NSW);
        let go = build.icmp(IntPred::Slt, onward, limit);
        build.br_if(go, next, &[onward], done, &[]);

        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, over, next, again, done])
    }

    /// Builds a loop with a loop inside it, each of them reading the array it was handed.
    ///
    /// The outer loop reads one element per outer iteration, which is a check in its own blocks. The
    /// inner loop reads one per inner iteration, and whether that one is checked is the argument, so
    /// that the same nest can be a nest whose inner loop is worth splitting and one whose is not.
    fn nested(inner_reads: bool) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let params = [Type::PTR, Type::int(64), Type::int(64)];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let outer = func.create_block();
        let inner = func.create_block();
        let round = func.create_block();
        let after = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let rows = func.append_param(entry, Type::int(64));
        let columns = func.append_param(entry, Type::int(64));
        let row = func.append_param(outer, Type::int(64));
        let column = func.append_param(inner, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(outer, &[zero]);

        let mut build = Builder::new(&mut func, outer);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, row, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let at = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, at);
        build.load(Type::int(32), at, mem(), Flags::NONE);
        let start = build.iconst(Type::int(64), 0);
        build.jump(inner, &[start]);

        let mut build = Builder::new(&mut func, inner);
        let wide = build.iconst(Type::int(64), WIDTH);
        let along = build.binary(Opcode::Mul, column, wide, Flags::NSW);
        let args = build.func().push_values(&[array, along]);
        let here = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        if inner_reads {
            check(&mut build, here);
            build.load(Type::int(32), here, mem(), Flags::NONE);
        }
        build.jump(round, &[]);

        let mut build = Builder::new(&mut func, round);
        let one = build.iconst(Type::int(64), 1);
        let onward = build.binary(Opcode::Add, column, one, Flags::NSW);
        let more = build.icmp(IntPred::Slt, onward, columns);
        build.br_if(more, inner, &[onward], after, &[]);

        let mut build = Builder::new(&mut func, after);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, row, one, Flags::NSW);
        let again = build.icmp(IntPred::Slt, next, rows);
        build.br_if(again, outer, &[next], done, &[]);

        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, outer, inner, round, after, done])
    }

    /// Builds a nest whose outer loop reads at an address the inner loop worked out.
    ///
    /// The check is in the outer loop's own blocks, so it is one the outer guard would speak for,
    /// but the offset it reads at is defined inside the inner loop. That value is not the same
    /// number wherever it is read and it is not a parameter of the outer header, so neither the
    /// guard nor the preheader has it in hand, and naming it in either of them names something that
    /// does not reach there.
    fn reading_what_the_inner_loop_found() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let params = [Type::PTR, Type::int(64), Type::int(64)];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let outer = func.create_block();
        let inner = func.create_block();
        let after = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let rows = func.append_param(entry, Type::int(64));
        let columns = func.append_param(entry, Type::int(64));
        let row = func.append_param(outer, Type::int(64));
        let column = func.append_param(inner, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(outer, &[zero]);

        let start = Builder::new(&mut func, outer).iconst(Type::int(64), 0);
        Builder::new(&mut func, outer).jump(inner, &[start]);

        let mut build = Builder::new(&mut func, inner);
        let one = build.iconst(Type::int(64), 1);
        let onward = build.binary(Opcode::Add, column, one, Flags::NSW);
        let more = build.icmp(IntPred::Slt, onward, columns);
        build.br_if(more, inner, &[onward], after, &[]);

        let mut build = Builder::new(&mut func, after);
        let args = build.func().push_values(&[array, onward]);
        let at = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        checking(&mut build, at, byte());
        build.load(Type::int(8), at, byte(), Flags::NONE);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, row, one, Flags::NSW);
        let again = build.icmp(IntPred::Slt, next, rows);
        build.br_if(again, outer, &[next], done, &[]);

        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, outer, inner, after, done])
    }

    /// What one access in the loop covers.
    fn mem() -> MemInfo {
        MemInfo {
            size: WIDTH as u64,
            align: WIDTH as u32,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        }
    }

    /// What one access covers in a loop that walks a byte at a time.
    ///
    /// A walk the guard has to measure has to be over something wanting no alignment, because a step
    /// nobody wrote down is a step nothing can divide by the alignment. Which is what the loops this
    /// reaches look like anyway: they are scanners over text.
    fn byte() -> MemInfo {
        MemInfo {
            size: 1,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        }
    }

    /// Puts `cap_of` and a `check_bounds` at `pointer` into a block.
    ///
    /// The shape `rucc-safety` emits, written out here rather than reached for, because `rucc-opt`
    /// is rank 9 alongside `rucc-safety` and cannot depend on it.
    fn check(build: &mut Builder<'_>, pointer: Value) {
        checking(build, pointer, mem());
    }

    /// The same, for an access of some other width.
    fn checking(build: &mut Builder<'_>, pointer: Value, info: MemInfo) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = build.func().push_values(&[capability, pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
    }

    /// Puts the `cap_of` and the `check_deriv` `rucc-safety` writes behind pointer arithmetic into
    /// a block, naming `from` as the pointer the arithmetic started from.
    ///
    /// Built at the end of the block and then moved in front of the terminator, which is what the
    /// builder makes easy and is where a check on an address the block works out belongs anyway.
    fn deriving(func: &mut Func, block: Block, from: Value, derived: Value) {
        let term = func.terminator(block).expect("the block ends in a branch");
        let held: Vec<Inst> = func.insts(block).collect();
        let mut build = Builder::new(func, block);
        let args = build.func().push_values(&[from]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let stride = build.iconst(Type::int(64), WIDTH);
        let args = build.func().push_values(&[capability, from, derived, stride]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckDeriv) }, &[]);
        let added: Vec<Inst> = func.insts(block).filter(|inst| !held.contains(inst)).collect();
        for inst in added {
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
    }

    /// A second walk off the same pointer, stepping by `step` bytes a time round.
    ///
    /// The counter the header carries scaled by something other than the stride the loop already
    /// walks by, which is a pointer following the same anchor at a rate of its own.
    fn beside(func: &mut Func, block: Block, from: Value, step: i128) -> Value {
        let term = func.terminator(block).expect("the block ends in a branch");
        let mul = func
            .insts(block)
            .find(|&inst| func[inst].opcode == Opcode::Mul)
            .expect("the loop scales its counter");
        let counter = func[func[mul].args][0];
        let mut build = Builder::new(func, block);
        let by = build.iconst(Type::int(64), step);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[from, scaled]);
        let along = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        for value in [by, scaled, along] {
            let inst = super::inst_of(func, value);
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
        along
    }

    /// One stride past a pointer, worked out in front of the block's terminator.
    fn stepped(func: &mut Func, block: Block, from: Value) -> Value {
        let term = func.terminator(block).expect("the block ends in a branch");
        let mut build = Builder::new(func, block);
        let by = build.iconst(Type::int(64), WIDTH);
        let args = build.func().push_values(&[from, by]);
        let along = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        for value in [by, along] {
            let inst = super::inst_of(func, value);
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
        along
    }

    /// The address the loop works out and the pointer it started from.
    fn arithmetic(func: &Func, block: Block) -> (Value, Value) {
        let add = func
            .insts(block)
            .find(|&inst| func[inst].opcode == Opcode::PtrAdd)
            .expect("the loop works out an address");
        let from = func[func[add].args][0];
        let derived = func[add].results().next().expect("a ptr_add gives one pointer");
        (from, derived)
    }

    /// Canonicalizes and then splits, with as much fuel as both want.
    ///
    /// Both, because the pass is written against the shape [`Canon`] leaves, and it is
    /// canonicalization that gives the loop the preheader the limit is worked out in.
    fn split_up(func: &mut Func) -> Stats {
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(func, &mut an, &mut Fuel::unlimited());
        Split.run(func, &mut an, &mut Fuel::unlimited())
    }

    #[test]
    fn a_loop_whose_result_is_read_after_it_is_put_back_into_closed_form_first() {
        // Canonicalization runs a long way in front of this pass and `simplify-cfg` between the two
        // undoes some of what it did, which is why the loop here is canonicalized and then broken.
        // Both halves would define the value the code after the loop reads, so the pass repairs the
        // one loop it is about to copy rather than refusing it or running canonicalization again.
        let (mut names, mut func, blocks) = leaving();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());

        let (head, done) = (blocks[1], blocks[3]);
        let read = func
            .insts(head)
            .find(|&inst| func[inst].opcode == Opcode::Load)
            .and_then(|inst| func[inst].results().next())
            .expect("the loop loads what it walks over");
        let term = func.terminator(done).expect("the block after the loop returns");
        let sum = Builder::new(&mut func, done).binary(Opcode::Add, read, read, Flags::NONE);
        let inst = super::inst_of(&func, sum);
        func.remove_inst(inst);
        func.insert_before(inst, term);
        an.clear();

        let stats = Split.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, super::CLOSED_HERE), 1);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(func[done].params.len(), 1, "the block after the loop took the value in");
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");
        sound(&func, &mut names);
    }

    #[test]
    fn a_value_read_past_a_join_that_neither_way_out_dominates_is_handed_over_there_as_well() {
        // Two ways out of the loop and they meet again, so a parameter at each of them is a name
        // the code at the meeting cannot say. The repair puts one there too, which is where the
        // iterated dominance frontier comes in, and both halves then hand their own value along.
        let (mut names, mut func, blocks) = joining();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());

        let (head, join) = (blocks[1], blocks[5]);
        let read = func
            .insts(head)
            .find(|&inst| func[inst].opcode == Opcode::Load)
            .and_then(|inst| func[inst].results().next())
            .expect("the loop loads what it walks over");
        let term = func.terminator(join).expect("the block the two ways out meet at returns");
        let sum = Builder::new(&mut func, join).binary(Opcode::Add, read, read, Flags::NONE);
        let inst = super::inst_of(&func, sum);
        func.remove_inst(inst);
        func.insert_before(inst, term);
        an.clear();

        let stats = Split.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, super::CLOSED_HERE), 1);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(stats.count(Kind::Missed, super::ESCAPES), 0);
        assert_eq!(func[join].params.len(), 1, "the meeting took the value in as well");
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");
        sound(&func, &mut names);
    }

    #[test]
    fn a_loop_whose_value_the_next_loops_guard_names_is_left_alone() {
        // The second loop starts where the first one stopped, so its guard names a value the first
        // loop's body defines. Splitting the first loop would leave that value with one definition
        // per half and the guard naming neither, and the repair cannot help because the guard is
        // not written down yet. Without the refusal the verifier reports the guard's address as a
        // value that arrives at a block and does not reach the use, which is what SQLite hit.
        let (mut names, mut func, _) = one_after_another();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());

        let stats = Split.run(&mut func, &mut an, &mut Fuel::unlimited());
        sound(&func, &mut names);
        assert_eq!(stats.count(Kind::Missed, super::WANTED_ELSEWHERE), 1);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
    }

    #[test]
    fn a_loop_with_a_loop_inside_it_is_split() {
        // Nothing about an inner loop makes the copy wrong. The whole nest is copied, the guard goes
        // in front of the outer header, and the check in the outer loop's own blocks comes out of
        // the fast half. The inner loop reads nothing here, so it plans nothing and does not compete
        // with the outer one for the blocks they have in common.
        let (mut names, mut func, _) = nested(false);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(stats.count(Kind::Missed, super::NESTED_WITH_ONE), 0);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");
        sound(&func, &mut names);
    }

    #[test]
    fn the_inner_loop_is_the_one_split_when_both_of_them_could_be() {
        // Both loops plan, and the two plans name the inner loop's blocks between them, so only one
        // of them may run. The inner one is kept: its checks run once per inner iteration rather
        // than once per outer one, and it is the smaller thing to copy. The outer one is left for
        // the next run of the pipeline.
        let (mut names, mut func, _) = nested(true);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(stats.count(Kind::Missed, super::NESTED_WITH_ONE), 1);
        assert_eq!(stats.count(Kind::Missed, super::INSIDE_A_LOOP), 1);
        sound(&func, &mut names);
    }

    #[test]
    fn a_value_the_inner_loop_defined_is_not_one_the_guard_may_write_again() {
        // The check is in the outer loop's own blocks, so the guard would speak for it, and the
        // offset it reads at came out of the inner loop. Reading that value again in the preheader
        // is not writing it again, because it is not the same number wherever it is read, and it is
        // not a parameter of the outer header either, so it is neither of the two things the walk
        // stops at. Treating it as the first of them puts a name in the guard that does not reach
        // there, which the verifier catches, so the address is refused and the check stays.
        //
        // Canonicalization is what would otherwise hide this, since the repair gives the block after
        // the inner loop a parameter for the value and the address then names that instead. It is
        // left out here for that reason, and the loop has its preheader written into the fixture.
        let (mut names, mut func, _) = reading_what_the_inner_loop_found();
        let mut an = crate::machine::fixtures::analyses();
        let stats = Split.run(&mut func, &mut an, &mut Fuel::unlimited());
        // Soundness first, because it is the stronger of the two: without the refusal the guard and
        // the preheader both name the inner loop's value and the verifier says so at each of them.
        sound(&func, &mut names);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 0);
    }

    /// What a value is, when it is a number written down.
    fn number(func: &Func, value: Value) -> Option<i128> {
        let inst = crate::trip::inst_of(func, value);
        if func[inst].opcode != Opcode::IConst {
            return None;
        }
        let Extra::Imm(imm) = func[inst].extra else { return None };
        Some(func[imm].signed(func[value].ty))
    }

    /// Every instruction in the function with this opcode, and the block it is in.
    fn all(func: &Func, opcode: Opcode) -> Vec<(Block, Inst)> {
        func.blocks()
            .flat_map(|block| func.insts(block).map(move |inst| (block, inst)).collect::<Vec<_>>())
            .filter(|&(_, inst)| func[inst].opcode == opcode)
            .collect()
    }

    /// Insists the function is one the rest of the compiler may believe.
    ///
    /// This is what the tests here rest on. The pass makes a second copy of a loop, gives a new
    /// block parameters that stand for the old header's, and moves a preheader's worth of
    /// arithmetic in front of a terminator that was already there, so whether every value is in
    /// scope where it is read is not something reading the code settles.
    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    #[test]
    fn a_loop_that_can_stop_early_is_split_even_though_hoisting_will_not_touch_it() {
        // The census row this pass was written for. Of the checks SQLite still carries at -O2, the
        // largest group by far is in loops with a second way out, which is exactly the loop here.
        let (mut names, mut func, _) = leaving();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());
        let refused = crate::hoist::Hoist.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert!(!refused.changed(), "hoisting has nothing to say about this loop");

        let stats = Split.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        sound(&func, &mut names);
    }

    #[test]
    fn the_half_the_loop_runs_first_has_no_check_in_it_and_the_other_one_keeps_it() {
        // One check went in and one check came out, and the one that came out is in the copy. That
        // is the whole transformation: the same work, with the checking half reached only once the
        // guard says the run of safe iterations is over.
        let (mut names, mut func, blocks) = leaving();
        let head = blocks[1];
        split_up(&mut func);

        let left = all(&func, Opcode::CheckBounds);
        assert_eq!(left.len(), 1, "one check, and it is the one the slow half kept");
        assert_ne!(left[0].0, head, "and it is not in the block the loop started in");
        sound(&func, &mut names);
    }

    #[test]
    fn the_derivation_check_on_an_index_that_walks_goes_the_way_the_bounds_check_beside_it_goes() {
        // `a[i]` is two judgements, one about the arithmetic and one about the access, and the
        // window covers both. It covers the arithmetic more easily than the access, since a
        // derivation is allowed to land anywhere the access is allowed to and a stride short of
        // that as well. Until the guard spoke for it this was the whole of what the fast half of a
        // byte at a time loop still had in it.
        let (mut names, mut func, blocks) = walking(Some(TRIPS), Flags::NSW);
        let (from, derived) = arithmetic(&func, blocks[1]);
        deriving(&mut func, blocks[1], from, derived);

        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");
        assert_eq!(all(&func, Opcode::CheckDeriv).len(), 1, "and the derivation check with it");
        sound(&func, &mut names);
    }

    #[test]
    fn two_checks_on_one_address_ask_the_runtime_one_question() {
        // tamnd/rucc#871. `a[i]` carries a bounds check and a derivation check and the guard sizes
        // both of them from the same address, so the preheader called the runtime twice about it.
        // What made the two calls different was how many bytes each one said the loop was going to
        // read, and that stopped meaning anything when the query stopped walking, so both ask for
        // everything now and the second is a value the preheader already has. On `a-string-scan` it
        // was five calls at one address.
        let (mut names, mut func, blocks) = walking(Some(TRIPS), Flags::NSW);
        let (from, derived) = arithmetic(&func, blocks[1]);
        deriving(&mut func, blocks[1], from, derived);

        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CapExtent).len(), 1, "one question for the two checks");
        sound(&func, &mut names);
    }

    #[test]
    fn a_derivation_check_whose_walk_starts_along_from_the_pointer_it_is_about_is_taken() {
        // `&a[i] + 1` walks from a stride past `a`, so a window measured where the walk begins is a
        // window about whoever owns that address rather than about whoever owns `a`. Measuring from
        // `a` instead and widening the window by the stride answers both: `a` is in it on the first
        // iteration and the walk is in it on every one.
        let (mut names, mut func, blocks) = walking(Some(TRIPS), Flags::NSW);
        let head = blocks[1];
        let (from, walked) = arithmetic(&func, head);
        let past = stepped(&mut func, head, walked);
        deriving(&mut func, head, from, past);

        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOT_FROM_THE_START), 0);
        assert_eq!(all(&func, Opcode::CheckDeriv).len(), 1, "the fast half lost the check");
        sound(&func, &mut names);
    }

    #[test]
    fn a_derivation_check_whose_pointer_sits_above_the_walk_is_taken() {
        // The same thing the other way round. The pointer the check names is a stride past `a` and
        // the walk starts on `a`, so the lower of the two is where the walk begins and the window
        // is as wide as the gap. Which of the pair is the one that moves does not come into it.
        let (mut names, mut func, blocks) = walking(Some(TRIPS), Flags::NSW);
        let head = blocks[1];
        let (array, walked) = arithmetic(&func, head);
        let above = stepped(&mut func, head, array);
        deriving(&mut func, head, above, walked);

        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOT_FROM_THE_START), 0);
        assert_eq!(all(&func, Opcode::CheckDeriv).len(), 1, "the fast half lost the check");
        sound(&func, &mut names);
    }

    #[test]
    fn a_derivation_check_whose_walk_begins_a_handed_distance_into_the_object_is_taken() {
        // `a[start + i]`, where the check is about `a` and the walk begins `start` elements in. The
        // window goes on `a`, which is the object the check is about, and the guard takes the gap
        // off what it measured there and asks for the gap to be at or above zero.
        let (mut names, mut func, blocks) = offsetting(Flags::NSW);
        let head = blocks[1];
        let (array, walked) = arithmetic(&func, head);
        deriving(&mut func, head, array, walked);

        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOT_FROM_THE_START), 0);
        assert_eq!(all(&func, Opcode::CheckDeriv).len(), 1, "the fast half lost the check");
        sound(&func, &mut names);
    }

    #[test]
    fn a_derivation_check_whose_pointer_moves_and_whose_walk_begins_a_handed_distance_in_stays() {
        // Both pointers walk and the distance off the array is not a number, so neither window is
        // available: the pair cannot be measured against each other and the one that would go on
        // the pointer the check names needs that pointer to stand still. It is the gap left over.
        let (mut names, mut func, blocks) = offsetting(Flags::NSW);
        let head = blocks[1];
        let (_, walked) = arithmetic(&func, head);
        let past = stepped(&mut func, head, walked);
        deriving(&mut func, head, walked, past);

        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOT_FROM_THE_START), 1);
        assert_eq!(all(&func, Opcode::CheckDeriv).len(), 2, "the check is in both halves");
        sound(&func, &mut names);
    }

    #[test]
    fn a_derivation_check_whose_pointer_walks_at_a_step_of_its_own_stays() {
        // The case neither window speaks for. The pointer the check names runs away at twice the
        // rate the walk does, so the distance between the two is a different number every time
        // round and no window a number of bytes wide holds the pair for more than one iteration.
        let (mut names, mut func, blocks) = walking(Some(TRIPS), Flags::NSW);
        let head = blocks[1];
        let (array, walked) = arithmetic(&func, head);
        let faster = beside(&mut func, head, array, 2 * WIDTH);
        deriving(&mut func, head, faster, walked);

        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOT_FROM_THE_START), 1);
        assert_eq!(all(&func, Opcode::CheckDeriv).len(), 2, "the check is in both halves");
        sound(&func, &mut names);
    }

    #[test]
    fn a_derivation_check_whose_own_pointer_walks_beside_the_new_one_is_taken_by_one_window() {
        // `p = p + k`, where the pointer the check names is the one that moves, so no window
        // measured from a single address speaks for it. One measured from the lower of the two and
        // a step and a byte wide holds the pair wherever the walk has got to, and that says the old
        // pointer is inside the object and the new one did not leave it. This is `a-string-scan`,
        // where the derivation check was the whole of what the fast half still had.
        let (mut names, mut func, blocks) = walking(Some(TRIPS), Flags::NSW);
        let head = blocks[1];
        let (_, walked) = arithmetic(&func, head);
        let past = stepped(&mut func, head, walked);
        deriving(&mut func, head, walked, past);

        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOT_FROM_THE_START), 0);
        assert_eq!(all(&func, Opcode::CheckDeriv).len(), 1, "the fast half lost it");
        sound(&func, &mut names);
    }

    #[test]
    fn how_far_the_runtime_is_asked_to_look_is_settled_in_front_of_the_loop() {
        // The one thing a compiler cannot work out here is how many bytes belong to the object, so
        // it is asked, once, before the loop starts. Once is what makes this worth doing: a query
        // per loop in place of a check per iteration.
        let (mut names, mut func, _) = leaving();
        split_up(&mut func);

        let asked = all(&func, Opcode::CapExtent);
        assert_eq!(asked.len(), 1, "one question for the one check that was sized");
        let cfg = crate::Cfg::new(&func);
        let doms = crate::Dominators::new(&cfg);
        let loops = crate::Loops::new(&cfg, &doms);
        assert!(
            loops.all().all(|id| !loops.contains(id, asked[0].0)),
            "and it is outside the loop"
        );
        sound(&func, &mut names);
    }

    #[test]
    fn a_walk_that_starts_at_an_index_the_caller_handed_in_is_split() {
        // #810. The first address is `a + 4 * start` and the question has to be put about that
        // address rather than about the array, because an extent measured from the array covers
        // bytes in front of where the loop begins and would say the walk fits when it does not.
        let (mut names, mut func, blocks) = from_an_index();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        let asked = all(&func, Opcode::CapExtent);
        assert_eq!(asked.len(), 1, "one question for the one check that was sized");
        let at = func[func[asked[0].1].args][1];
        let inst = super::inst_of(&func, at);
        assert_eq!(func[inst].opcode, Opcode::PtrAdd, "the question is asked about a displacement");
        assert_eq!(func[func[inst].args][0], func[blocks[0]].params[0], "off the array");
        sound(&func, &mut names);
    }

    #[test]
    fn a_walk_over_a_file_scope_array_is_split_and_the_address_is_written_out_again() {
        // #810. The address of a global is a link time constant, so it does not change inside a
        // loop wherever the instruction that works it out happens to sit. The question in front of
        // the loop gets a `global_addr` of its own rather than reading the one inside, which is one
        // instruction and is the same trade `crate::licm` already makes for these.
        let (mut names, mut func, _) = over_a_global();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        let asked = all(&func, Opcode::CapExtent);
        assert_eq!(asked.len(), 1, "one question for the one check that was sized");
        let at = func[func[asked[0].1].args][1];
        let inst = super::inst_of(&func, at);
        assert_eq!(func[inst].opcode, Opcode::GlobalAddr, "asked about the array itself");

        let cfg = crate::Cfg::new(&func);
        let doms = crate::Dominators::new(&cfg);
        let loops = crate::Loops::new(&cfg, &doms);
        let addresses = all(&func, Opcode::GlobalAddr);
        assert_eq!(addresses.len(), 3, "one in each half of the loop and one in front of them");
        assert_eq!(
            addresses
                .iter()
                .filter(|&&(block, _)| loops.all().all(|id| !loops.contains(id, block)))
                .count(),
            1,
            "and the one in front is outside every loop, which is where the question is asked",
        );
        sound(&func, &mut names);
    }

    #[test]
    fn a_walk_whose_step_is_not_a_number_is_split_and_the_guard_measures_how_far_it_got() {
        // #810. The pointer moves by one or by two and nothing knows which, so there is no step to
        // carry and no count to keep. What the guard can do instead is subtract: where the pointer
        // is now, less where it was on the way in, is the displacement itself rather than a number
        // standing in for it, so the same window and the same rule apply unchanged.
        let (mut names, mut func, blocks) = by_what_it_read();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        let asked = all(&func, Opcode::CapExtent);
        assert_eq!(asked.len(), 1, "one question for the one check that was sized");
        assert_eq!(
            func[func[asked[0].1].args][1], func[blocks[0]].params[0],
            "asked about the pointer the loop was handed, which is where the walk begins",
        );

        let measured = all(&func, Opcode::PtrToInt);
        assert_eq!(measured.len(), 2, "where the pointer began and where it is now");
        sound(&func, &mut names);
    }

    #[test]
    fn a_guard_that_measures_carries_nothing_round_the_loop() {
        // The measured offset costs less than the counted one rather than more. It is worked out
        // from a pointer the loop already hands itself, so the guard needs no parameter for it and
        // the latch needs no add, and what is left is one subtraction where there was a block
        // parameter and an increment.
        let (mut names, mut func, _) = by_what_it_read();
        split_up(&mut func);

        let cfg = crate::Cfg::new(&func);
        let doms = crate::Dominators::new(&cfg);
        let loops = crate::Loops::new(&cfg, &doms);
        let guard = loops
            .all()
            .map(|id| loops.header(id))
            .find(|&block| func.insts(block).any(|inst| func[inst].opcode == Opcode::PtrToInt))
            .expect("the guard is the header of the loop it took over");
        assert_eq!(func[guard].params.len(), 1, "the pointer the header carried, and nothing else");
        sound(&func, &mut names);
    }

    #[test]
    fn an_address_built_out_of_what_the_header_carries_is_written_again_in_the_guard() {
        // #810. `p + (i & 7)` is not an induction variable and scalar evolution has nothing to say
        // about it, and it is not a fixed distance from a pointer either, so measuring where the
        // pointer went does not reach it. It is still a function of the two parameters the header
        // carries, so both the guard and the preheader can write the two instructions out again
        // from what each of them already has, and then the subtraction is the one that was already
        // here.
        let (mut names, mut func, blocks) = from_what_it_carries(false);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        let masks = all(&func, Opcode::And);
        assert_eq!(masks.len(), 4, "one per half, one in the guard and one in the preheader");
        let inside: Vec<Block> = masks.iter().map(|&(block, _)| block).collect();
        assert!(inside.contains(&blocks[0]), "the preheader works the first address out");

        let asked = all(&func, Opcode::CapExtent);
        assert_eq!(asked.len(), 1, "one question, in front of the loop");
        assert_eq!(asked[0].0, blocks[0], "asked in the preheader about the first address");
        let measured = all(&func, Opcode::PtrToInt);
        assert_eq!(measured.len(), 2, "where the address began and where it is now");
        sound(&func, &mut names);
    }

    #[test]
    fn an_address_built_on_something_read_out_of_memory_is_left_alone() {
        // The same loop with a load where the mask was. A second copy of a load in the guard is a
        // second read at another moment, which is not the same number, and a copy of it in the
        // preheader is a read on a loop that may run no iterations at all. So the address stops
        // being something either block could work out and the check stays in both halves.
        let (mut names, mut func, _) = from_what_it_carries(true);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 0, "the loop is left alone");
        assert_eq!(stats.count(Kind::Missed, super::NOT_FOLLOWED), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "and the check stays where it was");
        assert!(all(&func, Opcode::CapExtent).is_empty(), "with nothing asked in front of it");
        sound(&func, &mut names);
    }

    #[test]
    fn a_walk_down_a_linked_list_is_left_alone() {
        // Splitting a list would be sound and would not pay. The guard tests the difference at run
        // time, so a second node that landed inside the first one's object would pass it, but the
        // next node of a heap allocated list is its own object and the guard fails from the second
        // iteration on, leaving two copies of the loop with every check in both. What stops it is
        // the walk over the back edge, which insists the pointer is its own former self plus bytes,
        // and a load is not.
        let (mut names, mut func, _) = down_a_list();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 0, "the loop is left alone");
        assert_eq!(stats.count(Kind::Missed, super::NOT_FOLLOWED), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "and the check stays where it was");
        assert!(all(&func, Opcode::CapExtent).is_empty(), "with nothing asked in front of it");
        sound(&func, &mut names);
    }

    #[test]
    fn a_walk_the_guard_would_measure_is_left_alone_when_its_access_wants_alignment() {
        // A step nobody wrote down is a step nothing can divide by the alignment, so a measured walk
        // has no answer about whether the second access is as aligned as the first. Refusing is the
        // conservative reading and it has its own line in the census, so what it costs is a number.
        let (mut names, mut func, _) = by_what_it_read();
        for (_, inst) in all(&func, Opcode::CheckBounds) {
            let extra = Extra::Mem(func.add_mem(mem()));
            func[inst].extra = extra;
        }
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 0, "the loop is left alone");
        assert_eq!(stats.count(Kind::Missed, super::MEASURED_ALIGN), 1);
        sound(&func, &mut names);
    }

    #[test]
    fn a_walk_from_an_index_in_int_is_split_and_the_extension_is_emitted_in_front() {
        // #810, and the shape that is actually in C rather than the one that is convenient to
        // build. The chrec of `start + i` is in `int` and its base is `start`, so widening it to
        // pointer width wants `sext(start)`, which nothing in the function computes. The invariant
        // describes the extension instead and this pass emits it, once, in the preheader.
        let (mut names, mut func, blocks) = from_a_narrow_index();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        let asked = all(&func, Opcode::CapExtent);
        assert_eq!(asked.len(), 1, "one question for the one check that was sized");
        let at = func[func[asked[0].1].args][1];
        let sum = super::inst_of(&func, at);
        assert_eq!(func[sum].opcode, Opcode::PtrAdd, "the question is asked about a displacement");
        assert_eq!(func[func[sum].args][0], func[blocks[0]].params[0], "off the array");
        let widened = all(&func, Opcode::SExt);
        assert_eq!(widened.len(), 3, "one extension in each half of the loop and one in front");
        let start = func[blocks[0]].params[1];
        assert_eq!(
            widened.iter().filter(|&&(_, inst)| func[func[inst].args][0] == start).count(),
            1,
            "and the one in front is of the index the caller handed in, which the halves never take",
        );
        sound(&func, &mut names);
    }

    #[test]
    fn a_walk_from_high_to_low_is_split_and_the_question_goes_the_other_way() {
        // #680. The offset the guard carries counts bytes moved rather than bytes added, so it goes
        // up here exactly as it does in an ascending loop and the guard is the same guard. The one
        // thing that turns over is which end of the object the runtime is asked about, and it is
        // asked at the end of the first access rather than at its start so that the window is room
        // below and the rule the pass asks is the mirror of the one it asks going up.
        let (mut names, mut func, blocks) = downwards();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        assert!(all(&func, Opcode::CapExtent).is_empty(), "nothing asked about the bytes above");
        let asked = all(&func, Opcode::CapExtentBack);
        assert_eq!(asked.len(), 1, "one question for the one check that was sized");
        let at = func[func[asked[0].1].args][1];
        let end = super::inst_of(&func, at);
        assert_eq!(func[end].opcode, Opcode::PtrAdd, "asked at the end of the first access");
        let from = func[func[end].args][0];
        let first = super::inst_of(&func, from);
        assert_eq!(
            func[first].opcode,
            Opcode::PtrAdd,
            "past a first access that is a displacement"
        );
        assert_eq!(func[func[first].args][0], func[blocks[0]].params[0], "off the array");
        sound(&func, &mut names);
    }

    #[test]
    fn a_loop_with_a_call_in_it_that_might_free_is_left_alone() {
        // The extent is asked once and believed for the whole of the fast half, so anything that
        // could hand the storage back in the middle makes the answer stale and the fast half has
        // nothing left in it to notice.
        let (_, mut func, _) = calling(Flags::NONE);
        let stats = split_up(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::A_CALL_INSIDE), 1);
    }

    #[test]
    fn a_loop_with_a_call_in_it_that_cannot_free_is_split() {
        // Whether the storage can be handed back is a question about the callee, and `crate::nofree`
        // answers it before the pipeline starts. This is the largest row of the census by a long way,
        // and it is also the row where this pass and hoisting come apart the furthest: hoisting
        // refuses a call whatever it does, because it needs the loop to reach the end of what its
        // count says, and this never claims that.
        let (mut names, mut func, _) = calling(Flags::NOFREE);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");
        assert_eq!(all(&func, Opcode::Call).len(), 2, "and both halves kept the call");
        sound(&func, &mut names);
    }

    /// The loop with a call added to its latch, carrying whatever the caller says about it.
    fn calling(flags: Flags) -> (Interner, Func, Vec<Block>) {
        let (mut names, mut func, blocks) = leaving();
        let more = blocks[2];
        let term = func.terminator(more).expect("the latch branches");
        let callee = names.intern("somewhere");
        let signature = func.add_signature(Signature::new());
        let call = Builder::new(&mut func, more).call(callee, signature, &[]);
        func[call].flags |= flags;
        func.remove_inst(call);
        func.insert_before(call, term);
        (names, func, blocks)
    }

    #[test]
    fn a_check_whose_address_does_not_move_is_taken_too() {
        // One check on the array itself, every time round, alongside the one that walks. Hoisting
        // would rather have the still one, but this loop has a second way out, so hoisting will not
        // touch it and the check is still here to be taken. A step of zero is what carries it: the
        // access fits on the first iteration or on none of them, so it puts no limit on the loop.
        let (mut names, mut func, _) = standing(false);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CapExtent).len(), 1, "and both were sized by one question");
        assert_eq!(
            all(&func, Opcode::CheckBounds).len(),
            2,
            "the fast half lost both checks and the slow half kept both"
        );
        sound(&func, &mut names);
    }

    #[test]
    fn the_window_is_worked_out_without_dividing_by_anything() {
        // The reason it counts bytes rather than iterations. Iterations came out of a division by
        // the step, which is a step of zero on a check whose address does not move, and on x86 that
        // is a fault rather than a wrong number, so tamnd/rucc#818 was a program dying on the way
        // into a loop it was never going to fail in. It is also why the claim could not be a rule:
        // the divide and the multiply that went with it are what z3 would not finish on. The plan
        // here has one check of each kind, which is the shape fifty six of SQLite's two hundred and
        // sixty eight split loops have.
        let (mut names, mut func, _) = standing(false);
        split_up(&mut func);
        for opcode in [Opcode::SDiv, Opcode::UDiv] {
            assert!(all(&func, opcode).is_empty(), "{opcode:?} is left in the window arithmetic");
        }
        sound(&func, &mut names);
    }

    #[test]
    fn two_checks_that_walk_by_the_same_amount_share_one_offset() {
        // One value round the loop rather than one per check, which is what the common shape wants:
        // a loop that reads one array and writes another walks both by the same step, so they are
        // at the same offset on every iteration and the window is the smaller of the two.
        let (mut names, mut func, blocks) = twinned();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CapExtent).len(), 2, "both addresses were sized in front");

        let head = blocks[1];
        let cfg = crate::Cfg::new(&func);
        let into = cfg.predecessors(head);
        assert_eq!(into.len(), 1, "the guard is the only way into the header now");
        let guard = into[0];
        assert_eq!(
            func[guard].params.len(),
            func[head].params.len() + 1,
            "one offset, not one per check"
        );
        sound(&func, &mut names);
    }

    /// The loop with a second walking check in it, on the element after the one it reads.
    ///
    /// Two checks that move by the same amount, which is what a loop that reads one array and writes
    /// another is, and what a loop that looks one element ahead is. The window arithmetic keeps one
    /// offset for the pair of them rather than one each, and this is the fixture that says so.
    fn twinned() -> (Interner, Func, Vec<Block>) {
        let (names, mut func, blocks) = leaving();
        let (entry, head) = (blocks[0], blocks[1]);
        let array = func[entry].params[0];
        let counter = func[head].params[0];
        let term = func.terminator(head).expect("the header branches");
        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let ahead = build.binary(Opcode::Add, scaled, by, Flags::NSW);
        let args = build.func().push_values(&[array, ahead]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let made: Vec<Inst> = func.insts(head).skip_while(|&inst| inst != term).skip(1).collect();
        for inst in made {
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
        (names, func, blocks)
    }

    #[test]
    fn a_loop_where_nothing_moves_picks_its_half_once_and_counts_nothing() {
        // Half the loops this takes on SQLite are like this, and they need none of the machinery the
        // rest of them do. Which half runs is decided by the answer to a question asked in the
        // preheader, the answer does not change while the loop runs, so the way into the loop is
        // where the two halves are chosen between and there is no counter and no guard block.
        let (mut names, mut func, blocks) = standing(true);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CapExtent).len(), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        let (entry, head) = (blocks[0], blocks[1]);
        let term = func.terminator(entry).expect("the preheader still ends in something");
        assert_eq!(func[term].opcode, Opcode::BrIf, "the way in is the choice");
        assert_eq!(func[head].params.len(), 1, "and the header took on no counter");
        sound(&func, &mut names);
    }

    /// The loop with a check on the array itself added to its header, every time round.
    ///
    /// Hoisting would rather have that check, and it takes the ones in loops it is willing to touch.
    /// This loop has a second way out, so hoisting will not touch it and the check is still here.
    /// `alone` takes the walking check away, which leaves a loop where nothing moves at all.
    fn standing(alone: bool) -> (Interner, Func, Vec<Block>) {
        let (names, mut func, blocks) = leaving();
        let (entry, head) = (blocks[0], blocks[1]);
        let array = func[entry].params[0];
        let walking = all(&func, Opcode::CheckBounds);
        let term = func.terminator(head).expect("the header branches");
        let mut build = Builder::new(&mut func, head);
        check(&mut build, array);
        let made: Vec<Inst> = func.insts(head).skip_while(|&inst| inst != term).skip(1).collect();
        for inst in made {
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
        if alone {
            for (_, inst) in walking {
                func.remove_inst(inst);
            }
        }
        (names, func, blocks)
    }

    #[test]
    fn a_loop_whose_count_is_an_expression_is_split_on_what_that_expression_says() {
        // How far to look is worked out in the preheader rather than written down, out of a value
        // the loop does not change. Nothing here promises the arithmetic stays inside sixty four
        // bits, and it does not have to: a limit that wrapped is still answered with a true count
        // of the bytes that belong to the object.
        let (mut names, mut func, _) = counting();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CapExtent).len(), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");
        sound(&func, &mut names);
    }

    #[test]
    fn a_loop_nobody_counted_is_split_and_asks_for_as_much_as_the_arithmetic_carries() {
        // The difference from hoisting in one test. Hoisting refuses this loop, because the count
        // is what it sizes the check it writes with and a count nobody settled is not one it may
        // write a check from. Nothing here rests on the count: it is spent on how far to ask the
        // runtime to look, and the runtime answers with a true count of the bytes that belong to the
        // object whatever it was asked for.
        //
        // tamnd/rucc#871. What the ask used to be worked out from was a guess of ten iterations, and
        // that was a bound on how far the runtime would walk rather than anything the guard wanted.
        // tamnd/rucc#861 stopped it walking, so a loop nobody counted asks for everything and gets
        // the extent of the object at the same price a small ask would have cost.
        let (mut names, mut func, _) = uncounted();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());
        let refused = crate::hoist::Hoist.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert!(!refused.changed(), "hoisting will not size a check from a count nobody settled");

        let stats = Split.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        let asked = all(&func, Opcode::CapExtent);
        assert_eq!(asked.len(), 1, "one question for the one check that was sized");
        let want = func[func[asked[0].1].args][2];
        assert_eq!(number(&func, want), Some(i128::from(i64::MAX)), "and it asked for everything");
        sound(&func, &mut names);
    }

    #[test]
    fn the_pass_stops_when_the_fuel_runs_out() {
        // What `-fopt-fuel` is for, and the reason every transformation here goes through the
        // counter rather than round it.
        let (_, mut func, _) = leaving();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());
        let stats = Split.run(&mut func, &mut an, &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
    }
}
