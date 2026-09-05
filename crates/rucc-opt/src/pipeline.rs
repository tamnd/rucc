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
//! What the manager does beyond running the list is the three things that make a pass debuggable:
//! it counts each pass's transformations against its fuel, it dumps the IR around whichever
//! passes were asked for, and it runs the verifier after any pass that changed anything.

use std::collections::HashMap;
use std::fmt::Write as _;

use rucc_base::Interner;
use rucc_ir::Module;
use rucc_session::OptLevel;

use crate::{Fuel, Pass, pass};

/// `-O0`. Nothing. Section 9.1 gives this level SSA construction, which the lowering walk in
/// `spec/08-ir.md` already does, and mem2reg for the allocas that are left, which is the next
/// pass to be written. No analyses are computed and no dominator tree is built.
const O0: &[&str] = &[];

/// `-O1`. Section 9.1 asks for one e-graph round, conservative inlining, simplify-CFG, SROA,
/// GVN, DCE, LICM and the loop canonicalizations. Folding is the part of that which exists.
const O1: &[&str] = &["fold"];

/// `-O2`. The level the code quality claim is about. Section 9.1 asks for two e-graph rounds
/// around the loop pipeline, the full inlining cost model, Memory SSA and the full alias
/// analysis stack, and then the scalar and machine passes on top.
const O2: &[&str] = &["fold"];

/// `-O3`. `-O2` plus loop vectorization, larger inlining and unrolling thresholds, interchange
/// and distribution where the dependence analysis is confident, and function specialization.
const O3: &[&str] = &["fold"];

/// `-Os`. `-O2`'s passes under a size cost model: inlining only where it shrinks, no unrolling
/// and no vectorization.
const OS: &[&str] = &["fold"];

/// `-Oz`. `-Os` and additionally the outliner, with instruction selection preferring the smaller
/// encoding wherever there is a choice.
const OZ: &[&str] = &["fold"];

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

    /// The passes that will run, in order.
    ///
    /// A pass named by `-f<name>` that the level did not choose is appended, because the only
    /// place it could go that does not need an ordering rule nobody wrote down is the end.
    #[must_use]
    pub fn passes(&self) -> Vec<&'static dyn Pass> {
        let mut names: Vec<&str> = for_level(self.level).to_vec();
        for (name, on) in &self.toggles {
            let name = name.as_str();
            match *on {
                true if !names.contains(&name) => names.push(name),
                true => {}
                false => names.retain(|it| *it != name),
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
}

/// Runs the pipeline over the module.
///
/// Every pass sees every function with a body, one at a time, and a pass runs over the whole
/// module before the next one starts. That order is what makes the dumps readable: a dump is
/// the state of the program between two passes rather than between two functions.
pub fn run(module: &mut Module, names: &Interner, opts: &Options) -> Report {
    let mut report = Report::default();
    for (index, pass) in opts.passes().into_iter().enumerate() {
        let name = pass.name();
        if opts.dumps.wants_before(name) {
            report.dumps.push(dump(index, "before", name, module, names));
        }
        let mut fuel = match opts.fuel.get(name) {
            Some(&count) => Fuel::of(count),
            None => Fuel::unlimited(),
        };
        let mut changed = false;
        for id in module.funcs() {
            if module[id].is_declaration() {
                continue;
            }
            changed |= pass.run(&mut module[id], &mut fuel);
        }
        report.spent.push((name, fuel.spent()));
        // Only after a pass that says it changed something, because a verifier run over an
        // unchanged module is a verifier run over what the last one already accepted.
        if changed && opts.verify {
            if let Err(errors) = rucc_ir::verify(module, names) {
                for error in errors {
                    report.broke.push(format!("the {name} pass left invalid IR, {error}"));
                }
            }
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
    let passes = opts.passes();
    if passes.is_empty() {
        let _ = writeln!(out, "no passes");
        return out;
    }
    for (index, pass) in passes.iter().enumerate() {
        let _ = writeln!(out, "{}: {}, {}", index + 1, pass.name(), pass.describe());
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
    use crate::pass;

    /// A module with one function whose body has something to fold in it.
    fn module() -> (Interner, Module) {
        let mut names = Interner::new();
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_returns(&[Type::int(64)]));
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let narrow = build.iconst(Type::int(32), 7);
        let wide = build.unary(Opcode::SExt, narrow, Type::int(64));
        build.ret(&[wide]);
        module.add_func(func);
        (names, module)
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

    #[test]
    fn nothing_runs_at_no_optimization_and_something_runs_above_it() {
        assert!(Options::for_level(OptLevel::O0).passes().is_empty());
        assert!(!Options::for_level(OptLevel::O2).passes().is_empty());
    }

    #[test]
    fn a_pass_is_removed_by_no_and_added_by_the_bare_name_and_the_last_word_wins() {
        let mut opts = Options::for_level(OptLevel::O2);
        opts.toggles.push(("fold".to_owned(), false));
        assert!(opts.passes().is_empty());
        opts.toggles.push(("fold".to_owned(), true));
        assert_eq!(opts.passes().len(), 1);

        let mut off = Options::for_level(OptLevel::O0);
        off.toggles.push(("fold".to_owned(), true));
        assert_eq!(off.passes().len(), 1, "a pass the level did not choose is still reachable");
    }

    #[test]
    fn asking_for_a_pass_twice_does_not_run_it_twice() {
        let mut opts = Options::for_level(OptLevel::O2);
        opts.toggles.push(("fold".to_owned(), true));
        assert_eq!(opts.passes().len(), 1);
    }

    #[test]
    fn the_pipeline_listing_names_the_level_and_every_pass_in_order() {
        let text = super::print(&Options::for_level(OptLevel::O2));
        assert!(text.starts_with("level: -O2\n"), "{text}");
        assert!(text.contains("1: fold, "), "{text}");
        let none = super::print(&Options::for_level(OptLevel::O0));
        assert!(none.contains("no passes"), "{none}");
    }

    #[test]
    fn running_the_pipeline_changes_the_module_and_reports_what_it_spent() {
        let (names, mut module) = module();
        let report = super::run(&mut module, &names, &Options::for_level(OptLevel::O2));
        assert_eq!(report.spent, vec![("fold", 1)]);
        assert!(report.broke.is_empty(), "{:?}", report.broke);
        assert!(report.dumps.is_empty(), "nothing asked for a dump");
        assert!(rucc_ir::print(&module, &names).contains("iconst.i64 7"));
    }

    #[test]
    fn no_pass_runs_at_no_optimization_however_much_there_is_to_do() {
        let (names, mut module) = module();
        let before = rucc_ir::print(&module, &names);
        let report = super::run(&mut module, &names, &Options::for_level(OptLevel::O0));
        assert!(report.spent.is_empty());
        assert_eq!(rucc_ir::print(&module, &names), before);
    }

    #[test]
    fn every_pass_at_no_fuel_leaves_the_module_exactly_as_it_found_it() {
        // The check section 9.10 asks for by name, and the reason it is here rather than in each
        // pass is that it has to hold for every pass that is ever added.
        for pass in pass::PASSES {
            let (names, mut module) = module();
            let before = rucc_ir::print(&module, &names);
            let mut opts = Options::for_level(OptLevel::O0);
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
        assert_eq!(report.spent, vec![("fold", 1)]);
        let text = rucc_ir::print(&module, &names);
        assert_eq!(text.matches("sext.i64").count(), 1, "{text}");
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
        let (names, mut module) = module();
        let mut opts = Options::for_level(OptLevel::O2);
        opts.dumps.add("all").expect("all is always a dump");
        let report = super::run(&mut module, &names, &opts);
        let taken: Vec<&str> = report.dumps.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(taken, vec!["00-before-fold", "00-after-fold"]);
        assert!(report.dumps[0].text.contains("sext.i64"));
        assert!(!report.dumps[1].text.contains("sext.i64"));
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
