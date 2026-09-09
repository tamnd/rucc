//! The read modify writes the machine has no single instruction for, as a loop around the compare
//! and exchange.
//!
//! Design: `spec/10-backend.md` section 10.5, which names this as the third exemption from the rule
//! that every lowering is a rule in the table.
//!
//! # What is here and why it is not a rule
//!
//! `Opcode::AtomicRmw` carries thirteen operations. x86-64 has an instruction for three of them:
//! `xchg` puts a value there, `lock xadd` adds one, and a subtraction is the same instruction over
//! the negated operand. The other ten have no instruction at any width, and what stands in for one
//! is the loop every architecture manual writes out by hand: read what is there, work out what
//! should be there instead, put it back if nothing else got in first, and go round again when
//! something did.
//!
//! That loop is blocks, and blocks are why this is a pass rather than a rule. A rule rewrites one
//! instruction into instructions; it has no way to say that control leaves a block here and arrives
//! somewhere else. So the shape is built in the IR, before anything below has been told what the
//! blocks are, and by the time the selector sees it there is nothing left but a compare and exchange
//! it already has a rule for.
//!
//! # Where it runs
//!
//! Straight after `switch::switches` and before `expand::orderings`.
//!
//! After the switches because both of these create blocks and `expand` may not: every pass in
//! `expand.rs` rewrites an instruction in the place it stands, and the two passes that change the
//! shape of the control flow are kept together at the front where the function is still the one the
//! optimizer handed over.
//!
//! Before the orderings because the head of the loop reads the address with an `atomic_load`, and it
//! is `expand::orderings` that turns that into the plain load this machine does anyway. Running the
//! other way round would leave an ordered access nothing below understands.
//!
//! # What the loop is
//!
//! For `old = atomic_rmw op, addr, operand` in a block, with `tail` for whatever followed it:
//!
//! ```text
//! head:                              ; what was in front of the instruction
//!   first = atomic_load ty, addr     ; relaxed, because the compare and exchange carries the order
//!   jump spin(first)
//! spin(seen):
//!   want = <op> seen, operand
//!   got, ok = cmpxchg addr, seen, want
//!   br_if ok, done(seen), spin(got)
//! done(before):                      ; `tail`, with every use of `old` reading `before`
//! ```
//!
//! The value the loop answers is the one that was there before, which is what `AtomicRmw` answers,
//! so the value handed to `done` is `seen` and not `want`. A name that asked for the value afterwards
//! got the arithmetic that works one out from the other back in `rucc-lower`, over the result of the
//! instruction this pass is rewriting, and that arithmetic is in `tail` and needs nothing from here.
//!
//! The failing edge carries `got` rather than going back to the load. That is the whole reason the
//! compare and exchange answers what it found: a second read would be a second chance to be wrong,
//! and the value the exchange saw is the freshest one there is.
//!
//! The edge from `spin` to itself is critical, since `spin` has two ways out and is arrived at two
//! ways. Nothing here splits it, because `split::critical` runs below and splitting it twice is
//! worse than splitting it once.
//!
//! # What it leaves alone
//!
//! An exchange, an addition and a subtraction, because those three have instructions and a loop
//! would be slower and larger for no reason. Anything whose value is not an integer the machine
//! compares and exchanges at, which is the two floating operations: a compare and exchange of a
//! float wants the value carried through an integer of the same width, an eighty bit float has no
//! such width, and until that is worked out the refusal in `lower.rs` is the honest answer.

use rucc_ir::{
    Block, Builder, Extra, Flags, Func, Inst, IntPred, MemInfo, MemOrder, Opcode, RmwOp, Type,
    Value,
};

/// Rewrites every read modify write this machine has no instruction for into a loop around the
/// compare and exchange, and leaves the rest of them alone.
///
/// The function is changed in place. It gains two blocks and loses one instruction for each one
/// rewritten, and every function without one of these is untouched.
pub fn loops(func: &mut Func) {
    let found: Vec<Inst> = func
        .blocks()
        .flat_map(|block| func.insts(block).collect::<Vec<_>>())
        .filter(|&inst| wanted(func, inst))
        .collect();
    for inst in found {
        rewrite(func, inst);
    }
}

/// Whether this instruction is one of the ones with no instruction behind it.
///
/// The three the machine has are left as they are, and so is anything whose value is not an integer
/// at a width the machine compares and exchanges at, which is what the two floating operations are.
fn wanted(func: &Func, inst: Inst) -> bool {
    let Extra::Rmw(op, _) = func[inst].extra else { return false };
    if matches!(op, RmwOp::Xchg | RmwOp::Add | RmwOp::Sub) {
        return false;
    }
    let Some(old) = func[inst].first_result else { return false };
    let ty = func[old].ty;
    ty.is_int() && matches!(ty.bits(), 8 | 16 | 32 | 64)
}

/// One read modify write, as the three blocks the loop is.
fn rewrite(func: &mut Func, inst: Inst) {
    let head = func.block_of(inst).expect("the instruction is in a block");
    let span = func.span(inst);
    let Extra::Rmw(op, mem) = func[inst].extra else { return };
    let info = func[mem];
    let flags = func[inst].flags;
    let [addr, operand] = func[func[inst].args] else { return };
    let Some(old) = func[inst].first_result else { return };
    let ty = func[old].ty;

    // Everything after the instruction, which is what moves into the block the loop leaves to.
    // Collected before anything is taken out of the block, because taking one out is what the list
    // is walked in order to do.
    let tail: Vec<Inst> = func.insts(head).skip_while(|&at| at != inst).skip(1).collect();

    let spin = func.create_block();
    let done = func.create_block();
    let seen = func.append_param(spin, ty);
    let before = func.append_param(done, ty);

    func.remove_inst(inst);
    for at in tail {
        func.remove_inst(at);
        func.append_inst(done, at);
    }

    // Relaxed, because what makes the whole of this indivisible is the compare and exchange and a
    // stronger load in front of it would be a barrier bought twice. The read is not the moment the
    // operation happens; the exchange that agrees with it is.
    let mut build = Builder::new(func, head).at(span);
    let first = build.atomic_load(ty, addr, MemInfo { order: MemOrder::Relaxed, ..info }, flags);
    build.jump(spin, &[first]);

    let mut build = Builder::new(func, spin).at(span);
    let want = compute(&mut build, op, seen, operand, ty);
    let (got, ok) = build.cmpxchg(addr, seen, want, info, flags);
    build.br_if(ok, done, &[seen], spin, &[got]);

    // Last, so that nothing written above is rewritten by it. None of it reads `old` anyway, but the
    // arguments the branch above carries are values this walk looks at, and a substitution that is
    // right only because of what happens not to be in a list is one waiting to be wrong.
    replace(func, old, before);
}

/// What the loop puts back, which is the operation over what it read and the operand.
///
/// Six shapes for eight operations. Four are one instruction. A nand is the and and then every bit
/// of the answer flipped, which is an exclusive or against every bit set because the IR has no not
/// and that is what one is. The four that take a maximum or a minimum are a comparison and a select,
/// and which comparison is the whole of the difference between the signed pair and the unsigned one.
fn compute(build: &mut Builder<'_>, op: RmwOp, seen: Value, operand: Value, ty: Type) -> Value {
    let opcode = match op {
        RmwOp::And | RmwOp::Nand => Opcode::And,
        RmwOp::Or => Opcode::Or,
        RmwOp::Xor => Opcode::Xor,
        RmwOp::SMax => return pick(build, IntPred::Sgt, seen, operand),
        RmwOp::SMin => return pick(build, IntPred::Slt, seen, operand),
        RmwOp::UMax => return pick(build, IntPred::Ugt, seen, operand),
        RmwOp::UMin => return pick(build, IntPred::Ult, seen, operand),
        _ => unreachable!("the operations with an instruction never reach this pass"),
    };
    let answer = build.binary(opcode, seen, operand, Flags::NONE);
    if op != RmwOp::Nand {
        return answer;
    }
    let ones = build.iconst(ty, -1);
    build.binary(Opcode::Xor, answer, ones, Flags::NONE)
}

/// Whichever of the two the comparison prefers, as the comparison and a select over it.
///
/// The value read comes first in the comparison and is what the select takes when it holds, so a
/// maximum of two equal values answers the one that was there. That is not observable here, since
/// the two are equal, and it is the way round every other compiler writes it.
fn pick(build: &mut Builder<'_>, pred: IntPred, seen: Value, operand: Value) -> Value {
    let wins = build.icmp(pred, seen, operand);
    build.select(wins, seen, operand)
}

/// Every use of one value made a use of another, across the whole function.
///
/// Two places hold a use: the operand list of an instruction, and the argument list of a branch's
/// edge. Both are runs of values in the same pool, so both are the same rewrite, and a walk that
/// covers the two of them covers every use there is.
///
/// The whole function rather than the part below the loop, because a use above it cannot exist. The
/// value being replaced is the result of an instruction that stood where the loop now stands, and
/// nothing that runs before an instruction reads what it produced.
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
    use rucc_ir::{
        Builder, Float, Func, MemInfo, MemOrder, Module, Opcode, Restrict, Signature, Type,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{Flags, Inst, RmwOp, Value, loops};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    fn info(bytes: u64) -> MemInfo {
        MemInfo {
            size: bytes,
            align: u32::try_from(bytes).expect("a small width"),
            order: MemOrder::SeqCst,
            tbaa: None,
            restrict: Restrict::NONE,
        }
    }

    /// `ty rmw(ty *p, ty v) { return __atomic_fetch_<op>(p, v, 5); }` as the front end builds it,
    /// which is one block with the read modify write in the middle of it.
    fn built(op: RmwOp, ty: Type) -> (Interner, Func) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, ty]).with_returns(&[ty]);
        let mut func = Func::new(names.intern("rmw"), signature);
        let entry = func.create_block();
        let addr = func.append_param(entry, Type::PTR);
        let operand = func.append_param(entry, ty);

        let bytes = u64::from(ty.bits() / 8);
        let mut build = Builder::new(&mut func, entry);
        let old = build.atomic_rmw(op, addr, operand, info(bytes), Flags::NONE);
        build.ret(&[old]);
        (names, func)
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("rmw.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    fn verified(func: &Func, names: &mut Interner) {
        let module = Module::new(names.intern("rmw.c"), &target());
        rucc_ir::verify_func(&module, func, names).expect("the rewrite builds valid IR");
    }

    fn opcodes(func: &Func) -> Vec<Opcode> {
        func.blocks().flat_map(|block| func.insts(block).map(|inst| func[inst].opcode)).collect()
    }

    fn only(func: &Func, opcode: Opcode) -> Inst {
        let found: Vec<Inst> = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == opcode)
            .collect();
        assert_eq!(found.len(), 1, "expected one {opcode:?}");
        found[0]
    }

    /// The four the machine has no instruction for become three blocks and a compare and exchange.
    ///
    /// One block for what was in front of it, one for the loop and one for what came after, and the
    /// read modify write itself is gone. The IR is checked rather than the shape being believed,
    /// because a loop built by hand is exactly the thing that gets the block arguments wrong.
    #[test]
    fn an_operation_with_no_instruction_becomes_a_loop() {
        for op in [RmwOp::And, RmwOp::Nand, RmwOp::Or, RmwOp::Xor] {
            let (mut names, mut func) = built(op, Type::int(32));
            loops(&mut func);
            verified(&func, &mut names);

            let text = printed(&func, &mut names);
            assert_eq!(func.blocks().count(), 3, "{op:?}: {text}");
            let kinds = opcodes(&func);
            assert!(kinds.contains(&Opcode::Cmpxchg), "{op:?}: {text}");
            assert!(kinds.contains(&Opcode::AtomicLoad), "{op:?}: {text}");
            assert!(!kinds.contains(&Opcode::AtomicRmw), "{op:?}: {text}");
        }
    }

    /// The value the loop answers is what was there before, which is what the instruction answered.
    ///
    /// It is the expected operand of the compare and exchange that goes to the block the loop leaves
    /// to, and not what the exchange found or what was put there. Getting that wrong is the way this
    /// shape is usually wrong, and it is invisible in the assembly until two threads run.
    #[test]
    fn the_loop_answers_the_value_that_was_there_before() {
        let (mut names, mut func) = built(RmwOp::Or, Type::int(32));
        loops(&mut func);

        let exchange = only(&func, Opcode::Cmpxchg);
        let expected = func[func[exchange].args][1];
        let branch = only(&func, Opcode::BrIf);
        let taken = func.successors(branch).next().expect("a branch has a first edge");
        assert_eq!(func[taken.args], [expected], "{}", printed(&func, &mut names));

        // And the edge back round carries what the exchange found, since a second read would be a
        // second chance to be wrong.
        let found = func[exchange].first_result.expect("the exchange answers what it found");
        let again = func.successors(branch).nth(1).expect("a branch has a second edge");
        assert_eq!(func[again.args], [found], "{}", printed(&func, &mut names));
    }

    /// A use of the value below the loop reads the parameter of the block the loop leaves to.
    ///
    /// The `return` was in the block the instruction was in, so it moved, and what it returns is no
    /// longer a value anything defines. This is the substitution that makes the rewrite correct
    /// rather than merely well shaped.
    #[test]
    fn a_use_below_the_loop_reads_the_block_parameter() {
        let (mut names, mut func) = built(RmwOp::Xor, Type::int(32));
        loops(&mut func);

        let ret = only(&func, Opcode::Return);
        let block = func.block_of(ret).expect("the return is in a block");
        let returned: Vec<Value> = func[func[ret].args].to_vec();
        assert_eq!(returned, func[block].params, "{}", printed(&func, &mut names));
    }

    /// The four operations that take a maximum or a minimum are a comparison and a select.
    ///
    /// No builtin in either family writes one of these yet, so the only way to reach them is to
    /// build the instruction here. The pass covers them because the opcode does, and the two pairs
    /// differ only in whether the comparison is signed.
    #[test]
    fn a_maximum_or_a_minimum_is_a_compare_and_a_select() {
        for op in [RmwOp::SMax, RmwOp::SMin, RmwOp::UMax, RmwOp::UMin] {
            let (mut names, mut func) = built(op, Type::int(64));
            loops(&mut func);
            verified(&func, &mut names);

            let kinds = opcodes(&func);
            assert!(kinds.contains(&Opcode::ICmp), "{op:?}");
            assert!(kinds.contains(&Opcode::Select), "{op:?}");
            assert!(kinds.contains(&Opcode::Cmpxchg), "{op:?}");
        }
    }

    /// The three with an instruction are left exactly as they were, and so are the two on floats.
    ///
    /// A loop for an exchange or an add would be slower and larger for no reason, and a loop for a
    /// float would need the value carried through an integer of the same width, which an eighty bit
    /// float has none of. Both are left for `crate::lower` to answer, one by lowering it and one by
    /// refusing it.
    #[test]
    fn what_has_an_instruction_and_what_has_no_width_are_left_alone() {
        for op in [RmwOp::Xchg, RmwOp::Add, RmwOp::Sub] {
            let (_, mut func) = built(op, Type::int(32));
            loops(&mut func);
            assert_eq!(func.blocks().count(), 1, "{op:?} has an instruction");
            assert!(opcodes(&func).contains(&Opcode::AtomicRmw), "{op:?}");
        }
        for op in [RmwOp::FAdd, RmwOp::FSub] {
            let (_, mut func) = built(op, Type::float(Float::F64));
            loops(&mut func);
            assert_eq!(func.blocks().count(), 1, "{op:?} has no width to carry it");
            assert!(opcodes(&func).contains(&Opcode::AtomicRmw), "{op:?}");
        }
    }
}
