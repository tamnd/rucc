//! Judging what a call to the printf family reads and writes, at the call.
//!
//! Design: `spec/safe-memory/10-boundaries.md` section 10.3.
//!
//! `crate::wrap` models a boundary by pointing the call at a wrapper that judges and then does the
//! work. That is not open to `printf` and its relatives, because a wrapper for a variadic C
//! function has to be a variadic Rust function or go through `vsnprintf` and a `va_list`, and both
//! are unstable. What is open is the call site. The format is a literal in nearly every call
//! anybody writes, so which arguments a `%s` reads is known where the call is, and so is the
//! destination of a `sprintf` and the size it was given. This pass reads the format and puts the
//! judgements in front of the call, as calls to the three rows of `rucc-safe-rt`'s `format` group.
//!
//! # What is judged
//!
//! Every `%s` argument is read to its terminator, or to the precision when the format writes one,
//! which is what the C library is about to do with it. A string that was freed, or copied without
//! its terminator, is refused before anything prints it.
//!
//! The output of `sprintf` and `snprintf` is judged over the bytes the call is about to write, and
//! not over the size the program passed. A size is a promise about the destination that a correct
//! program is allowed to make loosely, and `snprintf(buf, sizeof big, "%d", n)` into a buffer with
//! room for the number does nothing wrong. So the pass asks the C library how long the output is,
//! with a `snprintf` that has no destination and the same format and arguments, and judges the
//! smaller of that plus the terminator and the size. The output is formatted twice, which is what a
//! safety build pays for knowing where it ends before any of it is written.
//!
//! # What is left alone
//!
//! A format that is not a literal this module holds, since there is nothing to read. A format with
//! a conversion this does not know, or with positional arguments like `%1$s`, which name their
//! arguments out of order and are rare enough not to be worth a second parser. A `%s` whose
//! precision is `*`, which is an argument rather than something written down. A `%ls`, which reads
//! a wide string and is tamnd/rucc#3268. The `v` forms, whose arguments are a `va_list` and not in
//! hand. And a name the module defines itself, for the reason `crate::wrap` leaves one alone.
//!
//! # Why before the optimizer
//!
//! For `crate::wrap`'s reason. `printf("%s\n", p)` becomes `puts(p)` in the optimizer, and a
//! `sprintf` with a format of one `%s` may become a `strcpy`, and either would leave this nothing
//! to read. The judgements put in here are calls to names the optimizer has no opinion about, so
//! they stay where they are whatever happens to the call behind them.

use rucc_base::{Interner, Symbol};
use rucc_ir::{
    CallInfo, Datum, Def, Extra, Flags, Func, FuncId, Imm, Inst, InstData, IntPred, Linkage,
    Module, Opcode, Signature, SymbolRef, Type, Value,
};

/// The rows of `rucc-safe-rt`'s `format` group, in the order that file writes them.
///
/// Called as `crate::wrap::PREFIX` and the name, the way every wrapper is. None of them is in
/// [`crate::wrap::INTERPOSED`], because nothing a program writes is redirected to them, and `cargo
/// xtask interpose` holds this list to that file instead.
pub const JUDGES: &[&str] = &["printf_output", "printf_string", "printf_string_within"];

/// Where a member of the family keeps its format, and what it writes.
struct Member {
    /// The name a program calls it by.
    name: &'static str,
    /// Which argument is the format. The arguments it converts are the ones after it.
    format: usize,
    /// What the call writes besides what it prints.
    output: Output,
}

/// Where the output of a member goes, when it goes into memory.
#[derive(Clone, Copy)]
enum Output {
    /// To a stream or a descriptor, which is nothing this judges.
    Elsewhere,
    /// To the argument at this position, with no size to stop at.
    Unbounded(usize),
    /// To the argument at the first position, stopping at the size the one at the second gives.
    Bounded(usize, usize),
}

/// The members this reads, the fortified forms included, since a build with `_FORTIFY_SOURCE`
/// calls those instead.
const FAMILY: &[Member] = &[
    Member { name: "printf", format: 0, output: Output::Elsewhere },
    Member { name: "fprintf", format: 1, output: Output::Elsewhere },
    Member { name: "dprintf", format: 1, output: Output::Elsewhere },
    Member { name: "sprintf", format: 1, output: Output::Unbounded(0) },
    Member { name: "snprintf", format: 2, output: Output::Bounded(0, 1) },
    Member { name: "__printf_chk", format: 1, output: Output::Elsewhere },
    Member { name: "__fprintf_chk", format: 2, output: Output::Elsewhere },
    Member { name: "__dprintf_chk", format: 2, output: Output::Elsewhere },
    Member { name: "__sprintf_chk", format: 3, output: Output::Unbounded(0) },
    Member { name: "__snprintf_chk", format: 4, output: Output::Bounded(0, 1) },
];

/// What one conversion does with the argument it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arg {
    /// Something this does not judge: a number, a character, a pointer printed as an address, a
    /// wide string, or a `*` width or precision.
    Other,
    /// A `%s`, read to its terminator.
    String,
    /// A `%s` with a precision, read to its terminator or that many bytes.
    StringWithin(u64),
}

/// Puts the judgements in front of every call to the printf family whose format can be read, and
/// says how many it put in.
///
/// The count is added to what `crate::wrap::redirect` moved, since both are calls the monitor now
/// models.
pub fn judge(module: &mut Module, names: &mut Interner) -> usize {
    // Found rather than interned, since a member this unit never names is one no call goes to.
    let family: Vec<(Symbol, &Member)> =
        FAMILY.iter().filter_map(|member| Some((names.find(member.name)?, member))).collect();
    if family.is_empty() {
        return 0;
    }
    let ids: Vec<FuncId> = module.funcs().collect();
    let defined: Vec<Symbol> =
        ids.iter().filter(|&&id| !module[id].is_declaration()).map(|&id| module[id].name).collect();
    let family: Vec<(Symbol, &Member)> =
        family.into_iter().filter(|(name, _)| !defined.contains(name)).collect();
    let rows = Rows {
        output: names.intern(&[crate::wrap::PREFIX, JUDGES[0]].concat()),
        string: names.intern(&[crate::wrap::PREFIX, JUDGES[1]].concat()),
        within: names.intern(&[crate::wrap::PREFIX, JUDGES[2]].concat()),
        measure: names.intern("snprintf"),
        size: Type::int(module.datalayout.pointer_bits),
    };

    let mut judged = 0;
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        // Read while the module can still be read, since the format is in one of its globals.
        let found: Vec<(Inst, &Member, Vec<Arg>)> = {
            let func = &module[id];
            let insts = func.blocks().flat_map(|block| func.insts(block));
            insts
                .filter_map(|inst| {
                    let member = called(func, inst, &family)?;
                    let format = *func[func[inst].args].get(member.format)?;
                    let args = conversions(&literal(module, func, format)?)?;
                    Some((inst, member, args))
                })
                .collect()
        };
        let func = &mut module[id];
        for (inst, member, args) in found {
            judged += site(func, inst, member, &args, &rows);
        }
    }
    judged
}

/// The member of the family `inst` calls, when it calls one by name and its declaration names
/// every argument up to the format and leaves the rest to the `...`.
///
/// A declaration written some other way, `int printf();` with no prototype, passes the arguments
/// some other way too, and the call this pass makes to measure the output could not pass them the
/// same.
fn called<'a>(func: &Func, inst: Inst, family: &[(Symbol, &'a Member)]) -> Option<&'a Member> {
    if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
        return None;
    }
    let Extra::Call(at) = func[inst].extra else { return None };
    let info = func[at];
    let callee = info.callee?;
    let &(_, member) = family.iter().find(|&&(name, _)| name == callee)?;
    let signature = &func[info.signature];
    (signature.variadic && signature.params.len() == member.format + 1).then_some(member)
}

/// The bytes of the string `value` points at, up to its terminator, when it is a constant this
/// module defines and nothing at link time can put something else in its place.
fn literal(module: &Module, func: &Func, value: Value) -> Option<Vec<u8>> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let (base, offset) = match func[inst].opcode {
        Opcode::GlobalAddr => (inst, 0),
        Opcode::PtrAdd => {
            let args = &func[func[inst].args];
            let Def::Result { inst: base, .. } = func[*args.first()?].def else { return None };
            let Def::Result { inst: by, .. } = func[*args.get(1)?].def else { return None };
            let Extra::Imm(imm) = func[by].extra else { return None };
            if func[by].opcode != Opcode::IConst || func[base].opcode != Opcode::GlobalAddr {
                return None;
            }
            (base, usize::try_from(func[imm].bits()).ok()?)
        }
        _ => return None,
    };
    let Extra::Symbol(name) = func[base].extra else { return None };
    let Some(SymbolRef::Global(id)) = module.lookup(name) else { return None };
    let global = &module[id];
    if !global.constant || !matches!(global.linkage, Linkage::External | Linkage::Internal) {
        return None;
    }
    let mut bytes = Vec::new();
    for &datum in &module[global.init?] {
        match datum {
            Datum::Bytes(range) => bytes.extend_from_slice(&module[range]),
            Datum::Zero(count) => {
                bytes.resize(bytes.len().checked_add(usize::try_from(count).ok()?)?, 0);
            }
            // Anything else is not a byte this can read, and what follows it is at an offset that
            // is right only if its width is, so the reading stops.
            _ => return None,
        }
    }
    let bytes = bytes.get(offset..)?;
    let end = bytes.iter().position(|&byte| byte == 0)?;
    Some(bytes[..end].to_vec())
}

/// What each argument after the format is to the conversion that takes it, in order, or `None`
/// for a format this does not read.
///
/// The grammar is C's with glibc's additions: flags, a width that is digits or `*`, a precision
/// that is a dot and digits or `*`, a length, and the conversion. `%m` takes no argument and `%%`
/// is a percent sign.
fn conversions(format: &[u8]) -> Option<Vec<Arg>> {
    let mut args = Vec::new();
    let mut at = 0;
    while at < format.len() {
        if format[at] != b'%' {
            at += 1;
            continue;
        }
        at += 1;
        if format.get(at) == Some(&b'%') {
            at += 1;
            continue;
        }
        while matches!(format.get(at), Some(b'-' | b'+' | b' ' | b'#' | b'0' | b'\'' | b'I')) {
            at += 1;
        }
        if format.get(at) == Some(&b'*') {
            args.push(Arg::Other);
            at += 1;
        }
        // Digits here are a width, or the number of a positional argument when a `$` follows.
        while format.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if format.get(at) == Some(&b'$') {
            return None;
        }
        // Nothing when there is no precision, and nothing inside when it is a `*`, which is an
        // argument and not a bound written down.
        let mut precision = None;
        if format.get(at) == Some(&b'.') {
            at += 1;
            if format.get(at) == Some(&b'*') {
                args.push(Arg::Other);
                precision = Some(None);
                at += 1;
            } else {
                let mut digits: u64 = 0;
                while let Some(&digit) = format.get(at).filter(|byte| byte.is_ascii_digit()) {
                    digits = digits.saturating_mul(10).saturating_add(u64::from(digit - b'0'));
                    at += 1;
                }
                precision = Some(Some(digits));
            }
            if format.get(at) == Some(&b'$') {
                return None;
            }
        }
        let mut wide = false;
        while let Some(&length) = format.get(at) {
            match length {
                b'l' => wide = true,
                b'h' | b'L' | b'q' | b'j' | b'z' | b'Z' | b't' => {}
                _ => break,
            }
            at += 1;
        }
        let conversion = *format.get(at)?;
        at += 1;
        match conversion {
            b's' if !wide => args.push(match precision {
                None => Arg::String,
                Some(Some(limit)) => Arg::StringWithin(limit),
                Some(None) => Arg::Other,
            }),
            b'd' | b'i' | b'o' | b'u' | b'x' | b'X' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G'
            | b'a' | b'A' | b'c' | b'p' | b'n' | b'C' | b'S' | b's' => args.push(Arg::Other),
            b'm' => {}
            _ => return None,
        }
    }
    Some(args)
}

/// The names the judgements call, and the type a size is.
struct Rows {
    /// `printf_output`, for what `sprintf` writes.
    output: Symbol,
    /// `printf_string`, for a `%s`.
    string: Symbol,
    /// `printf_string_within`, for a `%s` with a precision.
    within: Symbol,
    /// `snprintf`, which measures the output.
    measure: Symbol,
    /// `size_t` on this target.
    size: Type,
}

/// Puts the judgements for one call in front of it, and says how many.
fn site(func: &mut Func, inst: Inst, member: &Member, args: &[Arg], rows: &Rows) -> usize {
    let passed: Vec<Value> = func[func[inst].args].to_vec();
    let mut judged = 0;
    for (&arg, &value) in args.iter().zip(&passed[member.format + 1..]) {
        if !func[value].ty.is_ptr() {
            continue;
        }
        match arg {
            Arg::Other => continue,
            Arg::String => {
                call(func, inst, rows.string, &[value], &[Type::PTR], None);
            }
            Arg::StringWithin(limit) => {
                let limit = constant(func, inst, rows.size, i128::from(limit));
                call(func, inst, rows.within, &[value, limit], &[Type::PTR, rows.size], None);
            }
        }
        judged += 1;
    }

    let (dst, size) = match member.output {
        Output::Elsewhere => return judged,
        Output::Unbounded(dst) => (passed[dst], None),
        Output::Bounded(dst, size) => (passed[dst], Some(passed[size])),
    };
    if !func[dst].ty.is_ptr() || size.is_some_and(|size| func[size].ty != rows.size) {
        return judged;
    }
    // `snprintf(NULL, 0, format, ...)`, which writes nothing and answers how long the output is.
    let Extra::Call(at) = func[inst].extra else { return judged };
    let varargs = func[at].varargs;
    let zero = constant(func, inst, rows.size, 0);
    let null = unary(func, inst, Opcode::IntToPtr, zero, Type::PTR);
    let mut measured = vec![null, zero];
    measured.extend_from_slice(&passed[member.format..]);
    let int = Type::int(32);
    let Some(length) = call(
        func,
        inst,
        rows.measure,
        &measured,
        &[Type::PTR, rows.size, Type::PTR],
        Some((int, varargs)),
    ) else {
        return judged;
    };
    // What the call will write is the output and its terminator, nothing when the output cannot
    // be formatted at all, and no more than the size when there is one.
    let wide =
        if rows.size == int { length } else { unary(func, inst, Opcode::SExt, length, rows.size) };
    let one = constant(func, inst, rows.size, 1);
    let mut wrote = binary(func, inst, Opcode::Add, wide, one);
    if let Some(size) = size {
        let less = icmp(func, inst, IntPred::Ult, wrote, size);
        wrote = select(func, inst, less, wrote, size);
    }
    let none = constant(func, inst, int, 0);
    let failed = icmp(func, inst, IntPred::Slt, length, none);
    let wrote = select(func, inst, failed, zero, wrote);
    call(func, inst, rows.output, &[dst, wrote], &[Type::PTR, rows.size], None);
    judged + 1
}

/// Puts a new instruction in front of `at`, with its span.
fn before(func: &mut Func, at: Inst, data: InstData, results: &[Type]) -> Inst {
    let span = func.span(at);
    let made = func.create_inst(data, results, span);
    func.insert_before(made, at);
    made
}

/// The one value a new instruction in front of `at` produces.
fn value(func: &mut Func, at: Inst, data: InstData, ty: Type) -> Value {
    let made = before(func, at, data, &[ty]);
    func[made].first_result.expect("one result was asked for")
}

/// An integer constant in front of `at`.
fn constant(func: &mut Func, at: Inst, ty: Type, number: i128) -> Value {
    let extra = Extra::Imm(func.add_imm(Imm::int(number, ty)));
    value(func, at, InstData { extra, ..InstData::new(Opcode::IConst) }, ty)
}

/// A one operand instruction in front of `at`.
fn unary(func: &mut Func, at: Inst, opcode: Opcode, arg: Value, ty: Type) -> Value {
    let args = func.push_values(&[arg]);
    value(func, at, InstData { args, ..InstData::new(opcode) }, ty)
}

/// A two operand instruction in front of `at`, whose result has the type of its operands.
fn binary(func: &mut Func, at: Inst, opcode: Opcode, lhs: Value, rhs: Value) -> Value {
    let ty = func[lhs].ty;
    let args = func.push_values(&[lhs, rhs]);
    value(func, at, InstData { args, flags: Flags::NONE, ..InstData::new(opcode) }, ty)
}

/// An integer comparison in front of `at`.
fn icmp(func: &mut Func, at: Inst, pred: IntPred, lhs: Value, rhs: Value) -> Value {
    let args = func.push_values(&[lhs, rhs]);
    let data = InstData { args, extra: Extra::IntPred(pred), ..InstData::new(Opcode::ICmp) };
    value(func, at, data, Type::I1)
}

/// One of two values in front of `at`.
fn select(func: &mut Func, at: Inst, cond: Value, then: Value, other: Value) -> Value {
    let ty = func[then].ty;
    let args = func.push_values(&[cond, then, other]);
    value(func, at, InstData { args, ..InstData::new(Opcode::Select) }, ty)
}

/// A call in front of `at`, and what it returns when `returns` says it returns something.
///
/// `returns` also carries how the arguments past `params` travel, which for the measuring call is
/// the way the call it measures passes the same ones.
fn call(
    func: &mut Func,
    at: Inst,
    callee: Symbol,
    args: &[Value],
    params: &[Type],
    returns: Option<(Type, rucc_ir::AbiList)>,
) -> Option<Value> {
    let mut signature = Signature::new().with_params(params);
    let varargs = match returns {
        Some((ty, varargs)) => {
            signature = signature.with_returns(&[ty]).variadic();
            varargs
        }
        None => func.push_abis(&[]),
    };
    let signature = func.add_signature(signature);
    let info = func.add_call(CallInfo { callee: Some(callee), signature, varargs });
    let args = func.push_values(args);
    let results: Vec<Type> = returns.map(|(ty, _)| ty).into_iter().collect();
    let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
    let made = before(func, at, data, &results);
    func[made].results().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_format_says_which_arguments_are_strings() {
        assert_eq!(
            conversions(b"%s and %d %.3s"),
            Some(vec![Arg::String, Arg::Other, Arg::StringWithin(3)])
        );
        assert_eq!(conversions(b"%5.2f %*d %%"), Some(vec![Arg::Other, Arg::Other, Arg::Other]));
        assert_eq!(conversions(b"%-10s|%ls|%m"), Some(vec![Arg::String, Arg::Other]));
        assert_eq!(conversions(b"%lld %zu %hhx"), Some(vec![Arg::Other, Arg::Other, Arg::Other]));
    }

    #[test]
    fn a_precision_that_is_an_argument_is_not_a_bound_this_can_read() {
        assert_eq!(conversions(b"%.*s"), Some(vec![Arg::Other, Arg::Other]));
    }

    #[test]
    fn a_format_this_cannot_read_is_left_alone() {
        assert_eq!(conversions(b"%1$s"), None);
        assert_eq!(conversions(b"%.*1$s"), None);
        assert_eq!(conversions(b"%k"), None);
        assert_eq!(conversions(b"trailing %"), None);
    }
}
