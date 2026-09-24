//! One AArch64 machine instruction as assembly text.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1.
//!
//! Everything about a file that is not an instruction is the same on both machines and is written
//! by [`crate::att`]: the sections, the labels, the unwind rows and the variables. What differs is
//! the instruction, and for AArch64 what an opcode is written as is `rucc_target::aarch64::written`,
//! which is the same table the encoder reads. Each argument is filled in with
//! `rucc_target::aarch64::fill` and the line is spelled by `rucc_target::aarch64::write`, so a
//! listing is written from the values an object file would be encoded from and the two cannot come
//! to say different things.
//!
//! An addressing mode here is a base register and a constant, which is every mode the rule file
//! writes today. A symbol is reached with `adrp` and a low twelve bits argument rather than through
//! a mode, so a mode with a symbol, an index or a label in it is a function this writer has not
//! been taught and is refused rather than written as something else.

use std::fmt::Write as _;

use rucc_base::Interner;
use rucc_mir::{Amode, Block, Func, Inst, defs};
use rucc_target::aarch64::{self, Addr, Arg, Mode, Offset, Operands};

use crate::Error;

/// The prefix every AArch64 opcode carries in the machine IR.
pub(crate) const PREFIX: &str = "a64.";

/// What an instruction needs from the file it is written into.
pub(crate) struct Context<'a> {
    /// The names every symbol and opcode is interned in.
    pub names: &'a Interner,
    /// What goes in front of a symbol on this object format.
    pub symbol: &'a str,
    /// The name of the function the instruction is in.
    pub func_name: &'a str,
}

/// One instruction of the machine IR, as however many instructions of the machine it is, each on
/// a line of its own.
///
/// `label` is the name the block a branch goes to is written with, which the file hands in since
/// it is the one that numbered them.
pub(crate) fn inst(
    out: &mut String,
    at: &Context<'_>,
    func: &Func,
    block: Block,
    inst: Inst,
    label: impl Fn(Block) -> String,
) -> Result<(), Error> {
    let data = func[inst];
    let spelled = at.names.resolve(data.opcode.name());
    let opcode = spelled.strip_prefix(PREFIX).unwrap_or(spelled);
    let refused = || Error::Opcode { func: at.func_name.to_owned(), opcode: spelled.to_owned() };
    let Some(written) = aarch64::written(opcode) else {
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
    let mem = match data.mem {
        Some(mem) => Some(address(&func[mem], &regs).ok_or_else(refused)?),
        None => None,
    };
    let with = Operands {
        regs: &regs,
        reads: defs(operands),
        imm: data.imm.map_or(0, |imm| func[imm].0),
        mem,
    };

    // The one name the line can carry, which is the block for a branch and the symbol for anything
    // else. A conditional branch goes to the first arm, because the layout falls into the second.
    let branches = written.iter().any(|machine| machine.args.contains(&Arg::Label));
    let symbol = if branches {
        func[block].succs.first().map(|to| label(to.block))
    } else {
        data.symbol.map(|symbol| format!("{}{}", at.symbol, at.names.resolve(symbol)))
    };

    for machine in written {
        let mut values = Vec::with_capacity(machine.args.len());
        for &arg in machine.args {
            values.push(aarch64::fill(arg, &with).map_err(|_| refused())?);
        }
        let line = aarch64::write(machine.mnemonic, &values, symbol.as_deref());
        let _ = writeln!(out, "\t{line}");
    }
    Ok(())
}

/// A base register and a constant as the address the encoder takes, or `None` for a mode with
/// anything else in it.
fn address(amode: &Amode, regs: &[u8]) -> Option<Addr> {
    if amode.index.is_some()
        || amode.symbol.is_some()
        || amode.block.is_some()
        || amode.table.is_some()
        || amode.segment.is_some()
    {
        return None;
    }
    let base = *regs.get(usize::from(amode.base?))?;
    Some(Addr { base, offset: Offset::Imm(i64::from(amode.disp)), mode: Mode::Offset })
}
