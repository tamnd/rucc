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
//! An addressing mode here is a base register and either a constant or an index register, which is
//! every mode this machine has for an ordinary access. The index can be shifted by the size of the
//! access and there is no room for a constant beside it. A symbol is reached with `adrp` and a low
//! twelve bits argument rather than through a mode, so a mode with a symbol or a label in it is a
//! function this writer has not been taught and is refused rather than written as something else.
//!
//! Every line is also handed to the encoder before it is written. What the assembler would reject,
//! such as a constant too wide for the instruction, is refused here with the encoder's reason
//! rather than written for the assembler to find.

use std::fmt::Write as _;

use rucc_base::Interner;
use rucc_mir::{Amode, Block, Func, Inst, defs};
use rucc_target::aarch64::{self, Addr, Arg, Extend, Mode, Offset, Operands};
use rucc_target::template::template_filled;

use crate::Error;

/// The prefix every AArch64 opcode carries in the machine IR.
pub(crate) const PREFIX: &str = "a64.";

/// What an instruction needs from the file it is written into.
pub(crate) struct Context<'a> {
    /// The names every symbol and opcode is interned in.
    pub names: &'a Interner,
    /// What goes in front of a symbol on this object format.
    pub symbol: &'a str,
    /// Which assembler the line is for, which decides how it asks for part of an address.
    pub spelling: aarch64::Spelling,
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
    if opcode == aarch64::TEMPLATE {
        return template(out, at, func, inst);
    }
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
    // A block or a table of this function is a label rather than an addressing mode, which `adr`
    // takes as its symbol.
    let near = data.mem.map(|mem| &func[mem]).and_then(|amode| match amode {
        Amode { base: None, index: None, disp: 0, block: Some(to), .. } => Some(label(*to)),
        Amode { base: None, index: None, disp: 0, table: Some(at), .. } => Some(table(*at)),
        _ => None,
    });
    let mem = match data.mem {
        Some(_) if near.is_some() => None,
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
    } else if near.is_some() {
        near
    } else {
        data.symbol.map(|symbol| format!("{}{}", at.symbol, at.names.resolve(symbol)))
    };

    for machine in written {
        let mut values = Vec::with_capacity(machine.args.len());
        for &arg in machine.args {
            values.push(aarch64::fill(arg, &with).map_err(|_| refused())?);
        }
        if let Err(why) = aarch64::encode(machine.mnemonic, &values) {
            return Err(Error::Encode {
                func: at.func_name.to_owned(),
                opcode: spelled.to_owned(),
                why: why.to_string(),
            });
        }
        let line = aarch64::write(machine.mnemonic, &values, symbol.as_deref(), at.spelling);
        let _ = writeln!(out, "\t{line}");
    }
    Ok(())
}

/// An `asm` template kept as text, as its own lines with each register it names spelled as the
/// register its operand was given.
///
/// A hole says the operand and whether it is the `w` or the `x` name of the register, and a name
/// gets the prefix this object format puts in front of every symbol. The text is not handed to the
/// encoder, since nothing reads it back into instructions, so what is wrong with it is the
/// assembler's to say.
fn template(out: &mut String, at: &Context<'_>, func: &Func, inst: Inst) -> Result<(), Error> {
    let data = func[inst];
    let spelled = at.names.resolve(data.opcode.name());
    let Some(text) = data.symbol else {
        return Err(Error::Opcode { func: at.func_name.to_owned(), opcode: spelled.to_owned() });
    };
    let operands = &func[data.operands];
    if operands.iter().any(|operand| operand.reg.phys().is_none()) {
        return Err(Error::Virtual { func: at.func_name.to_owned(), opcode: spelled.to_owned() });
    }
    let reg = |held: usize, width: char| match operands.get(held).and_then(|op| op.reg.phys()) {
        Some(phys) => format!("{width}{}", phys.number()),
        None => String::from("?"),
    };
    let filled =
        template_filled(at.names.resolve(text), "", |name| format!("{}{name}", at.symbol), reg);
    for line in filled.lines() {
        let _ = writeln!(out, "\t{}", line.trim_start());
    }
    Ok(())
}

/// A base register and a constant or an index as the address the encoder takes, or `None` for a
/// mode with anything else in it.
///
/// An index scaled by anything but a power of two, or with a constant beside it, is a mode this
/// machine has no way to say in one instruction.
fn address(amode: &Amode, regs: &[u8]) -> Option<Addr> {
    if amode.symbol.is_some()
        || amode.block.is_some()
        || amode.table.is_some()
        || amode.segment.is_some()
    {
        return None;
    }
    let base = *regs.get(usize::from(amode.base?))?;
    let offset = match amode.index {
        None => Offset::Imm(i64::from(amode.disp)),
        Some(at) if amode.disp == 0 && amode.scale.is_power_of_two() => Offset::Reg {
            reg: *regs.get(usize::from(at))?,
            extend: Extend::Uxtx,
            amount: (amode.scale > 1)
                .then(|| u8::try_from(amode.scale.trailing_zeros()).unwrap_or(0)),
        },
        Some(_) => return None,
    };
    Some(Addr { base, offset, mode: Mode::Offset })
}
