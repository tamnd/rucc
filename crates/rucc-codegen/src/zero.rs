//! Zeroing the registers a function leaves behind, which is `-fzero-call-used-regs=`.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12, and tamnd/rucc#2281.
//!
//! The kernel builds with `used-gpr` under `CONFIG_ZERO_CALL_USED_REGS`, so that a value a function
//! computed is not still sitting in a register for the next gadget to use. Every `ret` gets one
//! instruction in front of it for each register it clears, and what is cleared is what gcc 13
//! clears, in the order it clears them:
//!
//! - Only a register a call may clobber, since the ones a call keeps are restored by the epilogue
//!   already. On x86-64 that is `rax`, `rdx`, `rcx`, `rsi`, `rdi` and `r8` to `r11`, and on AArch64
//!   `x0` to `x17`. gcc also clears `x18` there, which this compiler always keeps for the platform.
//! - Never a register the return value is in.
//! - For the `used` choices, only a register the body itself wrote or read. A register a call
//!   clobbers is not one the body used, but a register an argument arrived in is, and on AArch64 a
//!   function that makes a call has used `x16` and `x17`, since the linker may put a veneer that
//!   writes them between the call and its target.
//! - For the `-arg` choices, only a register an argument is passed in.
//!
//! It runs after the tail calls are made, since a call that became a jump leaves through the
//! callee's `ret` and gcc clears nothing in front of it, and before the mitigations, so that the
//! registers are clear before a `ret` is turned into a jump to the return thunk.

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{Constraint, MachineInsts, PhysReg, RegClass, RegFile, Role};

/// Which registers a `ret` clears, once a choice other than `skip` is in force.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Zeroing {
    /// Every register a call may clobber rather than only the ones the body used, which is the
    /// `all-gpr` choices.
    pub all: bool,
    /// Only the registers arguments are passed in, which is the choices ending in `-arg`.
    pub arg: bool,
}

/// What one target clears and how.
struct Plan {
    /// The registers a call may clobber, in the order gcc clears them.
    clobbered: &'static [&'static str],
    /// How many of those at the front are the ones arguments are passed in.
    args: usize,
    /// The registers a call writes on its way, which a function that calls anything has used.
    veneers: &'static [&'static str],
    /// The instruction that clears one, and the immediate it carries if it carries one.
    clear: &'static str,
    imm: Option<i64>,
}

const X86_64: Plan = Plan {
    clobbered: &["rax", "rdx", "rcx", "rsi", "rdi", "r8", "r9", "r10", "r11"],
    args: 7,
    veneers: &[],
    clear: "xor_rr_32",
    imm: None,
};

const AARCH64: Plan = Plan {
    clobbered: &[
        "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13",
        "x14", "x15", "x16", "x17",
    ],
    args: 8,
    veneers: &["x16", "x17"],
    clear: "mov_ri_64",
    imm: Some(0),
};

/// Clears the registers `how` asks for in front of every `ret` in `func`, and says how many
/// instructions it put in.
///
/// Nothing is done on a target other than x86-64 and AArch64, which the driver does not take the
/// flag for.
///
/// # Panics
///
/// When the target's register file does not name one of the registers above, or has no
/// instruction of the name above, both of which would be a mistake in this file.
pub fn apply(
    func: &mut mir::Func,
    how: Option<Zeroing>,
    insts: &MachineInsts,
    file: &RegFile,
    names: &mut Interner,
) -> usize {
    let Some(how) = how else { return 0 };
    let plan = match insts.prefix {
        "x64." => &X86_64,
        "a64." => &AARCH64,
        _ => return 0,
    };
    let ret = mir::Opcode::new(names.intern(&format!("{}ret", insts.prefix)));
    let clear = mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, plan.clear)));
    let shape = (insts.operands)(plan.clear).expect("the clearing instruction is in the table");
    let reg = |name: &str| file.reg_named(name).expect("a register the file names");

    let all: Vec<mir::Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    let returns: Vec<mir::Inst> =
        all.iter().copied().filter(|&inst| func[inst].opcode == ret).collect();
    if returns.is_empty() {
        return 0;
    }

    let mut used: Vec<(RegClass, PhysReg)> = Vec::new();
    let mut result: Vec<(RegClass, PhysReg)> = Vec::new();
    let mut returned: Vec<(RegClass, PhysReg)> = Vec::new();
    let mut read: Vec<(RegClass, PhysReg)> = Vec::new();
    let mut called = false;
    for &inst in &all {
        let name = names.resolve(func[inst].opcode.name());
        let bare = insts.bare(name);
        let call = insts.calls(name);
        called |= call;
        let answer = bare.starts_with("ret_val");
        for operand in &func[func[inst].operands] {
            let Some(phys) = operand.reg.phys() else { continue };
            // What a call clobbers is written as a def the allocator must not put a value
            // across, and the body did not use it. The value a call returns is fixed to its
            // register and is the body's once something reads it, which is how gcc sees a call
            // whose result is thrown away.
            if call && operand.role.is_def() {
                if matches!(operand.constraint, Constraint::Fixed(_)) {
                    returned.push((operand.class, phys));
                }
                continue;
            }
            used.push((operand.class, phys));
            if operand.role == Role::Use {
                read.push((operand.class, phys));
            }
            if answer && operand.role == Role::Use {
                result.push((operand.class, phys));
            }
        }
    }
    used.extend(returned.into_iter().filter(|found| read.contains(found)));
    if called {
        used.extend(plan.veneers.iter().map(|name| reg(name)));
    }

    let candidates = if how.arg { &plan.clobbered[..plan.args] } else { plan.clobbered };
    let cleared: Vec<(RegClass, PhysReg)> = candidates
        .iter()
        .map(|name| reg(name))
        .filter(|found| !result.contains(found))
        .filter(|found| how.all || used.contains(found))
        .collect();

    let mut added = 0;
    for &at in &returns {
        for &(class, phys) in &cleared {
            let mut build = func.build_loose(clear);
            for want in shape {
                let operand = mir::Operand {
                    reg: mir::Reg::physical(phys),
                    class,
                    role: want.role,
                    constraint: Constraint::Fixed(phys),
                };
                build = build.operand(operand);
            }
            if let Some(imm) = plan.imm {
                build = build.imm(imm);
            }
            let inst = build.finish();
            func.insert_before(at, inst);
            added += 1;
        }
    }
    added
}
