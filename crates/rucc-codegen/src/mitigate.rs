//! The x86-64 speculation mitigations the kernel builds with, applied once every register is known.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12, and tamnd/rucc#2280.
//!
//! Four of gcc's flags, each a rewrite of a branch the code generator already wrote:
//!
//! - `-mindirect-branch=thunk-extern` turns `call *%rax` into `call __x86_indirect_thunk_rax` and
//!   `jmp *%rax` into `jmp __x86_indirect_thunk_rax`. The thunk is the kernel's, and at boot it is
//!   either a retpoline or patched back into the plain branch.
//! - `-mindirect-branch-cs-prefix` puts `cs` in front of a call or a jump to a thunk through `r8`
//!   to `r15`, so that each is six bytes and the kernel can write the plain branch over it in place.
//! - `-mfunction-return=thunk-extern` turns `ret` into `jmp __x86_return_thunk`.
//! - `-mharden-sls=` puts `int3` after a `ret` and after a jump through a register, including one
//!   that became a jump to a thunk, so that nothing past them runs even speculatively.
//!
//! It runs last, after the tail calls are made, because the register a call goes through is only
//! known after allocation and because a `ret` a tail call turned into a jump is not a return any
//! more. Every branch is changed in place rather than replaced, so the unwind rows the epilogue
//! hung on a `ret` stay on the jump that took its place.

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::RegFile;

/// What the command line asked for, and what a function's attributes left of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mitigations {
    /// `-mindirect-branch=thunk-extern`.
    pub indirect: bool,
    /// `-mindirect-branch-cs-prefix`.
    pub cs_prefix: bool,
    /// `-mfunction-return=thunk-extern`.
    pub returns: bool,
    /// `-mharden-sls=return` or `all`.
    pub sls_return: bool,
    /// `-mharden-sls=indirect-jmp` or `all`.
    pub sls_jump: bool,
}

impl Mitigations {
    /// Whether any of them is on, which is whether the pass has anything to do.
    #[must_use]
    pub const fn any(self) -> bool {
        self.indirect || self.returns || self.sls_return || self.sls_jump
    }
}

/// The thunk a branch through `reg` goes to, which is named after the register the way the
/// kernel's `arch/x86/lib/retpoline.S` names them.
fn thunk(reg: &str) -> String {
    format!("__x86_indirect_thunk_{reg}")
}

/// Whether a register is one of the eight a REX prefix reaches, whose thunk call is a byte longer
/// than the others and gets `cs` to make the other eight the same length.
fn extended(reg: &str) -> bool {
    matches!(reg, "r8" | "r9" | "r10" | "r11" | "r12" | "r13" | "r14" | "r15")
}

/// Rewrites every return and indirect branch in `func` as `on` asks, and says how many it changed.
///
/// # Panics
///
/// When a branch through a register is not through a physical one the file names, which would be
/// a function that did not go through the allocator.
pub fn apply(func: &mut mir::Func, on: Mitigations, file: &RegFile, names: &mut Interner) -> usize {
    if !on.any() {
        return 0;
    }
    let mut opcode = |name: &str| mir::Opcode::new(names.intern(&format!("x64.{name}")));
    let ret = opcode("ret");
    let call_reg = opcode("call_reg");
    let jmp_reg = opcode("jmp_reg");
    let call = opcode("call");
    let call_cs = opcode("call_cs");
    let away = opcode("jmp_away");
    let away_cs = opcode("jmp_away_cs");
    let int3 = opcode("int3");

    let insts: Vec<mir::Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    let mut changed = 0;
    for inst in insts {
        let was = func[inst].opcode;
        if was == ret {
            if on.returns {
                func[inst].opcode = away;
                func[inst].symbol = Some(names.intern("__x86_return_thunk"));
            } else if on.sls_return {
                let stop = func.build_loose(int3).finish();
                func.insert_after(inst, stop);
            } else {
                continue;
            }
            changed += 1;
        } else if was == call_reg || was == jmp_reg {
            let jump = was == jmp_reg;
            if on.indirect {
                let operands = &func[func[inst].operands];
                let through = operands[mir::defs(operands)];
                let reg = through.reg.phys().expect("a branch is through a register by now");
                let name = file.name(through.class, reg).expect("a register the file names");
                let cs = on.cs_prefix && extended(name);
                func[inst].opcode = match (jump, cs) {
                    (false, false) => call,
                    (false, true) => call_cs,
                    (true, false) => away,
                    (true, true) => away_cs,
                };
                func[inst].symbol = Some(names.intern(&thunk(name)));
            }
            if jump && on.sls_jump {
                let stop = func.build_loose(int3).finish();
                func.insert_after(inst, stop);
            }
            if on.indirect || (jump && on.sls_jump) {
                changed += 1;
            }
        }
    }
    changed
}
