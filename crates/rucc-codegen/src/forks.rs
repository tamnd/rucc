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

use rucc_ir::{Block, Builder, Func, Inst, Opcode, Value};

/// Every select in the function, as a branch.
pub fn branches(func: &mut Func) {
    let found: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::Select)
        .collect();
    for inst in found {
        fork(func, inst);
    }
}

/// One select, as the three blocks above.
fn fork(func: &mut Func, inst: Inst) {
    let head = func.block_of(inst).expect("the instruction is in a block");
    let span = func.span(inst);
    let [cond, then, other] = func[func[inst].args] else { return };
    let Some(result) = func[inst].first_result else { return };
    let ty = func[result].ty;

    // Collected before anything moves, for the reason `crate::retry` gives.
    let tail: Vec<Inst> = func.insts(head).skip_while(|&at| at != inst).skip(1).collect();

    let yes = func.create_block();
    let done = func.create_block();
    let chosen = func.append_param(done, ty);

    func.remove_inst(inst);
    for at in tail {
        func.remove_inst(at);
        func.append_inst(done, at);
    }

    let mut build = Builder::new(func, head).at(span);
    build.br_if(cond, yes, &[], done, &[other]);
    let mut build = Builder::new(func, yes).at(span);
    build.jump(done, &[then]);

    replace(func, result, chosen);
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
}
