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
//!   `thunk` does the same, and [`bodies`] is the copy of each thunk the unit carries.
//!   `thunk-inline` writes the thunk's instructions in place of the branch instead, as a template
//!   kept as text, which is what the kernel's vDSO is built with.
//! - `-mindirect-branch-cs-prefix` puts a code segment override on its own line in front of a
//!   call or jump to the thunk for `r8` to `r15`.
//! - `-mfunction-return=thunk-extern` turns a `ret` into `jmp __x86_return_thunk`, and the other
//!   two answers go the way they do for an indirect branch.
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
use rucc_target::template::template_reg;
use rucc_target::{FrameInsts, Speculation, Thunk};

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
    let template = opcode(thunks.template);

    let all: Vec<mir::Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in all {
        let was = func[inst].opcode;
        // Whether a processor could run on past this instruction into whatever comes next.
        let speculated = if was == ret {
            match (asked.returns, away) {
                (Thunk::Inline, _) => {
                    func[inst].opcode = template;
                    func[inst].symbol = Some(names.intern(thunks.back));
                    false
                }
                (Thunk::Extern | Thunk::Comdat, Some(away)) => {
                    func[inst].opcode = away;
                    func[inst].symbol = Some(names.intern(thunks.ret));
                    false
                }
                _ => asked.after_return,
            }
        } else if was == call_through || was == jump_through {
            let calls = was == call_through;
            let operands = &func[func[inst].operands];
            let through = mir::defs(operands);
            let reg = operands[through].reg.phys();
            if asked.indirect == Thunk::Inline {
                // The register is a hole in the text, filled with whatever the writer spells the
                // operand as, which is the one the allocator gave the address.
                let hole = template_reg(through, 'q');
                let text = if calls { (thunks.around)(&hole) } else { (thunks.through)(&hole) };
                func[inst].opcode = template;
                func[inst].symbol = Some(names.intern(&text));
            } else if asked.indirect.named() {
                let reg = reg.expect("an indirect branch whose address was never given a register");
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

/// The thunks `-mindirect-branch=thunk` and `-mfunction-return=thunk` ask the unit to carry, one
/// for each thunk a function in `funcs` goes to, as the text of an `asm` at file scope.
///
/// Each is in a section of its own named after it and in a COMDAT group about its name, hidden, so
/// every object that calls one has a copy and the link keeps one. That is what gcc writes, down to
/// the order, which is by name.
#[must_use]
pub fn bodies(
    funcs: &[mir::Func],
    insts: &FrameInsts,
    asked: Speculation,
    names: &Interner,
) -> Vec<String> {
    let Some(thunks) = insts.thunks else { return Vec::new() };
    let wanted = |name: &str| {
        if name == thunks.ret {
            asked.returns == Thunk::Comdat
        } else {
            asked.indirect == Thunk::Comdat && name.starts_with(thunks.indirect)
        }
    };
    let kinds = [thunks.call, thunks.call_padded, thunks.jump, thunks.jump_padded];
    let away = insts.away;
    let mut used: Vec<&str> = funcs
        .iter()
        .flat_map(|func| {
            func.blocks().flat_map(move |block| func.insts(block).map(move |inst| &func[inst]))
        })
        .filter(|data| {
            let opcode = names.resolve(data.opcode.name());
            let opcode = opcode.strip_prefix(insts.prefix).unwrap_or(opcode);
            kinds.contains(&opcode) || away == Some(opcode)
        })
        .filter_map(|data| data.symbol.map(|symbol| names.resolve(symbol)))
        .filter(|name| wanted(name))
        .collect();
    used.sort_unstable();
    used.dedup();
    used.into_iter()
        .map(|name| {
            let body = match name.strip_prefix(thunks.indirect) {
                Some(reg) => (thunks.through)(&format!("%{reg}")),
                None => thunks.back.to_owned(),
            };
            format!(
                ".section .text.{name},\"axG\",@progbits,{name},comdat\n.globl {name}\n\
                 .hidden {name}\n.type {name}, @function\n{name}:\n{body}\n.size {name}, .-{name}"
            )
        })
        .collect()
}

/// The two instructions that put the global offset table's address in `%ebx`, which i386 position
/// independent code writes at the top of a function that reaches a name. Kept as a template by
/// `crate::lower`, and looked for by [`pc_thunk`].
pub const TABLE_BASE: &str = "call\t__x86.get_pc_thunk.bx\naddl\t$_GLOBAL_OFFSET_TABLE_, %ebx";

/// The routine [`TABLE_BASE`] calls, which reads its own return address into `%ebx`, when some
/// function in `funcs` calls it, and nothing otherwise.
///
/// Written the way gcc writes it: hidden, and in a group of its own so that the link keeps one copy
/// out of however many objects carry it. The name is gcc's too, so an object from either compiler
/// can be linked with the other's and the two copies are still one.
#[must_use]
pub fn pc_thunk(funcs: &[mir::Func], names: &Interner) -> Option<String> {
    let called = funcs.iter().any(|func| {
        func.blocks().any(|block| {
            func.insts(block)
                .any(|inst| func[inst].symbol.is_some_and(|text| names.resolve(text) == TABLE_BASE))
        })
    });
    let name = "__x86.get_pc_thunk.bx";
    called.then(|| {
        format!(
            ".section .text.{name},\"axG\",@progbits,{name},comdat\n.globl {name}\n\
             .hidden {name}\n.type {name}, @function\n{name}:\nmovl (%esp), %ebx\nret\n\
             .size {name}, .-{name}"
        )
    })
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Func, Opcode, Operand, Reg};
    use rucc_target::x86_64::{FRAME, GPR, R11, RAX, RDI, REGS};
    use rucc_target::{Speculation, Thunk};

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
        let lines = hardened(Speculation { indirect: Thunk::Extern, ..Speculation::default() });
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
        let asked = Speculation { indirect: Thunk::Extern, padded: true, ..Speculation::default() };
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
        let asked =
            Speculation { returns: Thunk::Extern, after_return: true, ..Speculation::default() };
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
        let lines = hardened(Speculation { indirect: Thunk::Extern, ..asked });
        assert!(lines[2].starts_with("x64.jmp_thunk"), "{lines:?}");
        assert_eq!(lines[3], "x64.int3 block1", "{lines:?}");
        // Only the return half.
        let lines = hardened(Speculation { after_return: true, ..Speculation::default() });
        assert_eq!(lines[3..], ["x64.ret", "x64.int3"], "{lines:?}");
    }

    #[test]
    fn an_inline_thunk_is_the_sequence_written_where_the_branch_was() {
        let asked = Speculation {
            indirect: Thunk::Inline,
            returns: Thunk::Inline,
            ..Speculation::default()
        };
        let lines = hardened(asked);
        assert_eq!(lines.len(), 4, "{lines:?}");
        // A call jumps over its own copy of the thunk and calls it, a jump goes into one, and the
        // register stays the operand, for the writer to spell where the text has a hole for it.
        // The text runs over more than one line, and the first is enough to tell which it is.
        assert_eq!(
            lines,
            [
                "x64.template $rax, @jmp 3f",
                "x64.template $r11, @jmp 3f",
                "x64.template $rdi, @call 1f",
                "x64.template @call 1f"
            ]
        );
    }

    #[test]
    fn a_unit_carries_each_thunk_it_calls_once_and_in_order() {
        let mut names = Interner::new();
        let asked = Speculation {
            indirect: Thunk::Comdat,
            returns: Thunk::Comdat,
            ..Speculation::default()
        };
        let funcs = [branches(&mut names), branches(&mut names)].map(|mut func| {
            harden(&mut func, &FRAME, asked, &mut names);
            func
        });
        let bodies = super::bodies(&funcs, &FRAME, asked, &names);
        let heads: Vec<&str> = bodies
            .iter()
            .filter_map(|body| body.lines().find(|line| line.ends_with(':')))
            .collect();
        assert_eq!(
            heads,
            [
                "__x86_indirect_thunk_r11:",
                "__x86_indirect_thunk_rax:",
                "__x86_indirect_thunk_rdi:",
                "__x86_return_thunk:"
            ]
        );
        assert!(
            bodies[1].starts_with(
                ".section .text.__x86_indirect_thunk_rax,\"axG\",@progbits,\
                 __x86_indirect_thunk_rax,comdat"
            ),
            "{bodies:?}"
        );
        assert!(bodies[1].contains("mov %rax, (%rsp)"), "{bodies:?}");
        // A thunk linked in from somewhere else is not the unit's to carry.
        let extern_ = Speculation { indirect: Thunk::Extern, ..asked };
        assert_eq!(super::bodies(&funcs, &FRAME, extern_, &names).len(), 1);
    }
}
