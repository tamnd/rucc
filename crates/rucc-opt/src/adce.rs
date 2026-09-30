//! Aggressive dead code elimination: what nothing necessary depends on goes, branches included.
//!
//! [`crate::dce`] starts from the instructions nothing uses and works backwards, so it can only
//! remove something once every reader is gone, and a branch is never gone because a branch is a
//! terminator. This pass starts from the other end, which is section 17.2 of
//! `spec/optimizer/17-dce-and-dse.md`. It assumes everything is dead, marks what the program can be
//! seen doing, and keeps only what that needs. A branch is needed when a block that does something
//! is control dependent on it, and a branch that is not is sent straight to the block where its
//! arms meet, which is its immediate post dominator.
//!
//! The case that wants it is a test left behind by a loop that went. `loop-delete` takes the loop
//! out and the `if (n > 0)` in front of it stays, a compare and a jump around an empty block, and
//! `dce` cannot see that nothing depends on the jump.
//!
//! # What is necessary
//!
//! An instruction [`crate::dce::removable`] says no to, which is a store, a call that is not pure,
//! a volatile or atomic access and inline assembly. A terminator that leaves the function, which is
//! a return, a tail call, `unreachable` and every branch this pass does not know how to move. And
//! the terminator of every block with an edge back to a block no later in reverse postorder, which
//! is every loop latch and at least one edge of every cycle, reducible or not. Section 17.2 is
//! explicit that rucc does not delete a loop that may not terminate, and a loop whose latch is
//! necessary is a loop whose exit test is necessary too, since the latch is control dependent on
//! it.
//!
//! Then the operands of anything necessary are necessary, a branch is necessary when a block with
//! something necessary in it is control dependent on it, and a block parameter that is necessary
//! makes the argument each predecessor passes it necessary, and the branch that passes it.
//!
//! # What goes
//!
//! Every instruction not marked, every block parameter not marked, and every branch not marked,
//! which becomes a jump to where its arms meet. The blocks that jump strands are swept. A branch
//! whose arms meet only at the exit, or meet at a block with a parameter something needs, cannot
//! move and is necessary after all, which can make more necessary, so the marking runs again until
//! nothing changes.

use rucc_base::hash::Set;
use rucc_ir::{Block, BlockCall, Builder, Def, Func, Inst, Opcode, Value};

use crate::cfg::Cfg;
use crate::dce::removable;
use crate::frontier::ControlDependence;
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "adce";

/// Recorded for each instruction taken out.
const REMOVED: &str = "instruction nothing necessary reads removed";

/// Recorded for each branch turned into a jump.
const BRANCH: &str = "branch nothing necessary depends on sent to where its arms meet";

/// Recorded for each block parameter taken out, with the argument every branch passed it.
const PARAM: &str = "block parameter nothing necessary reads removed";

/// Recorded for an instruction or a branch that would have gone if there had been fuel for it.
const NO_FUEL: &str = "dead instruction or branch kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adce;

impl Pass for Adce {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "whatever nothing necessary depends on is removed, a branch included"
    }

    fn preserves(&self) -> Preserved {
        // Branches become jumps and the blocks that strands go.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(entry) = func.entry() else { return stats };
        let (params, branches, insts) = {
            let cfg = an.cfg(func);
            // A block control never reaches has no post dominator to ask about, and the verifier
            // does not allow one anyway. A block an image names is the one that can be here.
            if func.blocks().any(|block| !cfg.reaches(block)) {
                return stats;
            }
            let post = an.post_dominators(func);
            let mut mark = Mark::new(func, cfg, an.control_dependence(func));
            for block in func.blocks() {
                for inst in func.insts(block) {
                    if necessary(func, cfg, block, inst, an) {
                        mark.inst(inst);
                    }
                }
                for &param in &func[block].params {
                    if block == entry || func[param].ty.is_mem() {
                        mark.value(param);
                    }
                }
            }
            let mut paid: Set<Inst> = Set::default();
            loop {
                mark.run();
                let mut again = false;
                for block in func.blocks() {
                    for inst in func.insts(block) {
                        if mark.insts.contains(&inst) {
                            continue;
                        }
                        if func.is_terminator(inst) {
                            if func[inst].opcode == Opcode::Jump {
                                continue;
                            }
                            let meet = post.immediate_post_dominator(block);
                            let stuck = meet.is_none_or(|meet| {
                                func[meet].params.iter().any(|param| mark.params.contains(param))
                            });
                            if stuck {
                                mark.inst(inst);
                                again = true;
                                continue;
                            }
                        }
                        if paid.contains(&inst) {
                            continue;
                        }
                        if fuel.take() {
                            paid.insert(inst);
                        } else {
                            // Out of fuel keeps the instruction, and keeping it keeps what it reads,
                            // so the marking has to run again before anything is taken out.
                            stats.missed(NO_FUEL);
                            mark.inst(inst);
                            again = true;
                        }
                    }
                }
                if !again {
                    break;
                }
            }
            let params: Vec<(Block, Vec<usize>)> = func
                .blocks()
                .filter(|&block| block != entry)
                .filter_map(|block| {
                    let all = &func[block].params;
                    let keep: Vec<usize> =
                        (0..all.len()).filter(|&at| mark.params.contains(&all[at])).collect();
                    (keep.len() != all.len()).then_some((block, keep))
                })
                .collect();
            let mut branches: Vec<(Block, Block)> = Vec::new();
            let mut insts: Vec<Inst> = Vec::new();
            for block in func.blocks() {
                for inst in func.insts(block) {
                    if mark.insts.contains(&inst) || !paid.contains(&inst) {
                        continue;
                    }
                    if func.is_terminator(inst) {
                        let meet = post.immediate_post_dominator(block);
                        branches
                            .push((block, meet.expect("a branch that moves has somewhere to go")));
                    } else {
                        insts.push(inst);
                    }
                }
            }
            (params, branches, insts)
        };
        if params.is_empty() && branches.is_empty() && insts.is_empty() {
            return stats;
        }
        for (block, keep) in &params {
            for _ in keep.len()..func[*block].params.len() {
                stats.optimized(PARAM);
            }
        }
        drop_params(func, &params);
        for &(block, meet) in &branches {
            let term = func.terminator(block).expect("a block ends in its branch");
            func.remove_inst(term);
            Builder::new(func, block).jump(meet, &[]);
            stats.optimized(BRANCH);
        }
        for inst in insts {
            func.remove_inst(inst);
            stats.optimized(REMOVED);
        }
        an.clear();
        crate::simplify_cfg::sweep(func, an, &mut stats);
        an.clear();
        stats
    }
}

/// Whether this instruction is necessary before anything else is known.
fn necessary(func: &Func, cfg: &Cfg, block: Block, inst: Inst, an: &Analyses) -> bool {
    if !func.is_terminator(inst) {
        return !removable(func, inst, an.purity());
    }
    let movable = matches!(func[inst].opcode, Opcode::Jump | Opcode::BrIf | Opcode::Switch);
    let back = cfg.successors(block).iter().any(|&succ| cfg.rank(succ) <= cfg.rank(block));
    !movable || back
}

/// Takes out the parameters that go and the argument in the same place on every branch to them.
///
/// The same as [`crate::memssa`] does for the memory parameters, and for the same reason the places
/// are worked out before anything is renumbered.
fn drop_params(func: &mut Func, params: &[(Block, Vec<usize>)]) {
    if params.is_empty() {
        return;
    }
    for block in func.blocks().collect::<Vec<Block>>() {
        let Some(term) = func.terminator(block) else { continue };
        for target in func.target_list(term).iter() {
            let call = func[target];
            let Some((_, keep)) = params.iter().find(|(at, _)| *at == call.block) else {
                continue;
            };
            let args: Vec<Value> = keep.iter().map(|&at| func[call.args][at]).collect();
            let args = func.push_values(&args);
            func.set_block_call(target, BlockCall { args, ..call });
        }
    }
    for (block, keep) in params {
        let kept: Vec<Value> = keep.iter().map(|&at| func[*block].params[at]).collect();
        func.retain_params(*block, |param| kept.contains(&param));
    }
}

/// What is necessary so far, and what has been marked and not yet followed.
struct Mark<'a> {
    func: &'a Func,
    cfg: &'a Cfg,
    control: &'a ControlDependence,
    insts: Set<Inst>,
    params: Set<Value>,
    work: Vec<Inst>,
    arrived: Vec<Value>,
}

impl<'a> Mark<'a> {
    fn new(func: &'a Func, cfg: &'a Cfg, control: &'a ControlDependence) -> Self {
        Self {
            func,
            cfg,
            control,
            insts: Set::default(),
            params: Set::default(),
            work: Vec::new(),
            arrived: Vec::new(),
        }
    }

    fn inst(&mut self, inst: Inst) {
        if self.insts.insert(inst) {
            self.work.push(inst);
        }
    }

    fn value(&mut self, value: Value) {
        match self.func[value].def {
            Def::Result { inst, .. } => self.inst(inst),
            Def::Param { .. } => {
                if self.params.insert(value) {
                    self.arrived.push(value);
                }
            }
        }
    }

    /// Follows everything marked until there is nothing new.
    fn run(&mut self) {
        loop {
            if let Some(inst) = self.work.pop() {
                self.through(inst);
            } else if let Some(param) = self.arrived.pop() {
                self.arrive(param);
            } else {
                break;
            }
        }
    }

    /// What a necessary instruction needs: what it reads, what it passes to a parameter that is
    /// necessary, and every branch its block is control dependent on.
    fn through(&mut self, inst: Inst) {
        let func = self.func;
        for &value in &func[func[inst].args] {
            self.value(value);
        }
        for call in func.successors(inst) {
            for (at, param) in func[call.block].params.iter().enumerate() {
                if self.params.contains(param) {
                    self.value(func[call.args][at]);
                }
            }
        }
        let block = func.block_of(inst).expect("a marked instruction is in a block");
        for &on in self.control.on(block) {
            self.inst(func.terminator(on).expect("a block ends in a terminator"));
        }
    }

    /// What a necessary parameter needs: the branch from each predecessor and what it passes.
    fn arrive(&mut self, param: Value) {
        let func = self.func;
        let Def::Param { block, index } = func[param].def else { return };
        for &pred in self.cfg.predecessors(block) {
            let term = func.terminator(pred).expect("a block ends in a terminator");
            self.inst(term);
            for call in func.successors(term) {
                if call.block == block {
                    self.value(func[call.args][index as usize]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::Adce;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it under this much fuel.
    fn run(body: &str, fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            Adce.run(&mut module[id], &mut an, fuel);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn cleaned(body: &str) -> String {
        run(body, &mut Fuel::unlimited())
    }

    const DIAMOND: &str = r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    %2 = iconst.i32 0
    %3 = icmp slt %0, %2
    br_if %3, block1, block2

block1:
    %4 = add %0, %1
    jump block3(%4)

block2:
    %5 = sub %0, %1
    jump block3(%5)

block3(%6: i32):
    return %1
}
"#;

    /// Both arms compute a value the join throws away, so the branch, both arms and the parameter
    /// they fed all go, and what is left is the return.
    #[test]
    fn a_diamond_whose_arms_compute_nothing_anyone_reads_goes() {
        let out = cleaned(DIAMOND);
        assert!(!out.contains("br_if"), "{out}");
        assert!(!out.contains("icmp"), "{out}");
        assert!(!out.contains("add") && !out.contains("sub"), "{out}");
        assert!(!out.contains(": i32):\n    return"), "the parameter went too, {out}");
        assert!(out.contains("return %1"), "{out}");
    }

    /// The same diamond with the join returning what the arms computed keeps all of it.
    #[test]
    fn a_diamond_whose_value_is_returned_stays() {
        let out = cleaned(&DIAMOND.replace("return %1", "return %6"));
        assert!(out.contains("br_if"), "{out}");
        assert!(out.contains("add") && out.contains("sub"), "{out}");
    }

    /// `if (c) *p = 1;` keeps the branch, because the store is control dependent on it.
    #[test]
    fn a_branch_a_store_is_control_dependent_on_stays() {
        let out = cleaned(
            r#"
func @f(i32, ptr), linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = iconst.i32 0
    %3 = icmp ne %0, %2
    br_if %3, block1, block2

block1:
    %4 = iconst.i32 1
    store %4 -> %1, align 4
    jump block2

block2:
    return
}
"#,
        );
        assert!(out.contains("br_if"), "{out}");
        assert!(out.contains("store"), "{out}");
    }

    /// A loop whose count nothing reads may still never end, so it stays with its exit test,
    /// which is `loop-delete`'s to argue about and not this pass's.
    #[test]
    fn a_loop_whose_value_is_unused_but_may_not_end_stays() {
        let out = cleaned(
            r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 0
    jump block1(%1)

block1(%2: i32):
    %3 = icmp slt %2, %0
    br_if %3, block2, block3

block2:
    %4 = iconst.i32 3
    %5 = add %2, %4
    jump block1(%5)

block3:
    return %0
}
"#,
        );
        assert!(out.contains("br_if"), "{out}");
        assert!(out.contains("block1(%2: i32)"), "the counter the exit test reads stays, {out}");
        assert!(out.contains("add"), "{out}");
    }

    /// A parameter only a dead instruction reads goes, with the argument every branch passed it,
    /// while the one beside it that is returned stays.
    #[test]
    fn a_block_parameter_nothing_necessary_reads_goes() {
        let out = cleaned(
            r#"
func @f(i32, i32) -> i32, linkage(external) {
block0(%0: i32, %1: i32):
    jump block1(%0, %1)

block1(%2: i32, %3: i32):
    %4 = mul %3, %3
    return %2
}
"#,
        );
        assert!(out.contains("block1(%2: i32):"), "{out}");
        assert!(out.contains("jump block1(%0)"), "{out}");
        assert!(!out.contains("mul"), "{out}");
    }

    /// Fuel stops the removing, and what the pass could not afford keeps what it reads, so the
    /// output verifies at every setting on the way from none to enough.
    #[test]
    fn fuel_stops_the_removing() {
        let none = run(DIAMOND, &mut Fuel::of(0));
        assert!(none.contains("br_if") && none.contains("add"), "{none}");
        for limit in 1..6 {
            run(DIAMOND, &mut Fuel::of(limit));
        }
        let all = run(DIAMOND, &mut Fuel::of(64));
        assert!(!all.contains("br_if"), "{all}");
    }
}
