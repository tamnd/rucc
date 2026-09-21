//! What a function does to memory, one answer for each pointer parameter.
//!
//! Section 34.3 of `spec/optimizer/34-ipa.md`, and the analysis section 34.6 asks for right after
//! [`crate::purity`]. Purity answers one question about the whole of memory: does this function
//! read it, does it write it. That is enough to delete a call whose result nobody reads and it is
//! not enough to move a load past one, because a loop that calls `helper(other)` and reloads
//! `mine[i]` does not want to know whether `helper` wrote something. It wants to know whether
//! `helper` wrote *this*, and the answer to that is per parameter.
//!
//! # What a summary holds, which is section 34.6's deliverable and no more
//!
//! For each pointer parameter: does the function read through it, does it write through it, does
//! the address go somewhere the caller cannot see. And one answer for everything else, which is
//! the globals and every address the body did not get as an argument.
//!
//! No offsets, no sizes, no access trees, no aggregate granularity. A parameter is a whole object
//! here or it is nothing. `gcc/ipa-modref.cc` keeps far more than this over five and a half
//! thousand lines, and the part that pays for itself in a loop is the part that is here.
//!
//! # Working it out
//!
//! [`summarize`] is the analysis and it goes the way [`crate::purity::infer`] goes, for section
//! 34.5's reason: start every function at "touches nothing", read the bodies over the condensation
//! callee before caller, and lower an answer when the body contradicts it. Starting at the other
//! end and raising would answer "writes everything" for a pair of functions that call each other
//! and touch nothing, which is the case the optimism is for.
//!
//! The escape analysis underneath is the same walk [`Escapes`] does and it runs with what this
//! module has worked out so far, which is section 34.6's upgrade to it: an address handed to a
//! call is an address gone to [`Escapes::of`], and that is most of what a C program does with the
//! address of a local, so a callee whose summary says it keeps nothing takes a whole class of
//! locals back out of the escaped set. Both directions of that are used here. A local this
//! function only lent out stays private, so what the callee did to it never reaches the summary,
//! and a parameter handed on takes the callee's own answer for that position rather than the
//! blanket one.
//!
//! One thing is deliberately not as precise as it could be, and it is written down in
//! tamnd/rucc#1557 rather than done here: a parameter is a whole object, so a callee that writes
//! one field of a structure is a callee that wrote the structure.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_ir::{AttrSet, Block, Def, Func, Inst, Module, Opcode, Value};

use crate::alias::{Escapes, Origin, keeps_address, origin};
use crate::callgraph::{CallGraph, Node};
use crate::purity::Callee;

/// How many instructions of one body the walk will read before it gives up on that body.
///
/// `gcc/params.opt:300` gives `ipa-max-aa-steps` an `Init(25000)` for the same job, and the same
/// number is used here because the thing it is protecting is the same: a generated function with
/// a hundred thousand instructions in it should cost a compile that is linear in the module and
/// not one that is quadratic in the worst function. A body over the limit gets the answer that
/// cannot be wrong, which costs its callers precision and costs nobody a correct program.
const MAX_STEPS: usize = 25_000;

/// What a function does to one part of memory.
///
/// Ordered weakest first, so that adding up what a body does is a maximum and narrowing what was
/// promised against what was worked out is a minimum. There is no fourth value for writing without
/// reading: a summary that claimed it would have to be believed by a load as well as by a store,
/// and nothing at this granularity has earned that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effect {
    /// Never touched.
    #[default]
    Nothing,
    /// Read and not written.
    Reads,
    /// Written, and read as far as anything here can tell.
    Writes,
}

impl Effect {
    /// Both of these happened.
    #[must_use]
    pub fn and_then(self, other: Self) -> Self {
        self.max(other)
    }

    /// Both of these are true of the same function, so the tighter one is.
    #[must_use]
    pub fn as_well_as(self, other: Self) -> Self {
        self.min(other)
    }

    /// Whether the bytes can have been read.
    #[must_use]
    pub fn reads(self) -> bool {
        self != Self::Nothing
    }

    /// Whether the bytes can have been written.
    #[must_use]
    pub fn writes(self) -> bool {
        self == Self::Writes
    }

    /// What this reads as in a remark.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Nothing => "nothing",
            Self::Reads => "reads",
            Self::Writes => "writes",
        }
    }
}

/// What a function does to the object one parameter points at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Touch {
    /// What happens to the bytes.
    pub effect: Effect,
    /// Whether the address itself ends up somewhere the caller cannot see, which is what stops
    /// the caller reasoning about the object after the call returns.
    pub escapes: bool,
}

impl Touch {
    /// Never touched and never kept.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// Written and kept, which is what an unknown callee does to what it is handed.
    #[must_use]
    pub fn everything() -> Self {
        Self { effect: Effect::Writes, escapes: true }
    }

    /// Both of these happened.
    #[must_use]
    pub fn and_then(self, other: Self) -> Self {
        Self { effect: self.effect.and_then(other.effect), escapes: self.escapes || other.escapes }
    }

    /// Both of these are true of the same parameter, so the tighter one is.
    #[must_use]
    pub fn as_well_as(self, other: Self) -> Self {
        Self {
            effect: self.effect.as_well_as(other.effect),
            escapes: self.escapes && other.escapes,
        }
    }
}

/// What a function does to memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    outside: Effect,
    params: Box<[Touch]>,
}

impl Summary {
    /// A function that touches no memory at all, taking this many parameters.
    #[must_use]
    pub fn nothing(arity: usize) -> Self {
        Self { outside: Effect::Nothing, params: vec![Touch::nothing(); arity].into() }
    }

    /// A function nothing is known about, taking this many parameters.
    #[must_use]
    pub fn everything(arity: usize) -> Self {
        Self { outside: Effect::Writes, params: vec![Touch::everything(); arity].into() }
    }

    /// A function that does this to what it was not handed and that to each of its parameters.
    ///
    /// The general one, for a caller that has worked the answer out rather than read it off an
    /// attribute. The common shapes have their own names above and below this.
    #[must_use]
    pub fn doing(outside: Effect, params: &[Touch]) -> Self {
        Self { outside, params: params.into() }
    }

    /// A function that reads whatever it likes and writes nothing, taking this many parameters.
    ///
    /// `readonly`, which is `__attribute__((pure))` written as an effect. The addresses are kept
    /// rather than dropped: a function that does not write cannot have put one anywhere a later
    /// call could find it, but it can hand one back, and returning it is not writing.
    #[must_use]
    pub fn reading(arity: usize) -> Self {
        Self::doing(Effect::Reads, &vec![Touch { effect: Effect::Reads, escapes: true }; arity])
    }

    /// A function that does what it likes to what it was handed and nothing to anything else,
    /// taking this many parameters.
    ///
    /// `argmemonly`. It says where and not what, so everything that can happen to an argument is
    /// taken to have happened to every one of them.
    #[must_use]
    pub fn through_arguments(arity: usize) -> Self {
        Self::doing(Effect::Nothing, &vec![Touch::everything(); arity])
    }

    /// What it does to memory it was not handed: the globals, and anything reached through an
    /// address that did not arrive as an argument.
    #[must_use]
    pub fn outside(&self) -> Effect {
        self.outside
    }

    /// What it does to the object the parameter in this position points at.
    ///
    /// A position past the end is an argument no parameter stands for, which is a variadic call,
    /// and the answer for one of those is that anything may have happened to it.
    #[must_use]
    pub fn param(&self, index: usize) -> Touch {
        self.params.get(index).copied().unwrap_or_else(Touch::everything)
    }

    /// How many parameters it has an answer for.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.params.len()
    }

    /// Whether everything it touches, it reached through an argument.
    ///
    /// This is `__attribute__((access))`'s promise and the IR's `argmemonly`, worked out rather
    /// than declared, and it is what lets [`crate::alias`] ask about the arguments one at a time
    /// instead of giving up.
    #[must_use]
    pub fn only_through_arguments(&self) -> bool {
        self.outside == Effect::Nothing
    }

    /// Whether it writes nothing anywhere, which is `readonly` worked out rather than declared.
    #[must_use]
    pub fn writes_nothing(&self) -> bool {
        !self.outside.writes() && self.params.iter().all(|touch| !touch.effect.writes())
    }

    /// Whether it touches nothing anywhere, which is `readnone`.
    #[must_use]
    pub fn touches_nothing(&self) -> bool {
        self.outside == Effect::Nothing
            && self.params.iter().all(|touch| touch.effect == Effect::Nothing)
    }

    /// Both of these are true of the same function, so the tighter one is.
    #[must_use]
    fn as_well_as(&self, other: &Self) -> Self {
        let arity = self.params.len().max(other.params.len());
        let params = (0..arity).map(|at| self.param(at).as_well_as(other.param(at))).collect();
        Self { outside: self.outside.as_well_as(other.outside), params }
    }

    /// Everything that happens to what is behind this pointer, whatever the pointer is.
    fn touch_everything(&mut self) {
        self.outside = Effect::Writes;
        for touch in &mut self.params {
            *touch = Touch::everything();
        }
    }
}

/// What is known about each function in the module.
///
/// Built once, because a summary belongs to the callee and there is one callee and many call
/// sites. A pass holding one function and no module has [`Summaries::nothing`], which answers
/// `None` to everything and leaves every caller with the conservative answer.
#[derive(Clone, Debug, Default)]
pub struct Summaries {
    known: HashMap<Symbol, Summary>,
}

impl Summaries {
    /// Nothing known about anything.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// What the attributes on each of the module's functions promise, before anything is read.
    ///
    /// The three that say something about memory are the three [`crate::alias`] already reads, and
    /// what they promise is the floor the analysis starts from rather than something it can
    /// contradict. A function with none of them gets no entry, which is not the same as an entry
    /// saying it does everything: the difference is what lets [`summarize`] tell a promise it has
    /// to keep from an absence it may fill in.
    #[must_use]
    pub fn of_module(module: &Module) -> Self {
        let mut summaries = Self::default();
        for id in module.funcs() {
            let func = &module[id];
            let arity = func.signature().params.len();
            if let Some(summary) = from_attributes(func.attrs.set, arity) {
                summaries.known.insert(func.name, summary);
            }
        }
        summaries
    }

    /// What is known about this function, or nothing.
    #[must_use]
    pub fn of(&self, name: Symbol) -> Option<&Summary> {
        self.known.get(&name)
    }

    /// What is known about what this call reaches.
    ///
    /// Only a direct call has an answer. A call through an address, inline assembly and a target
    /// intrinsic are all names this cannot put to a body.
    #[must_use]
    pub fn at(&self, func: &Func, call: Inst) -> Option<&Summary> {
        match Callee::of(func, call)? {
            Callee::Direct(name) => self.of(name),
            Callee::Indirect | Callee::Intrinsic(_) | Callee::Asm => None,
        }
    }

    /// Writes down what the analysis worked out.
    ///
    /// Narrowed against whatever the attributes promised rather than written over it, because a
    /// promise the body does not keep is still a promise the caller was told to rely on, and the
    /// build that reports the function whose attribute was a lie wants both halves.
    pub fn record(&mut self, name: Symbol, summary: Summary) {
        let merged = match self.known.get(&name) {
            Some(said) => said.as_well_as(&summary),
            None => summary,
        };
        self.known.insert(name, merged);
    }

    /// How many functions there is an answer for.
    #[must_use]
    pub fn len(&self) -> usize {
        self.known.len()
    }

    /// Whether nothing is known about anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }
}

/// What the attributes a person wrote already promise, which is where the analysis starts.
///
/// None of the three says anything about whether the address is kept, so every parameter of a
/// declared summary escapes. `const` is the one to watch: it promises the result comes out of the
/// arguments and it does not promise the function did not hand one of them back, and a caller that
/// took the escape bit at face value would go on to treat a local it had passed in as one nothing
/// else can reach. Where there is a body the walk overrules this, because the walk looks.
fn from_attributes(set: AttrSet, arity: usize) -> Option<Summary> {
    if set.contains(AttrSet::READNONE) {
        let params = vec![Touch { effect: Effect::Nothing, escapes: true }; arity];
        return Some(Summary { outside: Effect::Nothing, params: params.into() });
    }
    // `readonly` writes nothing anywhere, so the effect is a read wherever it reaches. The
    // addresses are left alone: a function that does not write cannot have put one anywhere a
    // later call could find it, but it can hand one back, and returning it is not writing.
    if set.contains(AttrSet::READONLY) {
        return Some(Summary::reading(arity));
    }
    // `argmemonly` says where, not what, so everything that can happen to an argument may have.
    if set.contains(AttrSet::ARGMEM_ONLY) {
        return Some(Summary::through_arguments(arity));
    }
    None
}

/// Works out what every function in the module does to memory.
///
/// Callee before caller over the condensation, so a caller is read once its callees have settled,
/// and round a cycle until nothing moves. The answers go into `summaries`, narrowed against
/// whatever the attributes already promised.
pub fn summarize(module: &Module, graph: &CallGraph, summaries: &mut Summaries) {
    let arity = |node: Node| match graph.func(node) {
        Some(id) => module[id].signature().params.len(),
        None => 0,
    };
    let answers = graph.solve(
        |node| Summary::nothing(arity(node)),
        |node, answers| match graph.trusted_body(node) {
            Some(id) => what_the_body_does(&module[id], graph, answers, summaries),
            // A declaration, an ifunc, or a definition this link may replace with another
            // object's. What the attributes promised still holds, and nothing else does.
            None => match summaries.of(graph.name(node)) {
                Some(said) => said.clone(),
                None => Summary::everything(arity(node)),
            },
        },
    );
    for node in graph.nodes() {
        if graph.trusted_body(node).is_none() {
            continue;
        }
        summaries.record(graph.name(node), answers[node.index()].clone());
    }
}

/// What one body does, given what everything it calls does.
fn what_the_body_does(
    func: &Func,
    graph: &CallGraph,
    answers: &[Summary],
    said: &Summaries,
) -> Summary {
    let arity = func.signature().params.len();
    let Some(entry) = func.entry() else { return Summary::everything(arity) };
    // The parameter in position `n` of the signature is the parameter in position `n` of the entry
    // block, and the argument in position `n` of a direct call to it. Every mapping below rests on
    // that, so a function where it does not hold gets no answer rather than a wrong one.
    if func[entry].params.len() != arity {
        return Summary::everything(arity);
    }
    // What each call in this body reaches, worked out before anything else because the escape
    // analysis below reads it. A call that keeps nothing it is handed is a call that did not let
    // this function's own locals out, which is section 34.6's upgrade, and the answers are still
    // moving while this asks, which is why it is these rather than a finished [`Summaries`].
    let mut callees: HashMap<Inst, Summary> = HashMap::new();
    let mut steps = 0;
    for block in func.blocks() {
        for inst in func.insts(block) {
            steps += 1;
            if steps > MAX_STEPS {
                return Summary::everything(arity);
            }
            if let Some(summary) = what_that_call_does(func, inst, graph, answers, said) {
                callees.insert(inst, summary);
            }
        }
    }
    let escapes = Escapes::with(func, |inst, index| {
        callees.get(&inst).is_some_and(|summary| !summary.param(index).escapes)
    });
    let mut summary = Summary::nothing(arity);
    for block in func.blocks() {
        for inst in func.insts(block) {
            // Before the call check, because an `asm goto` is a call by [`Callee::of`] and still
            // hands operands to the blocks it can land in.
            add_block_escapes(func, entry, &mut summary, inst);
            if let Some(callee) = callees.get(&inst) {
                add_call(func, entry, &escapes, &mut summary, inst, callee);
                continue;
            }
            add_access(func, entry, &escapes, &mut summary, inst);
            add_operand_escapes(func, entry, &mut summary, inst);
        }
    }
    summary
}

/// The summary of what this instruction calls, for an instruction that is a call.
fn what_that_call_does(
    func: &Func,
    inst: Inst,
    graph: &CallGraph,
    answers: &[Summary],
    said: &Summaries,
) -> Option<Summary> {
    let callee = Callee::of(func, inst)?;
    // An argument's position is a parameter's position only for the two forms where the operands
    // are the arguments and nothing else. `call_indirect` puts the address it calls first, and it
    // has no name to look up anyway.
    let direct = matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall);
    let Callee::Direct(name) = callee else {
        // Through an address, inline assembly, or an intrinsic whose name is all there is of it.
        return Some(Summary::everything(0));
    };
    if !direct {
        return Some(Summary::everything(0));
    }
    // Mid flight for anything in this component and settled for everything below it, which is
    // what [`CallGraph::solve`] promises and is why nothing may read one of these until the
    // component it is in has stopped moving.
    let walked = graph.node(name).map(|node| answers[node.index()].clone());
    let promised = said.of(name).cloned();
    Some(match (walked, promised) {
        (Some(walked), Some(promised)) => walked.as_well_as(&promised),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => Summary::everything(0),
    })
}

/// What a call does to this function's own memory, which is what it does to each thing it is given.
fn add_call(
    func: &Func,
    entry: Block,
    escapes: &Escapes,
    summary: &mut Summary,
    inst: Inst,
    callee: &Summary,
) {
    summary.outside = summary.outside.and_then(callee.outside());
    let args = &func[func[inst].args];
    for (at, &arg) in args.iter().enumerate() {
        if !func[arg].ty.is_ptr() {
            continue;
        }
        let touch = callee.param(at);
        if touch == Touch::nothing() {
            continue;
        }
        match behind(func, entry, escapes, arg) {
            // The callee's own answer for the address it was given is this function's answer for
            // the parameter that address came from, escape bit and all. This is the one place a
            // parameter does better than [`Escapes`] would: handing it to a call that does not let
            // it out has not let it out.
            Behind::Param(at) => summary.params[at] = summary.params[at].and_then(touch),
            // A local nobody outside this function can reach, so whatever the callee did to it
            // stayed inside this function and none of it is in the summary.
            Behind::Private => {}
            Behind::Outside => summary.outside = summary.outside.and_then(touch.effect),
        }
    }
}

/// What an instruction that is not a call does to memory.
fn add_access(func: &Func, entry: Block, escapes: &Escapes, summary: &mut Summary, inst: Inst) {
    let data = func[inst];
    if !data.opcode.has_effects() || data.opcode.is_terminator() {
        return;
    }
    // Neither the storage nor the addresses these reach are the program's, which is the whole of
    // [`Opcode::touches_only_planes`]. Nothing a summary is read for can be one of them.
    if data.opcode.touches_only_planes() {
        return;
    }
    let args = &func[data.args];
    let mut through = |at: usize, effect: Effect| match behind(func, entry, escapes, args[at]) {
        Behind::Param(at) => {
            summary.params[at].effect = summary.params[at].effect.and_then(effect);
        }
        Behind::Private => {}
        Behind::Outside => summary.outside = summary.outside.and_then(effect),
    };
    match data.opcode {
        // Storage this call made, which no caller has a name for.
        Opcode::Alloca => {}
        Opcode::Load | Opcode::AtomicLoad | Opcode::Prefetch => through(0, Effect::Reads),
        Opcode::Store | Opcode::AtomicStore => through(1, Effect::Writes),
        Opcode::AtomicRmw | Opcode::Cmpxchg | Opcode::Memset => through(0, Effect::Writes),
        Opcode::Memcpy | Opcode::Memmove => {
            through(0, Effect::Writes);
            through(1, Effect::Reads);
        }
        // An opcode with effects that is not named here is one this was not written for, and the
        // answer that cannot be wrong is that it did everything. A whitelist for section 8.6's
        // reason: the next opcode added to the IR should make this pass say less, not miscompile.
        _ => summary.touch_everything(),
    }
}

/// Which parameters this instruction lets the address of out of the function.
///
/// The same walk [`Escapes`] does for a local, over the entry block's parameters instead, and a
/// whitelist for the same reason. A call is not on the list, so a parameter handed to one would
/// escape here, which is why [`add_call`] handles a call on its own and this is not asked about
/// one.
fn add_operand_escapes(func: &Func, entry: Block, summary: &mut Summary, inst: Inst) {
    let data = func[inst];
    for (at, &arg) in func[data.args].iter().enumerate() {
        if keeps_address(data.opcode, at) {
            continue;
        }
        if let Some(at) = param_behind(func, entry, arg) {
            summary.params[at].escapes = true;
        }
    }
}

/// What a branch hands to a block parameter, which is where an address stops being one the walk
/// above can follow back to anything, so the answer for it has to be given up here instead.
fn add_block_escapes(func: &Func, entry: Block, summary: &mut Summary, inst: Inst) {
    for call in func.successors(inst) {
        for &arg in &func[call.args] {
            if let Some(at) = param_behind(func, entry, arg) {
                summary.params[at].escapes = true;
            }
        }
    }
}

/// What the object behind an address is, as far as a summary cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behind {
    /// The object the parameter in this position points at.
    Param(usize),
    /// Storage this function made that nothing outside it can reach.
    Private,
    /// Anything else, which the caller has to be told about as a whole.
    Outside,
}

fn behind(func: &Func, entry: Block, escapes: &Escapes, pointer: Value) -> Behind {
    match origin(func, pointer).0 {
        Origin::Local(local) if !escapes.escaped(local) => Behind::Private,
        Origin::Unknown(value) => match param_of(func, entry, value) {
            Some(at) => Behind::Param(at),
            None => Behind::Outside,
        },
        _ => Behind::Outside,
    }
}

/// Which of this function's parameters an address came from, when it came from one.
fn param_behind(func: &Func, entry: Block, pointer: Value) -> Option<usize> {
    let Origin::Unknown(value) = origin(func, pointer).0 else { return None };
    param_of(func, entry, value)
}

fn param_of(func: &Func, entry: Block, value: Value) -> Option<usize> {
    match func[value].def {
        Def::Param { block, index } if block == entry => Some(index as usize),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Builder, Extra, Flags, InstData, MemInfo, MemOrder, Pic, Restrict, Signature, Type,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{
        AttrSet, CallGraph, Effect, Func, Module, Opcode, Summaries, Summary, Touch, Value,
        summarize,
    };

    /// A four byte access of ordinary memory, which is what every test below uses.
    fn access() -> MemInfo {
        MemInfo {
            size: 4,
            align: 4,
            owns: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        }
    }

    /// The address of a file scope variable, which is memory no caller handed over.
    fn somewhere(build: &mut Builder<'_>, names: &mut Interner) -> Value {
        let name = names.intern("v");
        build.value(
            InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        )
    }

    /// Four bytes of stack.
    fn stack(build: &mut Builder<'_>) -> Value {
        let mem = build.func().add_mem(access());
        build.value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// Reads four bytes from there.
    fn reads(build: &mut Builder<'_>, addr: Value) -> Value {
        build.load(Type::int(32), addr, access(), Flags::NONE)
    }

    /// Writes four zero bytes there.
    fn writes(build: &mut Builder<'_>, addr: Value) {
        let zero = build.iconst(Type::int(32), 0);
        build.store(zero, addr, access(), Flags::NONE);
    }

    /// Calls that name with those pointers and throws away whatever came back.
    fn calls(build: &mut Builder<'_>, names: &mut Interner, name: &str, args: &[Value]) {
        let name = names.intern(name);
        let params = vec![Type::PTR; args.len()];
        let signature = build.func().add_signature(Signature::new().with_params(&params));
        build.call(name, signature, args);
    }

    /// One function's body, given the values its parameters arrived as.
    type Body = fn(&mut Interner, &mut Builder<'_>, &[Value]);

    /// A module with the summaries worked out over it, which is what every test asks.
    struct Worked {
        names: Interner,
        summaries: Summaries,
    }

    impl Worked {
        /// Each function is its name, how many pointer parameters it takes, and its body.
        fn out(bodies: &[(&str, usize, AttrSet, Option<Body>)]) -> Self {
            let mut names = Interner::new();
            let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
            let mut module = Module::new(names.intern("t.c"), &target);
            for &(name, arity, attrs, body) in bodies {
                let params = vec![Type::PTR; arity];
                let mut func = Func::new(names.intern(name), Signature::new().with_params(&params));
                func.attrs.set = attrs;
                if let Some(body) = body {
                    let entry = func.create_block();
                    let args: Vec<Value> =
                        (0..arity).map(|_| func.append_param(entry, Type::PTR)).collect();
                    let mut build = Builder::new(&mut func, entry);
                    body(&mut names, &mut build, &args);
                }
                module.add_func(func);
            }
            let mut summaries = Summaries::of_module(&module);
            summarize(&module, &CallGraph::of(&module, Pic::Executable), &mut summaries);
            Self { names, summaries }
        }

        /// What was worked out about that name.
        fn about(&mut self, name: &str) -> Summary {
            let name = self.names.intern(name);
            self.summaries.of(name).expect("a defined function has a summary").clone()
        }
    }

    /// Does nothing but come back.
    fn nothing(_: &mut Interner, build: &mut Builder<'_>, _: &[Value]) {
        build.ret(&[]);
    }

    #[test]
    fn a_body_that_goes_nowhere_near_memory_says_so() {
        let mut worked = Worked::out(&[("f", 2, AttrSet::NONE, Some(nothing))]);
        let f = worked.about("f");
        assert!(f.touches_nothing());
        assert!(f.writes_nothing());
        assert!(f.only_through_arguments());
        assert_eq!(f.arity(), 2);
        assert_eq!(f.param(0), Touch::nothing());
        assert_eq!(f.param(1), Touch::nothing());
    }

    #[test]
    fn a_load_through_one_parameter_is_a_read_of_that_one() {
        fn body(_: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            let value = reads(build, args[0]);
            build.ret(&[value]);
        }
        let mut worked = Worked::out(&[("f", 2, AttrSet::NONE, Some(body))]);
        let f = worked.about("f");
        assert_eq!(f.param(0).effect, Effect::Reads);
        assert_eq!(f.param(1).effect, Effect::Nothing);
        assert_eq!(f.outside(), Effect::Nothing);
        assert!(f.writes_nothing());
        assert!(f.only_through_arguments());
        assert!(!f.param(0).escapes, "dereferencing an address is not keeping it");
    }

    #[test]
    fn a_store_through_one_parameter_is_a_write_of_that_one() {
        fn body(_: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            writes(build, args[1]);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", 2, AttrSet::NONE, Some(body))]);
        let f = worked.about("f");
        assert_eq!(f.param(0).effect, Effect::Nothing);
        assert_eq!(f.param(1).effect, Effect::Writes);
        assert!(!f.writes_nothing());
        assert!(f.only_through_arguments(), "the only thing it wrote, it was handed");
    }

    #[test]
    fn a_global_is_not_anybody_s_parameter() {
        fn body(names: &mut Interner, build: &mut Builder<'_>, _: &[Value]) {
            let global = somewhere(build, names);
            writes(build, global);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", 1, AttrSet::NONE, Some(body))]);
        let f = worked.about("f");
        assert_eq!(f.outside(), Effect::Writes);
        assert_eq!(f.param(0), Touch::nothing());
        assert!(!f.only_through_arguments());
    }

    #[test]
    fn what_a_function_did_to_its_own_stack_is_nobody_else_s_business() {
        fn body(_: &mut Interner, build: &mut Builder<'_>, _: &[Value]) {
            let local = stack(build);
            writes(build, local);
            let value = reads(build, local);
            build.ret(&[value]);
        }
        let mut worked = Worked::out(&[("f", 1, AttrSet::NONE, Some(body))]);
        assert!(worked.about("f").touches_nothing());
    }

    #[test]
    fn a_copy_writes_the_one_it_writes_and_reads_the_one_it_reads() {
        fn body(_: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            let mem = build.func().add_mem(access());
            let list = build.func().push_values(&[args[0], args[1]]);
            build.inst(
                InstData { args: list, extra: Extra::Mem(mem), ..InstData::new(Opcode::Memcpy) },
                &[],
            );
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", 2, AttrSet::NONE, Some(body))]);
        let f = worked.about("f");
        assert_eq!(f.param(0).effect, Effect::Writes);
        assert_eq!(f.param(1).effect, Effect::Reads);
        assert!(f.only_through_arguments());
        assert!(!f.param(0).escapes);
        assert!(!f.param(1).escapes);
    }

    #[test]
    fn an_opcode_this_was_not_written_for_did_everything() {
        // The whitelist, which is the part of this that has to stay wrong in the safe direction
        // when somebody adds an opcode to the IR and not to the list.
        fn body(_: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            let mem = build.func().add_mem(access());
            let list = build.func().push_values(&[args[0]]);
            build.inst(
                InstData { args: list, extra: Extra::Mem(mem), ..InstData::new(Opcode::VaStart) },
                &[],
            );
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", 1, AttrSet::NONE, Some(body))]);
        let f = worked.about("f");
        assert_eq!(f.outside(), Effect::Writes);
        assert_eq!(f.param(0), Touch::everything());
    }

    #[test]
    fn what_the_callee_does_to_what_it_was_handed_is_what_the_caller_does() {
        fn callee(_: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            writes(build, args[0]);
            build.ret(&[]);
        }
        fn caller(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            calls(build, names, "callee", &[args[1]]);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[
            ("callee", 1, AttrSet::NONE, Some(callee)),
            ("caller", 2, AttrSet::NONE, Some(caller)),
        ]);
        let caller = worked.about("caller");
        // The second one, because that is the one that was passed along, and this is the whole
        // point of the analysis: the call did not write memory, it wrote that one.
        assert_eq!(caller.param(0), Touch::nothing());
        assert_eq!(caller.param(1).effect, Effect::Writes);
        assert_eq!(caller.outside(), Effect::Nothing);
        assert!(caller.only_through_arguments());
    }

    #[test]
    fn a_parameter_handed_to_something_that_does_not_keep_it_has_not_got_out() {
        // The one place the walk does better than [`Escapes`] would on its own, which marks a
        // local as gone the moment it is an argument of anything.
        fn callee(_: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            let value = reads(build, args[0]);
            build.ret(&[value]);
        }
        fn caller(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            calls(build, names, "callee", &[args[0]]);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[
            ("callee", 1, AttrSet::NONE, Some(callee)),
            ("caller", 1, AttrSet::NONE, Some(caller)),
        ]);
        assert!(!worked.about("callee").param(0).escapes);
        assert!(!worked.about("caller").param(0).escapes);
    }

    #[test]
    fn a_parameter_written_down_somewhere_has_got_out() {
        fn body(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            let global = somewhere(build, names);
            build.store(args[0], global, access(), Flags::NONE);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", 1, AttrSet::NONE, Some(body))]);
        let f = worked.about("f");
        assert!(f.param(0).escapes);
        // And what it did to the bytes behind that address is still nothing.
        assert_eq!(f.param(0).effect, Effect::Nothing);
        assert_eq!(f.outside(), Effect::Writes);
    }

    #[test]
    fn a_parameter_a_caller_cannot_be_told_about_travels_up_as_a_write_of_everything() {
        fn callee(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            let global = somewhere(build, names);
            build.store(args[0], global, access(), Flags::NONE);
            build.ret(&[]);
        }
        fn caller(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            calls(build, names, "callee", &[args[0]]);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[
            ("callee", 1, AttrSet::NONE, Some(callee)),
            ("caller", 1, AttrSet::NONE, Some(caller)),
        ]);
        let caller = worked.about("caller");
        assert!(caller.param(0).escapes, "the callee kept it, so the caller let it go");
        assert_eq!(caller.outside(), Effect::Writes);
    }

    #[test]
    fn what_a_callee_did_to_a_local_it_was_only_lent_stays_inside() {
        // Section 34.6's upgrade to the escape analysis, read from the other end. Without it the
        // address of `place` is gone the moment it is an argument, so what the callee wrote
        // through it is a write of memory this function's own callers would have to be told
        // about, and every one of them loses every load across this call.
        fn callee(_: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            writes(build, args[0]);
            build.ret(&[]);
        }
        fn caller(names: &mut Interner, build: &mut Builder<'_>, _: &[Value]) {
            let place = stack(build);
            calls(build, names, "callee", &[place]);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[
            ("callee", 1, AttrSet::NONE, Some(callee)),
            ("caller", 0, AttrSet::NONE, Some(caller)),
        ]);
        assert_eq!(worked.about("callee").param(0).effect, Effect::Writes);
        assert!(worked.about("caller").touches_nothing());
    }

    #[test]
    fn a_local_the_callee_wrote_down_is_one_this_function_lost() {
        // The same shape with the one difference that matters, which is that the callee keeps the
        // address rather than only using it. Everything after the call has to give up on it.
        fn callee(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            let global = somewhere(build, names);
            build.store(args[0], global, access(), Flags::NONE);
            build.ret(&[]);
        }
        fn caller(names: &mut Interner, build: &mut Builder<'_>, _: &[Value]) {
            let place = stack(build);
            calls(build, names, "callee", &[place]);
            writes(build, place);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[
            ("callee", 1, AttrSet::NONE, Some(callee)),
            ("caller", 0, AttrSet::NONE, Some(caller)),
        ]);
        assert!(worked.about("callee").param(0).escapes);
        assert_eq!(worked.about("caller").outside(), Effect::Writes);
    }

    #[test]
    fn a_call_through_an_address_did_everything_to_everything() {
        fn body(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            calls(build, names, "unknown", &[args[0]]);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[
            ("unknown", 1, AttrSet::NONE, None),
            ("f", 1, AttrSet::NONE, Some(body)),
        ]);
        let f = worked.about("f");
        assert_eq!(f.outside(), Effect::Writes);
        assert_eq!(f.param(0), Touch::everything());
    }

    #[test]
    fn two_functions_that_call_each_other_and_touch_nothing_touch_nothing() {
        // The reason the analysis starts optimistic. Reading these bodies once each, starting at
        // the answer that cannot be wrong, would have each of them writing everything because the
        // other one does, and neither would ever come back down.
        fn ping(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            calls(build, names, "pong", &[args[0]]);
            build.ret(&[]);
        }
        fn pong(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            calls(build, names, "ping", &[args[0]]);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[
            ("ping", 1, AttrSet::NONE, Some(ping)),
            ("pong", 1, AttrSet::NONE, Some(pong)),
        ]);
        assert!(worked.about("ping").touches_nothing());
        assert!(worked.about("pong").touches_nothing());
    }

    #[test]
    fn a_write_inside_a_cycle_is_still_found() {
        fn ping(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            calls(build, names, "pong", &[args[0]]);
            build.ret(&[]);
        }
        fn pong(names: &mut Interner, build: &mut Builder<'_>, args: &[Value]) {
            writes(build, args[0]);
            calls(build, names, "ping", &[args[0]]);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[
            ("ping", 1, AttrSet::NONE, Some(ping)),
            ("pong", 1, AttrSet::NONE, Some(pong)),
        ]);
        assert_eq!(worked.about("ping").param(0).effect, Effect::Writes);
        assert_eq!(worked.about("pong").param(0).effect, Effect::Writes);
        assert!(worked.about("ping").only_through_arguments());
    }

    #[test]
    fn a_declaration_is_whatever_it_promised_and_nothing_more() {
        let mut worked = Worked::out(&[
            ("plain", 1, AttrSet::NONE, None),
            ("none", 1, AttrSet::READNONE, None),
            ("only", 1, AttrSet::READONLY, None),
            ("args", 1, AttrSet::ARGMEM_ONLY, None),
        ]);
        let names = worked.names.intern("plain");
        assert!(worked.summaries.of(names).is_none(), "nobody promised anything about it");
        assert!(worked.about("none").touches_nothing());
        let only = worked.about("only");
        assert!(only.writes_nothing());
        assert_eq!(only.outside(), Effect::Reads);
        assert_eq!(only.param(0).effect, Effect::Reads);
        let args = worked.about("args");
        assert!(args.only_through_arguments());
        assert!(!args.writes_nothing());
        assert_eq!(args.param(0), Touch::everything());
    }

    #[test]
    fn no_attribute_promises_the_address_was_not_kept() {
        // The one that has to be got right, because the escape bit is what takes a local out of
        // the escaped set and a `const` function is allowed to hand its argument straight back.
        let mut worked = Worked::out(&[
            ("none", 1, AttrSet::READNONE, None),
            ("only", 1, AttrSet::READONLY, None),
            ("args", 1, AttrSet::ARGMEM_ONLY, None),
        ]);
        for name in ["none", "only", "args"] {
            assert!(worked.about(name).param(0).escapes, "{name} promised no such thing");
        }
    }

    #[test]
    fn a_promise_the_body_does_not_keep_is_still_a_promise() {
        // Somebody wrote the attribute and the callers were told to believe it. What the walk
        // found is recorded next to it rather than over it, so that the build that reports the
        // function whose attribute was a lie has both halves to report.
        fn body(names: &mut Interner, build: &mut Builder<'_>, _: &[Value]) {
            let global = somewhere(build, names);
            writes(build, global);
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", 1, AttrSet::READNONE, Some(body))]);
        assert!(worked.about("f").touches_nothing());
    }

    #[test]
    fn an_entry_block_that_does_not_match_the_signature_gets_no_answer() {
        // Every mapping in the walk rests on position `n` of the signature being position `n` of
        // the entry block and position `n` of the argument list. A function where that does not
        // hold is one this cannot say anything about without risking saying it about the wrong
        // object.
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let params = vec![Type::PTR; 2];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let only = func.append_param(entry, Type::PTR);
        let mut build = Builder::new(&mut func, entry);
        build.ret(&[only]);
        module.add_func(func);

        let mut summaries = Summaries::of_module(&module);
        summarize(&module, &CallGraph::of(&module, Pic::Executable), &mut summaries);
        let f = summaries.of(names.intern("f")).expect("a defined function has a summary");
        assert_eq!(*f, Summary::everything(2));
    }

    #[test]
    fn a_position_no_parameter_stands_for_is_a_position_anything_happened_to() {
        // Which is what a variadic call hands over, and what a call that disagrees with its
        // callee about how many arguments there are hands over.
        let summary = Summary::nothing(1);
        assert_eq!(summary.param(0), Touch::nothing());
        assert_eq!(summary.param(1), Touch::everything());
        assert_eq!(summary.param(9), Touch::everything());
    }

    #[test]
    fn the_two_ways_of_combining_are_the_lattice_they_claim_to_be() {
        let all = [Effect::Nothing, Effect::Reads, Effect::Writes];
        for one in all {
            assert_eq!(one.and_then(one), one, "{one:?} is not idempotent");
            assert_eq!(one.as_well_as(one), one, "{one:?} is not idempotent");
            assert_eq!(one.and_then(Effect::Nothing), one, "nothing happening changes nothing");
            assert_eq!(one.as_well_as(Effect::Writes), one, "writing promises nothing");
            for two in all {
                assert_eq!(one.and_then(two), two.and_then(one), "{one:?} and {two:?} disagree");
                assert_eq!(one.as_well_as(two), two.as_well_as(one), "{one:?} and {two:?}");
                // Whatever the two of them did together covers whatever either of them did.
                let both = one.and_then(two);
                assert!(both.reads() >= one.reads());
                assert!(both.writes() >= one.writes());
            }
        }
        assert_eq!(Effect::Nothing.name(), "nothing");
        assert_eq!(Effect::Reads.name(), "reads");
        assert_eq!(Effect::Writes.name(), "writes");
    }

    #[test]
    fn a_read_is_a_read_and_only_a_write_is_a_write() {
        assert!(!Effect::Nothing.reads());
        assert!(!Effect::Nothing.writes());
        assert!(Effect::Reads.reads());
        assert!(!Effect::Reads.writes());
        assert!(Effect::Writes.reads(), "a written byte is one the call could have looked at");
        assert!(Effect::Writes.writes());
    }

    #[test]
    fn nothing_known_about_anything_is_a_thing_this_can_be() {
        let mut names = Interner::new();
        let summaries = Summaries::nothing();
        assert!(summaries.is_empty());
        assert_eq!(summaries.len(), 0);
        assert!(summaries.of(names.intern("f")).is_none());
    }

    #[test]
    fn only_a_direct_call_has_a_summary_at_the_call_site() {
        let mut worked = Worked::out(&[("callee", 1, AttrSet::READNONE, None)]);
        let func = Worked::caller(&mut worked.names);
        let direct = func.1;
        assert!(worked.summaries.at(&func.0, direct).is_some_and(Summary::touches_nothing));
        assert!(worked.summaries.at(&func.0, func.2).is_none(), "through an address");
        assert!(worked.summaries.at(&func.0, func.3).is_none(), "not a call at all");
    }

    impl Worked {
        /// A function calling `callee` directly, then through an address, then returning.
        fn caller(names: &mut Interner) -> (Func, super::Inst, super::Inst, super::Inst) {
            let mut func = Func::new(names.intern("caller"), Signature::new());
            let block = func.create_block();
            let mut build = Builder::new(&mut func, block);
            let signature = build.func().add_signature(Signature::new());
            let direct = build.call(names.intern("callee"), signature, &[]);
            let varargs = build.func().push_abis(&[]);
            let info =
                build.func().add_call(rucc_ir::CallInfo { callee: None, signature, varargs });
            let indirect = build.inst(
                InstData { extra: Extra::Call(info), ..InstData::new(Opcode::CallIndirect) },
                &[],
            );
            let end = build.ret(&[]);
            (func, direct, indirect, end)
        }
    }
}
