//! A fill the library would have been called for, as a loop of stores, under
//! `-finline-stringops=memset`.
//!
//! # Why
//!
//! Linux 7.3 builds its vDSO with `-finline-stringops=memset` when the compiler takes it
//! (`CONFIG_CC_OPT_INLINE_MEMSET`). The vDSO is mapped into every process and is linked against
//! nothing, so a `memset` call in it is an undefined symbol the vDSO checks refuse. gcc 14 and later
//! answer the flag by writing every fill inline, a fill whose length the program works out
//! included, and this is the same promise (tamnd/rucc#3761).
//!
//! # What the loop is
//!
//! For `memset to, byte, length` in a block, with `tail` for whatever followed it:
//!
//! ```text
//! head:                              ; what was in front of the instruction
//!   n = length as a word
//!   v = byte times 0x0101..01
//!   jump words(to, n)
//! words(p, left):
//!   br_if left >= w, wide(p, left), bytes(p, left)
//! wide(p, left):
//!   store v -> p
//!   jump words(p + w, left - w)
//! bytes(p, left):
//!   br_if left != 0, one(p, left), done()
//! one(p, left):
//!   store byte -> p
//!   jump bytes(p + 1, left - 1)
//! done:                              ; `tail`
//! ```
//!
//! `w` is the word, or the alignment the fill is known to have where that is less and the machine
//! may not store a word at any address. A fill that can only be stored a byte at a time has no
//! word loop, and `head` jumps to `bytes` straight away.
//!
//! `rucc-lower` writes each `__builtin_memset` under the flag as the IR's fill, a length that is not
//! a constant as its operand. A plain call to `memset` reaches here as the call, and it is a fill
//! as well where the function may take the name to mean the C library's, as gcc takes it. Its
//! answer is its first argument, so a use of the answer becomes a use of that.
//!
//! # What it leaves alone
//!
//! A fill of a known size that [`crate::expand::bulk`] writes as stores anyway, since its plan is
//! no loop and no branch at all, and a call it takes apart into such a fill. A call that unwinds
//! to a landing pad, since the loop has no edge to one. And every fill in a function that was not
//! built with the flag, which is what [`AttrSet::INLINE_MEMSET`] says.
//!
//! # Where it runs
//!
//! Beside `retry::loops`, at the front, for the reason given there: it makes blocks and nothing in
//! `expand.rs` may.

use rucc_base::Interner;
use rucc_cost::Goal;
use rucc_ir::{
    AttrSet, Block, Builder, Extra, Flags, Func, Inst, IntPred, MemInfo, MemOrder, Opcode,
    Restrict, Type, Value,
};

use crate::{expand, retry};

/// One fill, as the loop needs it.
struct Fill {
    /// Where it writes.
    to: Value,
    /// The byte, at whatever width the instruction or the call has it.
    byte: Value,
    /// The length where the program works it out, and nothing where it is `info.size`.
    length: Option<Value>,
    /// What the program said about the memory.
    info: MemInfo,
    /// The flags a store may keep.
    flags: Flags,
    /// The value a call answers, which is `to`.
    answer: Option<Value>,
}

/// Rewrites every fill in a function built with `-finline-stringops=memset` that would otherwise
/// be a call to `memset` into the loop the module describes.
///
/// `word` is how many bytes a register holds, `unaligned` is whether a word may be stored at any
/// address and `goal` is what the threshold for a fill of known size is read for.
pub fn loops(func: &mut Func, names: &Interner, word: u32, unaligned: bool, goal: Goal) {
    if !func.attrs.set.contains(AttrSet::INLINE_MEMSET) {
        return;
    }
    let most = expand::fills(goal);
    let found: Vec<(Inst, Fill)> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter_map(|inst| {
            wanted(func, names, inst, word, unaligned, most).map(|fill| (inst, fill))
        })
        .collect();
    for (inst, fill) in found {
        rewrite(func, inst, &fill, word, unaligned);
    }
}

/// The fill an instruction is, where [`expand::bulk`] would make a call of it or it is one.
fn wanted(
    func: &Func,
    names: &Interner,
    inst: Inst,
    word: u32,
    unaligned: bool,
    most: usize,
) -> Option<Fill> {
    let data = &func[inst];
    let flags = data.flags.intersection(Flags::legal_on(Opcode::Store));
    if data.opcode == Opcode::Memset {
        let bulk = func.bulk(inst)?;
        let Extra::Mem(mem) = data.extra else { return None };
        let info = func[mem];
        let called = bulk.length.is_some() || expand::chunks(info, word, unaligned, most).is_none();
        let fill =
            Fill { to: bulk.to, byte: bulk.with, length: bulk.length, info, flags, answer: None };
        return called.then_some(fill);
    }
    if data.opcode != Opcode::Call
        || func.attrs.set.contains(AttrSet::NO_BUILTIN)
        || func.unwinds_to_pad(inst)
    {
        return None;
    }
    let Extra::Call(call) = data.extra else { return None };
    if names.resolve(func[call].callee?) != "memset" {
        return None;
    }
    let &[to, byte, length] = &func[data.args] else { return None };
    // What `expand::small` takes apart into stores on a machine that stores a word anywhere.
    let size = expand::number(func, length).and_then(|n| u64::try_from(n).ok());
    if unaligned
        && expand::literal(func, byte).is_some()
        && size.is_some_and(|size| expand::plan(size, word, word, most).is_some())
    {
        return None;
    }
    let info = MemInfo {
        size: 0,
        align: 1,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    Some(Fill {
        to,
        byte,
        length: Some(length),
        info,
        flags: Flags::NONE,
        answer: data.first_result,
    })
}

/// The widest store the word loop may make, or one when there is to be no word loop.
fn width(info: MemInfo, word: u32, unaligned: bool) -> u32 {
    let widest = if unaligned { word } else { word.min(info.align) };
    if widest.is_power_of_two() { widest } else { 1 }
}

/// One fill, as the loop the module describes.
fn rewrite(func: &mut Func, inst: Inst, fill: &Fill, word: u32, unaligned: bool) {
    let head = func.block_of(inst).expect("the instruction is in a block");
    let span = func.span(inst);
    let wide = width(fill.info, word, unaligned);
    let count = Type::int(word * 8);
    let constant = expand::literal(func, fill.byte);

    // Everything after the instruction, collected before anything is taken out of the block, as
    // in `retry::rewrite`.
    let tail: Vec<Inst> = func.insts(head).skip_while(|&at| at != inst).skip(1).collect();
    let mut looped = || {
        let pair = (func.create_block(), func.create_block());
        for block in [pair.0, pair.1] {
            func.append_param(block, Type::PTR);
            func.append_param(block, count);
        }
        pair
    };
    let words = (wide > 1).then(&mut looped);
    let bytes = looped();
    let done = func.create_block();
    func.remove_inst(inst);
    for at in tail {
        func.remove_inst(at);
        func.append_inst(done, at);
    }

    let mut build = Builder::new(func, head).at(span);
    let n = match fill.length {
        Some(length) => {
            let bits = build.func()[length].ty.bits();
            if bits < count.bits() {
                build.unary(Opcode::ZExt, length, count)
            } else if bits > count.bits() {
                build.unary(Opcode::Trunc, length, count)
            } else {
                length
            }
        }
        None => build.iconst(count, i128::from(fill.info.size)),
    };
    let byte = match constant {
        Some(byte) => build.iconst(Type::int(8), i128::from(byte)),
        None if build.func()[fill.byte].ty == Type::int(8) => fill.byte,
        None => build.unary(Opcode::Trunc, fill.byte, Type::int(8)),
    };
    // Worked out here rather than in the loop, so the loop stores one register over and over.
    let ty = Type::int(wide * 8);
    let value = match constant {
        _ if wide == 1 => byte,
        Some(byte) => build.iconst(ty, i128::from(expand::spread(byte, wide))),
        None => {
            let spread = build.unary(Opcode::ZExt, byte, ty);
            let ones = build.iconst(ty, i128::from(expand::spread(1, wide)));
            build.binary(Opcode::Mul, spread, ones, Flags::NONE)
        }
    };
    build.jump(words.unwrap_or(bytes).0, &[fill.to, n]);

    if let Some(words) = words {
        let align = wide.min(fill.info.align.max(1));
        let access = MemInfo { size: u64::from(wide), align, ..fill.info };
        step(func, words, (bytes.0, true), (value, access, fill.flags), span);
    }
    let access = MemInfo { size: 1, align: 1, ..fill.info };
    step(func, bytes, (done, false), (byte, access, fill.flags), span);

    if let Some(answer) = fill.answer {
        retry::replace(func, answer, fill.to);
    }
}

/// One loop, of a test block and a body that stores a value and steps past it.
///
/// Both blocks take the pointer and the count left. The test leaves for `out` when fewer bytes are
/// left than one store covers, with the two of them where `out` carries them on and with nothing
/// where it is the block the fill ends in.
fn step(
    func: &mut Func,
    (test, body): (Block, Block),
    (out, carry): (Block, bool),
    (value, access, flags): (Value, MemInfo, Flags),
    span: rucc_diag::Span,
) {
    let count = func[func[test].params[1]].ty;
    let width = i128::from(access.size);
    let (at, left) = (func[test].params[0], func[test].params[1]);
    let mut build = Builder::new(func, test).at(span);
    let more = if width == 1 {
        let zero = build.iconst(count, 0);
        build.icmp(IntPred::Ne, left, zero)
    } else {
        let one = build.iconst(count, width);
        build.icmp(IntPred::Uge, left, one)
    };
    let carried: &[Value] = if carry { &[at, left] } else { &[] };
    build.br_if(more, body, &[at, left], out, carried);

    let (at, left) = (func[body].params[0], func[body].params[1]);
    let mut build = Builder::new(func, body).at(span);
    build.store(value, at, access, flags);
    let by = build.iconst(Type::int(64), width);
    let next = build.binary(Opcode::PtrAdd, at, by, Flags::NONE);
    let one = build.iconst(count, width);
    let rest = build.binary(Opcode::Sub, left, one, Flags::NONE);
    build.jump(test, &[next, rest]);
}
