//! Memory SSA: the chain, and the budgeted walk back to the store a load sees.
//!
//! Design: `spec/optimizer/09-memory-ssa.md`. The representation is in `rucc-ir` and this is what
//! builds it and what reads it.
//!
//! # One variable
//!
//! GCC has had this since 2004 and calls it virtual operands: a statement that reads memory
//! carries a VUSE, one that writes memory carries a VDEF, and both are versions of one artificial
//! variable called `.MEM`. LLVM calls the same three things `MemoryUse`, `MemoryDef` and
//! `MemoryPhi`. The idea in both is to reuse the scalar SSA machinery for memory by pretending
//! memory is one scalar, and it is the right idea, so this does the same.
//!
//! The consequence is that the def-use chain over memory is maximally conservative. Every store
//! kills every load, structurally. All of the precision comes from walking it, which is what
//! [`Walk::clobber`] does.
//!
//! [`build`] is the construction: place a memory parameter at every join the memory versions
//! reach, which is the same iterated dominance frontier that SSA construction uses, and thread
//! the operand through every instruction that touches memory. A memory phi is an ordinary block
//! parameter, so nothing here is a side table and the CFG updates that keep memory SSA in step
//! with the blocks are the ones every other value already needed.
//!
//! # The walk
//!
//! [`Walk::clobber`] is GCC's `walk_non_aliased_vuses` at `gcc/tree-ssa-alias.cc:3915`. Given the
//! version of memory a load reads, it walks back through the defs, asks the alias analysis at each
//! one whether that def could have written what the load reads, and stops at the first one that
//! could. Two parts of GCC's interface are worth copying and both are here.
//!
//! **The budget.** `sccvn-max-alias-queries-per-access`, default 1000 at `gcc/params.opt:1020`,
//! and it is [`MAX_ALIAS_QUERIES_PER_ACCESS`] here under the same name, because a user who knows
//! to raise GCC's should not have to learn a second one. The walk is worst case quadratic: every
//! load can walk back through every store and each step is an alias query, so a function with a
//! thousand of each and no disambiguation is a million queries per pass that uses it, and there
//! are four such passes. Exceeding the budget gives [`Clobber::Unknown`], which is not an answer
//! and is not a no.
//!
//! **`translate`.** When the walk reaches a def it cannot see past, the caller may adjust the
//! reference and carry on, which is [`Step::Retry`]. This is what lets value numbering follow a
//! load through a `memcpy` by rewriting the reference to the copy's source, and section 9.2 says
//! it is the mechanism behind a surprising fraction of GCC's memory optimization. Without it the
//! walk is a stopping condition. With it, it is a way to rewrite the question.
//!
//! # Five answers, not two
//!
//! [`Clobber`] has five variants and the shape of it is deliberate. Section 9.6 names two ways
//! this goes wrong and the type is what rules both out.
//!
//! The first is a caller treating a budget exhaustion as a no. There is no `Option` anywhere in
//! the return and there is no default arm to fall into, so [`Clobber::Unknown`] has to be handled
//! by name.
//!
//! The second is partial overlap. A four byte store followed by a one byte load at offset one:
//! the load sees the store, but it cannot be replaced by the stored value, because the byte it
//! wants is somewhere inside that value and getting it out is a shift and a truncate. So a
//! clobber that wrote exactly the bytes of the reference is [`Clobber::Exact`], one that wrote
//! some of them is [`Clobber::Partial`], and one that may have written them is
//! [`Clobber::Maybe`]. Section 9.5 says getting this down to two answers is a class of
//! miscompilation.
//!
//! # What is conservative on purpose
//!
//! Every atomic and every fence is a full memory def and a full memory use. Section 9.5 says this
//! is correct and it is what M4 should do, and that doing better means modelling the memory model
//! rather than the memory, which is post-1.0. The failure mode it names is treating a relaxed
//! atomic load as an ordinary load because it orders nothing: it orders nothing and it is still a
//! load, and hoisting it out of a loop changes an observable. Atomics are never moved.
//!
//! `volatile` is checked before anything else and is never walked past. Alias analysis says
//! nothing about how many times an access happens and `volatile` constrains that too, so it is a
//! separate bit rather than a strong alias fact.
//!
//! # The cache
//!
//! There is not one. Section 9.3 is explicit: build the uncached walk, instrument how many alias
//! queries a `-O2` compilation makes, and add caching only if that number is a measurable
//! fraction of compile time. GCC has run without it for twenty years and LLVM's caching walker is
//! a large part of its MemorySSA complexity and a known source of invalidation bugs. The
//! instrumentation is the M4 deliverable and it is [`Counts`]. The number that decides it is the
//! fraction of walks that end by exhausting the budget rather than by finding a clobber: above
//! one percent and the budget is too small or the alias analysis is too weak, and both of those
//! are better fixed than cached around.

use std::collections::{HashMap, HashSet};

use rucc_ir::{
    Block, BlockCall, Def, Flags, Func, Inst, InstData, MemOrder, Module, Opcode, Type, Value,
};

use crate::alias::{Access, Alias, Answer, Options};
use crate::cfg::Cfg;
use crate::dom::Dominators;

/// How many alias queries one walk may make before it gives up.
///
/// GCC's `sccvn-max-alias-queries-per-access`, default 1000 at `gcc/params.opt:1020`, under the
/// same name on purpose. Exceeding it gives [`Clobber::Unknown`] rather than a wrong answer.
pub const MAX_ALIAS_QUERIES_PER_ACCESS: u32 = 1000;

/// What the walk found.
///
/// Five variants, and section 9.6 is why. Three of them are a clobber and they differ in how much
/// of the reference the clobber covers, because a caller that cannot tell `Exact` from `Partial`
/// replaces a one byte load with the wrong byte of a four byte store. The other two are the ways
/// a walk ends without one, and `Unknown` is not a no.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clobber {
    /// This instruction wrote exactly the bytes the reference covers.
    ///
    /// The only answer redundant load elimination may act on by taking the stored value, and
    /// even then only after checking the two types are the same width.
    Exact(Inst),
    /// This instruction wrote some of the reference, or wrote all of it and more.
    ///
    /// The load sees it, and what it sees cannot be had without taking part of what was stored
    /// or combining it with something else, which is document 16's decision rather than this
    /// one's.
    Partial(Inst),
    /// This instruction may have written the reference, and there is no telling how much.
    Maybe(Inst),
    /// Nothing in this function wrote it. The walk reached the start of the chain.
    NoClobber,
    /// The walk ran out of budget, or the paths into a join disagreed. Nothing is known.
    Unknown,
}

impl Clobber {
    /// The instruction, for the three answers that name one.
    #[must_use]
    pub const fn inst(self) -> Option<Inst> {
        match self {
            Self::Exact(inst) | Self::Partial(inst) | Self::Maybe(inst) => Some(inst),
            Self::NoClobber | Self::Unknown => None,
        }
    }
}

/// What a caller does when the walk reaches a def it cannot see past.
///
/// GCC's `translate` callback, section 9.2. A caller with no rewrite to offer says [`Step::Stop`]
/// and gets the clobber. One that can see through the def rewrites the reference and the walk
/// carries on with the new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Stop here. This is the answer.
    Stop,
    /// Carry on past this def, asking about this reference instead.
    Retry(Access),
}

/// What the walks have cost, which section 9.7 asks for as its own counter.
///
/// The walk is charged to whichever pass made it, so `-ftime-report` shows it under GVN and PRE
/// and not under memory SSA. That is misleading, and the fix section 9.7 asks for is to report
/// the step count separately from the wall time, because it is the thing to look at when a
/// pathological input turns up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    walks: u64,
    steps: u64,
    exhausted: u64,
}

impl Counts {
    /// How many walks were made.
    #[must_use]
    pub const fn walks(&self) -> u64 {
        self.walks
    }

    /// How many defs those walks looked at, which is one alias query each.
    #[must_use]
    pub const fn steps(&self) -> u64 {
        self.steps
    }

    /// How many walks ended by running out of budget.
    ///
    /// This is the number section 9.3 says decides whether the cache gets built. Above one
    /// percent of walks and the budget is too small or the alias analysis is too weak.
    #[must_use]
    pub const fn exhausted(&self) -> u64 {
        self.exhausted
    }
}

/// Puts a function on the memory chain, and says whether it did.
///
/// Construction is the same iterated dominance frontier SSA construction uses, over one variable:
/// the blocks that write memory are the definitions, the joins their versions reach get a memory
/// parameter, and a walk of the dominator tree threads the operand through every instruction that
/// touches memory. Linear with a dominance frontier factor, per section 9.7.
///
/// It gives back `false` and changes nothing for a function that has no memory operations at all,
/// for a declaration, and for one that is already on the chain. The first of those is the reason
/// the answer is a `bool` rather than nothing: a function with no memory in it must not get a
/// `mem_entry`, because a chain that starts and reaches nothing is a chain the verifier turns
/// down and a reader would have to interpret.
pub fn build(func: &mut Func) -> bool {
    let Some(entry) = func.entry() else {
        return false;
    };
    let cfg = Cfg::new(func);
    let doms = Dominators::new(&cfg);

    // Where the writes are, which is where the versions of memory are defined.
    let mut defs = vec![entry];
    let mut any = false;
    for block in func.blocks() {
        // A block nothing reaches is one the verifier turns down on its own, and it is not on
        // the dominator tree either, so threading would leave it off the chain and the chain
        // would then be neither all of the function nor none of it. Running the cleanup that
        // deletes it first is the caller's job.
        if !cfg.reaches(block) {
            return false;
        }
        let mut writes = false;
        for inst in func.insts(block) {
            if func.carries_mem(inst) {
                return false;
            }
            let opcode = func[inst].opcode;
            any |= opcode.touches_memory();
            writes |= opcode.writes_memory();
        }
        if writes && block != entry {
            defs.push(block);
        }
    }
    // An entry block with nothing in it has no terminator either, so this is not a function the
    // verifier would have let through and there is nothing sensible to build over it.
    let Some(first) = func.insts(entry).next() else {
        return false;
    };
    if !any {
        return false;
    }

    let joins = iterated_frontier(&cfg, &doms, &defs);
    let mut params = HashMap::new();
    for block in func.blocks().collect::<Vec<_>>() {
        if joins.contains(&block) {
            params.insert(block, func.append_param(block, Type::MEM));
        }
    }

    let start = start_of_chain(func, first);
    let ends = thread(func, &doms, &params, entry, start);
    pass_it_on(func, &params, &ends);
    true
}

/// The `mem_entry` above that instruction, which is where every chain starts.
///
/// It goes at the very top of the entry block, and the verifier insists on that: a start to the
/// chain anywhere else would have instructions above it that are on the chain and reach a version
/// of memory defined below them.
fn start_of_chain(func: &mut Func, first: Inst) -> Value {
    let span = func.span(first);
    let inst = func.create_inst(InstData::new(Opcode::MemEntry), &[Type::MEM], span);
    func.insert_before(inst, first);
    func[inst].results().next().expect("mem_entry produces one value")
}

/// Threads the operand through every instruction that touches memory, and says which version of
/// memory each block ends with.
///
/// The walk is over the dominator tree rather than the CFG, because the version reaching the top
/// of a block is the one its immediate dominator ended with unless the block has a parameter of
/// its own. That is the ordinary SSA renaming and memory is an ordinary variable here.
fn thread(
    func: &mut Func,
    doms: &Dominators,
    params: &HashMap<Block, Value>,
    entry: Block,
    start: Value,
) -> HashMap<Block, Value> {
    // An instruction cannot grow a result, so threading one makes a new instruction beside it and
    // the old one goes away. What the old one produced is forwarded to what the new one produces,
    // at the same positions, in one substitution at the end rather than as each is replaced,
    // because an instruction threaded early can be an operand of one threaded late.
    let mut forward: Vec<(Value, Value)> = Vec::new();
    let mut ends = HashMap::new();
    let mut stack = vec![(entry, start)];
    while let Some((block, incoming)) = stack.pop() {
        let mut current = params.get(&block).copied().unwrap_or(incoming);
        for inst in func.insts(block).collect::<Vec<_>>() {
            if !func[inst].opcode.touches_memory() {
                continue;
            }
            let fresh = func.with_mem(inst, current);
            func.insert_before(fresh, inst);
            for (old, new) in func[inst].results().zip(func[fresh].results()) {
                forward.push((old, new));
            }
            func.remove_inst(inst);
            if let Some(next) = func.mem_out(fresh) {
                current = next;
            }
        }
        ends.insert(block, current);
        stack.extend(doms.children(block).map(|child| (child, current)));
    }

    let forward: HashMap<Value, Value> = forward.into_iter().collect();
    if !forward.is_empty() {
        substitute(func, &forward);
    }
    ends
}

/// Replaces every use of what a threaded instruction produced with what its replacement produces.
fn substitute(func: &mut Func, forward: &HashMap<Value, Value>) {
    let with = |value: Value| forward.get(&value).copied().unwrap_or(value);
    for block in func.blocks().collect::<Vec<_>>() {
        for inst in func.insts(block).collect::<Vec<_>>() {
            let args = func[inst].args;
            func.rewrite(args, with);
            for call in func.successors(inst).collect::<Vec<_>>() {
                func.rewrite(call.args, with);
            }
        }
    }
}

/// Passes the version of memory each block ends with to the joins it branches to.
fn pass_it_on(func: &mut Func, params: &HashMap<Block, Value>, ends: &HashMap<Block, Value>) {
    for block in func.blocks().collect::<Vec<_>>() {
        let Some(terminator) = func.terminator(block) else {
            continue;
        };
        let Some(&value) = ends.get(&block) else {
            continue;
        };
        for at in func.target_list(terminator).iter() {
            let call = func[at];
            if !params.contains_key(&call.block) {
                continue;
            }
            // The memory parameter was appended last, so the argument goes last too, which is
            // the same rule the operand follows and for the same reason.
            let args = func.append_arg(call.args, value);
            func.set_block_call(at, BlockCall { block: call.block, args });
        }
    }
}

/// The blocks that need a memory parameter, which is the iterated dominance frontier of the
/// blocks that define a version of memory.
fn iterated_frontier(cfg: &Cfg, doms: &Dominators, defs: &[Block]) -> HashSet<Block> {
    let frontier = frontiers(cfg, doms);
    let mut placed = HashSet::new();
    let mut seen: HashSet<Block> = defs.iter().copied().collect();
    let mut work: Vec<Block> = defs.to_vec();
    while let Some(block) = work.pop() {
        let Some(targets) = frontier.get(&block) else {
            continue;
        };
        for &target in targets {
            if placed.insert(target) && seen.insert(target) {
                work.push(target);
            }
        }
    }
    placed
}

/// The dominance frontier of every block, by Cytron's walk from each join up to its immediate
/// dominator.
fn frontiers(cfg: &Cfg, doms: &Dominators) -> HashMap<Block, Vec<Block>> {
    let mut frontier: HashMap<Block, Vec<Block>> = HashMap::new();
    for block in cfg.reverse_postorder() {
        let preds = cfg.predecessors(block);
        if preds.len() < 2 {
            continue;
        }
        let Some(top) = doms.immediate_dominator(block) else {
            continue;
        };
        for &pred in preds {
            let mut runner = pred;
            while runner != top {
                let at = frontier.entry(runner).or_default();
                if !at.contains(&block) {
                    at.push(block);
                }
                let Some(next) = doms.immediate_dominator(runner) else {
                    break;
                };
                runner = next;
            }
        }
    }
    frontier
}

/// The walk back through the memory chain.
///
/// It borrows the function rather than owning anything, and it holds the alias analysis because
/// every step is a query and the escape analysis inside it is worth building once.
#[derive(Debug)]
pub struct Walk<'a> {
    func: &'a Func,
    cfg: Cfg,
    alias: Alias<'a>,
    limit: u32,
    counts: Counts,
}

impl<'a> Walk<'a> {
    /// A walk over this function, with GCC's budget.
    #[must_use]
    pub fn new(func: &'a Func, module: &'a Module) -> Self {
        Self::with(func, module, Options::default(), MAX_ALIAS_QUERIES_PER_ACCESS)
    }

    /// The same, with the alias options the command line left and a budget of your own.
    #[must_use]
    pub fn with(func: &'a Func, module: &'a Module, options: Options, limit: u32) -> Self {
        Self {
            func,
            cfg: Cfg::new(func),
            alias: Alias::with(func, module, options),
            limit,
            counts: Counts::default(),
        }
    }

    /// What the walks have cost so far.
    #[must_use]
    pub const fn counts(&self) -> &Counts {
        &self.counts
    }

    /// The alias analysis underneath, whose own counters say which layer answered.
    #[must_use]
    pub const fn alias(&self) -> &Alias<'a> {
        &self.alias
    }

    /// The store this load sees.
    ///
    /// [`Clobber::Unknown`] for an instruction that reads nothing, for one that is not on the
    /// chain, and for a walk that ran out of budget, because all three mean the same thing to a
    /// caller, which is that nothing was established.
    pub fn clobber(&mut self, load: Inst) -> Clobber {
        self.clobber_with(load, &mut |_, _| Step::Stop)
    }

    /// The same, with the chance to rewrite the reference at every def the walk cannot see past.
    ///
    /// Section 9.2's `translate`. The callback is handed the reference as it stands and the def
    /// in the way, and answers [`Step::Stop`] to take the clobber or [`Step::Retry`] to carry on
    /// past it asking about something else. Following a load through a `memcpy` by rewriting the
    /// reference to the copy's source is the case worth having it for, since that is what a
    /// struct assignment lowers to.
    ///
    /// Section 9.6 calls a `translate` that rewrites the reference wrongly the subtlest bug in
    /// the document and essentially untestable by unit test, so the defence is differential
    /// execution per document 41 rather than anything here.
    pub fn clobber_with(
        &mut self,
        load: Inst,
        translate: &mut dyn FnMut(&Access, Inst) -> Step,
    ) -> Clobber {
        let (Some(reference), Some(version)) = (self.alias.reads(load), self.func.mem_in(load))
        else {
            return Clobber::Unknown;
        };
        self.counts.walks += 1;
        let mut budget = self.limit;
        let mut seen = HashSet::new();
        let answer = self.back(reference, version, &mut budget, &mut seen, translate);
        // Nothing new on any path back is nothing that wrote it, which is the same answer as
        // reaching the start of the chain and is only reachable through a cycle of parameters.
        answer.unwrap_or(Clobber::NoClobber)
    }

    /// One version of memory, and everything that reaches it.
    ///
    /// `None` means this version has already been accounted for on another path, which is the
    /// neutral answer: it is how a loop is cut, since the back edge of a loop whose body writes
    /// nothing relevant leads back to the parameter the walk started from.
    fn back(
        &mut self,
        reference: Access,
        version: Value,
        budget: &mut u32,
        seen: &mut HashSet<Value>,
        translate: &mut dyn FnMut(&Access, Inst) -> Step,
    ) -> Option<Clobber> {
        if !seen.insert(version) {
            return None;
        }
        match self.func[version].def {
            // A memory phi. The answer is the same down every path into the block or it is not
            // an answer, which is conservative and is what keeps a caller from acting on a store
            // that only one predecessor made.
            Def::Param { block, index } => {
                let mut answer = None;
                for pred in self.cfg.predecessors(block).to_vec() {
                    let Some(terminator) = self.func.terminator(pred) else {
                        continue;
                    };
                    for call in self.func.successors(terminator).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let Some(&incoming) = self.func[call.args].get(index as usize) else {
                            continue;
                        };
                        let one = self.back(reference, incoming, budget, seen, translate);
                        answer = combine(answer, one);
                        if answer == Some(Clobber::Unknown) {
                            return answer;
                        }
                    }
                }
                answer
            }
            Def::Result { inst, .. } => {
                if self.func[inst].opcode == Opcode::MemEntry {
                    return Some(Clobber::NoClobber);
                }
                if *budget == 0 {
                    self.counts.exhausted += 1;
                    return Some(Clobber::Unknown);
                }
                *budget -= 1;
                self.counts.steps += 1;
                let past = match self.wrote(&reference, inst) {
                    None => reference,
                    Some(answer) => match translate(&reference, inst) {
                        Step::Stop => return Some(answer),
                        // The reference has changed, so a version already visited is a version
                        // worth visiting again. What stops this running away is the budget,
                        // which every step through a def spends whether or not the caller
                        // rewrote anything.
                        Step::Retry(next) => {
                            seen.clear();
                            next
                        }
                    },
                };
                let next = self.func.mem_in(inst)?;
                self.back(past, next, budget, seen, translate)
            }
        }
    }

    /// Whether this def wrote the reference, and how much of it.
    ///
    /// `None` is the answer that lets the walk carry on, and it is only given where the alias
    /// analysis said the two cannot touch the same byte.
    fn wrote(&mut self, reference: &Access, inst: Inst) -> Option<Clobber> {
        // Section 9.5, and it is first. Alias analysis says nothing about how many times an
        // access happens and `volatile` constrains that too, so this is a separate bit rather
        // than a strong alias fact, and it is checked before the analysis is asked anything.
        if reference.volatile || self.func[inst].flags.contains(Flags::VOLATILE) {
            return Some(Clobber::Maybe(inst));
        }
        // Every atomic and every fence is a full def and a full use. Pessimistic for lock-free
        // code and correct, and section 9.5 says doing better means modelling the memory model
        // rather than the memory, which is post-1.0.
        if self.ordered(inst) {
            return Some(Clobber::Maybe(inst));
        }
        if let Some(write) = self.alias.writes(inst) {
            return match self.alias.query(reference, &write) {
                Answer::No(_) => None,
                Answer::May => Some(self.extent(reference, &write, inst)),
            };
        }
        // A call, or anything else that writes memory without an access saying what. What a call
        // touches is its attributes and the escape analysis, which is section 8.4's, and without
        // those the honest answer is that it wrote everything.
        match self.alias.clobbered_by(reference, inst) {
            Answer::No(_) => None,
            Answer::May => Some(Clobber::Maybe(inst)),
        }
    }

    /// How much of the reference a write that may touch it covered.
    ///
    /// Two accesses to the same origin with both offsets and both sizes known are two runs of
    /// bytes at known places, and comparing them is what tells `Exact` from `Partial`. Anything
    /// less is `Maybe`, since a `May` from the alias analysis is not a proof that anything was
    /// written at all.
    ///
    /// `Exact` is the same bytes and not merely a superset of them. A four byte store and the
    /// one byte load at offset one inside it is `Partial`, because the byte the load wants is
    /// somewhere in the value the store wrote and getting it out is a shift and a truncate that
    /// document 16 decides on rather than this. Two runs that are the same bytes can still be
    /// two different types, and checking that is the caller's as well.
    fn extent(&self, reference: &Access, write: &Access, inst: Inst) -> Clobber {
        if reference.origin != write.origin {
            return Clobber::Maybe(inst);
        }
        let (Some(want), Some(wrote)) = (reference.range(), write.range()) else {
            return Clobber::Maybe(inst);
        };
        if want == wrote {
            Clobber::Exact(inst)
        } else if wrote.0 < want.1 && want.0 < wrote.1 {
            Clobber::Partial(inst)
        } else {
            // No overlap at all, which the alias analysis should have said no to. Saying `Maybe`
            // rather than walking past is the conservative reading of a disagreement.
            Clobber::Maybe(inst)
        }
    }

    /// Whether the instruction orders memory, which is every atomic and every fence.
    fn ordered(&self, inst: Inst) -> bool {
        use rucc_ir::Extra;
        let order = match self.func[inst].extra {
            Extra::Mem(at) => self.func[at].order,
            Extra::Rmw(_, at) => self.func[at].order,
            Extra::Order(order) => order,
            _ => return false,
        };
        order != MemOrder::NotAtomic
    }
}

/// Two answers from two paths into a join.
///
/// The same answer on both is the answer. Nothing on one path is whatever the other said, which
/// is how a cycle contributes nothing. Anything else is a disagreement, and a disagreement is
/// `Unknown` rather than the weaker of the two, because there is no order on these that a caller
/// could act on.
fn combine(a: Option<Clobber>, b: Option<Clobber>) -> Option<Clobber> {
    match (a, b) {
        (None, other) | (other, None) => other,
        (Some(one), Some(other)) if one == other => Some(one),
        _ => Some(Clobber::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, MemInfo, Restrict, Signature, parse, verify_func};

    use super::*;

    /// A module and a function built from the text, which is how these are written.
    fn read(text: &str) -> (Module, Interner) {
        let mut names = Interner::new();
        let module = parse(text, &mut names).expect("the text parses");
        (module, names)
    }

    const HEADER: &str = "\
; ModuleID = 'mem.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
";

    fn wrap(signature: &str, body: &str) -> String {
        format!("{HEADER}\nfunc @f{signature}, linkage(external) {{\n{body}}}\n")
    }

    /// Builds memory SSA over the function and insists the result verifies, which is where most
    /// of the strength of these tests is: the rules in the verifier are the specification of the
    /// chain and construction has to satisfy all of them.
    fn built(text: &str) -> (Module, bool) {
        let (mut module, names) = read(text);
        let id = module.funcs().next().expect("one function");
        let changed = build(&mut module[id]);
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("{errors:#?}");
        }
        (module, changed)
    }

    fn one(module: &Module) -> &Func {
        &module[module.funcs().next().expect("one function")]
    }

    /// The instruction with that opcode, counting from the top of the function.
    fn nth(func: &Func, opcode: Opcode, want: usize) -> Inst {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == opcode)
            .nth(want)
            .expect("that many of them")
    }

    #[test]
    fn a_function_with_no_memory_in_it_gets_no_chain() {
        let text = wrap(
            "(i32) -> i32",
            "block0(%0: i32):
    %1 = add %0, %0
    return %1
",
        );
        let (module, changed) = built(&text);
        assert!(!changed);
        assert_eq!(one(&module).blocks().count(), 1);
    }

    #[test]
    fn a_straight_line_is_threaded_in_order() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    %2 = load.i32 %0, align 4
    return %2
",
        );
        let (module, changed) = built(&text);
        assert!(changed);
        let func = one(&module);
        let start = nth(func, Opcode::MemEntry, 0);
        let store = nth(func, Opcode::Store, 0);
        let load = nth(func, Opcode::Load, 0);
        assert_eq!(func.mem_in(store), func.mem_out(start));
        assert_eq!(func.mem_in(load), func.mem_out(store));
        assert_eq!(func.mem_out(load), None);
    }

    #[test]
    fn a_join_gets_a_memory_parameter_and_every_branch_passes_one() {
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    br_if %1, block1, block2

block1:
    %2 = iconst.i32 7
    store %2 -> %0, align 4
    jump block3

block2:
    jump block3

block3:
    %3 = load.i32 %0, align 4
    return %3
",
        );
        let (module, _) = built(&text);
        let func = one(&module);
        let join = func.blocks().nth(3).expect("four blocks");
        assert_eq!(func[join].params.len(), 1);
        let param = func[join].params[0];
        assert!(func[param].ty.is_mem());
        assert_eq!(func.mem_in(nth(func, Opcode::Load, 0)), Some(param));
    }

    #[test]
    fn a_block_that_only_reads_needs_no_parameter() {
        let text = wrap(
            "(ptr, i1) -> i32",
            "block0(%0: ptr, %1: i1):
    br_if %1, block1, block2

block1:
    %2 = load.i32 %0, align 4
    jump block3

block2:
    jump block3

block3:
    %3 = load.i32 %0, align 4
    return %3
",
        );
        let (module, _) = built(&text);
        let func = one(&module);
        // One version of memory reaches the whole function, so no join needs a parameter and
        // every load reads what `mem_entry` produced.
        for block in func.blocks() {
            assert!(func[block].params.iter().all(|&param| !func[param].ty.is_mem()));
        }
    }

    #[test]
    fn every_arm_of_a_switch_passes_its_own_version_along() {
        let text = wrap(
            "(ptr, i32) -> i32",
            "block0(%0: ptr, %1: i32):
    switch %1, block1, [0 => block2, 1 => block3]

block1:
    %2 = iconst.i32 1
    store %2 -> %0, align 4
    jump block4

block2:
    %3 = iconst.i32 2
    store %3 -> %0, align 4
    jump block4

block3:
    jump block4

block4:
    %4 = load.i32 %0, align 4
    return %4
",
        );
        let (module, _) = built(&text);
        let func = one(&module);
        let join = func.blocks().nth(4).expect("five blocks");
        let param = *func[join].params.last().expect("a parameter");
        assert!(func[param].ty.is_mem());
        // Each arm reaches the join with the version it ended on, and the two that wrote reach
        // it with the version their own store produced.
        for (arm, want) in [(1, Some(0)), (2, Some(1)), (3, None)] {
            let block = func.blocks().nth(arm).expect("that block");
            let jump = func.terminator(block).expect("a terminator");
            let call = func.successors(jump).next().expect("one target");
            let sent = *func[call.args].last().expect("an argument");
            let expect = match want {
                Some(store) => func.mem_out(nth(func, Opcode::Store, store)),
                None => func.mem_out(nth(func, Opcode::MemEntry, 0)),
            };
            assert_eq!(Some(sent), expect, "arm {arm} passed the wrong version");
        }
    }

    #[test]
    fn a_function_with_a_block_nothing_reaches_is_left_alone() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    jump block2

block1:
    %2 = iconst.i32 9
    store %2 -> %0, align 4
    jump block2

block2:
    %3 = load.i32 %0, align 4
    return %3
",
        );
        // Block 1 has no predecessor. Half a function on the chain is worse than none of it, so
        // this declines rather than producing something the verifier would turn down.
        let (mut module, _) = read(&text);
        let id = module.funcs().next().expect("one function");
        assert!(!build(&mut module[id]));
        assert_eq!(module[id].blocks().filter(|&b| !module[id][b].params.is_empty()).count(), 1);
    }

    /// The last load in the function, which is the one every walk here starts from.
    fn last_load(func: &Func) -> Inst {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::Load)
            .last()
            .expect("a load")
    }

    /// A load, a store and the walk between them, over a function written as text.
    fn walked(text: &str) -> (Clobber, Counts) {
        let (module, changed) = built(text);
        assert!(changed, "the function has memory in it");
        let func = one(&module);
        let mut walk = Walk::new(func, &module);
        let answer = walk.clobber(last_load(func));
        (answer, *walk.counts())
    }

    #[test]
    fn a_load_sees_the_store_before_it() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    %2 = load.i32 %0, align 4
    return %2
",
        );
        let (answer, counts) = walked(&text);
        assert!(matches!(answer, Clobber::Exact(_)));
        assert_eq!(counts.walks(), 1);
        assert_eq!(counts.steps(), 1);
        assert_eq!(counts.exhausted(), 0);
    }

    #[test]
    fn a_load_walks_past_a_store_to_another_object() {
        let text = wrap(
            "() -> i32",
            "block0:
    %0 = alloca, size 8, align 8
    %1 = alloca, size 8, align 8
    %2 = iconst.i32 7
    store %2 -> %0, align 4
    %3 = load.i32 %1, align 4
    return %3
",
        );
        let (answer, counts) = walked(&text);
        assert_eq!(answer, Clobber::NoClobber);
        // It looked at the store, said no, and reached the start of the chain.
        assert_eq!(counts.steps(), 1);
    }

    #[test]
    fn a_load_of_one_byte_of_a_wider_store_is_partial() {
        let text = wrap(
            "() -> i8",
            "block0:
    %0 = alloca, size 8, align 8
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    %2 = iconst.i64 1
    %3 = ptr_add %0, %2
    %4 = load.i8 %3, align 1
    return %4
",
        );
        let (answer, _) = walked(&text);
        assert!(matches!(answer, Clobber::Partial(_)), "{answer:?}");
    }

    #[test]
    fn a_load_after_a_call_that_cannot_reach_it_walks_past_the_call() {
        let text = wrap(
            "() -> i32",
            "block0:
    %0 = alloca, size 8, align 8
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    call @g() : ()
    %2 = load.i32 %0, align 4
    return %2
",
        );
        // The local's address never leaves the function, so the call cannot touch it and the
        // walk goes straight past to the store. That is the escape layer paying for itself.
        let (answer, _) = walked(&text);
        assert!(matches!(answer, Clobber::Exact(_)), "{answer:?}");
    }

    #[test]
    fn a_load_after_a_call_that_could_have_the_address_sees_the_call() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    call @g() : ()
    %2 = load.i32 %0, align 4
    return %2
",
        );
        let (answer, _) = walked(&text);
        assert!(matches!(answer, Clobber::Maybe(_)), "{answer:?}");
    }

    #[test]
    fn a_load_after_an_atomic_store_sees_it_whatever_it_wrote() {
        let text = wrap(
            "() -> i32",
            "block0:
    %0 = alloca, size 8, align 8
    %1 = alloca, size 8, align 8
    %2 = iconst.i32 7
    atomic_store %2 -> %0, align 4, release
    %3 = load.i32 %1, align 4
    return %3
",
        );
        // Two different objects, and it still stops: an atomic is a full def and a full use, per
        // section 9.5, and this is the test that says so rather than a comment.
        let (answer, _) = walked(&text);
        assert!(matches!(answer, Clobber::Maybe(_)), "{answer:?}");
    }

    #[test]
    fn a_load_after_a_volatile_store_sees_it_whatever_it_wrote() {
        let text = wrap(
            "() -> i32",
            "block0:
    %0 = alloca, size 8, align 8
    %1 = alloca, size 8, align 8
    %2 = iconst.i32 7
    store.volatile %2 -> %0, align 4
    %3 = load.i32 %1, align 4
    return %3
",
        );
        let (answer, _) = walked(&text);
        assert!(matches!(answer, Clobber::Maybe(_)), "{answer:?}");
    }

    #[test]
    fn paths_that_disagree_are_unknown_rather_than_the_weaker_of_the_two() {
        let text = wrap(
            "(i1) -> i32",
            "block0(%0: i1):
    %1 = alloca, size 8, align 8
    br_if %0, block1, block2

block1:
    %2 = iconst.i32 7
    store %2 -> %1, align 4
    jump block3

block2:
    jump block3

block3:
    %3 = load.i32 %1, align 4
    return %3
",
        );
        let (answer, _) = walked(&text);
        assert_eq!(answer, Clobber::Unknown);
    }

    #[test]
    fn a_loop_that_writes_nothing_relevant_walks_out_of_it() {
        let text = wrap(
            "(i32) -> i32",
            "block0(%0: i32):
    %1 = alloca, size 8, align 8
    %2 = alloca, size 8, align 8
    %3 = iconst.i32 7
    store %3 -> %1, align 4
    jump block1(%0)

block1(%4: i32):
    %5 = iconst.i32 1
    %6 = sub %4, %5
    store %5 -> %2, align 4
    %7 = icmp sgt %6, %5
    br_if %7, block1(%6), block2

block2:
    %8 = load.i32 %1, align 4
    return %8
",
        );
        // The store in the loop is to the other object, so the walk goes round the back edge,
        // meets the parameter it started from, contributes nothing, and takes the answer from
        // the path that leaves the loop.
        let (answer, counts) = walked(&text);
        assert!(matches!(answer, Clobber::Exact(_)), "{answer:?}");
        assert_eq!(counts.exhausted(), 0);
    }

    #[test]
    fn a_budget_of_nothing_gives_unknown_and_says_so() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    %2 = load.i32 %0, align 4
    return %2
",
        );
        let (module, _) = built(&text);
        let func = one(&module);
        let load = nth(func, Opcode::Load, 0);
        let mut walk = Walk::with(func, &module, Options::default(), 0);
        assert_eq!(walk.clobber(load), Clobber::Unknown);
        assert_eq!(walk.counts().exhausted(), 1);
    }

    #[test]
    fn translate_carries_the_walk_past_a_def_it_would_have_stopped_at() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    memcpy %0, %0, size 4, align 4
    %2 = load.i32 %0, align 4
    return %2
",
        );
        let (module, _) = built(&text);
        let func = one(&module);
        let load = nth(func, Opcode::Load, 0);

        // With no rewrite to offer, the copy is where it stops.
        let mut walk = Walk::new(func, &module);
        let stopped_at = walk.clobber(load).inst().expect("something wrote it");
        assert_eq!(func[stopped_at].opcode, Opcode::Memcpy);

        // The same walk, with a caller that can see through the copy. It says nothing about the
        // reference here, which is enough to show the callback is reached and obeyed.
        let mut walk = Walk::new(func, &module);
        let mut seen = Vec::new();
        let answer = walk.clobber_with(load, &mut |reference, inst| {
            seen.push(func[inst].opcode);
            if func[inst].opcode == Opcode::Memcpy { Step::Retry(*reference) } else { Step::Stop }
        });
        assert_eq!(seen, [Opcode::Memcpy, Opcode::Store]);
        assert_eq!(answer.inst().map(|inst| func[inst].opcode), Some(Opcode::Store));
    }

    #[test]
    fn building_twice_changes_nothing_the_second_time() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = load.i32 %0, align 4
    return %1
",
        );
        let (mut module, _) = read(&text);
        let id = module.funcs().next().expect("one function");
        let func = &mut module[id];
        assert!(build(func));
        let before = func.counts().insts;
        assert!(!build(func));
        assert_eq!(func.counts().insts, before);
    }

    /// The builder path rather than the parser path, since a pass that adds a store adds it with
    /// the builder and the chain has to survive that too.
    #[test]
    fn a_function_built_by_hand_threads_the_same_way() {
        let mut names = Interner::new();
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("f"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        let addr = func.append_param(entry, Type::PTR);
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let seven = b.iconst(i32_, 7);
        b.store(seven, addr, info, Flags::NONE);
        let read = b.load(i32_, addr, info, Flags::NONE);
        b.ret(&[read]);

        assert!(build(&mut func));
        let store = nth(&func, Opcode::Store, 0);
        let load = nth(&func, Opcode::Load, 0);
        assert_eq!(func.mem_in(load), func.mem_out(store));
    }
}
