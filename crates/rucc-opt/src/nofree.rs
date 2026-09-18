//! Which functions cannot free memory, and writing that onto the calls to them.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.5, which asks for a summary per
//! function recording "which pointer parameters are dereferenced and over what range, which are
//! freed, which escape, and whether the function can free memory at all", and then says which of
//! the four matters most: "The last is the one that unlocks temporal elimination, a call to a
//! function summarized as `nofree` does not kill liveness facts, and `nofree` is true of a very
//! large fraction of leaf functions." Document 08 section 8.8 puts a number on it, and the number
//! is why this is built before the other three: without it a fact dies at every call, and the
//! temporal checks cost forty per cent rather than five.
//!
//! Two more of the four are here now, the freed parameters and the escaping ones, and they are the
//! same walk over the same call graph at a finer grain. [`Reach`] is what one call can do to the
//! storage its caller can see: which parameters it can hand back, which ones it can leave somewhere
//! the caller cannot see, and whether it can end a lifetime it did not reach through a parameter.
//! The whole function answer above is the case of that where the first is empty and the third is
//! false, so there is one fixed point rather than two and `nofree` falls out of it.
//!
//! The dereferenced ranges are the fourth and are not here.
//!
//! # Why the finer answer is worth having
//!
//! tamnd/rucc#849 prices it. The two largest rows in the safety census on SQLite are both a fact
//! thrown away because a call stood between the check that established it and the check that wanted
//! it, 2194 bounds checks at 626 sites and 2177 liveness checks at 625, and marking every call
//! nofree whatever it does takes them to 68 at 35. That is the ceiling, and a whole function
//! question cannot get near it: 1516 of the 1551 names that fail to clear are defined in the
//! amalgamation itself and almost every one of them reaches the allocator somewhere, so "can this
//! free anything" answers no for almost nothing.
//!
//! The question that does work is "can this free the object this check is about", and it is asked
//! of the object rather than of the callee. That needs the escape analysis section 7.6 asks for and
//! it is not here. What is here is the half the callee owes it: a call that keeps none of the
//! pointers it was handed cannot be the reason one of them turns up somewhere later, which is what
//! makes an object that was never stored anywhere an object a later call has no way to reach.
//!
//! What the three answers come to on that file is worth writing down, since the next piece of work
//! is going to be judged against it. 2649 names get a summary. 768 of them free nothing at all,
//! which is the answer this module already had. Only 19 free a parameter, and 1878 can free
//! something they reached some other way, which is why the whole function answer is 768 and not
//! most of them. 606 keep none of the pointers they are handed, and 396 both keep nothing and free
//! nothing outside their parameters, and that last number is the one a caller asking about a
//! particular object needs, because those are the calls it can rule out.
//!
//! # What is claimed about a name in the table
//!
//! The `NEVER_FREES` table now says two things rather than one. It already said the function ends no
//! lifetime, and it says as well that the function keeps none of the pointers it is handed, which is
//! what lets a pointer passed to `memcpy` still be a pointer nothing else can reach. The bar is the
//! same one the table already had: every name in it is specified as moving or reading bytes, and a
//! specification that says what a function does with the bytes and mentions no retention is a
//! specification that does not allow it. That is the same argument every compiler makes about the
//! same list of names.
//!
//! # Where the answer goes
//!
//! Whether the function being called can free is a fact about a different function, and a pass is
//! given one function and not the module around it. So the answer is not kept in a table a pass
//! would have to be handed. It is written into the IR, as [`Flags::NOFREE`] on the call site, by
//! [`annotate`] before the pipeline starts, and `crate::discharge` reads it off the instruction in
//! front of it. That is what the frontend already does with a call that never comes back: it puts
//! an `unreachable` after the call rather than expecting every later pass to look the callee up.
//!
//! Two deviations from where the design puts this, both deliberate and both worth writing down.
//! Section 7.5 and document 15's crate table say `rucc-lto` records the summaries, and `rucc-lto`
//! is a crate that holds its layer rank and nothing else until M8. This is here instead, one
//! translation unit at a time, which is the part of the answer a compile of one file can have and
//! is what the pass consuming it can use today. The second is that the summary is not a summary
//! anybody can read back: it is spent on the call sites and thrown away. The day link time
//! optimization arrives it will want a record that survives the file it was worked out in, and
//! [`Summaries`] is the shape of it.
//!
//! # What it takes to be nofree
//!
//! A function this module defines is nofree when every call in its body goes somewhere nofree and
//! it ends no lifetime itself. Everything else it can do is arithmetic, memory traffic and control
//! flow, none of which ends anything.
//!
//! A function this module does not define is nofree only if the `NEVER_FREES` table names it. That
//! is the same bar `crate::purity` sets for its library table and for the same reason: a name
//! missing from the table costs a check that stays, and a wrong name in the table costs a check
//! that goes when it should not have, which is a hole in the safety this compiler is for. Nothing
//! goes in there unless the standard says what the function does and what it does is not freeing.
//!
//! The one exception to needing a standard is this compiler's own runtime, which has a table of
//! its own. Those bodies are in this repository, so the reason for the bar does not apply to them
//! and reading what they do is the check the standard stands in for everywhere else.
//!
//! Not defining it and not declaring it are two different things, and the table is asked in both
//! cases. A module usually has a declaration for every name it calls, because that is what the C
//! it was compiled from had to have, but `rucc-safety` puts calls in that no source wrote: a
//! witness at every boundary crossing, and a redirect of a library call to the wrapper around it.
//! Both intern a name and emit a call to it without adding a function to hang the name on. Asking
//! only the declarations meant the table was never asked about either of them, so the wrapper
//! spelling `never_frees` handles below did nothing for the calls it was written for, and on the
//! SQLite amalgamation 1322 call sites read as possible frees on that account alone. See
//! tamnd/rucc#810.
//!
//! `meta_end` and `meta_transfer` are counted as freeing wherever they appear. Nothing emits
//! either of them yet, so today this costs nothing, and when the instrumentation starts ending
//! lifetimes it will be conservative rather than wrong. The refinement is that `meta_end` on an
//! automatic instance the callee created in its own frame cannot be about storage the caller had a
//! pointer to before the call, but knowing that needs the storage class the matching `meta_begin`
//! carries and the escape analysis in section 7.6, so it waits for them.
//!
//! A call through an address, inline assembly and a target intrinsic are all counted as freeing.
//! The first two could reach anything. The third could not, since a target intrinsic is a machine
//! instruction, but the intrinsic set is open and named rather than enumerated, so nothing here
//! knows which one it is looking at, and [`crate::purity`] answers the same way for the same
//! reason.
//!
//! # Recursion, and which way the fixed point goes
//!
//! Every defined function starts in the set and is taken out when something it calls is not in it,
//! until nothing changes. That is the least fixed point of "can free", and starting the other way
//! round would be wrong in the direction that matters less but is still wrong: a function that
//! calls itself and frees nothing would never get into the set, and a pair of functions that call
//! each other and free nothing would keep each other out of it forever.
//!
//! # What is trusted about a definition
//!
//! A `weak` or `common` definition is not trusted, because the linker is allowed to throw it away
//! and take a definition from another object instead, and this analysis read the one that will not
//! run.
//!
//! An ordinary external definition is trusted when the link that is coming puts every name in the
//! same program, and not otherwise. A shared library's exported symbol can be interposed at run
//! time, by `LD_PRELOAD` or by an earlier object in the search order, and the definition that runs
//! is then one this module never saw, so under `-fPIC` a name like that is left out of the set and
//! every caller of it pays for the possibility. This used to be an assumption instead, written down
//! in this comment and true of nothing but an executable, which is what tamnd/rucc#756 turned into
//! a question the compiler can actually ask.
//!
//! `-fno-semantic-interposition` puts the trust back, and that is a promise the build makes rather
//! than anything deduced here. Every distribution makes it, because a library that cannot believe
//! its own bodies pays for an interposition that almost never happens. `-fvisibility=hidden` gets
//! the same result by making the names uninterposable, which is a stronger thing to say and needs
//! no promise.

use std::collections::{HashMap, HashSet};

use rucc_base::{Interner, Symbol};
use rucc_ir::{Block, Def, Extra, Flags, Func, FuncId, Inst, Linkage, Module, Opcode, Pic, Value};

use crate::cfg::Cfg;
use crate::copy;

/// A set of parameter positions.
///
/// Sixty four of them by name and the rest together, which is not a limit anything runs into: the
/// widest function in the SQLite amalgamation takes nine arguments and the C standard only promises
/// a translator will accept a hundred and twenty seven. A function past the sixty fourth position
/// gets one bit for all of them, and the bit is set rather than clear wherever the answer is not
/// known exactly, so a function that wide costs a check that stays rather than losing one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Params {
    /// One bit for each of the first sixty four positions.
    bits: u64,
    /// Whether every position past those is in the set.
    rest: bool,
}

impl Params {
    /// No position at all.
    pub const NONE: Self = Self { bits: 0, rest: false };

    /// Every position there is, which is how a callee nothing is known about is answered.
    pub const ALL: Self = Self { bits: u64::MAX, rest: true };

    /// Whether the parameter in that position is in the set.
    #[must_use]
    pub fn contains(self, at: usize) -> bool {
        match u32::try_from(at) {
            Ok(at) if at < u64::BITS => self.bits & (1 << at) != 0,
            _ => self.rest,
        }
    }

    /// Whether no position is.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.bits == 0 && !self.rest
    }

    /// The same set with that position added.
    #[must_use]
    fn with(self, at: usize) -> Self {
        match u32::try_from(at) {
            Ok(at) if at < u64::BITS => Self { bits: self.bits | (1 << at), rest: self.rest },
            _ => Self { bits: self.bits, rest: true },
        }
    }

    /// Everything in either of them.
    #[must_use]
    fn union(self, other: Self) -> Self {
        Self { bits: self.bits | other.bits, rest: self.rest || other.rest }
    }
}

/// What one call to a function can do to storage its caller can see.
///
/// Three answers rather than one, and they are not the same question asked three ways. Freeing a
/// parameter is a thing that has happened by the time the call comes back. Keeping one is a thing
/// that has not happened yet and may never: it says the callee left the pointer somewhere, so some
/// later call reaches it without being handed it. Freeing something else is the escape hatch, and a
/// call with it set is one this tells the caller nothing useful about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reach {
    /// The parameters whose storage the call can hand back.
    pub freed: Params,
    /// The parameters the call can leave somewhere the caller cannot see.
    pub kept: Params,
    /// Whether it can end the lifetime of storage it did not reach through a parameter.
    pub frees_other: bool,
}

impl Reach {
    /// A call that does none of the three.
    pub const NOTHING: Self = Self { freed: Params::NONE, kept: Params::NONE, frees_other: false };

    /// A call nothing is known about, which is what a name with no body here and no table entry
    /// gets and is the answer that costs checks rather than losing them.
    pub const UNKNOWN: Self = Self { freed: Params::ALL, kept: Params::ALL, frees_other: true };

    /// Whether a call to it ends no lifetime at all, which is the whole function answer.
    #[must_use]
    pub fn frees_nothing(self) -> bool {
        self.freed.is_empty() && !self.frees_other
    }

    /// Everything either of them can do.
    #[must_use]
    fn union(self, other: Self) -> Self {
        Self {
            freed: self.freed.union(other.freed),
            kept: self.kept.union(other.kept),
            frees_other: self.frees_other || other.frees_other,
        }
    }
}

/// What is known about which functions cannot free.
///
/// Built from the module once, because the answer belongs to the callee and there is one callee
/// and many call sites, which is the same shape [`crate::purity::Facts`] has.
#[derive(Debug, Clone, Default)]
pub struct Summaries {
    nofree: HashSet<Symbol>,
    reach: HashMap<Symbol, Reach>,
}

impl Summaries {
    /// Nothing known about anything, which answers no to every question and is correct.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// Works out which of the module's functions cannot free.
    ///
    /// The interner is here for the library table, which is written in text because that is what
    /// the C standard names the functions. Nothing after this call needs it.
    #[must_use]
    pub fn of_module(module: &Module, names: &Interner, pic: Pic) -> Self {
        let ids: Vec<FuncId> = module.funcs().collect();
        let mut reach: HashMap<Symbol, Reach> = HashMap::new();
        for &id in &ids {
            let func = &module[id];
            if func.is_declaration() {
                if let Some(known) = table(names.resolve(func.name)) {
                    reach.insert(func.name, known);
                }
            } else if trusted(func, pic) {
                // Optimistic, and narrowed below. See the module comment for which way round the
                // fixed point has to go and what starting from the other end would cost.
                reach.insert(func.name, Reach::NOTHING);
            } else {
                reach.insert(func.name, Reach::UNKNOWN);
            }
        }
        // The same question again, for a name the module calls and has no function of any kind
        // for. See the module comment for why those exist and why the loop above cannot see them.
        let present: HashSet<Symbol> = ids.iter().map(|&id| module[id].name).collect();
        for &id in &ids {
            let func = &module[id];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    let Some(callee) = called(func, inst) else { continue };
                    if present.contains(&callee) || reach.contains_key(&callee) {
                        continue;
                    }
                    if let Some(known) = table(names.resolve(callee)) {
                        reach.insert(callee, known);
                    }
                }
            }
        }
        // Read once and kept, because the fixed point below goes round the bodies as many times as
        // it has to and what each value is built out of does not change between the rounds. Only
        // the values some parameter reaches are in there, which is a small part of a function.
        let bodies: Vec<(FuncId, HashMap<Value, Params>)> = ids
            .iter()
            .copied()
            .filter(|&id| !module[id].is_declaration() && trusted(&module[id], pic))
            .map(|id| (id, derived(&module[id], &Cfg::new(&module[id]))))
            .collect();
        loop {
            let mut settled = true;
            for (id, from) in &bodies {
                let func = &module[*id];
                let was = reach.get(&func.name).copied().unwrap_or(Reach::UNKNOWN);
                let now = was.union(reaches(func, from, &reach));
                if now != was {
                    reach.insert(func.name, now);
                    settled = false;
                }
            }
            if settled {
                break;
            }
        }
        let nofree =
            reach.iter().filter(|(_, what)| what.frees_nothing()).map(|(&name, _)| name).collect();
        Self { nofree, reach }
    }

    /// Whether a call to that name reaches nothing that ends a lifetime.
    #[must_use]
    pub fn cannot_free(&self, name: Symbol) -> bool {
        self.nofree.contains(&name)
    }

    /// What a call to that name can do to the storage its caller can see.
    #[must_use]
    pub fn reach(&self, name: Symbol) -> Reach {
        self.reach.get(&name).copied().unwrap_or(Reach::UNKNOWN)
    }

    /// How many names are in the set, which is what a caller reporting the summary wants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nofree.len()
    }

    /// Whether nothing at all was established.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nofree.is_empty()
    }
}

/// Works out the summaries and marks every call site they vouch for, saying how many it marked.
///
/// Only sets the flag, never clears one. The flag is an assertion like the rest of them, so a
/// caller that put one there meant it, and this adds the ones it can prove rather than replacing
/// what it finds.
pub fn annotate(module: &mut Module, names: &Interner, pic: Pic) -> usize {
    let summaries = Summaries::of_module(module, names, pic);
    let mut marked = 0;
    let ids: Vec<FuncId> = module.funcs().collect();
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        let func = &mut module[id];
        let insts: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
        for inst in insts {
            // A tail call as well as a call. Nothing after a tail call needs the fact, but the
            // flag says what the call reaches rather than what happens after it, and a call that
            // carried it under one spelling and not the other would read as a disagreement.
            if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
                continue;
            }
            let Extra::Call(at) = func[inst].extra else { continue };
            let Some(callee) = func[at].callee else { continue };
            if func[inst].flags.contains(Flags::NOFREE)
                || !ends_nothing(func, inst, summaries.reach(callee))
            {
                continue;
            }
            func[inst].flags |= Flags::NOFREE;
            marked += 1;
        }
    }
    marked
}

/// Whether this call ends no lifetime, which is a question about the site and not only the callee.
///
/// The whole function answer is the case of this where the callee frees nothing at all. What the
/// site adds is that a callee which can only free through its parameters frees nothing here if it
/// was handed no pointer in a position it frees. A function that takes a count and a flag and frees
/// what it can reach through them reaches nothing, whatever it does when it is handed a pointer.
///
/// A modest rule on its own and it is the shape of the useful one. The question that pays is asked
/// of the object rather than of the call, and it wants the escape analysis in section 7.6 as well as
/// the [`Reach::kept`] half of this.
fn ends_nothing(func: &Func, inst: Inst, reach: Reach) -> bool {
    if reach.frees_other {
        return false;
    }
    let args = &func[func[inst].args];
    !args.iter().enumerate().any(|(at, &arg)| reach.freed.contains(at) && func[arg].ty.is_ptr())
}

/// The name a call names, or `None` when the instruction is not a call to a name.
fn called(func: &Func, inst: Inst) -> Option<Symbol> {
    if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
        return None;
    }
    let Extra::Call(at) = func[inst].extra else { return None };
    func[at].callee
}

/// Whether the definition in hand is the one that will run.
///
/// Two linkages say otherwise whatever the link is. `weak` and `common` are both definitions the
/// linker is allowed to throw away in favour of one from another object, so a body with either of
/// them is one this analysis may have read for nothing.
///
/// The third way is the link itself. Under `-fPIC` an exported name is one the dynamic linker may
/// find another definition of first, so this body is not the one that runs however plainly it is
/// written here, and `pic` is what carries that. A `static` is never such a name and neither is one
/// marked hidden or protected, which is the same rule the code generator uses to decide which
/// addresses go through the table and is why it is the same method.
fn trusted(func: &Func, pic: Pic) -> bool {
    !matches!(func.linkage, Linkage::Weak | Linkage::Common)
        && !pic.replaceable(func.linkage, func.visibility)
}

/// Which parameters each value in a body may be built out of.
///
/// A map rather than a walk from each value, because the same question is asked of every operand of
/// every instruction below and a body with a loop in it has values that reach each other. Only the
/// values some parameter reaches are in it. Everything else is a number, a constant address, or
/// something read out of memory, and the empty set is the right answer for all of them.
///
/// A load is the one worth saying out loud. What comes back from an address built on a parameter is
/// the bytes at that address rather than the parameter, and they are some other object's, so the
/// walk stops there. That is what makes `free(p->next)` read as freeing something that is not `p`,
/// which is both true and the answer that costs a check.
fn derived(func: &Func, cfg: &Cfg) -> HashMap<Value, Params> {
    let mut from: HashMap<Value, Params> = HashMap::new();
    let Some(entry) = func.entry() else { return from };
    // Values come out in the order they were made, which is a definition before its uses, so a body
    // with no loop in it settles in one pass and the loop is for the ones that have one.
    loop {
        let mut settled = true;
        for value in func.values() {
            let was = from.get(&value).copied().unwrap_or(Params::NONE);
            let now = was.union(source(func, cfg, &from, entry, value));
            if now != was {
                from.insert(value, now);
                settled = false;
            }
        }
        if settled {
            return from;
        }
    }
}

/// Where one value comes from, given what is known so far about the values it is built out of.
///
/// Everything that follows an operand here is on [`holds`], and it has to be: a pointer that goes
/// into an instruction and comes out of it is a pointer this has to keep hold of, or the instruction
/// would be one that puts it somewhere nothing accounts for. The integer arithmetic is here for
/// that reason rather than because anybody subscripts with a pointer. `p - q` is a `ptr_to_int` on
/// each side and a subtract, and if the subtract stopped the walk then storing the difference would
/// be storing something this had lost track of.
fn source(
    func: &Func,
    cfg: &Cfg,
    from: &HashMap<Value, Params>,
    entry: Block,
    value: Value,
) -> Params {
    let known = |of: Value| from.get(&of).copied().unwrap_or(Params::NONE);
    match func[value].def {
        // The parameters of the entry block are the function's parameters, and the index is the
        // position, which is the whole of what this analysis is about.
        Def::Param { block, index } if block == entry => Params::NONE.with(index as usize),
        Def::Param { block, index } => {
            let mut out = Params::NONE;
            for &pred in cfg.predecessors(block) {
                let Some(term) = func.terminator(pred) else { continue };
                if let Some(&came) = copy::edge_args(func, term, block).get(index as usize) {
                    out = out.union(known(came));
                }
            }
            out
        }
        Def::Result { inst, .. } => {
            let args = &func[func[inst].args];
            match func[inst].opcode {
                Opcode::PtrAdd | Opcode::PtrToInt | Opcode::IntToPtr | Opcode::Bitcast => {
                    args.first().map_or(Params::NONE, |&of| known(of))
                }
                // The condition is not one of them, since which way it went is not where the
                // address came from.
                Opcode::Select => match (args.get(1), args.get(2)) {
                    (Some(&one), Some(&two)) => known(one).union(known(two)),
                    _ => Params::NONE,
                },
                Opcode::Add
                | Opcode::Sub
                | Opcode::Mul
                | Opcode::Shl
                | Opcode::LShr
                | Opcode::AShr
                | Opcode::And
                | Opcode::Or
                | Opcode::Xor
                | Opcode::Trunc
                | Opcode::SExt
                | Opcode::ZExt => args.iter().fold(Params::NONE, |out, &of| out.union(known(of))),
                _ => Params::NONE,
            }
        }
    }
}

/// What a body does to the storage its parameters point at.
///
/// `at` is what is known about the callees as the fixed point stands, so this is asked again every
/// time one of them learns something, and the answer only ever grows.
fn reaches(func: &Func, from: &HashMap<Value, Params>, at: &HashMap<Symbol, Reach>) -> Reach {
    let known = |of: Value| from.get(&of).copied().unwrap_or(Params::NONE);
    // Freeing something no parameter reaches is the third field rather than nothing, because the
    // storage may still be the caller's: a pointer this function read out of a global is a pointer
    // the caller could be holding too.
    let ended = |what: Params, out: &mut Reach| {
        if what.is_empty() {
            out.frees_other = true;
        } else {
            out.freed = out.freed.union(what);
        }
    };
    let mut out = Reach::NOTHING;
    for block in func.blocks() {
        for inst in func.insts(block) {
            let args = &func[func[inst].args];
            match func[inst].opcode {
                // Saying a lifetime is over, about whatever the first operand is about. Nothing
                // emits either of these yet.
                Opcode::MetaEnd | Opcode::MetaTransfer => {
                    ended(args.first().map_or(Params::NONE, |&of| known(of)), &mut out);
                }
                Opcode::Call | Opcode::TailCall => {
                    let reach = called(func, inst).map_or(Reach::UNKNOWN, |name| {
                        at.get(&name).copied().unwrap_or(Reach::UNKNOWN)
                    });
                    out.frees_other |= reach.frees_other;
                    for (place, &arg) in args.iter().enumerate() {
                        if reach.freed.contains(place) && func[arg].ty.is_ptr() {
                            ended(known(arg), &mut out);
                        }
                        if reach.kept.contains(place) {
                            out.kept = out.kept.union(known(arg));
                        }
                    }
                }
                // Nothing here knows what any of these reach, so every pointer they are handed is
                // one that could go anywhere and the storage they end could be anybody's.
                Opcode::CallIndirect | Opcode::InlineAsm | Opcode::TargetIntrinsic => {
                    out.frees_other = true;
                    for &arg in args {
                        out.kept = out.kept.union(known(arg));
                    }
                }
                // The value written and not the address written to. Putting a pointer into memory
                // is how it gets somewhere a later call can find it; writing through one is not.
                Opcode::Store => {
                    out.kept = out.kept.union(args.first().map_or(Params::NONE, |&of| known(of)));
                }
                // Handing it back is leaving it somewhere this function does not own, which is the
                // same thing as storing it as far as a later call is concerned.
                Opcode::Return => {
                    for &arg in args {
                        out.kept = out.kept.union(known(arg));
                    }
                }
                opcode if holds(opcode) => {}
                // Everything else, counted as putting every pointer it was given somewhere. An
                // atomic exchange is the one this really is about, since the pointer it writes and
                // the pointer it is given are in positions this would have to read the payload to
                // tell apart, and the answer that costs a check is the one to give.
                _ => {
                    for &arg in args {
                        out.kept = out.kept.union(known(arg));
                    }
                }
            }
        }
    }
    out
}

/// Whether an instruction can be handed a pointer without leaving it anywhere a later call could
/// find it.
///
/// A named list rather than a question about effects, for the reason `crate::split::plain` has one:
/// what has to hold is that the pointer is still only where this function put it, and naming what
/// is allowed makes an opcode added later read as leaving it somewhere until somebody looks at it.
///
/// Anything on this list that produces a value has to be followed by [`source`] as well, or a
/// pointer would go in one end of it and come out of the other with nothing said about where it
/// went. A load is the exception that proves it: the value it produces is the bytes at the address
/// rather than the address, so there is nothing to follow.
///
/// The three block memory operations are here because they move bytes between addresses they are
/// handed and keep neither address. A pointer that is part of the bytes they move was written to
/// memory by something, and that something is where it was kept.
///
/// The branches are here because the arguments an edge carries are where a block's parameters come
/// from, which [`source`] reads off the edge, so a pointer going round a loop is a pointer this
/// followed rather than one it lost.
///
/// `cap_of` is the one to look at again the day anything writes a capability to memory. What it
/// produces names the object the pointer is in, and [`source`] does not follow it, so a capability
/// left somewhere would be a pointer's worth of reach this did not account for. Nothing emits a
/// `cap_store` today, and the instruction that would is the one to come back here with.
fn holds(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Load
            | Opcode::PtrAdd
            | Opcode::PtrToInt
            | Opcode::IntToPtr
            | Opcode::Bitcast
            | Opcode::Select
            | Opcode::ICmp
            | Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Trunc
            | Opcode::SExt
            | Opcode::ZExt
            | Opcode::Memcpy
            | Opcode::Memmove
            | Opcode::Memset
            | Opcode::Jump
            | Opcode::BrIf
            | Opcode::Switch
            | Opcode::CapOf
            | Opcode::CheckBounds
            | Opcode::CheckLive
            | Opcode::CheckType
            | Opcode::CheckInit
            | Opcode::CheckDeriv
            | Opcode::CheckRace
            | Opcode::CheckRestrictRead
            | Opcode::CheckRestrictWrite
            | Opcode::CheckFree
    )
}

/// What the tables say about a name with no body here, or nothing when neither of them names it.
fn table(name: &str) -> Option<Reach> {
    if never_frees(name) {
        return Some(Reach::NOTHING);
    }
    let bare = name.strip_prefix(WRAPPER_PREFIX).unwrap_or(name);
    let bare = bare.strip_prefix("__builtin_").unwrap_or(bare);
    let at = FREES_ITS_ARGUMENT.binary_search_by_key(&bare, |&(name, _)| name).ok()?;
    let (_, place) = FREES_ITS_ARGUMENT[at];
    Some(Reach { freed: Params::NONE.with(place), kept: Params::NONE, frees_other: false })
}

/// The functions outside this module that are known to end no lifetime.
///
/// Short on purpose, and the entries are the ones whose behaviour the C standard writes down. The
/// allocating ones are here because handing out new storage is not ending old storage, and
/// `realloc` is deliberately absent: it may free what it was given.
///
/// Sorted, and a test checks that it is sorted and says each name once.
const NEVER_FREES: &[&str] = &[
    "abs",
    "aligned_alloc",
    "bcopy",
    "bzero",
    "calloc",
    "imaxabs",
    "labs",
    "llabs",
    "malloc",
    "memchr",
    "memcmp",
    "memcpy",
    "memmove",
    "memset",
    "posix_memalign",
    "pread",
    "pwrite",
    "read",
    "readv",
    "recv",
    "send",
    "stpcpy",
    "strcat",
    "strchr",
    "strcmp",
    "strcpy",
    "strcspn",
    "strlen",
    "strncat",
    "strncmp",
    "strncpy",
    "strnlen",
    "strpbrk",
    "strrchr",
    "strspn",
    "strstr",
    "write",
    "writev",
];

/// The functions outside this module that end the lifetime of what they were handed and of nothing
/// else, and which argument each of them is handed it as.
///
/// Two names, and the bar for a third is [`NEVER_FREES`]'s: the standard has to say what the
/// function does. `free` and `realloc` are the two the standard says it about. Neither keeps the
/// pointer, since neither has anywhere to put it that outlives the call, and `realloc` hands back a
/// pointer to storage that is new or is the old storage moved, which is not the old pointer kept.
///
/// Sorted, and the test that checks the other two tables checks this one.
const FREES_ITS_ARGUMENT: &[(&str, usize)] = &[("free", 0), ("realloc", 0)];

/// What `rucc-safety` puts in front of the name of a function it interposes.
///
/// The same string as `rucc_safety::wrap::PREFIX`, written again here because `rucc-opt` and
/// `rucc-safety` are the same layer rank and neither can see the other. Repeating it is safe in
/// the direction that matters: if the two ever disagree, a wrapped call stops being recognised and
/// a check stays, which costs nothing but the check.
const WRAPPER_PREFIX: &str = "__rucc_wrap_";

/// The entry points of this compiler's own runtime that end no lifetime.
///
/// A different table from the one above because the bar is different, not because the answer is.
/// The C table takes a name only where the standard says what the function does, since nothing in
/// a build can see the body. These bodies are in this repository, in `runtime/rucc-safe-rt`, and
/// what each of them does is read the lifetime plane and then either return a number or report.
/// Reporting ends the program under `-fsafety=detect` and records under `-fsafety=recover`, and
/// neither of those hands storage back. Six of them write a plane rather than reading one, which is
/// memory of the runtime's own and not storage the program was ever given, and the four edges move a
/// clock that lives beside the thread, which is the same answer for the same reason.
///
/// These twenty and no more. The rest of what the runtime exports is the allocator's own bookkeeping,
/// `__rucc_alloc_purge` and the frame calls and the rest, and those are exactly the things that do
/// end a lifetime. Nothing generated calls them, so leaving them out costs nothing, and a name
/// added here without reading what it does would be a hole in the safety this compiler is for.
///
/// Sorted, and the same test that checks the table above checks this one.
const RUNTIME_NEVER_FREES: &[&str] = &[
    "__rucc_cap_copy",
    "__rucc_cap_witness",
    "__rucc_check_bounds",
    "__rucc_check_deriv",
    "__rucc_check_free",
    "__rucc_check_init",
    "__rucc_check_live",
    "__rucc_check_race",
    "__rucc_check_type",
    "__rucc_extent",
    "__rucc_extent_back",
    "__rucc_meta_acquire",
    "__rucc_meta_epoch",
    "__rucc_meta_fence_acquire",
    "__rucc_meta_fence_release",
    "__rucc_meta_init",
    "__rucc_meta_init_copy",
    "__rucc_meta_init_handed",
    "__rucc_meta_release",
    "__rucc_meta_type",
    "__rucc_meta_type_copy",
];

/// Whether either table vouches for that name, under any of the spellings it can arrive in.
///
/// `__builtin_memcpy` is the program saying which function it means. `__rucc_wrap_memcpy` is what
/// a call to `memcpy` becomes under `-fsafety`, and the wrapper checks the access and then calls
/// the function it wraps, so it ends whatever that one ends, which is nothing.
fn never_frees(name: &str) -> bool {
    if RUNTIME_NEVER_FREES.binary_search(&name).is_ok() {
        return true;
    }
    let name = name.strip_prefix(WRAPPER_PREFIX).unwrap_or(name);
    let name = name.strip_prefix("__builtin_").unwrap_or(name);
    NEVER_FREES.binary_search(&name).is_ok()
}

#[cfg(test)]
mod tests {
    use rucc_base::{Interner, Symbol};
    use rucc_ir::{
        Builder, CallInfo, Extra, Flags, Func, InstData, Linkage, MemInfo, MemOrder, Module,
        Opcode, Pic, Restrict, Sig, Signature, Type, Value, Visibility,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{
        FREES_ITS_ARGUMENT, NEVER_FREES, Params, RUNTIME_NEVER_FREES, Reach, Summaries, annotate,
    };

    /// What one function in a test module is.
    struct Def<'a> {
        /// Its name.
        name: &'a str,
        /// Whether it has a body. A function without one is a declaration.
        defined: bool,
        /// The functions it calls, in order.
        calls: &'a [&'a str],
    }

    /// A definition that calls those names.
    fn defines<'a>(name: &'a str, calls: &'a [&'a str]) -> Def<'a> {
        Def { name, defined: true, calls }
    }

    /// A declaration, which has no body and so calls nothing.
    fn declares(name: &str) -> Def<'_> {
        Def { name, defined: false, calls: &[] }
    }

    /// A module holding those functions.
    ///
    /// Every one of them takes a pointer and hands it to everything it calls, which is what a call
    /// to `free` has to look like for the freed parameter summary to have anything to say about it.
    /// A body that called `free` with no arguments would be right to read as freeing nothing, and
    /// there is no such body in any C anybody wrote.
    fn module(defs: &[Def<'_>]) -> (Interner, Module) {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        for def in defs {
            let mut func =
                Func::new(names.intern(def.name), Signature::new().with_params(&[Type::PTR]));
            if def.defined {
                let block = func.create_block();
                let held = func.append_param(block, Type::PTR);
                let mut build = Builder::new(&mut func, block);
                let signature =
                    build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
                for call in def.calls {
                    build.call(names.intern(call), signature, &[held]);
                }
                build.ret(&[]);
            }
            module.add_func(func);
        }
        (names, module)
    }

    /// Whether the summaries say that name cannot free.
    fn cannot_free(names: &mut Interner, module: &Module, name: &str) -> bool {
        let summaries = Summaries::of_module(module, names, Pic::Executable);
        summaries.cannot_free(names.intern(name))
    }

    /// Every call in the module that carries the flag, by the name it calls.
    fn marked(names: &Interner, module: &Module) -> Vec<String> {
        let mut found = Vec::new();
        for id in module.funcs() {
            let func = &module[id];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    if !func[inst].flags.contains(Flags::NOFREE) {
                        continue;
                    }
                    let Extra::Call(at) = func[inst].extra else { continue };
                    let Some(callee) = func[at].callee else { continue };
                    found.push(names.resolve(callee).to_string());
                }
            }
        }
        found
    }

    #[test]
    fn a_function_that_calls_nothing_frees_nothing() {
        let (mut names, module) = module(&[defines("leaf", &[])]);
        assert!(cannot_free(&mut names, &module, "leaf"));
    }

    #[test]
    fn a_function_that_calls_free_can_free_and_so_can_its_callers() {
        let (mut names, module) = module(&[
            declares("free"),
            defines("releases", &["free"]),
            defines("above", &["releases"]),
        ]);
        assert!(!cannot_free(&mut names, &module, "free"));
        assert!(!cannot_free(&mut names, &module, "releases"));
        assert!(!cannot_free(&mut names, &module, "above"));
    }

    #[test]
    fn a_function_that_only_calls_nofree_ones_frees_nothing() {
        let (mut names, module) = module(&[
            declares("memcpy"),
            defines("leaf", &[]),
            defines("above", &["leaf", "memcpy"]),
        ]);
        assert!(cannot_free(&mut names, &module, "above"));
    }

    #[test]
    fn the_table_is_asked_about_a_name_the_module_has_no_function_for() {
        // Which is what a call `rucc-safety` put in looks like. It interned the name and emitted
        // the call, and there is no declaration anywhere in the module to go with it.
        let (mut names, module) = module(&[defines("above", &["__rucc_wrap_memcpy"])]);
        assert!(cannot_free(&mut names, &module, "__rucc_wrap_memcpy"));
        assert!(cannot_free(&mut names, &module, "above"));
    }

    #[test]
    fn a_name_the_module_has_no_function_for_and_the_table_does_not_know_can_still_free() {
        let (mut names, module) = module(&[defines("above", &["somebodys_free"])]);
        assert!(!cannot_free(&mut names, &module, "somebodys_free"));
        assert!(!cannot_free(&mut names, &module, "above"));
    }

    #[test]
    fn two_functions_that_call_each_other_and_free_nothing_are_both_nofree() {
        // Which is what starting optimistic and narrowing buys. Each waits on the other, so an
        // analysis that only ever added to the set would never put either of them in it.
        let (mut names, module) = module(&[defines("ping", &["pong"]), defines("pong", &["ping"])]);
        assert!(cannot_free(&mut names, &module, "ping"));
        assert!(cannot_free(&mut names, &module, "pong"));
    }

    #[test]
    fn a_cycle_with_a_free_anywhere_in_it_is_nofree_nowhere() {
        let (mut names, module) = module(&[
            declares("free"),
            defines("ping", &["pong"]),
            defines("pong", &["ping", "free"]),
        ]);
        assert!(!cannot_free(&mut names, &module, "ping"));
        assert!(!cannot_free(&mut names, &module, "pong"));
    }

    #[test]
    fn a_name_this_module_never_heard_of_can_free() {
        let (mut names, module) = module(&[defines("leaf", &[])]);
        assert!(!cannot_free(&mut names, &module, "elsewhere"));
    }

    #[test]
    fn the_library_table_is_read_under_every_spelling_a_name_arrives_in() {
        let (mut names, module) = module(&[
            declares("memcpy"),
            declares("__builtin_memcpy"),
            declares("__rucc_wrap_memcpy"),
            declares("realloc"),
        ]);
        assert!(cannot_free(&mut names, &module, "memcpy"));
        assert!(cannot_free(&mut names, &module, "__builtin_memcpy"));
        assert!(cannot_free(&mut names, &module, "__rucc_wrap_memcpy"));
        // `realloc` is the one that looks like the others and is not. It may free what it was
        // given, which is exactly the event the flag is about.
        assert!(!cannot_free(&mut names, &module, "realloc"));
    }

    #[test]
    fn a_definition_the_linker_may_replace_is_not_believed() {
        let (mut names, mut module) = module(&[defines("weakly", &[])]);
        let id = module.funcs().next().unwrap();
        module[id].linkage = Linkage::Weak;
        assert!(!cannot_free(&mut names, &module, "weakly"));
    }

    /// The whole of what `-fPIC` costs this analysis. A body it can read is one the dynamic linker
    /// may find a different definition of first, so the one in front of it is not the one that
    /// runs and nothing may be read off it.
    #[test]
    fn a_library_cannot_believe_a_body_something_else_may_replace() {
        let (mut names, module) = module(&[defines("exported", &[])]);
        let name = names.intern("exported");
        assert!(Summaries::of_module(&module, &names, Pic::Executable).cannot_free(name));
        assert!(!Summaries::of_module(&module, &names, Pic::Library).cannot_free(name));
    }

    /// And what it costs for a name nothing outside the library can reach, which is nothing. Both
    /// halves matter: the first is why `-fvisibility=hidden` is worth writing and the second is
    /// why a `static` helper is still believed in a library.
    #[test]
    fn a_library_believes_the_bodies_nothing_outside_it_can_name() {
        let (mut names, mut module) = module(&[defines("shy", &[]), defines("quiet", &[])]);
        let mut ids = module.funcs();
        let shy = ids.next().unwrap();
        let quiet = ids.next().unwrap();
        module[shy].visibility = Visibility::Hidden;
        module[quiet].linkage = Linkage::Internal;
        let summaries = Summaries::of_module(&module, &names, Pic::Library);
        assert!(summaries.cannot_free(names.intern("shy")));
        assert!(summaries.cannot_free(names.intern("quiet")));
    }

    #[test]
    fn a_call_through_an_address_could_reach_anything() {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let mut func = Func::new(names.intern("dispatch"), Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(Signature::new());
        let varargs = build.func().push_abis(&[]);
        let info = build.func().add_call(CallInfo { callee: None, signature, varargs });
        build.inst(
            InstData { extra: Extra::Call(info), ..InstData::new(Opcode::CallIndirect) },
            &[],
        );
        build.ret(&[]);
        module.add_func(func);
        assert!(!cannot_free(&mut names, &module, "dispatch"));
    }

    #[test]
    fn the_flag_goes_on_the_calls_the_summaries_vouch_for_and_no_others() {
        let (names, mut module) = module(&[
            declares("free"),
            declares("memcpy"),
            defines("leaf", &[]),
            defines("above", &["leaf", "memcpy", "free"]),
        ]);
        assert_eq!(annotate(&mut module, &names, Pic::Executable), 2);
        assert_eq!(marked(&names, &module), ["leaf", "memcpy"]);
        // Running it again finds nothing left to say, which is what makes it safe to run in a
        // pipeline that has already been through it once.
        assert_eq!(annotate(&mut module, &names, Pic::Executable), 0);
        assert_eq!(marked(&names, &module).len(), 2);
    }

    /// The plain word of memory these tests read and write pointers through.
    fn word() -> MemInfo {
        MemInfo {
            size: 8,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        }
    }

    /// What the summaries say about a definition named `holder` that takes one pointer and has
    /// whatever body the caller writes.
    ///
    /// `free` is declared alongside it, since most of what these ask about is what happens when the
    /// pointer reaches it. The builder is handed the parameter and a signature taking one pointer.
    fn reach_of(body: impl FnOnce(&mut Builder<'_>, Value, Sig, &mut Interner)) -> Reach {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        module
            .add_func(Func::new(names.intern("free"), Signature::new().with_params(&[Type::PTR])));
        let one = Signature::new().with_params(&[Type::PTR]);
        let mut func = Func::new(names.intern("holder"), one);
        let block = func.create_block();
        let held = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        body(&mut build, held, signature, &mut names);
        module.add_func(func);
        let holder = names.intern("holder");
        Summaries::of_module(&module, &names, Pic::Executable).reach(holder)
    }

    #[test]
    fn a_set_of_parameters_holds_the_ones_put_in_it() {
        assert!(Params::NONE.is_empty());
        assert!(!Params::NONE.contains(0));
        assert!(Params::NONE.with(3).contains(3));
        assert!(!Params::NONE.with(3).contains(4));
        assert!(Params::ALL.contains(0));
        // Past the sixty fourth position there is one bit for all of them, and a function that
        // wide is answered yes rather than no, which costs a check rather than losing one.
        assert!(!Params::NONE.with(3).contains(200));
        assert!(Params::NONE.with(200).contains(201));
        assert!(Params::ALL.contains(200));
    }

    #[test]
    fn the_parameter_handed_to_free_is_the_one_reported_as_freed() {
        let reach = reach_of(|build, held, signature, names| {
            build.call(names.intern("free"), signature, &[held]);
            build.ret(&[]);
        });
        assert!(reach.freed.contains(0), "the parameter it was handed");
        assert!(!reach.frees_other, "and nothing else");
        assert!(reach.kept.is_empty(), "free keeps nothing it is handed");
        assert!(!reach.frees_nothing());
    }

    #[test]
    fn a_displacement_off_the_parameter_is_still_the_parameter() {
        // `free(&p->tail)` is a program doing something it should not, and what it frees is the
        // storage the caller handed over rather than some storage of its own.
        let reach = reach_of(|build, held, signature, names| {
            let by = build.iconst(Type::int(64), 8);
            let at = build.binary(Opcode::PtrAdd, held, by, Flags::NONE);
            build.call(names.intern("free"), signature, &[at]);
            build.ret(&[]);
        });
        assert!(reach.freed.contains(0));
        assert!(!reach.frees_other);
    }

    #[test]
    fn freeing_something_read_out_of_the_parameter_is_freeing_something_else() {
        // `free(p->next)` ends a lifetime that is not `p`'s, and this has no name for whose it is,
        // so it is reported as reaching past the parameters. That is the answer that costs a check.
        let reach = reach_of(|build, held, signature, names| {
            let next = build.load(Type::PTR, held, word(), Flags::NONE);
            build.call(names.intern("free"), signature, &[next]);
            build.ret(&[]);
        });
        assert!(reach.freed.is_empty(), "not the parameter");
        assert!(reach.frees_other, "something this cannot name");
        assert!(!reach.frees_nothing());
    }

    #[test]
    fn a_parameter_written_into_memory_is_one_the_call_kept() {
        let reach = reach_of(|build, held, _signature, _names| {
            let slot = build.load(Type::PTR, held, word(), Flags::NONE);
            build.store(held, slot, word(), Flags::NONE);
            build.ret(&[]);
        });
        assert!(reach.kept.contains(0), "it is somewhere a later call can find it");
        assert!(reach.frees_nothing(), "and nothing was freed doing it");
    }

    #[test]
    fn writing_through_a_parameter_is_not_keeping_it() {
        // The whole of why the value and not the address. `sqlite3VdbeAddOp2(Vdbe *p, ...)` writes
        // into `p` all day and leaves it nowhere, and a summary that could not tell the two apart
        // would say every function keeps every pointer it touches.
        let reach = reach_of(|build, held, _signature, _names| {
            let zero = build.iconst(Type::int(64), 0);
            let value = build.unary(Opcode::IntToPtr, zero, Type::PTR);
            build.store(value, held, word(), Flags::NONE);
            build.ret(&[]);
        });
        assert!(reach.kept.is_empty());
        assert!(reach.frees_nothing());
    }

    #[test]
    fn a_parameter_handed_back_is_one_the_call_kept() {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let mut func = Func::new(
            names.intern("holder"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[Type::PTR]),
        );
        let block = func.create_block();
        let held = func.append_param(block, Type::PTR);
        Builder::new(&mut func, block).ret(&[held]);
        module.add_func(func);
        let holder = names.intern("holder");
        let reach = Summaries::of_module(&module, &names, Pic::Executable).reach(holder);
        assert!(reach.kept.contains(0), "the caller is not the only one holding it now");
        assert!(reach.frees_nothing());
    }

    #[test]
    fn a_call_through_an_address_keeps_every_pointer_it_was_handed() {
        let reach = reach_of(|build, held, signature, _names| {
            let varargs = build.func().push_abis(&[]);
            let info = build.func().add_call(CallInfo { callee: None, signature, varargs });
            let args = build.func().push_values(&[held]);
            build.inst(
                InstData { extra: Extra::Call(info), args, ..InstData::new(Opcode::CallIndirect) },
                &[],
            );
            build.ret(&[]);
        });
        assert!(reach.kept.contains(0));
        assert!(reach.frees_other);
    }

    #[test]
    fn a_name_the_tables_vouch_for_keeps_nothing_it_is_handed() {
        // Which is the claim the module comment adds to `NEVER_FREES`, and the one that makes a
        // pointer handed to `memcpy` still a pointer nothing else can reach.
        let (mut names, module) = module(&[declares("memcpy")]);
        let memcpy = names.intern("memcpy");
        let reach = Summaries::of_module(&module, &names, Pic::Executable).reach(memcpy);
        assert!(reach.kept.is_empty());
        assert!(reach.freed.is_empty());
        assert!(!reach.frees_other);
    }

    #[test]
    fn a_name_nothing_is_known_about_could_have_done_anything() {
        let (mut names, module) = module(&[declares("elsewhere")]);
        let elsewhere = names.intern("elsewhere");
        let reach = Summaries::of_module(&module, &names, Pic::Executable).reach(elsewhere);
        assert_eq!(reach, Reach::UNKNOWN);
    }

    #[test]
    fn a_call_that_hands_no_pointer_to_a_freeing_position_ends_nothing() {
        // What the site adds to the callee. `holder` frees its first parameter, and a call that
        // puts a number there frees nothing whatever the callee does with a pointer.
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        module
            .add_func(Func::new(names.intern("free"), Signature::new().with_params(&[Type::PTR])));
        let mut holder =
            Func::new(names.intern("holder"), Signature::new().with_params(&[Type::PTR]));
        let block = holder.create_block();
        let held = holder.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut holder, block);
        let taking_a_pointer =
            build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        build.call(names.intern("free"), taking_a_pointer, &[held]);
        build.ret(&[]);
        module.add_func(holder);

        let mut above = Func::new(names.intern("above"), Signature::new());
        let block = above.create_block();
        let mut build = Builder::new(&mut above, block);
        let taking_a_number =
            build.func().add_signature(Signature::new().with_params(&[Type::int(32)]));
        let number = build.iconst(Type::int(32), 7);
        build.call(names.intern("holder"), taking_a_number, &[number]);
        build.ret(&[]);
        module.add_func(above);

        assert_eq!(annotate(&mut module, &names, Pic::Executable), 1);
        assert_eq!(marked(&names, &module), ["holder"]);
    }

    #[test]
    fn nothing_is_known_when_nothing_was_asked() {
        let empty = Summaries::nothing();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(!empty.cannot_free(Symbol::from_raw(0)));
    }

    #[test]
    fn the_library_table_is_sorted_and_says_each_name_once() {
        // Sorted because the lookup is a binary search, and each name once because an entry here
        // is believed without being checked against anything.
        for pair in NEVER_FREES.windows(2) {
            assert!(pair[0] < pair[1], "{} and {} are out of order", pair[0], pair[1]);
        }
        for &name in NEVER_FREES {
            assert!(!name.starts_with("__builtin_"), "{name} is reached under every spelling");
            assert!(!name.starts_with(super::WRAPPER_PREFIX), "{name} likewise");
        }
        for pair in RUNTIME_NEVER_FREES.windows(2) {
            assert!(pair[0] < pair[1], "{} and {} are out of order", pair[0], pair[1]);
        }
        for pair in FREES_ITS_ARGUMENT.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} and {} are out of order", pair[0].0, pair[1].0);
        }
        // And the two tables have to disagree about nothing, since the first one asked wins and a
        // name in both would be answered by whichever that is rather than by anybody's intent.
        for &(name, _) in FREES_ITS_ARGUMENT {
            assert!(!super::never_frees(name), "{name} is in both tables");
        }
        // The four nobody should be tempted to add, written down so that adding one is a test
        // failure rather than a decision somebody makes alone. The last two are the runtime's own
        // and are the ones that do end a lifetime.
        assert!(!super::never_frees("realloc"));
        assert!(!super::never_frees("free"));
        assert!(!super::never_frees("__rucc_alloc_purge"));
        assert!(!super::never_frees("__rucc_frame_clear"));
    }

    #[test]
    fn the_runtime_entry_points_generated_code_calls_end_no_lifetime() {
        // The witness is the one a compile actually puts in front of the optimizer, at every place
        // a pointer crosses the instrumentation boundary, and it has no function in the module.
        let (mut names, module) = module(&[defines("above", &["__rucc_cap_witness"])]);
        assert!(cannot_free(&mut names, &module, "above"));
        for &name in RUNTIME_NEVER_FREES {
            assert!(super::never_frees(name), "{name} is in the table and not read from it");
        }
    }
}
