//! An ordered access two registers wide, as `lock cmpxchg8b` on i386 and `lock cmpxchg16b` on
//! x86-64.
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
//! does in one go into a plain one, so that what is left two registers wide is exactly what has
//! no plain form.
//!
//! # Sixteen bytes on x86-64
//!
//! x86-64 has the same instruction one size up, with `rdx:rax` and `rcx:rbx`, and the same shapes
//! apply. It is not in the baseline, so gcc uses it only in a function built with `cx16`, and only
//! for the `__sync` names: the `__atomic` names call libatomic at sixteen bytes whatever the
//! function is built for. `rucc-lower` makes the same choice, so an access this wide that reaches
//! here on x86-64 is one that was meant to be the instruction.
//!
//! # Sixteen bytes on AArch64
//!
//! AArch64 has no one instruction for this in its baseline, and gcc writes the loop every manual
//! gives: `ldaxp` reads the pair and marks the address, `stlxp` writes the new pair only if nothing
//! else wrote there since, and the loop goes round again when something did. A pair read with
//! `ldaxp` is only known to be one read once a store exclusive to the same address has succeeded,
//! so when the pair is not the one expected the loop writes back what it read, as gcc 13 and later
//! do. The large system extensions add `caspal`, which is the whole loop in one instruction, and a
//! function built for them gets that. gcc does this for the `__sync` names, and so does
//! `rucc-lower`, which also leaves the C11 names to libatomic at this width as it does on x86-64.
//! See [`paired`].

use rucc_base::Interner;
use rucc_ir::{
    AsmInfo, Block, BlockCallList, Extra, Func, Imm, Inst, InstData, IntPred, Opcode, Type, Value,
};
use rucc_target::CallRegs;

use crate::expand::{ahead, ahead_cmp, ahead_const};

/// Every ordered access two registers wide, on x86 under the convention `conv`, as the compare and
/// exchange of that many bytes. Nothing on AArch64, which has no such instruction.
pub fn ordered(func: &mut Func, names: &mut Interner, conv: &CallRegs) {
    if !conv.total_store_order {
        return;
    }
    let pair = Pair { half: conv.word * 8 };
    let found: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| matches!(func[inst].opcode, Opcode::Cmpxchg | Opcode::AtomicLoad))
        .filter(|&inst| func[inst].first_result.is_some_and(|value| pair.is(func[value].ty)))
        .collect();
    for inst in found {
        if func[inst].opcode == Opcode::AtomicLoad {
            load(func, names, pair, inst);
        } else {
            exchange(func, names, pair, inst);
        }
    }
}

/// The sixteen byte integer on AArch64, the register each half of it is in and the status a store
/// exclusive leaves.
const WHOLE: Type = Type::int(128);
const HALF: Type = Type::int(64);
const STATUS: Type = Type::int(32);

/// How a compare and exchange of two registers is written on AArch64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusive {
    /// The loop of `ldaxp` and `stlxp`, which every AArch64 processor has.
    Loop,
    /// `caspal`, for a function built for the large system extensions.
    Casp,
}

/// Every ordered access of sixteen bytes on AArch64, as the compare and exchange of two registers
/// written the way `how` says. A load is the exchange of zero for zero, as it is on x86 above.
pub fn paired(func: &mut Func, names: &mut Interner, how: Exclusive) {
    let found: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| matches!(func[inst].opcode, Opcode::Cmpxchg | Opcode::AtomicLoad))
        .filter(|&inst| func[inst].first_result.is_some_and(|value| func[value].ty == WHOLE))
        .collect();
    for inst in found {
        let results: Vec<Value> = func[inst].results().collect();
        let (addr, expected, desired) = match (func[inst].opcode, &func[func[inst].args]) {
            (Opcode::AtomicLoad, &[addr]) => {
                let zero = ahead_const(func, inst, Imm::int(0, WHOLE), WHOLE);
                (addr, zero, zero)
            }
            (Opcode::Cmpxchg, &[addr, expected, desired]) => (addr, expected, desired),
            _ => continue,
        };
        let (got, ok) = exclusive(func, names, how, inst, addr, (expected, desired));
        func.remove_inst(inst);
        replace(func, results[0], got);
        if let Some(&exchanged) = results.get(1) {
            replace(func, exchanged, ok);
        }
    }
}

/// The loop of `ldaxp` and `stlxp`. Operands zero and one are the pair it read, two is the status
/// of the store exclusive, three the address, four and five the pair it expects and six and seven
/// the pair it puts there. The flags after `ccmp` say whether the pair read is the one expected,
/// and nothing after them changes the flags, so the two ways out of the loop are told apart again
/// in the IR from the pair read.
const LOOP: &str = "0:\tldaxp\t%0, %1, [%3]\n\
                    \tcmp\t%0, %4\n\
                    \tccmp\t%1, %5, #0, eq\n\
                    \tb.ne\t1f\n\
                    \tstlxp\t%w2, %6, %7, [%3]\n\
                    \tcbnz\t%w2, 0b\n\
                    \tb\t2f\n\
                    1:\tstlxp\t%w2, %0, %1, [%3]\n\
                    \tcbnz\t%w2, 0b\n\
                    2:";

/// `caspal`, which wants each pair in an even register and the one after it. No constraint letter
/// says that, so the pairs go through `x0` to `x3`, which are clobbered. Operands zero and one are
/// the pair it read, two the address, three and four the pair it expects and five and six the
/// pair it puts there.
const CASP: &str = "mov\tx0, %3\n\
                    \tmov\tx1, %4\n\
                    \tmov\tx2, %5\n\
                    \tmov\tx3, %6\n\
                    \tcaspal\tx0, x1, x2, x3, [%2]\n\
                    \tmov\t%0, x0\n\
                    \tmov\t%1, x1";

/// The statement, in front of `inst`, and what it found and whether it put `desired` there.
///
/// Each value is handed over as its two halves, worked out with a truncation and a shift that
/// `crate::wide` splits like any other, and what it found comes back as two halves put together
/// again. Whether it put the value there is whether what it found is what it expected.
fn exclusive(
    func: &mut Func,
    names: &mut Interner,
    how: Exclusive,
    inst: Inst,
    addr: Value,
    (expected, desired): (Value, Value),
) -> (Value, Value) {
    let shift = ahead_const(func, inst, Imm::int(64, WHOLE), WHOLE);
    let halves = |func: &mut Func, whole: Value| {
        let low = ahead(func, inst, Opcode::Trunc, &[whole], HALF);
        let top = ahead(func, inst, Opcode::LShr, &[whole, shift], WHOLE);
        let high = ahead(func, inst, Opcode::Trunc, &[top], HALF);
        [low, high]
    };
    let [want_low, want_high] = halves(func, expected);
    let [put_low, put_high] = halves(func, desired);
    let inputs = [addr, want_low, want_high, put_low, put_high];
    let (template, constraints, clobbers, outputs): (_, _, _, &[Type]) = match how {
        Exclusive::Loop => (LOOP, "=&r,=&r,=&r,r,r,r,r,r", "memory,cc", &[HALF, HALF, STATUS]),
        Exclusive::Casp => (CASP, "=r,=r,r,r,r,r,r", "x0,x1,x2,x3,memory", &[HALF, HALF]),
    };
    let info = AsmInfo {
        template: names.intern(template),
        constraints: names.intern(constraints),
        clobbers: names.intern(clobbers),
        targets: BlockCallList::EMPTY,
    };
    let asm = func.add_asm(info);
    let args = func.push_values(&inputs);
    let data = InstData { args, extra: Extra::Asm(asm), ..InstData::new(Opcode::InlineAsm) };
    let span = func.span(inst);
    let made = func.create_inst(data, outputs, span);
    func.insert_before(made, inst);
    let found: Vec<Value> = func[made].results().collect();
    let low = ahead(func, inst, Opcode::ZExt, &[found[0]], WHOLE);
    let high = ahead(func, inst, Opcode::ZExt, &[found[1]], WHOLE);
    let high = ahead(func, inst, Opcode::Shl, &[high, shift], WHOLE);
    let got = ahead(func, inst, Opcode::Or, &[low, high], WHOLE);
    let ok = ahead_cmp(func, inst, Opcode::ICmp, Extra::IntPred(IntPred::Eq), &[got, expected]);
    (got, ok)
}

/// The two registers the instruction moves, by how wide one of them is.
#[derive(Clone, Copy)]
struct Pair {
    /// How many bits one register holds, which is thirty two on i386 and sixty four on x86-64.
    half: u32,
}

impl Pair {
    /// Whether a value is the integer the instruction moves.
    fn is(self, ty: Type) -> bool {
        ty.is_int() && ty.lanes() == 1 && ty.bits() == self.half * 2
    }

    /// The type of the whole value.
    fn whole(self) -> Type {
        Type::int(self.half * 2)
    }

    /// The instruction, with the address as operand four.
    fn template(self) -> &'static str {
        if self.half == 64 { "lock; cmpxchg16b (%4)" } else { "lock; cmpxchg8b (%4)" }
    }
}

/// A load as the compare and exchange of zero for zero, whose answer is what was there.
fn load(func: &mut Func, names: &mut Interner, pair: Pair, inst: Inst) {
    let [addr] = func[func[inst].args] else { return };
    let Some(old) = func[inst].first_result else { return };
    let whole = pair.whole();
    let zero = ahead_const(func, inst, Imm::int(0, whole), whole);
    let (got, _) = written(func, names, pair, inst, addr, (zero, zero));
    func.remove_inst(inst);
    replace(func, old, got);
}

/// The compare and exchange itself.
fn exchange(func: &mut Func, names: &mut Interner, pair: Pair, inst: Inst) {
    let [addr, expected, desired] = func[func[inst].args] else { return };
    let results: Vec<Value> = func[inst].results().collect();
    let (got, ok) = written(func, names, pair, inst, addr, (expected, desired));
    func.remove_inst(inst);
    if let [old, exchanged] = results[..] {
        replace(func, old, got);
        replace(func, exchanged, ok);
    }
}

/// The statement, in front of `inst`, and what it found and whether it put `desired` there.
///
/// The value to put is handed over as its two halves in `ebx` and `ecx`, or `rbx` and `rcx`,
/// worked out here with a truncation and a shift that `crate::wide` splits like any other. The
/// address is in whichever register the other operands left, which is `esi`, `edi` or `ebp` on
/// i386.
///
/// Whether it put the value there is whether what it found is what it expected, which is what the
/// zero flag says too. The flag is not read out with `sete`, because on i386 with `eax`, `ebx`,
/// `ecx` and `edx` all taken there is no register left with a byte to set.
fn written(
    func: &mut Func,
    names: &mut Interner,
    pair: Pair,
    inst: Inst,
    addr: Value,
    (expected, desired): (Value, Value),
) -> (Value, Value) {
    let half = Type::int(pair.half);
    let whole = pair.whole();
    let low = ahead(func, inst, Opcode::Trunc, &[desired], half);
    let shift = ahead_const(func, inst, Imm::int(i128::from(pair.half), whole), whole);
    let top = ahead(func, inst, Opcode::LShr, &[desired, shift], whole);
    let high = ahead(func, inst, Opcode::Trunc, &[top], half);
    let info = AsmInfo {
        template: names.intern(pair.template()),
        constraints: names.intern("=A,0,b,c,r"),
        clobbers: names.intern("memory,cc"),
        targets: BlockCallList::EMPTY,
    };
    let asm = func.add_asm(info);
    let args = func.push_values(&[expected, low, high, addr]);
    let data = InstData { args, extra: Extra::Asm(asm), ..InstData::new(Opcode::InlineAsm) };
    let span = func.span(inst);
    let made = func.create_inst(data, &[whole], span);
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
