//! Block frequency: how often a block runs, relative to the function it is in.
//!
//! Design: sections 11.3, 11.4 and 11.5 of `spec/optimizer/11-profile-and-frequency.md`.
//!
//! # From probabilities to frequencies
//!
//! [`crate::predict`] answers a local question, which is which way one branch goes. Almost every
//! consumer wants the global one instead: inlining, unrolling, block layout, spill placement and
//! if-conversion all ask how often this block runs compared with the function entry, and if that
//! answer is wrong they are all wrong together in a way that is very hard to attribute to anything.
//!
//! The frequency of a block is the sum, over the edges into it, of the source's frequency times the
//! probability of the edge, with the entry pinned at one. On an acyclic graph that is one pass in
//! reverse postorder. On a loop it is not, because the header's frequency depends on the latch's
//! and the latch's depends on the header's.
//!
//! Wu and Larus's answer, which is the one section 11.3 asks for, is to take the loops from the
//! inside out. For each loop, work out the cyclic probability, which is how likely the loop is to
//! go round again, and then the header runs `1 / (1 - p)` times for every entry to it, that being
//! the sum of the geometric series. The rest of the loop follows from the header by the acyclic
//! rule. So the whole computation is one walk of the loop forest to get a number per loop and then
//! one reverse-postorder walk of the function with the back edges left out, and [`Frequency`] does
//! the series and the clamp in [`Frequency::repeated_while`].
//!
//! # The two ways this breaks, and what is done about them
//!
//! A loop whose exit no predictor recognised has a cyclic probability of certainty, and one over
//! zero is not a frequency. The count is capped at [`MAX_PREDICTED_ITERATIONS`], which is section
//! 11.2's `max-predicted-iterations`, and the cap lives inside the type so that no caller can skip
//! it. A capped header is recorded, because it is the one place where the sum of what arrives does
//! not equal what is there, and the check in [`Frequencies::problems`] would otherwise report the
//! cap as a bug.
//!
//! Nested loops multiply, so frequencies overflow. That is section 11.6's first entry, and the
//! defence is that [`Frequency`] saturates rather than wrapping and says it has.
//!
//! # Irreducible regions
//!
//! A region with two entries has no header, so there is no series to sum and no well defined
//! frequency for anything in it. Document 06.4 declines to transform these and this is where the
//! consequence lands: the blocks get a frequency computed as though the edges that go backwards in
//! reverse postorder were not there, which is wrong but bounded, and they are marked so a consumer
//! can decline them. GCC does the same. [`Frequencies::is_reliable`] is the mark, and it spreads
//! forward, because a block whose frequency was computed from a wrong one is wrong too.

use rucc_cost::heuristics::{MAX_PREDICTED_ITERATIONS, PROFILE_SUM_TOLERANCE_PERCENT};
use rucc_ir::{Block, Func};

use crate::cfg::Cfg;
use crate::loops::{LoopId, Loops};
use crate::predict::{Callees, Predictions};
use crate::profile::{Frequency, Probability, Quality};

/// How often every block in a function runs, with the entry at one.
///
/// The predictions this was worked out from are kept, because every consumer of a frequency wants
/// the edge probabilities as well and because the two have to be the same pair of numbers or the
/// check in [`Frequencies::problems`] is checking one against something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frequencies {
    told: Predictions,
    of: Vec<Frequency>,
    reliable: Vec<bool>,
    capped: Vec<bool>,
    cyclic: Vec<Probability>,
    entry: Frequency,
}

impl Frequencies {
    /// Predicts every branch and then works out every block's frequency from that.
    ///
    /// One walk of the loop forest and one reverse-postorder walk of the function, which is what
    /// section 11.7 says this costs. A function with no entry, which is a declaration, gets an
    /// empty answer rather than an error, because a pipeline is handed declarations.
    #[must_use]
    pub fn of(func: &Func, cfg: &Cfg, loops: &Loops, callees: &Callees) -> Self {
        let told = Predictions::of(func, cfg, loops, callees);
        let width = cfg.capacity();
        let cyclic = cyclic_probabilities(cfg, loops, &told);
        let mut of = vec![Frequency::NEVER; width];
        let mut reliable = vec![true; width];
        let mut capped = vec![false; width];

        let Some(entry) = cfg.entry() else {
            return Self { told, of, reliable, capped, cyclic, entry: Frequency::UNKNOWN };
        };
        of[entry.index()] = Frequency::ENTRY;

        for block in cfg.reverse_postorder() {
            if block != entry {
                let mut total = Frequency::NEVER;
                let mut sound = !loops.is_irreducible(block);
                for &pred in cfg.predecessors(block) {
                    if !forward(cfg, pred, block) {
                        continue;
                    }
                    total = total.plus(of[pred.index()].along(edge(&told, cfg, pred, block)));
                    sound = sound && reliable[pred.index()];
                }
                of[block.index()] = total;
                reliable[block.index()] = sound;
            }
            let Some(id) = heads(loops, block) else { continue };
            let again = cyclic[id.index()];
            of[block.index()] = of[block.index()].repeated_while(again, MAX_PREDICTED_ITERATIONS);
            capped[block.index()] = is_capped(again);
        }

        let entry = of[entry.index()];
        Self { told, of, reliable, capped, cyclic, entry }
    }

    /// The predictions the frequencies were worked out from.
    #[must_use]
    pub fn told(&self) -> &Predictions {
        &self.told
    }

    /// How likely this edge out of this block is to be the one taken.
    ///
    /// The index is into [`Cfg::successors`], which is the order [`Predictions::edges`] is in.
    #[must_use]
    pub fn taken(&self, block: Block, index: usize) -> Probability {
        self.told.taken(block, index)
    }

    /// How often this block runs, with the entry at one.
    #[must_use]
    pub fn get(&self, block: Block) -> Frequency {
        self.of.get(block.index()).copied().unwrap_or(Frequency::UNKNOWN)
    }

    /// The entry's own frequency, which is what every other one is relative to.
    #[must_use]
    pub fn entry(&self) -> Frequency {
        self.entry
    }

    /// Whether this block's frequency means anything.
    ///
    /// False inside an irreducible region and anywhere downstream of one. A consumer that cares
    /// about being right rather than fast should decline these rather than treat them as cold,
    /// which is what they will look like.
    #[must_use]
    pub fn is_reliable(&self, block: Block) -> bool {
        self.reliable.get(block.index()).copied().unwrap_or(false)
    }

    /// Whether this block is a loop header whose iteration count hit the cap.
    ///
    /// Which is a loop nothing predicted an exit for, so the answer is
    /// [`MAX_PREDICTED_ITERATIONS`] rather than a number anybody worked out.
    #[must_use]
    pub fn is_capped(&self, block: Block) -> bool {
        self.capped.get(block.index()).copied().unwrap_or(false)
    }

    /// Whether this block is hot compared with the rest of its function, per section 11.4.
    #[must_use]
    pub fn is_hot(&self, block: Block) -> bool {
        self.get(block).is_hot_in_function(self.entry)
    }

    /// How likely this loop is to go round again.
    #[must_use]
    pub fn cyclic(&self, id: LoopId) -> Probability {
        self.cyclic.get(id.index()).copied().unwrap_or_else(Probability::never)
    }

    /// How many times this loop is estimated to run, which is one over the chance of leaving.
    ///
    /// Capped at [`MAX_PREDICTED_ITERATIONS`], and that cap is what a loop nothing predicted an
    /// exit for gets. This is the estimate unrolling and loop alignment want, and it is a guess
    /// unless it says otherwise.
    #[must_use]
    pub fn iterations(&self, id: LoopId) -> u32 {
        let once = Frequency::ENTRY.repeated_while(self.cyclic(id), MAX_PREDICTED_ITERATIONS);
        let count = once.raw() / u64::from(Probability::SCALE);
        u32::try_from(count).unwrap_or(MAX_PREDICTED_ITERATIONS)
    }

    /// The block that runs most often, and `None` for a function with no blocks.
    #[must_use]
    pub fn hottest(&self, func: &Func) -> Option<Block> {
        func.blocks().max_by_key(|&block| self.get(block).raw())
    }

    /// What section 11.5 asks the verifier to check after every pass.
    ///
    /// Two things. The probabilities out of a block sum to one, and the frequencies arriving at a
    /// block sum to the block's own frequency. The second is exact in real arithmetic even at a
    /// loop header, where the entry and the back edge add up to the header precisely because the
    /// series says they do, so a tolerance of [`PROFILE_SUM_TOLERANCE_PERCENT`] is there for the
    /// remainder every fixed point division throws away and for nothing else.
    ///
    /// Three kinds of block are not checked, and each of them is a place where the sum is known
    /// not to hold: the entry, which nothing arrives at; a header whose count was capped, where
    /// the cap is deliberately not the sum; and anything in or downstream of an irreducible
    /// region, where the frequency was never claimed to mean anything.
    ///
    /// What this catches is the pass that split a block and forgot to split its count, which
    /// section 11.6 says is the failure that costs the most and shows up the least. What it does
    /// not catch is a proportionally wrong but consistent assignment, and nothing does except
    /// review.
    #[must_use]
    pub fn problems(&self, func: &Func, cfg: &Cfg) -> Vec<String> {
        let mut problems = Vec::new();
        let entry = cfg.entry();
        for block in func.blocks() {
            let out: u32 = self.told.edges(block).iter().map(|edge| edge.parts()).sum();
            if !self.told.edges(block).is_empty() && out != Probability::SCALE {
                problems.push(format!(
                    "the edges out of {block:?} are taken {out} parts in {} of the time",
                    Probability::SCALE
                ));
            }
            if Some(block) == entry || self.is_capped(block) || !self.is_reliable(block) {
                continue;
            }
            let mut arriving = Frequency::NEVER;
            let mut edges = 0;
            for &pred in cfg.predecessors(block) {
                arriving = arriving.plus(self.get(pred).along(edge(&self.told, cfg, pred, block)));
                edges += 1;
            }
            let here = self.get(block);
            let apart = here.raw().abs_diff(arriving.raw());
            // A percent of the block's own frequency, and on top of that one part for each edge,
            // because each edge divides by the scale once and loses the remainder.
            let allowed = here.raw() / 100 * u64::from(PROFILE_SUM_TOLERANCE_PERCENT) + edges;
            if apart > allowed {
                problems.push(format!(
                    "{block:?} runs at {here} and the paths into it add up to {arriving}"
                ));
            }
        }
        problems
    }
}

/// How likely each loop is to go round again.
///
/// Innermost first, because an outer loop's cyclic probability is worked out from frequencies that
/// already have the inner loops' iteration counts in them. [`Loops::all`] is outer before inner, so
/// this walks it backwards.
fn cyclic_probabilities(cfg: &Cfg, loops: &Loops, told: &Predictions) -> Vec<Probability> {
    let mut cyclic = vec![Probability::never(); loops.count()];
    let mut relative = vec![Frequency::NEVER; cfg.capacity()];
    let order: Vec<LoopId> = loops.all().collect();

    for &id in order.iter().rev() {
        let header = loops.header(id);
        let mut inside: Vec<Block> = loops.blocks(id).to_vec();
        inside.sort_by_key(|&block| cfg.rank(block));
        for &block in &inside {
            relative[block.index()] = Frequency::NEVER;
        }
        relative[header.index()] = Frequency::ENTRY;

        for &block in &inside {
            if block != header {
                let mut total = Frequency::NEVER;
                for &pred in cfg.predecessors(block) {
                    // Everything outside the loop is left out, which for anything but the header
                    // is nothing: a natural loop is entered at its header and nowhere else.
                    if !loops.contains(id, pred) || !forward(cfg, pred, block) {
                        continue;
                    }
                    total = total.plus(relative[pred.index()].along(edge(told, cfg, pred, block)));
                }
                relative[block.index()] = total;
            }
            let Some(inner) = heads(loops, block) else { continue };
            if inner == id {
                continue;
            }
            let again = cyclic[inner.index()];
            relative[block.index()] =
                relative[block.index()].repeated_while(again, MAX_PREDICTED_ITERATIONS);
        }

        let mut round = Frequency::NEVER;
        for &latch in loops.latches(id) {
            round = round.plus(relative[latch.index()].along(edge(told, cfg, latch, header)));
        }
        let parts = u32::try_from(round.raw()).unwrap_or(Probability::SCALE);
        cyclic[id.index()] = Probability::new(parts, round.quality().min(Quality::Guessed));
    }
    cyclic
}

/// The loop this block is the header of, if it heads one.
fn heads(loops: &Loops, block: Block) -> Option<LoopId> {
    let id = loops.innermost(block)?;
    (loops.header(id) == block).then_some(id)
}

/// Whether this edge goes forwards, which is the edges the acyclic pass may read.
///
/// Reverse postorder rank rather than dominance, because the two agree on every back edge of a
/// natural loop and the rank still answers inside an irreducible region, where there is no header
/// to dominate anything. An edge from a block the entry never reaches has no rank and is not one.
fn forward(cfg: &Cfg, from: Block, to: Block) -> bool {
    match (cfg.rank(from), cfg.rank(to)) {
        (Some(from), Some(to)) => from < to,
        _ => false,
    }
}

/// How likely this edge is to be the one taken.
fn edge(told: &Predictions, cfg: &Cfg, from: Block, to: Block) -> Probability {
    match cfg.successors(from).iter().position(|&block| block == to) {
        Some(at) => told.taken(from, at),
        None => Probability::never(),
    }
}

/// Whether a loop this likely to go round again is one the cap decided for.
fn is_capped(again: Probability) -> bool {
    let stop = Probability::SCALE - again.parts().min(Probability::SCALE);
    stop <= Probability::SCALE.div_ceil(MAX_PREDICTED_ITERATIONS)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Func, Signature, Type};

    use super::Frequencies;
    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::loops::Loops;
    use crate::predict::Callees;
    use crate::profile::{Frequency, Probability, Quality};

    /// One entry's worth, which is what every frequency here is a multiple of.
    const ONE: u64 = Probability::SCALE as u64;

    /// Everything a frequency is worked out from, and then the frequencies.
    fn frequencies(func: &Func) -> (Frequencies, Cfg, Loops) {
        let cfg = Cfg::new(func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        let of = Frequencies::of(func, &cfg, &loops, &Callees::nothing());
        assert!(of.problems(func, &cfg).is_empty(), "{:?}", of.problems(func, &cfg));
        (of, cfg, loops)
    }

    /// A function with `n` blocks.
    fn blank(blocks: usize) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let list = (0..blocks).map(|_| func.create_block()).collect();
        (names, func, list)
    }

    /// Returns zero, which is how most of these functions end.
    fn ret(func: &mut Func, block: Block) {
        let mut build = Builder::new(func, block);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);
    }

    /// Blocks 0 and 1 in a line, then a return.
    fn line() -> (Func, Vec<Block>) {
        let (_, mut func, at) = blank(3);
        Builder::new(&mut func, at[0]).jump(at[1], &[]);
        Builder::new(&mut func, at[1]).jump(at[2], &[]);
        ret(&mut func, at[2]);
        (func, at)
    }

    /// A branch nothing predicts, with both arms joining again.
    fn fork() -> (Func, Vec<Block>) {
        let (_, mut func, at) = blank(4);
        let mut build = Builder::new(&mut func, at[0]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[1], &[], at[2], &[]);
        Builder::new(&mut func, at[1]).jump(at[3], &[]);
        Builder::new(&mut func, at[2]).jump(at[3], &[]);
        ret(&mut func, at[3]);
        (func, at)
    }

    /// A loop: 0 enters, 1 heads it and tests, 2 is the body and the latch, 3 is after it.
    fn loop_shape() -> (Func, Vec<Block>) {
        let (_, mut func, at) = blank(4);
        Builder::new(&mut func, at[0]).jump(at[1], &[]);
        let mut build = Builder::new(&mut func, at[1]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[2], &[], at[3], &[]);
        Builder::new(&mut func, at[2]).jump(at[1], &[]);
        ret(&mut func, at[3]);
        (func, at)
    }

    /// A loop inside a loop: 1 heads the outer, 2 heads the inner, 3 is the inner body, 4 is the
    /// outer latch, 5 is after both.
    fn nest() -> (Func, Vec<Block>) {
        let (_, mut func, at) = blank(6);
        Builder::new(&mut func, at[0]).jump(at[1], &[]);
        for (test, stay, leave) in [(at[1], at[2], at[5]), (at[2], at[3], at[4])] {
            let mut build = Builder::new(&mut func, test);
            let cond = build.iconst(Type::int(1), 1);
            build.br_if(cond, stay, &[], leave, &[]);
        }
        Builder::new(&mut func, at[3]).jump(at[2], &[]);
        Builder::new(&mut func, at[4]).jump(at[1], &[]);
        ret(&mut func, at[5]);
        (func, at)
    }

    #[test]
    fn a_straight_line_runs_once_and_that_is_not_a_guess() {
        let (func, at) = line();
        let (of, ..) = frequencies(&func);
        for block in at {
            assert_eq!(of.get(block).raw(), ONE, "{block:?}");
            assert_eq!(of.get(block).quality(), Quality::Precise);
        }
    }

    #[test]
    fn the_arms_of_a_branch_nobody_predicted_run_half_the_time_each() {
        let (func, at) = fork();
        let (of, ..) = frequencies(&func);
        assert_eq!(of.get(at[1]).raw(), ONE / 2);
        assert_eq!(of.get(at[2]).raw(), ONE / 2);
        // And the join runs as often as the branch, because the arms put it back together.
        assert_eq!(of.get(at[3]).raw(), ONE);
        // Half of a guess is a guess, and so is the sum of two of them.
        assert_eq!(of.get(at[3]).quality(), Quality::Guessed);
    }

    #[test]
    fn a_loop_body_runs_as_many_times_as_the_series_says() {
        let (func, at) = loop_shape();
        let (of, _, loops) = frequencies(&func);
        let id = loops.all().next().expect("a loop");
        // The back edge is taken 89 times in 100, so the header runs 1 / 0.11 times per entry.
        assert_eq!(of.cyclic(id), Probability::percent(89, Quality::Guessed));
        // Which for a loop with one latch and nothing else in it is the header's own edge.
        assert_eq!(of.taken(at[1], 0), of.cyclic(id));
        assert_eq!(of.get(at[1]).raw(), ONE * ONE / 1_100);
        assert_eq!(of.iterations(id), 9);
        // The body is the header times the chance of staying in.
        assert_eq!(of.get(at[2]).raw(), of.get(at[1]).along(of.cyclic(id)).raw());
        // What comes out of a loop that was entered once is a run through it, near enough.
        assert!(of.get(at[3]).raw().abs_diff(ONE) < ONE / 100, "{}", of.get(at[3]));
    }

    #[test]
    fn a_loop_inside_a_loop_multiplies() {
        let (func, at) = nest();
        let (of, _, loops) = frequencies(&func);
        let mut all = loops.all();
        let outer = all.next().expect("the outer loop");
        let inner = all.next().expect("the inner loop");
        assert_eq!(loops.header(outer), at[1]);
        assert_eq!(loops.header(inner), at[2]);
        // Nine times round the outer loop and nine round the inner one for each of those.
        assert_eq!(of.iterations(outer), 9);
        assert_eq!(of.iterations(inner), 9);
        let round = u64::from(of.iterations(outer) * of.iterations(inner));
        assert!(of.get(at[3]).raw() > round * ONE * 3 / 4, "{}", of.get(at[3]));
        // The block after both runs once, however deep the nest got.
        assert!(of.get(at[5]).raw().abs_diff(ONE) < ONE / 100, "{}", of.get(at[5]));
    }

    #[test]
    fn a_loop_nothing_predicts_an_exit_for_gets_the_cap_rather_than_a_division_by_zero() {
        let (_, mut func, at) = blank(2);
        Builder::new(&mut func, at[0]).jump(at[1], &[]);
        Builder::new(&mut func, at[1]).jump(at[1], &[]);
        let (of, _, loops) = frequencies(&func);
        let id = loops.all().next().expect("a loop");
        assert_eq!(of.cyclic(id).parts(), Probability::SCALE);
        assert!(of.is_capped(at[1]));
        assert_eq!(of.iterations(id), 100);
        assert_eq!(of.get(at[1]).raw(), ONE * 100);
    }

    #[test]
    fn a_frequency_in_an_irreducible_region_says_it_does_not_mean_anything() {
        let (_, mut func, at) = blank(3);
        let mut build = Builder::new(&mut func, at[0]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[1], &[], at[2], &[]);
        Builder::new(&mut func, at[1]).jump(at[2], &[]);
        Builder::new(&mut func, at[2]).jump(at[1], &[]);
        let (of, ..) = frequencies(&func);
        assert!(of.is_reliable(at[0]));
        assert!(!of.is_reliable(at[1]), "a two entry cycle has no header and no series");
        assert!(!of.is_reliable(at[2]));
    }

    #[test]
    fn a_block_nothing_reaches_never_runs_and_is_not_hot() {
        let (_, mut func, at) = blank(3);
        Builder::new(&mut func, at[0]).jump(at[1], &[]);
        ret(&mut func, at[1]);
        ret(&mut func, at[2]);
        let (of, ..) = frequencies(&func);
        assert_eq!(of.get(at[2]), Frequency::NEVER);
        assert!(!of.is_hot(at[2]));
        assert!(of.is_hot(at[0]));
    }

    #[test]
    fn the_hottest_block_of_a_loop_is_the_one_in_it() {
        let (func, at) = loop_shape();
        let (of, ..) = frequencies(&func);
        assert_eq!(of.hottest(&func), Some(at[1]));
        assert!(of.is_hot(at[2]));
        assert_eq!(of.entry(), Frequency::ENTRY);
    }

    #[test]
    fn what_arrives_at_a_block_adds_up_to_the_block_which_is_the_check_section_11_5_asks_for() {
        // `frequencies` runs the check on every shape here, so this one is about it failing.
        for (func, _) in [line(), fork(), loop_shape(), nest()] {
            let cfg = Cfg::new(&func);
            let doms = Dominators::new(&cfg);
            let loops = Loops::new(&cfg, &doms);
            let mut of = Frequencies::of(&func, &cfg, &loops, &Callees::nothing());
            assert!(of.problems(&func, &cfg).is_empty());
            // A pass that split a block and did not split its count, which is section 11.6's
            // first failure and the one this check is here for.
            let last = func.blocks().last().expect("a block");
            of.of[last.index()] = Frequency::times(7, Quality::Precise);
            let complaints = of.problems(&func, &cfg);
            assert_eq!(complaints.len(), 1, "{complaints:?}");
            assert!(complaints[0].contains("add up to"), "{}", complaints[0]);
        }
    }
}
