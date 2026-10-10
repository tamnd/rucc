//! The low byte of a register i386 has no name for.
//!
//! x86-64 can name the low byte of every general purpose register. i386 can name it for `eax`,
//! `ecx`, `edx` and `ebx` only, because the encodings that reach `sil` and `dil` on x86-64 are the
//! ones a REX prefix makes, and there is no REX prefix here. [`bar`] keeps a value named as a
//! byte out of both when the allocator hands them out, but a reload of a spilled value lands in
//! one of them, and when the instruction
//! reading or writing that value names its low byte there is nothing an assembler can write.
//!
//! So each such instruction gets an exchange on either side of it, with a register that has a low
//! byte and that the instruction does not mention at all, and in between it names that register
//! instead. The exchange writes no flags, so a comparison in front of one and the branch or the
//! `cmov` behind it still meet. What the instruction reads is where it expects to find it, since
//! the two registers traded everything they held, and what it writes goes back where the rest of
//! the function expects it when they trade back.

use rucc_base::Interner;
use rucc_base::hash::Set;
use rucc_mir::{Block, Func, Inst, Opcode, Reg};
use rucc_target::x86_64::{Arg, FLAGS, Width, written};
use rucc_target::{PhysReg, Reads, RegClass, x86};

/// The registers with a low byte, in the order they are borrowed. `ebx` first, because nothing
/// names it without saying so and an instruction that names a byte of `esi` or `edi` is often one
/// that also reads `ecx` for a shift or `eax` and `edx` for a division.
const LENDERS: [PhysReg; 4] = [x86::EBX, x86::EDX, x86::ECX, x86::EAX];

/// The two registers that have no low byte and that an instruction may still be handed.
const BYTELESS: [PhysReg; 2] = [x86::ESI, x86::EDI];

/// Keeps every value an instruction names the low byte of out of `esi` and `edi`, ahead of the
/// allocator.
///
/// Only the backtracking allocator hears of it, and it is what lets [`crate::pipeline`] hand the
/// two scratch registers out at all: given one, a value read with `cmpb` or written with `sete`
/// costs two exchanges around each of those, which is more than the register saved. A value it
/// still lands in one, through a reload, is put right by [`reach`].
///
/// Not the byte a comparison writes for the branch or the select behind it to read, which `fused`
/// says of it. The two become one instruction after allocation and nothing names that byte then,
/// and keeping it out of two registers cost `n` its register in a loop that counts to `n`.
pub fn bar(func: &mut Func, prefix: &str, names: &Interner, fused: impl Fn(Inst) -> bool) {
    let all: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in all.into_iter().filter(|&inst| !fused(inst)) {
        let Some(bytes) = named(func, inst, prefix, names) else { continue };
        let operands = func[inst].operands;
        let regs: Vec<Reg> = bytes
            .iter()
            .filter_map(|&at| func[operands].get(usize::from(at)).map(|op| op.reg))
            .collect();
        for reg in regs {
            for at in BYTELESS {
                func.bar(reg, at);
            }
        }
    }
}

/// The operands an instruction names the low byte of, by position, or `None` for one that is not
/// a machine instruction.
fn named(func: &Func, inst: Inst, prefix: &str, names: &Interner) -> Option<Vec<u8>> {
    let name = names.resolve(func[inst].opcode.name()).strip_prefix(prefix)?;
    let spelled = written(name)?;
    let bytes = spelled
        .iter()
        .flat_map(|one| one.args.iter())
        .filter_map(|arg| match *arg {
            Arg::Reg(at, Width::Byte) | Arg::Low(at) => Some(at),
            _ => None,
        })
        .collect();
    Some(bytes)
}

/// Gives every instruction that names the low byte of `esi` or `edi` a register that has one.
///
/// A `testb` of one of them asks whether its low byte is zero, and `testl $255` asks the same of
/// the whole register in one instruction where the exchanges make it three. The two leave the zero,
/// the carry and the parity alike and differ in the sign, so it is only written where nothing reads
/// the sign before the flags are written again. That is a `bool` kept across a call in one of the
/// registers the call saves, and in drivers/md/md.c on i386 it was twenty `testb`s.
pub fn reach(func: &mut Func, prefix: &str, class: RegClass, names: &mut Interner) {
    let exchange = Opcode::new(names.intern(&format!("{prefix}xchg_rr_32")));
    let whole = Opcode::new(names.intern(&format!("{prefix}test_ri_32")));
    let all: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in all {
        let Some(bytes) = named(func, inst, prefix, names) else { continue };
        let operands = func[inst].operands;
        let mut stuck: Vec<PhysReg> = bytes
            .iter()
            .filter_map(|&at| func[operands].get(usize::from(at)).and_then(|op| op.reg.phys()))
            .filter(|reg| BYTELESS.contains(reg))
            .collect();
        stuck.dedup();
        if stuck.is_empty() {
            continue;
        }
        if names.resolve(func[inst].opcode.name()).strip_prefix(prefix) == Some("test_rr_8")
            && zero_only(func, inst, prefix, names)
        {
            let imm = func.add_imm(0xff);
            func[inst].opcode = whole;
            func[inst].imm = Some(imm);
            continue;
        }
        let named: Vec<PhysReg> = func[operands].iter().filter_map(|op| op.reg.phys()).collect();
        let mut free = LENDERS.iter().copied().filter(|reg| !named.contains(reg));
        let pairs: Vec<(PhysReg, PhysReg)> =
            stuck.iter().map_while(|&from| free.next().map(|to| (from, to))).collect();
        if pairs.len() != stuck.len() {
            continue;
        }
        for op in &mut func[operands] {
            let Some(reg) = op.reg.phys() else { continue };
            if let Some(&(_, to)) = pairs.iter().find(|&&(from, _)| from == reg) {
                op.reg = Reg::physical(to);
            }
        }
        for &(from, to) in &pairs {
            let (from, to) = (Reg::physical(from), Reg::physical(to));
            let before = func
                .build_loose(exchange)
                .def(from, class)
                .def(to, class)
                .uses(from, class)
                .uses(to, class)
                .finish();
            func.insert_before(inst, before);
            let after = func
                .build_loose(exchange)
                .def(from, class)
                .def(to, class)
                .uses(from, class)
                .uses(to, class)
                .finish();
            func.insert_after(inst, after);
        }
    }
}

/// Whether everything that reads the flags an instruction leaves, up to the next instruction that
/// writes them, asks only about the zero or about the carry with it.
///
/// The flags that reach the end of the block are followed into each block it goes to, which is
/// where the branch a `testb` is for has left them, and on through any of those that writes no
/// flags either, which is a block of moves the allocator put on an edge.
fn zero_only(func: &Func, inst: Inst, prefix: &str, names: &Interner) -> bool {
    let Some(block) = func.block_of(inst) else { return false };
    let rest = func.insts(block).skip_while(|&at| at != inst).skip(1);
    let mut seen: Set<Block> = Set::default();
    let mut ahead = match read(func, rest, prefix, names) {
        Some(answer) => return answer,
        None => vec![block],
    };
    while let Some(block) = ahead.pop() {
        for call in &func[block].succs {
            if !seen.insert(call.block) {
                continue;
            }
            match read(func, func.insts(call.block), prefix, names) {
                Some(false) => return false,
                Some(true) => {}
                None => ahead.push(call.block),
            }
        }
    }
    true
}

/// Whether the instructions read the flags as [`zero_only`] asks, up to the first that writes
/// them, or `None` when none of them does.
fn read(
    func: &Func,
    insts: impl Iterator<Item = Inst>,
    prefix: &str,
    names: &Interner,
) -> Option<bool> {
    for inst in insts {
        let Some(name) = names.resolve(func[inst].opcode.name()).strip_prefix(prefix) else {
            return Some(false);
        };
        if (FLAGS.compares_itself)(name) {
            return Some(true);
        }
        match FLAGS.reads(name) {
            Some(Reads::Zero | Reads::Unsigned) | None => {}
            Some(_) => return Some(false),
        }
        if (FLAGS.writes)(name) {
            return Some(true);
        }
    }
    None
}
