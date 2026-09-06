//! Static branch prediction: which way a branch goes, when there is no profile that says.
//!
//! Design: section 11.2 of `spec/optimizer/11-profile-and-frequency.md`.
//!
//! # Ten predictors and not fifty five
//!
//! GCC has fifty five, in `gcc/predict.def`, each naming a syntactic situation and the rate at
//! which the guess turned out right when somebody measured it. Ten of those are for Fortran, and
//! a long tail of the rest sit below sixty five percent. Section 11.2 keeps ten: the ones that
//! survive both cuts. A predictor at fifty nine percent moves a probability nine points off even,
//! and nothing downstream of a frequency decides differently over nine points, so it costs a
//! branch of code here and buys nothing.
//!
//! The numbers themselves are Ball and Larus's and Wu and Larus's, from the middle of the 1990s,
//! and they have held up because they are facts about how people write programs rather than about
//! any machine. They live in [`rucc_cost::heuristics`] with the document that argued for them, the
//! way section 40.12 says every threshold has to.
//!
//! # First match
//!
//! The predictors are ordered and the first one that applies decides. GCC computes both this and a
//! Dempster-Shafer combination of every predictor that applies, and uses first match by default;
//! this does the part GCC uses. The order is the order section 11.2 lists them in and it is the
//! part of this file most worth getting right, because it is where the predictors disagree that
//! the order is doing anything at all. `__builtin_expect` is first because a user who wrote it
//! meant it, and the `cold` attribute is near the top for the same reason.
//!
//! # What a prediction is worth
//!
//! Every probability out of here is [`Quality::Guessed`], with one exception: a block with one
//! way out takes it, and that is [`Quality::Precise`] because it is not a guess. So a function
//! with no branches in it gets precise frequencies, which is the right answer and comes out of the
//! arithmetic rather than out of a special case.
//!
//! # Where the noreturn predictor gets its answer
//!
//! Two places, and only the first needs a call graph. The IR says it directly: the front end emits
//! [`Opcode::Unreachable`] after a call to a `noreturn` function, so a block from which no `return`
//! is reachable is a block control does not come back from, and that is a walk backwards from the
//! returns. The other place is the callee's own attributes, which are per function and not at the
//! call site, so a caller that has the module hands them over in [`Callees`]. A function pass that
//! has only its function passes [`Callees::nothing`] and keeps the first answer, which is most of
//! what the predictor was for: C error handling is `if (x) { report(); abort(); }` and it is the
//! `abort` that shows up as unreachable.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_cost::heuristics::{
    PREDICT_CALL_NOT_TAKEN, PREDICT_COLD_CALL, PREDICT_CONTINUE_TAKEN, PREDICT_EXPECT,
    PREDICT_LOOP_EXIT_NOT_TAKEN, PREDICT_LOOP_GUARD_TAKEN, PREDICT_NEGATIVE_RETURN,
    PREDICT_NEVER_RETURNS, PREDICT_NULL_RETURN, PREDICT_POINTER_NOT_NULL, PREDICT_RETURN_BLOCKS,
};
use rucc_ir::{AttrSet, Attrs, Block, Def, Extra, Func, Inst, IntPred, Module, Opcode, Value};

use crate::cfg::Cfg;
use crate::fold::constant;
use crate::loops::Loops;
use crate::profile::{Probability, Quality};

/// Which predictor decided a branch.
///
/// In the order they are asked, which is section 11.2's order, so a comparison between two of
/// these says which one wins where both apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Predictor {
    /// `__builtin_expect` named an arm.
    Expect,
    /// One arm does not come back.
    NeverReturns,
    /// One arm calls a function the user marked `cold`.
    ColdCall,
    /// One arm leaves the loop and the other stays in it.
    LoopExit,
    /// The branch decides whether to run a loop at all.
    LoopGuard,
    /// The condition compares a pointer against null.
    PointerNotNull,
    /// One arm returns a negative constant.
    NegativeReturn,
    /// One arm returns a null pointer.
    NullReturn,
    /// One arm contains a call and the other does not.
    CallNotTaken,
    /// One arm goes back to the top of the loop.
    Continue,
    /// Nothing applied, so the arms are even.
    Nothing,
}

impl Predictor {
    /// How it reads in a dump.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expect => "__builtin_expect",
            Self::NeverReturns => "the arm that does not come back",
            Self::ColdCall => "the arm that calls a cold function",
            Self::LoopExit => "the loop exit",
            Self::LoopGuard => "the loop guard",
            Self::PointerNotNull => "the pointer is not null",
            Self::NegativeReturn => "the arm that returns a negative number",
            Self::NullReturn => "the arm that returns null",
            Self::CallNotTaken => "the arm that calls something",
            Self::Continue => "the continue",
            Self::Nothing => "nothing, so even",
        }
    }

    /// The rate at which it was measured right, in percent, and fifty for no prediction at all.
    #[must_use]
    pub const fn hit_rate(self) -> u32 {
        match self {
            Self::Expect => PREDICT_EXPECT,
            Self::NeverReturns => PREDICT_NEVER_RETURNS,
            Self::ColdCall => PREDICT_COLD_CALL,
            Self::LoopExit => PREDICT_LOOP_EXIT_NOT_TAKEN,
            Self::LoopGuard => PREDICT_LOOP_GUARD_TAKEN,
            Self::PointerNotNull => PREDICT_POINTER_NOT_NULL,
            Self::NegativeReturn => PREDICT_NEGATIVE_RETURN,
            Self::NullReturn => PREDICT_NULL_RETURN,
            Self::CallNotTaken => PREDICT_CALL_NOT_TAKEN,
            Self::Continue => PREDICT_CONTINUE_TAKEN,
            // Even, which is the absence of a prediction rather than one, and not a number
            // anybody would tune.
            Self::Nothing => 50,
        }
    }

    /// The ten, in the order they are asked.
    pub const ORDER: [Self; 10] = [
        Self::Expect,
        Self::NeverReturns,
        Self::ColdCall,
        Self::LoopExit,
        Self::LoopGuard,
        Self::PointerNotNull,
        Self::NegativeReturn,
        Self::NullReturn,
        Self::CallNotTaken,
        Self::Continue,
    ];
}

impl std::fmt::Display for Predictor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the predictors know about the functions this one calls.
///
/// A call site carries the callee's name and the signature it is called with, and not the callee's
/// attributes, because the attributes belong to the callee and there is one of it and many call
/// sites. So whoever has the module builds this once and hands it over. A caller that does not
/// have one passes [`Callees::nothing`], which answers no to everything and costs the two
/// predictors that read it.
#[derive(Debug, Clone, Default)]
pub struct Callees {
    known: HashMap<Symbol, AttrSet>,
}

impl Callees {
    /// Nothing known about anything.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// Every function in the module, by name, with what it promises.
    ///
    /// Declarations count and are most of the value: `abort` is declared and not defined, and it
    /// is the one the predictor most wants to know about.
    #[must_use]
    pub fn of_module(module: &Module) -> Self {
        let mut known = HashMap::new();
        for id in module.funcs() {
            let func = &module[id];
            known.insert(func.name, func.attrs.set);
        }
        Self { known }
    }

    /// Records what one function promises, for a caller assembling this by hand.
    pub fn record(&mut self, name: Symbol, attrs: Attrs) {
        self.known.insert(name, attrs.set);
    }

    /// Whether control does not come back from a call to it.
    #[must_use]
    pub fn never_returns(&self, name: Symbol) -> bool {
        self.known.get(&name).is_some_and(|set| set.contains(AttrSet::NORETURN))
    }

    /// Whether the user said it is rarely called.
    #[must_use]
    pub fn is_cold(&self, name: Symbol) -> bool {
        self.known.get(&name).is_some_and(|set| set.contains(AttrSet::COLD))
    }
}

/// How likely each edge out of each block is.
///
/// Indexed the way the graph is: [`Predictions::edges`] gives one probability for each block in
/// [`Cfg::successors`], in that order. They sum to exactly [`Probability::SCALE`] for every block
/// that has any, which is what the frequency computation in section 11.3 needs and what the test
/// at the bottom of this file checks on every shape it builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predictions {
    edges: Vec<Vec<Probability>>,
    by: Vec<Predictor>,
}

impl Predictions {
    /// Predicts every branch in the function.
    ///
    /// Linear in the blocks and the edges, except for the two return value predictors, which walk
    /// forward from an arm over blocks with one way out and stop after
    /// [`PREDICT_RETURN_BLOCKS`] of them.
    #[must_use]
    pub fn of(func: &Func, cfg: &Cfg, loops: &Loops, callees: &Callees) -> Self {
        let width = cfg.capacity();
        let mut edges: Vec<Vec<Probability>> = vec![Vec::new(); width];
        let mut by = vec![Predictor::Nothing; width];
        let returns = returning(func, cfg);

        for block in func.blocks() {
            let Some(term) = func.terminator(block) else { continue };
            let succs = cfg.successors(block);
            if succs.len() == 2 && func[term].opcode == Opcode::BrIf {
                let (taken, who) = branch(func, cfg, loops, callees, &returns, block);
                edges[block.index()] = vec![taken, taken.complement()];
                by[block.index()] = who;
                continue;
            }
            let (parts, who) = share(func, cfg, callees, &returns, block, term);
            edges[block.index()] = parts;
            by[block.index()] = who;
        }

        Self { edges, by }
    }

    /// The probability of each edge out of this block, in [`Cfg::successors`] order.
    #[must_use]
    pub fn edges(&self, block: Block) -> &[Probability] {
        self.edges.get(block.index()).map_or(&[], Vec::as_slice)
    }

    /// The probability of the edge at that position among this block's successors.
    ///
    /// Zero for an edge that is not there, because the chance of taking an edge that does not
    /// exist is not a guess.
    #[must_use]
    pub fn taken(&self, block: Block, index: usize) -> Probability {
        self.edges(block).get(index).copied().unwrap_or_else(Probability::never)
    }

    /// Which predictor decided this block's branch.
    #[must_use]
    pub fn by(&self, block: Block) -> Predictor {
        self.by.get(block.index()).copied().unwrap_or(Predictor::Nothing)
    }
}

/// The probability of the first arm, given which arm the predictor thinks is taken.
fn toward(first: bool, percent: u32) -> Probability {
    let likely = Probability::percent(percent, Quality::Guessed);
    if first { likely } else { likely.complement() }
}

/// Predicts a two armed branch, first match, in section 11.2's order.
///
/// The answer is the probability of the first successor, which for a `br_if` is the arm taken when
/// the condition is one. The second gets the complement, so the two sum to certainty exactly.
fn branch(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    callees: &Callees,
    returns: &[bool],
    block: Block,
) -> (Probability, Predictor) {
    let succs = cfg.successors(block);
    let (first, second) = (succs[0], succs[1]);
    let term = func.terminator(block).expect("a block with successors has a terminator");
    let cond = *func[func[term].args].first().expect("a br_if has a condition");

    if let Some(taken) = expect(func, cond) {
        return (taken, Predictor::Expect);
    }

    let gone = |at: Block| never_comes_back(func, callees, returns, at);
    if gone(first) != gone(second) {
        return (toward(!gone(first), PREDICT_NEVER_RETURNS), Predictor::NeverReturns);
    }

    let cold = |at: Block| calls_named(func, at, |name| callees.is_cold(name));
    if cold(first) != cold(second) {
        return (toward(!cold(first), PREDICT_COLD_CALL), Predictor::ColdCall);
    }

    let leaves = |at: Block| match loops.innermost(block) {
        Some(id) => !loops.contains(id, at),
        None => false,
    };
    if leaves(first) != leaves(second) {
        return (toward(!leaves(first), PREDICT_LOOP_EXIT_NOT_TAKEN), Predictor::LoopExit);
    }

    let enters = |at: Block| enters_loop(cfg, loops, block, at);
    if enters(first) != enters(second) {
        return (toward(enters(first), PREDICT_LOOP_GUARD_TAKEN), Predictor::LoopGuard);
    }

    if let Some(taken) = pointer_null(func, cond) {
        return (taken, Predictor::PointerNotNull);
    }

    let gives = |at: Block| returns_constant(func, cfg, at);
    let negative = |at: Block| matches!(gives(at), Some(Returned::Negative));
    if negative(first) != negative(second) {
        return (toward(!negative(first), PREDICT_NEGATIVE_RETURN), Predictor::NegativeReturn);
    }
    let null = |at: Block| matches!(gives(at), Some(Returned::Null));
    if null(first) != null(second) {
        return (toward(!null(first), PREDICT_NULL_RETURN), Predictor::NullReturn);
    }

    let calls = |at: Block| has_call(func, at);
    if calls(first) != calls(second) {
        return (toward(!calls(first), PREDICT_CALL_NOT_TAKEN), Predictor::CallNotTaken);
    }

    let again = |at: Block| goes_round_again(loops, block, at);
    if again(first) != again(second) {
        return (toward(again(first), PREDICT_CONTINUE_TAKEN), Predictor::Continue);
    }

    (Probability::even(), Predictor::Nothing)
}

/// Splits a block's outgoing probability when it is not a two armed branch.
///
/// A jump takes its one edge, and that is a certainty rather than a guess. A `switch` and an
/// `indirect_br` split evenly, weighted by how many edges name each successor, because a block two
/// labels lead to is reached two ways. The one prediction that still applies is the noreturn one:
/// a `switch` arm that aborts is as unlikely here as it is on a branch, and the arms that come back
/// share what is left.
fn share(
    func: &Func,
    cfg: &Cfg,
    callees: &Callees,
    returns: &[bool],
    block: Block,
    term: Inst,
) -> (Vec<Probability>, Predictor) {
    let succs = cfg.successors(block);
    if succs.is_empty() {
        return (Vec::new(), Predictor::Nothing);
    }
    if succs.len() == 1 {
        return (vec![Probability::always()], Predictor::Nothing);
    }

    let mut weight = vec![0u64; succs.len()];
    for call in func.successors(term) {
        if let Some(at) = succs.iter().position(|&block| block == call.block) {
            weight[at] += 1;
        }
    }
    let gone: Vec<bool> =
        succs.iter().map(|&at| never_comes_back(func, callees, returns, at)).collect();

    let total = |side: bool| -> u64 {
        weight.iter().zip(&gone).filter(|&(_, &away)| away == side).map(|(w, _)| *w).sum()
    };
    let whole = u64::from(Probability::SCALE);
    let mut parts = vec![0u32; succs.len()];
    let who = if total(true) == 0 || total(false) == 0 {
        // Every arm comes back or none of them does, and either way there is nothing true of one
        // of them that is not true of all of them. The side with the weight takes everything.
        hand_out(whole, &weight, &gone, total(false) == 0, &mut parts);
        Predictor::Nothing
    } else {
        let budget = u64::from(
            Probability::percent(PREDICT_NEVER_RETURNS, Quality::Guessed).complement().parts(),
        );
        hand_out(budget, &weight, &gone, true, &mut parts);
        hand_out(whole - budget, &weight, &gone, false, &mut parts);
        Predictor::NeverReturns
    };

    let split = parts.into_iter().map(|parts| Probability::new(parts, Quality::Guessed)).collect();
    (split, who)
}

/// Divides a budget between the successors on one side of a question, in proportion to how many
/// edges lead to each.
///
/// What the division leaves over goes to the first of them, so the parts add up to the budget
/// exactly. A budget of nothing is a group that gets nothing and is not an error: a switch whose
/// every arm aborts has no arm to give the other side's share to.
fn hand_out(budget: u64, weight: &[u64], gone: &[bool], side: bool, parts: &mut [u32]) {
    let total: u64 =
        weight.iter().zip(gone).filter(|&(_, &away)| away == side).map(|(w, _)| *w).sum();
    if total == 0 || budget == 0 {
        return;
    }
    let mut spent = 0;
    let mut first = None;
    for (at, &w) in weight.iter().enumerate() {
        if gone[at] != side {
            continue;
        }
        let share = budget * w / total;
        parts[at] = u32::try_from(share).unwrap_or(Probability::SCALE);
        spent += share;
        if first.is_none() {
            first = Some(at);
        }
    }
    if let Some(at) = first {
        parts[at] += u32::try_from(budget - spent).unwrap_or(0);
    }
}

/// The prediction a `__builtin_expect` on the condition makes, if there is one.
///
/// Nothing emits [`Opcode::Expect`] today: `crates/rucc-sema/src/check/builtin/expect.rs` replaces
/// the call with its first argument and drops the hint, and it says why, which is that a node
/// every pass has to step over is a cost with no consumer. This is the consumer, so the hint has
/// somewhere to arrive.
fn expect(func: &Func, cond: Value) -> Option<Probability> {
    let Def::Result { inst, .. } = func[cond].def else { return None };
    if func[inst].opcode != Opcode::Expect {
        return None;
    }
    let hint = *func[func[inst].args].get(1)?;
    let (value, ty) = constant(func, hint)?;
    Some(toward(value.signed(ty) != 0, PREDICT_EXPECT))
}

/// The prediction a comparison of a pointer against null makes, if that is what the condition is.
fn pointer_null(func: &Func, cond: Value) -> Option<Probability> {
    let Def::Result { inst, .. } = func[cond].def else { return None };
    let data = &func[inst];
    if data.opcode != Opcode::ICmp {
        return None;
    }
    let Extra::IntPred(pred) = data.extra else { return None };
    let args = &func[data.args];
    let lhs = *args.first()?;
    let rhs = *args.get(1)?;
    // Exactly one side null. Both sides null is a comparison of two constants, which simplify-cfg
    // answers properly rather than guessing at.
    if is_null(func, lhs) == is_null(func, rhs) {
        return None;
    }
    match pred {
        IntPred::Eq => Some(toward(false, PREDICT_POINTER_NOT_NULL)),
        IntPred::Ne => Some(toward(true, PREDICT_POINTER_NOT_NULL)),
        _ => None,
    }
}

/// Whether this value is a null pointer.
///
/// Which is `int_to_ptr` of a zero, because that is what `crates/rucc-lower/src/body.rs` writes for
/// one: `iconst` produces an integer and never a pointer, so a pointer constant is always a
/// conversion of an integer one.
fn is_null(func: &Func, value: Value) -> bool {
    if !func[value].ty.is_ptr() {
        return false;
    }
    let Def::Result { inst, .. } = func[value].def else { return false };
    if func[inst].opcode != Opcode::IntToPtr {
        return false;
    }
    let Some(&arg) = func[func[inst].args].first() else { return false };
    match constant(func, arg) {
        Some((value, ty)) => value.signed(ty) == 0,
        None => false,
    }
}

/// What the two return value predictors found at the end of an arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Returned {
    /// A negative constant, which in C means the call failed.
    Negative,
    /// A null pointer.
    Null,
    /// A constant that is neither.
    Other,
}

/// What this arm returns, if it goes straight to a `return` of a constant.
///
/// Forward over blocks with one way out, stopping after [`PREDICT_RETURN_BLOCKS`] of them. GCC
/// propagates the prediction backwards from the return over every path that reaches it, which
/// needs the paths. This finds `if (bad) return -1;` and the two or three statements somebody put
/// in front of the return, which is the shape the predictor was measured on.
fn returns_constant(func: &Func, cfg: &Cfg, start: Block) -> Option<Returned> {
    let mut at = start;
    for _ in 0..PREDICT_RETURN_BLOCKS {
        let term = func.terminator(at)?;
        if func[term].opcode == Opcode::Return {
            let &value = func[func[term].args].first()?;
            if is_null(func, value) {
                return Some(Returned::Null);
            }
            let (value, ty) = constant(func, value)?;
            return Some(if value.signed(ty) < 0 { Returned::Negative } else { Returned::Other });
        }
        match cfg.successors(at) {
            [only] => at = *only,
            _ => return None,
        }
    }
    None
}

/// Whether control comes back from this block at all.
///
/// Two questions in one, because they have the same answer and the same consequence: whether a
/// `return` is reachable from here, and whether the block calls something the callee's own
/// attributes say does not come back.
fn never_comes_back(func: &Func, callees: &Callees, returns: &[bool], block: Block) -> bool {
    !returns[block.index()] || calls_named(func, block, |name| callees.never_returns(name))
}

/// Whether this block holds a direct call to a function the predicate accepts.
///
/// A call through a pointer is never one, because there is no name to ask about.
fn calls_named(func: &Func, block: Block, mut ok: impl FnMut(Symbol) -> bool) -> bool {
    func.insts(block).any(|inst| {
        let data = &func[inst];
        if !matches!(data.opcode, Opcode::Call | Opcode::TailCall) {
            return false;
        }
        let Extra::Call(at) = data.extra else { return false };
        match func[at].callee {
            Some(name) => ok(name),
            None => false,
        }
    })
}

/// Whether this block calls anything at all, by name or through a pointer.
fn has_call(func: &Func, block: Block) -> bool {
    func.insts(block).any(|inst| {
        matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall | Opcode::CallIndirect)
    })
}

/// Whether taking this edge runs a loop the branch is outside of.
///
/// The header itself, or the one block in front of it, because a guard the front end wrote usually
/// branches to a preheader rather than to the header.
fn enters_loop(cfg: &Cfg, loops: &Loops, from: Block, at: Block) -> bool {
    if heads_a_loop(loops, from, at) {
        return true;
    }
    match cfg.successors(at) {
        [only] => heads_a_loop(loops, from, *only),
        _ => false,
    }
}

/// Whether this block is the header of a loop the other block is not in.
fn heads_a_loop(loops: &Loops, from: Block, at: Block) -> bool {
    let Some(id) = loops.innermost(at) else { return false };
    loops.header(id) == at && !loops.contains(id, from)
}

/// Whether this edge is a `continue`, which is a jump back to the top from inside the body.
fn goes_round_again(loops: &Loops, from: Block, at: Block) -> bool {
    match loops.innermost(from) {
        Some(id) => loops.header(id) == at,
        None => false,
    }
}

/// Which blocks a `return` is reachable from.
///
/// Backwards from every block that ends in one. What this answers is the noreturn question without
/// a call graph: a block from which no return is reachable either aborts or spins forever, and in
/// C the first is nearly always what it is. A block with no terminator is a function under
/// construction and counts as not returning, which costs nothing because a pass asking this has a
/// function the verifier has already accepted.
fn returning(func: &Func, cfg: &Cfg) -> Vec<bool> {
    let mut yes = vec![false; cfg.capacity()];
    let mut stack = Vec::new();
    for block in func.blocks() {
        let Some(term) = func.terminator(block) else { continue };
        if matches!(func[term].opcode, Opcode::Return | Opcode::TailCall) {
            yes[block.index()] = true;
            stack.push(block);
        }
    }
    while let Some(block) = stack.pop() {
        for &pred in cfg.predecessors(block) {
            if !yes[pred.index()] {
                yes[pred.index()] = true;
                stack.push(pred);
            }
        }
    }
    yes
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        AttrSet, Attrs, Block, Builder, Func, InstData, IntPred, Opcode, Signature, Type,
    };

    use super::{Callees, Predictions, Predictor};
    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::loops::Loops;
    use crate::profile::{Probability, Quality};

    /// The three analyses a prediction is read against.
    fn shape(func: &Func) -> (Cfg, Loops) {
        let cfg = Cfg::new(func);
        let doms = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &doms);
        (cfg, loops)
    }

    /// Predicts with nothing known about any callee, which is what a function pass has.
    fn predict(func: &Func) -> (Predictions, Cfg) {
        let (cfg, loops) = shape(func);
        let seen = Predictions::of(func, &cfg, &loops, &Callees::nothing());
        (seen, cfg)
    }

    /// A function with `n` blocks and a name to call things by.
    fn blank(blocks: usize) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let list = (0..blocks).map(|_| func.create_block()).collect();
        (names, func, list)
    }

    #[test]
    fn a_block_with_one_way_out_takes_it_and_that_is_not_a_guess() {
        let (_, mut func, at) = blank(2);
        Builder::new(&mut func, at[0]).jump(at[1], &[]);
        let mut build = Builder::new(&mut func, at[1]);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let (seen, _) = predict(&func);
        assert_eq!(seen.edges(at[0]).len(), 1);
        assert_eq!(seen.taken(at[0], 0), Probability::always());
        assert_eq!(seen.taken(at[0], 0).quality(), Quality::Precise);
        // The block that returns has no edges at all, and an edge that is not there is not taken.
        assert!(seen.edges(at[1]).is_empty());
        assert_eq!(seen.taken(at[1], 0), Probability::never());
    }

    #[test]
    fn the_arm_that_does_not_come_back_is_the_one_not_taken() {
        // `if (x) abort();` as the front end leaves it, which is a branch to a block ending in
        // `unreachable`. No call graph is needed to see it.
        let (_, mut func, at) = blank(3);
        let mut build = Builder::new(&mut func, at[0]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[1], &[], at[2], &[]);
        Builder::new(&mut func, at[1]).unreachable();
        let mut build = Builder::new(&mut func, at[2]);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[0]), Predictor::NeverReturns);
        assert_eq!(seen.taken(at[0], 0), Probability::percent(99, Quality::Guessed).complement());
        assert_eq!(seen.taken(at[0], 1), Probability::percent(99, Quality::Guessed));
    }

    #[test]
    fn the_arm_that_calls_a_noreturn_function_is_the_one_not_taken() {
        // The same prediction from the other direction: control does come back from the block as
        // far as the graph is concerned, and it is the callee's attributes that say otherwise.
        let (mut names, mut func, at) = blank(4);
        let abort = names.intern("abort");
        let sig = func.add_signature(Signature::new());
        let mut build = Builder::new(&mut func, at[0]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[1], &[], at[2], &[]);
        let mut build = Builder::new(&mut func, at[1]);
        build.call(abort, sig, &[]);
        build.jump(at[3], &[]);
        Builder::new(&mut func, at[2]).jump(at[3], &[]);
        let mut build = Builder::new(&mut func, at[3]);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let mut callees = Callees::nothing();
        callees.record(abort, Attrs { set: AttrSet::NORETURN, ..Attrs::NONE });
        let (cfg, loops) = shape(&func);

        let told = Predictions::of(&func, &cfg, &loops, &callees);
        assert_eq!(told.by(at[0]), Predictor::NeverReturns);
        assert_eq!(told.taken(at[0], 0), Probability::percent(99, Quality::Guessed).complement());

        // And with nothing known about the callee the two arms are the same shape, so the call
        // predictor is what is left to say anything about them.
        let (guessed, _) = predict(&func);
        assert_eq!(guessed.by(at[0]), Predictor::CallNotTaken);
    }

    #[test]
    fn the_arm_that_calls_a_cold_function_is_the_one_not_taken() {
        let (mut names, mut func, at) = blank(4);
        let report = names.intern("report");
        let sig = func.add_signature(Signature::new());
        let mut build = Builder::new(&mut func, at[0]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[1], &[], at[2], &[]);
        let mut build = Builder::new(&mut func, at[1]);
        build.call(report, sig, &[]);
        build.jump(at[3], &[]);
        Builder::new(&mut func, at[2]).jump(at[3], &[]);
        let mut build = Builder::new(&mut func, at[3]);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let mut callees = Callees::nothing();
        callees.record(report, Attrs { set: AttrSet::COLD, ..Attrs::NONE });
        let (cfg, loops) = shape(&func);
        let told = Predictions::of(&func, &cfg, &loops, &callees);

        // The call predictor would also have fired here, at sixty seven percent. First match is
        // what makes the user's own statement win over the guess, which is what section 11.2
        // asks for when it says the attribute is honoured rather than blended.
        assert_eq!(told.by(at[0]), Predictor::ColdCall);
        assert_eq!(told.taken(at[0], 0), Probability::percent(99, Quality::Guessed).complement());
    }

    /// A loop: entry, header, body, exit, with the header testing and the body going round.
    fn loop_shape() -> (Func, Vec<Block>) {
        let (_, mut func, at) = blank(4);
        Builder::new(&mut func, at[0]).jump(at[1], &[]);
        let mut build = Builder::new(&mut func, at[1]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[2], &[], at[3], &[]);
        Builder::new(&mut func, at[2]).jump(at[1], &[]);
        let mut build = Builder::new(&mut func, at[3]);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);
        (func, at)
    }

    #[test]
    fn a_loop_exit_is_the_edge_not_taken() {
        let (func, at) = loop_shape();
        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[1]), Predictor::LoopExit);
        // Staying in the loop, which is the first arm here.
        assert_eq!(seen.taken(at[1], 0), Probability::percent(89, Quality::Guessed));
        assert_eq!(seen.taken(at[1], 1), Probability::percent(89, Quality::Guessed).complement());
    }

    #[test]
    fn a_loop_guard_is_taken_more_often_than_not() {
        // `if (n) { while (...) ... }`, where the guard branches to the preheader rather than to
        // the header, which is the shape the front end produces.
        let (_, mut func, at) = blank(6);
        let mut build = Builder::new(&mut func, at[0]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[1], &[], at[2], &[]);
        Builder::new(&mut func, at[1]).jump(at[3], &[]);
        Builder::new(&mut func, at[2]).jump(at[5], &[]);
        let mut build = Builder::new(&mut func, at[3]);
        let test = build.iconst(Type::int(1), 1);
        build.br_if(test, at[4], &[], at[5], &[]);
        Builder::new(&mut func, at[4]).jump(at[3], &[]);
        let mut build = Builder::new(&mut func, at[5]);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[0]), Predictor::LoopGuard);
        assert_eq!(seen.taken(at[0], 0), Probability::percent(73, Quality::Guessed));
    }

    #[test]
    fn a_continue_goes_round_again_more_often_than_it_falls_through() {
        let (_, mut func, at) = blank(5);
        Builder::new(&mut func, at[0]).jump(at[1], &[]);
        let mut build = Builder::new(&mut func, at[1]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[2], &[], at[3], &[]);
        let mut build = Builder::new(&mut func, at[2]);
        let again = build.iconst(Type::int(1), 1);
        build.br_if(again, at[1], &[], at[4], &[]);
        Builder::new(&mut func, at[4]).jump(at[1], &[]);
        let mut build = Builder::new(&mut func, at[3]);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[2]), Predictor::Continue);
        assert_eq!(seen.taken(at[2], 0), Probability::percent(67, Quality::Guessed));
    }

    #[test]
    fn a_pointer_tested_against_null_is_predicted_not_null() {
        let (_, mut func, at) = blank(3);
        let mut build = Builder::new(&mut func, at[0]);
        let seven = build.iconst(Type::int(64), 7);
        let some = build.unary(Opcode::IntToPtr, seven, Type::PTR);
        let zero = build.iconst(Type::int(64), 0);
        let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
        let cond = build.icmp(IntPred::Eq, some, null);
        build.br_if(cond, at[1], &[], at[2], &[]);
        for block in [at[1], at[2]] {
            let mut build = Builder::new(&mut func, block);
            let zero = build.iconst(Type::int(32), 0);
            build.ret(&[zero]);
        }

        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[0]), Predictor::PointerNotNull);
        // The arm taken when the pointer is null, which is the thirty percent of the time.
        assert_eq!(seen.taken(at[0], 0), Probability::percent(70, Quality::Guessed).complement());
    }

    #[test]
    fn an_arm_that_returns_a_negative_number_is_the_one_not_taken() {
        let (_, mut func, at) = blank(3);
        let mut build = Builder::new(&mut func, at[0]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[1], &[], at[2], &[]);
        let mut build = Builder::new(&mut func, at[1]);
        let bad = build.iconst(Type::int(32), -1);
        build.ret(&[bad]);
        let mut build = Builder::new(&mut func, at[2]);
        let good = build.iconst(Type::int(32), 0);
        build.ret(&[good]);

        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[0]), Predictor::NegativeReturn);
        assert_eq!(seen.taken(at[0], 0), Probability::percent(98, Quality::Guessed).complement());
    }

    #[test]
    fn an_arm_that_returns_null_is_the_one_not_taken_and_by_a_smaller_margin() {
        let (_, mut func, at) = blank(3);
        let mut build = Builder::new(&mut func, at[0]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[1], &[], at[2], &[]);
        let mut build = Builder::new(&mut func, at[1]);
        let zero = build.iconst(Type::int(64), 0);
        let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
        build.ret(&[null]);
        let mut build = Builder::new(&mut func, at[2]);
        let seven = build.iconst(Type::int(64), 7);
        let some = build.unary(Opcode::IntToPtr, seven, Type::PTR);
        build.ret(&[some]);

        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[0]), Predictor::NullReturn);
        assert_eq!(seen.taken(at[0], 0), Probability::percent(71, Quality::Guessed).complement());
        // The end of a list is an ordinary answer and a negative return is a failure, which is
        // why one of these predictors is at seventy one and the other at ninety eight.
        assert!(Predictor::NullReturn.hit_rate() < Predictor::NegativeReturn.hit_rate());
    }

    #[test]
    fn nothing_to_go_on_is_an_even_split_that_says_it_is_a_guess() {
        let (_, mut func, at) = blank(3);
        let mut build = Builder::new(&mut func, at[0]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, at[1], &[], at[2], &[]);
        for block in [at[1], at[2]] {
            let mut build = Builder::new(&mut func, block);
            let zero = build.iconst(Type::int(32), 0);
            build.ret(&[zero]);
        }

        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[0]), Predictor::Nothing);
        assert_eq!(seen.taken(at[0], 0), Probability::even());
        assert_eq!(seen.taken(at[0], 0).quality(), Quality::Guessed);
        assert!(!seen.taken(at[0], 0).is_predictable());
    }

    #[test]
    fn a_builtin_expect_wins_over_every_predictor_after_it() {
        // The arm the user named is also the arm that aborts, and the user wins. This is the one
        // test that says what first match is for: without it the noreturn predictor would answer,
        // and it would answer the other way round.
        let (_, mut func, at) = blank(3);
        let mut build = Builder::new(&mut func, at[0]);
        let value = build.iconst(Type::int(1), 1);
        let hint = build.iconst(Type::int(1), 1);
        let args = build.func().push_values(&[value, hint]);
        let cond = build.value(InstData { args, ..InstData::new(Opcode::Expect) }, Type::int(1));
        build.br_if(cond, at[1], &[], at[2], &[]);
        Builder::new(&mut func, at[1]).unreachable();
        let mut build = Builder::new(&mut func, at[2]);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[0]), Predictor::Expect);
        assert_eq!(seen.taken(at[0], 0), Probability::percent(90, Quality::Guessed));
    }

    #[test]
    fn a_builtin_expect_of_zero_names_the_other_arm() {
        let (_, mut func, at) = blank(3);
        let mut build = Builder::new(&mut func, at[0]);
        let value = build.iconst(Type::int(1), 1);
        let hint = build.iconst(Type::int(1), 0);
        let args = build.func().push_values(&[value, hint]);
        let cond = build.value(InstData { args, ..InstData::new(Opcode::Expect) }, Type::int(1));
        build.br_if(cond, at[1], &[], at[2], &[]);
        for block in [at[1], at[2]] {
            let mut build = Builder::new(&mut func, block);
            let zero = build.iconst(Type::int(32), 0);
            build.ret(&[zero]);
        }

        let (seen, _) = predict(&func);
        assert_eq!(seen.by(at[0]), Predictor::Expect);
        assert_eq!(seen.taken(at[0], 0), Probability::percent(90, Quality::Guessed).complement());
    }

    /// A switch on four values, where the first case aborts and the last two share a block.
    fn switch_shape() -> (Func, Vec<Block>) {
        let (_, mut func, at) = blank(5);
        let mut build = Builder::new(&mut func, at[0]);
        let value = build.iconst(Type::int(32), 0);
        build.switch(value, at[1], &[(0, at[2]), (1, at[3]), (2, at[4]), (3, at[4])]);
        Builder::new(&mut func, at[2]).unreachable();
        for block in [at[1], at[3], at[4]] {
            let mut build = Builder::new(&mut func, block);
            let zero = build.iconst(Type::int(32), 0);
            build.ret(&[zero]);
        }
        (func, at)
    }

    #[test]
    fn a_switch_arm_that_aborts_leaves_the_rest_to_share_what_is_left() {
        let (func, at) = switch_shape();
        let (seen, cfg) = predict(&func);
        let succs = cfg.successors(at[0]);
        let aborts = succs.iter().position(|&block| block == at[2]).expect("the arm is an edge");
        let shared = succs.iter().position(|&block| block == at[4]).expect("the arm is an edge");
        let alone = succs.iter().position(|&block| block == at[3]).expect("the arm is an edge");

        assert_eq!(seen.by(at[0]), Predictor::NeverReturns);
        // One percent between the arms that do not come back, of which there is one.
        assert_eq!(
            seen.taken(at[0], aborts),
            Probability::percent(99, Quality::Guessed).complement()
        );
        // Two cases lead to the same block, so it is reached two ways and gets twice the share.
        assert_eq!(seen.taken(at[0], shared).parts(), 2 * seen.taken(at[0], alone).parts());
    }

    #[test]
    fn the_edges_out_of_every_block_add_up_to_certainty() {
        // What the frequency computation in section 11.3 needs, and the one property of this file
        // that a caller is entitled to assume without reading it.
        let (guarded, _) = {
            let (_, mut func, at) = blank(3);
            let mut build = Builder::new(&mut func, at[0]);
            let cond = build.iconst(Type::int(1), 1);
            build.br_if(cond, at[1], &[], at[2], &[]);
            for block in [at[1], at[2]] {
                let mut build = Builder::new(&mut func, block);
                let zero = build.iconst(Type::int(32), 0);
                build.ret(&[zero]);
            }
            (func, at)
        };
        let (looped, _) = loop_shape();
        let (switched, _) = switch_shape();

        for func in [guarded, looped, switched] {
            let (seen, cfg) = predict(&func);
            for block in func.blocks() {
                let edges = seen.edges(block);
                if edges.is_empty() {
                    continue;
                }
                assert_eq!(edges.len(), cfg.successors(block).len());
                let total: u32 = edges.iter().map(|edge| edge.parts()).sum();
                assert_eq!(total, Probability::SCALE, "block {block:?} does not add up");
            }
        }
    }

    #[test]
    fn the_ten_are_the_ten_the_document_named_and_they_are_asked_in_its_order() {
        assert_eq!(Predictor::ORDER.len(), 10);
        assert!(!Predictor::ORDER.contains(&Predictor::Nothing));
        let mut sorted = Predictor::ORDER;
        sorted.sort_unstable();
        assert_eq!(sorted, Predictor::ORDER, "the enum order is the order they are asked in");
        for one in Predictor::ORDER {
            assert!(one.hit_rate() > Predictor::Nothing.hit_rate(), "{one} predicts nothing");
            assert!(!one.as_str().is_empty());
        }
    }
}
