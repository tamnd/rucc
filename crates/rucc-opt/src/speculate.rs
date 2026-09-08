//! Whether a value may be worked out at a point the program might not have worked it out at.
//!
//! Design: section 27.1 of `spec/optimizer/27-licm.md`, which owns this question because loop
//! invariant motion is where it is asked most, and names documents 15.6, 16.6 and 22.6 as the
//! other three that need the same answer. One function, and its default answer is no.
//!
//! # What the question is
//!
//! A value is safe to speculate at a point if working it out there cannot trap, cannot fault and
//! cannot be observed. The three verbs are separate failures and each has its own arm below. A
//! division traps, a load faults, and a volatile access is observed by something outside the
//! program whether or not it does either.
//!
//! # Why it is not [`Opcode::has_effects`]
//!
//! That whitelist answers a different question, which is whether an instruction may be deleted or
//! reordered. Signed division is on it, because dividing does nothing to memory and two divisions
//! of the same operands are the same value. It is still not safe to speculate, because a divisor
//! of zero traps and the program that was going to divide might never have got there. So the two
//! predicates overlap and neither implies the other, and a pass that uses the wrong one produces a
//! program that crashes on an input the original handled.
//!
//! # No means no rather than not yet
//!
//! Every arm that cannot prove safety returns a reason, and the reasons are public so that a pass
//! can put one in an `-fopt-info` remark rather than saying only that it declined. A caller that
//! wants a boolean has [`is_safe`].

use rucc_ir::{Block, Extra, Flags, Func, Inst, MemOrder, Opcode};

use crate::alias::{Origin, origin};
use crate::range::query::Ranges;

/// Every access to it is part of what the program does, so doing one more is doing more.
pub const VOLATILE: &str = "it is volatile and every access to it is observable";

/// Its ordering is part of the program, per document 09.5.
pub const ATOMIC: &str = "it is atomic and its place in the order is part of the program";

/// It writes memory, calls something, or is otherwise not a value being worked out.
pub const EFFECTS: &str = "it does something rather than working out a value";

/// A call, which needs the module to say what it may do.
pub const CALL: &str = "nothing here knows what the call does";

/// The divisor could be zero, which traps.
pub const BY_ZERO: &str = "the divisor is not known to be other than zero";

/// The most negative value over minus one, which traps on x86 and is undefined in C.
pub const OVERFLOW: &str = "the division could be the most negative value over minus one";

/// The address is not known to be one the program is allowed to read.
pub const ADDRESS: &str = "the address is not known to be one it may read";

/// Whether working this instruction out somewhere the program might not have is safe.
///
/// The wrapper for a caller that has nothing to say about why. Everything else should take the
/// reason and report it, because a missed optimization nobody can explain is one nobody fixes.
#[must_use]
pub fn is_safe(func: &Func, inst: Inst, ranges: &mut Ranges<'_>, at: Block) -> bool {
    why_not(func, inst, ranges, at).is_none()
}

/// Why this instruction may not be worked out early at `at`, or `None` when it may.
///
/// The answer is about the instruction and the block it is being asked about, and both are needed
/// rather than only the first. A divisor guarded by `if (d)` is non-zero inside the guard and
/// unknown outside it, so an instruction can be safe where it stands and unsafe one block earlier,
/// and a caller that asks about the wrong block gets a yes it cannot use. `at` is the block the
/// instruction would be worked out in, which for a caller asking about where it already is means
/// its own block.
///
/// What is still the caller's question is whether that block is reached and whether the operands
/// are available there. This does not ask either.
#[must_use]
pub fn why_not(
    func: &Func,
    inst: Inst,
    ranges: &mut Ranges<'_>,
    at: Block,
) -> Option<&'static str> {
    let data = func[inst];
    if data.flags.contains(Flags::VOLATILE) {
        return Some(VOLATILE);
    }
    let order = match data.extra {
        Extra::Mem(at) | Extra::Rmw(_, at) => func[at].order,
        _ => MemOrder::NotAtomic,
    };
    if order != MemOrder::NotAtomic {
        return Some(ATOMIC);
    }
    match data.opcode {
        // Integer division is the trapping arithmetic, and it is the case document 27.6 says
        // people forget because it does not look like a memory access.
        Opcode::SDiv | Opcode::SRem => division(func, inst, ranges, at, true),
        Opcode::UDiv | Opcode::URem => division(func, inst, ranges, at, false),
        // Floating point division does not trap. IEEE says a division by zero produces an
        // infinity and raises a flag, and a program that reads the flag has said so with
        // `#pragma STDC FENV_ACCESS`, which the front end turns into a volatile access.
        Opcode::Load => load(func, inst),
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => Some(CALL),
        other if other.has_effects() => Some(EFFECTS),
        _ => None,
    }
}

/// Whether this division is known not to trap.
///
/// Two ways it can, and both are asked of document 10's ranges at the block the division would be
/// in rather than at the function, because a divisor guarded by `if (d)` is only non-zero inside
/// the guard. Asking at the division's own block is what a caller wants when it is deciding
/// whether the division is safe where it already is, and it is the wrong question for a caller
/// deciding whether to move it somewhere the guard does not reach.
fn division(
    func: &Func,
    inst: Inst,
    ranges: &mut Ranges<'_>,
    at: Block,
    signed: bool,
) -> Option<&'static str> {
    let args = &func[func[inst].args];
    let (top, bottom) = (*args.first()?, *args.get(1)?);
    let ty = func[bottom].ty;
    if !ty.is_int() || !ty.is_scalar() {
        // A vector division divides lane by lane and each lane is its own question. Nothing asks
        // yet, and saying no costs a hoist that has never come up.
        return Some(BY_ZERO);
    }
    if !ranges.at(bottom, at).nonzero() {
        return Some(BY_ZERO);
    }
    if !signed {
        return None;
    }
    // The other trap is the most negative value over minus one, whose quotient is not
    // representable. Either operand ruling its half out is enough. Both are written in the
    // unsigned pattern a range holds, so minus one is every bit set at that width.
    let bits = ty.bits();
    let all_ones = u128::MAX >> (u128::BITS - bits);
    let most_negative = all_ones ^ (all_ones >> 1);
    if ranges.at(bottom, at).contains(all_ones) && ranges.at(top, at).contains(most_negative) {
        return Some(OVERFLOW);
    }
    None
}

/// Whether this load is known to be reading storage that is there.
///
/// Section 27.1 allows two proofs and this is the first: the address is a local whose size is
/// known and the bytes being read are inside it. The second, that the same address is read
/// unconditionally somewhere else in the region, needs the region and belongs to the caller that
/// has one.
///
/// A local is the case that matters, because a pointer parameter is a pointer the function was
/// handed and nothing in the function says how much of it there is.
fn load(func: &Func, inst: Inst) -> Option<&'static str> {
    let pointer = *func[func[inst].args].first()?;
    let (Origin::Local(local), Some(offset)) = origin(func, pointer) else {
        return Some(ADDRESS);
    };
    // An alloca with an operand is one whose size is worked out at run time, and the size in its
    // memory record is not the whole of it.
    let data = func[local];
    if !func[data.args].is_empty() {
        return Some(ADDRESS);
    }
    let Extra::Mem(at) = data.extra else {
        return Some(ADDRESS);
    };
    let object = i128::from(func[at].size);
    let ty = func[inst].results().next().map(|value| func[value].ty)?;
    // A pointer and a capability have no width in the IR, because how wide an address is belongs
    // to the target and this does not have one. That costs a load of a pointer out of a local.
    let bits = u64::from(ty.bits()) * u64::from(ty.lanes());
    if bits == 0 {
        return Some(ADDRESS);
    }
    let start = i128::from(offset);
    let end = start + i128::from(bits.div_ceil(8));
    if start >= 0 && end <= object { None } else { Some(ADDRESS) }
}

#[cfg(test)]
mod tests {
    use rucc_base::{Interner, Symbol};
    use rucc_ir::{
        Builder, Extra, Flags, Func, Inst, InstData, MemInfo, MemOrder, Opcode, Restrict,
        Signature, Type, Value,
    };

    use super::{ADDRESS, ATOMIC, BY_ZERO, CALL, EFFECTS, OVERFLOW, VOLATILE, is_safe, why_not};
    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::range::query::Ranges;

    /// A memory record of that many bytes with that ordering.
    fn record(size: u64, order: MemOrder) -> MemInfo {
        MemInfo { size, align: 8, order, tbaa: None, restrict: Restrict::NONE }
    }

    /// A local of that many bytes.
    fn local(build: &mut Builder<'_>, size: u64) -> Value {
        let mem = build.func().add_mem(record(size, MemOrder::NotAtomic));
        build.value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// The answer about the last instruction the caller wrote.
    ///
    /// One block, three parameters, two 32 bit integers and a pointer, and the pointer is there
    /// because the interesting thing about a load is whether the function knows how much storage
    /// is behind the address, and for a parameter it does not. The symbol is a name to call,
    /// because the interner is not the function and a caller cannot reach it through the builder.
    fn asked(write: impl FnOnce(&mut Builder<'_>, [Value; 3], Symbol)) -> Option<&'static str> {
        let mut names = Interner::new();
        let params = [Type::int(32), Type::int(32), Type::PTR];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let callee = names.intern("g");
        let entry = func.create_block();
        let handed = params.map(|ty| func.append_param(entry, ty));
        let mut build = Builder::new(&mut func, entry);
        write(&mut build, handed, callee);
        build.ret(&[]);
        let last: Inst = func
            .insts(entry)
            .filter(|inst| !func.is_terminator(*inst))
            .last()
            .expect("the caller wrote one");
        let cfg = Cfg::new(&func);
        let dom = Dominators::new(&cfg);
        let mut ranges = Ranges::new(&func, &cfg, &dom);
        let answer = why_not(&func, last, &mut ranges, entry);
        assert_eq!(is_safe(&func, last, &mut ranges, entry), answer.is_none(), "the two agree");
        answer
    }

    #[test]
    fn adding_two_numbers_may_happen_early() {
        let why = asked(|build, [a, b, _], _| {
            build.binary(Opcode::Add, a, b, Flags::NONE);
        });
        assert_eq!(why, None);
    }

    #[test]
    fn dividing_by_something_that_could_be_zero_may_not() {
        let why = asked(|build, [a, b, _], _| {
            build.binary(Opcode::UDiv, a, b, Flags::NONE);
        });
        assert_eq!(why, Some(BY_ZERO));
    }

    #[test]
    fn dividing_by_a_number_the_ranges_have_settled_may() {
        let why = asked(|build, [a, _, _], _| {
            let three = build.iconst(Type::int(32), 3);
            build.binary(Opcode::UDiv, a, three, Flags::NONE);
        });
        assert_eq!(why, None);
    }

    #[test]
    fn a_remainder_is_the_same_question_as_a_division() {
        let why = asked(|build, [a, b, _], _| {
            build.binary(Opcode::SRem, a, b, Flags::NONE);
        });
        assert_eq!(why, Some(BY_ZERO));
    }

    #[test]
    fn the_most_negative_value_over_minus_one_is_the_other_trap() {
        // Minus one is not zero, so the first question passes and the second one is what catches
        // this. Section 27.6 names it as the one people forget.
        let why = asked(|build, [a, _, _], _| {
            let minus_one = build.iconst(Type::int(32), -1);
            build.binary(Opcode::SDiv, a, minus_one, Flags::NONE);
        });
        assert_eq!(why, Some(OVERFLOW));
    }

    #[test]
    fn either_operand_ruling_out_its_half_of_the_overflow_is_enough() {
        let why = asked(|build, [_, _, _], _| {
            let one = build.iconst(Type::int(32), 1);
            let minus_one = build.iconst(Type::int(32), -1);
            build.binary(Opcode::SDiv, one, minus_one, Flags::NONE);
        });
        assert_eq!(why, None);
    }

    #[test]
    fn an_unsigned_division_has_no_overflow_to_ask_about() {
        // The same operands as the signed case above, which does not get past the second question.
        let why = asked(|build, [a, _, _], _| {
            let minus_one = build.iconst(Type::int(32), -1);
            build.binary(Opcode::UDiv, a, minus_one, Flags::NONE);
        });
        assert_eq!(why, None);
    }

    #[test]
    fn a_volatile_load_may_not_happen_early() {
        let why = asked(|build, [_, _, _], _| {
            let slot = local(build, 4);
            build.load(Type::int(32), slot, record(4, MemOrder::NotAtomic), Flags::VOLATILE);
        });
        assert_eq!(why, Some(VOLATILE), "the address is fine and the volatility is not");
    }

    #[test]
    fn an_atomic_load_may_not_happen_early() {
        let why = asked(|build, [_, _, _], _| {
            let slot = local(build, 4);
            build.atomic_load(Type::int(32), slot, record(4, MemOrder::Acquire), Flags::NONE);
        });
        assert_eq!(why, Some(ATOMIC));
    }

    #[test]
    fn a_store_may_not_happen_early() {
        let why = asked(|build, [a, _, _], _| {
            let slot = local(build, 4);
            build.store(a, slot, record(4, MemOrder::NotAtomic), Flags::NONE);
        });
        assert_eq!(why, Some(EFFECTS));
    }

    #[test]
    fn a_call_may_not_happen_early() {
        let why = asked(|build, [_, _, _], callee| {
            let signature = build.func().add_signature(Signature::new());
            build.call(callee, signature, &[]);
        });
        assert_eq!(why, Some(CALL));
    }

    #[test]
    fn a_load_of_bytes_that_are_inside_a_local_may_happen_early() {
        let why = asked(|build, [_, _, _], _| {
            let slot = local(build, 4);
            build.load(Type::int(32), slot, record(4, MemOrder::NotAtomic), Flags::NONE);
        });
        assert_eq!(why, None);
    }

    #[test]
    fn a_load_that_runs_off_the_end_of_a_local_may_not() {
        let why = asked(|build, [_, _, _], _| {
            let slot = local(build, 2);
            build.load(Type::int(32), slot, record(4, MemOrder::NotAtomic), Flags::NONE);
        });
        assert_eq!(why, Some(ADDRESS), "four bytes read out of two bytes of storage");
    }

    #[test]
    fn a_load_at_an_offset_that_is_still_inside_may() {
        let why = asked(|build, [_, _, _], _| {
            let slot = local(build, 8);
            let four = build.iconst(Type::int(64), 4);
            let at = build.binary(Opcode::PtrAdd, slot, four, Flags::NONE);
            build.load(Type::int(32), at, record(4, MemOrder::NotAtomic), Flags::NONE);
        });
        assert_eq!(why, None);
    }

    #[test]
    fn a_load_through_a_pointer_the_function_was_handed_may_not() {
        // Nothing in the function says how much storage is behind it, and section 27.1 says the
        // answer to a question nobody can settle is no.
        let why = asked(|build, [_, _, pointer], _| {
            build.load(Type::int(32), pointer, record(4, MemOrder::NotAtomic), Flags::NONE);
        });
        assert_eq!(why, Some(ADDRESS));
    }
}
