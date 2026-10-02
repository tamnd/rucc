//! An ordered access to eight bytes on a machine whose registers hold four, as `lock cmpxchg8b`.
//!
//! i386 has one instruction that reads or writes eight bytes of ordinary memory in one go and is
//! told which registers to use, and it is the compare and exchange. gcc reaches for it for every
//! `long long` that `__atomic` or `__sync` touches on a Pentium or later, and so does this:
//!
//! - a compare and exchange is the instruction itself, with the value it expects in `edx:eax`, the
//!   one it puts there in `ecx:ebx`, and what it found in `edx:eax`;
//! - an ordered load is a compare and exchange of zero for zero, which writes back what was there
//!   if it was zero and leaves it alone if it was not, and either way says what it was;
//! - a read modify write and an ordered store were already a loop around a compare and exchange by
//!   the time this runs, which is `crate::retry`'s doing.
//!
//! The instruction is written as an assembly statement rather than as a rule, because no rule can
//! name the four registers a pair of pairs has to be in. `crate::wide` already knows that an `"A"`
//! operand of a `long long` on i386 is `edx:eax`, and splits the statement when it splits
//! everything else, so this is the only place the instruction is spelled out. The `lock` prefix
//! makes it a full barrier, which is the strongest ordering a program can ask for, so every
//! ordering is met by the one shape.
//!
//! Runs in the orderings step, after `crate::expand::orderings` has turned every access the machine
//! does in one go into a plain one, so that what is left at eight bytes is exactly what has no
//! plain form.

use rucc_base::Interner;
use rucc_ir::{
    AsmInfo, Block, BlockCallList, Extra, Func, Imm, Inst, InstData, IntPred, Opcode, Type, Value,
};

use crate::expand::{ahead, ahead_cmp, ahead_const};

/// Every ordered access wider than a register, on a machine whose registers are `word` bytes, as
/// the compare and exchange of eight bytes. Nothing anywhere but i386, the one target with four
/// byte registers.
pub fn pairs(func: &mut Func, names: &mut Interner, word: u32) {
    if word != 4 {
        return;
    }
    let found: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| matches!(func[inst].opcode, Opcode::Cmpxchg | Opcode::AtomicLoad))
        .filter(|&inst| func[inst].first_result.is_some_and(|value| eight(func[value].ty)))
        .collect();
    for inst in found {
        if func[inst].opcode == Opcode::AtomicLoad {
            load(func, names, inst);
        } else {
            exchange(func, names, inst);
        }
    }
}

/// Whether a value is the eight byte integer the instruction moves.
fn eight(ty: Type) -> bool {
    ty.is_int() && ty.lanes() == 1 && ty.bits() == 64
}

/// A load as the compare and exchange of zero for zero, whose answer is what was there.
fn load(func: &mut Func, names: &mut Interner, inst: Inst) {
    let [addr] = func[func[inst].args] else { return };
    let Some(old) = func[inst].first_result else { return };
    let i64 = Type::int(64);
    let zero = ahead_const(func, inst, Imm::int(0, i64), i64);
    let (got, _) = written(func, names, inst, addr, zero, zero);
    func.remove_inst(inst);
    replace(func, old, got);
}

/// The compare and exchange itself.
fn exchange(func: &mut Func, names: &mut Interner, inst: Inst) {
    let [addr, expected, desired] = func[func[inst].args] else { return };
    let results: Vec<Value> = func[inst].results().collect();
    let (got, ok) = written(func, names, inst, addr, expected, desired);
    func.remove_inst(inst);
    if let [old, exchanged] = results[..] {
        replace(func, old, got);
        replace(func, exchanged, ok);
    }
}

/// The statement, in front of `inst`, and what it found and whether it put `desired` there.
///
/// The value to put is handed over as its two halves in `ebx` and `ecx`, worked out here with a
/// truncation and a shift that `crate::wide` splits like any other. The address is in whichever
/// register the other operands left, which is `esi`, `edi` or `ebp`.
///
/// Whether it put the value there is whether what it found is what it expected, which is what the
/// zero flag says too. The flag is not read out with `sete`, because with `eax`, `ebx`, `ecx` and
/// `edx` all taken there is no register left with a byte to set.
fn written(
    func: &mut Func,
    names: &mut Interner,
    inst: Inst,
    addr: Value,
    expected: Value,
    desired: Value,
) -> (Value, Value) {
    let i32 = Type::int(32);
    let i64 = Type::int(64);
    let low = ahead(func, inst, Opcode::Trunc, &[desired], i32);
    let shift = ahead_const(func, inst, Imm::int(32, i64), i64);
    let top = ahead(func, inst, Opcode::LShr, &[desired, shift], i64);
    let high = ahead(func, inst, Opcode::Trunc, &[top], i32);
    let info = AsmInfo {
        template: names.intern("lock; cmpxchg8b (%4)"),
        constraints: names.intern("=A,0,b,c,r"),
        clobbers: names.intern("memory,cc"),
        targets: BlockCallList::EMPTY,
    };
    let asm = func.add_asm(info);
    let args = func.push_values(&[expected, low, high, addr]);
    let data = InstData { args, extra: Extra::Asm(asm), ..InstData::new(Opcode::InlineAsm) };
    let span = func.span(inst);
    let made = func.create_inst(data, &[i64], span);
    func.insert_before(made, inst);
    let got = func[made].first_result.expect("one result was asked for");
    let ok = ahead_cmp(func, inst, Opcode::ICmp, Extra::IntPred(IntPred::Eq), &[got, expected]);
    (got, ok)
}

/// Every read of `from` reads `to` instead, in the arguments and in what the branches carry.
fn replace(func: &mut Func, from: Value, to: Value) {
    let blocks: Vec<Block> = func.blocks().collect();
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let mut lists = vec![func[inst].args];
            lists.extend(func.successors(inst).map(|call| call.args));
            for list in lists {
                func.rewrite(list, |value| if value == from { to } else { value });
            }
        }
    }
}
