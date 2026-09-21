//! What a call is allowed to do, which is the question every pass asks before it moves one.
//!
//! Design: section 41.3 of `spec/optimizer/41-correctness.md`. Documents 08, 17, 20 and 34 all
//! depend on classifying calls and all of them left the completeness argument to that section.
//!
//! # Not a boolean, and not a lattice anybody may extend casually
//!
//! GCC's version is the nineteen `ECF_` bits at `gcc/tree-core.h:46`. The ones that decide
//! anything are `ECF_CONST`, which is a result that depends only on the arguments, and `ECF_PURE`,
//! which reads memory but does not write it, and then `ECF_LOOPING_CONST_OR_PURE`, which is the
//! one worth noticing: a function's result can depend only on its arguments while the function
//! still fails to return, and deleting a call to one of those is not the same decision. GCC keeps
//! the two properties apart and so does [`Purity`].
//!
//! # Opaque is the default and it is the most conservative answer
//!
//! There is no `Unknown` here. Where nothing is known the answer is [`Purity::Opaque`], which
//! permits everything, so a classifier that has not been taught about something produces a missed
//! optimization rather than a wrong program. The library table below can only ever strengthen an
//! answer, which means a name missing from it costs nothing and a wrong entry in it is a
//! miscompilation. That is the bar for adding one.
//!
//! # Exhaustive over what is being called
//!
//! [`Facts::purity_of`] matches on [`Callee`] with no wildcard arm. Adding a new kind of callee to
//! the IR is then a compile error here until somebody says what it can do, which is the one thing
//! Rust offers a compiler over C++ in this file and is not worth giving away to save four lines.
//!
//! # What the user wrote and what the compiler worked out are separate
//!
//! A person writing `__attribute__((const))` on a function that is not const is asserting
//! something, and the compiler honours the assertion. [`infer`] works out its own answer for the
//! functions it can see, and that answer lives in a different field of [`Facts`], because keeping
//! them apart is what makes it possible to check one against the other later. The two are combined
//! with [`Purity::stronger`] at the point of use and nowhere else.
//!
//! # Working it out, which is section 34.2
//!
//! [`infer`] is the analysis. It reads bodies over the call graph, so a function whose body says
//! nothing can still come out `const` because everything it calls is. Section 34.5 says how the
//! cycles go: start every function at [`Purity::Const`], lower it when the body contradicts that,
//! and do not read the answer for anything in a component until the component has stopped moving.
//! Starting at the other end and raising would answer `opaque` for a pair of functions that call
//! each other and do nothing else, which is the case the optimism is for.
//!
//! What a body is allowed to do is a whitelist, for the reason [`crate::alias::keeps_address`] is
//! one: an opcode nobody has thought about here is [`Purity::Opaque`], so adding one to the IR
//! costs a missed optimization rather than a wrong program. Two of the entries in it are worth
//! naming. A load or a store of a local this function never let out of its hands is not an access
//! of anything the caller can see, which is what lets an ordinary function with a temporary in it
//! come out `const` on a compiler that has no SROA yet. And a cycle in the control flow graph is
//! the looping half of 34.2: a function with one is `const, may not return` rather than `const`,
//! because deleting a call to it would change whether the program terminates. Proving a particular
//! loop finite is [`crate::scev`]'s and nothing here asks it yet.

use std::collections::{HashMap, HashSet};

use rucc_base::{Interner, Symbol};
use rucc_ir::{AttrSet, Extra, Flags, Func, Inst, InstData, MemOrder, Module, Opcode, Value};

use crate::alias::{Escapes, Origin, origin};
use crate::callgraph::{CallGraph, Node};
use crate::cfg::Cfg;

/// What a call can do.
///
/// Five, because there are two questions with two answers each and then everything else. Does the
/// result depend on memory, does the call come back, and if either answer is not known then the
/// call is [`Purity::Opaque`] and no pass may assume anything at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purity {
    /// Reads no memory, writes none, and comes back. `__attribute__((const))`, GCC's `ECF_CONST`.
    Const,
    /// Reads no memory and writes none, and may not come back. GCC's `ECF_CONST` together with
    /// `ECF_LOOPING_CONST_OR_PURE`.
    LoopingConst,
    /// Reads memory, writes none, and comes back. `__attribute__((pure))`, GCC's `ECF_PURE`.
    Pure,
    /// Reads memory, writes none, and may not come back.
    LoopingPure,
    /// Anything, which is what a call is until something says otherwise.
    Opaque,
}

impl Purity {
    /// The five, for a test that walks them.
    pub const ALL: [Self; 5] =
        [Self::Const, Self::LoopingConst, Self::Pure, Self::LoopingPure, Self::Opaque];

    /// How it reads in a dump.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Const => "const",
            Self::LoopingConst => "const, may not return",
            Self::Pure => "pure",
            Self::LoopingPure => "pure, may not return",
            Self::Opaque => "opaque",
        }
    }

    /// Whether the call may read memory the caller cares about.
    #[must_use]
    pub const fn reads_memory(self) -> bool {
        match self {
            Self::Const | Self::LoopingConst => false,
            Self::Pure | Self::LoopingPure | Self::Opaque => true,
        }
    }

    /// Whether the call may write memory.
    ///
    /// Only an opaque call may. That is what the other four have in common and it is most of what
    /// makes them worth telling apart from the rest.
    #[must_use]
    pub const fn writes_memory(self) -> bool {
        matches!(self, Self::Opaque)
    }

    /// Whether control is known to come back from the call.
    ///
    /// Not known and known not to are the same answer here, because both of them stop the same
    /// transformations. Which of the two it is belongs to `noreturn`, which is an attribute on the
    /// function rather than a level of this.
    #[must_use]
    pub const fn terminates(self) -> bool {
        matches!(self, Self::Const | Self::Pure)
    }

    /// Whether the result is a function of the arguments and nothing else.
    ///
    /// This is what lets two calls with the same arguments become one call with no question asked
    /// about what happened to memory in between. A [`Purity::Pure`] call can be folded the same way
    /// when the caller can show nothing wrote memory between the two, which is a question for the
    /// alias analysis and not for this.
    #[must_use]
    pub const fn depends_only_on_arguments(self) -> bool {
        !self.reads_memory() && !self.writes_memory()
    }

    /// Whether a call whose result nothing reads may be removed.
    ///
    /// Both halves are needed. A call that writes memory does something even when its result is
    /// thrown away, and a call that may not come back does something by not coming back, which is
    /// why the looping levels exist at all.
    #[must_use]
    pub const fn can_be_deleted_when_unused(self) -> bool {
        !self.writes_memory() && self.terminates()
    }

    /// The strongest thing true of both, for a caller that has two sources and believes each.
    ///
    /// [`Purity::Opaque`] is nothing known, so it gives way to whatever the other source says. Two
    /// sources that each know half give the whole: a declaration saying the result comes out of the
    /// arguments and an analysis saying the loop inside terminates add up to [`Purity::Const`].
    #[must_use]
    pub const fn stronger(self, other: Self) -> Self {
        match (self, other) {
            (Self::Opaque, it) | (it, Self::Opaque) => it,
            (one, two) => Self::of(
                one.reads_memory() && two.reads_memory(),
                one.terminates() || two.terminates(),
            ),
        }
    }

    /// The strongest thing true of either, for a caller that has to cover both.
    ///
    /// Which is what a call site with more than one possible callee needs, and what a caller
    /// summarising a whole function needs.
    #[must_use]
    pub const fn weaker(self, other: Self) -> Self {
        match (self, other) {
            (Self::Opaque, _) | (_, Self::Opaque) => Self::Opaque,
            (one, two) => Self::of(
                one.reads_memory() || two.reads_memory(),
                one.terminates() && two.terminates(),
            ),
        }
    }

    /// The level with those two answers, which is the four that are not opaque.
    const fn of(reads: bool, terminates: bool) -> Self {
        match (reads, terminates) {
            (false, true) => Self::Const,
            (false, false) => Self::LoopingConst,
            (true, true) => Self::Pure,
            (true, false) => Self::LoopingPure,
        }
    }
}

impl std::fmt::Display for Purity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What is being called.
///
/// The thing [`Facts::purity_of`] is exhaustive over. The closed intrinsics are not here because
/// they are opcodes rather than calls and each carries its own meaning in the opcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Callee {
    /// A named function, which may or may not be one this module defines.
    Direct(Symbol),
    /// A call through an address.
    Indirect,
    /// A target-specific intrinsic, named on the instruction, which is the open half of the
    /// intrinsic set and is where the vector builtins land.
    Intrinsic(Symbol),
    /// Inline assembly, including `asm goto`.
    Asm,
}

impl Callee {
    /// What this instruction calls, and `None` for an instruction that calls nothing.
    #[must_use]
    pub fn of(func: &Func, inst: Inst) -> Option<Self> {
        let data = &func[inst];
        match data.opcode {
            Opcode::Call | Opcode::TailCall | Opcode::CallIndirect => match data.extra {
                Extra::Call(at) => Some(match func[at].callee {
                    Some(name) => Self::Direct(name),
                    None => Self::Indirect,
                }),
                _ => Some(Self::Indirect),
            },
            Opcode::TargetIntrinsic => match data.extra {
                Extra::Symbol(name) => Some(Self::Intrinsic(name)),
                _ => Some(Self::Asm),
            },
            Opcode::InlineAsm => Some(Self::Asm),
            _ => None,
        }
    }
}

/// What is known about the functions a call could reach.
///
/// Built once from the module, because the attributes belong to the callee and there is one callee
/// and many call sites. A caller with no module has [`Facts::nothing`], which answers
/// [`Purity::Opaque`] to everything and is correct.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    declared: HashMap<Symbol, AttrSet>,
    inferred: HashMap<Symbol, Purity>,
    from_the_library: HashMap<Symbol, Purity>,
}

impl Facts {
    /// Nothing known about anything, which is what a pass holding one function has.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// What the module says about each of its functions.
    ///
    /// The interner is here for the library table, which is written in text because that is what
    /// the C standard names. Nothing after this call needs it.
    #[must_use]
    pub fn of_module(module: &Module, names: &Interner) -> Self {
        let mut facts = Self::default();
        let mut defined = HashSet::new();
        for id in module.funcs() {
            let func = &module[id];
            facts.declared.insert(func.name, func.attrs.set);
            if !func.is_declaration() {
                defined.insert(func.name);
            }
        }
        // A name this module defines is not the library's, whatever it is spelled, because the
        // definition in hand is the function that will be called.
        for &name in facts.declared.keys() {
            if defined.contains(&name) {
                continue;
            }
            if let Some(purity) = library_purity(names.resolve(name)) {
                facts.from_the_library.insert(name, purity);
            }
        }
        facts
    }

    /// Turns off the whole library table, which is `-fno-builtin` and `-ffreestanding`.
    ///
    /// A freestanding program has no C library for the name to be the name of, and a program that
    /// means its own thing by `strlen` is the reason the flag exists.
    pub fn without_the_library(&mut self) {
        self.from_the_library.clear();
    }

    /// Takes one name away from the table, which is `-fno-builtin-<name>`.
    ///
    /// What a build that means its own `memcpy` and the library's everything else writes, which is
    /// what the kernel does for a handful of names.
    pub fn not_the_library_name(&mut self, name: Symbol) {
        self.from_the_library.remove(&name);
    }

    /// Records what document 34's analysis worked out about a function.
    ///
    /// A separate field from the declaration on purpose. The two are combined where they are read
    /// and are never written over each other, so that a later build can check one against the other
    /// and report the function whose attribute was a lie.
    pub fn record_inferred(&mut self, name: Symbol, purity: Purity) {
        self.inferred.insert(name, purity);
    }

    /// What the user declared about this function, on its own.
    #[must_use]
    pub fn declared(&self, name: Symbol) -> Purity {
        match self.declared.get(&name) {
            Some(&set) => from_attributes(set),
            None => Purity::Opaque,
        }
    }

    /// What analysis worked out about this function, on its own.
    #[must_use]
    pub fn inferred(&self, name: Symbol) -> Purity {
        self.inferred.get(&name).copied().unwrap_or(Purity::Opaque)
    }

    /// What this call can do.
    ///
    /// The match has no wildcard arm and adding a kind of callee should keep it that way.
    #[must_use]
    pub fn purity_of(&self, callee: Callee) -> Purity {
        match callee {
            Callee::Direct(name) => self.of_name(name),
            // The address could be anything with its own definition, including a function this
            // module never saw. Document 34's call graph narrows this and until then it does not.
            Callee::Indirect => Purity::Opaque,
            // The open half of the intrinsic set is named rather than enumerated, so nothing here
            // knows what one does. The closed half are opcodes and never reach this.
            Callee::Intrinsic(_) => Purity::Opaque,
            // A template the compiler does not read, with a clobber list it has to believe.
            Callee::Asm => Purity::Opaque,
        }
    }

    /// Everything known about a named function, from all three sources.
    fn of_name(&self, name: Symbol) -> Purity {
        self.what_was_said_about(name).stronger(self.inferred(name))
    }

    /// What the declaration and the library say, leaving out what an analysis worked out.
    ///
    /// Which is the question [`infer`] has to ask about a callee, because it is in the middle of
    /// working the third source out and a half finished answer is not one anything may read.
    /// Section 34.5 of `spec/optimizer/34-ipa.md`: the result is only sound after the fixpoint.
    fn what_was_said_about(&self, name: Symbol) -> Purity {
        match self.from_the_library.get(&name) {
            Some(&known) => self.declared(name).stronger(known),
            None => self.declared(name),
        }
    }
}

/// Works out what each function in this module does, and writes it into the facts.
///
/// Section 34.2, which is `gcc/ipa-pure-const.cc` at 2,415 lines. The header of that file says
/// when it may run and the same applies here: "This must be run after inlining decisions have been
/// made since otherwise, the local sets will not contain information that is consistent with post
/// inlined state." There is no inliner yet, so today it runs after nothing, and when there is one
/// this has to move below it.
///
/// The graph is the caller's because it is the module's rather than this analysis's, and the three
/// analyses section 34.6 lists after this one want the same graph. Building it here would mean
/// building it four times.
///
/// Only an answer that is not [`Purity::Opaque`] is recorded. Opaque is what [`Facts::inferred`]
/// says about a name it has never heard of, so writing it down would put four thousand entries in
/// a map to say the thing an empty map already says.
pub fn infer(module: &Module, graph: &CallGraph, facts: &mut Facts) {
    // Optimistic, per section 34.5: "Starting optimistic (`const`) and lowering on contradiction
    // gives the right answer". It is only the cycles that notice. Outside one, every callee of a
    // node has settled before the node is asked, so what the starting value was never came up.
    let answers = graph.solve(
        |_| Purity::Const,
        |node, answers| match graph.trusted_body(node) {
            // The flag is the cheap half of the same answer the walk below would reach. A body
            // that calls through an address, or contains inline assembly, or contains a target
            // intrinsic nobody has written down the meaning of, is opaque either way, and this
            // saves walking it to find that out.
            Some(id) if !graph.reaches_unknown(node) => {
                let purity = what_the_body_does(&module[id], graph, answers, facts);
                // Recursion is the other way a function fails to come back, and it is not a cycle
                // in any one body's control flow graph, so the walk below cannot see it. A
                // function that calls itself, directly or round a ring of other functions, has no
                // more of a promise to terminate than one with a loop in it.
                match in_a_cycle(graph, node) {
                    true => purity.weaker(Purity::LoopingConst),
                    false => purity,
                }
            }
            // Either there is no body, or there is one this link may replace with another
            // object's, which is section 34.1's gate and is the graph's to answer.
            _ => Purity::Opaque,
        },
    );
    for node in graph.nodes() {
        let purity = answers[node.index()];
        if purity != Purity::Opaque {
            facts.record_inferred(graph.name(node), purity);
        }
    }
}

/// What one body adds up to, given what everything it calls came out as.
///
/// [`Purity::Const`] is the identity of [`Purity::weaker`], so an instruction that says nothing
/// answers with it and the combining needs no special case for the instructions that are most of
/// a function.
fn what_the_body_does(func: &Func, graph: &CallGraph, answers: &[Purity], facts: &Facts) -> Purity {
    let mut so_far = Purity::Const;
    // Built when the first access wants it and not before, because most of what this walks over
    // is a function that reaches its first opaque instruction and stops.
    let mut escapes: Option<Escapes> = None;
    for block in func.blocks() {
        for inst in func.insts(block) {
            let data = func[inst];
            so_far = so_far.weaker(if let Some(callee) = Callee::of(func, inst) {
                what_that_call_does(callee, graph, answers, facts)
            } else if !data.opcode.has_effects() || data.opcode.is_terminator() {
                // Arithmetic, and the branches and returns that hold the function together.
                // `asm goto` is a terminator and does not reach here, because it is a callee.
                Purity::Const
            } else {
                match data.opcode {
                    // Moving the stack pointer. The caller cannot name the storage and it is
                    // gone by the time control is back with them.
                    Opcode::Alloca => Purity::Const,
                    Opcode::Load if plain(func, data) => {
                        let escapes = escapes.get_or_insert_with(|| Escapes::of(func));
                        match ours(func, escapes, func[data.args][0]) {
                            true => Purity::Const,
                            false => Purity::Pure,
                        }
                    }
                    Opcode::Store if plain(func, data) => {
                        let escapes = escapes.get_or_insert_with(|| Escapes::of(func));
                        match ours(func, escapes, func[data.args][1]) {
                            true => Purity::Const,
                            false => Purity::Opaque,
                        }
                    }
                    // Everything else, which is where a `volatile` access, an atomic one of
                    // either kind, a `memcpy`, a `va_arg` and every opcode added after this was
                    // written all land. A whitelist, for the reason
                    // [`crate::alias::keeps_address`] is one.
                    _ => Purity::Opaque,
                }
            });
            if so_far == Purity::Opaque {
                return Purity::Opaque;
            }
        }
    }
    // Last, because it costs a graph and most functions have already answered by here.
    match has_a_cycle(func) {
        true => so_far.weaker(Purity::LoopingConst),
        false => so_far,
    }
}

/// What a call site adds up to.
///
/// The declaration is combined with the worked out answer rather than replacing it, which is the
/// case a person writing `__attribute__((const))` on a function that loops is asking for: the
/// assertion is honoured, and the half of the answer the assertion said nothing about is the half
/// the body was read for.
fn what_that_call_does(
    callee: Callee,
    graph: &CallGraph,
    answers: &[Purity],
    facts: &Facts,
) -> Purity {
    let Callee::Direct(name) = callee else {
        // An address, inline assembly, or an intrinsic named rather than enumerated. The same
        // three [`Facts::purity_of`] gives up on, and for the same reasons.
        return Purity::Opaque;
    };
    let said = facts.what_was_said_about(name);
    match graph.node(name) {
        Some(node) => said.stronger(answers[node.index()]),
        // Every name a body calls has a node, so this is unreachable with a graph built from the
        // module being read. A caller that hands over somebody else's graph gets the conservative
        // answer rather than a panic.
        None => said,
    }
}

/// Whether this access is one that nothing outside the program's own data flow can tell happened.
///
/// A `volatile` access is one the program asked for by name. An atomic access is part of an order
/// other threads can see, at every strength, which is the same line [`crate::dce`] draws and for
/// the same reason.
fn plain(func: &Func, data: InstData) -> bool {
    if data.flags.contains(Flags::VOLATILE) {
        return false;
    }
    match data.extra {
        Extra::Mem(mem) => func[mem].order == MemOrder::NotAtomic,
        _ => false,
    }
}

/// Whether that address is in a local this function never let out of its hands.
///
/// Storage like that is not memory as far as the caller is concerned. Nothing outside the function
/// had the address to read it with before the function started and nothing has it afterwards, so
/// what the function put there is not a write anybody can see and what it took back out is not a
/// read of anything that could have changed.
fn ours(func: &Func, escapes: &Escapes, pointer: Value) -> bool {
    matches!(origin(func, pointer).0, Origin::Local(local) if !escapes.escaped(local))
}

/// Whether this function can end up calling itself.
///
/// Which is a component with more than one function in it, or a component of one that has an edge
/// to itself. The condensation has already worked this out and the second case is the one it does
/// not record, because a single function is a component whether or not it calls itself.
fn in_a_cycle(graph: &CallGraph, node: Node) -> bool {
    graph.components()[graph.component_of(node)].len() > 1 || graph.calls(node).contains(&node)
}

/// Whether control can arrive at a block it has already been at.
///
/// A reverse postorder is a topological order exactly when the graph is acyclic, so an edge that
/// arrives at a block ranked no later than the one it leaves is an edge that closes a cycle, and a
/// function with no such edge has none. That is cheaper than the loop forest and is all this
/// wants: one cycle anywhere is enough to stop the function promising to come back, whether or not
/// it is a loop in the sense [`crate::loops`] means.
///
/// A block the entry cannot reach has no rank and is skipped. Control never arrives there, so a
/// cycle among such blocks is a cycle the program cannot go round.
fn has_a_cycle(func: &Func) -> bool {
    let cfg = Cfg::new(func);
    func.blocks().any(|block| {
        let Some(from) = cfg.rank(block) else { return false };
        cfg.successors(block).iter().any(|&to| cfg.rank(to).is_some_and(|to| to <= from))
    })
}

/// What an attribute set says on its own.
///
/// `noreturn` is what turns either level into its looping one. A call that does not come back does
/// something by not coming back, however little it touches, and that is the case
/// `ECF_LOOPING_CONST_OR_PURE` exists for.
fn from_attributes(set: AttrSet) -> Purity {
    let terminates = !set.contains(AttrSet::NORETURN);
    if set.contains(AttrSet::READNONE) {
        return Purity::of(false, terminates);
    }
    if set.contains(AttrSet::READONLY) {
        return Purity::of(true, terminates);
    }
    Purity::Opaque
}

/// What the C standard library functions do, for the ones where the answer is not arguable.
///
/// Only entries that strengthen the answer are here, so a name that is missing costs a missed
/// optimization and a name that is wrong costs a wrong program. Nothing that writes memory, sets
/// `errno`, touches a stream or allocates belongs in here, which rules out most of the library and
/// all of `<math.h>`, since a math function sets `errno` unless the command line says it does not.
///
/// Sorted, and a test checks that it is sorted and says each name once.
const LIBRARY: &[(&str, Purity)] = &[
    ("abs", Purity::Const),
    ("imaxabs", Purity::Const),
    ("labs", Purity::Const),
    ("llabs", Purity::Const),
    ("memchr", Purity::Pure),
    ("memcmp", Purity::Pure),
    ("strchr", Purity::Pure),
    ("strcmp", Purity::Pure),
    ("strcspn", Purity::Pure),
    ("strlen", Purity::Pure),
    ("strncmp", Purity::Pure),
    ("strnlen", Purity::Pure),
    ("strpbrk", Purity::Pure),
    ("strrchr", Purity::Pure),
    ("strspn", Purity::Pure),
    ("strstr", Purity::Pure),
];

/// What the library says about a name, under either spelling.
///
/// The `__builtin_` prefix is the program saying which function it means, so it reaches the same
/// entry. Whether the plain spelling is allowed to is decided before this is called.
fn library_purity(name: &str) -> Option<Purity> {
    let name = name.strip_prefix("__builtin_").unwrap_or(name);
    LIBRARY.binary_search_by_key(&name, |&(named, _)| named).ok().map(|at| LIBRARY[at].1)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        AsmInfo, AttrSet, BlockCallList, Builder, CallInfo, Extra, Flags, Func, InstData, IntPred,
        MemInfo, MemOrder, Module, Opcode, Pic, Restrict, Signature, Type, Value,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{CallGraph, Callee, Facts, LIBRARY, Purity, infer};

    /// A module with those functions in it, declared unless they are asked to have a body.
    fn module(named: &[(&str, bool, AttrSet)]) -> (Interner, Module) {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        for &(name, defined, attrs) in named {
            let mut func = Func::new(names.intern(name), Signature::new());
            func.attrs.set = attrs;
            if defined {
                let block = func.create_block();
                let mut build = Builder::new(&mut func, block);
                let zero = build.iconst(Type::int(32), 0);
                build.ret(&[zero]);
            }
            module.add_func(func);
        }
        (names, module)
    }

    /// What the module says about a name.
    fn purity(names: &mut Interner, module: &Module, name: &str) -> Purity {
        let facts = Facts::of_module(module, names);
        let symbol = names.intern(name);
        facts.purity_of(Callee::Direct(symbol))
    }

    #[test]
    fn a_function_nobody_promised_anything_about_is_opaque() {
        let (mut names, module) = module(&[("f", true, AttrSet::NONE)]);
        assert_eq!(purity(&mut names, &module, "f"), Purity::Opaque);
    }

    #[test]
    fn a_name_this_module_never_heard_of_is_opaque_as_well() {
        let (mut names, module) = module(&[("f", true, AttrSet::NONE)]);
        let facts = Facts::of_module(&module, &names);
        assert_eq!(facts.purity_of(Callee::Direct(names.intern("g"))), Purity::Opaque);
    }

    #[test]
    fn the_const_attribute_is_honoured_because_the_user_asserted_it() {
        let (mut names, module) = module(&[("f", false, AttrSet::READNONE)]);
        let purity = purity(&mut names, &module, "f");
        assert_eq!(purity, Purity::Const);
        assert!(purity.depends_only_on_arguments());
        assert!(purity.can_be_deleted_when_unused());
    }

    #[test]
    fn the_pure_attribute_reads_memory_and_writes_none() {
        let (mut names, module) = module(&[("f", false, AttrSet::READONLY)]);
        let purity = purity(&mut names, &module, "f");
        assert_eq!(purity, Purity::Pure);
        assert!(purity.reads_memory());
        assert!(!purity.writes_memory());
        assert!(!purity.depends_only_on_arguments());
        assert!(purity.can_be_deleted_when_unused());
    }

    #[test]
    fn a_const_function_that_does_not_come_back_may_not_be_deleted() {
        // Which is the whole reason the looping levels are in the enum. Its result depends only
        // on its arguments and the call still does something, which is not come back.
        let (mut names, module) =
            module(&[("f", false, AttrSet::READNONE.union(AttrSet::NORETURN))]);
        let purity = purity(&mut names, &module, "f");
        assert_eq!(purity, Purity::LoopingConst);
        assert!(purity.depends_only_on_arguments());
        assert!(!purity.can_be_deleted_when_unused());
    }

    #[test]
    fn nothing_that_is_not_a_direct_call_is_anything_but_opaque() {
        let (mut names, module) = module(&[("f", true, AttrSet::READNONE)]);
        let facts = Facts::of_module(&module, &names);
        // Even though the module holds a const function of that name, none of these is known to
        // be it, and each is opaque for its own reason.
        assert_eq!(facts.purity_of(Callee::Indirect), Purity::Opaque);
        assert_eq!(facts.purity_of(Callee::Asm), Purity::Opaque);
        let vector = names.intern("__builtin_ia32_paddb");
        assert_eq!(facts.purity_of(Callee::Intrinsic(vector)), Purity::Opaque);
    }

    #[test]
    fn the_library_names_are_known_under_both_spellings() {
        let (mut names, module) = module(&[
            ("strlen", false, AttrSet::NONE),
            ("abs", false, AttrSet::NONE),
            ("__builtin_strlen", false, AttrSet::NONE),
            ("printf", false, AttrSet::NONE),
        ]);
        assert_eq!(purity(&mut names, &module, "strlen"), Purity::Pure);
        assert_eq!(purity(&mut names, &module, "__builtin_strlen"), Purity::Pure);
        assert_eq!(purity(&mut names, &module, "abs"), Purity::Const);
        // Everything else in the library, which is most of it, is opaque and stays that way.
        assert_eq!(purity(&mut names, &module, "printf"), Purity::Opaque);
    }

    #[test]
    fn a_module_that_defines_strlen_means_its_own() {
        let (mut names, module) = module(&[("strlen", true, AttrSet::NONE)]);
        assert_eq!(purity(&mut names, &module, "strlen"), Purity::Opaque);
    }

    #[test]
    fn no_builtin_takes_the_table_away_and_the_named_form_takes_one_entry() {
        let (mut names, module) =
            module(&[("strlen", false, AttrSet::NONE), ("abs", false, AttrSet::NONE)]);
        let mut facts = Facts::of_module(&module, &names);
        let strlen = names.intern("strlen");
        let abs = names.intern("abs");
        facts.not_the_library_name(strlen);
        assert_eq!(facts.purity_of(Callee::Direct(strlen)), Purity::Opaque);
        assert_eq!(facts.purity_of(Callee::Direct(abs)), Purity::Const);
        facts.without_the_library();
        assert_eq!(facts.purity_of(Callee::Direct(abs)), Purity::Opaque);
    }

    #[test]
    fn what_the_user_wrote_and_what_analysis_worked_out_are_kept_apart() {
        let (mut names, module) =
            module(&[("f", true, AttrSet::READNONE.union(AttrSet::NORETURN))]);
        let mut facts = Facts::of_module(&module, &names);
        let f = names.intern("f");
        assert_eq!(facts.declared(f), Purity::LoopingConst);
        assert_eq!(facts.inferred(f), Purity::Opaque);
        // Document 34 gets to say the loop inside it terminates. The declaration said the result
        // comes out of the arguments. Together that is const, and each is still readable on its
        // own, which is what makes checking one against the other possible later.
        facts.record_inferred(f, Purity::Pure);
        assert_eq!(facts.declared(f), Purity::LoopingConst);
        assert_eq!(facts.inferred(f), Purity::Pure);
        assert_eq!(facts.purity_of(Callee::Direct(f)), Purity::Const);
    }

    #[test]
    fn what_an_instruction_calls_is_read_off_the_instruction() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("caller"), Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(Signature::new());
        let direct = build.call(names.intern("f"), signature, &[]);
        let varargs = build.func().push_abis(&[]);
        let info = build.func().add_call(CallInfo { callee: None, signature, varargs });
        let indirect = build.inst(
            InstData { extra: Extra::Call(info), ..InstData::new(Opcode::CallIndirect) },
            &[],
        );
        let asm = build.inline_asm(
            AsmInfo {
                template: names.intern("nop"),
                constraints: names.intern(""),
                clobbers: names.intern(""),
                targets: BlockCallList::EMPTY,
            },
            &[],
            &[],
            Flags::NONE,
        );
        let nothing = build.ret(&[]);

        let f = names.intern("f");
        assert_eq!(Callee::of(&func, nothing), None);
        assert_eq!(Callee::of(&func, direct), Some(Callee::Direct(f)));
        assert_eq!(Callee::of(&func, indirect), Some(Callee::Indirect));
        assert_eq!(Callee::of(&func, asm), Some(Callee::Asm));
    }

    #[test]
    fn the_two_ways_of_combining_are_the_lattice_they_claim_to_be() {
        for one in Purity::ALL {
            assert_eq!(one.stronger(one), one, "{one} is not idempotent");
            assert_eq!(one.weaker(one), one, "{one} is not idempotent");
            assert_eq!(one.stronger(Purity::Opaque), one, "opaque should say nothing");
            assert_eq!(one.weaker(Purity::Opaque), Purity::Opaque, "opaque covers everything");
            for two in Purity::ALL {
                assert_eq!(one.stronger(two), two.stronger(one), "{one} and {two} disagree");
                assert_eq!(one.weaker(two), two.weaker(one), "{one} and {two} disagree");
                // Whatever comes out of the weaker of the two permits whatever either permitted.
                let both = one.weaker(two);
                assert!(both.reads_memory() >= one.reads_memory());
                assert!(both.writes_memory() >= one.writes_memory());
                assert!(both.terminates() <= one.terminates());
            }
        }
    }

    #[test]
    fn only_an_opaque_call_may_write_memory() {
        for purity in Purity::ALL {
            assert_eq!(purity.writes_memory(), purity == Purity::Opaque, "{purity}");
            assert_eq!(purity.can_be_deleted_when_unused(), purity.terminates(), "{purity}");
        }
    }

    #[test]
    fn the_library_table_is_sorted_says_each_name_once_and_writes_no_memory() {
        // Sorted because the lookup is a binary search, and the rest because an entry here is
        // believed without being checked against anything.
        for pair in LIBRARY.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} and {} are out of order", pair[0].0, pair[1].0);
        }
        for &(name, purity) in LIBRARY {
            assert!(!purity.writes_memory(), "{name} would not be worth an entry");
            assert!(purity.terminates(), "{name} is in the table to be deletable");
            assert!(!name.starts_with("__builtin_"), "{name} is reached under both spellings");
        }
    }

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

    /// The address of a file scope variable, which is memory this function did not make.
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

    /// A call to that name with no arguments and nothing done with what came back.
    fn calls(build: &mut Builder<'_>, names: &mut Interner, name: &str) {
        let name = names.intern(name);
        let signature = build.func().add_signature(Signature::new());
        build.call(name, signature, &[]);
    }

    /// One function's body, which is how a test says what its function does.
    type Body = fn(&mut Interner, &mut Func);

    /// A module of functions with bodies, with the purity worked out over it.
    ///
    /// Each body is a plain function pointer rather than a closure, so that a test says what its
    /// function does in one place and two tests can share a body without sharing a module.
    struct Worked {
        names: Interner,
        facts: Facts,
    }

    impl Worked {
        /// Builds the module, then runs the analysis over it as the pipeline would.
        fn out(bodies: &[(&str, Body)]) -> Self {
            Self::linked(Pic::Executable, bodies)
        }

        /// The same, for a link where an exported definition may be replaced at load time.
        fn linked(pic: Pic, bodies: &[(&str, Body)]) -> Self {
            let mut names = Interner::new();
            let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
            let mut module = Module::new(names.intern("t.c"), &target);
            for &(name, body) in bodies {
                let mut func = Func::new(names.intern(name), Signature::new());
                body(&mut names, &mut func);
                module.add_func(func);
            }
            let mut facts = Facts::of_module(&module, &names);
            infer(&module, &CallGraph::of(&module, pic), &mut facts);
            Self { names, facts }
        }

        /// What the analysis worked out on its own, which is the field these tests are about.
        fn about(&mut self, name: &str) -> Purity {
            let name = self.names.intern(name);
            self.facts.inferred(name)
        }

        /// Everything known about it from all three sources, which is what a call site sees.
        fn at_a_call_site(&mut self, name: &str) -> Purity {
            let name = self.names.intern(name);
            self.facts.purity_of(Callee::Direct(name))
        }
    }

    /// Adds two constants and hands back the answer.
    fn only_arithmetic(_: &mut Interner, func: &mut Func) {
        let block = func.create_block();
        let mut build = Builder::new(func, block);
        let a = build.iconst(Type::int(32), 2);
        let b = build.iconst(Type::int(32), 3);
        let sum = build.binary(Opcode::Add, a, b, Flags::NONE);
        build.ret(&[sum]);
    }

    /// Nothing at all, which is a body and not a declaration.
    fn empty(_: &mut Interner, func: &mut Func) {
        let block = func.create_block();
        Builder::new(func, block).ret(&[]);
    }

    /// No body, which is what a declaration is.
    fn declared(_: &mut Interner, _: &mut Func) {}

    #[test]
    fn a_function_that_only_computes_is_const() {
        assert_eq!(Worked::out(&[("f", only_arithmetic)]).about("f"), Purity::Const);
    }

    #[test]
    fn a_function_that_reads_memory_it_did_not_make_is_pure() {
        fn reads(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            let at = somewhere(&mut build, names);
            let value = build.load(Type::int(32), at, access(), Flags::NONE);
            build.ret(&[value]);
        }
        let mut worked = Worked::out(&[("f", reads)]);
        assert_eq!(worked.about("f"), Purity::Pure);
        assert!(worked.about("f").can_be_deleted_when_unused());
    }

    #[test]
    fn a_function_that_writes_memory_it_did_not_make_is_opaque() {
        fn writes(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            let at = somewhere(&mut build, names);
            let zero = build.iconst(Type::int(32), 0);
            build.store(zero, at, access(), Flags::NONE);
            build.ret(&[]);
        }
        assert_eq!(Worked::out(&[("f", writes)]).about("f"), Purity::Opaque);
    }

    #[test]
    fn a_local_this_function_kept_to_itself_is_not_memory() {
        // The case the compiler meets everywhere until there is an SROA, which is an ordinary
        // function with a temporary in it. Writing to the temporary and reading it back is not
        // an access of anything the caller had the address of.
        fn temporary(_: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            let at = stack(&mut build);
            let zero = build.iconst(Type::int(32), 0);
            build.store(zero, at, access(), Flags::NONE);
            let back = build.load(Type::int(32), at, access(), Flags::NONE);
            build.ret(&[back]);
        }
        assert_eq!(Worked::out(&[("f", temporary)]).about("f"), Purity::Const);
    }

    #[test]
    fn a_local_whose_address_the_function_hands_back_is_memory_like_any_other() {
        fn handed_back(_: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            let at = stack(&mut build);
            let zero = build.iconst(Type::int(32), 0);
            build.store(zero, at, access(), Flags::NONE);
            build.ret(&[at]);
        }
        assert_eq!(Worked::out(&[("f", handed_back)]).about("f"), Purity::Opaque);
    }

    #[test]
    fn a_volatile_read_is_opaque_however_private_the_storage_is() {
        // An access the program asked for by name happens whether or not anybody wanted the
        // value, so which object it is of decides nothing.
        fn volatile(_: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            let at = stack(&mut build);
            let value = build.load(Type::int(32), at, access(), Flags::VOLATILE);
            build.ret(&[value]);
        }
        assert_eq!(Worked::out(&[("f", volatile)]).about("f"), Purity::Opaque);
    }

    #[test]
    fn a_caller_is_what_the_function_it_calls_is() {
        fn calls_g(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            calls(&mut build, names, "g");
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", calls_g), ("g", only_arithmetic)]);
        assert_eq!(worked.about("g"), Purity::Const);
        assert_eq!(worked.about("f"), Purity::Const);
    }

    #[test]
    fn a_caller_of_something_nobody_can_see_is_opaque() {
        fn calls_g(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            calls(&mut build, names, "g");
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", calls_g), ("g", declared)]);
        assert_eq!(worked.about("g"), Purity::Opaque);
        assert_eq!(worked.about("f"), Purity::Opaque);
    }

    #[test]
    fn what_the_library_says_about_a_callee_reaches_the_caller() {
        fn calls_strlen(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            calls(&mut build, names, "strlen");
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", calls_strlen), ("strlen", declared)]);
        assert_eq!(worked.about("f"), Purity::Pure);
    }

    #[test]
    fn two_functions_that_call_each_other_and_do_nothing_else_are_not_opaque() {
        // The case the optimism of section 34.5 is for. Starting each of them at opaque and
        // raising would leave both there, because each is waiting on the other. Starting at const
        // and lowering settles at the looping level, which is right: neither body does anything,
        // and a pair of functions calling each other round a ring need not ever come back.
        fn calls_g(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            calls(&mut build, names, "g");
            build.ret(&[]);
        }
        fn calls_f(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            calls(&mut build, names, "f");
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", calls_g), ("g", calls_f)]);
        assert_eq!(worked.about("f"), Purity::LoopingConst);
        assert_eq!(worked.about("g"), Purity::LoopingConst);
        assert!(worked.about("f").depends_only_on_arguments());
        assert!(!worked.about("f").can_be_deleted_when_unused());
    }

    #[test]
    fn a_function_that_calls_itself_may_not_come_back() {
        fn calls_itself(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            calls(&mut build, names, "f");
            build.ret(&[]);
        }
        assert_eq!(Worked::out(&[("f", calls_itself)]).about("f"), Purity::LoopingConst);
    }

    #[test]
    fn a_function_with_a_loop_in_it_may_not_come_back() {
        // Nothing here proves the loop finite, and until something does, deleting a call to this
        // would be deleting the program's chance to hang.
        fn loops(_: &mut Interner, func: &mut Func) {
            let entry = func.create_block();
            let head = func.create_block();
            let done = func.create_block();
            let mut build = Builder::new(func, entry);
            build.jump(head, &[]);
            let mut build = Builder::new(func, head);
            let zero = build.iconst(Type::int(32), 0);
            let cond = build.icmp(IntPred::Eq, zero, zero);
            build.br_if(cond, head, &[], done, &[]);
            Builder::new(func, done).ret(&[]);
        }
        let mut worked = Worked::out(&[("f", loops)]);
        assert_eq!(worked.about("f"), Purity::LoopingConst);
        assert!(!worked.about("f").can_be_deleted_when_unused());
    }

    #[test]
    fn a_branch_that_joins_again_is_not_a_loop() {
        // The other half of the test above, because a reverse postorder rank that was compared
        // the wrong way round would call every `if` a loop and nothing would ever be const.
        fn branches(_: &mut Interner, func: &mut Func) {
            let entry = func.create_block();
            let arm = func.create_block();
            let join = func.create_block();
            let mut build = Builder::new(func, entry);
            let zero = build.iconst(Type::int(32), 0);
            let cond = build.icmp(IntPred::Eq, zero, zero);
            build.br_if(cond, arm, &[], join, &[]);
            Builder::new(func, arm).jump(join, &[]);
            Builder::new(func, join).ret(&[]);
        }
        assert_eq!(Worked::out(&[("f", branches)]).about("f"), Purity::Const);
    }

    #[test]
    fn a_declaration_has_nothing_worked_out_about_it() {
        assert_eq!(Worked::out(&[("f", declared)]).about("f"), Purity::Opaque);
    }

    #[test]
    fn a_body_this_link_may_replace_has_nothing_worked_out_about_it() {
        // Section 34.1's gate, which the call graph answers and this one only obeys. The body in
        // hand does nothing, and in a library the definition that runs may be another object's.
        let mut worked = Worked::linked(Pic::Library, &[("f", only_arithmetic)]);
        assert_eq!(worked.about("f"), Purity::Opaque);
        let mut worked = Worked::linked(Pic::Executable, &[("f", only_arithmetic)]);
        assert_eq!(worked.about("f"), Purity::Const);
    }

    #[test]
    fn nothing_is_written_down_for_a_function_that_came_out_opaque() {
        // Opaque is what an empty map already says, so recording it would put an entry in for
        // every function in the module to say the thing the absence of an entry says.
        let mut worked = Worked::out(&[("f", declared)]);
        assert!(worked.facts.inferred.is_empty());
        assert_eq!(worked.about("f"), Purity::Opaque);
    }

    #[test]
    fn what_the_user_declared_and_what_the_body_says_are_both_kept() {
        // A person writing `__attribute__((const))` on a function with a loop in it is asserting
        // that the loop finishes. The assertion is honoured at the call site and the worked out
        // answer is still there to be checked against it later, which is what two fields are for.
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let mut func = Func::new(names.intern("f"), Signature::new());
        func.attrs.set = AttrSet::READNONE;
        let entry = func.create_block();
        let head = func.create_block();
        Builder::new(&mut func, entry).jump(head, &[]);
        Builder::new(&mut func, head).jump(head, &[]);
        module.add_func(func);
        let mut facts = Facts::of_module(&module, &names);
        infer(&module, &CallGraph::of(&module, Pic::Executable), &mut facts);
        let name = names.intern("f");
        assert_eq!(facts.inferred(name), Purity::LoopingConst);
        assert_eq!(facts.declared(name), Purity::Const);
        assert_eq!(facts.purity_of(Callee::Direct(name)), Purity::Const);
    }

    #[test]
    fn an_answer_travels_as_far_up_the_chain_as_it_holds() {
        fn calls_g(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            calls(&mut build, names, "g");
            build.ret(&[]);
        }
        fn calls_h(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            calls(&mut build, names, "h");
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", calls_g), ("g", calls_h), ("h", empty)]);
        assert_eq!(worked.about("h"), Purity::Const);
        assert_eq!(worked.about("g"), Purity::Const);
        assert_eq!(worked.about("f"), Purity::Const);
        assert_eq!(worked.at_a_call_site("f"), Purity::Const);
    }

    #[test]
    fn a_reader_under_a_writer_makes_the_caller_opaque_and_not_pure() {
        // Order does not come into it: one opaque callee anywhere in a body is the whole answer,
        // and the walk stops at the first one rather than combining the rest.
        fn writes(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            let at = somewhere(&mut build, names);
            let zero = build.iconst(Type::int(32), 0);
            build.store(zero, at, access(), Flags::NONE);
            build.ret(&[]);
        }
        fn calls_both(names: &mut Interner, func: &mut Func) {
            let block = func.create_block();
            let mut build = Builder::new(func, block);
            calls(&mut build, names, "g");
            calls(&mut build, names, "h");
            build.ret(&[]);
        }
        let mut worked = Worked::out(&[("f", calls_both), ("g", only_arithmetic), ("h", writes)]);
        assert_eq!(worked.about("f"), Purity::Opaque);
    }
}
