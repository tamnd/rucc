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
//! A bounds check on every access, and nothing else. Milestone S0 in
//! `spec/safe-memory/16-milestones.md` asks for the crate to exist at its rank with something real
//! enough to run on hand written IR, and this is that. The tier machinery, the other five checks,
//! the plane writes and the fact propagation are S1 and after.
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

#![doc(html_root_url = "https://docs.rs/rucc-safety/0.4.3")]

use rucc_ir::{Extra, Func, Inst, InstData, Opcode, Type, Value};

/// How many checks a run of [`insert`] put in.
///
/// Reported rather than discarded because the number of checks a function starts with is the
/// denominator of everything document 13 measures, and it is not recoverable later: by the time
/// the optimizer has run, the checks that were discharged are gone and nothing says how many
/// there were.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Accesses that were given a bounds check.
    pub checked: usize,
    /// Accesses that were not, because the pointer they go through is not a value this pass can
    /// take the capability of.
    pub skipped: usize,
}

/// Puts a bounds check in front of every access in a function.
///
/// Section 6.3: every `load` and `store` gets `check_bounds`, with the capability coming from
/// `cap_of` on the pointer operand. The size and the alignment are the access's own, since a
/// check that asked about a different number of bytes from the access it guards would be checking
/// something the program does not do.
///
/// Nothing is discharged here. A `check_bounds` on a pointer whose bounds are statically obvious
/// is still emitted, and the fact propagation in `rucc-opt` is what removes it. That split is the
/// whole design: this pass is a walk anybody can read, and the deletions are rules that are
/// verified.
pub fn insert(func: &mut Func) -> Counts {
    let mut counts = Counts::default();
    let accesses: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block).collect::<Vec<_>>())
        .filter(|&inst| matches!(func[inst].opcode, Opcode::Load | Opcode::Store))
        .collect();
    for access in accesses {
        match pointer_of(func, access) {
            Some(pointer) => {
                check(func, access, pointer);
                counts.checked += 1;
            }
            None => counts.skipped += 1,
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

/// Puts `cap_of` and `check_bounds` immediately before one access.
fn check(func: &mut Func, access: Inst, pointer: Value) {
    let span = func.span(access);
    let Extra::Mem(info) = func[access].extra else { return };
    let info = func[info];

    let args = func.push_values(&[pointer]);
    let cap =
        func.create_inst(InstData { args, ..InstData::new(Opcode::CapOf) }, &[Type::CAP], span);
    func.insert_before(cap, access);
    let capability = func[cap].results().next().expect("cap_of produces one value");

    // The check reads the same bytes the access does, so it carries the access's own payload
    // rather than a copy of it that could later disagree.
    let args = func.push_values(&[capability, pointer]);
    let extra = Extra::Mem(func.add_mem(info));
    let check =
        func.create_inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[], span);
    func.insert_before(check, access);
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Builder, MemInfo, MemOrder, Module, Restrict, Signature, print_func, verify_func,
    };
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
    fn every_access_gets_a_bounds_check() {
        let mut names = Interner::new();
        let mut func = one_of_each(&mut names);
        assert_eq!(insert(&mut func), Counts { checked: 2, skipped: 0 });

        let module = Module::new(names.intern("both.c"), &target());
        assert_eq!(
            print_func(&module, &func, &names),
            "func @both(ptr) -> i32, linkage(external) {\n\
             block0(%0: ptr):\n    \
             %1 = cap_of %0\n    \
             check_bounds %1, %0, size 4, align 4\n    \
             %2 = load.i32 %0, size 4, align 4\n    \
             %3 = cap_of %0\n    \
             check_bounds %3, %0, size 4, align 4\n    \
             store %2 -> %0, size 4, align 4\n    \
             return %2\n\
             }\n"
        );
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
