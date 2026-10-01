//! The low byte of a register i386 has no name for.
//!
//! x86-64 can name the low byte of every general purpose register. i386 can name it for `eax`,
//! `ecx`, `edx` and `ebx` only, because the encodings that reach `sil` and `dil` on x86-64 are the
//! ones a REX prefix makes, and there is no REX prefix here. The allocator is never asked for
//! either register, since `crate::pipeline::X86_SCRATCH` holds both back, but a reload of a
//! spilled value lands in one of them, and when the instruction reading or writing that value names
//! its low byte there is nothing an assembler can write.
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

/// Gives every instruction that names the low byte of `esi` or `edi` a register that has one.
pub fn reach(func: &mut Func, prefix: &str, class: RegClass, names: &mut Interner) {
    let exchange = Opcode::new(names.intern(&format!("{prefix}xchg_rr_32")));
    let all: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in all {
        let Some(name) = names.resolve(func[inst].opcode.name()).strip_prefix(prefix) else {
            continue;
        };
        let Some(spelled) = written(name) else { continue };
        let operands = func[inst].operands;
        let bytes: Vec<u8> = spelled
            .iter()
            .flat_map(|one| one.args.iter())
            .filter_map(|arg| match *arg {
                Arg::Reg(at, Width::Byte) | Arg::Low(at) => Some(at),
                _ => None,
            })
            .collect();
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
