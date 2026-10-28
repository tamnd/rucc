//! A copy or fill of a known size too large to be a run of moves, as a loop of four words, in a
//! function that may not use the vector registers.
//!
//! # Why
//!
//! [`crate::expand::bulk`] writes a copy or fill of up to [`expand::copies`] or [`expand::fills`]
//! moves inline and calls `memcpy` or `memset` for a larger one. gcc 16 on x86-64 calls the library
//! only above 256 bytes. Below that it writes vector moves where it may, and a loop of four
//! general purpose words where it may not, which is the whole kernel: `-mno-sse` or
//! `-mgeneral-regs-only` is on every kernel file. A copy of `struct pt_regs` (168 bytes) on every
//! page fault and the zeroing of a `struct kstat` (192 bytes) on every `stat` were calls to
//! `__memcpy` and `__memset`. Each one cost about 130 to 220 instructions in the kernel, where the
//! loop is about 60, and the call took the registers a call clobbers away from the code around it.
//! With the calls, a Linux 7.3 `defconfig` built by rucc ran 1.33 times the instructions of the
//! gcc build for each `stat` and 1.30 times for each `brk`.
//!
//! # What the loop is
//!
//! For `memcpy to, from` of `size` bytes, with `tail` for whatever followed it, and `whole` for
//! `size` rounded down to a multiple of 32:
//!
//! ```text
//! head:                              ; what was in front of the instruction
//!   jump body(0)
//! body(i):
//!   a, b, c, d = load from + i, from + i + 8, from + i + 16, from + i + 24
//!   store a, b, c, d -> to + i, to + i + 8, to + i + 16, to + i + 24
//!   br_if i + 32 < whole, body(i + 32), done()
//! done:
//!   memcpy to + whole, from + whole  ; size - whole bytes, which `bulk` writes as moves
//!   tail
//! ```
//!
//! A fill stores one word of its byte four times instead, made in `head`. A call to `memcpy` or
//! `memset` of a constant size becomes the same loop, as [`expand::bulk`] takes a smaller one
//! apart, and its answer is its first argument.
//!
//! # What it leaves alone
//!
//! A fill whose byte the program works out, a call that unwinds to a landing pad, a call in a
//! function that said the names are not the library's, a size the target writes no loop for, and
//! every function built for size or allowed the vector registers. Those last ones are left to the
//! threshold `cargo xtask stringop` measured against the C library (tamnd/rucc#2971).
//!
//! # Where it runs
//!
//! Beside `fills::loops`, at the front, since it makes blocks and nothing in `expand.rs` may.

use rucc_base::Interner;
use rucc_cost::Goal;
use rucc_ir::{
    AttrSet, Builder, Extra, Flags, Func, Inst, InstData, IntPred, MemInfo, MemOrder, Opcode,
    Restrict, Type, Value,
};

use crate::{expand, retry};

/// How many bytes one pass of the loop moves: four words of eight bytes.
const PASS: u32 = 32;

/// One copy or fill, as the loop needs it.
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
}

/// Rewrites every copy and fill of a known size that [`expand::bulk`] would make a call of, and
/// that is no more than `most` bytes, into the loop the module describes.
///
/// `word` is how many bytes a register holds, `unaligned` is whether a word may be moved at any
/// address, `most` is [`rucc_target::CallRegs::rolled`], which the pipeline makes zero for a
/// function with the vector registers, and `goal` is what the threshold of [`expand::bulk`] is
/// read for.
pub fn loops(
    func: &mut Func,
    names: &Interner,
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
        rewrite(func, inst, &roll);
    }
}

/// The copy or fill an instruction is, where it is one this pass writes as a loop.
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
            if bulk.length.is_some() || func.carries_mem(inst) {
                return None;
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
            let size = u64::try_from(expand::number(func, length)?).ok()?;
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
    let moves = if opcode == Opcode::Memset { moves.1 } else { moves.0 };
    // Only what `bulk` would call for. A word may be moved at any address here, so that is a size
    // whose plan at the widest word is more moves than the threshold.
    if info.size < u64::from(PASS)
        || info.size > u64::from(most)
        || info.order != MemOrder::NotAtomic
        || expand::plan(info.size, 8, 8, moves).is_some()
    {
        return None;
    }
    let flags = flags.intersection(Flags::legal_on(Opcode::Store));
    Some(Roll { opcode, to, with, byte, info, flags, answer })
}

/// One copy or fill, as the loop the module describes.
fn rewrite(func: &mut Func, inst: Inst, roll: &Roll) {
    let head = func.block_of(inst).expect("the instruction is in a block");
    let span = func.span(inst);
    let index = Type::int(64);
    let whole = roll.info.size / u64::from(PASS) * u64::from(PASS);
    let rest = roll.info.size - whole;

    let body = func.create_block();
    let at = func.append_param(body, index);
    let done = func.create_block();
    func.move_after(inst, done);
    func.remove_inst(inst);

    let mut build = Builder::new(func, head).at(span);
    let copy = roll.opcode == Opcode::Memcpy;
    // Worked out here rather than in the loop, so the loop stores one register over and over.
    let value = (!copy).then(|| build.iconst(index, i128::from(expand::spread(roll.byte, 8))));
    // Where the part after the loop starts, which `done` takes apart into moves.
    let past = (rest > 0).then(|| {
        let by = build.iconst(index, i128::from(whole));
        let to = build.binary(Opcode::PtrAdd, roll.to, by, Flags::NONE);
        let with =
            if copy { build.binary(Opcode::PtrAdd, roll.with, by, Flags::NONE) } else { roll.with };
        (to, with)
    });
    let zero = build.iconst(index, 0);
    build.jump(body, &[zero]);

    let access = MemInfo { size: 8, align: roll.info.align.clamp(1, 8), ..roll.info };
    let mut build = Builder::new(func, body).at(span);
    let to = build.binary(Opcode::PtrAdd, roll.to, at, Flags::NONE);
    let stepped = |build: &mut Builder<'_>, base: Value, by: u64| {
        if by == 0 {
            return base;
        }
        let by = build.iconst(index, i128::from(by));
        build.binary(Opcode::PtrAdd, base, by, Flags::NONE)
    };
    let words: Vec<Value> = match value {
        Some(value) => vec![value; 4],
        None => {
            let from = build.binary(Opcode::PtrAdd, roll.with, at, Flags::NONE);
            (0..4)
                .map(|k| {
                    let there = stepped(&mut build, from, k * 8);
                    build.load(index, there, access, roll.flags)
                })
                .collect()
        }
    };
    for (k, word) in (0..4).zip(words) {
        let here = stepped(&mut build, to, k * 8);
        build.store(word, here, access, roll.flags);
    }
    let pass = build.iconst(index, i128::from(PASS));
    let next = build.binary(Opcode::Add, at, pass, Flags::NONE);
    let end = build.iconst(index, i128::from(whole));
    let more = build.icmp(IntPred::Ult, next, end);
    build.br_if(more, body, &[next], done, &[]);

    if let Some((to, with)) = past {
        let info = MemInfo { size: rest, align: roll.info.align.clamp(1, PASS), ..roll.info };
        let extra = Extra::Mem(func.add_mem(info));
        let args = func.push_values(&[to, with]);
        let flags = roll.flags;
        let data = InstData { args, extra, flags, ..InstData::new(roll.opcode) };
        let made = func.create_inst(data, &[], span);
        let first = func.insts(done).next().expect("the block a loop ends in has a terminator");
        func.insert_before(made, first);
    }
    if let Some(answer) = roll.answer {
        retry::replace(func, answer, roll.to);
    }
}
