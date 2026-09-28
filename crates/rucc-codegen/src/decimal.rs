//! The decimal floating types, as the calls to libgcc's routines for them.
//!
//! `_Decimal32`, `_Decimal64` and `_Decimal128` are carried in the binary integer decimal encoding,
//! which is what gcc uses on x86-64 and what the `__bid_` routines in libgcc compute with. No
//! instruction on this machine does arithmetic on that encoding, so every operation on one is a call.
//! Moving, loading, storing and passing one is not, because the convention puts a decimal in the same
//! vector register a binary float of its width goes in, and the rule set is written by width for
//! exactly those. So this pass is the whole of the difference, the same way [`crate::quad`] is for
//! `_Float128`.
//!
//! Every name here is built rather than looked up in [`crate::capability`], because the family is
//! regular: the operation, then `sd`, `dd` or `td` for the decimal width, then the other format's
//! letters where there is one, then the operand count for arithmetic and comparisons. The one part
//! that is not a pattern is which conversions libgcc calls `extend` and which `trunc`, and
//! `across` spells that out.
//!
//! What is not done here is left to the steps after it. A negation or a constant at thirty two or
//! sixty four bits is the sign bit and the bits, which [`crate::expand::floats`] already does by
//! width without caring what the bits mean. A constant at a hundred and twenty eight bits goes through
//! the frame in [`crate::quad`] for the same reason a `_Float128` one does. A conversion between a
//! decimal and a `__bf16` has no routine and is left for the selector to refuse by name.

use rucc_base::Interner;
use rucc_ir::{Extra, Float, FloatPred, Func, Imm, Inst, InstData, IntPred, Opcode, Type};
use rucc_target::AbiDescription;

use crate::quad::{ahead_const, becomes, call, flipped, into_call, written};

/// The width the comparison routines answer in, and the narrower of the two integer widths the
/// conversion routines take.
const NARROW: u32 = 32;

/// Rewrites every operation on a decimal float into the call that performs it.
///
/// The instructions are collected before any of them is touched, for the reason [`crate::quad`]
/// gives: a rewrite puts instructions in front of the one it replaces.
pub fn calls(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::FAdd | Opcode::FSub | Opcode::FMul | Opcode::FDiv => {
                arithmetic(func, names, abi, inst);
            }
            Opcode::FNeg => negate(func, names, abi, inst),
            Opcode::FCmp => compare(func, names, abi, inst),
            Opcode::FPExt | Opcode::FPTrunc => convert(func, names, abi, inst),
            Opcode::SIToFP | Opcode::UIToFP => from_integer(func, names, abi, inst),
            Opcode::FPToSI | Opcode::FPToUI => to_integer(func, names, abi, inst),
            _ => {}
        }
    }
}

/// The letters libgcc names a float format by, where it has a routine for it.
fn letters(ty: Type) -> Option<&'static str> {
    if !ty.is_scalar() {
        return None;
    }
    Some(match ty.format()? {
        Float::D32 => "sd",
        Float::D64 => "dd",
        Float::D128 => "td",
        Float::F16 => "hf",
        Float::F32 => "sf",
        Float::F64 => "df",
        Float::F80 => "xf",
        Float::F128 => "tf",
    })
}

/// The letters of a decimal type, and nothing for any other type.
fn decimal(ty: Type) -> Option<&'static str> {
    if ty.format().is_some_and(Float::is_decimal) { letters(ty) } else { None }
}

/// The type of an instruction's first result, or nothing where it has none.
fn produced(func: &Func, inst: Inst) -> Option<Type> {
    func[inst].first_result.map(|value| func[value].ty)
}

/// The four operations, each the routine of its name over the two operands, in place.
fn arithmetic(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(mode) = decimal(ty) else { return };
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    let operation = match func[inst].opcode {
        Opcode::FAdd => "add",
        Opcode::FSub => "sub",
        Opcode::FMul => "mul",
        _ => "div",
    };
    into_call(func, names, abi, inst, &format!("__bid_{operation}{mode}3"), &[a, b]);
}

/// The negation, which is only the sign bit at every width.
///
/// At thirty two and sixty four bits that is [`crate::expand::floats`]'s exclusive or, which reads
/// the bits by width and so is right for a decimal as it stands. At a hundred and twenty eight there
/// is no register to do it in, and `__negtf2` is a routine that flips bit one hundred and twenty
/// seven and reads nothing else, in libgcc's soft float and in compiler-rt alike, which is the whole
/// of a decimal negation too.
fn negate(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    if decimal(ty) != Some("td") {
        return;
    }
    into_call(func, names, abi, inst, "__negtf2", &[arg]);
}

/// A comparison, as the call that answers it and the test of that answer against zero.
///
/// The routines answer the way libgcc's binary ones do, so this is the table [`crate::quad`] has
/// with other names. An ordered predicate is one routine read the way its name says, an unordered
/// one is the routine for its negation read the other way round, and `one` and `ueq` are two calls.
fn compare(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    let Some(mode) = decimal(func[a].ty) else { return };
    let Extra::FloatPred(pred) = func[inst].extra else { return };
    let named = |operation: &str| format!("__bid_{operation}{mode}2");
    let answer = Type::int(NARROW);
    let single = match pred {
        FloatPred::Oeq => Some(("eq", IntPred::Eq)),
        FloatPred::Une => Some(("ne", IntPred::Ne)),
        FloatPred::Olt => Some(("lt", IntPred::Slt)),
        FloatPred::Ole => Some(("le", IntPred::Sle)),
        FloatPred::Ogt => Some(("gt", IntPred::Sgt)),
        FloatPred::Oge => Some(("ge", IntPred::Sge)),
        FloatPred::Uno => Some(("unord", IntPred::Ne)),
        FloatPred::Ord => Some(("unord", IntPred::Eq)),
        FloatPred::Ult => Some(("ge", IntPred::Slt)),
        FloatPred::Ule => Some(("gt", IntPred::Sle)),
        FloatPred::Ugt => Some(("le", IntPred::Sgt)),
        FloatPred::Uge => Some(("lt", IntPred::Sge)),
        _ => None,
    };
    if let Some((operation, test)) = single {
        let got = call(func, names, abi, inst, &named(operation), &[a, b], answer);
        let zero = ahead_const(func, inst, Imm::int(0, answer), answer);
        becomes(func, inst, Opcode::ICmp, Extra::IntPred(test), &[got, zero]);
        return;
    }
    if let FloatPred::False | FloatPred::True = pred {
        let bits = i128::from(pred == FloatPred::True);
        let extra = Extra::Imm(func.add_imm(Imm::int(bits, Type::I1)));
        becomes(func, inst, Opcode::IConst, extra, &[]);
        return;
    }
    let (FloatPred::One | FloatPred::Ueq) = pred else { return };
    let mut tested = |operation: &str, test: IntPred| {
        let got = call(func, names, abi, inst, &named(operation), &[a, b], answer);
        let zero = ahead_const(func, inst, Imm::int(0, answer), answer);
        let args = func.push_values(&[got, zero]);
        let extra = Extra::IntPred(test);
        written(func, inst, InstData { args, extra, ..InstData::new(Opcode::ICmp) }, Type::I1)
    };
    let ordered = tested("unord", IntPred::Eq);
    let different = tested("ne", IntPred::Ne);
    let (opcode, args) = if pred == FloatPred::One {
        (Opcode::And, [ordered, different])
    } else {
        let unordered = flipped(func, inst, ordered);
        let same = flipped(func, inst, different);
        (Opcode::Or, [unordered, same])
    };
    becomes(func, inst, opcode, Extra::None, &args);
}

/// A conversion between two float formats where at least one of them is a decimal.
///
/// The IR's opcode is not what picks the routine's name, [`across`] is, because libgcc does not
/// name these by which way the width goes. Between two decimals it does, with a `2` on the end.
fn convert(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    let from = func[arg].ty;
    if decimal(ty).is_none() && decimal(from).is_none() {
        return;
    }
    let (Some(into), Some(out)) = (letters(ty), letters(from)) else { return };
    let way = if across(from, ty) { "extend" } else { "trunc" };
    let tail = if decimal(ty).is_some() && decimal(from).is_some() { "2" } else { "" };
    into_call(func, names, abi, inst, &format!("__bid_{way}{out}{into}{tail}"), &[arg]);
}

/// Whether libgcc calls the conversion from one format to the other an `extend`.
///
/// It is the wider of the two answers when the widths differ, and at one width it is the conversion
/// into the decimal, so `__bid_extenddfdd` and `__bid_truncdddf` and `__bid_extendtftd` and
/// `__bid_trunctdtf`. The eighty bit format counts its eighty bits here, which is why `_Decimal64`
/// from it is `__bid_truncxfdd` and `_Decimal128` from it is `__bid_extendxftd`.
fn across(from: Type, into: Type) -> bool {
    let (a, b) = (into.bits(), from.bits());
    a > b || (a == b && decimal(into).is_some())
}

/// The width of the routine that serves an integer of this width.
///
/// libgcc has one at thirty two, sixty four and a hundred and twenty eight bits, signed and unsigned,
/// so anything narrower than thirty two goes through that one. A `__int128` is passed whole here,
/// because this pass runs above [`crate::wide`] and that step splits a call's operand the way it
/// splits any other.
fn holder(bits: u32) -> Option<(u32, &'static str)> {
    match bits {
        0..=32 => Some((32, "si")),
        33..=64 => Some((64, "di")),
        65..=128 => Some((128, "ti")),
        _ => None,
    }
}

/// An integer becoming a decimal, which is a widening to a width there is a routine at and then it.
fn from_integer(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    let from = func[arg].ty;
    let Some(mode) = decimal(ty) else { return };
    if !from.is_int() || !from.is_scalar() {
        return;
    }
    let Some((width, letters)) = holder(from.bits()) else { return };
    let signed = func[inst].opcode == Opcode::SIToFP;
    let value = if from.bits() == width {
        arg
    } else {
        let opcode = if signed { Opcode::SExt } else { Opcode::ZExt };
        let args = func.push_values(&[arg]);
        written(func, inst, InstData { args, ..InstData::new(opcode) }, Type::int(width))
    };
    let kind = if signed { "float" } else { "floatuns" };
    into_call(func, names, abi, inst, &format!("__bid_{kind}{letters}{mode}"), &[value]);
}

/// A decimal becoming an integer, which is the routine at a width there is one at and then a
/// truncation to the width the program asked for, the same way [`crate::quad`] does it.
fn to_integer(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    let Some(mode) = decimal(func[arg].ty) else { return };
    if !ty.is_int() || !ty.is_scalar() {
        return;
    }
    let Some((width, letters)) = holder(ty.bits()) else { return };
    let kind = if func[inst].opcode == Opcode::FPToSI { "fix" } else { "fixuns" };
    let routine = format!("__bid_{kind}{mode}{letters}");
    if ty.bits() == width {
        into_call(func, names, abi, inst, &routine, &[arg]);
        return;
    }
    let answer = call(func, names, abi, inst, &routine, &[arg], Type::int(width));
    becomes(func, inst, Opcode::Trunc, Extra::None, &[answer]);
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, Module, Signature, Value};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple, x86_64};

    use super::{Float, FloatPred, Func, Opcode, Type, calls};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("d.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    fn shell(names: &mut Interner, params: &[Type], returns: &[Type]) -> (Func, Block, Vec<Value>) {
        let signature = Signature::new().with_params(params).with_returns(returns);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let values = params.iter().map(|&ty| func.append_param(entry, ty)).collect();
        (func, entry, values)
    }

    /// One instruction over the parameters, run through the pass and printed.
    fn after(
        params: &[Type],
        returns: Type,
        make: impl FnOnce(&mut Builder<'_>, &[Value]) -> Value,
    ) -> String {
        let mut names = Interner::new();
        let (mut func, entry, args) = shell(&mut names, params, &[returns]);
        let mut build = Builder::new(&mut func, entry);
        let made = make(&mut build, &args);
        build.ret(&[made]);
        calls(&mut func, &mut names, x86_64::SYSV.abi);
        printed(&func, &mut names)
    }

    #[test]
    fn arithmetic_at_each_width_is_the_routine_with_that_widths_letters() {
        for (format, name) in [
            (Float::D32, "__bid_addsd3"),
            (Float::D64, "__bid_adddd3"),
            (Float::D128, "__bid_addtd3"),
        ] {
            let ty = Type::float(format);
            let text = after(&[ty, ty], ty, |build, args| {
                build.binary(Opcode::FAdd, args[0], args[1], Flags::NONE)
            });
            assert!(text.contains(name), "{text}");
            assert!(!text.contains("fadd"), "{text}");
        }
    }

    #[test]
    fn a_binary_float_is_left_for_the_steps_after_this_one() {
        let ty = Type::float(Float::F64);
        let text = after(&[ty, ty], ty, |build, args| {
            build.binary(Opcode::FMul, args[0], args[1], Flags::NONE)
        });
        assert!(text.contains("fmul"), "{text}");
        assert!(!text.contains("__bid"), "{text}");
    }

    #[test]
    fn a_comparison_is_the_routine_and_a_test_of_its_answer() {
        let ty = Type::float(Float::D64);
        let text = after(&[ty, ty], Type::I1, |build, args| {
            build.fcmp(FloatPred::Olt, args[0], args[1], Flags::NONE)
        });
        assert!(text.contains("__bid_ltdd2"), "{text}");
        assert!(text.contains("icmp slt"), "{text}");
        let text = after(&[ty, ty], Type::I1, |build, args| {
            build.fcmp(FloatPred::One, args[0], args[1], Flags::NONE)
        });
        assert!(text.contains("__bid_unorddd2") && text.contains("__bid_nedd2"), "{text}");
    }

    #[test]
    fn a_conversion_is_named_the_way_libgcc_names_it() {
        let rows = [
            (Float::D32, Float::D64, "__bid_extendsddd2"),
            (Float::D128, Float::D64, "__bid_trunctddd2"),
            (Float::F64, Float::D64, "__bid_extenddfdd"),
            (Float::D64, Float::F64, "__bid_truncdddf"),
            (Float::F80, Float::D64, "__bid_truncxfdd"),
            (Float::F80, Float::D128, "__bid_extendxftd"),
            (Float::D32, Float::F128, "__bid_extendsdtf"),
        ];
        for (from, into, name) in rows {
            let (from, into) = (Type::float(from), Type::float(into));
            let opcode = if super::across(from, into) { Opcode::FPExt } else { Opcode::FPTrunc };
            let text = after(&[from], into, |build, args| build.unary(opcode, args[0], into));
            assert!(text.contains(name), "{name}: {text}");
        }
    }

    #[test]
    fn an_integer_conversion_goes_through_a_width_there_is_a_routine_at() {
        let ty = Type::float(Float::D64);
        let text =
            after(&[Type::int(16)], ty, |build, args| build.unary(Opcode::SIToFP, args[0], ty));
        assert!(text.contains("sext") && text.contains("__bid_floatsidd"), "{text}");
        let text = after(&[ty], Type::int(8), |build, args| {
            build.unary(Opcode::FPToUI, args[0], Type::int(8))
        });
        assert!(text.contains("__bid_fixunsddsi") && text.contains("trunc"), "{text}");
        let text =
            after(&[Type::int(128)], ty, |build, args| build.unary(Opcode::UIToFP, args[0], ty));
        assert!(text.contains("__bid_floatunstidd"), "{text}");
    }
}
