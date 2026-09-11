//! The memory safety monitor: check insertion over the IR.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` section 6.3.
//!
//! The one decision this crate exists to make is *when* checks are inserted. Every sanitizer that
//! came before instruments after the optimizer, so that the optimizer cannot delete its checks,
//! and pays the full naive cost of every one of them forever. We insert before the optimizer and
//! let it discharge what it can prove, which is only possible because a check is an instruction
//! with defined semantics rather than a call the optimizer has no opinion about.
//!
//! # What is here so far
//!
//! The three checks milestone S1 in `spec/safe-memory/16-milestones.md` asks for: bounds and
//! lifetime on every access, and a derivation check on every pointer computed from another
//! pointer. Nothing is discharged, so a function comes out with a check in front of everything,
//! which is the baseline every elimination claim at S4 is measured against.
//!
//! And the boundary, in [`mod@wrap`]: a call the program wrote to one of the C library functions
//! `rucc-safe-rt` has a row for is pointed at that row's wrapper instead, so the judgements happen
//! before the call rather than not at all. That is milestone S2 and
//! `spec/safe-memory/10-boundaries.md` section 10.3 is what it implements.
//!
//! And the other end of it, in [`mod@lower`]: after the optimizer has run, every check still standing
//! becomes a call to the runtime carrying the index of a row in a table this crate puts in the
//! object. That module is where the reason S1's checks are calls rather than compares is argued.
//!
//! And the rest of the boundary, in [`mod@boundary`]: the places where a pointer crosses between
//! this build and code nobody instrumented, which is a function of this file that somebody else can
//! call and a call this file makes to a library that has no wrapper. Neither can be modelled, so
//! each of them is counted instead, which is what section 10.2 says the honest answer to a question
//! you cannot answer is.
//!
//! And what all of that came to, in [`mod@summary`]: the counts `--emit=safety-summary` prints,
//! which are what `spec/safe-memory/10-boundaries.md` section 10.2 means by a trust set that is
//! counted per build rather than asserted.
//!
//! And the first of the planes, in [`mod@plane`]: every store records what the bytes it wrote were
//! stored through, which is the judgement C 6.5 says a store makes, and every copy carries whatever
//! the bytes it read said over to the bytes it wrote, which is the other half of the same rule.
//! Those two are every write the type plane has, and every read that names a type now asks the
//! plane whether the bytes agree with it, which is judgement J3. The order was deliberate: a check
//! against a plane that only some of the writes maintain reports on programs that are correct, so
//! the writes went in first and the question went in once they were all in.
//!
//! And the second of them, over the same two writes: every store records that the bytes it wrote
//! hold something, and every copy carries whether the bytes it read held anything over to the bytes
//! it wrote. The init plane is one bit per byte, so a store records the same thing whatever it
//! stored, and a copy is the reason padding a member by member fill never touched is still padding
//! nothing wrote after the structure moves. Every read now asks whether anything ever wrote the
//! bytes it is about to read, which is document 03's Y6, and the writes went in first for the
//! reason the type plane's did.
//!
//! And the one check that is not about a single access, in [`mod@promise`]: a block that declares
//! `restrict` pointers keeps a record of what each of them reached, and every access through one of
//! them asks whether another got there first. That is judgement J8 and it is off unless the build
//! asks for it with `-fsafety-restrict`, which is the only check here that is, and the reason is on
//! [`rucc_session::Promise`].
//!
//! The padding rule of `spec/safe-memory/09-type-init-and-races.md` section 9.3 arrives here as one
//! number. A store carries how much padding the member it went through owns, and the range it
//! records is the wider of that and what it wrote, which is all of `-fsafety-init=nopadding`. How
//! far the padding goes takes a record's layout and this crate reads IR, so the front end is what
//! decides it and `MemInfo.owns` is how the answer travels.
//!
//! The two questions a read asks come apart in one place. A read the front end named no type for
//! asks the type plane nothing, because the question there is which type the bytes hold, and it
//! asks the init plane the same thing every other read does, because the question there is about
//! the bytes rather than about the access. What no `load` in any program asks about is padding: a
//! read compiled into a `load` reads a member and a member is never padding, so the reads that
//! cover padding are `memcmp` of two structures, hashing one and handing one to `write`, every one
//! of which is a call into the movement group of [`mod@wrap`]. Which is why the flag selects what a
//! store records rather than what a read asks about: the reads that would need it are not `load`s.
//!
//! The race check is not here, because the epoch plane is not written at all and a check against a
//! plane nobody maintains would either report on every access or on none. That is S6. Neither are
//! the other plane writes: `meta_begin` and `meta_end` for an automatic instance need the escape
//! analysis of document 08 section 8.4, and until that exists the only instances the runtime knows
//! about are the ones the allocator reports, which is also why a store to a local records into a
//! plane that is not there and costs a call that decides nothing.
//!
//! # Why the rank matters
//!
//! `rucc-safety` is rank 10, alongside `rucc-lower` and `rucc-opt`, so it can depend on neither.
//! That is the constraint and not an inconvenience: it consumes IR and produces IR, it never sees
//! the AST, and `rucc-driver` at rank 13 is what sequences it between the two.
//! `spec/safe-memory/15-integration.md` section 15.1 argues it out.
//!
//! # Stability
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-safety/0.10.22")]

pub mod boundary;
pub mod lower;
pub mod plane;
pub mod promise;
pub mod summary;
pub mod wrap;

pub use boundary::{Sites, WITNESS, witness};
pub use lower::{Descriptor, SECTION, lower};
pub use plane::Plane;
pub use promise::{Kept, promise};
pub use summary::{Frames, Summary, summarize};
pub use wrap::{INTERPOSED, PREFIX, redirect};

use rucc_ir::{Def, Extra, Func, Imm, Inst, InstData, Module, Opcode, Type, Value};
pub use rucc_session::{Promise, Subobject};

/// How many checks a run of [`insert`] put in.
///
/// Reported rather than discarded because the number of checks a function starts with is the
/// denominator of everything document 13 measures, and it is not recoverable later: by the time
/// the optimizer has run, the checks that were discharged are gone and nothing says how many
/// there were.
///
/// The three counts are kept apart rather than added up because they are discharged by different
/// rules and at very different rates. Document 07 expects bounds to go away often, lifetime to go
/// away when the instance does not escape, and derivation to survive, so one number would hide
/// exactly the thing the measurement is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Accesses that were given a bounds check.
    pub checked: usize,
    /// Accesses that were given a lifetime check, which is the same set as `checked`.
    pub live: usize,
    /// Pointers computed from another pointer that were given a derivation check.
    pub derived: usize,
    /// Accesses that got nothing, because the pointer they go through is not a value this pass
    /// can take the capability of.
    pub skipped: usize,
    /// Stores that recorded what the bytes they wrote were stored through.
    ///
    /// Not in `--emit=safety-summary` yet, which is the one count here that is not. The summary
    /// reports a class as a pair, how many went in and how many are left, and nothing discharges a
    /// plane write today, so the pair would be one number written twice. It goes in beside the
    /// first rule that removes one.
    pub judged: usize,
    /// Copies that carried whatever the bytes they read said over to the bytes they wrote.
    ///
    /// Kept apart from `judged` for the reason the three check counts are kept apart. A store
    /// records a type the compiler knows and a copy records one only the plane knows, so the two
    /// are discharged by different rules: a store into storage nothing watches can be dropped by
    /// looking at the store, and a copy cannot be looked at the same way.
    pub carried: usize,
    /// Accesses that asked the plane whether the bytes agree with the type they name.
    ///
    /// Fewer than `checked`, and the reason is in `ask`: an access the front end did not name a
    /// type for has no question to put. Without `-fsafety-subobject` it is fewer again, because
    /// only a read asks, and the reason a store asks only when somebody asked for it is on
    /// [`rucc_session::Subobject`].
    pub asked: usize,
    /// Stores that recorded that the bytes they wrote hold what they wrote.
    ///
    /// The same set as `checked` minus the reads, and unlike `judged` it does not thin: a store
    /// records into the init plane whatever it was storing through, because what the init plane
    /// holds is whether anything was stored at all.
    pub wrote: usize,
    /// Reads that asked the plane whether anything ever wrote the bytes they are about to read.
    ///
    /// The same set as the reads in `checked`, and unlike `asked` it does not thin: the question is
    /// whether the bytes hold anything at all, which is a question about every read whatever type
    /// the front end did or did not name for it.
    pub filled: usize,
    /// Copies that carried whether the bytes they read held anything over to the bytes they wrote.
    ///
    /// The same set as `carried`, and counted beside it for the reason `wrote` is counted beside
    /// `judged`: the two planes will be discharged by different rules, so the day one of them
    /// thins the numbers have to be able to differ.
    pub moved: usize,
    /// Accesses that asked their block whether another `restrict` pointer of it got there first.
    ///
    /// Zero without `-fsafety-restrict`, and zero in the overwhelming majority of functions with
    /// it, because the only accesses that ask are the ones the front end traced back to a
    /// `restrict` declaration. [`mod@promise`] is where both of those are argued.
    pub promised: usize,
    /// Blocks that opened a scope, which is one per `restrict` clique that has an access in it.
    ///
    /// Kept apart from `promised` because it is the part of the cost that is paid per call rather
    /// than per access: two calls and a stack slot, against which a block that checks a thousand
    /// accesses and a block that checks one look very different.
    pub scoped: usize,
}

impl Counts {
    /// Adds another function's counts to these.
    fn add(&mut self, other: Counts) {
        self.checked += other.checked;
        self.live += other.live;
        self.derived += other.derived;
        self.skipped += other.skipped;
        self.judged += other.judged;
        self.carried += other.carried;
        self.asked += other.asked;
        self.wrote += other.wrote;
        self.filled += other.filled;
        self.moved += other.moved;
        self.promised += other.promised;
        self.scoped += other.scoped;
    }

    /// Adds what the `restrict` walk of one function came to.
    ///
    /// A second function rather than a second [`Counts`] because that walk counts two things and
    /// has no opinion about the other ten, and a conversion that filled in ten zeroes would let a
    /// later count be lost by being added to a zero.
    fn add_kept(&mut self, kept: Kept) {
        self.promised += kept.promised;
        self.scoped += kept.scoped;
    }
}

/// Puts checks in every function a module defines.
///
/// The whole module rather than a function at a time, because that is the unit the driver hands
/// around and because the pass has nothing to say about the order: no check depends on anything
/// outside the function it is in. A declaration has no body and is skipped, for the same reason
/// the back end skips it.
///
/// Whether this runs at all is `-fsafety=`, and the driver decides it. This crate does not read
/// the flag, because a pass that decides for itself whether it runs is a pass whose effect cannot
/// be read off the pipeline.
pub fn run(module: &mut Module, subobject: Subobject, promise: Promise) -> Counts {
    // Before the walk, because the entries live in the module and a function is borrowed out of
    // the module while its stores are being instrumented. It is also the reason this is the entry
    // point rather than [`insert`]: there is one plane per module and every function records into
    // the same one.
    let plane = Plane::build(module);
    // The one thing a pointer typed access cannot work out for itself, which is how wide it is.
    let width = u64::from(module.datalayout.pointer_bits / 8);
    let mut counts = Counts::default();
    for id in module.funcs() {
        if !module[id].is_declaration() {
            counts.add(insert(&mut module[id], &plane, width, subobject, promise));
        }
    }
    counts
}

/// Puts checks in front of every access and every derivation in a function.
///
/// Section 6.3: every `load` and `store` gets `check_bounds` and `check_live`, with the
/// capability coming from `cap_of` on the pointer operand, and every `ptr_add` gets
/// `check_deriv` on the pointer it was computed from. The size and the alignment are the
/// access's own, since a check that asked about a different number of bytes from the access it
/// guards would be checking something the program does not do.
///
/// The two access checks are separate instructions rather than one fused check, which section
/// 6.2.2 asks for and which matters more than it looks: the common case document 07 is built
/// around is that the bounds check is discharged and the lifetime check is not, or the other way
/// round for a local whose frame the compiler can see. One instruction would mean keeping both
/// whenever either survived. Where both do survive, the backend fuses them behind one branch.
///
/// Nothing is discharged here. A `check_bounds` on a pointer whose bounds are statically obvious
/// is still emitted, and the fact propagation in `rucc-opt` is what removes it. That split is the
/// whole design: this pass is a walk anybody can read, and the deletions are rules that are
/// verified.
pub fn insert(
    func: &mut Func,
    plane: &Plane,
    width: u64,
    subobject: Subobject,
    promise: Promise,
) -> Counts {
    let mut counts = Counts::default();
    let insts: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in insts {
        match func[inst].opcode {
            Opcode::Load | Opcode::Store => match pointer_of(func, inst) {
                Some(pointer) => {
                    let capability = check(func, inst, pointer, width);
                    counts.checked += 1;
                    counts.live += 1;
                    if func[inst].opcode == Opcode::Store {
                        // In front of the store, and only when the build asked for it. Every other
                        // question at a store is a recording made afterwards, and this is the one
                        // that can refuse, so it has to be asked while the bytes still say what
                        // they said before.
                        if subobject.asks() && ask(func, plane, inst, pointer, capability, width) {
                            counts.asked += 1;
                        }
                        // The init plane's write goes in first so that the type plane's ends up in
                        // front of it, since both are inserted after the store and the one that
                        // goes in second is the one that lands nearer to it.
                        if wrote(func, inst, pointer, width) {
                            counts.wrote += 1;
                        }
                        if judge(func, plane, inst, pointer, width) {
                            counts.judged += 1;
                        }
                    } else {
                        // Both go in front of the read, and the one that goes in second is the one
                        // that lands nearer to it, so this order prints the type question and then
                        // the init question. Either order is correct: neither reads what the other
                        // wrote and the read happens after both.
                        if ask(func, plane, inst, pointer, capability, width) {
                            counts.asked += 1;
                        }
                        if filled(func, inst, pointer, capability, width) {
                            counts.filled += 1;
                        }
                    }
                }
                None => counts.skipped += 1,
            },
            Opcode::Memcpy | Opcode::Memmove => {
                // Second for the reason a store's two are in the order they are in.
                if moved(func, inst) {
                    counts.moved += 1;
                }
                if carry(func, inst) {
                    counts.carried += 1;
                }
            }
            Opcode::PtrAdd => {
                if derivation(func, inst) {
                    counts.derived += 1;
                } else {
                    counts.skipped += 1;
                }
            }
            _ => {}
        }
    }
    // Last, so that the check it puts in front of an access lands after the bounds check that is
    // already there. It is its own walk rather than another arm above because what it puts in is
    // not one check per access: the scopes are per function and the two calls that keep one go in
    // the entry block and at every exit.
    if promise.checks() {
        counts.add_kept(promise::promise(func, width));
    }
    counts
}

/// The pointer an access goes through.
///
/// A `load` reads through its first operand and a `store` writes through its second, the value
/// being written coming first because that is the order the text writes them in.
fn pointer_of(func: &Func, access: Inst) -> Option<Value> {
    let args = &func[func[access].args];
    let at = match func[access].opcode {
        Opcode::Load => 0,
        Opcode::Store => 1,
        _ => return None,
    };
    let &value = args.get(at)?;
    func[value].ty.is_ptr().then_some(value)
}

/// Puts `cap_of`, `check_bounds` and `check_live` immediately before one access.
///
/// Gives back the capability the two checks read, so that a third check on the same access can read
/// the same one rather than taking it again. An access with no payload gets nothing and answers
/// nothing, which is the shape a caller has to handle anyway.
fn check(func: &mut Func, access: Inst, pointer: Value, width: u64) -> Option<Value> {
    let span = func.span(access);
    let Extra::Mem(info) = func[access].extra else { return None };
    let mut info = func[info];
    info.size = covered(func, access, info.size, width);
    // Not the padding after it. What a check is about is the bytes the access touches, and the
    // padding is about what a store records rather than about what it reads or writes.
    info.owns = 0;

    let capability = cap_of(func, pointer, access);

    // The check reads the same bytes the access does, so it carries the access's own payload
    // rather than a copy of it that could later disagree.
    let args = func.push_values(&[capability, pointer]);
    let extra = Extra::Mem(func.add_mem(info));
    let bounds =
        func.create_inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[], span);
    func.insert_before(bounds, access);

    // No payload on this one. Whether the capability still names whoever owns the address is a
    // question about the pointer and not about how many bytes are being read through it.
    let args = func.push_values(&[capability, pointer]);
    let live = func.create_inst(InstData { args, ..InstData::new(Opcode::CheckLive) }, &[], span);
    func.insert_before(live, access);

    Some(capability)
}

/// Puts a `meta_type` immediately after one store, recording what its bytes were stored through.
///
/// The judgement of C 6.5: a store through an lvalue of type `T` sets the effective type of what it
/// wrote to `T`, and the plane is where that is written down. What the store names is the aliasing
/// node the walk put on it, and [`Plane::entry`] is the translation from that to the entry the
/// plane holds, including the two cases that are not a type.
///
/// After the store rather than before it, which is the one thing about the placement that matters.
/// The bytes are stored through that type once the store has happened, and a plane that said so
/// first would be describing a store that the bounds check in front of it may yet refuse.
///
/// The length is a value rather than a field of the payload because that is the shape the opcode
/// has, and it is a `meta_type` over a range because one store writes a run of bytes. Where the
/// value comes from is [`extent`].
fn judge(func: &mut Func, plane: &Plane, store: Inst, pointer: Value, width: u64) -> bool {
    let Extra::Mem(info) = func[store].extra else { return false };
    let size = covered(func, store, func[info].size, width);
    // A store whose width nothing states covers no bytes anybody can name, and a plane write over
    // nothing is an instruction with no effect.
    if size == 0 {
        return false;
    }
    let node = plane.entry(func[info].tbaa);

    let span = func.span(store);
    let (made, length) = extent(func, store, size);
    let args = func.push_values(&[pointer, length]);
    let data = InstData { args, extra: Extra::Node(node), ..InstData::new(Opcode::MetaType) };
    let judged = func.create_inst(data, &[], span);
    // After the constant it reads rather than after the store, since both go in the same place and
    // the one that goes in second ends up in front.
    func.insert_after(judged, made);
    true
}

/// Puts a `check_type` immediately before one read, asking whether the bytes agree with the type
/// they are about to be read as.
///
/// Judgement J3, and the half of the type plane that decides something. The two writes record what
/// a store and a copy left behind, and this is the question they were recorded for: the effective
/// type rule of C 6.5 says an object's stored value may only be read through a type compatible with
/// the one it was stored through, and the plane is where the compiler wrote down which that was.
///
/// # Why only a read
///
/// A store does not ask, it answers. The plane covers storage the allocator reported and nothing
/// else, which is exactly the storage C gives no declared type, and the effective type of such an
/// object is whatever the last store through it set. So a store cannot disagree with the plane: it
/// is what makes the plane say what it says, and a check in front of one would refuse the reuse of
/// a buffer that the standard permits.
///
/// # Why a read that names no type asks nothing
///
/// An access whose payload carries no aliasing node is an access the front end did not say the type
/// of, which is a copy of an aggregate, an array, or anything else reached by address. That is not
/// the same as reading bytes nothing has been stored through, and the plane's untyped entry means
/// the second one. Asking with it would refuse every read of a structure whose members had been
/// stored through their own types, which is every correct program that has one.
///
/// The check goes in front of the read, behind the bounds and lifetime checks that are also in
/// front of it. A question about what the bytes say is worth asking only once somebody owns them,
/// and what the runtime answers for an address no region covers is nothing rather than a refusal.
fn ask(
    func: &mut Func,
    plane: &Plane,
    read: Inst,
    pointer: Value,
    capability: Option<Value>,
    width: u64,
) -> bool {
    let Some(capability) = capability else { return false };
    let Extra::Mem(at) = func[read].extra else { return false };
    let mut info = func[at];
    let Some(node) = info.tbaa else { return false };
    info.size = covered(func, read, info.size, width);
    // A read whose width nothing states reads no bytes anybody can name, the same way a store of
    // none writes none.
    if info.size == 0 {
        return false;
    }
    // The payload the check carries is the access's, with the aliasing node replaced by the plane
    // entry for it, because the plane and the aliasing tree are two vocabularies and the question is
    // put in the plane's.
    info.tbaa = Some(plane.entry(Some(node)));
    // As in `access_checks`, and here it could never be anything else: a read carries no padding.
    info.owns = 0;

    let span = func.span(read);
    let args = func.push_values(&[capability, pointer]);
    let extra = Extra::Mem(func.add_mem(info));
    let data = InstData { args, extra, ..InstData::new(Opcode::CheckType) };
    let asked = func.create_inst(data, &[], span);
    func.insert_before(asked, read);
    true
}

/// Puts a `meta_type_copy` immediately after one copy, carrying what its source said to its
/// destination.
///
/// The other half of the judgement C 6.5 describes. A copy does not store through a type, so there
/// is no type for the compiler to record: what the copied bytes are is whatever the bytes they came
/// from were, and the only place that is written down is the plane over the source. So this names
/// two ranges and no node, and the runtime moves the entries across.
///
/// Without it the destination would keep whatever the bytes there said before the copy, which is
/// the thing that makes a check against the plane unusable. A structure copied into a fresh
/// allocation would come out untyped at best and, once the allocation had been reused, wrong at
/// worst, and the very next read of a field would be refused on a program that is correct.
///
/// After the copy rather than before it, for the same reason a store's judgement goes after the
/// store. The bytes say the new thing once the copy has happened. Reading the source's plane
/// afterwards is the same answer as reading it before, overlap included, because a copy writes no
/// plane entries of its own.
fn carry(func: &mut Func, copy: Inst) -> bool {
    let Extra::Mem(info) = func[copy].extra else { return false };
    // A copy of a known size is what the opcode is, and the verifier refuses one whose payload says
    // zero, so this is a shape that does not arise rather than a case being handled.
    let size = func[info].size;
    if size == 0 {
        return false;
    }
    let [to, from] = func[func[copy].args] else { return false };

    let span = func.span(copy);
    let (made, length) = extent(func, copy, size);
    let args = func.push_values(&[to, from, length]);
    let data = InstData { args, ..InstData::new(Opcode::MetaTypeCopy) };
    let carried = func.create_inst(data, &[], span);
    // After the constant it reads rather than after the copy, since both go in the same place and
    // the one that goes in second ends up in front.
    func.insert_after(carried, made);
    true
}

/// Puts a `meta_init` immediately after one store, recording that its bytes hold what it wrote.
///
/// The judgement of `spec/safe-memory/09-type-init-and-races.md` section 9.2, and the write the
/// init plane is made of. An instance beginning is the only thing that makes a byte unwritten, and
/// this is the only thing that makes one written again, so between the two of them the plane holds
/// exactly the bytes the monitor watched a store land on.
///
/// After the store, and for the same reason the type plane's judgement goes after one: the bytes
/// hold what was written once the store has happened, and saying so first would be describing a
/// store the bounds check in front of it may yet refuse.
///
/// # Where the padding rule lives
///
/// Section 9.3 says a store that writes an object as a whole initializes it as a whole, padding
/// included, and that a member by member fill leaves the padding alone. Nothing here implements
/// that, and nothing has to. Both arrive as a range and the range is the access's own width: a
/// store through a member of a structure is a `store` of the member's width and names the member,
/// and a structure assigned whole, a `= {0}`, a `memset` and a `memcpy` are all a copy of `sizeof`
/// bytes and name the object. The rule falls out of what the front end already lowered rather than
/// out of anything this pass knows about structures, which is what keeps it one rule rather than a
/// special case per shape.
///
/// # Why this does not thin
///
/// A store records into the init plane whatever type it was storing through, including the two
/// cases the type plane has no entry for. What the init plane holds is whether anything was stored
/// at all, and the answer to that does not depend on what the store thought it was writing, so
/// every store that covers a byte records it.
fn wrote(func: &mut Func, store: Inst, pointer: Value, width: u64) -> bool {
    let Extra::Mem(info) = func[store].extra else { return false };
    // The padding after a member, where the front end was asked to say how far it goes. That is
    // the whole of `-fsafety-init=nopadding` and it is a number rather than a mode here, because
    // what the padding is takes a record's layout and this pass reads IR.
    let size = covered(func, store, func[info].size, width).max(u64::from(func[info].owns));
    // A store whose width nothing states writes no bytes anybody can name, the same way the type
    // plane's judgement over one records nothing.
    if size == 0 {
        return false;
    }

    let span = func.span(store);
    let (made, length) = extent(func, store, size);
    let args = func.push_values(&[pointer, length]);
    let data = InstData { args, ..InstData::new(Opcode::MetaInit) };
    let judged = func.create_inst(data, &[], span);
    // After the constant it reads rather than after the store, since both go in the same place and
    // the one that goes in second ends up in front.
    func.insert_after(judged, made);
    true
}

/// Puts a `check_init` in front of one read, asking whether anything ever wrote the bytes it is
/// about to read.
///
/// Document 03's Y6, and the class MSan exists for. The two writes are in, a store recording that
/// the bytes it wrote hold something and a copy carrying whether the bytes it read held anything,
/// so the plane now says something true about every byte a program wrote and this is the question
/// those writes were recorded for.
///
/// # Why every read and not only the ones that named a type
///
/// [`ask`] passes over a read the front end named no type for, because a question about which type
/// bytes hold has nothing to ask when the access names none. This one has no such case: the plane
/// holds one bit per byte and the bit says whether anything was ever stored there, which is a fact
/// about the bytes and not about the access, so a read of an aggregate by address asks it just as a
/// read of an `int` does.
///
/// # Padding
///
/// It does not come up here and that is worth writing down, because section 9.3 is where the
/// padding rule lives and this is the check the rule is about. A read compiled into a `load` reads
/// a member, and a member is never padding, so no `load` in any program covers a byte a
/// member-by-member fill left alone. The reads that do cover padding are `memcmp` of two
/// structures, hashing one, and handing one to `write`, and every one of those is a call into the
/// movement group of `crate::wrap` rather than a `load`. That is where `-fsafety-init=padding` will
/// have something to select, and it is why the flag is not here.
///
/// # Why a whole width and not a byte
///
/// The payload's size, the same one [`ask`] uses, so a read that straddles the end of what was
/// written is refused on the first byte nothing wrote rather than on the byte the address names.
/// A read of four bytes where two were written is a read of memory that was never written, and
/// reporting it at the access is the only place a report means anything.
fn filled(
    func: &mut Func,
    read: Inst,
    pointer: Value,
    capability: Option<Value>,
    width: u64,
) -> bool {
    let Some(capability) = capability else { return false };
    let Extra::Mem(at) = func[read].extra else { return false };
    let mut info = func[at];
    info.size = covered(func, read, info.size, width);
    // A read whose width nothing states reads no bytes anybody can name, as in [`ask`].
    if info.size == 0 {
        return false;
    }
    // The plane holds no types, so whatever the access named is not a thing this question is in
    // terms of, and carrying it would suggest the check compares against it.
    info.tbaa = None;

    let span = func.span(read);
    let args = func.push_values(&[capability, pointer]);
    let extra = Extra::Mem(func.add_mem(info));
    let data = InstData { args, extra, ..InstData::new(Opcode::CheckInit) };
    let asked = func.create_inst(data, &[], span);
    func.insert_before(asked, read);
    true
}

/// Puts a `meta_init_copy` immediately after one copy, carrying whether its source held anything
/// over to its destination.
///
/// The other half of the same write, and the thing that makes an infoleak visible rather than what
/// hides it. A copy writes no values of its own: whether a destination byte holds anything is
/// whether the byte it came from did, and the only place that is written down is the plane over the
/// source. So this names two ranges and a length and nothing else, exactly as the type plane's
/// carriage does.
///
/// A structure filled member by member and then handed whole to `write` or to a socket is the case
/// worth stating. Marking the destination written would lose it, because the bytes that leave the
/// program would be bytes the plane had just been told were fine, and those are exactly the bytes
/// of CWE-200. Carrying the source's answer keeps the padding unwritten all the way to the
/// boundary, which is where the read that matters happens.
fn moved(func: &mut Func, copy: Inst) -> bool {
    let Extra::Mem(info) = func[copy].extra else { return false };
    // As in `carry`: the verifier refuses a copy whose payload says zero, so this is a shape that
    // does not arise rather than a case being handled.
    let size = func[info].size;
    if size == 0 {
        return false;
    }
    let [to, from] = func[func[copy].args] else { return false };

    let span = func.span(copy);
    let (made, length) = extent(func, copy, size);
    let args = func.push_values(&[to, from, length]);
    let data = InstData { args, ..InstData::new(Opcode::MetaInitCopy) };
    let carried = func.create_inst(data, &[], span);
    func.insert_after(carried, made);
    true
}

/// The constant a plane write over a range reads its length from, put in just after `at`.
///
/// Gives back the instruction as well as the value, because the caller inserts itself after the
/// constant rather than after `at`: both go in the same place, and the one that goes in second ends
/// up in front of the one that went in first.
///
/// Written in sixty four bits here and put into the target's width by [`lower::lower`], which is
/// where the only thing that knows the target's width is.
fn extent(func: &mut Func, at: Inst, size: u64) -> (Inst, Value) {
    let span = func.span(at);
    let word = Type::int(64);
    let extra = Extra::Imm(func.add_imm(Imm::int(i128::from(size), word)));
    let made = func.create_inst(InstData { extra, ..InstData::new(Opcode::IConst) }, &[word], span);
    func.insert_after(made, at);
    let length = func[made].results().next().expect("a constant created with one result has one");
    (made, length)
}

/// How many bytes an access covers.
///
/// An ordinary `load` or `store` leaves the `size` field of its payload at zero and takes its width
/// from the type instead, which is fine for an access and no use at all to a check: a check is
/// asked how many bytes are being touched and has no type of its own to read. So the width is
/// worked out here and written into the copy of the payload the check carries, and an access that
/// did fill the field in keeps what it said.
///
/// `width` is the target's pointer width in bytes, and it is a parameter because a pointer is the
/// one type in the IR that has no width of its own. Reading a zero off `Type::PTR` and passing it
/// on is what made every check over a pointer decide over a single byte, which is #953.
fn covered(func: &Func, access: Inst, stated: u64, width: u64) -> u64 {
    if stated != 0 {
        return stated;
    }
    // A `load` produces the value and a `store` takes it as its first operand.
    let ty = match func[access].opcode {
        Opcode::Load => func[access].results().next().map(|value| func[value].ty),
        Opcode::Store => func[func[access].args].first().map(|&value| func[value].ty),
        _ => None,
    };
    ty.map_or(0, |ty| {
        if ty.is_ptr() {
            return width;
        }
        u64::from(ty.bits().div_ceil(8)) * u64::from(ty.lanes())
    })
}

/// Puts `cap_of` and `check_deriv` immediately before one `ptr_add`.
///
/// Judgement J2, which is the one that catches a pointer walking off its object *before* anything
/// is read through it. C says computing such a pointer is already undefined, and catching it here
/// rather than at the eventual access is what lets the report name the loop that ran too far
/// instead of whatever unrelated line finally dereferenced the result.
///
/// The check is handed the pointer the derivation produced, so it goes immediately after the
/// derivation rather than in front of it like the access checks. That is what section 6.2.2's
/// third operand means: the judgement is about where the derived pointer landed, and there is
/// nothing to decide before it has landed.
///
/// The fourth operand is the stride, which is how wide one element of whatever is being stepped
/// over is. Document 03 section 3.1 widened S5's window to `[lo - stride, hi]`, so the runtime
/// cannot decide the low end without it, and it is a value rather than a constant because a walk
/// over a variable length array steps by a width the program computes.
fn derivation(func: &mut Func, add: Inst) -> bool {
    let Some(&base) = func[func[add].args].first() else { return false };
    if !func[base].ty.is_ptr() {
        return false;
    }
    let Some(derived) = func[add].results().next() else { return false };

    let span = func.span(add);
    let width = stride(func, add);
    let capability = cap_of(func, base, add);
    let args = func.push_values(&[capability, base, derived, width]);
    let check = func.create_inst(InstData { args, ..InstData::new(Opcode::CheckDeriv) }, &[], span);
    func.insert_after(check, add);
    true
}

/// How wide one element of the thing a `ptr_add` steps over is.
///
/// C computes a byte offset before the pointer arithmetic happens, so `ptr_add` takes bytes and the
/// element width is not in it. What is in it is the shape the frontend left behind, because this
/// pass runs before the optimizer and the offset operand is still exactly what lowering emitted:
/// `mul index, k` for a constant width, `mul index, w` for one the program computes, either of them
/// under a `sub 0, ...` for a walk that goes backwards, and the bare index when the width is one.
///
/// So the width is read back off that shape. Getting it wrong is not a soundness question: the
/// stride only decides how far below an object a derivation may land before it is refused, and an
/// access below the object is refused by judgement J1 either way. A shape nobody recognises answers
/// one byte, which is the strict reading of C and is where this check was before the window moved.
fn stride(func: &mut Func, add: Inst) -> Value {
    // The offset is the one operand of a `ptr_add` that is an integer, so its type is the width an
    // address is computed in and is the type the check's fourth operand has to have.
    let Some(&offset) = func[func[add].args].get(1) else { return one(func, add, Type::int(64)) };
    let word = func[offset].ty;
    // A walk that goes backwards negates the offset rather than the width, so the shape underneath
    // is the same one a forward walk has.
    let forwards = match operand_of(func, offset, Opcode::Sub, 0) {
        Some(zero) if is_zero(func, zero) => operand_of(func, offset, Opcode::Sub, 1),
        _ => None,
    };
    let scaled = forwards.unwrap_or(offset);
    match operand_of(func, scaled, Opcode::Mul, 1) {
        // The width is the right operand because `step` builds the multiply that way round, with
        // the index on the left and the size of one element on the right.
        Some(width) if func[width].ty == word => width,
        _ => one(func, add, word),
    }
}

/// Operand `index` of the instruction that produced `value`, when that instruction is `opcode`.
fn operand_of(func: &Func, value: Value, opcode: Opcode, index: usize) -> Option<Value> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != opcode {
        return None;
    }
    func[func[inst].args].get(index).copied()
}

/// Whether a value is a constant zero, which is the left half of how a backwards walk is spelled.
fn is_zero(func: &Func, value: Value) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    match func[inst].extra {
        Extra::Imm(imm) if func[inst].opcode == Opcode::IConst => func[imm].bits() == 0,
        _ => false,
    }
}

/// A stride of one byte, which is what a shape this pass does not recognise answers.
fn one(func: &mut Func, at: Inst, ty: Type) -> Value {
    let span = func.span(at);
    let extra = Extra::Imm(func.add_imm(Imm::int(1, ty)));
    let made = func.create_inst(InstData { extra, ..InstData::new(Opcode::IConst) }, &[ty], span);
    func.insert_before(made, at);
    func[made].results().next().expect("a constant created with one result has one")
}

/// Puts a `cap_of` for `pointer` immediately before `at`, and gives back what it produced.
fn cap_of(func: &mut Func, pointer: Value, at: Inst) -> Value {
    let span = func.span(at);
    let args = func.push_values(&[pointer]);
    let cap =
        func.create_inst(InstData { args, ..InstData::new(Opcode::CapOf) }, &[Type::CAP], span);
    func.insert_before(cap, at);
    func[cap].results().next().expect("cap_of produces one value")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Builder, Flags, MemInfo, MemOrder, Meta, MetaNode, PlaneNode, Restrict, Signature,
        TbaaNode, print_func, verify_func,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// A module to record into, and the plane entries it holds.
    ///
    /// The plane is the module's, so a test that instruments a bare function still has to have one
    /// to hand. It is empty of types here, since the functions these tests build name none.
    fn planed(names: &mut Interner, unit: &str) -> (Module, Plane) {
        let mut module = Module::new(names.intern(unit), &target());
        let plane = Plane::build(&mut module);
        (module, plane)
    }

    /// A function that loads through its parameter and stores what it read back.
    fn one_of_each(names: &mut Interner) -> Func {
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("both"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);

        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        let loaded = b.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, i32_);
        let args = b.func().push_values(&[loaded, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[loaded]);
        func
    }

    /// The same shape, with the two accesses said to go through two `restrict` pointers of a block.
    fn promising(names: &mut Interner) -> Func {
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("kernel"),
            Signature::new().with_params(&[Type::PTR, Type::PTR]),
        );
        let entry = func.create_block();
        let to = func.append_param(entry, Type::PTR);
        let from = func.append_param(entry, Type::PTR);

        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict { clique: 1, base: 1 },
        };
        let mut b = Builder::new(&mut func, entry);
        let read = MemInfo { restrict: Restrict { clique: 1, base: 2 }, ..info };
        let loaded = b.load(i32_, from, read, Flags::default());
        b.store(loaded, to, info, Flags::default());
        b.ret(&[]);
        func
    }

    #[test]
    fn the_restrict_checks_wait_until_the_build_asks_for_them() {
        // The one check in this crate that is off by default. What it costs is paid by the blocks
        // that declare `restrict` pointers and nobody else, and what it reports includes programs
        // the standard permits, so which it is is the build's decision. `rucc_session::Promise` is
        // where that is argued.
        let mut names = Interner::new();
        let (_, plane) = planed(&mut names, "kernel.c");

        let mut quiet = promising(&mut names);
        let counts = insert(&mut quiet, &plane, 8, Subobject::Off, Promise::Off);
        assert_eq!((counts.promised, counts.scoped), (0, 0));

        let mut asked = promising(&mut names);
        let counts = insert(&mut asked, &plane, 8, Subobject::Off, Promise::Blocks);
        assert_eq!((counts.promised, counts.scoped), (2, 1));
    }

    #[test]
    fn every_access_gets_a_bounds_check_and_a_lifetime_check() {
        let mut names = Interner::new();
        let mut func = one_of_each(&mut names);
        let (module, plane) = planed(&mut names, "both.c");
        assert_eq!(
            insert(&mut func, &plane, 8, Subobject::Off, Promise::Off),
            Counts { checked: 2, live: 2, judged: 1, wrote: 1, filled: 1, ..Counts::default() }
        );

        assert_eq!(
            print_func(&module, &func, &names),
            // The plane writes are after the store and not in front of it. The bytes were stored
            // through that type, and were stored at all, once the store has happened, and the
            // check in front of it may yet refuse the store both of them are about.
            "func @both(ptr) -> i32, linkage(external) {\n\
             block0(%0: ptr):\n    \
             %1 = cap_of %0\n    \
             check_bounds %1, %0, size 4, align 4\n    \
             check_live %1, %0\n    \
             check_init %1, %0, size 4, align 4\n    \
             %2 = load.i32 %0, size 4, align 4\n    \
             %3 = cap_of %0\n    \
             check_bounds %3, %0, size 4, align 4\n    \
             check_live %3, %0\n    \
             store %2 -> %0, size 4, align 4\n    \
             %4 = iconst.i64 4\n    \
             meta_type %0, %4, tbaa !1\n    \
             %5 = iconst.i64 4\n    \
             meta_init %0, %5\n    \
             return %2\n\
             }\n"
        );
    }

    /// A function that loads a pointer through its parameter.
    ///
    /// The payload states no size, which is what the front end emits: a load takes its width from
    /// the type it produces, and for a pointer that is the one type with no width of its own.
    fn one_pointer_read(names: &mut Interner) -> Func {
        let mut func = Func::new(
            names.intern("deref"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[Type::PTR]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);

        let info = MemInfo {
            size: 0,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        let loaded = b.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, Type::PTR);
        b.ret(&[loaded]);
        func
    }

    #[test]
    fn an_access_that_reads_a_pointer_is_checked_over_the_targets_pointer_width() {
        // A pointer is the one type in the IR with no width of its own, so the width has to come
        // from the target. Answering zero is what left a bounds check over a pointer deciding
        // about a single byte and left the init question out of it altogether, which was #953.
        let mut names = Interner::new();
        let mut func = one_pointer_read(&mut names);
        let (module, plane) = planed(&mut names, "deref.c");
        assert_eq!(
            insert(&mut func, &plane, 8, Subobject::Off, Promise::Off),
            Counts { checked: 1, live: 1, filled: 1, ..Counts::default() }
        );

        let printed = print_func(&module, &func, &names);
        assert!(printed.contains("check_bounds %1, %0, size 8, align 8\n"), "{printed}");
        assert!(printed.contains("check_init %1, %0, size 8, align 8\n"), "{printed}");
    }

    #[test]
    fn a_pointer_width_of_four_is_what_a_thirty_two_bit_target_gets() {
        // The number is the target's and not this crate's, so a build for a target where a pointer
        // is four bytes asks about four.
        let mut names = Interner::new();
        let mut func = one_pointer_read(&mut names);
        let (module, plane) = planed(&mut names, "deref.c");
        insert(&mut func, &plane, 4, Subobject::Off, Promise::Off);

        let printed = print_func(&module, &func, &names);
        assert!(printed.contains("check_bounds %1, %0, size 4, align 8\n"), "{printed}");
    }

    /// A module with one aliasing node under the root, and the plane built over it.
    ///
    /// Two nodes rather than one, because the root is the character type and a type of its own has
    /// to hang under something. What comes back is the module, the plane, and the node for `int`.
    fn typed(names: &mut Interner, unit: &str) -> (Module, Plane, Meta) {
        let mut module = Module::new(names.intern(unit), &target());
        let root = names.intern("char");
        let root =
            module.add_meta(MetaNode::Tbaa(TbaaNode { name: root, parent: None, offset: 0 }));
        let int = names.intern("int");
        let int =
            module.add_meta(MetaNode::Tbaa(TbaaNode { name: int, parent: Some(root), offset: 0 }));
        let plane = Plane::build(&mut module);
        (module, plane, int)
    }

    /// A function that reads through its parameter as an `int`, naming that type on the access.
    fn reading(names: &mut Interner, node: Option<Meta>) -> Func {
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("read"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let info = MemInfo {
            size: 0,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: node,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        let loaded = b.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, i32_);
        b.ret(&[loaded]);
        func
    }

    #[test]
    fn a_read_asks_the_plane_whether_the_bytes_agree_with_the_type_it_reads_them_as() {
        // Judgement J3, which is what the two plane writes were recorded for. The question is put
        // in the plane's vocabulary rather than the aliasing tree's, so what the check carries is
        // the entry for `int` and not the node for it.
        let mut names = Interner::new();
        let (module, plane, int) = typed(&mut names, "read.c");
        let mut func = reading(&mut names, Some(int));

        assert_eq!(insert(&mut func, &plane, 8, Subobject::Off, Promise::Off).asked, 1);

        let printed = print_func(&module, &func, &names);
        let entry = plane.entry(Some(int));
        assert_eq!(module[entry], MetaNode::Plane(PlaneNode::Type(int)));
        // Four bytes, which the payload does not say and the type of the value read does, and the
        // check is in front of the read rather than after it.
        let wanted = format!("check_type %1, %0, size 4, align 4, tbaa !{}\n", entry.index());
        assert!(printed.contains(&wanted), "{printed}");
        let asked = printed.find(&wanted).expect("the check is there");
        let read = printed.find("load.i32").expect("and so is the read");
        assert!(asked < read, "{printed}");

        if let Err(errors) = verify_func(&module, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_read_the_front_end_named_no_type_for_asks_nothing() {
        // An aggregate, an array, or anything else reached by address. The plane's untyped entry
        // means bytes nothing has stored through, which is a different statement from the front
        // end not having said what the access is through, and asking with it would refuse every
        // read of a structure whose members were stored through their own types.
        let mut names = Interner::new();
        let (module, plane, _) = typed(&mut names, "copy.c");
        let mut func = reading(&mut names, None);

        assert_eq!(insert(&mut func, &plane, 8, Subobject::Off, Promise::Off).asked, 0);
        let printed = print_func(&module, &func, &names);
        assert!(!printed.contains("check_type"), "{printed}");
    }

    /// A function that writes through its parameter as an `int`, naming that type on the access.
    fn writing(names: &mut Interner, node: Option<Meta>) -> Func {
        let i32_ = Type::int(32);
        let mut func =
            Func::new(names.intern("write"), Signature::new().with_params(&[Type::PTR, i32_]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let v = func.append_param(entry, i32_);
        let info = MemInfo {
            size: 0,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: node,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[v, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[]);
        func
    }

    #[test]
    fn a_store_asks_the_plane_too_once_the_build_has_said_the_member_matters() {
        // Row S4, which is a write that leaves one member and lands in the next. Read literally a
        // store like that is a program retyping storage it owns, which C 6.5 permits, so the
        // question is only put when somebody asked for it to be put.
        let mut names = Interner::new();
        let (module, plane, int) = typed(&mut names, "member.c");
        let mut func = writing(&mut names, Some(int));

        let counts = insert(&mut func, &plane, 8, Subobject::Members, Promise::Off);
        assert_eq!((counts.asked, counts.judged), (1, 1));

        let printed = print_func(&module, &func, &names);
        let entry = plane.entry(Some(int));
        let wanted = format!("check_type %2, %0, size 4, align 4, tbaa !{}\n", entry.index());
        assert!(printed.contains(&wanted), "{printed}");
        // In front of the store, because the bytes say what they said before it runs, and the
        // recording this pass makes afterwards is what would make the answer yes.
        let asked = printed.find(&wanted).expect("the check is there");
        let wrote = printed.find("store %1").expect("and so is the store");
        let recorded = printed.find("meta_type").expect("and so is the recording");
        assert!(asked < wrote && wrote < recorded, "{printed}");

        if let Err(errors) = verify_func(&module, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_store_the_front_end_named_no_type_for_asks_nothing_whatever_the_build_asked() {
        // The same reason a read of one does not. An access with no aliasing node is one the front
        // end did not say the type of, which is not the same as bytes nothing has been stored
        // through, and the plane has no way to tell the question apart from the answer.
        let mut names = Interner::new();
        let (module, plane, _) = typed(&mut names, "aggregate.c");
        let mut func = writing(&mut names, None);

        assert_eq!(insert(&mut func, &plane, 8, Subobject::Members, Promise::Off).asked, 0);
        let printed = print_func(&module, &func, &names);
        assert!(!printed.contains("check_type"), "{printed}");
    }

    #[test]
    fn a_store_answers_the_question_rather_than_asking_it() {
        // The plane covers storage the allocator reported, which is the storage C gives no declared
        // type, and the effective type of one of those is whatever the last store set. So a store
        // cannot disagree with the plane unless the build asked it to, and a check in front of one
        // by default would refuse the reuse of a buffer that the standard permits.
        let mut names = Interner::new();
        let (module, plane, int) = typed(&mut names, "write.c");
        let i32_ = Type::int(32);
        let mut func =
            Func::new(names.intern("write"), Signature::new().with_params(&[Type::PTR, i32_]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let v = func.append_param(entry, i32_);
        let info = MemInfo {
            size: 0,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: Some(int),
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[v, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[]);

        let counts = insert(&mut func, &plane, 8, Subobject::Off, Promise::Off);
        assert_eq!((counts.judged, counts.asked), (1, 0));
        let printed = print_func(&module, &func, &names);
        assert!(!printed.contains("check_type"), "{printed}");
    }

    #[test]
    fn a_store_records_the_type_it_stored_through() {
        // The judgement of C 6.5, which is the half of the type plane the compiler makes rather
        // than asks. The access names a type, so the entry the store records is that type rather
        // than the distinguished value a store that names nothing records.
        let mut names = Interner::new();
        let (module, plane, int) = typed(&mut names, "typed.c");

        let i32_ = Type::int(32);
        let mut func =
            Func::new(names.intern("record"), Signature::new().with_params(&[Type::PTR, i32_]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let v = func.append_param(entry, i32_);
        let info = MemInfo {
            size: 0,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: Some(int),
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[v, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[]);

        assert_eq!(insert(&mut func, &plane, 8, Subobject::Off, Promise::Off).judged, 1);

        let printed = print_func(&module, &func, &names);
        // Four bytes, which the payload does not say and the type of the value stored does.
        assert!(printed.contains("%3 = iconst.i64 4\n"), "{printed}");
        // The entry for `int`, which is the node the plane made for the node the access named.
        let entry = plane.entry(Some(int));
        assert_eq!(module[entry], MetaNode::Plane(PlaneNode::Type(int)));
        let wanted = format!("meta_type %0, %3, tbaa !{}\n", entry.index());
        assert!(printed.contains(&wanted), "{printed}");

        if let Err(errors) = verify_func(&module, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_store_records_that_the_bytes_it_wrote_hold_something() {
        // The init plane's half of the same store. One bit per byte and nothing else, so the write
        // carries a range and no type, and the range is the width of the value stored rather than
        // anything the payload says. A store of eight bytes makes eight bytes readable however it
        // came to be written.
        let mut names = Interner::new();
        let (module, plane) = planed(&mut names, "wrote.c");

        let i64_ = Type::int(64);
        let mut func =
            Func::new(names.intern("write"), Signature::new().with_params(&[Type::PTR, i64_]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let v = func.append_param(entry, i64_);
        let info = MemInfo {
            size: 0,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[v, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[]);

        assert_eq!(insert(&mut func, &plane, 8, Subobject::Off, Promise::Off).wrote, 1);

        let printed = print_func(&module, &func, &names);
        assert!(printed.contains("%4 = iconst.i64 8\n    meta_init %0, %4\n"), "{printed}");

        if let Err(errors) = verify_func(&module, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_store_that_owns_the_padding_after_it_records_that_too() {
        // `-fsafety-init=nopadding`, which by the time it gets here is a number on the store and
        // nothing else. A `char` member with three bytes of padding behind it owns four, so the
        // record it is in comes out whole once the other member is written and the ordinary reads
        // of one, which are a `memcmp` or a hash or a `write`, are not refused. Working out what
        // the padding is takes a record's layout, which the front end has and this pass does not,
        // and that is why the number arrives rather than the mode.
        let mut names = Interner::new();
        let (module, plane) = planed(&mut names, "owns.c");

        let byte = Type::int(8);
        let mut func =
            Func::new(names.intern("write"), Signature::new().with_params(&[Type::PTR, byte]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let v = func.append_param(entry, byte);
        let info = MemInfo {
            size: 0,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 4,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[v, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[]);

        assert_eq!(insert(&mut func, &plane, 8, Subobject::Off, Promise::Off).wrote, 1);

        let printed = print_func(&module, &func, &names);
        // Four rather than the one byte the store wrote.
        assert!(printed.contains("%4 = iconst.i64 4\n    meta_init %0, %4\n"), "{printed}");
        // And the bounds check is still about the one byte the store touches, since the padding
        // is what a store records and not what it writes.
        assert!(printed.contains("check_bounds %2, %0, size 1"), "{printed}");

        if let Err(errors) = verify_func(&module, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_read_asks_whether_anything_ever_wrote_the_bytes_it_is_about_to_read() {
        // Document 03's Y6. The question carries the access's width and no type, because the plane
        // holds one bit per byte and the bit says whether anything was stored there at all.
        let mut names = Interner::new();
        let (module, plane) = planed(&mut names, "ask.c");
        let mut func = reading(&mut names, None);

        assert_eq!(insert(&mut func, &plane, 8, Subobject::Off, Promise::Off).filled, 1);

        let printed = print_func(&module, &func, &names);
        assert!(printed.contains("check_init %1, %0, size 4, align 4\n"), "{printed}");

        if let Err(errors) = verify_func(&module, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_read_the_front_end_named_no_type_for_still_asks_the_init_plane() {
        // The one place the two questions a read asks come apart. A read with no type on it has
        // nothing to ask the type plane, because the question there is which type the bytes hold,
        // and it has the same thing to ask the init plane as any other read, because the question
        // there is about the bytes rather than about the access.
        let mut names = Interner::new();
        let (_module, plane) = planed(&mut names, "untyped.c");
        let mut func = reading(&mut names, None);

        let counts = insert(&mut func, &plane, 8, Subobject::Off, Promise::Off);
        assert_eq!(counts.asked, 0);
        assert_eq!(counts.filled, 1);
    }

    #[test]
    fn a_store_asks_the_init_plane_nothing() {
        // A store writes the bytes it is about to write, so whether anything wrote them before is
        // not a question about it. Asking would refuse the first write to every fresh instance,
        // which is every program.
        let mut names = Interner::new();
        let (module, plane) = planed(&mut names, "store.c");

        let i64_ = Type::int(64);
        let mut func =
            Func::new(names.intern("write"), Signature::new().with_params(&[Type::PTR, i64_]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let v = func.append_param(entry, i64_);
        let info = MemInfo {
            size: 8,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[v, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[]);

        assert_eq!(insert(&mut func, &plane, 8, Subobject::Off, Promise::Off).filled, 0);

        let printed = print_func(&module, &func, &names);
        assert!(!printed.contains("check_init"), "{printed}");
    }

    #[test]
    fn a_read_tells_the_init_plane_nothing() {
        // A read is a question and not a judgement. Whether the bytes it read hold anything is
        // what the plane already says, and a read that wrote the plane would make every read of
        // storage nothing ever wrote look like a read of storage something did.
        let mut names = Interner::new();
        let (module, plane) = planed(&mut names, "read.c");
        let mut func = reading(&mut names, None);

        assert_eq!(insert(&mut func, &plane, 8, Subobject::Off, Promise::Off).wrote, 0);

        let printed = print_func(&module, &func, &names);
        assert!(!printed.contains("meta_init"), "{printed}");
    }

    /// A function that copies a fixed number of bytes from one of its parameters to the other.
    fn one_copy(names: &mut Interner, opcode: Opcode) -> Func {
        let mut func =
            Func::new(names.intern("move"), Signature::new().with_params(&[Type::PTR, Type::PTR]));
        let entry = func.create_block();
        let to = func.append_param(entry, Type::PTR);
        let from = func.append_param(entry, Type::PTR);

        let info = MemInfo {
            size: 24,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[to, from]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(opcode) }, &[]);
        b.ret(&[]);
        func
    }

    #[test]
    fn a_copy_carries_whatever_the_bytes_it_read_said() {
        // The other half of the judgement C 6.5 describes. A copy does not store through a type, so
        // there is nothing here for the compiler to name: what the copied bytes are is whatever the
        // bytes they came from were, and the plane over the source is the only place that is
        // written down. Without this the destination would go on saying whatever was there before.
        let mut names = Interner::new();
        let mut func = one_copy(&mut names, Opcode::Memcpy);
        let (module, plane) = planed(&mut names, "move.c");
        assert_eq!(
            insert(&mut func, &plane, 8, Subobject::Off, Promise::Off),
            Counts { carried: 1, moved: 1, ..Counts::default() }
        );

        assert_eq!(
            print_func(&module, &func, &names),
            // After the copy, for the same reason a store's judgement is after the store.
            "func @move(ptr, ptr), linkage(external) {\n\
             block0(%0: ptr, %1: ptr):\n    \
             memcpy %0, %1, size 24, align 8\n    \
             %2 = iconst.i64 24\n    \
             meta_type_copy %0, %1, %2\n    \
             %3 = iconst.i64 24\n    \
             meta_init_copy %0, %1, %3\n    \
             return\n\
             }\n"
        );

        if let Err(errors) = verify_func(&module, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_copy_whose_ranges_may_overlap_is_carried_the_same_way() {
        // `memmove` is `memcpy` with the overlap allowed, and the overlap is the runtime's problem
        // rather than this pass's: a copy writes no plane entries of its own, so the entries over
        // the source are the same ones whichever end the bytes were moved from.
        let mut names = Interner::new();
        let mut func = one_copy(&mut names, Opcode::Memmove);
        let (module, plane) = planed(&mut names, "overlap.c");
        assert_eq!(
            insert(&mut func, &plane, 8, Subobject::Off, Promise::Off),
            Counts { carried: 1, moved: 1, ..Counts::default() }
        );

        let printed = print_func(&module, &func, &names);
        assert!(printed.contains("meta_type_copy %0, %1, %2\n"), "{printed}");
        assert!(printed.contains("meta_init_copy %0, %1, %3\n"), "{printed}");
    }

    #[test]
    fn a_walk_over_elements_hands_the_check_the_width_of_one() {
        // The low end of judgement J2's window is one element below the object, so the check has
        // to be told how wide an element is. C computed a byte offset before the arithmetic
        // happened, so the width is not in the `ptr_add`, and what is in it is the multiply the
        // frontend left behind. This pass runs before the optimizer, so that shape is still there.
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("walk"),
            Signature::new().with_params(&[Type::PTR, Type::int(64)]).with_returns(&[Type::PTR]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, Type::int(64));

        let mut b = Builder::new(&mut func, entry);
        let width = b.iconst(Type::int(64), 24);
        let bytes = b.binary(Opcode::Mul, n, width, Flags::NSW);
        let args = b.func().push_values(&[p, bytes]);
        let moved = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        b.ret(&[moved]);

        let (module, plane) = planed(&mut names, "walk.c");
        insert(&mut func, &plane, 8, Subobject::Off, Promise::Off);

        assert_eq!(
            print_func(&module, &func, &names),
            "func @walk(ptr, i64) -> ptr, linkage(external) {\n\
             block0(%0: ptr, %1: i64):\n    \
             %2 = iconst.i64 24\n    \
             %3 = mul.nsw %1, %2\n    \
             %4 = cap_of %0\n    \
             %5 = ptr_add %0, %3\n    \
             check_deriv %4, %0, %5, %2\n    \
             return %5\n\
             }\n"
        );
    }

    #[test]
    fn a_walk_that_goes_backwards_is_still_a_walk_over_elements() {
        // Which is the case the whole widening is for. A walk backwards negates the byte offset
        // rather than the width, so the multiply is one instruction further down and the width is
        // the same one. Missing it here would mean `&a[-1]` getting a one byte window and being
        // refused, which is the report this change exists to stop.
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("back"),
            Signature::new().with_params(&[Type::PTR, Type::int(64)]).with_returns(&[Type::PTR]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, Type::int(64));

        let mut b = Builder::new(&mut func, entry);
        let width = b.iconst(Type::int(64), 24);
        let bytes = b.binary(Opcode::Mul, n, width, Flags::NSW);
        let zero = b.iconst(Type::int(64), 0);
        let back = b.binary(Opcode::Sub, zero, bytes, Flags::NONE);
        let args = b.func().push_values(&[p, back]);
        let moved = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        b.ret(&[moved]);

        let (module, plane) = planed(&mut names, "back.c");
        insert(&mut func, &plane, 8, Subobject::Off, Promise::Off);

        let printed = print_func(&module, &func, &names);
        assert!(printed.contains("check_deriv %6, %0, %7, %2\n"), "{printed}");
    }

    #[test]
    fn a_pointer_computed_from_another_pointer_is_checked_where_it_is_computed() {
        // Judgement J2. The pointer that walked off its object is caught at the arithmetic, not
        // at whatever line eventually reads through it, which is what lets the report name the
        // loop that ran too far. Note where the check sits: after the ptr_add, because it is
        // handed the pointer the ptr_add produced.
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("walk"),
            Signature::new().with_params(&[Type::PTR, Type::int(64)]).with_returns(&[Type::PTR]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, Type::int(64));

        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p, n]);
        let moved = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        b.ret(&[moved]);

        let (module, plane) = planed(&mut names, "walk.c");
        assert_eq!(
            insert(&mut func, &plane, 8, Subobject::Off, Promise::Off),
            Counts { derived: 1, ..Counts::default() }
        );

        assert_eq!(
            print_func(&module, &func, &names),
            // The stride is one, because the offset here is a block parameter and nothing about
            // it says what it is a count of. That is the answer a shape this pass does not
            // recognise gets, and it is the strict reading of C.
            "func @walk(ptr, i64) -> ptr, linkage(external) {\n\
             block0(%0: ptr, %1: i64):\n    \
             %2 = iconst.i64 1\n    \
             %3 = cap_of %0\n    \
             %4 = ptr_add %0, %1\n    \
             check_deriv %3, %0, %4, %2\n    \
             return %4\n\
             }\n"
        );

        if let Err(errors) = verify_func(&module, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn what_it_produces_is_a_function_the_verifier_believes() {
        // The point of inserting checks as IR is that everything downstream may treat them as
        // IR, which is only true if the result is a module the verifier accepts.
        let mut names = Interner::new();
        let mut func = one_of_each(&mut names);
        let (module, plane) = planed(&mut names, "both.c");
        insert(&mut func, &plane, 8, Subobject::Off, Promise::Off);

        if let Err(errors) = verify_func(&module, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn every_definition_in_a_module_is_walked_and_the_declarations_are_not() {
        let mut names = Interner::new();
        let one = one_of_each(&mut names);
        let mut two = one_of_each(&mut names);
        two.name = names.intern("other");
        // A declaration of a function defined somewhere else. There is no body to put a check in
        // and reaching for one would be a crash rather than a wrong answer.
        let declared = Func::new(
            names.intern("elsewhere"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[Type::int(32)]),
        );

        let mut module = Module::new(names.intern("two.c"), &target());
        module.add_func(one);
        module.add_func(two);
        module.add_func(declared);

        assert_eq!(
            run(&mut module, Subobject::Off, Promise::Off),
            Counts { checked: 4, live: 4, judged: 2, wrote: 2, filled: 2, ..Counts::default() }
        );
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_function_with_no_accesses_is_left_alone() {
        let mut names = Interner::new();
        let i32_ = Type::int(32);
        let mut func = Func::new(names.intern("nothing"), Signature::new().with_returns(&[i32_]));
        let entry = func.create_block();
        let mut b = Builder::new(&mut func, entry);
        let zero = b.iconst(i32_, 0);
        b.ret(&[zero]);

        let (_module, plane) = planed(&mut names, "nothing.c");
        let before = func.counts();
        assert_eq!(insert(&mut func, &plane, 8, Subobject::Off, Promise::Off), Counts::default());
        assert_eq!(func.counts(), before);
    }
}
