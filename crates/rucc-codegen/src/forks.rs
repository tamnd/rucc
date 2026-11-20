//! A select on a machine with no conditional move, as the branch it was before the optimizer
//! turned it into one.
//!
//! Design: `spec/10-backend.md` section 10.5, for the same reason `crate::retry` is a pass rather
//! than a rule: what replaces the instruction is blocks, and a rule has no way to say that.
//!
//! # Which machines
//!
//! `cmov` came with the Pentium Pro. An i386 kernel built with `-march=i486`, `-march=i586`,
//! `-march=k6` or one of the other processors `arch/x86/Makefile_32.cpu` names that came before it
//! or beside it runs on a machine where the instruction is an invalid opcode, and gcc writes a
//! branch for every one of them. So does this, for the list `rucc_target::Isa` keeps, and leaves
//! every other machine alone.
//!
//! # The shape
//!
//! For `r = select c, t, f` in a block, with `tail` for whatever followed it:
//!
//! ```text
//! head:                ; what was in front of the instruction
//!   br_if c, yes(), done(f)
//! yes:
//!   jump done(t)
//! done(r):             ; `tail`
//! ```
//!
//! The edge from `head` to `done` is critical, and `split::critical` below takes care of it as it
//! does for the loop `crate::retry` builds. It runs last in the group, because the splitting of a
//! `long long` writes selects of its own and every one of those needs the same treatment.
//!
//! # Selects on one condition
//!
//! Selects that test the same condition in one block are one branch, with a parameter of `done`
//! for each. The split of a `long long` shift by a register writes two of them, one for each half,
//! both on whether the count is 32 or more. As two branches, the condition is kept as a byte with
//! `setne` across the first and tested again with `testb` for the second, and each arm of each
//! branch has its own block. gcc writes one `testb $32, %cl` and one `je` over two moves. In
//! FreeBSD's `boot2`, which has to fit in 7680 bytes, `fsread` has four of these shifts.
//!
//! A later select joins the first one only where it can move up to it: no operand of it is written
//! between the two, by an instruction or by a select of the group.

use rucc_base::hash::Set;
use rucc_ir::{Block, Builder, Func, Inst, Opcode, Value};
use rucc_target::CallRegs;

/// Every select in the function, as a branch, with the selects on one condition as one branch.
pub fn branches(func: &mut Func) {
    let found: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::Select)
        .collect();
    let mut forked: Set<Inst> = Set::default();
    for inst in found {
        if forked.contains(&inst) {
            continue;
        }
        let group = alike(func, inst);
        forked.extend(group.iter().copied());
        fork(func, &group);
    }
}

/// `inst` and each select after it in its block that tests the same condition and can move up to
/// it, in their order.
fn alike(func: &Func, inst: Inst) -> Vec<Inst> {
    let Some(head) = func.block_of(inst) else { return vec![inst] };
    let Some(&cond) = func[func[inst].args].first() else { return vec![inst] };
    let mut group = vec![inst];
    // The values written from `inst` on. A select that reads one of them stays where it is.
    let mut written: Set<Value> = func[inst].results().collect();
    for at in func.insts(head).skip_while(|&it| it != inst).skip(1) {
        let args = &func[func[at].args];
        if func[at].opcode == Opcode::Select
            && args.first() == Some(&cond)
            && !args.iter().any(|arg| written.contains(arg))
        {
            group.push(at);
        }
        written.extend(func[at].results());
    }
    group
}

/// Every select of a value on the x87 stack, as a branch, on a machine that has a conditional move.
///
/// A conditional move picks between two registers, and a value on the x87 stack is in a frame slot
/// rather than in a register, so there is nothing for it to pick between. gcc writes `fcmov` for
/// these on a machine that has it. This writes the branch it writes on a machine that does not,
/// which is the same answer one jump slower.
pub fn stacked(func: &mut Func, conv: &CallRegs) {
    let found: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::Select)
        .filter(|&inst| {
            func[inst].first_result.is_some_and(|it| crate::abi::on_the_stack(func[it].ty, conv))
        })
        .collect();
    for inst in found {
        fork(func, &[inst]);
    }
}

/// Selects on one condition in one block, the first of them where the branch goes, as the three
/// blocks above with a parameter of `done` for each.
fn fork(func: &mut Func, group: &[Inst]) {
    let Some(&inst) = group.first() else { return };
    let head = func.block_of(inst).expect("the instruction is in a block");
    let span = func.span(inst);
    let mut cond = None;
    let (mut thens, mut others, mut results) = (Vec::new(), Vec::new(), Vec::new());
    for &member in group {
        let [test, then, other] = func[func[member].args] else { return };
        let Some(result) = func[member].first_result else { return };
        cond = Some(test);
        thens.push(then);
        others.push(other);
        results.push(result);
    }
    let Some(cond) = cond else { return };

    // Collected before anything moves, for the reason `crate::retry` gives.
    let tail: Vec<Inst> = func
        .insts(head)
        .skip_while(|&at| at != inst)
        .skip(1)
        .filter(|at| !group.contains(at))
        .collect();

    let yes = func.create_block();
    let done = func.create_block();
    let chosen: Vec<Value> =
        results.iter().map(|&result| func.append_param(done, func[result].ty)).collect();

    for &member in group {
        func.remove_inst(member);
    }
    for at in tail {
        func.remove_inst(at);
        func.append_inst(done, at);
    }

    let mut build = Builder::new(func, head).at(span);
    build.br_if(cond, yes, &[], done, &others);
    let mut build = Builder::new(func, yes).at(span);
    build.jump(done, &thens);

    for (result, chosen) in results.into_iter().zip(chosen) {
        replace(func, result, chosen);
    }
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

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Flags, Func, IntPred, Module, Opcode, Signature, Type};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    /// `int f(int a, int b) { return (a < b ? a : b) + 1; }`, which leaves one block holding the
    /// select and an add after it that reads what it chose.
    #[test]
    fn a_select_becomes_a_branch_and_a_block_parameter() {
        let mut names = Interner::new();
        let i32 = Type::int(32);
        let signature = Signature::new().with_params(&[i32, i32]).with_returns(&[i32]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let a = func.append_param(entry, i32);
        let b = func.append_param(entry, i32);
        let mut build = Builder::new(&mut func, entry);
        let less = build.icmp(IntPred::Slt, a, b);
        let min = build.select(less, a, b);
        let one = build.iconst(i32, 1);
        let more = build.binary(Opcode::Add, min, one, Flags::NONE);
        build.ret(&[more]);

        super::branches(&mut func);

        let opcodes: Vec<Opcode> = func
            .blocks()
            .flat_map(|block| func.insts(block).map(|inst| func[inst].opcode))
            .collect();
        assert!(!opcodes.contains(&Opcode::Select), "{opcodes:?}");
        assert_eq!(func.blocks().count(), 3);
        let done = func.blocks().last().expect("three blocks");
        let shape: Vec<Opcode> = func.insts(done).map(|inst| func[inst].opcode).collect();
        assert_eq!(shape, [Opcode::IConst, Opcode::Add, Opcode::Return]);
        let add = func.insts(done).nth(1).expect("the add moved");
        assert_ne!(func[func[add].args][0], min, "the add reads the parameter");
        let target = TargetInfo::new(Triple::new(Arch::X86, Os::Linux, Env::Gnu));
        let module = Module::new(names.intern("f.c"), &target);
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    /// The two halves of `x << n` on a `long long`, each a select on whether `n` is 32 or more,
    /// and a third select that reads the first and so cannot move up to it.
    #[test]
    fn selects_on_one_condition_are_one_branch() {
        let mut names = Interner::new();
        let i32 = Type::int(32);
        let signature = Signature::new().with_params(&[i32, i32, i32]).with_returns(&[i32]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let low = func.append_param(entry, i32);
        let high = func.append_param(entry, i32);
        let count = func.append_param(entry, i32);
        let mut build = Builder::new(&mut func, entry);
        let bit = build.iconst(i32, 32);
        let masked = build.binary(Opcode::And, count, bit, Flags::NONE);
        let zero = build.iconst(i32, 0);
        let whole = build.icmp(IntPred::Ne, masked, zero);
        let first = build.select(whole, zero, low);
        let second = build.select(whole, low, high);
        let third = build.select(whole, first, high);
        let sum = build.binary(Opcode::Add, second, third, Flags::NONE);
        build.ret(&[sum]);

        super::branches(&mut func);

        let mut branches = 0;
        let mut params = Vec::new();
        for block in func.blocks() {
            params.push(func[block].params.len());
            branches += func.insts(block).filter(|&inst| func[inst].opcode == Opcode::BrIf).count();
        }
        assert_eq!(branches, 2, "one branch for the first two, one for the third: {params:?}");
        assert!(params.contains(&2), "the first two are two parameters of one block: {params:?}");
        let target = TargetInfo::new(Triple::new(Arch::X86, Os::Linux, Env::Gnu));
        let module = Module::new(names.intern("f.c"), &target);
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }
}
