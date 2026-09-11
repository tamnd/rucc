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
//! stored through, which is the judgement C 6.5 says a store makes and the thing the type check of
//! milestone S5 will later ask about, and every copy carries whatever the bytes it read said over
//! to the bytes it wrote, which is the other half of the same rule. Those two are every write the
//! type plane has. The question is not here yet, and the order is deliberate: a check against a
//! plane that only some of the writes maintain reports on programs that are correct, so the writes
//! go in first and the check goes in once they are all in.
//!
//! The initialization and race checks are not here, because their planes are not written at all and
//! a check against a plane nobody maintains would either report on every access or on none. Those
//! are the rest of S5 and S6. Neither are the other plane writes: `meta_begin` and `meta_end` for
//! an automatic instance need the escape analysis of document 08 section 8.4, and until that exists
//! the only instances the runtime knows about are the ones the allocator reports, which is also why
//! a store to a local records into a plane that is not there and costs a call that decides
//! nothing.
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

#![doc(html_root_url = "https://docs.rs/rucc-safety/0.10.19")]

pub mod boundary;
pub mod lower;
pub mod plane;
pub mod summary;
pub mod wrap;

pub use boundary::{Sites, WITNESS, witness};
pub use lower::{Descriptor, SECTION, lower};
pub use plane::Plane;
pub use summary::{Frames, Summary, summarize};
pub use wrap::{INTERPOSED, PREFIX, redirect};

use rucc_ir::{Def, Extra, Func, Imm, Inst, InstData, Module, Opcode, Type, Value};

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
pub fn run(module: &mut Module) -> Counts {
    // Before the walk, because the entries live in the module and a function is borrowed out of
    // the module while its stores are being instrumented. It is also the reason this is the entry
    // point rather than [`insert`]: there is one plane per module and every function records into
    // the same one.
    let plane = Plane::build(module);
    let mut counts = Counts::default();
    for id in module.funcs() {
        if !module[id].is_declaration() {
            counts.add(insert(&mut module[id], &plane));
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
pub fn insert(func: &mut Func, plane: &Plane) -> Counts {
    let mut counts = Counts::default();
    let insts: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in insts {
        match func[inst].opcode {
            Opcode::Load | Opcode::Store => match pointer_of(func, inst) {
                Some(pointer) => {
                    check(func, inst, pointer);
                    counts.checked += 1;
                    counts.live += 1;
                    if func[inst].opcode == Opcode::Store && judge(func, plane, inst, pointer) {
                        counts.judged += 1;
                    }
                }
                None => counts.skipped += 1,
            },
            Opcode::Memcpy | Opcode::Memmove => {
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
fn check(func: &mut Func, access: Inst, pointer: Value) {
    let span = func.span(access);
    let Extra::Mem(info) = func[access].extra else { return };
    let mut info = func[info];
    info.size = covered(func, access, info.size);

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
/// has, and it is a `meta_type` over a range because one store writes a run of bytes. It is written
/// in sixty four bits here and put into the target's width by [`lower::lower`], which is where the
/// only thing that knows the target's width is.
fn judge(func: &mut Func, plane: &Plane, store: Inst, pointer: Value) -> bool {
    let Extra::Mem(info) = func[store].extra else { return false };
    let size = covered(func, store, func[info].size);
    // A store whose width nothing states covers no bytes anybody can name, and a plane write over
    // nothing is an instruction with no effect.
    if size == 0 {
        return false;
    }
    let node = plane.entry(func[info].tbaa);

    let span = func.span(store);
    let word = Type::int(64);
    let extra = Extra::Imm(func.add_imm(Imm::int(i128::from(size), word)));
    let made = func.create_inst(InstData { extra, ..InstData::new(Opcode::IConst) }, &[word], span);
    func.insert_after(made, store);
    let length = func[made].results().next().expect("a constant created with one result has one");

    let args = func.push_values(&[pointer, length]);
    let data = InstData { args, extra: Extra::Node(node), ..InstData::new(Opcode::MetaType) };
    let judged = func.create_inst(data, &[], span);
    // After the constant it reads rather than after the store, since both go in the same place and
    // the one that goes in second ends up in front.
    func.insert_after(judged, made);
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
    let word = Type::int(64);
    let extra = Extra::Imm(func.add_imm(Imm::int(i128::from(size), word)));
    let made = func.create_inst(InstData { extra, ..InstData::new(Opcode::IConst) }, &[word], span);
    func.insert_after(made, copy);
    let length = func[made].results().next().expect("a constant created with one result has one");

    let args = func.push_values(&[to, from, length]);
    let data = InstData { args, ..InstData::new(Opcode::MetaTypeCopy) };
    let carried = func.create_inst(data, &[], span);
    // After the constant it reads rather than after the copy, since both go in the same place and
    // the one that goes in second ends up in front.
    func.insert_after(carried, made);
    true
}

/// How many bytes an access covers.
///
/// An ordinary `load` or `store` leaves the `size` field of its payload at zero and takes its width
/// from the type instead, which is fine for an access and no use at all to a check: a check is
/// asked how many bytes are being touched and has no type of its own to read. So the width is
/// worked out here and written into the copy of the payload the check carries, and an access that
/// did fill the field in keeps what it said.
fn covered(func: &Func, access: Inst, stated: u64) -> u64 {
    if stated != 0 {
        return stated;
    }
    // A `load` produces the value and a `store` takes it as its first operand.
    let ty = match func[access].opcode {
        Opcode::Load => func[access].results().next().map(|value| func[value].ty),
        Opcode::Store => func[func[access].args].first().map(|&value| func[value].ty),
        _ => None,
    };
    ty.map_or(0, |ty| u64::from(ty.bits().div_ceil(8)) * u64::from(ty.lanes()))
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
        Builder, Flags, MemInfo, MemOrder, MetaNode, PlaneNode, Restrict, Signature, TbaaNode,
        print_func, verify_func,
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

    #[test]
    fn every_access_gets_a_bounds_check_and_a_lifetime_check() {
        let mut names = Interner::new();
        let mut func = one_of_each(&mut names);
        let (module, plane) = planed(&mut names, "both.c");
        assert_eq!(
            insert(&mut func, &plane),
            Counts { checked: 2, live: 2, derived: 0, skipped: 0, judged: 1, carried: 0 }
        );

        assert_eq!(
            print_func(&module, &func, &names),
            // The plane write is after the store and not in front of it. The bytes were stored
            // through that type once the store has happened, and the check in front of it may yet
            // refuse the store it is about.
            "func @both(ptr) -> i32, linkage(external) {\n\
             block0(%0: ptr):\n    \
             %1 = cap_of %0\n    \
             check_bounds %1, %0, size 4, align 4\n    \
             check_live %1, %0\n    \
             %2 = load.i32 %0, size 4, align 4\n    \
             %3 = cap_of %0\n    \
             check_bounds %3, %0, size 4, align 4\n    \
             check_live %3, %0\n    \
             store %2 -> %0, size 4, align 4\n    \
             %4 = iconst.i64 4\n    \
             meta_type %0, %4, tbaa !1\n    \
             return %2\n\
             }\n"
        );
    }

    #[test]
    fn a_store_records_the_type_it_stored_through() {
        // The judgement of C 6.5, which is the half of the type plane the compiler makes rather
        // than asks. The access names a type, so the entry the store records is that type rather
        // than the distinguished value a store that names nothing records.
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("typed.c"), &target());
        let root = names.intern("char");
        let root =
            module.add_meta(MetaNode::Tbaa(TbaaNode { name: root, parent: None, offset: 0 }));
        let int = names.intern("int");
        let int =
            module.add_meta(MetaNode::Tbaa(TbaaNode { name: int, parent: Some(root), offset: 0 }));
        let plane = Plane::build(&mut module);

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
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[v, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[]);

        assert_eq!(insert(&mut func, &plane).judged, 1);

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
        assert_eq!(insert(&mut func, &plane), Counts { carried: 1, ..Counts::default() });

        assert_eq!(
            print_func(&module, &func, &names),
            // After the copy, for the same reason a store's judgement is after the store.
            "func @move(ptr, ptr), linkage(external) {\n\
             block0(%0: ptr, %1: ptr):\n    \
             memcpy %0, %1, size 24, align 8\n    \
             %2 = iconst.i64 24\n    \
             meta_type_copy %0, %1, %2\n    \
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
        assert_eq!(insert(&mut func, &plane), Counts { carried: 1, ..Counts::default() });

        let printed = print_func(&module, &func, &names);
        assert!(printed.contains("meta_type_copy %0, %1, %2\n"), "{printed}");
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
        insert(&mut func, &plane);

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
        insert(&mut func, &plane);

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
            insert(&mut func, &plane),
            Counts { checked: 0, live: 0, derived: 1, skipped: 0, judged: 0, carried: 0 }
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
        insert(&mut func, &plane);

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
            run(&mut module),
            Counts { checked: 4, live: 4, derived: 0, skipped: 0, judged: 2, carried: 0 }
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
        assert_eq!(insert(&mut func, &plane), Counts::default());
        assert_eq!(func.counts(), before);
    }
}
