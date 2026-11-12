//! The float remainder and the fused multiply add, as the calls to the C library that compute them.
//!
//! No target this back end has rules for has a remainder of two floats as one instruction: the x87
//! `fprem` is a loop of partial remainders and SSE has nothing at all. x86-64 at its baseline has
//! no fused multiply add either, since that arrived with FMA3 well above it. So what a remainder
//! or a fused multiply add becomes is what gcc 16 writes for `fmod` and `fma` at this baseline,
//! which is the call to the routine of that name.
//!
//! The exceptions are a fused multiply add at `float` or `double` in a function built for `fma`
//! on x86-64 and i386, which the selector writes as `vfmadd`, and the same on AArch64 in every
//! function, which it writes as `fmadd`. See `fuse`.
//!
//! The C library rather than libgcc, because libgcc has neither routine. `fmod` and `fma` are in
//! libm, and so are their `float` and `_Float128` neighbours, which is why a program that reaches
//! here links with `-lm` the way one calling those functions by name does.
//!
//! # Where these come from
//!
//! Nothing in the front end writes either opcode. `%` on two floats is refused by sema, and
//! `__builtin_fma` and `__builtin_fmod` are already calls by the time they reach the IR. The
//! opcodes are in the IR because the optimizer may one day write them, and a selector meeting one
//! it has no rule for refuses the function. This step is what makes that a call instead of a
//! refusal, and it is what lets the coverage table stop listing both as a gap.
//!
//! The fused multiply add is also written here, by `fuse`, from a call to `fma` or `fmaf` that
//! the front end marked as the library's, when the selector can answer it.
//!
//! # The square root
//!
//! A call to the library's `sqrt` or `sqrtf` is `sqrtsd` or `sqrtss` on x86-64 when the level
//! optimizes, as gcc writes it. Under `-fmath-errno`, which is the default, the routine must set
//! `errno` for an operand below zero, so the call stays behind a check for that case and the
//! instruction answers every other operand. A NaN and a negative zero are not below zero, and the
//! routine sets nothing for them. Under `-fno-math-errno` the instruction is the whole answer.
//! See `roots`.
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
use rucc_ir::{
    AsmInfo, BlockCallList, Builder, Extra, Flags, Float, FloatPred, Func, Inst, InstData, Opcode,
    Type, Value,
};
use rucc_target::AbiDescription;

use crate::quad;
use crate::retry::replace;

/// What a call to the library's `sqrt` or `sqrtf` becomes. See `roots`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Roots {
    /// The call stays, which is `-O0` and every machine but x86-64 with SSE.
    #[default]
    Calls,
    /// The instruction, and the call for an operand below zero, which is `-fmath-errno`.
    Checked,
    /// The instruction alone, which is `-fno-math-errno`.
    Bare,
}

/// Rewrites every float remainder and fused multiply add into the call that computes it.
///
/// When `fused` is true, the selector has a rule for a fused multiply add at `float` and `double`
/// and the function is built for the extension that has it. Then a call to `fma` or `fmaf` that
/// means the library's function becomes the instruction first, and the instruction at those two
/// types stays for the selector. See `fuse`.
///
/// The instructions are collected first, for the reason every pass in the group does it: a call
/// that passes an argument by address writes instructions in front of the one it replaces, and the
/// walk would otherwise see them.
///
/// `roots` says what a call to the library's `sqrt` or `sqrtf` becomes. See [`Roots`].
pub fn calls(
    func: &mut Func,
    names: &mut Interner,
    abi: &'static AbiDescription,
    fused: bool,
    roots: Roots,
) {
    if fused {
        fuse(func, names);
    }
    if roots != Roots::Calls {
        self::roots(func, names, roots);
    }
    let found: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in found {
        let opcode = func[inst].opcode;
        let Some(value) = func[inst].first_result else { continue };
        let ty = func[value].ty;
        if fused && opcode == Opcode::Fma && is_single_or_double(ty) {
            continue;
        }
        let Some(routine) = routine(opcode, ty) else { continue };
        let args = func[func[inst].args].to_vec();
        quad::into_call(func, names, abi, inst, routine, &args);
    }
}

/// Rewrites each call to `fma` or `fmaf` that has [`Flags::LIBRARY`] into the fused multiply add.
///
/// The call and the instruction give the same answer, since the routine is defined as the product
/// and the sum with one rounding, which is what the instruction does. gcc 16 does the same in a
/// function built for `fma`. It does it for `__builtin_fma` at every level and for a call to
/// `fma` from `-O1` up, and this does it for both at every level.
///
/// A call stays a call when it does not have the shape of the routine, which is three operands
/// and one answer all of the routine's type, and when it has an unwind edge, which the instruction
/// cannot carry. The front end does not mark a call to a function the program defines itself.
fn fuse(func: &mut Func, names: &Interner) {
    let found: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in found {
        let data = &func[inst];
        let Extra::Call(info) = data.extra else { continue };
        if data.opcode != Opcode::Call || !data.flags.contains(Flags::LIBRARY) {
            continue;
        }
        let Some(callee) = func[info].callee else { continue };
        let ty = match names.resolve(callee) {
            "fma" => Type::float(Float::F64),
            "fmaf" => Type::float(Float::F32),
            _ => continue,
        };
        let args = &func[data.args];
        let shaped = args.len() == 3
            && args.iter().all(|&arg| func[arg].ty == ty)
            && data.results().map(|value| func[value].ty).eq([ty]);
        if !shaped || func.unwinds_to_pad(inst) {
            continue;
        }
        let data = &mut func[inst];
        data.opcode = Opcode::Fma;
        data.extra = Extra::None;
        data.flags = data.flags.intersection(Flags::legal_on(Opcode::Fma));
    }
}

/// Rewrites each call to `sqrt` or `sqrtf` that has [`Flags::LIBRARY`] into the instruction.
///
/// The instruction is an `asm` statement with gcc's own template for it, `%vsqrtsd` with the
/// operands in the AT&T and the Intel order. `%v` writes the VEX form in a function built for AVX.
/// The output is in a vector register of its own and the operand in another, which is the shape
/// gcc gives the pattern.
///
/// With [`Roots::Checked`] the block is cut after the call. The instruction runs first and the
/// block then branches on whether the operand is below zero. That edge goes to a block with the
/// call in it, which sets `errno` and gives the same NaN. The other edge carries the answer of
/// the instruction. The two meet in a block that has the rest of the old block and takes the
/// answer as its parameter.
///
/// A call stays a call when it does not have the shape of the routine, and when it has an unwind
/// edge, for the reason `fuse` gives.
fn roots(func: &mut Func, names: &mut Interner, roots: Roots) {
    let found: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in found {
        let data = &func[inst];
        let Extra::Call(info) = data.extra else { continue };
        if data.opcode != Opcode::Call || !data.flags.contains(Flags::LIBRARY) {
            continue;
        }
        let Some(callee) = func[info].callee else { continue };
        let (ty, template) = match names.resolve(callee) {
            "sqrt" => (Type::float(Float::F64), "%vsqrtsd\t{%1, %d0|%d0, %1}"),
            "sqrtf" => (Type::float(Float::F32), "%vsqrtss\t{%1, %d0|%d0, %1}"),
            _ => continue,
        };
        let [operand] = func[data.args] else { continue };
        let shaped = func[operand].ty == ty && data.results().map(|value| func[value].ty).eq([ty]);
        if !shaped || func.unwinds_to_pad(inst) {
            continue;
        }
        let old = func[inst].first_result.expect("the call answers");
        let info = AsmInfo {
            template: names.intern(template),
            constraints: names.intern("=x,x"),
            clobbers: names.intern(""),
            targets: BlockCallList::EMPTY,
        };
        let asm = func.add_asm(info);
        let args = func.push_values(&[operand]);
        let data = InstData { args, extra: Extra::Asm(asm), ..InstData::new(Opcode::InlineAsm) };
        let span = func.span(inst);
        let made = func.create_inst(data, &[ty], span);
        func.insert_before(made, inst);
        let root = func[made].first_result.expect("one result was asked for");
        if roots == Roots::Bare {
            func.remove_inst(inst);
            replace(func, old, root);
            continue;
        }
        checked(func, inst, (operand, root, old), ty);
    }
}

/// Cuts the block of the call `inst` after it, so that the call runs only for an operand below
/// zero and `root` is the answer for every other one. `old` is the answer of the call.
fn checked(func: &mut Func, inst: Inst, (operand, root, old): (Value, Value, Value), ty: Type) {
    let head = func.block_of(inst).expect("the call is in a block");
    let span = func.span(inst);
    // Everything after the call, which moves into the block where the two paths meet.
    let tail: Vec<Inst> = func.insts(head).skip_while(|&at| at != inst).skip(1).collect();
    let slow = func.create_block();
    let done = func.create_block();
    let answer = func.append_param(done, ty);
    for at in tail {
        func.remove_inst(at);
        func.append_inst(done, at);
    }
    // Before the call moves, so that only the readers of its answer in the tail are rewritten and
    // the jump below still carries the call's own answer.
    replace(func, old, answer);
    func.remove_inst(inst);
    func.append_inst(slow, inst);
    Builder::new(func, slow).at(span).jump(done, &[old]);
    let mut build = Builder::new(func, head).at(span);
    let zero = build.fconst(ty, 0);
    let below = build.fcmp(FloatPred::Olt, operand, zero, Flags::NONE);
    build.br_if(below, slow, &[], done, &[root]);
}

/// Whether this is a scalar `float` or `double`, the two types the fused rules are written for.
fn is_single_or_double(ty: Type) -> bool {
    ty.is_scalar() && matches!(ty.format(), Some(Float::F32 | Float::F64))
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

    use super::{Float, Func, Opcode, Roots, Type, calls, routine};

    /// A function whose one instruction is a call to `name` with its three parameters, marked as
    /// the library's when `library` is true, run through the step with `fused` and checked by the
    /// verifier.
    fn called(name: &str, format: Float, library: bool, fused: bool) -> String {
        let ty = Type::float(format);
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[ty, ty, ty], &[ty]);
        let callee = names.intern(name);
        let sig =
            func.add_signature(Signature::new().with_params(&[ty, ty, ty]).with_returns(&[ty]));
        let mut build = Builder::new(&mut func, entry);
        let call = build.call(callee, sig, &params);
        let answer = build.func()[call].first_result.expect("the call answers");
        build.ret(&[answer]);
        if library {
            func[call].flags |= Flags::LIBRARY;
        }
        calls(&mut func, &mut names, x86_64::SYSV.abi, fused, Roots::Calls);
        let module = Module::new(names.intern("m.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the function is valid IR");
        printed(&func, &mut names)
    }

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
        calls(&mut func, &mut names, x86_64::SYSV.abi, false, Roots::Calls);
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
        calls(&mut func, &mut names, x86_64::SYSV.abi, false, Roots::Calls);
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

    /// A call to the library's `fma` or `fmaf` in a function the selector has the instruction for
    /// is the instruction, and stays one.
    #[test]
    fn a_library_call_to_fma_is_the_instruction_where_the_selector_has_it() {
        for (name, format) in [("fma", Float::F64), ("fmaf", Float::F32)] {
            let text = called(name, format, true, true);
            assert!(words(&text).contains(&"fma"), "the instruction is there: {text}");
            assert!(!text.contains("call"), "there is no call: {text}");
        }
    }

    /// The same call stays a call where the selector has no instruction for it, and where the
    /// front end did not mark it as the library's, which is a function of the program's own.
    #[test]
    fn a_call_to_fma_stays_a_call_without_the_instruction_or_the_mark() {
        for (library, fused) in [(true, false), (false, true)] {
            let text = called("fma", Float::F64, library, fused);
            assert!(words(&text).contains(&"@fma"), "{library} {fused}: {text}");
            assert!(!words(&text).contains(&"fma"), "{library} {fused}: {text}");
        }
    }

    /// A call to another routine of the same shape is not touched, and neither is the fused
    /// multiply add at `_Float128`, which has no instruction and is still the call to `fmaf128`.
    #[test]
    fn only_fma_and_fmaf_are_the_instruction() {
        let text = called("fmal", Float::F64, true, true);
        assert!(words(&text).contains(&"@fmal"), "{text}");
        let ty = Type::float(Float::F128);
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[ty, ty, ty], &[ty]);
        let mut build = Builder::new(&mut func, entry);
        let args = build.func().push_values(&params);
        let answer = build.value(InstData { args, ..InstData::new(Opcode::Fma) }, ty);
        build.ret(&[answer]);
        calls(&mut func, &mut names, x86_64::SYSV.abi, true, Roots::Calls);
        let text = printed(&func, &mut names);
        assert!(words(&text).contains(&"@fmaf128"), "{text}");
    }

    /// The formats with no routine here keep the instruction, for the selector to refuse by name.
    #[test]
    fn the_half_the_eighty_bit_and_the_decimal_formats_have_no_routine() {
        for format in [Float::F16, Float::F80, Float::D32, Float::D64, Float::D128] {
            assert_eq!(routine(Opcode::FRem, Type::float(format)), None, "{format:?}");
            assert_eq!(routine(Opcode::Fma, Type::float(format)), None, "{format:?}");
        }
    }

    /// A function that answers `name` of its one parameter, the call marked as the library's when
    /// `library` is true, run through the step with `roots` and checked by the verifier.
    fn rooted(name: &str, format: Float, library: bool, roots: Roots) -> String {
        let ty = Type::float(format);
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[ty], &[ty]);
        let callee = names.intern(name);
        let sig = func.add_signature(Signature::new().with_params(&[ty]).with_returns(&[ty]));
        let mut build = Builder::new(&mut func, entry);
        let call = build.call(callee, sig, &params);
        let answer = build.func()[call].first_result.expect("the call answers");
        let twice = build.binary(Opcode::FAdd, answer, answer, Flags::NONE);
        build.ret(&[twice]);
        if library {
            func[call].flags |= Flags::LIBRARY;
        }
        calls(&mut func, &mut names, x86_64::SYSV.abi, false, roots);
        let module = Module::new(names.intern("m.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the function is valid IR");
        printed(&func, &mut names)
    }

    /// Under `-fno-math-errno` the library's `sqrt` and `sqrtf` are the instruction alone.
    #[test]
    fn a_library_square_root_is_the_instruction_without_errno() {
        for (name, format, mnemonic) in
            [("sqrt", Float::F64, "vsqrtsd"), ("sqrtf", Float::F32, "vsqrtss")]
        {
            let text = rooted(name, format, true, Roots::Bare);
            assert!(text.contains(mnemonic), "{name}: {text}");
            assert!(!text.contains("call"), "{name}: {text}");
            assert!(!words(&text).contains(&"br_if"), "{name}: {text}");
        }
    }

    /// Under `-fmath-errno` the instruction answers, and the call is still there for an operand
    /// below zero, on a branch of its own. The rest of the function reads the answer through the
    /// block where the two paths meet.
    #[test]
    fn a_library_square_root_keeps_the_call_below_zero_with_errno() {
        let text = rooted("sqrt", Float::F64, true, Roots::Checked);
        assert!(text.contains("vsqrtsd"), "{text}");
        assert!(words(&text).contains(&"@sqrt"), "{text}");
        assert!(words(&text).contains(&"br_if"), "{text}");
        assert!(words(&text).contains(&"olt"), "{text}");
        let fadd = text.lines().find(|line| line.contains("fadd")).expect("the addition is kept");
        let block = text.lines().filter(|line| line.starts_with("block")).nth(2);
        let block = block.expect("a block where the two paths meet");
        let param = block.split(['(', ':']).nth(1).expect("the block has a parameter").trim();
        assert!(fadd.contains(param), "the addition reads {param}: {text}");
    }

    /// A call the front end did not mark is the program's own `sqrt`, and stays a call. So does
    /// the library's at `-O0` and on a machine without the instruction.
    #[test]
    fn a_square_root_stays_a_call_without_the_mark_or_the_instruction() {
        for (library, roots) in
            [(false, Roots::Bare), (false, Roots::Checked), (true, Roots::Calls)]
        {
            let text = rooted("sqrt", Float::F64, library, roots);
            assert!(!text.contains("sqrtsd"), "{library} {roots:?}: {text}");
            assert!(words(&text).contains(&"@sqrt"), "{library} {roots:?}: {text}");
        }
    }

    /// Nothing else is touched, so an addition at the same format is still an addition.
    #[test]
    fn an_operation_with_an_instruction_is_not_a_call() {
        assert_eq!(routine(Opcode::FAdd, Type::float(Float::F64)), None);
        assert_eq!(routine(Opcode::FMul, Type::float(Float::F32)), None);
    }
}
