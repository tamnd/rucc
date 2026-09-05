//! Alias analysis: whether two memory references can touch the same byte.
//!
//! Design: `spec/optimizer/08-alias-analysis.md`, with the switch section 41.9 of
//! `spec/optimizer/41-correctness.md` asks for.
//!
//! # The one question
//!
//! There is one primitive and everything else is built on it. Given two memory references, can
//! they touch the same byte. Every memory optimization documents 16, 17 and 27 will bring is
//! gated on it, an answer that is too conservative costs performance quietly and forever, and an
//! answer that is too aggressive miscompiles in the way that produces a bug report three years
//! later from somebody whose program worked on every other compiler.
//!
//! So the answer is an [`Answer`], which is either [`Answer::May`] or a no carrying the layer
//! that concluded it. Section 8.5 asks for that and it is the best decision in spec 9.4: a
//! miscompilation from an alias bug is localised to one layer rather than bisected across the
//! whole analysis, the layer statistics come for free, and a user asking why something was not
//! optimized gets a real answer. It costs one byte in a return value that was going in a
//! register anyway.
//!
//! # The layers, in the order they run
//!
//! Section 8.2 lists six. This is the first five, and the order they run in is load bearing.
//!
//! **Two volatile accesses conflict**, and that is checked before anything else. Not may
//! conflict: they are treated as conflicting so that neither can be moved across the other,
//! which is what `volatile` is for.
//!
//! **Distinct storage and provenance**, layers 1 and 2, are one walk here because the IR names
//! the object a pointer came from. [`origin`] chases a pointer back through `ptr_add` and
//! `bitcast` to the `alloca` or the `global_addr` it started at, and two different objects never
//! alias. GCC gets the same answer less directly, out of tracking base declarations through a
//! tree walk. This layer answers a startling fraction of the queries real code asks and it is
//! the only one `-O1` needs.
//!
//! **Offsets**, layer 4, run next and only for two references to the same object, and running
//! them before the type-based layer rather than after is the whole of what makes union type
//! punning work. Writing through one member of a union and reading another is two accesses to
//! one object at overlapping offsets with unrelated types. It is undefined in ISO C, it is
//! defined by GCC, an enormous amount of real C rests on it, and a layer that asked about the
//! types first would answer no and miscompile all of it. GCC's comment at
//! `gcc/tree-ssa-alias.cc:2461` says exactly this and rucc reproduces the ordering rather than
//! the accident.
//!
//! **Escape**, which section 8.4 counts as the cheapest interprocedural-flavoured fact there is:
//! a local whose address never leaves the function is not the object some pointer this function
//! cannot follow is pointing at, and it is not one a call can touch either.
//!
//! **`restrict`**, layer 5, is two small numbers on the access and one comparison, which is all
//! GCC's is. See [`rucc_ir::Restrict`], including the trap.
//!
//! **Type-based aliasing**, layer 3, runs last of the five. Two accesses conflict when one of
//! their type nodes is at or above the other in the metadata tree, so an access through `char`,
//! whose node is the root, conflicts with everything. `-fno-strict-aliasing` is one condition in
//! one place, [`Options::strict_aliasing`], which is what section 41.9 means by the flag having
//! to actually work.
//!
//! Layer 6 is points-to, and it is not here. It is a module-wide fixed point rather than a fact
//! the IR already carries, section 8.3 has an open question about which solver it should be, and
//! section 8.6 is emphatic that provenance and points-to are different things that must not be
//! confused. So [`Origin`] is provenance, there is no points-to type for it to be converted
//! into, and the solver lands separately with the constraint generator split out from it the way
//! GCC 16 split its own.
//!
//! # What the front end still owes this
//!
//! Layers 3 and 5 read fields the front end fills in during lowering, and lowering does not fill
//! them in yet: every access carries no type node and no `restrict` clique today. The layers are
//! here, they are tested, and they answer correctly for the accesses that do carry them. Giving
//! them something to read is the next piece of work, and section 8.2 says what it has to be
//! careful about, which is that an alias set is derived from a canonical encoding of the type
//! and never from allocation order, or document 35's LTO silently gains disambiguations.

use std::collections::HashSet;

use rucc_base::Symbol;
use rucc_ir::{
    AttrSet, Attrs, DataLayout, Def, Extra, Flags, Func, Imm, Inst, MemInfo, Meta, Module, Opcode,
    Restrict, SymbolRef, Type, Value,
};

/// How far back through address arithmetic a pointer is chased before the answer is given up on.
///
/// The chain from an `alloca` to the address a load uses is two or three instructions in
/// anything a person writes. The limit is here so that a generated function with a thousand
/// `ptr_add`s in a row costs a bounded amount, and giving up produces an unknown origin, which
/// is the conservative answer rather than a wrong one.
const CHASE_LIMIT: u32 = 64;

/// How far up the metadata tree a type node is followed.
///
/// The tree is shallow, and the verifier is what would catch one that is not a tree at all. The
/// limit means a query cannot fail to terminate even on a module that came from somewhere the
/// verifier has not run.
const TREE_LIMIT: u32 = 32;

/// Which rule concluded that two references cannot touch the same byte.
///
/// Section 8.5. This is the whole point of the return type being an enum rather than a boolean.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    /// They are references to two different objects, which is layers 1 and 2 together.
    Distinct,
    /// One is a local whose address never leaves the function and the other is not that local.
    Escape,
    /// They are references to one object at offsets whose byte ranges do not overlap.
    Offset,
    /// Their type nodes are in different parts of the tree, so no object has both types.
    Tbaa,
    /// They are in one `restrict` scope through different `restrict` pointers.
    Restrict,
    /// The callee's attributes say it does not touch memory this way.
    Attribute,
}

impl Reason {
    /// Every reason, which is what a report walks.
    pub const ALL: [Self; 6] =
        [Self::Distinct, Self::Escape, Self::Offset, Self::Tbaa, Self::Restrict, Self::Attribute];

    /// How many there are, which is the width of a [`Counts`].
    pub const COUNT: usize = Self::ALL.len();

    /// Where this sits in [`Reason::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Distinct => 0,
            Self::Escape => 1,
            Self::Offset => 2,
            Self::Tbaa => 3,
            Self::Restrict => 4,
            Self::Attribute => 5,
        }
    }

    /// The one word `-fdump-alias` prints for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Distinct => "distinct",
            Self::Escape => "escape",
            Self::Offset => "offset",
            Self::Tbaa => "tbaa",
            Self::Restrict => "restrict",
            Self::Attribute => "attribute",
        }
    }

    /// The sentence a user gets when they ask why something was not optimized.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Distinct => "they are two different objects",
            Self::Escape => "the address of that local never leaves this function",
            Self::Offset => "they are parts of one object that do not overlap",
            Self::Tbaa => "no object has both of those types",
            Self::Restrict => "restrict says those two pointers do not reach the same object",
            Self::Attribute => "the callee is declared not to touch memory that way",
        }
    }
}

/// What the analysis answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Answer {
    /// They may touch the same byte, which is the answer whenever nothing proved otherwise.
    May,
    /// They cannot, and this is the rule that says so.
    No(Reason),
}

impl Answer {
    /// Whether this is a no.
    #[must_use]
    pub const fn is_no(self) -> bool {
        matches!(self, Self::No(_))
    }

    /// The rule behind a no.
    #[must_use]
    pub const fn reason(self) -> Option<Reason> {
        match self {
            Self::No(reason) => Some(reason),
            Self::May => None,
        }
    }
}

/// What the command line turns off.
///
/// One field, because there is one flag. Section 41.9 asks that `-fno-strict-aliasing` disable
/// the type-based component and nothing else, exactly as `gcc/alias.cc:420` and :556 do, and the
/// way to make that true rather than hoped for is to have one condition in one place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Whether the type-based layer is consulted. GCC's default at `-O2` is on and rucc matches
    /// it, so `-fno-strict-aliasing` is what clears this.
    pub strict_aliasing: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { strict_aliasing: true }
    }
}

/// Where a pointer came from, as far as this function can tell.
///
/// This is provenance and it is not points-to. Provenance says which object a pointer was
/// derived from, which the IR knows locally and cheaply. Points-to says which objects a pointer
/// might hold at run time, which needs a module-wide fixed point. Section 8.6 lists confusing
/// the two as one of the ways this analysis goes wrong, so there is no conversion between them
/// and there is no points-to type here at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// An `alloca` in this function, which is storage nothing outside it knew about until the
    /// address was handed out.
    Local(Inst),
    /// A named object, by the symbol its address was taken by.
    Global(Symbol),
    /// An address this function cannot follow any further back: a parameter, something loaded
    /// out of memory, what a call returned, or an integer turned into a pointer.
    Unknown(Value),
}

impl Origin {
    /// Whether this names an object rather than an address of unknown origin.
    #[must_use]
    pub const fn is_object(self) -> bool {
        matches!(self, Self::Local(_) | Self::Global(_))
    }
}

/// Where a pointer came from, and how many bytes past the start of it the pointer is.
///
/// The offset is `None` when the walk passed arithmetic whose amount is not a constant, which
/// costs the offset layer and nothing else: the origin is still the origin, because adding an
/// unknown number of bytes to a pointer does not move it to a different object.
#[must_use]
pub fn origin(func: &Func, mut value: Value) -> (Origin, Option<i64>) {
    let mut offset = Some(0i64);
    for _ in 0..CHASE_LIMIT {
        let Def::Result { inst, .. } = func[value].def else {
            // A block parameter, which is where the address arrived from somewhere else.
            return (Origin::Unknown(value), offset);
        };
        let data = func[inst];
        match data.opcode {
            Opcode::Alloca => return (Origin::Local(inst), offset),
            Opcode::GlobalAddr => {
                let Extra::Symbol(name) = data.extra else {
                    return (Origin::Unknown(value), offset);
                };
                return (Origin::Global(name), offset);
            }
            Opcode::PtrAdd => {
                let args = &func[data.args];
                let (base, by) = (args[0], args[1]);
                offset = offset
                    .and_then(|so_far| Some((so_far, constant(func, by)?)))
                    .and_then(|(so_far, by)| so_far.checked_add(by));
                value = base;
            }
            // A cast between two pointers moves nothing, so it is the same address as its
            // operand and the walk goes through it.
            Opcode::Bitcast => value = func[data.args][0],
            _ => return (Origin::Unknown(value), offset),
        }
    }
    (Origin::Unknown(value), None)
}

/// One memory reference: which bytes an instruction touches and what it says about them.
///
/// Built by [`Alias::reads`] and [`Alias::writes`] rather than by hand, so that the size of a
/// load comes from the type it produces and the size of a `memcpy` comes from its access, and no
/// caller has to remember which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    /// The object, or the address the walk stopped at.
    pub origin: Origin,
    /// How many bytes past the start of that the reference begins, when the walk could tell.
    pub offset: Option<i64>,
    /// How many bytes it covers, when that is known.
    pub size: Option<u64>,
    /// The type node the front end attached, if it attached one.
    pub tbaa: Option<Meta>,
    /// The `restrict` scope the access is in.
    pub restrict: Restrict,
    /// Whether the access is `volatile`.
    pub volatile: bool,
}

impl Access {
    /// A reference to somewhere behind this address, of unknown size and with nothing known
    /// about its type.
    ///
    /// This is what a pointer handed to a call is: the call touches something through it and
    /// there is nothing on the call saying how much.
    #[must_use]
    pub fn through(func: &Func, pointer: Value) -> Self {
        let (origin, offset) = origin(func, pointer);
        Self { origin, offset, size: None, tbaa: None, restrict: Restrict::NONE, volatile: false }
    }

    /// The half-open range of bytes this covers within its origin, when both ends are known.
    #[must_use]
    pub fn range(&self) -> Option<(i128, i128)> {
        let (offset, size) = (self.offset?, self.size?);
        let start = i128::from(offset);
        Some((start, start + i128::from(size)))
    }
}

/// Which of a function's locals had their address leave it.
///
/// Section 8.4 calls this the most valuable interprocedural-flavoured fact available without
/// interprocedural analysis, because it covers every local a C programmer takes the address of
/// only to pass one field of, and because a local whose address never escaped cannot be touched
/// by any call at all.
///
/// Section 8.6 says how it goes wrong, which is by missing an escape, and what to do about it.
/// [`keeps_address`] is a whitelist: an opcode it does not name lets the address out, and so
/// does an opcode added to the IR after this was written. A blacklist would mean the next person
/// to add an opcode introduces a miscompilation without touching this file.
#[derive(Clone, Debug, Default)]
pub struct Escapes {
    escaped: HashSet<Inst>,
}

impl Escapes {
    /// Works out which locals of this function escaped it.
    #[must_use]
    pub fn of(func: &Func) -> Self {
        let mut escaped = HashSet::new();
        for block in func.blocks() {
            for inst in func.insts(block) {
                let data = func[inst];
                for (index, &arg) in func[data.args].iter().enumerate() {
                    if keeps_address(data.opcode, index) {
                        continue;
                    }
                    if let (Origin::Local(local), _) = origin(func, arg) {
                        escaped.insert(local);
                    }
                }
                // What a branch passes to a block parameter, which is where an address stops
                // being one this function can follow back to anything.
                for call in func.successors(inst) {
                    for &arg in &func[call.args] {
                        if let (Origin::Local(local), _) = origin(func, arg) {
                            escaped.insert(local);
                        }
                    }
                }
            }
        }
        Self { escaped }
    }

    /// Whether the address of this `alloca` left the function.
    #[must_use]
    pub fn escaped(&self, local: Inst) -> bool {
        self.escaped.contains(&local)
    }

    /// How many locals escaped.
    #[must_use]
    pub fn count(&self) -> usize {
        self.escaped.len()
    }
}

/// Whether a use of a pointer at this operand leaves the address inside the function.
///
/// A whitelist, per section 8.6, and the reason it is written this way is in [`Escapes`].
#[must_use]
pub const fn keeps_address(opcode: Opcode, index: usize) -> bool {
    match (opcode, index) {
        // Dereferenced, and the address itself goes nowhere.
        (Opcode::Load | Opcode::AtomicLoad, 0)
        | (Opcode::Store | Opcode::AtomicStore, 1)
        | (Opcode::AtomicRmw | Opcode::Cmpxchg, 0)
        | (Opcode::Memcpy | Opcode::Memmove, 0 | 1)
        | (Opcode::Memset | Opcode::Prefetch, 0) => true,
        // Copied, and the copy's own uses are walked in their turn, because the walk in
        // [`origin`] goes back through both of these.
        (Opcode::PtrAdd | Opcode::Bitcast, 0) => true,
        // Comparing two addresses neither reads them nor keeps them. Note that the answer may
        // not travel the other way: see [`rucc_ir::Restrict::disjoint`].
        (Opcode::ICmp, 0 | 1) => true,
        _ => false,
    }
}

/// How many queries each layer answered.
///
/// Section 8.5 says these come for free once the answer carries its reason, and section 8.3
/// wants them, because a layer that answers no on almost nothing is a layer to delete rather
/// than a layer to improve.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    queries: u64,
    answered: [u64; Reason::COUNT],
}

impl Counts {
    /// How many queries were asked.
    #[must_use]
    pub const fn queries(&self) -> u64 {
        self.queries
    }

    /// How many of them this layer answered no.
    #[must_use]
    pub const fn answered(&self, reason: Reason) -> u64 {
        self.answered[reason.index()]
    }

    /// How many were answered no by any layer.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.answered.iter().sum()
    }
}

/// The analysis over one function.
///
/// It borrows the module because a global's address is a symbol and whether two symbols are two
/// objects is a question about the module, and it borrows the function because everything else
/// is. The escape analysis is run once when this is built, since every query may ask it and it
/// is one walk over the function.
#[derive(Debug)]
pub struct Alias<'a> {
    func: &'a Func,
    module: &'a Module,
    options: Options,
    escapes: Escapes,
    counts: Counts,
}

impl<'a> Alias<'a> {
    /// The analysis of this function, with the type-based layer on, which is GCC's `-O2`.
    #[must_use]
    pub fn new(func: &'a Func, module: &'a Module) -> Self {
        Self::with(func, module, Options::default())
    }

    /// The same, with the type-based layer where the command line left it.
    #[must_use]
    pub fn with(func: &'a Func, module: &'a Module, options: Options) -> Self {
        Self { func, module, options, escapes: Escapes::of(func), counts: Counts::default() }
    }

    /// Which locals escaped, for a caller that wants the fact on its own.
    #[must_use]
    pub const fn escapes(&self) -> &Escapes {
        &self.escapes
    }

    /// What each layer has answered so far.
    #[must_use]
    pub const fn counts(&self) -> &Counts {
        &self.counts
    }

    /// The bytes this instruction reads, if it reads any.
    #[must_use]
    pub fn reads(&self, inst: Inst) -> Option<Access> {
        let data = self.func[inst];
        let args = &self.func[data.args];
        let info = self.mem(inst);
        let (pointer, size) = match data.opcode {
            Opcode::Load | Opcode::AtomicLoad => (args[0], self.width(self.result_type(inst)?)),
            // A copy reads its source, which is its second operand, for the size on the access.
            Opcode::Memcpy | Opcode::Memmove => (args[1], Some(info?.size)),
            // A read-modify-write reads and writes the same bytes, and the width is the width
            // of what it operates with.
            Opcode::AtomicRmw => (args[0], self.width(self.func[args[1]].ty)),
            Opcode::Cmpxchg => (args[0], self.width(self.func[args[1]].ty)),
            Opcode::VaObject => (args[0], Some(info?.size)),
            _ => return None,
        };
        Some(self.access(pointer, size, info, data.flags))
    }

    /// The bytes this instruction writes, if it writes any.
    #[must_use]
    pub fn writes(&self, inst: Inst) -> Option<Access> {
        let data = self.func[inst];
        let args = &self.func[data.args];
        let info = self.mem(inst);
        let (pointer, size) = match data.opcode {
            Opcode::Store | Opcode::AtomicStore => (args[1], self.width(self.func[args[0]].ty)),
            Opcode::Memcpy | Opcode::Memmove | Opcode::Memset => (args[0], Some(info?.size)),
            Opcode::AtomicRmw | Opcode::Cmpxchg => (args[0], self.width(self.func[args[1]].ty)),
            _ => return None,
        };
        Some(self.access(pointer, size, info, data.flags))
    }

    /// Whether these two references can touch the same byte.
    pub fn query(&mut self, a: &Access, b: &Access) -> Answer {
        self.counts.queries += 1;
        let answer = self.decide(a, b);
        if let Answer::No(reason) = answer {
            self.counts.answered[reason.index()] += 1;
        }
        answer
    }

    /// Whether this call can write the bytes the reference covers.
    ///
    /// GCC's `call_may_clobber_ref_p_1`. Without interprocedural summaries the honest answer for
    /// anything whose address escaped is yes, and section 8.4 says so plainly: the full mod and
    /// ref summary is `ipa-modref`, it is five and a half thousand lines, and it is document
    /// 34's. What is here is the cheap part of it, which is the attributes a C programmer
    /// already wrote and the escape analysis.
    pub fn clobbered_by(&mut self, reference: &Access, call: Inst) -> Answer {
        self.touched_by(reference, call, true)
    }

    /// Whether this call can read them.
    ///
    /// GCC's `ref_maybe_used_by_call_p_1`, and the same argument as [`Alias::clobbered_by`].
    pub fn read_by(&mut self, reference: &Access, call: Inst) -> Answer {
        self.touched_by(reference, call, false)
    }

    // The layers.

    fn decide(&self, a: &Access, b: &Access) -> Answer {
        // Section 8.1, and it is first because every layer below would be glad to say no.
        // Treating two volatile accesses as conflicting is what stops either being moved across
        // the other, which is the whole of what `volatile` promises.
        if a.volatile && b.volatile {
            return Answer::May;
        }

        // Two objects this function can name. Different objects never alias, and for one object
        // the offsets settle it on their own.
        //
        // The type-based layer is deliberately not reached from here, and that ordering is what
        // makes union type punning work: writing one member and reading another is two accesses
        // to one object at overlapping offsets whose types are unrelated, and asking about the
        // types first would answer no.
        if a.origin.is_object() && b.origin.is_object() {
            if self.distinct(a.origin, b.origin) {
                return Answer::No(Reason::Distinct);
            }
            if a.origin == b.origin {
                return by_offset(a, b);
            }
            return Answer::May;
        }

        // A local whose address never left the function is not what an address this function
        // cannot follow is pointing at, whatever it is pointing at.
        if let Some(local) = self.private(a).or_else(|| self.private(b)) {
            let _ = local;
            return Answer::No(Reason::Escape);
        }

        if a.restrict.disjoint(b.restrict) {
            return Answer::No(Reason::Restrict);
        }

        if self.options.strict_aliasing
            && let (Some(one), Some(other)) = (a.tbaa, b.tbaa)
            && !self.types_conflict(one, other)
        {
            return Answer::No(Reason::Tbaa);
        }

        // Two references through one address this function cannot follow, at offsets it can.
        if a.origin == b.origin {
            return by_offset(a, b);
        }

        Answer::May
    }

    /// The local one of these is a reference to, when it is one nothing outside can reach and
    /// the other reference is not to it.
    fn private(&self, reference: &Access) -> Option<Inst> {
        match reference.origin {
            Origin::Local(local) if !self.escapes.escaped(local) => Some(local),
            _ => None,
        }
    }

    /// Whether these two origins are two objects.
    fn distinct(&self, a: Origin, b: Origin) -> bool {
        match (a, b) {
            (Origin::Local(one), Origin::Local(other)) => one != other,
            // Fresh storage this function made is not any named object.
            (Origin::Local(_), Origin::Global(_)) | (Origin::Global(_), Origin::Local(_)) => true,
            (Origin::Global(one), Origin::Global(other)) => {
                one != other && self.one_object(one) && self.one_object(other)
            }
            _ => false,
        }
    }

    /// Whether this symbol is a name for an object no other name in the module also names.
    ///
    /// An `alias` or an `ifunc` is exactly a second name for something, so two different symbols
    /// can be one object and the rule that two objects do not alias does not reach them. A name
    /// the module does not have at all is treated the same way, because something is wrong and
    /// the conservative answer is the one to be wrong in the direction of.
    fn one_object(&self, name: Symbol) -> bool {
        matches!(self.module.lookup(name), Some(SymbolRef::Func(_) | SymbolRef::Global(_)))
    }

    /// Whether two type nodes can describe the same byte.
    ///
    /// They can when one is at or above the other in the tree, which is what makes an access
    /// through `char` conflict with everything: `char`'s node is the root and every other node
    /// hangs below it. Two nodes in different parts of the tree describe no object in common.
    fn types_conflict(&self, one: Meta, other: Meta) -> bool {
        self.at_or_below(one, other) || self.at_or_below(other, one)
    }

    /// Whether `node` is `ancestor` or hangs below it.
    fn at_or_below(&self, mut node: Meta, ancestor: Meta) -> bool {
        for _ in 0..TREE_LIMIT {
            if node == ancestor {
                return true;
            }
            match self.module[node].parent {
                Some(up) => node = up,
                None => return false,
            }
        }
        // A tree deeper than the limit, or a cycle the verifier would have turned down. Either
        // way the answer that cannot be wrong is that they conflict.
        true
    }

    fn touched_by(&mut self, reference: &Access, call: Inst, writing: bool) -> Answer {
        self.counts.queries += 1;
        let answer = self.decide_call(reference, call, writing);
        if let Answer::No(reason) = answer {
            self.counts.answered[reason.index()] += 1;
        }
        answer
    }

    fn decide_call(&self, reference: &Access, call: Inst, writing: bool) -> Answer {
        // Everything a call reaches, it reaches through an address, and an object whose address
        // never left this function is not one it has. Reaching here means the address was not
        // handed to this call either, because that would have been an escape.
        if self.private(reference).is_some() {
            return Answer::No(Reason::Escape);
        }

        let Some(attrs) = self.callee(call) else {
            return Answer::May;
        };
        // `const` reads no memory and writes none. `pure` may read and does not write.
        if attrs.set.contains(AttrSet::READNONE)
            || (writing && attrs.set.contains(AttrSet::READONLY))
        {
            return Answer::No(Reason::Attribute);
        }

        // Touching nothing except through the pointers it was passed. Every one of those is a
        // reference of its own, and if none of them can reach these bytes then neither can the
        // call. The reading is the non-transitive one the attribute's own documentation gives,
        // which is what makes this sound without a points-to solver behind it: what the callee
        // may reach by following a pointer it found in the memory it was passed is memory it
        // was passed.
        if attrs.set.contains(AttrSet::ARGMEM_ONLY) {
            let args = &self.func[self.func[call].args];
            let mut all = true;
            for &arg in args {
                if !self.func[arg].ty.is_ptr() {
                    continue;
                }
                let through = Access::through(self.func, arg);
                all &= self.decide(reference, &through).is_no();
            }
            if all {
                return Answer::No(Reason::Attribute);
            }
        }

        Answer::May
    }

    // Reading the instruction.

    /// What the callee of a direct call is declared to be, for a call whose callee the module
    /// has. An indirect call and a callee from nowhere both give nothing.
    fn callee(&self, call: Inst) -> Option<Attrs> {
        let Extra::Call(info) = self.func[call].extra else {
            return None;
        };
        let name = self.func[info].callee?;
        match self.module.lookup(name)? {
            SymbolRef::Func(id) => Some(self.module[id].attrs),
            _ => None,
        }
    }

    fn mem(&self, inst: Inst) -> Option<MemInfo> {
        match self.func[inst].extra {
            Extra::Mem(info) | Extra::Rmw(_, info) => Some(self.func[info]),
            Extra::VaObject(object) => Some(self.func[self.func[object].mem]),
            _ => None,
        }
    }

    fn result_type(&self, inst: Inst) -> Option<Type> {
        self.func[inst].results().next().map(|value| self.func[value].ty)
    }

    fn access(
        &self,
        pointer: Value,
        size: Option<u64>,
        info: Option<MemInfo>,
        flags: Flags,
    ) -> Access {
        let (origin, offset) = origin(self.func, pointer);
        Access {
            origin,
            offset,
            size,
            tbaa: info.and_then(|info| info.tbaa),
            restrict: info.map_or(Restrict::NONE, |info| info.restrict),
            volatile: flags.contains(Flags::VOLATILE),
        }
    }

    /// How many bytes a value of this type takes, which for an address is the target's answer
    /// and not the type's.
    fn width(&self, ty: Type) -> Option<u64> {
        let layout: &DataLayout = &self.module.datalayout;
        if ty.is_ptr() {
            return Some(u64::from(layout.pointer_bits).div_ceil(8));
        }
        let bits = u64::from(ty.bits()) * u64::from(ty.lanes());
        (bits > 0).then(|| bits.div_ceil(8))
    }
}

/// Layer 4: one object, two byte ranges.
fn by_offset(a: &Access, b: &Access) -> Answer {
    let (Some((a_start, a_end)), Some((b_start, b_end))) = (a.range(), b.range()) else {
        return Answer::May;
    };
    if a_end <= b_start || b_end <= a_start {
        return Answer::No(Reason::Offset);
    }
    Answer::May
}

/// The value of an integer constant, as a byte count.
fn constant(func: &Func, value: Value) -> Option<i64> {
    let Def::Result { inst, .. } = func[value].def else {
        return None;
    };
    let data = func[inst];
    if data.opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(imm) = data.extra else {
        return None;
    };
    i64::try_from(Imm::signed(func[imm], func[value].ty)).ok()
}

#[cfg(test)]
mod tests {
    use rucc_base::{Interner, Symbol};
    use rucc_ir::{
        AttrSet, Attrs, Builder, CallInfo, Extra, Flags, Func, Global, InstData, IntPred, MemInfo,
        MemOrder, MetaNode, Module, Opcode, Restrict, Signature, Type, Value,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::*;

    /// A module for the host-shaped target, and the interner its names are in.
    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        Module::new(names.intern("t.c"), &target)
    }

    /// A function taking those parameters, with an entry block and nothing in it.
    fn func(names: &mut Interner, params: &[Type]) -> Func {
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(params));
        let entry = func.create_block();
        for &ty in params {
            func.append_param(entry, ty);
        }
        func
    }

    /// A builder appending to the entry block, which is where every test here puts everything.
    fn builder(func: &mut Func) -> Builder<'_> {
        let entry = func.entry().expect("the function has an entry block");
        Builder::new(func, entry)
    }

    fn param(func: &Func, index: usize) -> Value {
        let entry = func.entry().expect("the function has an entry block");
        func[entry].params[index]
    }

    fn plain(align: u32) -> MemInfo {
        MemInfo { size: 0, align, order: MemOrder::NotAtomic, tbaa: None, restrict: Restrict::NONE }
    }

    fn sized(size: u64, align: u32) -> MemInfo {
        MemInfo { size, ..plain(align) }
    }

    /// An `alloca` of that many bytes in the entry block.
    fn local(build: &mut Builder<'_>, size: u64) -> Value {
        let mem = build.func().add_mem(sized(size, 8));
        build.value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// That address, moved on by a constant number of bytes.
    fn at(build: &mut Builder<'_>, base: Value, offset: i64) -> Value {
        let by = build.iconst(Type::int(64), i128::from(offset));
        build.binary(Opcode::PtrAdd, base, by, Flags::NONE)
    }

    /// The address of a global of that name, declared in the module as it goes.
    fn global(build: &mut Builder<'_>, module: &mut Module, name: Symbol) -> Value {
        module.add_global(Global::new(name, 16, 8));
        build.value(
            InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        )
    }

    #[test]
    fn two_different_locals_are_two_objects() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let one = local(&mut build, 16);
        let other = local(&mut build, 16);
        let read = build.load(Type::int(32), one, plain(4), Flags::NONE);
        build.store(read, other, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::No(Reason::Distinct));
        assert_eq!(alias.counts().answered(Reason::Distinct), 1);
        assert_eq!(alias.counts().queries(), 1);
    }

    /// The reference the first load in the function reads and the one the first store writes.
    fn two(alias: &Alias<'_>, func: &Func) -> (Access, Access) {
        let mut read = None;
        let mut written = None;
        for block in func.blocks() {
            for inst in func.insts(block) {
                if read.is_none() {
                    read = alias.reads(inst);
                }
                if written.is_none() {
                    written = alias.writes(inst);
                }
            }
        }
        (read.expect("a read"), written.expect("a write"))
    }

    #[test]
    fn a_local_and_a_global_are_two_objects() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let x = names.intern("x");
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let one = local(&mut build, 16);
        let other = global(&mut build, &mut module, x);
        let read = build.load(Type::int(32), one, plain(4), Flags::NONE);
        build.store(read, other, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::No(Reason::Distinct));
    }

    #[test]
    fn two_different_globals_are_two_objects() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (x, y) = (names.intern("x"), names.intern("y"));
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let one = global(&mut build, &mut module, x);
        let other = global(&mut build, &mut module, y);
        let read = build.load(Type::int(32), one, plain(4), Flags::NONE);
        build.store(read, other, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::No(Reason::Distinct));
    }

    #[test]
    fn a_global_the_module_does_not_have_is_not_argued_about() {
        // Nothing should produce this, and if something does, the answer that cannot be wrong
        // is that the two may alias.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (x, y) = (names.intern("x"), names.intern("y"));
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let one = global(&mut build, &mut module, x);
        let other = build.value(
            InstData { extra: Extra::Symbol(y), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        );
        let read = build.load(Type::int(32), one, plain(4), Flags::NONE);
        build.store(read, other, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::May);
    }

    #[test]
    fn two_parts_of_one_object_that_do_not_overlap_are_disjoint() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let object = local(&mut build, 16);
        let first = at(&mut build, object, 0);
        let second = at(&mut build, object, 4);
        let read = build.load(Type::int(32), first, plain(4), Flags::NONE);
        build.store(read, second, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::No(Reason::Offset));
    }

    #[test]
    fn two_parts_of_one_object_that_do_overlap_are_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let object = local(&mut build, 16);
        let first = at(&mut build, object, 0);
        let second = at(&mut build, object, 2);
        let read = build.load(Type::int(32), first, plain(4), Flags::NONE);
        build.store(read, second, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::May);
    }

    #[test]
    fn an_offset_nobody_knows_gives_up_the_offset_and_keeps_the_object() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[Type::int(64)]);
        let n = param(&f, 0);
        let mut build = builder(&mut f);
        let object = local(&mut build, 16);
        let somewhere = build.binary(Opcode::PtrAdd, object, n, Flags::NONE);
        let read = build.load(Type::int(32), somewhere, plain(4), Flags::NONE);
        build.store(read, object, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(a.origin, b.origin, "both are still that one object");
        assert_eq!(a.offset, None);
        assert_eq!(alias.query(&a, &b), Answer::May);
    }

    #[test]
    fn a_local_whose_address_stays_here_is_not_what_a_parameter_points_at() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[Type::PTR]);
        let outside = param(&f, 0);
        let mut build = builder(&mut f);
        let object = local(&mut build, 16);
        let read = build.load(Type::int(32), object, plain(4), Flags::NONE);
        build.store(read, outside, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        assert_eq!(alias.escapes().count(), 0);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::No(Reason::Escape));
    }

    #[test]
    fn a_local_whose_address_was_stored_somewhere_is() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[Type::PTR]);
        let outside = param(&f, 0);
        let mut build = builder(&mut f);
        let object = local(&mut build, 16);
        // The address itself is written out through a pointer this function did not make, and
        // from here anything can reach the object.
        build.store(object, outside, plain(8), Flags::NONE);
        let read = build.load(Type::int(32), object, plain(4), Flags::NONE);
        build.store(read, outside, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        assert_eq!(alias.escapes().count(), 1);
        let read = first(&f, Opcode::Load);
        let write = last(&f, Opcode::Store);
        let a = alias.reads(read).unwrap();
        let b = alias.writes(write).unwrap();
        assert_eq!(alias.query(&a, &b), Answer::May);
    }

    fn first(func: &Func, opcode: Opcode) -> Inst {
        func.blocks()
            .flat_map(|block| func.insts(block))
            .find(|&inst| func[inst].opcode == opcode)
            .expect("an instruction with that opcode")
    }

    fn last(func: &Func, opcode: Opcode) -> Inst {
        func.blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == opcode)
            .last()
            .expect("an instruction with that opcode")
    }

    #[test]
    fn an_address_carried_through_a_block_parameter_has_left_the_function() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[]);
        let start = f.entry().expect("an entry block");
        let next = f.create_block();
        f.append_param(next, Type::PTR);

        let mut build = Builder::new(&mut f, start);
        let object = local(&mut build, 16);
        build.jump(next, &[object]);
        let mut build = Builder::new(&mut f, next);
        build.ret(&[]);

        let alias = Alias::new(&f, &module);
        assert!(alias.escapes().escaped(first(&f, Opcode::Alloca)));
    }

    #[test]
    fn comparing_two_addresses_does_not_let_either_of_them_out() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[Type::PTR]);
        let outside = param(&f, 0);
        let mut build = builder(&mut f);
        let object = local(&mut build, 16);
        build.icmp(IntPred::Eq, object, outside);
        build.ret(&[]);

        let alias = Alias::new(&f, &module);
        assert_eq!(alias.escapes().count(), 0);
    }

    #[test]
    fn an_address_turned_into_a_number_has_left_the_function() {
        // The number can be turned back into a pointer anywhere, including in a different
        // translation unit, so this is an escape and the whitelist is what makes it one.
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let object = local(&mut build, 16);
        build.unary(Opcode::PtrToInt, object, Type::int(64));
        build.ret(&[]);

        let alias = Alias::new(&f, &module);
        assert!(alias.escapes().escaped(first(&f, Opcode::Alloca)));
    }

    #[test]
    fn two_restrict_pointers_in_one_scope_do_not_reach_the_same_object() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[Type::PTR, Type::PTR]);
        let (one, other) = (param(&f, 0), param(&f, 1));
        let mut build = builder(&mut f);
        let mut info = plain(4);
        info.restrict = Restrict { clique: 1, base: 1 };
        let read = build.load(Type::int(32), one, info, Flags::NONE);
        info.restrict = Restrict { clique: 1, base: 2 };
        build.store(read, other, info, Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::No(Reason::Restrict));
    }

    #[test]
    fn two_restrict_pointers_in_different_scopes_say_nothing_about_each_other() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[Type::PTR, Type::PTR]);
        let (one, other) = (param(&f, 0), param(&f, 1));
        let mut build = builder(&mut f);
        let mut info = plain(4);
        info.restrict = Restrict { clique: 1, base: 1 };
        let read = build.load(Type::int(32), one, info, Flags::NONE);
        info.restrict = Restrict { clique: 2, base: 1 };
        build.store(read, other, info, Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::May);
    }

    /// A module with a `char` root and an `int` and a `float` hanging off it.
    fn types(module: &mut Module, names: &mut Interner) -> (Meta, Meta, Meta) {
        let root =
            module.add_meta(MetaNode { name: names.intern("char"), parent: None, offset: 0 });
        let int =
            module.add_meta(MetaNode { name: names.intern("int"), parent: Some(root), offset: 0 });
        let float = module.add_meta(MetaNode {
            name: names.intern("float"),
            parent: Some(root),
            offset: 0,
        });
        (root, int, float)
    }

    #[test]
    fn two_unrelated_types_describe_no_object_in_common() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (_, int, float) = types(&mut module, &mut names);
        let mut f = func(&mut names, &[Type::PTR, Type::PTR]);
        let (one, other) = (param(&f, 0), param(&f, 1));
        let mut build = builder(&mut f);
        let mut info = plain(4);
        info.tbaa = Some(int);
        let read = build.load(Type::int(32), one, info, Flags::NONE);
        info.tbaa = Some(float);
        build.store(read, other, info, Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::No(Reason::Tbaa));
    }

    #[test]
    fn an_access_through_char_conflicts_with_everything() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (root, int, _) = types(&mut module, &mut names);
        let mut f = func(&mut names, &[Type::PTR, Type::PTR]);
        let (one, other) = (param(&f, 0), param(&f, 1));
        let mut build = builder(&mut f);
        let mut info = plain(4);
        info.tbaa = Some(int);
        let read = build.load(Type::int(32), one, info, Flags::NONE);
        info.tbaa = Some(root);
        build.store(read, other, info, Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::May);
    }

    #[test]
    fn turning_strict_aliasing_off_turns_off_that_layer_and_no_other() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (_, int, float) = types(&mut module, &mut names);
        let mut f = func(&mut names, &[Type::PTR, Type::PTR]);
        let (one, other) = (param(&f, 0), param(&f, 1));
        let mut build = builder(&mut f);
        let mut info = plain(4);
        info.tbaa = Some(int);
        info.restrict = Restrict { clique: 1, base: 1 };
        let read = build.load(Type::int(32), one, info, Flags::NONE);
        info.tbaa = Some(float);
        info.restrict = Restrict { clique: 1, base: 2 };
        build.store(read, other, info, Flags::NONE);
        build.ret(&[]);

        let options = Options { strict_aliasing: false };
        let mut alias = Alias::with(&f, &module, options);
        let (a, b) = two(&alias, &f);
        // The `restrict` layer still answers, which is the point: the flag is one condition in
        // one place and it does not reach anything else.
        assert_eq!(alias.query(&a, &b), Answer::No(Reason::Restrict));

        let mut without = Alias::with(&f, &module, options);
        let plainer = Access { restrict: Restrict::NONE, ..a };
        let other = Access { restrict: Restrict::NONE, ..b };
        assert_eq!(without.query(&plainer, &other), Answer::May);

        let mut with = Alias::new(&f, &module);
        assert_eq!(with.query(&plainer, &other), Answer::No(Reason::Tbaa));
    }

    #[test]
    fn writing_one_member_of_a_union_and_reading_another_is_one_object() {
        // The compatibility fact of section 8.6. Two accesses to one object at the same offset
        // with unrelated types, which is `union { int i; float f; }` written as one and read as
        // the other. The offset layer runs first, it says they overlap, and the type layer
        // never gets to say no. Twenty years of real C rests on this answer.
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (_, int, float) = types(&mut module, &mut names);
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let object = local(&mut build, 4);
        let mut info = plain(4);
        info.tbaa = Some(float);
        let read = build.load(Type::int(32), object, info, Flags::NONE);
        info.tbaa = Some(int);
        build.store(read, object, info, Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::May);
    }

    #[test]
    fn two_volatile_accesses_conflict_whatever_else_is_true_of_them() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let one = local(&mut build, 16);
        let other = local(&mut build, 16);
        let read = build.load(Type::int(32), one, plain(4), Flags::VOLATILE);
        build.store(read, other, plain(4), Flags::VOLATILE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        // Two different objects, and the answer is still that they conflict, because moving
        // one volatile access across another is the thing `volatile` exists to forbid.
        assert_eq!(alias.query(&a, &b), Answer::May);
    }

    #[test]
    fn one_volatile_access_and_one_ordinary_one_are_argued_about_as_usual() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let one = local(&mut build, 16);
        let other = local(&mut build, 16);
        let read = build.load(Type::int(32), one, plain(4), Flags::VOLATILE);
        build.store(read, other, plain(4), Flags::NONE);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let (a, b) = two(&alias, &f);
        assert_eq!(alias.query(&a, &b), Answer::No(Reason::Distinct));
    }

    #[test]
    fn a_copy_reads_its_source_and_writes_its_destination() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let to = local(&mut build, 16);
        let from = local(&mut build, 16);
        let mem = build.func().add_mem(sized(16, 8));
        let args = build.func().push_values(&[to, from]);
        build.inst(InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Memcpy) }, &[]);
        build.ret(&[]);

        let alias = Alias::new(&f, &module);
        let copy = first(&f, Opcode::Memcpy);
        let read = alias.reads(copy).expect("a copy reads");
        let written = alias.writes(copy).expect("a copy writes");
        assert_eq!(read.size, Some(16));
        assert_eq!(written.size, Some(16));
        assert_ne!(read.origin, written.origin);
    }

    /// A call to a function declared with those attributes.
    fn call_to(
        names: &mut Interner,
        module: &mut Module,
        f: &mut Func,
        attrs: Attrs,
        args: &[Value],
    ) -> Inst {
        let name = names.intern("g");
        let params: Vec<Type> = args.iter().map(|_| Type::PTR).collect();
        let mut callee = Func::new(name, Signature::new().with_params(&params));
        callee.attrs = attrs;
        module.add_func(callee);
        let signature = f.add_signature(Signature::new().with_params(&params));
        let mut build = builder(f);
        build.call(name, signature, args)
    }

    fn attrs(set: AttrSet) -> Attrs {
        Attrs { set, ..Attrs::NONE }
    }

    #[test]
    fn a_call_cannot_touch_a_local_whose_address_stayed_here() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut f = func(&mut names, &[Type::PTR]);
        let outside = param(&f, 0);
        let mut build = builder(&mut f);
        let object = local(&mut build, 16);
        let read = build.load(Type::int(32), object, plain(4), Flags::NONE);
        let _ = read;
        let call = call_to(&mut names, &mut module, &mut f, Attrs::NONE, &[outside]);
        let mut build = builder(&mut f);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let reference = alias.reads(first(&f, Opcode::Load)).unwrap();
        assert_eq!(alias.clobbered_by(&reference, call), Answer::No(Reason::Escape));
        assert_eq!(alias.read_by(&reference, call), Answer::No(Reason::Escape));
    }

    #[test]
    fn a_call_can_touch_a_local_it_was_handed() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let object = local(&mut build, 16);
        build.load(Type::int(32), object, plain(4), Flags::NONE);
        let call = call_to(&mut names, &mut module, &mut f, Attrs::NONE, &[object]);
        let mut build = builder(&mut f);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let reference = alias.reads(first(&f, Opcode::Load)).unwrap();
        assert_eq!(alias.clobbered_by(&reference, call), Answer::May);
    }

    #[test]
    fn a_pure_callee_reads_memory_and_writes_none() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut f = func(&mut names, &[Type::PTR]);
        let outside = param(&f, 0);
        let mut build = builder(&mut f);
        build.load(Type::int(32), outside, plain(4), Flags::NONE);
        let call = call_to(&mut names, &mut module, &mut f, attrs(AttrSet::READONLY), &[outside]);
        let mut build = builder(&mut f);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let reference = alias.reads(first(&f, Opcode::Load)).unwrap();
        assert_eq!(alias.clobbered_by(&reference, call), Answer::No(Reason::Attribute));
        assert_eq!(alias.read_by(&reference, call), Answer::May);
    }

    #[test]
    fn a_const_callee_touches_no_memory_at_all() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut f = func(&mut names, &[Type::PTR]);
        let outside = param(&f, 0);
        let mut build = builder(&mut f);
        build.load(Type::int(32), outside, plain(4), Flags::NONE);
        let call = call_to(&mut names, &mut module, &mut f, attrs(AttrSet::READNONE), &[outside]);
        let mut build = builder(&mut f);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let reference = alias.reads(first(&f, Opcode::Load)).unwrap();
        assert_eq!(alias.clobbered_by(&reference, call), Answer::No(Reason::Attribute));
        assert_eq!(alias.read_by(&reference, call), Answer::No(Reason::Attribute));
    }

    #[test]
    fn a_callee_that_touches_only_its_arguments_leaves_a_global_it_was_not_passed_alone() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let x = names.intern("x");
        let mut f = func(&mut names, &[Type::PTR]);
        let outside = param(&f, 0);
        let mut build = builder(&mut f);
        let object = global(&mut build, &mut module, x);
        build.load(Type::int(32), object, plain(4), Flags::NONE);
        let call =
            call_to(&mut names, &mut module, &mut f, attrs(AttrSet::ARGMEM_ONLY), &[outside]);
        let mut build = builder(&mut f);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let reference = alias.reads(first(&f, Opcode::Load)).unwrap();
        // The one pointer it was handed is a parameter of unknown origin, which may be that
        // global, so this is the answer that cannot be wrong.
        assert_eq!(alias.clobbered_by(&reference, call), Answer::May);
    }

    #[test]
    fn a_callee_that_touches_only_its_arguments_and_was_handed_one_object_leaves_the_other() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let (x, y) = (names.intern("x"), names.intern("y"));
        let mut f = func(&mut names, &[]);
        let mut build = builder(&mut f);
        let watched = global(&mut build, &mut module, x);
        let handed = global(&mut build, &mut module, y);
        build.load(Type::int(32), watched, plain(4), Flags::NONE);
        let call = call_to(&mut names, &mut module, &mut f, attrs(AttrSet::ARGMEM_ONLY), &[handed]);
        let mut build = builder(&mut f);
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let reference = alias.reads(first(&f, Opcode::Load)).unwrap();
        assert_eq!(alias.clobbered_by(&reference, call), Answer::No(Reason::Attribute));
    }

    #[test]
    fn an_indirect_call_is_not_argued_about() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let mut f = func(&mut names, &[Type::PTR, Type::PTR]);
        let (target, outside) = (param(&f, 0), param(&f, 1));
        let mut build = builder(&mut f);
        build.load(Type::int(32), outside, plain(4), Flags::NONE);
        let signature = build.func().add_signature(Signature::new().with_params(&[Type::PTR]));
        let varargs = build.func().push_abis(&[]);
        let info = build.func().add_call(CallInfo { callee: None, signature, varargs });
        let args = build.func().push_values(&[target, outside]);
        let call = build.inst(
            InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::CallIndirect) },
            &[],
        );
        build.ret(&[]);

        let mut alias = Alias::new(&f, &module);
        let reference = alias.reads(first(&f, Opcode::Load)).unwrap();
        assert_eq!(alias.clobbered_by(&reference, call), Answer::May);
    }

    #[test]
    fn every_reason_has_a_name_and_a_sentence() {
        for reason in Reason::ALL {
            assert!(!reason.name().is_empty());
            assert!(!reason.describe().is_empty());
            assert_eq!(Reason::ALL[reason.index()], reason);
        }
        assert_eq!(Reason::ALL.len(), Reason::COUNT);
        assert_eq!(Answer::No(Reason::Offset).reason(), Some(Reason::Offset));
        assert!(Answer::No(Reason::Offset).is_no());
        assert_eq!(Answer::May.reason(), None);
        assert!(!Answer::May.is_no());
    }
}
