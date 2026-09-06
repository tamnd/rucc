//! The analysis cache, and what a pass has to say about what it left standing.
//!
//! Design: section 4.3 of `spec/optimizer/04-pass-manager.md`, which calls this the analysis
//! manager and gives it four jobs and no more than four. Compute an analysis when somebody asks
//! and keep the answer. Throw an answer away when a pass says it broke the thing the answer was
//! about. Throw away everything built on top of that answer at the same time. Catch a pass that
//! says it preserved something it did not.
//!
//! The type is called [`Analyses`] rather than `Manager` because this crate already has a pass
//! manager in [`crate::pipeline`], and a bare `Manager` re-exported at the top of the crate would
//! not say which of the two it was.
//!
//! # What is cached and what is not
//!
//! The nine here are the nine that own their data: [`Cfg`], [`Dominators`], [`PostDominators`],
//! [`Loops`], [`Frontiers`], [`ControlDependence`], [`Frequencies`], [`Liveness`] and
//! [`Pressure`]. Each is built from the function once and then answers questions without looking
//! at it again, so each is a thing a cache can hold.
//!
//! The rest of the analyses in this crate are not here and do not belong here. [`crate::Alias`],
//! [`crate::memssa`], [`crate::Scev`] and [`crate::range::query::Ranges`] all borrow the function
//! they answer about, which means holding one across an edit is not something the cache would have
//! to be careful about, it is something the compiler refuses. They are query engines built on top
//! of the ones here, and the ones here are what they cost.
//!
//! # Why the cache is keyed by function elsewhere
//!
//! There is one of these per function, and [`crate::pipeline`] keeps a map from function to cache
//! because it runs a pass over the whole module before the next pass starts. Under that order a
//! cache that lived only as long as one function would be thrown away between every pass and every
//! analysis would be recomputed for every pass that wanted it. Section 4.2 of the design says to
//! turn the loop inside out in M4 and run every pass over one function before moving to the next,
//! and the day that lands the map goes away and one of these lives on the stack of the loop.

use rucc_ir::Func;

use crate::predict::Callees;
use crate::{
    Cfg, ControlDependence, Dominators, Frequencies, Frontiers, Liveness, Loops, PostDominators,
    Pressure,
};

/// One analysis this cache holds.
///
/// The order matters and is checked by a test: an analysis is built out of analyses that come
/// before it in this list and never out of one that comes after. That is what lets the
/// invalidation walk settle in one pass over the list rather than in a loop to a fixed point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Analysis {
    /// [`Cfg`], which everything else here is built on.
    Cfg,
    /// [`Dominators`].
    Dominators,
    /// [`PostDominators`].
    PostDominators,
    /// [`Loops`].
    Loops,
    /// [`Frontiers`].
    Frontiers,
    /// [`ControlDependence`].
    ControlDependence,
    /// [`Frequencies`], which carries the branch predictions it was worked out from.
    Frequencies,
    /// [`Liveness`].
    Liveness,
    /// [`Pressure`], which is the live counts split by register class.
    Pressure,
}

impl Analysis {
    /// Every analysis this cache holds, in dependency order.
    pub const EVERY: &'static [Analysis] = &[
        Analysis::Cfg,
        Analysis::Dominators,
        Analysis::PostDominators,
        Analysis::Loops,
        Analysis::Frontiers,
        Analysis::ControlDependence,
        Analysis::Frequencies,
        Analysis::Liveness,
        Analysis::Pressure,
    ];

    /// What it is called in a message to somebody debugging a pass.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cfg => "the control flow graph",
            Self::Dominators => "the dominator tree",
            Self::PostDominators => "the post-dominator tree",
            Self::Loops => "the loop forest",
            Self::Frontiers => "the dominance frontiers",
            Self::ControlDependence => "the control dependence relation",
            Self::Frequencies => "the block frequencies",
            Self::Liveness => "the liveness",
            Self::Pressure => "the register pressure",
        }
    }

    /// The analyses this one is built out of, which cannot outlive it.
    ///
    /// Section 4.4 of the design has a table of these and the entry for almost every row is
    /// "any CFG change", which is why [`Analysis::Cfg`] is what the other three name.
    #[must_use]
    pub const fn needs(self) -> &'static [Analysis] {
        match self {
            Self::Cfg => &[],
            Self::Dominators | Self::PostDominators => &[Analysis::Cfg],
            Self::Loops | Self::Frontiers => &[Analysis::Cfg, Analysis::Dominators],
            Self::ControlDependence => &[Analysis::Cfg, Analysis::PostDominators],
            Self::Frequencies => &[Analysis::Cfg, Analysis::Dominators, Analysis::Loops],
            Self::Liveness => &[Analysis::Cfg],
            Self::Pressure => &[Analysis::Cfg, Analysis::Liveness],
        }
    }

    /// Which bit of a [`Preserved`] set this one is.
    const fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// What a pass leaves standing.
///
/// A set rather than the three cases the design writes, because [`Preserved::ALL`] and
/// [`Preserved::NONE`] are the full set and the empty one and a named set is what is between
/// them. A pass that adds an analysis to this list is saying the code it produced answers the
/// same questions the code it was given did, which is a claim about a pass and not about a run,
/// so it is stated once on the pass rather than returned from each call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preserved(u16);

impl Preserved {
    /// Everything, which is what a pass that does not change the shape of a function says.
    pub const ALL: Preserved = Preserved(u16::MAX);

    /// Nothing, which is what a pass that moves an edge says, however small the move was.
    pub const NONE: Preserved = Preserved(0);

    /// This set and that analysis.
    #[must_use]
    pub const fn and(self, analysis: Analysis) -> Self {
        Self(self.0 | analysis.bit())
    }

    /// This set without that analysis.
    #[must_use]
    const fn without(self, analysis: Analysis) -> Self {
        Self(self.0 & !analysis.bit())
    }

    /// Whether the pass said this one survived.
    #[must_use]
    pub const fn keeps(self, analysis: Analysis) -> bool {
        self.0 & analysis.bit() != 0
    }
}

/// The analyses of one function, computed when asked for and kept until something breaks them.
///
/// Empty to start with. Nothing here is computed by existing, which matters because most
/// functions are walked by a pass that wants none of it.
#[derive(Clone, Debug, Default)]
pub struct Analyses {
    cfg: Option<Cfg>,
    doms: Option<Dominators>,
    post: Option<PostDominators>,
    loops: Option<Loops>,
    frontiers: Option<Frontiers>,
    control: Option<ControlDependence>,
    frequencies: Option<Frequencies>,
    live: Option<Liveness>,
    pressure: Option<Pressure>,
}

impl Analyses {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The control flow graph, computed if it is not already here.
    pub fn cfg(&mut self, func: &Func) -> &Cfg {
        self.cfg.get_or_insert_with(|| Cfg::new(func))
    }

    /// The dominator tree, computed if it is not already here.
    ///
    /// The graph comes out of the cache as well, so a caller that wants both pays for it once.
    /// Each of these is written against the field rather than through the method above it,
    /// because two fields of one structure can be borrowed at the same time and two calls that
    /// each take all of `self` cannot.
    pub fn dominators(&mut self, func: &Func) -> &Dominators {
        let cfg: &Cfg = self.cfg.get_or_insert_with(|| Cfg::new(func));
        self.doms.get_or_insert_with(|| Dominators::new(cfg))
    }

    /// The post-dominator tree, computed if it is not already here.
    ///
    /// # Panics
    ///
    /// Panics through [`PostDominators::new`], on a function with a block that control reaches
    /// and that has no path to any exit even after the fake edges have been added.
    pub fn post_dominators(&mut self, func: &Func) -> &PostDominators {
        let cfg: &Cfg = self.cfg.get_or_insert_with(|| Cfg::new(func));
        self.post.get_or_insert_with(|| PostDominators::new(cfg))
    }

    /// The loop forest, computed if it is not already here.
    pub fn loops(&mut self, func: &Func) -> &Loops {
        let cfg: &Cfg = self.cfg.get_or_insert_with(|| Cfg::new(func));
        let doms: &Dominators = self.doms.get_or_insert_with(|| Dominators::new(cfg));
        self.loops.get_or_insert_with(|| Loops::new(cfg, doms))
    }

    /// The dominance frontier of every block, computed if it is not already here.
    pub fn frontiers(&mut self, func: &Func) -> &Frontiers {
        let cfg: &Cfg = self.cfg.get_or_insert_with(|| Cfg::new(func));
        let doms: &Dominators = self.doms.get_or_insert_with(|| Dominators::new(cfg));
        self.frontiers.get_or_insert_with(|| Frontiers::new(cfg, doms))
    }

    /// Which branches decide whether each block runs, computed if it is not already here.
    ///
    /// # Panics
    ///
    /// Panics through [`PostDominators::new`], for the reason above it.
    pub fn control_dependence(&mut self, func: &Func) -> &ControlDependence {
        let cfg: &Cfg = self.cfg.get_or_insert_with(|| Cfg::new(func));
        let post: &PostDominators = self.post.get_or_insert_with(|| PostDominators::new(cfg));
        self.control.get_or_insert_with(|| ControlDependence::new(cfg, post))
    }

    /// How often each block runs and which way each branch goes, computed if it is not here.
    ///
    /// Predicted rather than measured, and every number out of it says so. A function pass is
    /// given one function and not the module around it, so nothing is known here about what any
    /// callee does. Section 11.2's two predictors that would like to know, which are the ones
    /// about a call that never returns and a call to something cold, still fire on what the IR
    /// says: the front end puts an unreachable after a call that does not come back. A module
    /// pass that wants the rest of the answer builds its own with [`Callees::of_module`].
    pub fn frequencies(&mut self, func: &Func) -> &Frequencies {
        let cfg: &Cfg = self.cfg.get_or_insert_with(|| Cfg::new(func));
        let doms: &Dominators = self.doms.get_or_insert_with(|| Dominators::new(cfg));
        let loops: &Loops = self.loops.get_or_insert_with(|| Loops::new(cfg, doms));
        self.frequencies
            .get_or_insert_with(|| Frequencies::of(func, cfg, loops, &Callees::nothing()))
    }

    /// What is live at the edges of every block, computed if it is not here.
    pub fn live(&mut self, func: &Func) -> &Liveness {
        let cfg: &Cfg = self.cfg.get_or_insert_with(|| Cfg::new(func));
        self.live.get_or_insert_with(|| Liveness::of(func, cfg))
    }

    /// How many registers of each class the function needs where, computed if it is not here.
    ///
    /// Section 40.6's one function with four consumers. It is in the cache rather than at each of
    /// them because four passes computing their own liveness is four chances for the numbers to
    /// disagree, and two passes making opposite decisions off different counts of the same thing
    /// is the failure that is hardest to see afterwards.
    pub fn pressure(&mut self, func: &Func) -> &Pressure {
        let cfg: &Cfg = self.cfg.get_or_insert_with(|| Cfg::new(func));
        let live: &Liveness = self.live.get_or_insert_with(|| Liveness::of(func, cfg));
        self.pressure.get_or_insert_with(|| Pressure::of(func, cfg, live))
    }

    /// Whether this one is here without computing it.
    ///
    /// For the debug check below and for tests. A pass has no business asking, because a pass
    /// that behaves differently depending on what somebody else happened to leave in the cache
    /// is a pass whose output depends on the pipeline around it.
    #[must_use]
    pub fn holds(&self, analysis: Analysis) -> bool {
        match analysis {
            Analysis::Cfg => self.cfg.is_some(),
            Analysis::Dominators => self.doms.is_some(),
            Analysis::PostDominators => self.post.is_some(),
            Analysis::Loops => self.loops.is_some(),
            Analysis::Frontiers => self.frontiers.is_some(),
            Analysis::ControlDependence => self.control.is_some(),
            Analysis::Frequencies => self.frequencies.is_some(),
            Analysis::Liveness => self.live.is_some(),
            Analysis::Pressure => self.pressure.is_some(),
        }
    }

    /// Takes the pass at its word, and in a checked build sees whether it was telling the truth.
    ///
    /// Call it after every pass over the function, with what the pass said it preserved. What
    /// comes back is the analyses the pass claimed to preserve and did not, which is empty when
    /// `check` is off and is empty on an honest pass. Everything the pass did not preserve is
    /// gone from the cache afterwards, and so is everything that was built on top of it.
    ///
    /// The check recomputes, which is why it is behind a flag and why the flag is the one that
    /// already turns the IR verifier on. Both are the same kind of thing: a cost paid in a
    /// build somebody is developing in, to catch the kind of mistake that produces a wrong
    /// program rather than a slow one.
    pub fn settle(&mut self, func: &Func, keeps: Preserved, check: bool) -> Vec<Analysis> {
        let lied = if check { self.lies(func, keeps) } else { Vec::new() };
        // What the pass said, minus what it was just caught being wrong about. A cache that
        // keeps an answer it has proved stale is worse than one that never looked, because the
        // complaint goes into a report somebody reads later and the stale answer goes into the
        // next pass now.
        let mut keeps = keeps;
        for &analysis in &lied {
            keeps = keeps.without(analysis);
        }
        // One pass over the list in dependency order. An analysis survives if the pass said so
        // and everything it is built out of also survived, and because `needs` only ever names
        // an earlier analysis, the answer for what it needs is already final by the time this
        // gets here.
        let mut alive = [false; Analysis::EVERY.len()];
        for &analysis in Analysis::EVERY {
            let kept =
                keeps.keeps(analysis) && analysis.needs().iter().all(|&need| alive[need as usize]);
            alive[analysis as usize] = kept;
            if !kept {
                self.drop(analysis);
            }
        }
        lied
    }

    /// Throws everything away, whatever any pass said.
    ///
    /// For the caller that changed the function itself rather than through a pass, and for a
    /// test that wants a cold cache.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Forgets one analysis and nothing else.
    fn drop(&mut self, analysis: Analysis) {
        match analysis {
            Analysis::Cfg => self.cfg = None,
            Analysis::Dominators => self.doms = None,
            Analysis::PostDominators => self.post = None,
            Analysis::Loops => self.loops = None,
            Analysis::Frontiers => self.frontiers = None,
            Analysis::ControlDependence => self.control = None,
            Analysis::Frequencies => self.frequencies = None,
            Analysis::Liveness => self.live = None,
            Analysis::Pressure => self.pressure = None,
        }
    }

    /// The analyses that are here, were claimed to be preserved, and do not match what the
    /// function says now.
    ///
    /// Only the ones that are here, because an analysis nobody asked for is one nobody can have
    /// been misled by, and recomputing it to check a claim about it would be the cache doing
    /// work the compilation never wanted.
    fn lies(&self, func: &Func, keeps: Preserved) -> Vec<Analysis> {
        let wanted: Vec<Analysis> = Analysis::EVERY
            .iter()
            .copied()
            .filter(|&it| self.holds(it) && keeps.keeps(it))
            .collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        // From the function rather than from anything cached, since what is cached is exactly
        // what is under suspicion.
        let cfg = Cfg::new(func);
        let mut lied = Vec::new();
        for analysis in wanted {
            let same = match analysis {
                Analysis::Cfg => self.cfg.as_ref() == Some(&cfg),
                Analysis::Dominators => self.doms.as_ref() == Some(&Dominators::new(&cfg)),
                Analysis::PostDominators => self.post.as_ref() == Some(&PostDominators::new(&cfg)),
                Analysis::Loops => {
                    self.loops.as_ref() == Some(&Loops::new(&cfg, &Dominators::new(&cfg)))
                }
                Analysis::Frontiers => {
                    self.frontiers.as_ref() == Some(&Frontiers::new(&cfg, &Dominators::new(&cfg)))
                }
                Analysis::ControlDependence => {
                    self.control.as_ref()
                        == Some(&ControlDependence::new(&cfg, &PostDominators::new(&cfg)))
                }
                Analysis::Frequencies => {
                    let doms = Dominators::new(&cfg);
                    let loops = Loops::new(&cfg, &doms);
                    let now = Frequencies::of(func, &cfg, &loops, &Callees::nothing());
                    self.frequencies.as_ref() == Some(&now)
                }
                Analysis::Liveness => self.live.as_ref() == Some(&Liveness::of(func, &cfg)),
                Analysis::Pressure => {
                    let live = Liveness::of(func, &cfg);
                    self.pressure.as_ref() == Some(&Pressure::of(func, &cfg, &live))
                }
            };
            if !same {
                lied.push(analysis);
            }
        }
        lied
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Func, Signature};

    use super::{Analyses, Analysis, Preserved};
    use crate::testing::graph;

    /// A diamond with a loop around the join, which is a shape every analysis here has something
    /// to say about.
    fn func() -> Func {
        graph(&[&[1, 2], &[3], &[3], &[4, 1], &[]])
    }

    #[test]
    fn an_analysis_is_built_out_of_ones_that_come_before_it() {
        // The invalidation walk depends on this and would silently keep a stale analysis if it
        // stopped being true, which is the one bug this file exists to stop.
        for &analysis in Analysis::EVERY {
            for &need in analysis.needs() {
                assert!(need < analysis, "{} is built out of a later analysis", analysis.name());
            }
        }
    }

    #[test]
    fn every_analysis_is_in_the_list_once() {
        for &analysis in Analysis::EVERY {
            let found = Analysis::EVERY.iter().filter(|&&it| it == analysis).count();
            assert_eq!(found, 1, "{} appears twice", analysis.name());
        }
        assert_eq!(Analysis::EVERY.len(), 9);
    }

    #[test]
    fn all_keeps_everything_and_none_keeps_nothing() {
        for &analysis in Analysis::EVERY {
            assert!(Preserved::ALL.keeps(analysis));
            assert!(!Preserved::NONE.keeps(analysis));
        }
    }

    #[test]
    fn a_named_set_holds_what_was_named_and_nothing_else() {
        let keeps = Preserved::NONE.and(Analysis::Cfg).and(Analysis::Loops);
        assert!(keeps.keeps(Analysis::Cfg));
        assert!(keeps.keeps(Analysis::Loops));
        assert!(!keeps.keeps(Analysis::Dominators));
        assert!(!keeps.keeps(Analysis::PostDominators));
    }

    #[test]
    fn nothing_is_computed_until_it_is_asked_for() {
        let mut an = Analyses::new();
        for &analysis in Analysis::EVERY {
            assert!(!an.holds(analysis));
        }
        let func = func();
        an.dominators(&func);
        // The graph as well, because the tree is built out of it and building it twice is what
        // the cache is here to stop.
        assert!(an.holds(Analysis::Cfg));
        assert!(an.holds(Analysis::Dominators));
        assert!(!an.holds(Analysis::Loops));
        assert!(!an.holds(Analysis::PostDominators));
    }

    #[test]
    fn asking_twice_gives_the_same_answer_and_the_second_one_is_free() {
        let func = func();
        let mut an = Analyses::new();
        let first = an.cfg(&func).clone();
        let second = an.cfg(&func);
        assert_eq!(&first, second);
    }

    #[test]
    fn the_loop_forest_pulls_in_what_it_is_built_out_of() {
        let func = func();
        let mut an = Analyses::new();
        an.loops(&func);
        assert!(an.holds(Analysis::Cfg));
        assert!(an.holds(Analysis::Dominators));
        assert!(an.holds(Analysis::Loops));
    }

    #[test]
    fn preserving_everything_keeps_everything() {
        let func = func();
        let mut an = Analyses::new();
        an.loops(&func);
        an.frontiers(&func);
        an.control_dependence(&func);
        an.frequencies(&func);
        an.pressure(&func);
        assert!(an.settle(&func, Preserved::ALL, true).is_empty());
        for &analysis in Analysis::EVERY {
            assert!(an.holds(analysis), "{} was thrown away", analysis.name());
        }
    }

    #[test]
    fn the_pressure_falls_with_the_liveness_it_was_counted_from() {
        let func = func();
        let mut an = Analyses::new();
        an.pressure(&func);
        assert!(an.holds(Analysis::Liveness), "it had to be computed to count anything");
        let keeps = Preserved::NONE.and(Analysis::Cfg).and(Analysis::Pressure);
        an.settle(&func, keeps, false);
        assert!(an.holds(Analysis::Cfg));
        assert!(!an.holds(Analysis::Liveness));
        assert!(!an.holds(Analysis::Pressure), "a count outlived what it counted");
    }

    #[test]
    fn preserving_nothing_empties_the_cache() {
        let func = func();
        let mut an = Analyses::new();
        an.loops(&func);
        an.frontiers(&func);
        an.control_dependence(&func);
        an.settle(&func, Preserved::NONE, false);
        for &analysis in Analysis::EVERY {
            assert!(!an.holds(analysis), "{} outlived the pass", analysis.name());
        }
    }

    #[test]
    fn losing_the_graph_loses_what_was_built_on_it() {
        let func = func();
        let mut an = Analyses::new();
        an.loops(&func);
        an.post_dominators(&func);
        // A pass that says it kept the trees and the forest and not the graph they came out of.
        // What it says about them is not wrong so much as meaningless, and taking it at its word
        // is how a stale dominator tree reaches the pass after next.
        let keeps = Preserved::NONE
            .and(Analysis::Dominators)
            .and(Analysis::PostDominators)
            .and(Analysis::Loops);
        an.settle(&func, keeps, false);
        for &analysis in Analysis::EVERY {
            assert!(!an.holds(analysis), "{} outlived the graph", analysis.name());
        }
    }

    #[test]
    fn losing_the_dominator_tree_loses_the_forest_and_leaves_the_graph() {
        let func = func();
        let mut an = Analyses::new();
        an.loops(&func);
        an.post_dominators(&func);
        let keeps =
            Preserved::NONE.and(Analysis::Cfg).and(Analysis::PostDominators).and(Analysis::Loops);
        an.settle(&func, keeps, false);
        assert!(an.holds(Analysis::Cfg));
        assert!(an.holds(Analysis::PostDominators));
        assert!(!an.holds(Analysis::Dominators), "the tree was not preserved");
        assert!(!an.holds(Analysis::Loops), "the forest outlived the tree it needs");
    }

    #[test]
    fn each_frontier_falls_with_the_tree_it_was_walked_on_and_not_the_other_one() {
        // The two frontiers are the same algorithm, but they are not the same analysis. A pass
        // that claims both and only keeps one of the two trees gets to keep one of them, and the
        // other goes with the tree it was walked on whatever the pass said about it.
        let func = func();
        let mut an = Analyses::new();
        an.frontiers(&func);
        an.control_dependence(&func);
        let keeps = Preserved::NONE
            .and(Analysis::Cfg)
            .and(Analysis::Dominators)
            .and(Analysis::Frontiers)
            .and(Analysis::ControlDependence);
        an.settle(&func, keeps, false);
        assert!(an.holds(Analysis::Frontiers), "the frontier stands on a tree that stood");
        assert!(!an.holds(Analysis::ControlDependence), "the post-dominator tree went with it");
    }

    #[test]
    fn the_frequencies_fall_with_the_loop_forest_they_were_worked_out_from() {
        let func = func();
        let mut an = Analyses::new();
        an.frequencies(&func);
        // Asking for them brings in the graph, the tree and the forest, because the series in
        // section 11.3 is per loop and there is no loop without all three.
        for analysis in [Analysis::Cfg, Analysis::Dominators, Analysis::Loops] {
            assert!(an.holds(analysis), "{} was not pulled in", analysis.name());
        }
        let keeps =
            Preserved::NONE.and(Analysis::Cfg).and(Analysis::Dominators).and(Analysis::Frequencies);
        an.settle(&func, keeps, false);
        assert!(!an.holds(Analysis::Loops), "the forest was not preserved");
        assert!(!an.holds(Analysis::Frequencies), "a frequency outlived the loop it counted");
    }

    #[test]
    fn a_pass_that_says_it_kept_the_graph_and_moved_an_edge_is_caught() {
        let mut func = func();
        let mut an = Analyses::new();
        an.loops(&func);
        // The edit a lying pass makes: block4 falls off the end of the diamond, and now it
        // returns to nobody instead. The blocks are the same blocks and the graph is not the
        // same graph.
        let block = Block::from_usize(3);
        let term = func.terminator(block).expect("the helper gives every block a terminator");
        func.remove_inst(term);
        let mut build = rucc_ir::Builder::new(&mut func, block);
        build.ret(&[]);
        let lied = an.settle(&func, Preserved::ALL, true);
        assert_eq!(lied, vec![Analysis::Cfg, Analysis::Dominators, Analysis::Loops]);
        // And it is thrown away anyway, because a cache that keeps what it just proved wrong is
        // worse than one that never checked.
        for &analysis in Analysis::EVERY {
            assert!(!an.holds(analysis));
        }
    }

    #[test]
    fn a_lie_about_the_frontiers_is_caught_the_same_way() {
        let mut func = func();
        let mut an = Analyses::new();
        an.frontiers(&func);
        an.control_dependence(&func);
        // The back edge goes away, so block1 stops being a join and block3 stops being a branch.
        // Both frontiers move, and a pass that swears they did not is wrong about both.
        let block = Block::from_usize(3);
        let term = func.terminator(block).expect("the helper gives every block a terminator");
        func.remove_inst(term);
        let mut build = rucc_ir::Builder::new(&mut func, block);
        build.ret(&[]);
        let lied = an.settle(&func, Preserved::ALL, true);
        assert!(lied.contains(&Analysis::Frontiers));
        assert!(lied.contains(&Analysis::ControlDependence));
    }

    #[test]
    fn the_check_costs_nothing_when_it_is_off() {
        let mut func = func();
        let mut an = Analyses::new();
        an.cfg(&func);
        let block = Block::from_usize(3);
        let term = func.terminator(block).expect("the helper gives every block a terminator");
        func.remove_inst(term);
        let mut build = rucc_ir::Builder::new(&mut func, block);
        build.ret(&[]);
        assert!(an.settle(&func, Preserved::ALL, false).is_empty());
        // Which is the trade the flag is: the lie is not caught, and the stale graph is still
        // there, exactly as the pass claimed.
        assert!(an.holds(Analysis::Cfg));
    }

    #[test]
    fn an_analysis_nobody_asked_for_is_not_checked() {
        let func = func();
        let mut an = Analyses::new();
        assert!(an.settle(&func, Preserved::ALL, true).is_empty());
    }

    #[test]
    fn a_declaration_has_analyses_like_anything_else() {
        // Because the pipeline hands the cache whatever the module holds, and a cache that
        // panicked on a function with no body would put the check in every caller.
        let mut names = Interner::new();
        let func = Func::new(names.intern("declared"), Signature::new());
        let mut an = Analyses::new();
        assert!(an.cfg(&func).entry().is_none());
        an.loops(&func);
        an.post_dominators(&func);
        assert!(an.settle(&func, Preserved::ALL, true).is_empty());
    }

    #[test]
    fn clearing_takes_everything() {
        let func = func();
        let mut an = Analyses::new();
        an.loops(&func);
        an.clear();
        for &analysis in Analysis::EVERY {
            assert!(!an.holds(analysis));
        }
    }
}
