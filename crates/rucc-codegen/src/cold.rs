//! Which blocks of a function are not expected to run, so that the layout can put them in a part
//! of their own.
//!
//! gcc splits a function in two at `-O2`: the blocks it believes never run go in `.text.unlikely`
//! under the function's name with `.cold` after it, and the rest stay where the function is. The
//! point is the instruction cache. An error path that prints a message and gives up is as long as
//! the work it guards and runs once in a lifetime, and in the kernel it is most of the bytes of
//! many functions, because `printk` and `WARN` are written `cold`.
//!
//! Which blocks those are is gcc's `determine_unlikely_bbs` in `gcc/predict.cc`, read the same way
//! here, because a block this puts somewhere gcc does not is a section the kernel's build checks do
//! not expect. Three steps:
//!
//! 1. A block is cold when it calls a function written `cold` before it does anything else that
//!    might not come back, which is any other call or a `volatile` asm. A function that is itself
//!    `cold` has nothing cold in it, since all of it is.
//! 2. A block every way into which is from a cold block is cold. A way in that runs backwards,
//!    which is the jump round a loop, does not count, so a loop whose way in is cold is cold.
//! 3. A block every way out of which is into a cold block is cold, as long as it does nothing that
//!    might not come back. A block with no way out, which is one that returns or one that ends in
//!    `unreachable`, is taken to leave by a way that is not cold. gcc's `__builtin_unreachable` is
//!    a call that does not come back, so the block it ends is never cold there either.
//!
//! The entry block is never cold, whatever the steps say. A function needs a first part, and a
//! function all of whose paths are cold is one gcc moves whole rather than splits.
//!
//! The answer is written on the machine blocks straight after selection, while they still stand
//! one for one with the IR blocks, and [`crate::layout`] reads it.

use rucc_ir::{self as ir, AttrSet, Extra, Flags, Opcode};
use rucc_mir as mir;
use rucc_opt::Cfg;

use crate::elsewhere::Elsewhere;

/// Says on the machine function which of its blocks are cold.
///
/// `blocks` is [`crate::lower::Lowered::blocks`], the machine block each IR block became. A
/// function the layout cannot split is left alone: one the program put in a section, whose cold
/// part would need a section the program did not name, and one with a landing pad, whose call site
/// table is written for one stretch of code.
pub fn mark(
    source: &ir::Func,
    blocks: &[Option<mir::Block>],
    func: &mut mir::Func,
    elsewhere: &Elsewhere,
) {
    if source.attrs.set.contains(AttrSet::COLD)
        || func.section.is_some()
        || func.retain
        || !func.landings.is_empty()
    {
        return;
    }
    let cold = unlikely(source, elsewhere);
    for block in source.blocks() {
        if !cold.get(block.index()).copied().unwrap_or(false) {
            continue;
        }
        if let Some(&Some(out)) = blocks.get(block.index()) {
            func.set_cold(out);
        }
    }
}

/// Which IR blocks are cold, indexed by the block's own number. See the module documentation.
fn unlikely(source: &ir::Func, elsewhere: &Elsewhere) -> Vec<bool> {
    let cfg = Cfg::new(source);
    let Some(entry) = cfg.entry() else { return Vec::new() };
    let mut cold = vec![false; cfg.capacity()];
    let mut work = Vec::new();
    for block in cfg.reverse_postorder() {
        if block != entry && calls_cold_first(source, block, elsewhere) {
            cold[block.index()] = true;
            work.push(block);
        }
    }
    // Forwards: a block whose every way in that is not a way round a loop is from a cold block.
    let backwards = |from: ir::Block, to: ir::Block| match (cfg.rank(from), cfg.rank(to)) {
        (Some(from), Some(to)) => to <= from,
        _ => false,
    };
    while let Some(block) = work.pop() {
        for &succ in cfg.successors(block) {
            if cold[succ.index()] || succ == entry {
                continue;
            }
            let warm = cfg
                .predecessors(succ)
                .iter()
                .any(|&pred| cfg.reaches(pred) && !cold[pred.index()] && !backwards(pred, succ));
            if !warm {
                cold[succ.index()] = true;
                work.push(succ);
            }
        }
    }
    // Backwards: a block whose every way out is into a cold block. A block with none leaves by
    // the way back to the caller, which is not cold.
    let mut warm_ways = vec![0usize; cfg.capacity()];
    for block in cfg.reverse_postorder() {
        if cold[block.index()] {
            continue;
        }
        let succs = cfg.successors(block);
        let warm =
            succs.iter().filter(|succ| !cold[succ.index()]).count() + usize::from(succs.is_empty());
        warm_ways[block.index()] = warm;
        if warm == 0 {
            work.push(block);
        }
    }
    while let Some(block) = work.pop() {
        if cold[block.index()] || block == entry || might_not_come_back(source, block) {
            continue;
        }
        cold[block.index()] = true;
        for &pred in cfg.predecessors(block) {
            if cold[pred.index()] || !cfg.reaches(pred) {
                continue;
            }
            warm_ways[pred.index()] = warm_ways[pred.index()].saturating_sub(1);
            if warm_ways[pred.index()] == 0 {
                work.push(pred);
            }
        }
    }
    cold
}

/// Whether the block calls a function written `cold` before anything else in it might not come
/// back.
fn calls_cold_first(source: &ir::Func, block: ir::Block, elsewhere: &Elsewhere) -> bool {
    for inst in source.insts(block) {
        if callee(source, inst).is_some_and(|name| elsewhere.cold(name)) {
            return true;
        }
        if stops(source, inst) {
            return false;
        }
    }
    false
}

/// Whether anything in the block might not come back, which is a call or a `volatile` asm.
fn might_not_come_back(source: &ir::Func, block: ir::Block) -> bool {
    source.insts(block).any(|inst| stops(source, inst))
}

/// Whether control might leave the function at this instruction or never get past it, which gcc
/// asks with `stmt_can_terminate_bb_p`. A call might, since whatever it calls might `exit`, and so
/// might a `volatile` asm, since nothing says what it does.
fn stops(source: &ir::Func, inst: ir::Inst) -> bool {
    let data = &source[inst];
    match data.opcode {
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => true,
        Opcode::InlineAsm => data.flags.contains(Flags::VOLATILE),
        _ => false,
    }
}

/// The name a direct call calls, and nothing for anything else.
fn callee(source: &ir::Func, inst: ir::Inst) -> Option<rucc_base::Symbol> {
    let data = &source[inst];
    if !matches!(data.opcode, Opcode::Call | Opcode::TailCall) {
        return None;
    }
    let Extra::Call(at) = data.extra else { return None };
    source[at].callee
}
