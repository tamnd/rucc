//! A copy or fill of a known size too large to be a run of moves, as one `rep movsq` or
//! `rep stosq`, in a function that may not use the vector registers.
//!
//! # Why
//!
//! [`crate::expand::bulk`] writes a copy or fill of up to [`expand::copies`] or [`expand::fills`]
//! moves inline and calls `memcpy` or `memset` for a larger one. Under `-mno-sse` gcc on x86-64
//! calls the library only above 8192 bytes, and below that writes the string instruction: a zero
//! fill of more than five words is `rep stosq`, and a copy of more than sixteen is `rep movsq`.
//! Here a copy is the string from where `bulk` stops at twelve.
//! `-mno-sse` or `-mgeneral-regs-only` is on every kernel file, so this is the whole kernel. A
//! copy of `struct pt_regs` (168 bytes) on every page fault and the zeroing of a `struct kstat`
//! (192 bytes) on every `stat` were calls to `__memcpy` and `__memset`, and then a loop of four
//! words that came to about fifty bytes of code each time, where gcc's is about fifteen. The bench
//! objects had 83 of those loops, and gcc 13 writes 353 `rep stosq` against rucc's 57.
//!
//! # What it writes
//!
//! For `memcpy to, from` of `size` bytes, with `whole` for `size` rounded down to a multiple of 8:
//!
//! ```text
//!   to', from' = asm "rep movsq" to, from, whole / 8   ; in rdi, rsi and rcx
//!   memcpy to', from'                                  ; size - whole bytes, as moves
//! ```
//!
//! A fill is `rep stosq` with the byte spread across `rax`. The instruction leaves `rdi` and `rsi`
//! just past what it wrote, so the tail is written from there, which `bulk` takes apart into
//! moves. A call to `memcpy` or `memset` of a constant size is rewritten the same way, and its
//! answer is its first argument.
//!
//! A size the program works out is written the same way when it is a whole number of words no
//! larger than 8192, which gcc reads off the value's range. The maple tree copies and clears parts
//! of a node by `size * sizeof(unsigned long)` with `size` an `unsigned char`, and those were 17
//! calls to `memcpy` and 7 to `memset` for every grow and shrink of `brk`, where gcc has one:
//!
//! ```text
//!   to', from' = asm "rep movsq" to, from, size >> 3
//! ```
//!
//! The string instruction is an `asm` statement rather than an instruction of its own, because a
//! template already says everything about it the allocator has to know: which three registers it
//! reads, which two it leaves changed, and that it writes memory.
//!
//! # What it leaves alone
//!
//! A fill whose byte the program works out, a call that unwinds to a landing pad, a call in a
//! function that said the names are not the library's, a size the target writes no string for,
//! and every function built for size or allowed the vector registers. Those last ones are left to
//! the threshold `cargo xtask stringop` measured against the C library (tamnd/rucc#2971).
//!
//! # Where it runs
//!
//! Beside `fills::loops`, at the front, so that `bulk` sees the tail it leaves.

use rucc_base::Interner;
use rucc_cost::Goal;
use rucc_ir::{
    AsmInfo, AttrSet, BlockCallList, Def, Extra, Flags, Func, Imm, Inst, InstData, MemInfo,
    MemOrder, Opcode, Restrict, Type, Value,
};

use crate::{expand, retry};

/// How many bytes the string instruction moves at a time: one word of eight.
const WORD: u64 = 8;

/// The most words a fill of zero is written as before it is `rep stosq`, which is gcc's
/// `clear_ratio` for `-mtune=generic` less one.
const CLEARS: usize = 5;

/// One copy or fill, as the rewrite needs it.
struct Roll {
    /// `Memcpy` or `Memset`.
    opcode: Opcode,
    /// Where it writes.
    to: Value,
    /// Where a copy reads from, or the byte of a fill at whatever width it came in.
    with: Value,
    /// The byte of a fill.
    byte: u8,
    /// What the program said about the memory, with the size.
    info: MemInfo,
    /// The flags a load or store may keep.
    flags: Flags,
    /// The value a call answers, which is `to`.
    answer: Option<Value>,
    /// How many bytes, where the program works the count out, which is then a whole number of
    /// words.
    length: Option<Value>,
}

/// Rewrites every copy and fill of a known size that [`expand::bulk`] would make a call of, and
/// that is no more than `most` bytes, into the string instruction the module describes.
///
/// `word` is how many bytes a register holds, `unaligned` is whether a word may be moved at any
/// address, `most` is [`rucc_target::CallRegs::rolled`], which the pipeline makes zero for a
/// function with the vector registers, and `goal` is what the threshold of [`expand::bulk`] is
/// read for.
pub fn loops(
    func: &mut Func,
    names: &mut Interner,
    (word, unaligned, most): (u32, bool, u32),
    goal: Goal,
) {
    if most == 0 || goal != Goal::Speed || !unaligned || word != 8 {
        return;
    }
    let moves = (expand::copies(goal), expand::fills(goal));
    let found: Vec<(Inst, Roll)> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter_map(|inst| wanted(func, names, inst, moves, most).map(|roll| (inst, roll)))
        .collect();
    for (inst, roll) in found {
        rewrite(func, names, inst, &roll);
    }
}

/// The copy or fill an instruction is, where it is one this pass writes as a string instruction.
fn wanted(
    func: &Func,
    names: &Interner,
    inst: Inst,
    moves: (usize, usize),
    most: u32,
) -> Option<Roll> {
    let data = &func[inst];
    let (opcode, to, with, info, flags, answer) = match data.opcode {
        Opcode::Memcpy | Opcode::Memset => {
            let bulk = func.bulk(inst)?;
            let Extra::Mem(mem) = data.extra else { return None };
            if func.carries_mem(inst) {
                return None;
            }
            if let Some(length) = bulk.length {
                return worked_out(func, data.opcode, (bulk.to, bulk.with, length), most)
                    .map(|roll| Roll { info: func[mem], flags: data.flags, ..roll });
            }
            (data.opcode, bulk.to, bulk.with, func[mem], data.flags, None)
        }
        Opcode::Call
            if !func.attrs.set.contains(AttrSet::NO_BUILTIN) && !func.unwinds_to_pad(inst) =>
        {
            let Extra::Call(call) = data.extra else { return None };
            let opcode = match names.resolve(func[call].callee?) {
                "memcpy" => Opcode::Memcpy,
                "memset" => Opcode::Memset,
                _ => return None,
            };
            let &[to, with, length] = &func[data.args] else { return None };
            let Some(size) = expand::number(func, length) else {
                return worked_out(func, opcode, (to, with, length), most)
                    .map(|roll| Roll { answer: data.first_result, ..roll });
            };
            let size = u64::try_from(size).ok()?;
            let info = MemInfo {
                size,
                align: 1,
                order: MemOrder::NotAtomic,
                tbaa: None,
                owns: 0,
                restrict: Restrict::NONE,
            };
            (opcode, to, with, info, Flags::NONE, data.first_result)
        }
        _ => return None,
    };
    let byte = match opcode {
        Opcode::Memset => expand::literal(func, with)?,
        _ => 0,
    };
    let moves = match opcode {
        Opcode::Memset if byte == 0 => CLEARS.min(moves.1),
        Opcode::Memset => moves.1,
        _ => moves.0,
    };
    // What `bulk` would call for, and a zero fill from where gcc stops writing moves. A word may
    // be moved at any address here, so that is a size whose plan at the widest word is more moves
    // than the threshold.
    if info.size < WORD
        || info.size > u64::from(most)
        || info.order != MemOrder::NotAtomic
        || expand::plan(info.size, 8, 8, moves).is_some()
    {
        return None;
    }
    let flags = flags.intersection(Flags::legal_on(Opcode::Store));
    Some(Roll { opcode, to, with, byte, info, flags, answer, length: None })
}

/// A copy or fill whose size the program works out, where that is a whole number of words and no
/// more than `most` bytes.
fn worked_out(
    func: &Func,
    opcode: Opcode,
    (to, with, length): (Value, Value, Value),
    most: u32,
) -> Option<Roll> {
    let (largest, zeros) = bounded(func, length, DEPTH)?;
    let words = largest <= u128::from(most) && zeros >= WORD.trailing_zeros();
    if !words || func[length].ty.bits() > 64 {
        return None;
    }
    let byte = match opcode {
        Opcode::Memset => expand::literal(func, with)?,
        _ => 0,
    };
    let info = MemInfo {
        size: 0,
        align: 1,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let length = Some(length);
    Some(Roll { opcode, to, with, byte, info, flags: Flags::NONE, answer: None, length })
}

/// How far back through the instructions a size is followed.
const DEPTH: u32 = 6;

/// The largest a value can be and how many of its low bits are known to be zero, for a value
/// built from a narrower number by widening, shifting it up, multiplying it by a constant or
/// masking it.
fn bounded(func: &Func, value: Value, depth: u32) -> Option<(u128, u32)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let bits = func[value].ty.bits();
    if depth == 0 || bits >= 128 {
        return None;
    }
    let fits = |n: u128| (n >> bits == 0).then_some(n);
    let args = &func[func[inst].args];
    match (func[inst].opcode, args) {
        (Opcode::IConst, _) => {
            let n = expand::number(func, value)?;
            Some((n, n.trailing_zeros()))
        }
        (Opcode::ZExt, &[narrow]) => {
            let all = (1u128 << func[narrow].ty.bits()) - 1;
            let (largest, zeros) = bounded(func, narrow, depth - 1).unwrap_or((all, 0));
            Some((largest.min(all), zeros))
        }
        // A number that cannot be negative widens the same either way, which is how an `int`
        // worked out from a narrower count reaches a size.
        (Opcode::SExt, &[narrow]) => {
            let (largest, zeros) = bounded(func, narrow, depth - 1)?;
            (largest >> (func[narrow].ty.bits() - 1) == 0).then_some((largest, zeros))
        }
        (Opcode::Shl, &[number, by]) => {
            let by = u32::try_from(expand::number(func, by)?).ok().filter(|&by| by < bits)?;
            let (largest, zeros) = bounded(func, number, depth - 1)?;
            Some((fits(largest.checked_shl(by)?)?, zeros + by))
        }
        (Opcode::Mul, &[a, b]) => {
            let (number, by) = match expand::number(func, b) {
                Some(by) => (a, by),
                None => (b, expand::number(func, a)?),
            };
            let (largest, zeros) = bounded(func, number, depth - 1)?;
            Some((fits(largest.checked_mul(by)?)?, zeros + by.trailing_zeros()))
        }
        (Opcode::And, &[a, b]) => {
            let (number, mask) = match expand::number(func, b) {
                Some(mask) => (a, mask),
                None => (b, expand::number(func, a)?),
            };
            let (largest, zeros) = bounded(func, number, depth - 1).unwrap_or((mask, 0));
            Some((largest.min(mask), zeros.max(mask.trailing_zeros())))
        }
        _ => None,
    }
}

/// The number of words in a length written as that number times the size of a word. The bound
/// was checked before, so the product did not wrap and the number is the length over 8.
fn words(func: &Func, length: Value) -> Option<Value> {
    let Def::Result { inst, .. } = func[length].def else { return None };
    let word = Some(u128::from(WORD));
    match (func[inst].opcode, &func[func[inst].args]) {
        (Opcode::Shl, &[number, by]) if expand::number(func, by) == Some(3) => Some(number),
        (Opcode::Mul, &[a, b]) if expand::number(func, b) == word => Some(a),
        (Opcode::Mul, &[a, b]) if expand::number(func, a) == word => Some(b),
        _ => None,
    }
}

/// One copy or fill, as the string instruction the module describes.
fn rewrite(func: &mut Func, names: &mut Interner, inst: Inst, roll: &Roll) {
    let span = func.span(inst);
    let index = Type::int(64);
    let whole = roll.info.size / WORD * WORD;
    let rest = roll.info.size - whole;
    let copy = roll.opcode == Opcode::Memcpy;
    let (template, constraints) =
        if copy { ("rep movsq", "=D,=S,=c,0,1,2") } else { ("rep stosq", "=D,=c,0,1,a") };
    let info = AsmInfo {
        template: names.intern(template),
        constraints: names.intern(constraints),
        clobbers: names.intern("memory"),
        targets: BlockCallList::EMPTY,
    };

    let count = match roll.length {
        Some(length) => {
            let (number, apart) = match words(func, length) {
                Some(number) => (number, false),
                None => (length, true),
            };
            let wide = if func[number].ty == index {
                number
            } else {
                expand::ahead(func, inst, Opcode::ZExt, &[number], index)
            };
            if apart {
                let shift = i128::from(WORD.trailing_zeros());
                let by = expand::ahead_const(func, inst, Imm::int(shift, index), index);
                expand::ahead(func, inst, Opcode::LShr, &[wide, by], index)
            } else {
                wide
            }
        }
        None => expand::ahead_const(func, inst, Imm::int(i128::from(whole / WORD), index), index),
    };
    let (args, results) = if copy {
        (vec![roll.to, roll.with, count], vec![Type::PTR, Type::PTR, index])
    } else {
        let spread = i128::from(expand::spread(roll.byte, 8));
        let value = expand::ahead_const(func, inst, Imm::int(spread, index), index);
        (vec![roll.to, count, value], vec![Type::PTR, index])
    };
    let extra = Extra::Asm(func.add_asm(info));
    let args = func.push_values(&args);
    let flags = Flags::VOLATILE;
    let data = InstData { args, extra, flags, ..InstData::new(Opcode::InlineAsm) };
    let string = func.create_inst(data, &results, span);
    func.insert_before(string, inst);
    let past: Vec<Value> = func[string].results().collect();

    if rest > 0 {
        let info = MemInfo { size: rest, align: roll.info.align.clamp(1, 8), ..roll.info };
        let extra = Extra::Mem(func.add_mem(info));
        let with = if copy { past[1] } else { roll.with };
        let args = func.push_values(&[past[0], with]);
        let flags = roll.flags;
        let data = InstData { args, extra, flags, ..InstData::new(roll.opcode) };
        let made = func.create_inst(data, &[], span);
        func.insert_before(made, inst);
    }
    func.remove_inst(inst);
    if let Some(answer) = roll.answer {
        retry::replace(func, answer, roll.to);
    }
}
