//! Letting a select on a comparison read what the comparison left in the condition state.
//!
//! Design: `spec/10-backend.md` section 10.6, and `spec/optimizer/37-machine-level-optimization.md`
//! section 37.4.
//!
//! A rule selects `select c, t, f` as a test of the byte `c` and a conditional move on the answer,
//! because the byte is the only thing a rule can name. When the byte came from a comparison that is
//! three instructions where the machine wanted one. The comparison sets the condition state, the
//! byte is written from it, the test of the byte sets the condition state again, and only then
//! does the move read it:
//!
//! ```text
//!   cmpl %esi, %edi
//!   setg %al                  cmpl %esi, %edi
//!   testb %al, %al      ->    cmovgl %edx, %ecx
//!   cmovnel %edx, %ecx
//! ```
//!
//! This is the branch [`crate::layout`] folds, written as a select, and it is done the same way for
//! the same reasons. [`fusable`] asks before allocation which comparisons have a byte that selects
//! in the same block are the whole of what reads, because that is a question about a register that
//! is written once. [`moves`] runs after the layout, where nothing is left that could put an
//! instruction between the comparison and a move reading what it left, and rewrites the comparison
//! and all of its selects together or none of them.
//!
//! What it is worth is what phiopt makes. A loop keeping the largest of each of eight slots, which
//! is `if (v > best[k]) best[k] = v` and becomes a select once the store is made on both paths,
//! runs two instructions fewer on every element, and those are two of the four on the path from
//! the load to the store.
//!
//! # What stops it
//!
//! Anything between the comparison and a select that writes the condition state, which the target
//! says of every name it does not know. And anything that writes the register the byte was given,
//! since a select reading that register afterwards is reading something else. Either one ends the
//! walk, and a select the walk did not reach keeps the byte, so the comparison keeps it too and
//! every select behind it stays as it was.
//!
//! A comparison of floats is not in the table and is left alone. What it leaves in the condition
//! state is two answers, one for whether the operands were ordered at all, and a move can read only
//! one of them.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_mir::{self as mir, Role};
use rucc_target::{BranchInsts, FlagInsts, Fusion, MachineInsts};

use crate::changes::{self, Changes, Plan};

/// The comparisons whose byte only selects in the same block read, each with how many do.
///
/// Run before allocation, on the same function [`moves`] is later given, for the reason
/// [`crate::layout::fusable`] is: whether anything else reads a register is a question about a
/// virtual one, and after allocation a register is written many times.
///
/// The byte has to be what a select reads as its condition and nothing else. A select that also
/// chose the byte as one of its values would want the byte kept, and so the count of reads has to
/// come out the same as the count of selects that read it as a condition.
#[must_use]
pub fn fusable(
    func: &mir::Func,
    insts: &BranchInsts,
    names: &mut Interner,
) -> HashMap<mir::Inst, usize> {
    let compares = compares(insts, names);
    let selects = selects(insts, names);
    let reads = changes::Reads::of(func);
    let mut found = HashMap::new();
    for block in func.blocks() {
        let mut waiting: HashMap<mir::Reg, (mir::Inst, usize)> = HashMap::new();
        for inst in func.insts(block) {
            let data = &func[inst];
            let operands = &func[data.operands];
            if selects.contains(&data.opcode) {
                let condition = operands.get(3).map(|operand| operand.reg);
                if let Some(entry) = condition.and_then(|reg| waiting.get_mut(&reg)) {
                    entry.1 += 1;
                }
            }
            if compares.contains_key(&data.opcode) {
                let byte = operands.first().filter(|operand| operand.role != Role::Use);
                if let Some(byte) = byte.filter(|byte| byte.reg.is_virtual()) {
                    waiting.insert(byte.reg, (inst, 0));
                }
            }
        }
        for (reg, (compare, count)) in waiting {
            if count > 0 && reads.count(reg) == count {
                found.insert(compare, count);
            }
        }
    }
    found
}

/// Turns every comparison [`fusable`] found, and the selects reading its byte, into the
/// comparison keeping nothing and the moves on its condition.
///
/// Gives back how many comparisons it did that for, which the tests read and nothing else does.
pub fn moves(
    func: &mut mir::Func,
    insts: &BranchInsts,
    flags: &FlagInsts,
    machine: &MachineInsts,
    names: &mut Interner,
    fusable: &HashMap<mir::Inst, usize>,
) -> usize {
    if fusable.is_empty() {
        return 0;
    }
    // Every name the rewrite could want, before the walk rather than inside it, for the reason the
    // compare pass gives: the walk reads names out of the interner while it edits the function.
    let compares = compares(insts, names);
    let kept: HashMap<&str, mir::Opcode> =
        insts.fused.iter().map(|fusion| (fusion.cmp, opcode(insts, names, fusion.cmp))).collect();
    let chosen: HashMap<(mir::Opcode, &str), mir::Opcode> = insts
        .moves
        .iter()
        .map(|entry| {
            let select = opcode(insts, names, entry.select);
            ((select, entry.when), opcode(insts, names, entry.cmov))
        })
        .collect();
    let names = &*names;
    let mut counts = changes::Reads::of(func);
    let mut made = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        let sequence: Vec<mir::Inst> = func.insts(block).collect();
        for (at, &compare) in sequence.iter().enumerate() {
            let Some(&wanted) = fusable.get(&compare) else { continue };
            let Some(&fusion) = compares.get(&func[compare].opcode) else { continue };
            let Some(&byte) = func[func[compare].operands].first() else { continue };
            let found = reached(func, flags, names, &chosen, fusion, byte, &sequence[at + 1..]);
            if found.len() != wanted {
                continue;
            }
            let Some(&cmp) = kept.get(fusion.cmp) else { continue };
            let mut set = Changes::new();
            set.rewrite(compare, flags_only(func, compare, cmp));
            for (select, cmov) in found {
                let mut plan = Plan::of(func, select);
                plan.operands.truncate(3);
                set.rewrite(select, Plan { opcode: cmov, ..plan });
            }
            if set.commit(func, &mut counts, names, machine).is_ok() {
                made += 1;
            }
        }
    }
    made
}

/// The selects on the byte a comparison wrote that the condition state it left still reaches, in
/// order, each with the move it becomes.
///
/// The walk ends at the first instruction that writes the condition state or the byte's register,
/// and a select on the byte is not the first of those even though its test is, because the test is
/// the half that goes.
fn reached(
    func: &mir::Func,
    flags: &FlagInsts,
    names: &Interner,
    chosen: &HashMap<(mir::Opcode, &str), mir::Opcode>,
    fusion: &Fusion,
    byte: mir::Operand,
    after: &[mir::Inst],
) -> Vec<(mir::Inst, mir::Opcode)> {
    let place = (byte.class, byte.reg);
    let mut found = Vec::new();
    for &inst in after {
        let data = &func[inst];
        let operands = &func[data.operands];
        let writes = operands
            .iter()
            .any(|operand| operand.role != Role::Use && (operand.class, operand.reg) == place);
        if let Some(&cmov) = chosen.get(&(data.opcode, fusion.if_true)) {
            let [_, false_arm, true_arm, condition] = operands else { break };
            let arms = [false_arm, true_arm];
            if (condition.class, condition.reg) == place
                && arms.iter().all(|arm| (arm.class, arm.reg) != place)
            {
                found.push((inst, cmov));
                if writes {
                    break;
                }
                continue;
            }
        }
        let Some(name) = names.resolve(data.opcode.name()).strip_prefix(flags.prefix) else {
            break;
        };
        if (flags.writes)(name) || writes {
            break;
        }
    }
    found
}

/// The comparison with the byte at the front taken off, which is the one that keeps nothing.
///
/// An addressing mode names its base and its index by where they are among the operands, and every
/// operand comes down one place, so the two positions come down with them.
fn flags_only(func: &mir::Func, compare: mir::Inst, cmp: mir::Opcode) -> Plan {
    let mut plan = Plan::of(func, compare);
    plan.operands.remove(0);
    plan.amode = plan.amode.map(|mut amode| {
        amode.base = amode.base.map(|position| position - 1);
        amode.index = amode.index.map(|position| position - 1);
        amode
    });
    Plan { opcode: cmp, ..plan }
}

/// The comparisons that keep a byte, by opcode, each with its entry in the branch table.
fn compares(insts: &BranchInsts, names: &mut Interner) -> HashMap<mir::Opcode, &'static Fusion> {
    insts.fused.iter().map(|fusion| (opcode(insts, names, fusion.set), fusion)).collect()
}

/// The selects that test a byte, by opcode.
fn selects(insts: &BranchInsts, names: &mut Interner) -> Vec<mir::Opcode> {
    let mut found: Vec<mir::Opcode> =
        insts.moves.iter().map(|entry| opcode(insts, names, entry.select)).collect();
    found.dedup();
    found
}

/// The opcode of that name on this target.
fn opcode(insts: &BranchInsts, names: &mut Interner, name: &str) -> mir::Opcode {
    mir::Opcode::new(names.intern(&format!("{}{name}", insts.prefix)))
}

#[cfg(test)]
mod tests {
    use rucc_target::x86_64::{BRANCH, FLAGS, GPR, MACHINE};

    use super::*;

    /// A function with one block, and the names it was built with.
    fn empty() -> (Interner, mir::Func, mir::Block) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> mir::Opcode {
        opcode(&BRANCH, names, name)
    }

    /// Both halves of the pass, the way the pipeline runs them, with nothing in between.
    fn fuse(func: &mut mir::Func, names: &mut Interner) -> usize {
        let found = fusable(func, &BRANCH, names);
        moves(func, &BRANCH, &FLAGS, &MACHINE, names, &found)
    }

    /// What every instruction in a block came to, as opcodes with the target's prefix taken off.
    fn shape(func: &mir::Func, names: &Interner, block: mir::Block) -> Vec<String> {
        func.insts(block)
            .map(|inst| {
                let name = names.resolve(func[inst].opcode.name());
                name.strip_prefix(BRANCH.prefix).unwrap_or("").to_owned()
            })
            .collect()
    }

    /// What a select writes, which is the register its false arm is in, the way the rule selects
    /// it and the allocator leaves it.
    fn tied(f: mir::Reg) -> mir::Operand {
        mir::Operand::write(f, GPR).with(rucc_mir::Constraint::Reuse(1))
    }

    /// A comparison of two registers keeping a byte, and a select on the byte.
    fn compare_and_select(
        func: &mut mir::Func,
        names: &mut Interner,
        block: mir::Block,
        condition: &str,
    ) -> [mir::Reg; 2] {
        let [x, y, byte, t, f] = [(); 5].map(|()| func.new_vreg(GPR));
        let cmp = op(names, &format!("cmp_set_{condition}_32"));
        let select = op(names, "test_cmov_ne_32");
        func.build(block, cmp).def(byte, GPR).uses(x, GPR).uses(y, GPR).finish();
        func.build(block, select)
            .operand(tied(f))
            .uses(f, GPR)
            .uses(t, GPR)
            .uses(byte, GPR)
            .finish();
        [byte, t]
    }

    /// The shape this is for. The comparison keeps nothing, the select becomes the move on the
    /// comparison's own condition, and its operands are the three it had without the byte.
    #[test]
    fn a_select_on_a_comparison_becomes_a_move_on_its_condition() {
        for condition in ["e", "l", "ge", "b", "a"] {
            let (mut names, mut func, block) = empty();
            compare_and_select(&mut func, &mut names, block, condition);

            assert_eq!(fuse(&mut func, &mut names), 1, "{condition}");
            let expected = ["cmp_rr_32".to_owned(), format!("cmov_{condition}_32")];
            assert_eq!(shape(&func, &names, block), expected);
            let last = func.insts(block).last().expect("the move");
            assert_eq!(func[func[last].operands].len(), 3);
        }
    }

    /// Two selects on one comparison, which is what `min` and `max` of the same pair come to. Both
    /// become moves, and the comparison keeps nothing because nothing is left to read the byte.
    #[test]
    fn two_selects_on_one_comparison_both_become_moves() {
        let (mut names, mut func, block) = empty();
        let [byte, _] = compare_and_select(&mut func, &mut names, block, "g");
        let [t, f] = [(); 2].map(|()| func.new_vreg(GPR));
        let select = op(&mut names, "test_cmov_ne_64");
        func.build(block, select)
            .operand(tied(f))
            .uses(f, GPR)
            .uses(t, GPR)
            .uses(byte, GPR)
            .finish();

        assert_eq!(fuse(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["cmp_rr_32", "cmov_g_32", "cmov_g_64"]);
    }

    /// A move between the two leaves the condition state alone, which is what the allocator puts
    /// there when the value the move overwrites is still wanted afterwards.
    #[test]
    fn a_copy_between_the_comparison_and_the_select_does_not_stop_it() {
        let (mut names, mut func, block) = empty();
        let [x, y, byte, t, f, spare] = [(); 6].map(|()| func.new_vreg(GPR));
        let cmp = op(&mut names, "cmp_set_l_32");
        let copy = op(&mut names, "mov_rr_64");
        let select = op(&mut names, "test_cmov_ne_32");
        func.build(block, cmp).def(byte, GPR).uses(x, GPR).uses(y, GPR).finish();
        func.build(block, copy).def(spare, GPR).uses(f, GPR).finish();
        func.build(block, select)
            .operand(tied(f))
            .uses(f, GPR)
            .uses(t, GPR)
            .uses(byte, GPR)
            .finish();

        assert_eq!(fuse(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["cmp_rr_32", "mov_rr_64", "cmov_l_32"]);
    }

    /// Arithmetic between the two writes the condition state, so what the select would read is
    /// what the arithmetic left and the test of the byte has to stay.
    #[test]
    fn arithmetic_between_the_comparison_and_the_select_keeps_the_test() {
        let (mut names, mut func, block) = empty();
        let [x, y, byte, t, f] = [(); 5].map(|()| func.new_vreg(GPR));
        let cmp = op(&mut names, "cmp_set_l_32");
        let add = op(&mut names, "add_rr_32");
        let select = op(&mut names, "test_cmov_ne_32");
        func.build(block, cmp).def(byte, GPR).uses(x, GPR).uses(y, GPR).finish();
        func.build(block, add).def(t, GPR).uses(t, GPR).uses(x, GPR).finish();
        func.build(block, select)
            .operand(tied(f))
            .uses(f, GPR)
            .uses(t, GPR)
            .uses(byte, GPR)
            .finish();

        assert_eq!(fuse(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["cmp_set_l_32", "add_rr_32", "test_cmov_ne_32"]);
    }

    /// A byte something other than a select reads, here a store of it, has to be written, so the
    /// comparison keeps it and the select keeps its test.
    #[test]
    fn a_byte_something_else_reads_is_kept_and_so_is_the_test() {
        let (mut names, mut func, block) = empty();
        let [byte, _] = compare_and_select(&mut func, &mut names, block, "e");
        let spare = func.new_vreg(GPR);
        let copy = op(&mut names, "mov_rr_64");
        func.build(block, copy).def(spare, GPR).uses(byte, GPR).finish();

        assert_eq!(fuse(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["cmp_set_e_32", "test_cmov_ne_32", "mov_rr_64"]);
    }

    /// A select that chooses the byte as one of its values wants the byte itself and not only what
    /// it said, so nothing changes.
    #[test]
    fn a_select_that_chooses_the_byte_itself_keeps_the_test() {
        let (mut names, mut func, block) = empty();
        let [x, y, byte, f] = [(); 4].map(|()| func.new_vreg(GPR));
        let cmp = op(&mut names, "cmp_set_ne_32");
        let select = op(&mut names, "test_cmov_ne_32");
        func.build(block, cmp).def(byte, GPR).uses(x, GPR).uses(y, GPR).finish();
        func.build(block, select)
            .operand(tied(f))
            .uses(f, GPR)
            .uses(byte, GPR)
            .uses(byte, GPR)
            .finish();

        assert_eq!(fuse(&mut func, &mut names), 0);
    }

    /// The byte's register written again between the comparison and the select, which after
    /// allocation is a reload into the same register. The select reads what the reload wrote,
    /// which the pass cannot tell is the same answer, so it leaves both alone.
    #[test]
    fn a_register_written_again_before_the_select_keeps_the_test() {
        let (mut names, mut func, block) = empty();
        let byte = mir::Reg::physical(rucc_target::x86_64::RAX);
        let [x, y, t, f, other] = [(); 5].map(|()| func.new_vreg(GPR));
        let cmp = op(&mut names, "cmp_set_l_32");
        let copy = op(&mut names, "mov_rr_64");
        let select = op(&mut names, "test_cmov_ne_32");
        func.build(block, cmp).def(byte, GPR).uses(x, GPR).uses(y, GPR).finish();
        func.build(block, copy).def(byte, GPR).uses(other, GPR).finish();
        func.build(block, select)
            .operand(tied(f))
            .uses(f, GPR)
            .uses(t, GPR)
            .uses(byte, GPR)
            .finish();
        let found = HashMap::from([(func.insts(block).next().expect("the comparison"), 1)]);

        assert_eq!(moves(&mut func, &BRANCH, &FLAGS, &MACHINE, &mut names, &found), 0);
        assert_eq!(shape(&func, &names, block), ["cmp_set_l_32", "mov_rr_64", "test_cmov_ne_32"]);
    }

    /// A comparison against memory keeps its address once the byte in front of the operands is
    /// gone, which means the positions the address names come down by one.
    #[test]
    fn a_comparison_against_memory_keeps_its_address() {
        let (mut names, mut func, block) = empty();
        let [x, base, byte, t, f] = [(); 5].map(|()| func.new_vreg(GPR));
        let cmp = op(&mut names, "cmp_set_l_rm_32");
        let select = op(&mut names, "test_cmov_ne_32");
        let place = mir::Mem { disp: 8, ..mir::Mem::at(mir::Operand::read(base, GPR)) };
        let compare = func.build(block, cmp).def(byte, GPR).uses(x, GPR).mem(place).finish();
        func.build(block, select)
            .operand(tied(f))
            .uses(f, GPR)
            .uses(t, GPR)
            .uses(byte, GPR)
            .finish();

        assert_eq!(fuse(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["cmp_rm_32", "cmov_l_32"]);
        let mode = func[func[compare].mem.expect("the address")];
        assert_eq!((mode.base, mode.disp), (Some(1), 8));
    }
}
