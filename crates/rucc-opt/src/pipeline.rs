//! The pipelines, one per optimization level, and the manager that runs one.
//!
//! Section 9.1 of `spec/09-optimizer.md` says the pipelines are written out rather than assembled
//! from flags, and gives the reason: the prior art ran the same pipeline at every level and named
//! that as a limitation. A level here is a list of pass names, and the list is the definition of
//! the level rather than something that emerges from which flags happen to be set.
//!
//! Section 9.10 says the manager is deliberately boring. There is no adaptive ordering and no
//! scheduling heuristic, because document 03's determinism rule needs the same input to produce
//! the same output on every host and predictability is worth more than the last percent.
//!
//! What the manager does beyond running the list is the four things that make a pass debuggable:
//! it counts each pass's transformations against its fuel, it collects what each pass said it did
//! and did not do, it dumps the IR around whichever passes were asked for, and it verifies any
//! function a pass changed.
//!
//! That last one is section 41.4 of `spec/optimizer/41-correctness.md`, which reads GCC's
//! `execute_function_todo` and takes six things from it. Three of them are already true here by
//! construction and are worth naming so that nobody looks for them. GCC verifies what the IR
//! currently is, by consulting `curr_properties`, because its IR passes through GENERIC, GIMPLE
//! with and without a CFG, GIMPLE in SSA, and RTL. rucc has one IR, it is in SSA from the moment
//! the lowering walk builds it, and it always has a CFG, so the applicable set never varies and a
//! bitmask saying so would have nothing to say. GCC guards the verifiers with `!seen_error()`,
//! because after a user error the IR is legitimately malformed and an internal error raised over
//! it hides the real diagnostic. Here the optimizer is not reached at all after a parse, check or
//! lowering error, which is the same guard placed one level up where it cannot be forgotten. And
//! GCC asserts that a verifier did not change the dominator state. Here a verifier takes the
//! module by shared reference, so that is a type error rather than an assertion.
//!
//! What is left of the six is the part below: verify what changed, not everything, and say which
//! function it was.

use std::collections::HashMap;
use std::fmt::Write as _;

use rucc_base::{Interner, Symbol};
use rucc_ir::{FuncId, Module};
use rucc_session::OptLevel;

use crate::{Analyses, Fuel, Gates, Pass, Preserved, Stats, pass};

/// `-O0`. One pass, and it is not an optimization. Section 9.1 gives this level SSA
/// construction, which the lowering walk in `spec/08-ir.md` already does, and mem2reg for the
/// allocas that are left, which is the next pass to be written.
///
/// `simplify-cfg` is here because a branch on a condition that is a constant is not a missed
/// optimization, it is a call to a function the program never calls, and a program that calls a
/// function it never calls is one that does not link. That is issue 359, gcc removes the code at
/// every level including this one, and a `-O0` that emitted it would be a `-O0` some correct
/// programs cannot be built at. Nothing else runs, and no analysis beyond the graph the pass
/// reads reachability out of is computed.
const O0: &[&str] = &["simplify-cfg"];

/// `-O1`. Section 9.1 asks for one e-graph round, conservative inlining, simplify-CFG, SROA,
/// GVN, DCE, LICM and the loop canonicalizations. Folding, control flow simplification and dead
/// code elimination are the part of that which exists, with the peephole among them. They run in
/// that order because folding and the peephole are what make most of the dead code there is to
/// eliminate, because a constant a fold produced is a branch condition the control flow pass can
/// then read, and because the comparison that branch was on is dead once it has.
const O1: &[&str] = &["fold", "simplify", "narrow", "simplify-cfg", "dce"];

/// `-O2`. The level the code quality claim is about. Section 9.1 asks for two e-graph rounds
/// around the loop pipeline, the full inlining cost model, Memory SSA and the full alias
/// analysis stack, and then the scalar and machine passes on top.
const O2: &[&str] = &["fold", "simplify", "narrow", "simplify-cfg", "dce"];

/// `-O3`. `-O2` plus loop vectorization, larger inlining and unrolling thresholds, interchange
/// and distribution where the dependence analysis is confident, and function specialization.
const O3: &[&str] = &["fold", "simplify", "narrow", "simplify-cfg", "dce"];

/// `-Os`. `-O2`'s passes under a size cost model: inlining only where it shrinks, no unrolling
/// and no vectorization.
const OS: &[&str] = &["fold", "simplify", "narrow", "simplify-cfg", "dce"];

/// `-Oz`. `-Os` and additionally the outliner, with instruction selection preferring the smaller
/// encoding wherever there is a choice.
const OZ: &[&str] = &["fold", "simplify", "narrow", "simplify-cfg", "dce"];

/// The passes this level runs, before the command line adds to or removes from them.
#[must_use]
pub const fn for_level(level: OptLevel) -> &'static [&'static str] {
    match level {
        OptLevel::O0 => O0,
        OptLevel::O1 => O1,
        OptLevel::O2 => O2,
        OptLevel::O3 => O3,
        OptLevel::Os => OS,
        OptLevel::Oz => OZ,
    }
}

/// Which passes the IR is written out around.
///
/// Empty by default, which is the whole point: a dump is a debugging aid and writing files
/// nobody asked for is not one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dumps {
    /// Every pass, on both sides.
    all: bool,
    /// The passes to write out before.
    before: Vec<String>,
    /// The passes to write out after.
    after: Vec<String>,
}

impl Dumps {
    /// Adds one `-fdump-ir=` argument.
    ///
    /// # Errors
    ///
    /// When the argument is not `all`, `before-<pass>` or `after-<pass>`, or when it names a
    /// pass this compiler does not have. A misspelled pass name that quietly dumped nothing
    /// would look exactly like a pass that did not run.
    pub fn add(&mut self, spec: &str) -> Result<(), String> {
        if spec == "all" {
            self.all = true;
            return Ok(());
        }
        let (side, name) = match spec.split_once('-') {
            Some(("before", name)) => (&mut self.before, name),
            Some(("after", name)) => (&mut self.after, name),
            _ => {
                return Err(format!(
                    "`{spec}` is not a dump this compiler makes, which are `all`, \
                     `before-<pass>` and `after-<pass>`"
                ));
            }
        };
        if pass::find(name).is_none() {
            return Err(format!("`{name}` is not a pass this compiler has, see --print-pipeline"));
        }
        side.push(name.to_owned());
        Ok(())
    }

    /// Whether anything is dumped at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.all && self.before.is_empty() && self.after.is_empty()
    }

    /// Whether the IR is written out before this pass runs.
    #[must_use]
    pub fn wants_before(&self, name: &str) -> bool {
        self.all || self.before.iter().any(|it| it == name)
    }

    /// Whether the IR is written out after this pass runs.
    #[must_use]
    pub fn wants_after(&self, name: &str) -> bool {
        self.all || self.after.iter().any(|it| it == name)
    }
}

/// What the command line asked the optimizer for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Which pipeline to start from.
    pub level: OptLevel,
    /// The passes `-f<name>` added and `-fno-<name>` removed, in the order they were given, so
    /// that the last mention of a pass is the one that decides.
    pub toggles: Vec<(String, bool)>,
    /// What `-fpass-fuel=<pass>=<n>` limited, by pass name.
    pub fuel: HashMap<String, u32>,
    /// What `-fpass-fuel-global=<n>` limited the whole pipeline to, across every pass.
    ///
    /// This is the outer search of the two in section 4.5 of
    /// `spec/optimizer/04-pass-manager.md`. Halving this finds the pass, and halving
    /// `-fpass-fuel` for that pass finds the rewrite inside it. Two searches of twenty
    /// compilations each beat one search over a space nobody knows the shape of.
    pub global_fuel: Option<u32>,
    /// What `-fdisable-<pass>` and `-fenable-<pass>` said about which functions a pass runs on.
    pub gates: Gates,
    /// What `-fdump-ir=` asked to see.
    pub dumps: Dumps,
    /// Whether the verifier runs after every pass that changed anything.
    pub verify: bool,
}

impl Default for Options {
    /// The default level with nothing added to it, and the verifier on in a debug build, which
    /// is what section 9.10 asks for.
    fn default() -> Self {
        Self {
            level: OptLevel::default(),
            toggles: Vec::new(),
            fuel: HashMap::new(),
            global_fuel: None,
            gates: Gates::default(),
            dumps: Dumps::default(),
            verify: cfg!(debug_assertions),
        }
    }
}

impl Options {
    /// The options a level asks for on its own.
    #[must_use]
    pub fn for_level(level: OptLevel) -> Self {
        Self { level, ..Self::default() }
    }

    /// The passes the level and the `-f` flags chose, in order, before the gates are consulted.
    ///
    /// A pass named by `-f<name>` that the level did not choose is appended, because the only
    /// place it could go that does not need an ordering rule nobody wrote down is the end.
    #[must_use]
    pub fn chosen(&self) -> Vec<&'static str> {
        let mut names: Vec<&str> = for_level(self.level).to_vec();
        for (name, on) in &self.toggles {
            let name = name.as_str();
            match *on {
                true if !names.contains(&name) => names.push(name),
                true => {}
                false => names.retain(|it| *it != name),
            }
        }
        names.into_iter().filter_map(pass::find).map(Pass::name).collect()
    }

    /// The passes that will run, in order, over at least one function.
    ///
    /// A pass `-fenable-<name>` reached that the level did not choose is appended after them,
    /// for the same reason and in the same place. It runs only over the functions the gate names,
    /// which is the whole point of the flag: a pass being in this list is not the same question as
    /// a pass running on the function somebody is looking at.
    #[must_use]
    pub fn passes(&self) -> Vec<&'static dyn Pass> {
        let mut names = self.chosen();
        for name in self.gates.enabled() {
            // Through the pass list rather than straight from the gate, because the name the
            // pass holds outlives this call and the one the gate holds does not.
            let Some(found) = pass::find(name) else { continue };
            if !names.contains(&found.name()) {
                names.push(found.name());
            }
        }
        names.into_iter().filter_map(pass::find).collect()
    }
}

/// One written out copy of the IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dump {
    /// What to call it, which is a number, a side and a pass name, as in `01-after-fold`. The
    /// number is there so that a directory listing is in the order the passes ran.
    pub name: String,
    /// The module, in the textual form from `spec/08-ir.md`.
    pub text: String,
}

/// What one pass had to say about one function.
///
/// One of these per pass per function with a body, whether or not the pass said anything, because
/// a pass that reports nothing being visible as a pass that reports nothing is the point of the
/// record. Section 42.2 of `spec/optimizer/42-measurement.md` has the argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remark {
    /// Which pass, by the name a `-f` flag spells.
    pub pass: &'static str,
    /// Which function, by the name in the source.
    pub func: Symbol,
    /// What it said.
    pub stats: Stats,
}

/// What running the pipeline produced beyond the changed module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The dumps asked for, in the order they were taken. The manager does not write files,
    /// because nothing below the driver in `spec/18-package-layout.md` knows what a file is.
    pub dumps: Vec<Dump>,
    /// A pass that left the IR in a state the verifier refuses, named, with what it said.
    pub broke: Vec<String>,
    /// How much fuel each pass spent, which is the number a bisection halves.
    pub spent: Vec<(&'static str, u32)>,
    /// What every pass said about every function, in the order the passes ran and then in the
    /// order the module holds its functions. This is what `-fopt-info` prints.
    pub remarks: Vec<Remark>,
}

impl Report {
    /// Everything one pass said across the whole module, added up.
    ///
    /// The counts of an event are addable across functions because an event names a site in a
    /// pass rather than a fact about a program, which is the reason [`crate::stats::Event::what`]
    /// is a fixed string.
    #[must_use]
    pub fn totals(&self, pass: &str) -> Stats {
        let mut total = Stats::new();
        for remark in self.remarks.iter().filter(|it| it.pass == pass) {
            total.merge(&remark.stats);
        }
        total
    }
}

/// Runs the pipeline over the module.
///
/// Every pass sees every function with a body, one at a time, and a pass runs over the whole
/// module before the next one starts. That order is what makes the dumps readable: a dump is
/// the state of the program between two passes rather than between two functions.
pub fn run(module: &mut Module, names: &Interner, opts: &Options) -> Report {
    let mut report = Report::default();
    let chosen = opts.chosen();
    // One cache per function, kept across passes because a pass runs over the whole module
    // before the next one starts. A cache that lived only as long as one function would be
    // thrown away between every pass and would never answer a second question. Section 4.2 of
    // `spec/optimizer/04-pass-manager.md` is the plan for turning the loop inside out, and the
    // day that happens this map becomes a local in the inner loop.
    let mut cached: HashMap<FuncId, Analyses> = HashMap::new();
    // What the whole pipeline has left, which every pass draws its own allowance out of and
    // gives the unspent part of back. A pass past the end of it is given nothing rather than
    // skipped, so it still runs, still reports, and still transforms nothing.
    let mut budget = opts.global_fuel;
    for (index, pass) in opts.passes().into_iter().enumerate() {
        let name = pass.name();
        if opts.dumps.wants_before(name) {
            report.dumps.push(dump(index, "before", name, module, names));
        }
        let mut fuel = match (opts.fuel.get(name).copied(), budget) {
            // Whichever limit is tighter, because two limits that disagree mean the one that
            // stops first, and a bisection that started with the global one has to stay inside
            // it while the per pass one is halved.
            (Some(count), Some(left)) => Fuel::of(count.min(left)),
            (Some(count), None) => Fuel::of(count),
            (None, Some(left)) => Fuel::of(left),
            (None, None) => Fuel::unlimited(),
        };
        // What the level and the `-f` flags decided, which is what a gate overrides for the
        // functions it names and leaves alone for the ones it does not.
        let default = chosen.contains(&name);
        for id in module.funcs() {
            if module[id].is_declaration() {
                continue;
            }
            if !opts.gates.allows(name, default, id.raw(), names.resolve(module[id].name)) {
                // No remark either. A pass that did not run on a function has nothing to say
                // about it, and a record saying it found nothing would read as a pass that
                // looked.
                continue;
            }
            let an = cached.entry(id).or_default();
            let stats = pass.run(&mut module[id], an, &mut fuel);
            // A pass that changed nothing preserved everything, whatever it says about itself,
            // so the cheap case does not need every pass to have a second opinion about it.
            // A pass that did change something is taken at its word, and in a checked build the
            // word is checked.
            let keeps = if stats.changed() { pass.preserves() } else { Preserved::ALL };
            for broken in an.settle(&module[id], keeps, opts.verify) {
                let func = names.resolve(module[id].name);
                report.broke.push(format!(
                    "the {name} pass said it preserved {} of {func} and did not",
                    broken.name()
                ));
            }
            // Here rather than after the pass, and this function rather than the module. A pass
            // is a function pass, so the only thing it can have broken is the function it was
            // given, and walking the other ones again after every one of them is the quadratic
            // walk `rucc_ir::verify_func` exists to avoid. Doing it here is also what lets the
            // message name the function, which the module walk could not, and it puts the
            // failure next to the pass that caused it rather than at the end of the module.
            if stats.changed() && opts.verify {
                if let Err(errors) = rucc_ir::verify_func(module, &module[id], names) {
                    let func = names.resolve(module[id].name);
                    for error in errors {
                        report
                            .broke
                            .push(format!("the {name} pass left invalid IR in {func}, {error}"));
                    }
                }
            }
            // The record is the only place the manager learns that anything happened, which is
            // why the pass cannot leave recording until later. See `crate::stats`.
            report.remarks.push(Remark { pass: name, func: module[id].name, stats });
        }
        report.spent.push((name, fuel.spent()));
        if let Some(left) = &mut budget {
            // Never below zero, because the allowance the pass was given was at most this.
            *left -= fuel.spent();
        }
        if opts.dumps.wants_after(name) {
            report.dumps.push(dump(index, "after", name, module, names));
        }
    }
    report
}

/// The module written out, under a name that sorts in the order the passes ran.
fn dump(index: usize, side: &str, name: &str, module: &Module, names: &Interner) -> Dump {
    Dump { name: format!("{index:02}-{side}-{name}"), text: rucc_ir::print(module, names) }
}

/// Renders what `--print-pipeline` prints.
///
/// One line per pass, numbered from one, with what the pass does after it. A level that runs
/// nothing says so rather than printing an empty list, because an empty answer and a broken
/// command look the same.
#[must_use]
pub fn print(opts: &Options) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "level: {}", opts.level);
    // Only when it was asked for, so the listing of a compilation nobody is bisecting is the
    // same listing it has always been. A run under a budget is a run whose output is not the
    // one the level asked for, and the listing is where that has to be visible.
    if let Some(count) = opts.global_fuel {
        let _ = writeln!(out, "global fuel: {count}");
    }
    let passes = opts.passes();
    if passes.is_empty() {
        let _ = writeln!(out, "no passes");
        return out;
    }
    for (index, pass) in passes.iter().enumerate() {
        let _ = write!(out, "{}: {}, {}", index + 1, pass.name(), pass.describe());
        // Only when a gate mentions the pass, so the listing of a compilation nobody is
        // debugging is the same listing it has always been.
        if let Some(note) = opts.gates.note(pass.name()) {
            let _ = write!(out, " [{note}]");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Func, Module, Opcode, Signature, Type};
    use rucc_session::OptLevel;
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{Dumps, Options, for_level};
    use crate::stats::Kind;
    use crate::{Pass, pass};

    /// A module with one function whose body has something to fold in it.
    fn module() -> (Interner, Module) {
        let mut names = Interner::new();
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        let func = foldable(&mut names, "f");
        module.add_func(func);
        (names, module)
    }

    /// A module with two of them, called `f` and `g`, in that order, so `f` is function 0.
    fn two_functions() -> (Interner, Module) {
        let mut names = Interner::new();
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        for name in ["f", "g"] {
            let func = foldable(&mut names, name);
            module.add_func(func);
        }
        (names, module)
    }

    /// A function that returns a sign extension of a constant, which folding rewrites.
    fn foldable(names: &mut Interner, name: &str) -> Func {
        let mut func =
            Func::new(names.intern(name), Signature::new().with_returns(&[Type::int(64)]));
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let narrow = build.iconst(Type::int(32), 7);
        let wide = build.unary(Opcode::SExt, narrow, Type::int(64));
        build.ret(&[wide]);
        func
    }

    /// Whether the pass said anything about the function, which it only does when it ran on it.
    fn spoke_about(report: &super::Report, pass: &str, func: &str, names: &Interner) -> bool {
        report.remarks.iter().any(|it| it.pass == pass && names.resolve(it.func) == func)
    }

    #[test]
    fn every_pass_a_pipeline_names_is_a_pass_that_exists() {
        for level in
            [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::O3, OptLevel::Os, OptLevel::Oz]
        {
            for name in for_level(level) {
                assert!(
                    pass::find(name).is_some(),
                    "{level} names `{name}` and no pass answers to it"
                );
            }
        }
    }

    #[test]
    fn no_pipeline_names_a_pass_twice() {
        for level in
            [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::O3, OptLevel::Os, OptLevel::Oz]
        {
            let names = for_level(level);
            for (index, name) in names.iter().enumerate() {
                assert!(!names[index + 1..].contains(name), "{level} runs `{name}` twice");
            }
        }
    }

    /// What a pass spent, or `None` if it did not run.
    fn spent(report: &super::Report, pass: &str) -> Option<u32> {
        report.spent.iter().find(|(name, _)| *name == pass).map(|&(_, count)| count)
    }

    /// The names of the passes a set of options would run, in order.
    fn names(opts: &Options) -> Vec<&'static str> {
        opts.passes().into_iter().map(Pass::name).collect()
    }

    #[test]
    fn the_level_that_optimizes_nothing_still_removes_what_nothing_reaches() {
        // One pass at `-O0`, and it is the one that is not an optimization. See the comment on
        // the level itself, and issue 359.
        assert_eq!(names(&Options::for_level(OptLevel::O0)), ["simplify-cfg"]);
        assert!(names(&Options::for_level(OptLevel::O2)).len() > 1);
    }

    #[test]
    fn a_pass_is_removed_by_no_and_added_by_the_bare_name_and_the_last_word_wins() {
        let mut opts = Options::for_level(OptLevel::O2);
        opts.toggles.push(("fold".to_owned(), false));
        assert!(!names(&opts).contains(&"fold"), "{:?}", names(&opts));
        opts.toggles.push(("fold".to_owned(), true));
        assert!(names(&opts).contains(&"fold"), "{:?}", names(&opts));

        let mut off = Options::for_level(OptLevel::O0);
        off.toggles.push(("fold".to_owned(), true));
        assert_eq!(
            names(&off),
            ["simplify-cfg", "fold"],
            "a pass the level did not choose is still reachable"
        );
    }

    #[test]
    fn asking_for_a_pass_twice_does_not_run_it_twice() {
        let mut opts = Options::for_level(OptLevel::O2);
        let before = names(&opts);
        opts.toggles.push(("fold".to_owned(), true));
        assert_eq!(names(&opts), before);
    }

    #[test]
    fn the_pipeline_listing_names_the_level_and_every_pass_in_order() {
        let text = super::print(&Options::for_level(OptLevel::O2));
        assert!(text.starts_with("level: -O2\n"), "{text}");
        assert!(text.contains("1: fold, "), "{text}");
        let mut none = Options::for_level(OptLevel::O0);
        none.toggles.push(("simplify-cfg".to_owned(), false));
        let none = super::print(&none);
        assert!(none.contains("no passes"), "{none}");
    }

    #[test]
    fn running_the_pipeline_changes_the_module_and_reports_what_it_spent() {
        let (names, mut module) = module();
        let report = super::run(&mut module, &names, &Options::for_level(OptLevel::O2));
        // Folding rewrites the sign extension into a constant, and then the constant it was
        // extending is read by nothing and dead code elimination takes it out. One
        // transformation each, which is what the two of them together are for. Asserted by
        // name rather than as the whole vector, so a pass added later does not fail this.
        assert_eq!(spent(&report, "fold"), Some(1));
        assert_eq!(spent(&report, "dce"), Some(1));
        assert!(report.broke.is_empty(), "{:?}", report.broke);
        assert!(report.dumps.is_empty(), "nothing asked for a dump");
        assert!(rucc_ir::print(&module, &names).contains("iconst.i64 7"));
    }

    #[test]
    fn the_analyses_survive_a_pass_that_keeps_them_and_not_one_that_does_not() {
        // The pipeline half of the analysis manager. A branch on a constant, so `simplify-cfg`
        // has something to do and says it preserved nothing, and the whole run comes out with
        // the verifier and the manager both satisfied. What a pass that lied would produce is in
        // `crate::analysis`, where a lie can be told on purpose.
        let mut names = Interner::new();
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let dead = func.create_block();
        let exit = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let never = build.iconst(Type::int(1), 0);
        build.br_if(never, dead, &[], exit, &[]);
        for block in [dead, exit] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        module.add_func(func);
        let report = super::run(&mut module, &names, &Options::for_level(OptLevel::O2));
        assert_eq!(spent(&report, "simplify-cfg"), Some(1));
        assert!(report.broke.is_empty(), "{:?}", report.broke);
        let text = rucc_ir::print(&module, &names);
        // The labels, which start a line, and not the mentions of one, which are indented.
        assert_eq!(
            text.matches("\nblock").count(),
            2,
            "the block nothing reaches is still here:\n{text}"
        );
    }

    #[test]
    fn no_pass_that_optimizes_runs_at_no_optimization_however_much_there_is_to_do() {
        let (names, mut module) = module();
        let before = rucc_ir::print(&module, &names);
        let report = super::run(&mut module, &names, &Options::for_level(OptLevel::O0));
        // The one pass the level runs looked, found no branch it could read and no block nothing
        // reaches, and spent nothing. The constant arithmetic the fixture is full of is still
        // there, which is the part of `-O0` that has not changed.
        assert_eq!(report.spent, vec![("simplify-cfg", 0)]);
        assert_eq!(rucc_ir::print(&module, &names), before);
    }

    #[test]
    fn a_gate_takes_a_pass_away_from_one_function_and_leaves_the_other_alone() {
        let (names, mut module) = two_functions();
        let mut opts = Options::for_level(OptLevel::O2);
        opts.gates.add(false, "fold=g").expect("g is a function and fold is a pass");
        let report = super::run(&mut module, &names, &opts);
        assert!(spoke_about(&report, "fold", "f", &names));
        assert!(!spoke_about(&report, "fold", "g", &names), "fold ran where it was gated off");
        assert!(spoke_about(&report, "dce", "g", &names), "one pass gated off is not all of them");
        // What the gate is for: the two functions came out different, and the difference is one
        // pass on one function rather than a level on a file.
        let text = rucc_ir::print(&module, &names);
        assert_eq!(text.matches("sext.i64").count(), 1, "{text}");
    }

    #[test]
    fn a_function_can_be_gated_by_the_number_it_has_in_the_module() {
        let (names, mut module) = two_functions();
        let mut opts = Options::for_level(OptLevel::O2);
        opts.gates.add(false, "fold=0").expect("0 is a function and fold is a pass");
        let report = super::run(&mut module, &names, &opts);
        assert!(!spoke_about(&report, "fold", "f", &names), "function 0 is the first one");
        assert!(spoke_about(&report, "fold", "g", &names));
    }

    #[test]
    fn enabling_a_pass_reaches_one_function_at_a_level_that_did_not_ask_for_it() {
        let (names, mut module) = two_functions();
        let mut opts = Options::for_level(OptLevel::O0);
        opts.gates.add(true, "fold=1").expect("1 is a function and fold is a pass");
        let running: Vec<&str> = opts.passes().into_iter().map(Pass::name).collect();
        assert_eq!(
            running,
            ["simplify-cfg", "fold"],
            "the flag has to put the pass in the pipeline"
        );
        let report = super::run(&mut module, &names, &opts);
        assert!(!spoke_about(&report, "fold", "f", &names), "nothing asked for f");
        assert!(spoke_about(&report, "fold", "g", &names));
        let text = rucc_ir::print(&module, &names);
        assert_eq!(text.matches("sext.i64").count(), 1, "{text}");
    }

    #[test]
    fn a_pass_gated_off_everywhere_runs_on_nothing_and_still_says_so() {
        let (names, mut module) = two_functions();
        let before = rucc_ir::print(&module, &names);
        let mut opts = Options::for_level(OptLevel::O2);
        for pass in pass::PASSES {
            opts.gates.add(false, pass.name()).expect("a pass in the list is a pass that exists");
        }
        let report = super::run(&mut module, &names, &opts);
        assert!(report.remarks.is_empty(), "a pass that did not run has nothing to report");
        assert_eq!(spent(&report, "fold"), Some(0), "the pass is still in the pipeline");
        assert_eq!(rucc_ir::print(&module, &names), before);
    }

    #[test]
    fn the_pipeline_listing_says_which_passes_a_gate_touched() {
        let mut opts = Options::for_level(OptLevel::O2);
        opts.gates.add(false, "fold=2-4").expect("fold is a pass");
        let text = super::print(&opts);
        assert!(text.contains("1: fold, "), "{text}");
        assert!(text.contains("[off for 2-4]"), "{text}");
        assert_eq!(text.matches('[').count(), 1, "a pass no gate mentions says nothing extra");
    }

    #[test]
    fn every_pass_at_no_fuel_leaves_the_module_exactly_as_it_found_it() {
        // The check section 9.10 asks for by name, and the reason it is here rather than in each
        // pass is that it has to hold for every pass that is ever added.
        for pass in pass::PASSES {
            let (names, mut module) = module();
            let before = rucc_ir::print(&module, &names);
            let mut opts = Options::for_level(OptLevel::O0);
            // The level's own pass out of the way first, so that what this measures is the one
            // pass under test. A pass turned off and then on again is on, so this is right for
            // that pass as well as for the others.
            opts.toggles.push(("simplify-cfg".to_owned(), false));
            opts.toggles.push((pass.name().to_owned(), true));
            opts.fuel.insert(pass.name().to_owned(), 0);
            let report = super::run(&mut module, &names, &opts);
            assert_eq!(
                report.spent,
                vec![(pass.name(), 0)],
                "{} spent fuel it had none of",
                pass.name()
            );
            assert_eq!(
                rucc_ir::print(&module, &names),
                before,
                "{} transformed the module at fuel zero",
                pass.name()
            );
        }
    }

    #[test]
    fn fuel_is_shared_across_the_functions_of_a_module() {
        let mut names = Interner::new();
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        for which in ["f", "g"] {
            let mut func =
                Func::new(names.intern(which), Signature::new().with_returns(&[Type::int(64)]));
            let block = func.create_block();
            let mut build = Builder::new(&mut func, block);
            let narrow = build.iconst(Type::int(32), 7);
            let wide = build.unary(Opcode::SExt, narrow, Type::int(64));
            build.ret(&[wide]);
            module.add_func(func);
        }
        let mut opts = Options::for_level(OptLevel::O2);
        opts.fuel.insert("fold".to_owned(), 1);
        let report = super::run(&mut module, &names, &opts);
        // One fold across both functions, because fuel is per pass and per compilation. Dead
        // code elimination has its own and spends it on the constant the one fold orphaned.
        assert_eq!(spent(&report, "fold"), Some(1));
        assert_eq!(spent(&report, "dce"), Some(1));
        let text = rucc_ir::print(&module, &names);
        assert_eq!(text.matches("sext.i64").count(), 1, "{text}");
    }

    #[test]
    fn global_fuel_is_spent_by_the_passes_in_order_and_the_rest_get_none() {
        let (names, mut module) = module();
        let mut opts = Options::for_level(OptLevel::O2);
        opts.global_fuel = Some(1);
        let report = super::run(&mut module, &names, &opts);
        // Folding is first and there is one thing to fold, so it takes the one unit and dead
        // code elimination gets nothing. Without the budget it would have taken the constant
        // that fold orphaned, which is what the other test measures.
        assert_eq!(spent(&report, "fold"), Some(1));
        assert_eq!(spent(&report, "dce"), Some(0));
        let text = rucc_ir::print(&module, &names);
        assert!(text.contains("iconst.i64 7"), "{text}");
        assert!(text.contains("iconst.i32 7"), "the orphaned constant is still there, {text}");
    }

    #[test]
    fn a_budget_of_nothing_leaves_the_module_alone_and_still_runs_every_pass() {
        let (names, mut module) = module();
        let before = rucc_ir::print(&module, &names);
        let mut opts = Options::for_level(OptLevel::O2);
        opts.global_fuel = Some(0);
        let report = super::run(&mut module, &names, &opts);
        assert_eq!(rucc_ir::print(&module, &names), before);
        assert!(report.spent.iter().all(|(_, spent)| *spent == 0), "{:?}", report.spent);
        // Every pass, because a pass out of fuel is a pass that ran and did nothing rather than
        // a pass that was skipped, and a bisection that skipped passes would be searching a
        // different pipeline at every step.
        assert_eq!(report.spent.len(), opts.passes().len());
    }

    #[test]
    fn the_tighter_of_the_two_limits_is_the_one_that_stops_the_pass() {
        // A pass allowed more than the budget gets the budget.
        let (names, mut under) = module();
        let mut opts = Options::for_level(OptLevel::O2);
        opts.global_fuel = Some(0);
        opts.fuel.insert("fold".to_owned(), 9);
        assert_eq!(spent(&super::run(&mut under, &names, &opts), "fold"), Some(0));

        // And a pass allowed less than the budget keeps its own limit, with the budget left
        // over for whatever comes after it.
        let (names, mut over) = module();
        let mut opts = Options::for_level(OptLevel::O2);
        opts.global_fuel = Some(9);
        opts.fuel.insert("fold".to_owned(), 0);
        let report = super::run(&mut over, &names, &opts);
        assert_eq!(spent(&report, "fold"), Some(0));
        assert_eq!(spent(&report, "dce"), Some(0), "nothing was orphaned for it to remove");
    }

    #[test]
    fn the_pipeline_listing_says_when_there_is_a_budget_and_says_nothing_when_there_is_not() {
        let opts = Options::for_level(OptLevel::O2);
        assert!(!super::print(&opts).contains("global fuel"));
        let with = Options { global_fuel: Some(12), ..Options::for_level(OptLevel::O2) };
        assert!(super::print(&with).contains("global fuel: 12"), "{}", super::print(&with));
    }

    #[test]
    fn a_dump_is_taken_on_the_side_that_asked_for_it_and_not_the_other() {
        let (names, mut module) = module();
        let mut opts = Options::for_level(OptLevel::O2);
        opts.dumps.add("after-fold").expect("a pass that exists");
        let report = super::run(&mut module, &names, &opts);
        assert_eq!(report.dumps.len(), 1);
        assert_eq!(report.dumps[0].name, "00-after-fold");
        assert!(report.dumps[0].text.contains("iconst.i64 7"));
    }

    #[test]
    fn asking_for_all_dumps_gives_both_sides_of_every_pass() {
        let (interner, mut module) = module();
        let opts = {
            let mut opts = Options::for_level(OptLevel::O2);
            opts.dumps.add("all").expect("all is always a dump");
            opts
        };
        let report = super::run(&mut module, &interner, &opts);
        // Both sides of every pass in the level, numbered by position, whatever the level
        // holds. Written out of the pipeline rather than as a literal, because the point of
        // the test is the pairing and the numbering and not which passes exist this month.
        let taken: Vec<&str> = report.dumps.iter().map(|d| d.name.as_str()).collect();
        let expected: Vec<String> = names(&opts)
            .into_iter()
            .enumerate()
            .flat_map(|(at, name)| {
                [format!("{at:02}-before-{name}"), format!("{at:02}-after-{name}")]
            })
            .collect();
        assert_eq!(taken, expected);
        assert!(report.dumps[0].text.contains("sext.i64"));
        assert!(!report.dumps[1].text.contains("sext.i64"));
    }

    #[test]
    fn every_pass_leaves_a_record_for_every_function_whether_or_not_it_had_anything_to_say() {
        let (names, mut module) = module();
        let opts = Options::for_level(OptLevel::O2);
        let report = super::run(&mut module, &names, &opts);
        let ran: Vec<&'static str> = opts.passes().into_iter().map(Pass::name).collect();
        // One function in the fixture, so one record per pass, and the passes in the order they
        // ran. A pass that found nothing is in here with an empty record, which is the point:
        // a pass that fires on nothing is either dead code or a bug, and output that leaves it
        // out cannot say which.
        let seen: Vec<&'static str> = report.remarks.iter().map(|it| it.pass).collect();
        assert_eq!(seen, ran);
        assert!(report.remarks.iter().all(|it| names.resolve(it.func) == "f"));
        assert!(
            report.remarks.iter().any(|it| it.pass == "simplify" && it.stats.is_empty()),
            "there is nothing in the fixture for the peephole to do"
        );
    }

    #[test]
    fn a_pass_spends_one_unit_of_fuel_for_each_rewrite_it_reports() {
        // The invariant that keeps the record honest, checked over every pass rather than
        // written into each one. Fuel is taken immediately before a transformation and a
        // rewrite is recorded immediately after it, so the two counts are the same number
        // arrived at from two directions. A pass where they disagree either transformed without
        // asking, which breaks bisection, or rewrote without recording, which means the manager
        // did not run the verifier over what it produced.
        let (names, mut module) = module();
        let report = super::run(&mut module, &names, &Options::for_level(OptLevel::O2));
        for (pass, spent) in &report.spent {
            assert_eq!(
                report.totals(pass).total(Kind::Optimized),
                *spent,
                "{pass} spent {spent} units of fuel and did not say on what"
            );
        }
        assert!(report.spent.iter().any(|(_, spent)| *spent > 0), "nothing happened at all");
    }

    #[test]
    fn what_the_passes_said_is_what_opt_info_prints() {
        let (names, mut module) = module();
        let report = super::run(&mut module, &names, &Options::for_level(OptLevel::O2));
        let text = crate::optinfo::render("t.c", &report, &names, crate::Wants::all());
        assert!(
            text.contains("t.c: f: optimized: integer instruction folded to a constant (1) [fold]"),
            "{text}"
        );
        assert!(
            text.contains(
                "t.c: f: optimized: instruction with no effects and no users removed (1) [dce]"
            ),
            "{text}"
        );
        // Nothing in the fixture is a miss, so asking only for the misses gets nothing back,
        // and that is different from the flag having been left off.
        let mut misses = crate::Wants::none();
        misses.add("missed").expect("that kind exists");
        assert_eq!(crate::optinfo::render("t.c", &report, &names, misses), "");
    }

    #[test]
    fn the_verifier_says_which_function_it_refused_and_leaves_the_others_out_of_it() {
        // Two functions with the same foldable body, and a block in the second one that nothing
        // reaches, which the verifier refuses. The pass is not what put it there, and the
        // complaint says the pass anyway, because a pass that hands back a function the
        // verifier will not take is where the search has to start whoever wrote the block.
        let mut names = Interner::new();
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        module.add_func(foldable(&mut names, "f"));
        let mut g = foldable(&mut names, "g");
        let stranded = g.create_block();
        let mut build = Builder::new(&mut g, stranded);
        let seven = build.iconst(Type::int(64), 7);
        build.ret(&[seven]);
        module.add_func(g);

        // Folding on its own, because simplify-CFG would take the stranded block out and there
        // would be nothing left to complain about.
        let mut opts = Options::for_level(OptLevel::O0);
        opts.toggles.push(("simplify-cfg".to_owned(), false));
        opts.toggles.push(("fold".to_owned(), true));
        opts.verify = true;
        let report = super::run(&mut module, &names, &opts);

        assert_eq!(report.broke.len(), 1, "{:?}", report.broke);
        let complaint = &report.broke[0];
        assert!(complaint.starts_with("the fold pass left invalid IR in g,"), "{complaint}");
        assert!(complaint.contains("this block is not reachable"), "{complaint}");
    }

    #[test]
    fn a_function_a_pass_did_not_change_is_not_verified_after_it() {
        // The stranded block is in `f` this time and `f` has nothing to fold, so the pass runs
        // over an invalid function, changes nothing, and says nothing. That is the whole trade:
        // the verifier answers for the rewrite that just happened, and a function no rewrite
        // touched was already answered for when it was built.
        let mut names = Interner::new();
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut f = Func::new(names.intern("f"), Signature::new().with_returns(&[Type::int(64)]));
        for _ in 0..2 {
            let block = f.create_block();
            let mut build = Builder::new(&mut f, block);
            let seven = build.iconst(Type::int(64), 7);
            build.ret(&[seven]);
        }
        module.add_func(f);
        module.add_func(foldable(&mut names, "g"));

        let mut opts = Options::for_level(OptLevel::O0);
        opts.toggles.push(("simplify-cfg".to_owned(), false));
        opts.toggles.push(("fold".to_owned(), true));
        opts.verify = true;
        let report = super::run(&mut module, &names, &opts);

        assert!(report.broke.is_empty(), "{:?}", report.broke);
        // And it did run on it, so this is the verifier staying quiet rather than the pass
        // being skipped.
        assert!(spoke_about(&report, "fold", "f", &names));
    }

    #[test]
    fn a_dump_of_a_pass_that_does_not_exist_is_refused_rather_than_ignored() {
        let mut dumps = Dumps::default();
        assert!(dumps.add("after-no-such-pass").is_err());
        assert!(dumps.add("sideways-fold").is_err());
        assert!(dumps.add("fold").is_err());
        assert!(dumps.is_empty());
    }
}
