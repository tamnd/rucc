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
use rucc_mir::{Func, Inst, Opcode, Reg};
use rucc_target::x86_64::{Arg, Width, written};
use rucc_target::{PhysReg, RegClass, x86};

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
pub fn reach(func: &mut Func, prefix: &str, class: RegClass, names: &mut Interner) {
    let exchange = Opcode::new(names.intern(&format!("{prefix}xchg_rr_32")));
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
