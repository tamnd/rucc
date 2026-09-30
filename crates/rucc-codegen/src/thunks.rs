//! Indirect branches and returns rewritten for the x86 speculation hardening flags.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12, and tamnd/rucc-kernel
//! `docs/plan/08-codegen-and-abi.md` section 8.6.
//!
//! Four flags, each asking for one rewrite of a finished function, and each written the way gcc
//! writes it because objtool reads the result and complains about anything else:
//!
//! - `-mindirect-branch=thunk-extern` sends a call or jump through a register to the thunk for
//!   that register, so `call *%rax` becomes `call __x86_indirect_thunk_rax`. Every indirect branch
//!   here already goes through a register, so there is never a load to take out of one first.
//! - `-mindirect-branch-cs-prefix` puts a code segment override on its own line in front of a
//!   call or jump to the thunk for `r8` to `r15`.
//! - `-mfunction-return=thunk-extern` turns a `ret` into `jmp __x86_return_thunk`.
//! - `-mharden-sls=` puts an `int3` after a `ret` and after a jump through a register or to a
//!   thunk. None goes after the jump to the return thunk, which is a direct jump, and gcc puts
//!   none there either.
//!
//! It runs last, once every register is settled and every epilogue and tail jump written, because
//! the thunk's name is the register's and a `ret` is only a `ret` once nothing else wants it. The
//! instruction is rewritten in place rather than replaced, so the operands the allocator placed,
//! the table a jump reads and the unwind rows hung on a `ret` all stay on it. The `int3` is the one
//! new instruction, and nothing after this pass asks what a block ends with.
//!
//! The sections objtool writes, `.retpoline_sites` and `.return_sites`, are not written here: they
//! list the call sites objtool found, and it finds them in the object this produces.

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{FrameInsts, Speculation};

/// Rewrites every indirect branch and return in `func` the way `asked` says.
///
/// Nothing happens on a machine with no thunks, which is what [`FrameInsts::thunks`] says, and
/// the driver refuses the flags for every such machine before any of this runs. A return is left
/// alone on a machine with no jump to a name.
///
/// # Panics
///
/// If an indirect branch is found whose address is not in a physical register, which would mean
/// this ran before the allocator.
pub fn harden(func: &mut mir::Func, insts: &FrameInsts, asked: Speculation, names: &mut Interner) {
    let Some(thunks) = insts.thunks else { return };
    if !asked.any() {
        return;
    }
    let mut opcode =
        |name: &str| mir::Opcode::new(names.intern(&format!("{}{name}", insts.prefix)));
    let ret = opcode(insts.ret);
    let away = insts.away.map(&mut opcode);
    let call_through = opcode(thunks.call_through);
    let jump_through = opcode(thunks.jump_through);
    let (call, call_padded) = (opcode(thunks.call), opcode(thunks.call_padded));
    let (jump, jump_padded) = (opcode(thunks.jump), opcode(thunks.jump_padded));
    let trap = opcode(thunks.trap);

    let all: Vec<mir::Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in all {
        let was = func[inst].opcode;
        // Whether a processor could run on past this instruction into whatever comes next.
        let speculated = if was == ret {
            match away.filter(|_| asked.returns) {
                Some(away) => {
                    func[inst].opcode = away;
                    func[inst].symbol = Some(names.intern(thunks.ret));
                    false
                }
                None => asked.after_return,
            }
        } else if was == call_through || was == jump_through {
            let calls = was == call_through;
            if asked.indirect {
                let operands = &func[func[inst].operands];
                let reg = operands[mir::defs(operands)]
                    .reg
                    .phys()
                    .expect("an indirect branch whose address was never given a register");
                let padded = asked.padded && reg.number() >= thunks.padded_from;
                let name = thunks.regs[usize::from(reg.number())];
                func[inst].opcode = match (calls, padded) {
                    (true, false) => call,
                    (true, true) => call_padded,
                    (false, false) => jump,
                    (false, true) => jump_padded,
                };
                func[inst].symbol = Some(names.intern(&format!("{}{name}", thunks.indirect)));
            }
            !calls && asked.after_jump
        } else {
            false
        };
        if speculated {
            let span = func.span(inst);
            let stop = func.build_loose(trap).at(span).finish();
            func.insert_after(inst, stop);
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Func, Opcode, Operand, Reg};
    use rucc_target::Speculation;
    use rucc_target::x86_64::{FRAME, GPR, R11, RAX, RDI, REGS};

    use super::harden;

    /// A function that calls through `%rax`, calls through `%r11`, jumps through `%rdi` to a block
    /// that returns, and returns from the first block's other arm too.
    fn branches(names: &mut Interner) -> Func {
        let mut func = Func::new(names.intern("f"));
        let first = func.create_block();
        let second = func.create_block();
        let call = Opcode::new(names.intern("x64.call_reg"));
        func.build(first, call).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        func.build(first, call).operand(Operand::read(Reg::physical(R11), GPR)).finish();
        let jump = Opcode::new(names.intern("x64.jmp_reg"));
        func.build(first, jump).operand(Operand::read(Reg::physical(RDI), GPR)).finish();
        func.succs_mut(first).push(BlockCall::to(second));
        func.build(second, Opcode::new(names.intern("x64.ret"))).finish();
        func
    }

    /// The function after the rewrite, one instruction to a line.
    fn hardened(asked: Speculation) -> Vec<String> {
        let mut names = Interner::new();
        let mut func = branches(&mut names);
        harden(&mut func, &FRAME, asked, &mut names);
        rucc_mir::print_func(&func, &names, &REGS)
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("x64."))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn nothing_asked_for_changes_nothing() {
        let lines = hardened(Speculation::default());
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[0].starts_with("x64.call_reg"), "{lines:?}");
        assert_eq!(lines[3], "x64.ret", "{lines:?}");
    }

    #[test]
    fn an_indirect_branch_goes_to_the_thunk_for_its_register() {
        let lines = hardened(Speculation { indirect: true, ..Speculation::default() });
        assert!(
            lines[0].starts_with("x64.call_thunk $rax, @__x86_indirect_thunk_rax"),
            "{lines:?}"
        );
        assert!(
            lines[1].starts_with("x64.call_thunk $r11, @__x86_indirect_thunk_r11"),
            "{lines:?}"
        );
        assert!(lines[2].starts_with("x64.jmp_thunk $rdi, @__x86_indirect_thunk_rdi"), "{lines:?}");
        assert_eq!(lines[3], "x64.ret", "{lines:?}");
    }

    #[test]
    fn only_the_thunks_for_the_high_registers_are_padded() {
        let asked = Speculation { indirect: true, padded: true, ..Speculation::default() };
        let lines = hardened(asked);
        assert!(lines[0].starts_with("x64.call_thunk $rax"), "{lines:?}");
        assert!(
            lines[1].starts_with("x64.call_thunk_cs $r11, @__x86_indirect_thunk_r11"),
            "{lines:?}"
        );
        assert!(lines[2].starts_with("x64.jmp_thunk $rdi"), "{lines:?}");
        // The override means nothing without the thunks, and gcc takes it on its own.
        let lines = hardened(Speculation { padded: true, ..Speculation::default() });
        assert!(lines[1].starts_with("x64.call_reg"), "{lines:?}");
    }

    #[test]
    fn a_return_jumps_to_the_return_thunk_with_nothing_after_it() {
        let asked = Speculation { returns: true, after_return: true, ..Speculation::default() };
        let lines = hardened(asked);
        assert_eq!(lines.last().map(String::as_str), Some("x64.jmp_away @__x86_return_thunk"));
        assert!(!lines.iter().any(|line| line == "x64.int3"), "{lines:?}");
    }

    #[test]
    fn a_breakpoint_follows_every_return_and_every_indirect_jump_and_no_call() {
        let asked = Speculation { after_return: true, after_jump: true, ..Speculation::default() };
        let lines = hardened(asked);
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert!(lines[2].starts_with("x64.jmp_reg"), "{lines:?}");
        // The block's edge is printed on its last instruction, which is now the breakpoint.
        assert_eq!(lines[3], "x64.int3 block1", "{lines:?}");
        assert_eq!(lines[4], "x64.ret", "{lines:?}");
        assert_eq!(lines[5], "x64.int3", "{lines:?}");
        // And after the jump to a thunk that takes an indirect jump's place.
        let lines = hardened(Speculation { indirect: true, ..asked });
        assert!(lines[2].starts_with("x64.jmp_thunk"), "{lines:?}");
        assert_eq!(lines[3], "x64.int3 block1", "{lines:?}");
        // Only the return half.
        let lines = hardened(Speculation { after_return: true, ..Speculation::default() });
        assert_eq!(lines[3..], ["x64.ret", "x64.int3"], "{lines:?}");
    }
}
