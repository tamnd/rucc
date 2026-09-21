//! Interprocedural constant propagation: a parameter every call passes the same number to.
//!
//! Design: `spec/optimizer/34-ipa.md` section 34.6, which asks for "interprocedural constant
//! propagation without cloning, roughly 500 lines, at `-O2`", with "the transformation restricted
//! to the case GCC also allows at `-O2`: a parameter that is the same constant in every call site
//! becomes a constant in the body, and the argument is dropped. No specialization, no clones."
//!
//! Section 34.4 reads gcc's `gcc/ipa-cp.cc`, 6,933 lines, and says the representation is the idea
//! worth stealing. It is jump functions: for each argument at each call site, what it is in the
//! caller's own terms. Gcc's three forms are at `gcc/ipa-cp.cc:60` and two of them are here, the
//! constant and the pass through. The third, unknown, is what everything else is.
//!
//! Cloning is `-fipa-cp-clone` and gcc turns it on at `-O3` (`gcc/opts.cc:704`). Section 34.6 says
//! it "should be `-O3` in rucc" as well, and that it "needs document 33.3's predicated summaries
//! to decide, which means it arrives with them or after them". Neither is here, so neither is
//! cloning.
//!
//! # The lattice
//!
//! Three values per parameter. Nothing, meaning no call site has said anything about it yet. A
//! number, meaning every call site that has said anything passed that one. And anything, meaning
//! two call sites disagreed or one of them passed something this cannot read. That is a lattice of
//! height three, which is what makes the walk over the condensation terminate.
//!
//! The start is optimistic: every parameter of every function this can see all the callers of
//! starts at nothing. Section 34.5 is specific about the price of that, which is that the answer
//! "is only sound after the fixpoint, so nothing may read the lattice mid-flight". Nothing here
//! reads it until the walk has settled.
//!
//! A parameter still at nothing once it has settled is a parameter of a function nothing calls.
//! Being internal, having no direct call in the unit and having no address anybody took is the
//! whole of what it takes to never be called, so that function is unreachable and is dead code
//! elimination's to remove rather than this pass's to guess a value for. It is left alone.
//!
//! # What a function has to be
//!
//! Internal linkage, an address this unit never hands out, a body this unit may read, a fixed
//! number of parameters, and at least one call to it. The first three together are what make the
//! set of call sites the whole set: an external name can be called from another object, an address
//! that escaped can be called through, and a body that is only declared here has its callers
//! somewhere else. Variadic is out because a position past the named parameters is not a parameter.
//!
//! # Why the walk goes the other way
//!
//! [`CallGraph::components`] gives the condensation callees first, because that is the order an
//! answer computed from a body flows in, and it is the order [`crate::purity`] and [`crate::modref`]
//! want. This one is the other question. What a parameter holds is something the callers say, so
//! the order is callers first, which is that list read backwards. Inside a component the rounds
//! repeat until nothing changes, which is recursion and is the case the optimistic start was for: a
//! function that calls itself with the parameter it was given says nothing about it either way, and
//! a pessimistic start would read that as a disagreement.
//!
//! # Sweeps, and where the arithmetic comes from
//!
//! Section 34.4 says the pass through form matters because it "is what makes the propagation
//! transitive", and that gcc's carries an operation with it: `f` passing `x + 1` to `g`, with `f`
//! always called with 3, means `g` is always called with 4.
//!
//! The pass through here carries no operation. It gets the same answer a different way, which is to
//! run the whole thing again. A parameter that became a number leaves `3 + 1` standing in the body,
//! [`crate::fold`] turns that into `4` where it stands, and the next sweep reads a call site
//! passing a number. Three sweeps, which is what `ipa-cp-sweeps` is at `gcc/params.opt:280`.
//!
//! The reason to do it that way is that the operation gcc's jump function carries is arithmetic,
//! and the arithmetic is already written down once. A second evaluator inside this file would be a
//! second answer to what an add of two constants is, and the first thing to go wrong with one of
//! those is that the two stop agreeing about an overflow under `nsw`. Each sweep settles its own
//! lattice before it changes anything, so each sweep is sound on its own terms, and a sweep only
//! ever replaces a value with one equal to it, so a later sweep can be more precise and cannot
//! contradict an earlier one.
//!
//! # What it does not do
//!
//! The argument is not dropped. Section 34.6 asks for that in the same sentence, and it is the
//! other half: a signature that changes has to have every call site changed with it, which is the
//! ABI care section 34.5 asks for and is the same work as removing a parameter nothing reads. It
//! belongs with that rather than here, and a parameter this leaves a constant in the body of is a
//! parameter nothing reads, so the two compose.
//!
//! Not a pointer. A constant address is a `global_addr` rather than an `iconst`, so reading one is
//! a jump function form of its own, and what it would be worth is mostly what devirtualization
//! would be worth, which section 34.6 puts outside M4.
//!
//! Not an aggregate, and no per field answer. `ipa-max-agg-items` at `gcc/params.opt:304` tracks
//! sixteen fields per parameter and section 34.6's list of what is not built has aggregate
//! parameter splitting in it.
//!
//! Not the return value. A function that returns the same constant from every return is the mirror
//! of this and is worth having, and it wants the call sites rewritten rather than the body, which
//! puts it with the half above.

use std::collections::{HashMap, HashSet};

use rucc_ir::{
    Block, Def, Extra, Func, FuncId, Imm, Inst, InstData, Linkage, Module, Opcode, Type, Value,
};

use crate::uses::substitute;
use crate::{CallGraph, Fuel, Stats, fold};

/// Recorded for each parameter that became a constant.
const KNOWN: &str =
    "parameter is the same constant at every call and is now a constant in the body";

/// Recorded for a parameter that would have become one if there had been fuel for it.
///
/// Not a missed optimization in the ordinary sense, since the fuel is somebody deliberately
/// stopping the pass. It is here because it is the number a bisection is searching for.
const NO_FUEL: &str = "parameter left alone, the pass ran out of fuel";

/// What this is called, which is gcc's spelling of it so that `-fno-ipa-cp` means what it means
/// everywhere else.
pub const NAME: &str = "ipa-cp";

/// How many times the whole thing repeats, which is `ipa-cp-sweeps` at `gcc/params.opt:280`.
///
/// A sweep past the first exists to read the arithmetic the sweep before it enabled, so the number
/// is how many levels of pass through with an operation the propagation reaches. Gcc settled on
/// three and there is no measurement here that argues for another number.
const SWEEPS: usize = 3;

/// Puts a constant into the body of every function whose callers all pass the same one.
///
/// Hands back what it did to each function it changed, one entry per function, for the manager to
/// turn into the remark a `-fopt-info` line comes from. A function that did not change has no
/// entry, because a module at a time transformation that reported on every function in the module
/// would bury the ones it touched.
pub fn propagate(module: &mut Module, graph: &CallGraph, fuel: &mut Fuel) -> Vec<(FuncId, Stats)> {
    let closed = closed(module, graph);
    if closed.is_empty() {
        return Vec::new();
    }
    let order = order(graph, &closed);
    let mut stats: HashMap<FuncId, Stats> = HashMap::new();
    let mut touched: Vec<FuncId> = Vec::new();
    for _ in 0..SWEEPS {
        let sites = sites(module, &closed);
        let known = settle(module, &closed, &order, &sites);
        let changed = rewrite(module, &closed, &sites, &known, fuel, &mut stats);
        if changed.is_empty() {
            break;
        }
        // The arithmetic the constants just enabled, evaluated where it stands so that the next
        // sweep reads a call site passing a number rather than a call site passing an add. Its
        // fuel is not this pass's, because what it is doing is reading a rewrite this pass already
        // paid for, and a bisection that cut here would leave a function half rewritten. What it
        // did is recorded against this pass, since this pass is what caused it.
        for &id in &changed {
            let folded = fold::fold_in(&mut module[id], &mut Fuel::unlimited());
            stats.entry(id).or_default().merge(&folded);
        }
        for id in changed {
            if !touched.contains(&id) {
                touched.push(id);
            }
        }
    }
    // In module order rather than in the order the sweeps found them, so that two builds of the
    // same module report in the same order, per spec 03.
    module.funcs().filter_map(|id| Some((id, stats.remove(&id)?))).collect()
}

/// The functions this unit can see every call to, with the parameters lined up.
///
/// Five things, and the first three are one thing said three ways. Internal linkage means no other
/// object can name it. No address taken means nothing in this one can reach it except by naming it.
/// A body this unit may read is [`CallGraph::trusted_body`], which is section 34.1's gate, and
/// without it there is nothing to put a constant into. Then the signature has to have a fixed
/// number of parameters, and the entry block has to have one value per parameter, which is what
/// makes the position of an argument at a call the position of a parameter in the body.
fn closed(module: &Module, graph: &CallGraph) -> Vec<FuncId> {
    let mut closed = Vec::new();
    for node in graph.nodes() {
        if graph.address_taken(node) {
            continue;
        }
        let Some(id) = graph.trusted_body(node) else { continue };
        let func = &module[id];
        if func.linkage != Linkage::Internal || func.signature().variadic {
            continue;
        }
        let Some(entry) = func.entry() else { continue };
        if func[entry].params.len() != func.signature().params.len() {
            continue;
        }
        closed.push(id);
    }
    // Module order, for the reason the return above gives.
    closed.sort_unstable_by_key(|id| id.raw());
    closed
}

/// The components of the call graph with callers before callees, each holding only closed nodes.
///
/// [`CallGraph::components`] is callees first, so this is that read backwards. A component with no
/// closed function in it is left out rather than walked over, since the round inside it would
/// compute nothing.
fn order(graph: &CallGraph, closed: &[FuncId]) -> Vec<Vec<FuncId>> {
    let inside: HashSet<FuncId> = closed.iter().copied().collect();
    let mut order = Vec::new();
    for part in graph.components().iter().rev() {
        let part: Vec<FuncId> = part
            .iter()
            .filter_map(|&node| graph.trusted_body(node))
            .filter(|id| inside.contains(id))
            .collect();
        if !part.is_empty() {
            order.push(part);
        }
    }
    order
}

/// Every direct call in the module to one of the closed functions, by the function called.
///
/// Every call, not only the ones from closed functions. A call from anywhere is a call, and what
/// the caller is only matters when the argument is the caller's own parameter, which is the one
/// place below that asks.
///
/// A call whose argument count does not match what the callee takes is a prototype disagreeing with
/// a definition, which a translation unit may contain. The positions would not line up, so the call
/// is not read and the callee is struck out instead of being read from the rest of its calls, since
/// what that one passes is exactly what is not known.
fn sites(module: &Module, closed: &[FuncId]) -> HashMap<FuncId, Sites> {
    let mut where_defined: HashMap<_, FuncId> = HashMap::new();
    for &id in closed {
        where_defined.insert(module[id].name, id);
    }
    let mut sites: HashMap<FuncId, Sites> = HashMap::new();
    for id in module.funcs() {
        let func = &module[id];
        if func.is_declaration() {
            continue;
        }
        for block in func.blocks() {
            for inst in func.insts(block) {
                if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
                    continue;
                }
                let Extra::Call(at) = func[inst].extra else { continue };
                let Some(callee) = func[at].callee else { continue };
                let Some(&target) = where_defined.get(&callee) else { continue };
                let entry = sites.entry(target).or_default();
                if module[target].signature().params.len() != func[func[inst].args].len() {
                    entry.ragged = true;
                    continue;
                }
                entry.calls.push((id, inst));
            }
        }
    }
    sites
}

/// Where one function is called from.
#[derive(Debug, Default)]
struct Sites {
    /// The caller and the instruction, for every call whose arguments line up.
    calls: Vec<(FuncId, Inst)>,
    /// Whether some call passed a number of arguments the function does not take.
    ///
    /// One of those and nothing is claimed about any parameter, because the call is real and what
    /// it passed is what cannot be read.
    ragged: bool,
}

/// What the propagation knows about one parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// No call site has said anything about it yet.
    ///
    /// The optimistic start. Once the walk has settled this means there are no call sites, which is
    /// a function nothing calls rather than a parameter that could be anything.
    Nothing,
    /// Every call site that has said anything passed this, with the type it was passed as.
    Number(Imm, Type),
    /// Two call sites disagreed, or one of them passed something this cannot read.
    Anything,
}

impl Held {
    /// What holds at both, which is the meet of the two.
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Nothing, it) | (it, Self::Nothing) => it,
            (Self::Number(a, x), Self::Number(b, y)) if a == b && x == y => Self::Number(a, x),
            _ => Self::Anything,
        }
    }
}

/// What every closed function's parameters hold, once the walk has stopped changing its mind.
///
/// Callers before callees over the condensation, with the rounds inside a component repeating until
/// nothing moves. A component of one function that does not call itself settles in one visit, which
/// is almost every component in a real unit.
///
/// # Panics
///
/// In a checked build, if a component has not settled after far more rounds than a lattice of
/// height three over a component that size could need. That is this file having stopped being
/// monotone rather than anything about the graph.
fn settle(
    module: &Module,
    closed: &[FuncId],
    order: &[Vec<FuncId>],
    sites: &HashMap<FuncId, Sites>,
) -> HashMap<FuncId, Vec<Held>> {
    let mut known: HashMap<FuncId, Vec<Held>> = closed
        .iter()
        .map(|&id| (id, vec![Held::Nothing; module[id].signature().params.len()]))
        .collect();
    for part in order {
        let ceiling = 1000 + part.len() * 64;
        let mut rounds = 0usize;
        loop {
            let mut settled = true;
            for &id in part {
                let now = row(module, sites, &known, id);
                if known.get(&id) != Some(&now) {
                    known.insert(id, now);
                    settled = false;
                }
            }
            if settled {
                break;
            }
            rounds += 1;
            debug_assert!(rounds < ceiling, "the propagation is not monotone");
        }
    }
    known
}

/// What one function's parameters hold, read off its call sites and what is known so far.
fn row(
    module: &Module,
    sites: &HashMap<FuncId, Sites>,
    known: &HashMap<FuncId, Vec<Held>>,
    id: FuncId,
) -> Vec<Held> {
    let count = module[id].signature().params.len();
    let Some(sites) = sites.get(&id) else { return vec![Held::Nothing; count] };
    if sites.ragged {
        return vec![Held::Anything; count];
    }
    let entry = module[id].entry().expect("a closed function has an entry block");
    (0..count)
        .map(|index| {
            let param = module[id][entry].params[index];
            let ty = module[id][param].ty;
            sites.calls.iter().fold(Held::Nothing, |so_far, &(caller, inst)| {
                so_far.and(passed(module, known, caller, inst, index, ty))
            })
        })
        .collect()
}

/// What one call site passes in one position, in terms of what is known about the caller.
///
/// This is the jump function, in the two of gcc's three forms that are here. A constant is read off
/// the argument. A pass through is the caller's own entry block parameter used as it stands, and
/// what it passes is whatever that parameter holds, which is why the caller has to be one of the
/// closed functions for it to be read at all. Everything else is anything.
fn passed(
    module: &Module,
    known: &HashMap<FuncId, Vec<Held>>,
    caller: FuncId,
    inst: Inst,
    index: usize,
    ty: Type,
) -> Held {
    let func = &module[caller];
    let Some(&arg) = func[func[inst].args].get(index) else { return Held::Anything };
    // A prototype that disagrees with the definition about a type rather than about a count. The
    // argument is the bits the call passes and the parameter is the bits the body reads, and two
    // types is two readings of them.
    if func[arg].ty != ty {
        return Held::Anything;
    }
    if let Some((imm, ty)) = number(func, arg) {
        return Held::Number(imm, ty);
    }
    let Def::Param { block, index: at } = func[arg].def else { return Held::Anything };
    if func.entry() != Some(block) {
        return Held::Anything;
    }
    match known.get(&caller).and_then(|row| row.get(at as usize)) {
        Some(&held) => held,
        // A caller whose own parameters are not being tracked, which is a caller anything can
        // reach, so the parameter it is handing on is one nobody counted.
        None => Held::Anything,
    }
}

/// The constant a value is, of either kind, with the type it has.
///
/// Both kinds, because a floating point parameter every call passes the same literal to is the same
/// optimization as an integer one, and an immediate is the bits either way. Two immediates are
/// equal when they are the same bits, which for a float is the reading that keeps a positive zero
/// and a negative zero apart, and that is the reading this wants: they are the same number and they
/// are not the same value, and `copysign` can tell.
fn number(func: &Func, value: Value) -> Option<(Imm, Type)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    if !matches!(data.opcode, Opcode::IConst | Opcode::FConst) {
        return None;
    }
    let Extra::Imm(at) = data.extra else { return None };
    let ty = func[value].ty;
    // A vector constant is a `splat` rather than either of the two above, so nothing here produces
    // one and nothing here would be able to build one back.
    (ty.is_scalar() && (ty.is_int() || ty.is_float())).then(|| (func[at], ty))
}

/// Puts the constants into the bodies, and hands back the functions that changed.
fn rewrite(
    module: &mut Module,
    closed: &[FuncId],
    sites: &HashMap<FuncId, Sites>,
    known: &HashMap<FuncId, Vec<Held>>,
    fuel: &mut Fuel,
    stats: &mut HashMap<FuncId, Stats>,
) -> Vec<FuncId> {
    let mut changed = Vec::new();
    for &id in closed {
        // A function with no call site has every parameter still at nothing, and nothing is not a
        // value to put in a body. See the module documentation: it is unreachable rather than
        // unconstrained, and removing it is dead code elimination's.
        if sites.get(&id).is_none_or(|sites| sites.calls.is_empty()) {
            continue;
        }
        let Some(row) = known.get(&id) else { continue };
        let func = &mut module[id];
        let entry = func.entry().expect("a closed function has an entry block");
        let read = operands(func);
        let mut same: HashMap<Value, Value> = HashMap::new();
        for (index, &held) in row.iter().enumerate() {
            let Held::Number(imm, ty) = held else { continue };
            let param = func[entry].params[index];
            // A parameter nothing in the body reads. Putting a constant in front of it would be an
            // instruction nobody uses and a line in `-fopt-info` saying something happened.
            if !read.contains(&param) || func[param].ty != ty {
                continue;
            }
            if !fuel.take() {
                // Out of fuel, which is a request to stop transforming and not to stop looking, so
                // the walk goes on and the count of what could have gone is the same at every fuel
                // setting. That is what makes a bisection over it monotonic.
                stats.entry(id).or_default().missed(NO_FUEL);
                continue;
            }
            same.insert(param, constant(func, entry, imm, ty));
            stats.entry(id).or_default().optimized(KNOWN);
        }
        if !same.is_empty() {
            substitute(func, &same);
            changed.push(id);
        }
    }
    changed
}

/// Every value the body reads, as an operand or as an argument on an edge.
fn operands(func: &Func) -> HashSet<Value> {
    let mut read = HashSet::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            read.extend(func[func[inst].args].iter().copied());
            for edge in func.successors(inst) {
                read.extend(func[edge.args].iter().copied());
            }
        }
    }
    read
}

/// Writes the constant at the top of the entry block and hands back its value.
///
/// At the top because that is the one place in the function that every reader of the parameter is
/// below, which is the whole of what the substitution needs of it. It takes the source location of
/// whatever it went in front of, so that a debugger asked about it lands on the first line of the
/// function rather than on nothing.
fn constant(func: &mut Func, entry: Block, imm: Imm, ty: Type) -> Value {
    let first = func.insts(entry).next().expect("a block ends in a terminator");
    let span = func.span(first);
    let at = func.add_imm(imm);
    let opcode = if ty.is_int() { Opcode::IConst } else { Opcode::FConst };
    let inst =
        func.create_inst(InstData { extra: Extra::Imm(at), ..InstData::new(opcode) }, &[ty], span);
    func.insert_before(inst, first);
    func[inst].results().next().expect("one result was asked for")
}

#[cfg(test)]
mod tests {
    use rucc_base::{Interner, Symbol};
    use rucc_ir::{Builder, Flags, Float, Pic, Signature};
    use rucc_target::{TargetInfo, Triple};

    use super::*;
    use crate::stats::Kind;

    /// The type every test below uses unless it is about another one.
    const INT: Type = Type::int(32);

    /// A module for a sixty four bit Linux, with somewhere to keep the names.
    struct Unit {
        names: Interner,
        module: Module,
    }

    impl Unit {
        /// An empty one.
        fn new() -> Self {
            let mut names = Interner::new();
            let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
            let module = Module::new(names.intern("t.c"), &target);
            Self { names, module }
        }

        /// A name, which a test needs before the function itself when the body mentions it.
        fn name(&mut self, name: &str) -> Symbol {
            self.names.intern(name)
        }

        /// Puts a function of that linkage under that signature into the module.
        ///
        /// The closure is handed the entry block parameters, in the order the signature named
        /// them, which is the order a call passes its arguments in. A return of nothing is added
        /// after it, so a body is only what the test is about.
        fn add(
            &mut self,
            name: Symbol,
            linkage: Linkage,
            signature: Signature,
            body: impl FnOnce(&mut Builder<'_>, &[Value]),
        ) {
            let params = signature.params.iter().map(|it| it.ty).collect::<Vec<Type>>();
            let mut func = Func::new(name, signature);
            func.linkage = linkage;
            let block = func.create_block();
            let values: Vec<Value> =
                params.into_iter().map(|ty| func.append_param(block, ty)).collect();
            let mut build = Builder::new(&mut func, block);
            body(&mut build, &values);
            build.ret(&[]);
            self.module.add_func(func);
        }

        /// The same, internal, which is what a `static` function in the source is.
        fn private(
            &mut self,
            name: &str,
            params: &[Type],
            body: impl FnOnce(&mut Builder<'_>, &[Value]),
        ) -> Symbol {
            let name = self.name(name);
            self.add(name, Linkage::Internal, Signature::new().with_params(params), body);
            name
        }

        /// A function outside anything the propagation looks at, to put call sites in.
        fn outside(&mut self, body: impl FnOnce(&mut Builder<'_>, &[Value])) {
            let name = self.name("f");
            self.add(name, Linkage::External, Signature::new(), body);
        }

        /// Runs the propagation over it with as much fuel as it asks for.
        fn propagate(&mut self) -> Vec<(FuncId, Stats)> {
            self.run(&mut Fuel::unlimited())
        }

        /// Runs the propagation over it.
        fn run(&mut self, fuel: &mut Fuel) -> Vec<(FuncId, Stats)> {
            let graph = CallGraph::of(&self.module, Pic::Executable);
            propagate(&mut self.module, &graph, fuel)
        }

        /// What the one reader of that function's parameter reads now, if it reads a number.
        ///
        /// Every body below reads its parameter in one place, and that place is the first
        /// instruction of that opcode, so this is its first operand read as a constant. It answers
        /// nothing when the operand is still the parameter, which is the propagation having left
        /// it alone.
        fn reads(&self, at: Symbol, opcode: Opcode) -> Option<i128> {
            let id = self.module.funcs().find(|&id| self.module[id].name == at);
            let func = &self.module[id.expect("the module has a function of that name")];
            let entry = func.entry()?;
            let at = func.insts(entry).find(|&inst| func[inst].opcode == opcode)?;
            let arg = *func[func[at].args].first()?;
            let (imm, ty) = number(func, arg)?;
            Some(if ty.is_int() { imm.signed(ty) } else { imm.bits() as i128 })
        }

        /// What the run said about the function of that name.
        fn said(&self, done: &[(FuncId, Stats)], at: Symbol) -> Stats {
            done.iter()
                .find(|(id, _)| self.module[*id].name == at)
                .map_or_else(Stats::new, |(_, stats)| stats.clone())
        }
    }

    /// A division of a value by itself, which is a body reading its parameter and nothing else.
    ///
    /// A division because the folding at the end of a sweep leaves one alone, so the instruction
    /// is still there afterwards for [`Unit::reads`] to read the operand off.
    fn reads(build: &mut Builder<'_>, value: Value) {
        build.binary(Opcode::SDiv, value, value, Flags::NONE);
    }

    /// Calls that function with those arguments, under a signature matching what it takes.
    fn call(build: &mut Builder<'_>, at: Symbol, params: &[Type], args: &[Value]) {
        let signature = build.func().add_signature(Signature::new().with_params(params));
        build.call(at, signature, args);
    }

    #[test]
    fn a_parameter_every_call_passes_the_same_number_to_becomes_that_number() {
        let mut unit = Unit::new();
        let g = unit.private("g", &[INT], |build, params| reads(build, params[0]));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
            call(build, g, &[INT], &[seven]);
        });
        let done = unit.propagate();
        assert_eq!(unit.said(&done, g).count(Kind::Optimized, KNOWN), 1);
        assert_eq!(unit.reads(g, Opcode::SDiv), Some(7));
    }

    #[test]
    fn a_parameter_two_calls_disagree_about_is_left_alone() {
        let mut unit = Unit::new();
        let g = unit.private("g", &[INT], |build, params| reads(build, params[0]));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            let eight = build.iconst(INT, 8);
            call(build, g, &[INT], &[seven]);
            call(build, g, &[INT], &[eight]);
        });
        assert!(unit.propagate().is_empty());
        assert_eq!(unit.reads(g, Opcode::SDiv), None);
    }

    #[test]
    fn a_parameter_handed_on_by_a_caller_whose_own_is_known_becomes_the_same_number() {
        // The pass through form, which is the one that makes a chain of static helpers worth
        // anything. Without it the propagation stops at the first level.
        let mut unit = Unit::new();
        let g = unit.private("g", &[INT], |build, params| reads(build, params[0]));
        let wrap = unit.private("wrap", &[INT], |build, params| {
            call(build, g, &[INT], &[params[0]]);
        });
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, wrap, &[INT], &[seven]);
        });
        let done = unit.propagate();
        assert_eq!(unit.said(&done, g).count(Kind::Optimized, KNOWN), 1);
        assert_eq!(unit.said(&done, wrap).count(Kind::Optimized, KNOWN), 1);
        assert_eq!(unit.reads(g, Opcode::SDiv), Some(7));
    }

    #[test]
    fn a_number_the_caller_worked_out_arrives_on_the_sweep_after_the_one_that_made_it() {
        // Section 34.4's transitive case, which gcc gets from an operation carried on the jump
        // function and this gets from running more than once. `wrap` is called with 7, so the add
        // in it becomes an add of two constants, so the folding at the end of the first sweep
        // makes it an 8, so the second sweep reads a call site passing 8.
        let mut unit = Unit::new();
        let g = unit.private("g", &[INT], |build, params| reads(build, params[0]));
        let wrap = unit.private("wrap", &[INT], |build, params| {
            let one = build.iconst(INT, 1);
            let sum = build.binary(Opcode::Add, params[0], one, Flags::NONE);
            call(build, g, &[INT], &[sum]);
        });
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, wrap, &[INT], &[seven]);
        });
        let done = unit.propagate();
        assert_eq!(unit.said(&done, wrap).count(Kind::Optimized, KNOWN), 1);
        assert_eq!(unit.said(&done, g).count(Kind::Optimized, KNOWN), 1);
        assert_eq!(unit.reads(g, Opcode::SDiv), Some(8));
    }

    #[test]
    fn a_recursive_call_handing_on_the_parameter_it_was_given_says_nothing_either_way() {
        // The case the optimistic start is for. `g` calls itself with the parameter it was handed,
        // so that call site is whatever the parameter already is. Starting it at anything would
        // read the recursion as a disagreement and lose the 7 the real call site passes.
        let mut unit = Unit::new();
        let g = unit.name("g");
        unit.add(g, Linkage::Internal, Signature::new().with_params(&[INT]), |build, params| {
            reads(build, params[0]);
            call(build, g, &[INT], &[params[0]]);
        });
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        let done = unit.propagate();
        assert_eq!(unit.said(&done, g).count(Kind::Optimized, KNOWN), 1);
        assert_eq!(unit.reads(g, Opcode::SDiv), Some(7));
    }

    #[test]
    fn a_function_another_object_can_call_is_left_alone() {
        // External linkage, so the call in this unit is one of its call sites and not all of them,
        // and what the others pass is not something this can see.
        let mut unit = Unit::new();
        let g = unit.name("g");
        unit.add(g, Linkage::External, Signature::new().with_params(&[INT]), |build, params| {
            reads(build, params[0]);
        });
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        assert!(unit.propagate().is_empty());
        assert_eq!(unit.reads(g, Opcode::SDiv), None);
    }

    #[test]
    fn a_function_whose_address_this_unit_hands_out_is_left_alone() {
        // The address could be called through, and a call through an address passes arguments
        // nobody counted.
        let mut unit = Unit::new();
        let g = unit.private("g", &[INT], |build, params| reads(build, params[0]));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
            let extra = Extra::Symbol(g);
            build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        });
        assert!(unit.propagate().is_empty());
        assert_eq!(unit.reads(g, Opcode::SDiv), None);
    }

    #[test]
    fn a_function_nothing_in_the_unit_calls_is_left_alone() {
        // Its parameter is still at nothing when the walk settles, and nothing is not a number.
        // The function is unreachable, which is dead code elimination's to act on and not this.
        let mut unit = Unit::new();
        let g = unit.private("g", &[INT], |build, params| reads(build, params[0]));
        assert!(unit.propagate().is_empty());
        assert_eq!(unit.reads(g, Opcode::SDiv), None);
    }

    #[test]
    fn a_variadic_function_is_left_alone() {
        // A position past the named parameters is not a parameter, so the positions a call passes
        // and the positions the body reads are not the same list.
        let mut unit = Unit::new();
        let g = unit.name("g");
        let mut signature = Signature::new().with_params(&[INT]);
        signature.variadic = true;
        unit.add(g, Linkage::Internal, signature, |build, params| reads(build, params[0]));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        assert!(unit.propagate().is_empty());
        assert_eq!(unit.reads(g, Opcode::SDiv), None);
    }

    #[test]
    fn a_call_passing_the_wrong_number_of_arguments_stops_the_parameter() {
        // A prototype disagreeing with the definition, which a translation unit may contain. The
        // call is real and the positions do not line up, so what it passes is exactly what cannot
        // be read, and the other call is not enough on its own.
        let mut unit = Unit::new();
        let g = unit.private("g", &[INT], |build, params| reads(build, params[0]));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
            call(build, g, &[INT, INT], &[seven, seven]);
        });
        assert!(unit.propagate().is_empty());
        assert_eq!(unit.reads(g, Opcode::SDiv), None);
    }

    #[test]
    fn a_floating_point_parameter_becomes_a_constant_too() {
        // The bits, which is the reading that keeps a positive zero and a negative zero apart.
        let mut unit = Unit::new();
        let half: u64 = 0x3fe0_0000_0000_0000;
        let g = unit.private("g", &[Type::float(Float::F64)], |build, params| {
            build.binary(Opcode::FAdd, params[0], params[0], Flags::NONE);
        });
        unit.outside(|build, _| {
            let it = build.fconst(Type::float(Float::F64), u128::from(half));
            call(build, g, &[Type::float(Float::F64)], &[it]);
            call(build, g, &[Type::float(Float::F64)], &[it]);
        });
        let done = unit.propagate();
        assert_eq!(unit.said(&done, g).count(Kind::Optimized, KNOWN), 1);
        assert_eq!(unit.reads(g, Opcode::FAdd), Some(i128::from(half)));
    }

    #[test]
    fn a_parameter_nothing_in_the_body_reads_is_not_given_a_constant() {
        // Nothing would read the constant either, so what it would leave behind is an instruction
        // with no users and a line in `-fopt-info` saying something had happened.
        let mut unit = Unit::new();
        let g = unit.private("g", &[INT], |_, _| ());
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        assert!(unit.propagate().is_empty());
    }

    #[test]
    fn without_fuel_the_parameter_stays_and_the_chance_is_still_counted() {
        let mut unit = Unit::new();
        let g = unit.private("g", &[INT], |build, params| reads(build, params[0]));
        unit.outside(|build, _| {
            let seven = build.iconst(INT, 7);
            call(build, g, &[INT], &[seven]);
        });
        let done = unit.run(&mut Fuel::of(0));
        assert_eq!(unit.said(&done, g).count(Kind::Missed, NO_FUEL), 1);
        assert_eq!(unit.said(&done, g).count(Kind::Optimized, KNOWN), 0);
        assert_eq!(unit.reads(g, Opcode::SDiv), None);
    }
}
