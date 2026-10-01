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
//! not expect. Four steps:
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
//! 4. A block that a block not cold branches to is not cold after all when some way into that
//!    block decides the branch towards it, as in `dput(self); if (ret) pr_err(...)` with `ret`
//!    known on one path. gcc works out the first three before its later jump threading runs, and
//!    the threader copies `dput(self)` onto that path, where it then falls into `pr_err` every
//!    time it runs. When gcc splits the function it keeps any block a way into which might run,
//!    which that one now is, so `pr_err` stays where the function is. What steps 2 and 3 made
//!    cold only because of such a block is not cold either. A way in can also decide the branch
//!    by coming out of a test of the same condition, as the way out of `for (i = 0; i < n; i++)`
//!    does for an `if (i < n) ... else pr_err(...)` after the loop.
//!
//! The entry block is never cold, whatever the steps say. A function needs a first part, and a
//! function all of whose paths are cold is one gcc moves whole rather than splits.
//!
//! The answer is written on the machine blocks straight after selection, while they still stand
//! one for one with the IR blocks, and [`crate::layout`] reads it.

use rucc_cost::heuristics;
use rucc_ir::{self as ir, AttrSet, Extra, Flags, Opcode};
use rucc_mir as mir;
use rucc_opt::{Cfg, thread};

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
    // Which cold blocks are cold for the blocks in front of them, which is step 2, and which for
    // the blocks after them, which is step 3. Step 4 can take either reason away.
    let mut ahead = vec![false; cfg.capacity()];
    let mut behind = vec![false; cfg.capacity()];
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
                ahead[succ.index()] = true;
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
        behind[block.index()] = true;
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
    // Last, the blocks gcc's later threading puts a way in that runs in front of, and then what
    // steps 2 and 3 said only because of them.
    let before = cold.clone();
    let warm = |block: ir::Block| !before[block.index()];
    for block in cfg.reverse_postorder() {
        if before[block.index()] && behind_a_thread(source, &cfg, block, warm) {
            cold[block.index()] = false;
            work.push(block);
        }
    }
    while let Some(block) = work.pop() {
        for &succ in cfg.successors(block) {
            let still = cfg
                .predecessors(succ)
                .iter()
                .all(|&pred| !cfg.reaches(pred) || cold[pred.index()] || backwards(pred, succ));
            if cold[succ.index()] && ahead[succ.index()] && !still {
                cold[succ.index()] = false;
                work.push(succ);
            }
        }
        for &pred in cfg.predecessors(block) {
            if cold[pred.index()] && behind[pred.index()] {
                cold[pred.index()] = false;
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

/// Whether a block not cold branches to this one on a way into it that decides the branch, which is
/// step 4. A block that only jumps on is looked through on the way up, since an edge from a block
/// that branches into one with more than one way in is split before selection and the branch then
/// goes to the block that only jumps.
fn behind_a_thread(
    source: &ir::Func,
    cfg: &Cfg,
    block: ir::Block,
    warm: impl Fn(ir::Block) -> bool + Copy,
) -> bool {
    cfg.predecessors(block).iter().any(|&pred| {
        let (mut pred, mut arm) = (pred, block);
        while let [only] = *cfg.predecessors(pred) {
            if !only_jumps(source, pred) {
                break;
            }
            (pred, arm) = (only, pred);
        }
        cfg.reaches(pred)
            && warm(pred)
            && source.insts(pred).count() <= heuristics::JUMP_THREAD_DUPLICATION_INSNS as usize
            && thread::decided_towards(source, pred, arm, warm)
    })
}

/// Whether all the block does is jump on.
fn only_jumps(source: &ir::Func, block: ir::Block) -> bool {
    source.insts(block).all(|inst| source[inst].opcode == Opcode::Jump)
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

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::Pic;
    use rucc_target::ObjectFormat;

    use super::unlikely;
    use crate::elsewhere::Elsewhere;

    /// The kernel's `proc_setup_self`, cut down: `ret` is -12 unless both allocations work, the
    /// second failing calls `dput` on the way to the test of `ret`, and the first goes straight to
    /// the `printk`. `{arm}` is what the test of `ret` goes to when it is not zero.
    const SETUP: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"

func @alloc() -> i32, linkage(external);
func @dput(), linkage(external);
func @_printk(), linkage(external), attrs(cold);
func @f() -> i32, linkage(external) {
block0:
    %0 = iconst.i32 -12
    %1 = call @alloc() : () -> i32
    br_if %1, block1, block8
block1:
    %2 = call @alloc() : () -> i32
    br_if %2, block2, block3
block2:
    %3 = iconst.i32 0
    jump block4(%3)
block3:
    jump block4(%0)
block4(%4: i32):
    call @dput() : ()
    %5 = iconst.i32 0
    %6 = icmp ne %4, %5
    br_if %6, {arm}, block6
block5:
    call @_printk() : ()
    jump block7
block6:
    jump block7
block7:
    return %0
block8:
    jump block5
block9:
    jump block5
block10(%7: i32):
    jump block5
}
"#;

    fn cold(arm: &str) -> Vec<usize> {
        let mut names = Interner::new();
        let text = SETUP.replace("{arm}", arm);
        let module = rucc_ir::parse(&text, &mut names).expect("the fixture is IR");
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        let id = module.funcs().last().expect("the fixture has a function");
        let func: &rucc_ir::Func = &module[id];
        let marks = unlikely(func, &elsewhere);
        (0..marks.len()).filter(|&at| marks[at]).collect()
    }

    /// gcc copies `dput` onto the path from block 3, where `ret` is known not to be zero, and that
    /// copy falls into the `printk` every time it runs, so the `printk` is not cold. Neither is
    /// block 8, which was cold only for going to the `printk` and which gcc does not have, since
    /// the -12 it carries rides on the edge there.
    #[test]
    fn a_cold_call_a_thread_would_put_a_call_in_front_of_stays() {
        assert_eq!(cold("block5"), Vec::<usize>::new());
    }

    /// The same with the edge into the `printk` split, as it is by the time selection reads it.
    #[test]
    fn a_block_that_only_jumps_on_to_the_cold_call_is_looked_through() {
        assert_eq!(cold("block9"), Vec::<usize>::new());
    }

    /// The same with `ret` handed to the `printk` block, as it is when the function returns it.
    #[test]
    fn a_way_in_that_hands_the_cold_call_a_value_is_the_same() {
        assert_eq!(cold("block10(%4)"), Vec::<usize>::new());
    }
}
