//! A load in a loop that writes nothing reads memory again on each pass, as kencc does.
//!
//! kencc keeps no value from memory in a register across a loop. Plan 9 code depends on this. The
//! `ilock` of 9front's `port/taslock.c` waits for another processor with
//!
//! ```c
//! while(l->key)
//!     ;
//! ```
//!
//! and `key` is not `volatile`. The loop writes no memory, so C lets a compiler read `l->key` one
//! time before the loop, and rucc did that. The loop then never stops, and the kernel hangs at
//! the first lock that is busy.
//!
//! Under `-fkencc` this step puts an empty `asm volatile("" ::: "memory")` at the top of the header
//! of each loop that has a load and no instruction that writes memory. A call does not count as a
//! write here, because `crate::modref` can show that a call writes nothing. The barrier is a write
//! of all memory that no pass can move, so a load after it cannot be the load before the loop.
//! The code generator emits nothing for it. A loop that writes memory needs no barrier, because
//! `-fkencc` also turns off the type based alias rules, and a store through a pointer can then
//! change what a load in the same loop reads.
//!
//! It runs first in the pipeline, before the inliner, because the inliner runs passes of its own.

use rucc_base::Interner;
use rucc_ir::{AsmInfo, BlockCallList, Flags, Func, FuncId, InstData, Module, Opcode};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::Loops;

/// Puts the barrier in each loop of each function in the module that needs one, and gives the
/// number of barriers.
pub fn run(module: &mut Module, names: &mut Interner) -> usize {
    let info = AsmInfo {
        template: names.intern(""),
        constraints: names.intern(""),
        clobbers: names.intern("memory"),
        targets: BlockCallList::EMPTY,
    };
    let mut added = 0;
    for id in module.funcs().collect::<Vec<FuncId>>() {
        if module[id].is_declaration() {
            continue;
        }
        added += func(&mut module[id], info);
    }
    added
}

/// Puts the barrier in each loop of one function that needs one.
fn func(func: &mut Func, info: AsmInfo) -> usize {
    let cfg = Cfg::new(func);
    let doms = Dominators::new(&cfg);
    let loops = Loops::new(&cfg, &doms);
    let headers: Vec<_> = loops
        .all()
        .filter(|&id| {
            let mut loads = false;
            for &block in loops.blocks(id) {
                for inst in func.insts(block) {
                    let opcode = func[inst].opcode;
                    if opcode.writes_memory() && opcode != Opcode::Call {
                        return false;
                    }
                    loads |= opcode == Opcode::Load;
                }
            }
            loads
        })
        .map(|id| loops.header(id))
        .collect();
    let mut added = 0;
    for header in headers {
        let Some(first) = func.insts(header).next() else { continue };
        let span = func.span(first);
        let asm = func.add_asm(info);
        let data = InstData {
            flags: Flags::VOLATILE,
            extra: rucc_ir::Extra::Asm(asm),
            ..InstData::new(Opcode::InlineAsm)
        };
        let barrier = func.create_inst(data, &[], span);
        func.insert_before(barrier, first);
        added += 1;
    }
    added
}
