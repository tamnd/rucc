//! Zeroing the registers a function leaves behind, which is `-fzero-call-used-regs=`.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12, tamnd/rucc#2281 and tamnd/rucc#2335.
//!
//! The kernel builds with `used-gpr` under `CONFIG_ZERO_CALL_USED_REGS`, so that a value a function
//! computed is not still sitting in a register for the next gadget to use. Every `ret` gets one
//! instruction in front of it for each register it clears, and what is cleared is what gcc 16
//! clears, in the order it clears them:
//!
//! - Only a register a call may clobber, since the ones a call keeps are restored by the epilogue
//!   already. On x86-64 that is `rax`, `rdx`, `rcx`, `rsi`, `rdi` and `r8` to `r11`, and on AArch64
//!   `x0` to `x17`. gcc also clears `x18` there, which this compiler always keeps for the platform.
//!   On i386 it is `eax`, `edx` and `ecx`, and `xmm0` to `xmm7` after them, all eight of which a
//!   call clobbers there.
//! - Never a register the return value is in.
//! - For the `used` choices, only a register the body itself wrote or read. A register a call
//!   clobbers is not one the body used, but a register an argument arrived in is, and on AArch64 a
//!   function that makes a call has used `x16` and `x17`, since the linker may put a veneer that
//!   writes them between the call and its target.
//! - For the `-arg` choices, only a register an argument is passed in. On i386 that is the three
//!   general purpose registers `-mregparm=3` would use and `xmm0` to `xmm2`, which is gcc's
//!   answer whatever the function's own convention is.
//! - For the choices without `-gpr` in them, the vector registers as well, by the same three
//!   rules: `xmm0` to `xmm15` on x86-64, with the eight arguments are passed in coming in gcc's
//!   order between `rdi` and `r8`, and `v0` to `v7` and `v16` to `v31` on AArch64 after every
//!   general purpose one, since the low halves of `v8` to `v15` are kept by a call there. A unit
//!   built with `-mno-sse` or `-mgeneral-regs-only` has no vector registers to clear.
//! - And the x87 stack on x86-64, for `used` and `all` but never for an `-arg` choice, since no
//!   argument is passed on it. All eight registers are cleared, less the ones a `long double` is
//!   being returned in, with as many `fldz` followed by as many `fstp %st(0)`, and that comes
//!   before every register cleared one at a time. `all` always clears it and `used` only when the body put something
//!   on the stack other than the value it returns. A unit built with `-mno-80387` has no stack.
//!
//! What a vector register is cleared with depends on the extensions the unit may use, as it does
//! for gcc. `pxor` with SSE alone. With AVX the VEX encoded `vxorps`, since that clears the upper
//! half of the `ymm` register as well, and for `all` with nothing returned in a vector register a
//! single `vzeroall` in front of everything else rather than sixteen of them. With AVX-512 `all`
//! also clears `zmm16` to `zmm31`, with `vpxord` or, when AVX-512VL and AVX-512DQ let it name them
//! as `xmm16` to `xmm31`, with `vxorps`, and the eight mask registers with `kxorw`. No value is
//! ever allocated to any of those, so they are named outright. i386 has no `zmm16` to `zmm31`,
//! so there only the masks are cleared.
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
    /// choices starting with `all`.
    pub all: bool,
    /// Only the registers arguments are passed in, which is the choices ending in `-arg`.
    pub arg: bool,
    /// The vector registers and the x87 stack as well as the general purpose registers, which is
    /// the four choices without `-gpr` in them.
    pub wide: bool,
}

/// What the unit may keep a value in besides the general purpose registers, which is what the
/// choices without `-gpr` in them clear and what decides how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Files {
    /// Whether there are vector registers, which `-mno-sse` on x86-64 and `-mgeneral-regs-only`
    /// on either machine take away.
    pub vector: bool,
    /// Which of the extensions that change how a vector register is cleared the unit may use.
    pub extension: Extension,
    /// Whether there is an x87 stack, which `-mno-80387` and `-mgeneral-regs-only` take away.
    pub x87: bool,
    /// How many values the function gives back on the x87 stack, which is one for a `long double`
    /// and two for a complex one. Those registers are not cleared.
    pub x87_returned: usize,
}

impl Default for Files {
    /// Vector registers with nothing past SSE2 and an x87 stack with nothing returned on it, which
    /// is a plain x86-64 unit.
    fn default() -> Self {
        Self { vector: true, extension: Extension::Sse, x87: true, x87_returned: 0 }
    }
}

/// The extensions on x86-64 that change how a vector register is cleared, the last one the unit
/// may use. Read on no other machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Extension {
    /// SSE2 and nothing that changes the answer, so `pxor`.
    #[default]
    Sse,
    /// AVX, so `vxorps` and `vzeroall`, which clear the upper half of a `ymm` register too.
    Avx,
    /// AVX-512F, which adds sixteen more vector registers and eight mask registers to clear. The
    /// upper sixteen are cleared whole with `vpxord`, since without AVX-512VL there is no way to
    /// name only their low 128 bits.
    Avx512,
    /// AVX-512F with AVX-512VL, where gcc clears the upper sixteen with `vxorps` on their `xmm`
    /// names instead, which writes the whole register all the same.
    Avx512Vl,
}

/// One register a call may clobber, and what it is to the choices.
#[derive(Clone, Copy)]
struct Entry {
    name: &'static str,
    /// Whether an argument is passed in it, which is what the `-arg` choices clear.
    arg: bool,
    /// Whether it is a vector register, which only the choices without `-gpr` clear.
    vector: bool,
}

const fn gpr(name: &'static str, arg: bool) -> Entry {
    Entry { name, arg, vector: false }
}

const fn vec(name: &'static str, arg: bool) -> Entry {
    Entry { name, arg, vector: true }
}

/// What one target clears and how.
struct Plan {
    /// The registers a call may clobber, in the order gcc clears them.
    clobbered: &'static [Entry],
    /// The registers a call writes on its way, which a function that calls anything has used.
    veneers: &'static [&'static str],
    /// The instruction that clears a general purpose register, and the immediate it carries if
    /// it carries one.
    clear: &'static str,
    imm: Option<i64>,
    /// The instruction that clears a vector register.
    clear_vector: &'static str,
    /// Whether there are sixteen more vector registers with AVX-512, which there are not on i386.
    upper: bool,
}

/// gcc's order is its own register numbers, which put the eight vector registers arguments are
/// passed in between `rdi` and `r8` and the other eight after `r11`.
const X86_64: Plan = Plan {
    clobbered: &[
        gpr("rax", true),
        gpr("rdx", true),
        gpr("rcx", true),
        gpr("rsi", true),
        gpr("rdi", true),
        vec("xmm0", true),
        vec("xmm1", true),
        vec("xmm2", true),
        vec("xmm3", true),
        vec("xmm4", true),
        vec("xmm5", true),
        vec("xmm6", true),
        vec("xmm7", true),
        gpr("r8", true),
        gpr("r9", true),
        gpr("r10", false),
        gpr("r11", false),
        vec("xmm8", false),
        vec("xmm9", false),
        vec("xmm10", false),
        vec("xmm11", false),
        vec("xmm12", false),
        vec("xmm13", false),
        vec("xmm14", false),
        vec("xmm15", false),
    ],
    veneers: &[],
    clear: "xor_rr_32",
    imm: None,
    clear_vector: "pxor_rr",
    upper: true,
};

/// gcc's numbers on i386 put `eax`, `edx` and `ecx` first and the vector registers after the x87
/// stack, and every vector register is one a call clobbers.
const I386: Plan = Plan {
    clobbered: &[
        gpr("eax", true),
        gpr("edx", true),
        gpr("ecx", true),
        vec("xmm0", true),
        vec("xmm1", true),
        vec("xmm2", true),
        vec("xmm3", false),
        vec("xmm4", false),
        vec("xmm5", false),
        vec("xmm6", false),
        vec("xmm7", false),
    ],
    veneers: &[],
    clear: "xor_rr_32",
    imm: None,
    clear_vector: "pxor_rr",
    upper: false,
};

/// `v8` to `v15` are not here, since a call keeps their low halves and gcc counts them as kept.
const AARCH64: Plan = Plan {
    clobbered: &[
        gpr("x0", true),
        gpr("x1", true),
        gpr("x2", true),
        gpr("x3", true),
        gpr("x4", true),
        gpr("x5", true),
        gpr("x6", true),
        gpr("x7", true),
        gpr("x8", false),
        gpr("x9", false),
        gpr("x10", false),
        gpr("x11", false),
        gpr("x12", false),
        gpr("x13", false),
        gpr("x14", false),
        gpr("x15", false),
        gpr("x16", false),
        gpr("x17", false),
        vec("v0", true),
        vec("v1", true),
        vec("v2", true),
        vec("v3", true),
        vec("v4", true),
        vec("v5", true),
        vec("v6", true),
        vec("v7", true),
        vec("v16", false),
        vec("v17", false),
        vec("v18", false),
        vec("v19", false),
        vec("v20", false),
        vec("v21", false),
        vec("v22", false),
        vec("v23", false),
        vec("v24", false),
        vec("v25", false),
        vec("v26", false),
        vec("v27", false),
        vec("v28", false),
        vec("v29", false),
        vec("v30", false),
        vec("v31", false),
    ],
    veneers: &["x16", "x17"],
    clear: "mov_ri_64",
    imm: Some(0),
    clear_vector: "movi_zero",
    upper: false,
};

/// How many registers the x87 stack has, all of which `all` clears.
const X87_DEPTH: usize = 8;

/// Clears the registers `how` asks for in front of every `ret` in `func`, and says how many
/// instructions it put in.
///
/// Nothing is done on a target other than x86-64, i386 and AArch64, which the driver does not take
/// the flag for.
///
/// # Panics
///
/// When the target's register file does not name one of the registers above, or has no
/// instruction of the name above, both of which would be a mistake in this file.
pub fn apply(
    func: &mut mir::Func,
    how: Option<Zeroing>,
    files: Files,
    insts: &MachineInsts,
    file: &RegFile,
    names: &mut Interner,
) -> usize {
    let Some(how) = how else { return 0 };
    let x86 = insts.prefix == "x64.";
    // i386 writes x86-64's instructions, and its file is the one with no `rax`.
    let plan = match insts.prefix {
        "x64." if file.reg_named("rax").is_none() => &I386,
        "x64." => &X86_64,
        "a64." => &AARCH64,
        _ => return 0,
    };
    let ret = mir::Opcode::new(names.intern(&format!("{}ret", insts.prefix)));
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

    let vectors = how.wide && files.vector;
    let cleared: Vec<(Entry, RegClass, PhysReg)> = plan
        .clobbered
        .iter()
        .filter(|entry| !how.arg || entry.arg)
        .filter(|entry| vectors || !entry.vector)
        .map(|&entry| {
            let (class, phys) = reg(entry.name);
            (entry, class, phys)
        })
        .filter(|&(_, class, phys)| !result.contains(&(class, phys)))
        .filter(|&(_, class, phys)| how.all || used.contains(&(class, phys)))
        .collect();

    // The x87 stack, which only `used` and `all` clear. `all` clears it whatever the body did,
    // and `used` when the body put something on it besides what it gives back, which is when it
    // was ever deeper than the values it returns leave it.
    let x87 = x86
        && how.wide
        && !how.arg
        && files.x87
        && files.x87_returned < X87_DEPTH
        && (how.all || x87_depth(func, &all, insts, names) > files.x87_returned);
    let x87 = if x87 { X87_DEPTH - files.x87_returned } else { 0 };

    // A whole unit's worth of AVX registers is one instruction, which gcc writes in front of the
    // general purpose registers when `all` clears every vector register there is. With anything
    // returned in one it cannot, and the rest are cleared one at a time.
    let extension = if x86 && vectors { Some(files.extension) } else { None };
    let every = how.all && !how.arg;
    let vector_count = cleared.iter().filter(|(entry, _, _)| entry.vector).count();
    let whole = extension >= Some(Extension::Avx)
        && every
        && vector_count == plan.clobbered.iter().filter(|entry| entry.vector).count();
    let high = every && extension >= Some(Extension::Avx512);
    let upper =
        if extension == Some(Extension::Avx512Vl) { "zero_xmm_high" } else { "zero_zmm_high" };
    let clear_vector =
        if extension >= Some(Extension::Avx) { "vxorps_rr" } else { plan.clear_vector };

    let opcode =
        |names: &mut Interner, name: &'static str| mir::Opcode::new(names.join(insts.prefix, name));
    // In gcc's order, which puts the vector registers that are cleared all at once in front of
    // everything, then the x87 stack, then one register at a time, and the masks last.
    let mut written: Vec<Write> = Vec::new();
    if whole {
        written.push(Write::Bare("vzeroall"));
        if high && plan.upper {
            written.push(Write::Bare(upper));
        }
    }
    written.extend(std::iter::repeat_n(Write::Bare("fldz"), x87));
    written.extend(std::iter::repeat_n(Write::Bare("fstp_top"), x87));
    for &(entry, class, phys) in &cleared {
        if !entry.vector {
            written.push(Write::Clear(plan.clear, plan.imm, class, phys));
        } else if !whole {
            written.push(Write::Clear(clear_vector, None, class, phys));
        }
    }
    if high && plan.upper && !whole {
        written.push(Write::Bare(upper));
    }
    if high {
        written.push(Write::Bare("zero_masks"));
    }

    let mut added = 0;
    for &at in &returns {
        for &write in &written {
            let inst = match write {
                Write::Bare(name) => {
                    let code = opcode(names, name);
                    func.build_loose(code).finish()
                }
                Write::Clear(name, imm, class, phys) => {
                    let code = opcode(names, name);
                    let shape = (insts.operands)(name).expect("the clearing instruction is known");
                    let mut build = func.build_loose(code);
                    for want in shape {
                        let operand = mir::Operand {
                            reg: mir::Reg::physical(phys),
                            class,
                            role: want.role,
                            constraint: Constraint::Fixed(phys),
                        };
                        build = build.operand(operand);
                    }
                    if let Some(imm) = imm {
                        build = build.imm(imm);
                    }
                    build.finish()
                }
            };
            func.insert_before(at, inst);
            added += 1;
        }
    }
    added
}

/// One instruction the zeroing writes in front of a `ret`.
#[derive(Clone, Copy)]
enum Write {
    /// One with no operands, which is the x87 stack and the vector registers all at once.
    Bare(&'static str),
    /// One that clears the register it names, with the immediate it carries if it carries one.
    Clear(&'static str, Option<i64>, RegClass, PhysReg),
}

/// How deep the body ever has the x87 stack, which is empty at the start of every block on
/// x86-64 since the lowering leaves it empty between the groups it writes.
///
/// An instruction that pushes adds one and one that pops takes one off, and the arithmetic and the
/// comparisons pop as well: an arithmetic one pops one of its two operands, and a comparison pops
/// both, one in the comparison itself and one in the `fstp` after it.
fn x87_depth(func: &mir::Func, all: &[mir::Inst], insts: &MachineInsts, names: &Interner) -> usize {
    use rucc_target::x86_64::Form;
    let mut deepest = 0;
    let mut depth: usize = 0;
    let mut block = None;
    for &inst in all {
        if func.block_of(inst) != block {
            block = func.block_of(inst);
            depth = 0;
        }
        let name = insts.bare(names.resolve(func[inst].opcode.name()));
        match rucc_target::x86_64::form(name) {
            Some(Form::PushX87) => depth += 1,
            Some(Form::PopX87 | Form::ArithX87) => depth = depth.saturating_sub(1),
            Some(Form::CmpSetX87 | Form::CmpSetX87Both) => depth = depth.saturating_sub(2),
            _ => {}
        }
        deepest = deepest.max(depth);
    }
    deepest
}
