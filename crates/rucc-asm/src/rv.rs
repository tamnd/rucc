//! One RISC-V machine instruction as assembly text.
//!
//! The same split [`crate::a64`] makes. The file around the instruction is written by
//! [`crate::att`], and the instruction is `rucc_target::riscv64::written`, filled in with
//! `rucc_target::riscv64::fill` and spelled by `rucc_target::riscv64::write`, which are the tables
//! and the values the encoder reads too.
//!
//! An addressing mode here is a base register and a constant and nothing else. A symbol is
//! reached by the instruction's own name, `lla` or `la`, with the constant beside the name as the
//! addend, and a block or a jump table is reached the same way by its label.
//!
//! Every line is handed to the encoder before it is written, so what the assembler would reject
//! is refused here with the encoder's reason.

use std::fmt::Write as _;

use rucc_base::Interner;
use rucc_mir::{Amode, Block, Func, Inst, defs};
use rucc_target::riscv64::{self, Addr, Arg, FPR, FPR_NAMES, GPR_NAMES, Offset, Operands};
use rucc_target::template::{template_arms, template_filled};

use crate::Error;

/// The prefix every RISC-V opcode carries in the machine IR.
pub(crate) const PREFIX: &str = "rv.";

/// What an instruction needs from the file it is written into.
pub(crate) struct Context<'a> {
    /// The names every symbol and opcode is interned in.
    pub names: &'a Interner,
    /// The name of the function the instruction is in.
    pub func_name: &'a str,
}

/// One instruction of the machine IR, as however many instructions of the machine it is, each on
/// a line of its own.
///
/// `label` is the name the block a branch goes to is written with, and `table` the name of a jump
/// table, which the file hands in since it is the one that numbered them.
pub(crate) fn inst(
    out: &mut String,
    at: &Context<'_>,
    func: &Func,
    block: Block,
    inst: Inst,
    label: impl Fn(Block) -> String,
    table: impl Fn(u32) -> String,
) -> Result<(), Error> {
    let data = func[inst];
    let spelled = at.names.resolve(data.opcode.name());
    let opcode = spelled.strip_prefix(PREFIX).unwrap_or(spelled);
    let refused = || Error::Opcode { func: at.func_name.to_owned(), opcode: spelled.to_owned() };
    if opcode == riscv64::TEMPLATE {
        return template(out, at, func, block, inst, label);
    }
    let Some(written) = riscv64::written(opcode) else {
        return Err(refused());
    };
    if written.is_empty() {
        return Ok(());
    }

    let operands = &func[data.operands];
    let mut regs = Vec::with_capacity(operands.len());
    for operand in operands {
        let Some(phys) = operand.reg.phys() else {
            return Err(Error::Virtual {
                func: at.func_name.to_owned(),
                opcode: spelled.to_owned(),
            });
        };
        regs.push(phys.number());
    }
    // A mode with no base is a name, a block or a table, which the instruction reaches by itself
    // and writes as its symbol. The constant beside it is the addend.
    let named = data.mem.map(|mem| &func[mem]).filter(|amode| amode.base.is_none());
    let named = named.and_then(|amode| {
        let name = match amode {
            Amode { block: Some(to), .. } => label(*to),
            Amode { table: Some(at), .. } => table(*at),
            Amode { symbol: Some(symbol), .. } => at.names.resolve(*symbol).to_owned(),
            _ => return None,
        };
        Some(match amode.disp {
            0 => name,
            disp => format!("{name}{disp:+}"),
        })
    });
    let mem = match data.mem {
        Some(_) if named.is_some() => None,
        Some(mem) => Some(address(&func[mem], &regs).ok_or_else(refused)?),
        None => None,
    };
    let with = Operands {
        regs: &regs,
        reads: defs(operands),
        imm: data.imm.map_or(0, |imm| func[imm].0),
        mem,
    };

    // The one name the line can carry. A conditional branch goes to the first arm, because the
    // layout falls into the second.
    let branches = written.iter().any(|machine| machine.args.contains(&Arg::Label));
    let symbol = if branches {
        func[block].succs.first().map(|to| label(to.block))
    } else if named.is_some() {
        named
    } else {
        data.symbol.map(|symbol| at.names.resolve(symbol).to_owned())
    };

    for machine in written {
        let mut values = Vec::with_capacity(machine.args.len());
        for &arg in machine.args {
            values.push(riscv64::fill(arg, &with).map_err(|_| refused())?);
        }
        if let Err(why) = riscv64::encode(machine.mnemonic, &values) {
            return Err(Error::Encode {
                func: at.func_name.to_owned(),
                opcode: spelled.to_owned(),
                why: why.to_string(),
            });
        }
        let line = riscv64::write(machine.mnemonic, &values, symbol.as_deref());
        let _ = writeln!(out, "\t{line}");
    }
    Ok(())
}

/// An `asm` template kept as text, as its own lines with each register it names spelled as the
/// register its operand was given, by its ABI name.
///
/// The one memory operand a template can have is written as GNU as reads one, the constant and
/// then the base in brackets. The text is not handed to the encoder, for the reason
/// [`crate::a64`] gives.
fn template(
    out: &mut String,
    at: &Context<'_>,
    func: &Func,
    block: Block,
    inst: Inst,
    label: impl Fn(Block) -> String,
) -> Result<(), Error> {
    let data = func[inst];
    let spelled = at.names.resolve(data.opcode.name());
    let Some(text) = data.symbol else {
        return Err(Error::Opcode { func: at.func_name.to_owned(), opcode: spelled.to_owned() });
    };
    let operands = &func[data.operands];
    if operands.iter().any(|operand| operand.reg.phys().is_none()) {
        return Err(Error::Virtual { func: at.func_name.to_owned(), opcode: spelled.to_owned() });
    }
    let name = |phys: rucc_target::PhysReg, fp: bool| {
        let names = if fp { &FPR_NAMES } else { &GPR_NAMES };
        names.get(usize::from(phys.number())).copied().unwrap_or("?")
    };
    let reg = |held: usize, _: char| match operands.get(held) {
        Some(op) => op.reg.phys().map_or("?", |phys| name(phys, op.class == FPR)).to_owned(),
        None => String::from("?"),
    };
    let mem = match data.mem.map(|mem| func[mem]) {
        Some(amode) => {
            let base = amode.base.and_then(|base| operands.get(usize::from(base)));
            let base = base.and_then(|op| op.reg.phys()).map_or("?", |phys| name(phys, false));
            format!("{}({base})", amode.disp)
        }
        None => String::new(),
    };
    let arms = &func[block].succs;
    let text = template_arms(at.names.resolve(text), |arm| Some(label(arms.get(arm)?.block)))
        .ok_or_else(|| Error::Encode {
            func: at.func_name.to_owned(),
            opcode: spelled.to_owned(),
            why: "it jumps to an arm its block does not have".to_owned(),
        })?;
    let filled = template_filled(&text, &mem, ToOwned::to_owned, reg);
    for line in filled.lines() {
        let _ = writeln!(out, "\t{}", line.trim_start());
    }
    Ok(())
}

/// A base register and a constant as the address the encoder takes, or `None` for a mode with
/// anything else in it, which this machine has no way to say in one instruction.
fn address(amode: &Amode, regs: &[u8]) -> Option<Addr> {
    let other = amode.index.is_some() || amode.symbol.is_some() || amode.segment.is_some();
    if other || amode.block.is_some() || amode.table.is_some() {
        return None;
    }
    let base = *regs.get(usize::from(amode.base?))?;
    Some(Addr { base, offset: Offset::Imm(i64::from(amode.disp)) })
}
