//! The float remainder and the fused multiply add, as the calls to the C library that compute them.
//!
//! Neither has an instruction on any target this back end has rules for. x86-64 at its baseline has
//! no fused multiply add, since that arrived with FMA3 well above it, and no target has a remainder
//! of two floats as one instruction: the x87 `fprem` is a loop of partial remainders and SSE has
//! nothing at all. So what a remainder or a fused multiply add becomes is what gcc 16 writes for
//! `fmod` and `fma` at this baseline, which is the call to the routine of that name.
//!
//! The C library rather than libgcc, because libgcc has neither routine. `fmod` and `fma` are in
//! libm, and so are their `float` and `_Float128` neighbours, which is why a program that reaches
//! here links with `-lm` the way one calling those functions by name does.
//!
//! # Where these come from
//!
//! Today nothing in the front end writes either opcode. `%` on two floats is refused by sema, and
//! `__builtin_fma` and `__builtin_fmod` are already calls by the time they reach the IR. The
//! opcodes are in the IR because the optimizer may one day write them, and a selector meeting one
//! it has no rule for refuses the function. This step is what makes that a call instead of a
//! refusal, and it is what lets the coverage table stop listing both as a gap.
//!
//! # What is left alone
//!
//! The half float, the eighty bit format and the decimal floats. A half reaching here would be the
//! routine at `float` with a widening in front and a narrowing behind, which is
//! [`crate::half`]'s job rather than this one's, and a fused multiply add at that format is not
//! the same answer done in `float` the way the four operations are. The eighty bit format travels
//! in the argument area as bytes, which is the shape [`crate::quad`] refuses on the same grounds,
//! and a decimal float has no remainder or fused multiply add in the library at all. All of them
//! stay where they are and the selector refuses them by name.

use rucc_base::Interner;
use rucc_ir::{Float, Func, Inst, Opcode, Type};
use rucc_target::AbiDescription;

use crate::quad;

/// Rewrites every float remainder and fused multiply add into the call that computes it.
///
/// The instructions are collected first, for the reason every pass in the group does it: a call
/// that passes an argument by address writes instructions in front of the one it replaces, and the
/// walk would otherwise see them.
pub fn calls(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription) {
    let found: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in found {
        let opcode = func[inst].opcode;
        let Some(value) = func[inst].first_result else { continue };
        let Some(routine) = routine(opcode, func[value].ty) else { continue };
        let args = func[func[inst].args].to_vec();
        quad::into_call(func, names, abi, inst, routine, &args);
    }
}

/// The library routine that computes this operation at this type, where there is one.
///
/// The suffixes are the C library's own: none for `double`, `f` for `float` and `f128` for
/// `_Float128`, which glibc has had since 2.26. A vector of floats is left alone, since the
/// routines take one value each and splitting a vector into lanes is not this step's work.
fn routine(opcode: Opcode, ty: Type) -> Option<&'static str> {
    if !ty.is_scalar() {
        return None;
    }
    let format = ty.format()?;
    let names = match opcode {
        Opcode::FRem => ["fmodf", "fmod", "fmodf128"],
        Opcode::Fma => ["fmaf", "fma", "fmaf128"],
        _ => return None,
    };
    let [single, double, quad] = names;
    match format {
        Float::F32 => Some(single),
        Float::F64 => Some(double),
        Float::F128 => Some(quad),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, InstData, Module, Signature, Value};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple, x86_64};

    use super::{Float, Func, Opcode, Type, calls, routine};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("m.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    /// A function of those parameters returning that, with its entry block and its parameters.
    fn shell(names: &mut Interner, params: &[Type], returns: &[Type]) -> (Func, Block, Vec<Value>) {
        let signature = Signature::new().with_params(params).with_returns(returns);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let values = params.iter().map(|&ty| func.append_param(entry, ty)).collect();
        (func, entry, values)
    }

    /// The printed function cut into names, with the `@` a callee is printed with kept on it.
    ///
    /// Cut this finely because one routine's name is the start of another's, `@fmod` of `@fmodf`,
    /// and the opcode is printed with its type after a dot, so a search for a substring would find
    /// things that are not there.
    fn words(text: &str) -> Vec<&str> {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '@')).collect()
    }

    /// A function whose one instruction is a remainder of its two parameters at that format, run
    /// through the step and checked by the verifier.
    fn remainder(format: Float) -> String {
        let ty = Type::float(format);
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[ty, ty], &[ty]);
        let mut build = Builder::new(&mut func, entry);
        let answer = build.binary(Opcode::FRem, params[0], params[1], Flags::NONE);
        build.ret(&[answer]);
        calls(&mut func, &mut names, x86_64::SYSV.abi);
        let module = Module::new(names.intern("m.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the call is valid IR");
        printed(&func, &mut names)
    }

    /// The same for a fused multiply add of its three parameters.
    fn fused(format: Float) -> String {
        let ty = Type::float(format);
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[ty, ty, ty], &[ty]);
        let mut build = Builder::new(&mut func, entry);
        let args = build.func().push_values(&params);
        let answer = build.value(InstData { args, ..InstData::new(Opcode::Fma) }, ty);
        build.ret(&[answer]);
        calls(&mut func, &mut names, x86_64::SYSV.abi);
        let module = Module::new(names.intern("m.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the call is valid IR");
        printed(&func, &mut names)
    }

    /// Each format calls the routine with its own suffix, which is the name a C program calling it
    /// by hand would have written.
    #[test]
    fn a_remainder_is_a_call_to_the_fmod_of_its_format() {
        for (format, name) in
            [(Float::F32, "@fmodf"), (Float::F64, "@fmod"), (Float::F128, "@fmodf128")]
        {
            let text = remainder(format);
            assert!(!words(&text).contains(&"frem"), "the instruction is gone: {text}");
            assert!(words(&text).contains(&name), "{name} is called: {text}");
        }
    }

    /// The same for the fused multiply add, with the three operands in the order the program gave
    /// them, since `fma(a, b, c)` is `a * b + c` and the order is the meaning.
    #[test]
    fn a_fused_multiply_add_is_a_call_to_the_fma_of_its_format() {
        for (format, name) in
            [(Float::F32, "@fmaf"), (Float::F64, "@fma"), (Float::F128, "@fmaf128")]
        {
            let text = fused(format);
            assert!(!words(&text).contains(&"fma"), "the instruction is gone: {text}");
            assert!(words(&text).contains(&name), "{name} is called: {text}");
        }
    }

    /// The formats with no routine here keep the instruction, for the selector to refuse by name.
    #[test]
    fn the_half_the_eighty_bit_and_the_decimal_formats_have_no_routine() {
        for format in [Float::F16, Float::F80, Float::D32, Float::D64, Float::D128] {
            assert_eq!(routine(Opcode::FRem, Type::float(format)), None, "{format:?}");
            assert_eq!(routine(Opcode::Fma, Type::float(format)), None, "{format:?}");
        }
    }

    /// Nothing else is touched, so an addition at the same format is still an addition.
    #[test]
    fn an_operation_with_an_instruction_is_not_a_call() {
        assert_eq!(routine(Opcode::FAdd, Type::float(Float::F64)), None);
        assert_eq!(routine(Opcode::FMul, Type::float(Float::F32)), None);
    }
}
