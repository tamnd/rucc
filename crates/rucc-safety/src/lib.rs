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
//! And the other end of it, in [`mod@lower`]: after the optimizer has run, every check still standing
//! becomes a call to the runtime carrying the index of a row in a table this crate puts in the
//! object. That module is where the reason S1's checks are calls rather than compares is argued.
//!
//! The type, initialization and race checks are not here, because their planes are not written
//! yet and a check against a plane nobody maintains would either report on every access or on
//! none. Those are S5 and S6. Neither are the plane writes: `meta_begin` and `meta_end` for an
//! automatic instance need the escape analysis of document 08 section 8.4, and until that exists
//! the only instances the runtime knows about are the ones the allocator reports.
//!
//! # Why the rank matters
//!
//! `rucc-safety` is rank 9, alongside `rucc-lower` and `rucc-opt`, so it can depend on neither.
//! That is the constraint and not an inconvenience: it consumes IR and produces IR, it never sees
//! the AST, and `rucc-driver` at rank 12 is what sequences it between the two.
//! `spec/safe-memory/15-integration.md` section 15.1 argues it out.
//!
//! # Stability
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-safety/0.5.1")]

pub mod lower;

pub use lower::{Descriptor, SECTION, lower};

use rucc_ir::{Extra, Func, Inst, InstData, Module, Opcode, Type, Value};

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
}

impl Counts {
    /// Adds another function's counts to these.
    fn add(&mut self, other: Counts) {
        self.checked += other.checked;
        self.live += other.live;
        self.derived += other.derived;
        self.skipped += other.skipped;
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
    let mut counts = Counts::default();
    for id in module.funcs() {
        if !module[id].is_declaration() {
            counts.add(insert(&mut module[id]));
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
pub fn insert(func: &mut Func) -> Counts {
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
                }
                None => counts.skipped += 1,
            },
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
fn derivation(func: &mut Func, add: Inst) -> bool {
    let Some(&base) = func[func[add].args].first() else { return false };
    if !func[base].ty.is_ptr() {
        return false;
    }
    let Some(derived) = func[add].results().next() else { return false };

    let span = func.span(add);
    let capability = cap_of(func, base, add);
    let args = func.push_values(&[capability, base, derived]);
    let check = func.create_inst(InstData { args, ..InstData::new(Opcode::CheckDeriv) }, &[], span);
    func.insert_after(check, add);
    true
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
    use rucc_ir::{Builder, MemInfo, MemOrder, Restrict, Signature, print_func, verify_func};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
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
        assert_eq!(insert(&mut func), Counts { checked: 2, live: 2, derived: 0, skipped: 0 });

        let module = Module::new(names.intern("both.c"), &target());
        assert_eq!(
            print_func(&module, &func, &names),
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
             return %2\n\
             }\n"
        );
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

        assert_eq!(insert(&mut func), Counts { checked: 0, live: 0, derived: 1, skipped: 0 });

        let module = Module::new(names.intern("walk.c"), &target());
        assert_eq!(
            print_func(&module, &func, &names),
            "func @walk(ptr, i64) -> ptr, linkage(external) {\n\
             block0(%0: ptr, %1: i64):\n    \
             %2 = cap_of %0\n    \
             %3 = ptr_add %0, %1\n    \
             check_deriv %2, %0, %3\n    \
             return %3\n\
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
        insert(&mut func);

        let module = Module::new(names.intern("both.c"), &target());
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

        assert_eq!(run(&mut module), Counts { checked: 4, live: 4, derived: 0, skipped: 0 });
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

        let before = func.counts();
        assert_eq!(insert(&mut func), Counts::default());
        assert_eq!(func.counts(), before);
    }
}
