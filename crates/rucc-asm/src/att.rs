//! Machine functions as assembly text, in AT&T syntax.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1, which asks that the text path and the
//! binary path share one instruction description so they cannot disagree about what an
//! instruction is. This is the text path, and the description is `rucc_target::x86_64`.
//!
//! So there is almost nothing about x86-64 in this file. What an opcode is called, how many
//! instructions it really is, which operand each of them is given and how wide each of those is
//! written are all read out of the target. What is here is the syntax: a register carries a `%`,
//! an immediate carries a `$`, an address is a displacement in front of a parenthesised base and
//! index, and the source is written before the destination.
//!
//! Intel syntax, which section 11.1 requires as an input and which `-masm=intel` will ask for as
//! an output, is the other order, no sigils and a different spelling of an address. It is a
//! second walk over the same description rather than a second description, and it is not written
//! yet.
//!
//! # What a block is
//!
//! A label, and then the instructions in it. Where a block goes is on the block rather than on
//! its terminator, so a jump has already been made into an instruction by the block layout by
//! the time anything gets here: what is left is to give each block a name, and the name is local
//! so that it leaves no symbol behind for a debugger to show as if it were a function.
//!
//! # What the unwinder is told
//!
//! Every ELF function is wrapped in `.cfi_startproc` and `.cfi_endproc`, including the ones with
//! no rows in them. An unwinder that lands on an address with no record covering it has to give
//! up, so a leaf that never moves the stack pointer still needs a record: the one the CIE hands
//! it, which says the frame ends at `rsp+8` and the return address is the word below that, is
//! already the right answer for such a function and the empty record is how it asks for it.
//!
//! A row written after the last instruction of the last block is dropped. It would describe an
//! address at or past the end of the function, which is outside what the record covers, and the
//! usual thing to find there is an epilogue putting back a state nothing is going to read.
//!
//! # What is not written
//!
//! An opcode that is not an instruction is written as nothing. Three of them exist to hold a
//! value in a register until something reads it, which is a fact the allocator needed and the
//! machine does not, and by here it has been acted on: the register in the operand is the answer.

use std::fmt::Write as _;

use rucc_base::Interner;
use rucc_mir::{Amode, Block, CfiOp, Func, Inst, Opcode, Operand, Reach, defs};
use rucc_object::{Alias, FUNC_ALIGN, Output, Sections};
use rucc_target::x86_64::{self, Arg, Width};
use rucc_target::{PhysReg, RegClass, Segment, TargetInfo};
use rucc_tuple::Arch;

use crate::Error;
use crate::data::{Globals, Piece, Variable};
use crate::format::{Directives, binding, visibility};

/// The prefix every x86-64 opcode carries in the machine IR.
///
/// An opcode is a name and a machine IR that holds two machines' instructions would otherwise
/// have two `add_rr_32` in it. The description in `rucc-target` is indexed without it, because
/// there it is already known which machine is being described.
const PREFIX: &str = "x64.";

/// Every function and every variable, as assembly text.
///
/// The functions first and the variables after them, which is the order every toolchain writes a
/// file in and the order a person reading one expects. The second names come last, because a
/// `.set` says nothing until the thing it names has been written down.
///
/// `unwind` is whether a function is described to an unwinder, which is
/// `rucc_session::Options::unwinds` and is asked of the build rather than worked out here, so that
/// this and the byte writer cannot answer it differently for one function.
///
/// `output` is whether each function and each variable is given a section of its own and what the
/// file says it was built to have checked, which are the other things about this listing the caller
/// decides: everything else is worked out from the functions and the target.
///
/// # Errors
///
/// [`Error::Machine`] for an architecture nothing here writes, and the two internal errors for a
/// function that should not have got this far. See [`Error`].
pub fn print(
    funcs: &[Func],
    globals: &Globals,
    aliases: &[Alias],
    names: &Interner,
    target: &TargetInfo,
    unwind: bool,
    output: Output,
) -> Result<String, Error> {
    let Output { sections, property } = output;
    if target.tuple.arch() != Arch::X86_64 {
        return Err(Error::Machine { triple: target.tuple.to_string() });
    }
    let directives = Directives::of(target.object_format);
    let mut writer = Writer {
        names,
        directives,
        // Nothing outside ELF reads one of these, and the directives for the other two formats are
        // not the same ones, so a request for a table there is a request for nothing.
        unwind: unwind && directives == Directives::Elf,
        out: String::new(),
        labels: Vec::new(),
        sections,
    };
    writer.out.push_str(writer.directives.text());
    writer.out.push('\n');
    for func in funcs {
        writer.func(func)?;
    }
    for var in &globals.vars {
        writer.variable(var);
    }
    for alias in aliases {
        writer.directives.alias(&mut writer.out, alias);
    }
    writer.directives.end(&mut writer.out, property);
    Ok(writer.out)
}

/// A file being written out.
struct Writer<'a> {
    names: &'a Interner,
    directives: Directives,
    /// Whether each function is wrapped in an unwind record.
    unwind: bool,
    out: String,
    /// The number each block is written as, indexed by its own, which is its place in the layout
    /// rather than the order somebody happened to create the blocks in.
    labels: Vec<u32>,
    /// Whether each function and each variable is given a section of its own.
    sections: Sections,
}

impl Writer<'_> {
    /// One function: what the assembler is told about it, then its blocks.
    fn func(&mut self, func: &Func) -> Result<(), Error> {
        let name = self.names.resolve(func.name).to_owned();
        self.number(func);
        let binding = binding(func.binding);
        let seen = visibility(func.visibility);
        let align = func.align.unwrap_or(FUNC_ALIGN);
        self.directives.code(&mut self.out, &name, self.sections);
        // What has to be written between what the assembler is told about the function and the
        // function's own label, which is nothing at all unless a patcher was promised room in
        // front of the label. See `patch`.
        let patch =
            func.patch.map(|patch| (patch, format!("{}pfe_{name}", self.directives.local())));
        let mut ahead = String::new();
        if let Some((patch, label)) = &patch {
            let back = if self.sections.functions {
                format!("\t.section\t.text.{name}")
            } else {
                self.directives.text().to_owned()
            };
            self.directives.patchable(&mut ahead, label, &back);
            if patch.before > 0 {
                let _ = writeln!(ahead, "{label}:");
                self.pad(&mut ahead, patch.pad, patch.before);
            }
        }
        self.directives.open(&mut self.out, &name, align, binding, seen, &ahead);
        let unwind = self.unwind;
        if unwind {
            let _ = writeln!(self.out, "\t.cfi_startproc");
        }
        let end = func.cfi_end();
        for (index, block) in func.blocks().enumerate() {
            let _ = writeln!(self.out, "{}{name}_{index}:", self.directives.local());
            for inst in func.insts(block) {
                // The other half of the room, which is named here rather than laid down here: the
                // instructions it is made of are in the entry block like any others, and all that
                // is missing is somewhere for the record to point. Named at the instruction rather
                // than at the top of the block because a landing pad goes in front of it, and the
                // room a patcher writes over does not include the pad.
                if let Some((patch, label)) = &patch {
                    if patch.before == 0 && patch.after == Some(inst) {
                        let _ = writeln!(self.out, "{label}:");
                    }
                }
                self.inst(func, block, inst, &name)?;
                if unwind && Some(inst) != end {
                    for op in func.cfi_after(inst) {
                        self.cfi(op);
                    }
                }
            }
        }
        if unwind {
            let _ = writeln!(self.out, "\t.cfi_endproc");
        }
        self.directives.close(&mut self.out, &name);
        Ok(())
    }

    /// The instructions that do nothing which go in front of a function's own label.
    ///
    /// Written from the opcode rather than through the machinery every other instruction goes
    /// through, because these are the only instructions in a finished function that are not in a
    /// block and so are not instructions the function holds. The opcode is one with no operands,
    /// which is what makes writing the mnemonic and nothing else the whole of it.
    fn pad(&self, out: &mut String, pad: Opcode, count: u32) {
        let spelled = self.names.resolve(pad.name());
        let opcode = spelled.strip_prefix(PREFIX).unwrap_or(spelled);
        for _ in 0..count {
            let _ = writeln!(out, "\t{opcode}");
        }
    }

    /// One row of the unwind table, as the directive an assembler reads it as.
    ///
    /// The registers are written as numbers rather than as names, which is what gcc writes and
    /// what avoids a second spelling of a register that could disagree with the first. They are
    /// DWARF's numbers, which are not the machine's, and the one place the mapping lives is the
    /// calling convention the prologue read it out of.
    fn cfi(&mut self, op: CfiOp) {
        let _ = match op {
            CfiOp::DefCfa { reg, offset } => {
                writeln!(self.out, "\t.cfi_def_cfa {reg}, {offset}")
            }
            CfiOp::DefCfaOffset(offset) => writeln!(self.out, "\t.cfi_def_cfa_offset {offset}"),
            CfiOp::DefCfaRegister(reg) => writeln!(self.out, "\t.cfi_def_cfa_register {reg}"),
            CfiOp::Offset { reg, offset } => writeln!(self.out, "\t.cfi_offset {reg}, {offset}"),
            CfiOp::Restore(reg) => writeln!(self.out, "\t.cfi_restore {reg}"),
            CfiOp::RememberState => writeln!(self.out, "\t.cfi_remember_state"),
            CfiOp::RestoreState => writeln!(self.out, "\t.cfi_restore_state"),
        };
    }

    /// One variable: what the assembler is told about it, then its image.
    fn variable(&mut self, var: &Variable) {
        if !self.directives.variable(&mut self.out, var, self.sections) {
            return;
        }
        for piece in &var.pieces {
            self.piece(piece);
        }
        self.directives.close(&mut self.out, &var.name);
    }

    /// One piece of an image, as the directive that says it.
    ///
    /// A number is written at the width it is rather than as the bytes it is made of, because the
    /// point of a listing is to be read and `.long 258` is what a person wrote. The bytes are the
    /// same either way, which is what [`crate::data`] is for.
    fn piece(&mut self, piece: &Piece) {
        match piece {
            // `.space` rather than `.zero`, which every assembler also takes, because the
            // directive set `spec/11-asm-objects-debug.md` says this compiler's own assembler
            // reads has the one and not the other in it.
            Piece::Zero(bytes) => {
                let _ = writeln!(self.out, "\t.space\t{bytes}");
            }
            Piece::Bytes(bytes) => {
                let _ = writeln!(self.out, "\t.ascii\t\"{}\"", escape(bytes));
            }
            Piece::Scalar(bytes) => match width(bytes.len()) {
                Some(directive) => {
                    let mut value = [0u8; 16];
                    value[..bytes.len()].copy_from_slice(bytes);
                    let _ = writeln!(self.out, "\t{directive}\t{}", u128::from_le_bytes(value));
                }
                // A width no directive names, which on this machine is the eighty bit float and
                // nothing else. Its bytes are what it is.
                None => {
                    let list = bytes.iter().map(u8::to_string).collect::<Vec<_>>().join(", ");
                    let _ = writeln!(self.out, "\t.byte\t{list}");
                }
            },
            // Four and eight are the only widths that reach here, because the walk that built
            // this refused every other one rather than leave the two halves to disagree.
            Piece::Addr { symbol, addend, bytes } => {
                let directive = if *bytes == 8 { ".quad" } else { ".long" };
                let name = format!("{}{symbol}", self.directives.symbol());
                match addend {
                    0 => {
                        let _ = writeln!(self.out, "\t{directive}\t{name}");
                    }
                    _ => {
                        let sign = if *addend < 0 { '-' } else { '+' };
                        let _ = writeln!(self.out, "\t{directive}\t{name}{sign}{}", addend.abs());
                    }
                }
            }
        }
    }

    /// Gives every block the number its label carries.
    fn number(&mut self, func: &Func) {
        self.labels.clear();
        self.labels.resize(func.block_count(), u32::MAX);
        for (index, block) in func.blocks().enumerate() {
            self.labels[block.index()] = u32::try_from(index).expect("a block number");
        }
    }

    /// One instruction of the machine IR, as however many instructions of the machine it is.
    fn inst(
        &mut self,
        func: &Func,
        block: Block,
        inst: Inst,
        func_name: &str,
    ) -> Result<(), Error> {
        let data = func[inst];
        let spelled = self.names.resolve(data.opcode.name());
        let opcode = spelled.strip_prefix(PREFIX).unwrap_or(spelled);
        let Some(written) = x86_64::written(opcode) else {
            return Err(Error::Opcode { func: func_name.to_owned(), opcode: spelled.to_owned() });
        };
        let operands = &func[data.operands];
        for machine in written {
            let mut args = Vec::with_capacity(machine.args.len());
            for arg in machine.args {
                args.push(match *arg {
                    Arg::Reg(at, width) => {
                        let operand = operands[usize::from(at)];
                        self.reg(operand, width, func_name, spelled)?
                    }
                    // A whole vector register, whose name the register file holds outright. The
                    // width is asked for anyway because the one thing that reads it is the general
                    // purpose file, and a register in any other class has one name.
                    Arg::Xmm(at) => {
                        let operand = operands[usize::from(at)];
                        self.reg(operand, Width::Quad, func_name, spelled)?
                    }
                    Arg::Named(register) => format!("%{register}"),
                    // A depth on the x87 stack rather than a register, which is why the number
                    // comes from the table and not from an operand. The assembler writes the top
                    // of the stack as `%st` on its own as well, and this writes `%st(0)` for it,
                    // because one spelling for all eight is one thing fewer to know.
                    Arg::Stack(depth) => format!("%st({depth})"),
                    // The first operand read, which is where a call puts the address it goes
                    // through. Everything in front of it is a register the call writes.
                    Arg::Through => {
                        let operand = operands[defs(operands)];
                        format!("*{}", self.reg(operand, Width::Quad, func_name, spelled)?)
                    }
                    Arg::Imm => match data.imm {
                        Some(imm) => format!("${}", func[imm].0),
                        None => "$0".to_owned(),
                    },
                    Arg::Mem => match data.mem {
                        Some(mem) => self.amode(operands, &func[mem], func_name, spelled)?,
                        None => "0".to_owned(),
                    },
                    Arg::Symbol => match data.symbol {
                        Some(symbol) => {
                            format!("{}{}", self.directives.symbol(), self.names.resolve(symbol))
                        }
                        None => "0".to_owned(),
                    },
                    // Where a conditional jump goes is the first arm, because the block layout
                    // guarantees the second is the block laid out next and is fallen into. An
                    // unconditional jump has one arm and it is the same one.
                    Arg::Label => match func[block].succs.first() {
                        Some(call) => self.label(func_name, call.block),
                        None => "0".to_owned(),
                    },
                });
            }
            if args.is_empty() {
                let _ = writeln!(self.out, "\t{}", machine.mnemonic);
            } else {
                let _ = writeln!(self.out, "\t{}\t{}", machine.mnemonic, args.join(", "));
            }
        }
        Ok(())
    }

    /// One register operand, as much of it as the instruction reads or writes.
    fn reg(
        &self,
        operand: Operand,
        width: Width,
        func_name: &str,
        opcode: &str,
    ) -> Result<String, Error> {
        let Some(phys) = operand.reg.phys() else {
            return Err(Error::Virtual { func: func_name.to_owned(), opcode: opcode.to_owned() });
        };
        Ok(format!("%{}", name_of(operand.class, phys, width)))
    }

    /// One address, which is a displacement and then whichever registers it names.
    ///
    /// A symbol with no base and no index is written relative to the instruction pointer, which
    /// is how a global is reached in position independent code and is the only way this compiler
    /// reaches one. A block is written the same way, and is the address a `&&label` produces.
    fn amode(
        &self,
        operands: &[Operand],
        amode: &Amode,
        func_name: &str,
        opcode: &str,
    ) -> Result<String, Error> {
        let mut out = String::new();
        // In front of everything, which is where an assembler wants it: the segment says which
        // storage the rest of the address is counted in, so `%fs:40` reads left to right.
        match amode.segment {
            Some(Segment::Fs) => out.push_str("%fs:"),
            Some(Segment::Gs) => out.push_str("%gs:"),
            None => {}
        }
        if let Some(symbol) = amode.symbol {
            let _ = write!(out, "{}{}", self.directives.symbol(), self.names.resolve(symbol));
            // The slot rather than the thing, which the assembler is told by the suffix and not by
            // the instruction: the two are the same `movq` and differ only in what goes in the
            // four bytes, so there is nowhere else to say it. The third one is a slot as well, and
            // what it holds is an offset into a thread's own block rather than an address.
            match amode.reach {
                Reach::Itself => {}
                Reach::Table => out.push_str("@GOTPCREL"),
                Reach::Thread => out.push_str("@GOTTPOFF"),
            }
            if amode.disp != 0 {
                let sign = if amode.disp < 0 { '-' } else { '+' };
                let _ = write!(out, "{sign}{}", i64::from(amode.disp).abs());
            }
        } else if let Some(block) = amode.block {
            // A label of this function, which is written the way a symbol is and reached the way a
            // symbol is, and is neither: what the assembler puts in the four bytes is a distance it
            // works out itself, since both ends are in the section it is writing.
            out.push_str(&self.label(func_name, block));
            if amode.disp != 0 {
                let sign = if amode.disp < 0 { '-' } else { '+' };
                let _ = write!(out, "{sign}{}", i64::from(amode.disp).abs());
            }
        } else if amode.disp != 0 || (amode.base.is_none() && amode.index.is_none()) {
            // A mode that names no register at all is an absolute address, and zero is one of
            // them, so the number is written even when it is zero and there is nothing else.
            let _ = write!(out, "{}", amode.disp);
        }
        let base = amode.base.and_then(|at| operands.get(usize::from(at)));
        let index = amode.index.and_then(|at| operands.get(usize::from(at)));
        if base.is_some() || index.is_some() {
            out.push('(');
            if let Some(operand) = base {
                out.push_str(&self.reg(*operand, Width::Quad, func_name, opcode)?);
            }
            if let Some(operand) = index {
                let reg = self.reg(*operand, Width::Quad, func_name, opcode)?;
                let _ = write!(out, ",{reg},{}", amode.scale);
            }
            out.push(')');
        } else if amode.symbol.is_some() || amode.block.is_some() {
            out.push_str("(%rip)");
        }
        Ok(out)
    }

    /// The label one block of one function carries.
    fn label(&self, func_name: &str, block: Block) -> String {
        match self.labels.get(block.index()).copied() {
            Some(u32::MAX) | None => format!("{}{func_name}_?", self.directives.local()),
            Some(number) => format!("{}{func_name}_{number}", self.directives.local()),
        }
    }
}

/// The directive that writes a number that many bytes wide, and `None` for a width none does.
fn width(bytes: usize) -> Option<&'static str> {
    match bytes {
        1 => Some(".byte"),
        2 => Some(".short"),
        4 => Some(".long"),
        8 => Some(".quad"),
        _ => None,
    }
}

/// Those bytes as the inside of a string an assembler reads back as the same bytes.
///
/// Everything outside printable ASCII is written as three octal digits rather than as itself,
/// which is what keeps a string with a newline in it on one line and what stops a digit after an
/// escape from being read as part of it.
fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for byte in bytes {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(char::from(*byte)),
            _ => {
                let _ = write!(out, "\\{byte:03o}");
            }
        }
    }
    out
}

/// What one register is called, at that width, without the sigil.
///
/// The width is a general purpose register's business and nothing else's on this machine, since
/// every other class here has one name per register, which is the name the register file gives.
fn name_of(class: RegClass, reg: PhysReg, width: Width) -> &'static str {
    let named = if class == x86_64::GPR {
        x86_64::gpr_name(reg, width)
    } else {
        x86_64::REGS.name(class, reg)
    };
    named.unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_base::Interner;
    use rucc_mir::{Func, Mem, Operand, Reg};
    use rucc_object::{Binding, Place, Visibility};
    use rucc_target::x86_64::{GPR, RAX, RCX, RDX, RSP};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    /// A target of that object format, which is what decides how a symbol is spelled.
    fn target(os: Os) -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, os, Env::Gnu))
    }

    /// One function of one block, with those instructions in it, written out.
    fn write(build: impl FnOnce(&mut Func, &mut Interner)) -> String {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        build(&mut func, &mut names);
        print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("a function that was allocated")
    }

    /// Those variables, written out for that object format.
    fn data(vars: Vec<Variable>, os: Os) -> String {
        let names = Interner::new();
        print(&[], &Globals { vars }, &[], &names, &target(os), true, Output::default())
            .expect("a machine with a writer")
    }

    /// The same, with every variable given a section of its own.
    fn split(vars: Vec<Variable>, os: Os) -> String {
        let names = Interner::new();
        let sections =
            Output { sections: Sections { functions: false, data: true }, ..Output::default() };
        print(&[], &Globals { vars }, &[], &names, &target(os), true, sections)
            .expect("a machine with a writer")
    }

    /// Two functions of those names, written out with each of them given a section of its own.
    fn split_code(first: &str, second: &str, os: Os) -> String {
        let mut names = Interner::new();
        let mut funcs = Vec::new();
        for name in [first, second] {
            let mut func = Func::new(names.intern(name));
            func.create_block();
            funcs.push(func);
        }
        let sections =
            Output { sections: Sections { functions: true, data: false }, ..Output::default() };
        print(&funcs, &Globals::default(), &[], &names, &target(os), true, sections)
            .expect("a machine with a writer")
    }

    /// A four byte variable of that name, in that section, holding that image.
    fn var(name: &str, place: Place, pieces: Vec<Piece>) -> Variable {
        Variable {
            name: name.to_owned(),
            size: 4,
            align: 4,
            place,
            binding: Binding::Global,
            visibility: Visibility::Default,
            pieces,
        }
    }

    /// The instruction lines of that text, without the directives or the labels.
    fn body(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|line| line.starts_with('\t') && !line.trim_start().starts_with('.'))
            .map(|line| line.trim_start())
            .collect()
    }

    #[test]
    fn an_instruction_is_written_the_way_the_target_says_it_is() {
        let text = write(|func, names| {
            let block = func.create_block();
            let add = Opcode::new(names.intern("x64.add_rr_32"));
            func.build(block, add)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RCX), GPR))
                .finish();
        });
        // The source before the destination, which is the reverse of the operand vector, and the
        // first source not written at all, because it is the destination.
        assert_eq!(body(&text), ["addl\t%ecx, %eax"]);
    }

    #[test]
    fn an_opcode_the_machine_has_no_single_instruction_for_is_written_as_the_ones_it_has() {
        let text = write(|func, names| {
            let block = func.create_block();
            let cmp = Opcode::new(names.intern("x64.cmp_set_l_64"));
            func.build(block, cmp)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RCX), GPR))
                .operand(Operand::read(Reg::physical(RDX), GPR))
                .finish();
        });
        // Two instructions, the comparison at the width it was asked for and the set at the width
        // a set is, which is the case that says why a width is a fact about an argument.
        assert_eq!(body(&text), ["cmpq\t%rdx, %rcx", "setl\t%al"]);
    }

    #[test]
    fn an_opcode_that_is_not_an_instruction_is_written_as_nothing() {
        let text = write(|func, names| {
            let block = func.create_block();
            let ret = Opcode::new(names.intern("x64.ret_val_32"));
            func.build(block, ret).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        });
        assert_eq!(body(&text), Vec::<&str>::new());
    }

    #[test]
    fn an_address_is_a_displacement_and_then_the_registers_it_names() {
        let text = write(|func, names| {
            let block = func.create_block();
            let lea = Opcode::new(names.intern("x64.lea_64"));
            func.build(block, lea)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(
                    Mem::at(Operand::read(Reg::physical(RCX), GPR))
                        .indexed(Operand::read(Reg::physical(RDX), GPR), 4)
                        .plus(-16),
                )
                .finish();
        });
        assert_eq!(body(&text), ["leaq\t-16(%rcx,%rdx,4), %rax"]);
    }

    #[test]
    fn an_address_in_a_thread_s_own_block_names_the_segment_and_no_register() {
        let text = write(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("x64.mov_rm_64"));
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::in_segment(Segment::Fs, 40))
                .finish();
        });
        // The first line of every function this compiler protects. No base and no index, because
        // where the block begins is something only the machine knows, and the segment written in
        // front of the constant rather than behind it, which is what an assembler reads.
        assert_eq!(body(&text), ["movq\t%fs:40, %rax"]);
    }

    #[test]
    fn the_touch_a_probing_prologue_writes_is_an_immediate_and_then_an_address() {
        let text = write(|func, names| {
            let block = func.create_block();
            let touch = Opcode::new(names.intern("x64.or_mi_8"));
            func.build(block, touch)
                .imm(0)
                .mem(Mem::at(Operand::read(Reg::physical(RSP), GPR)))
                .finish();
        });
        // The only instruction this compiler writes that has a number and an address and no
        // register of its own. An inclusive or of zero, so the byte it writes is the byte that was
        // there, which is what makes it safe on a page nothing has been put in yet.
        assert_eq!(body(&text), ["orb\t$0, (%rsp)"]);
    }

    #[test]
    fn an_address_with_nothing_but_a_symbol_in_it_is_relative_to_the_instruction_pointer() {
        let text = write(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("x64.mov_rm_64"));
            let global = names.intern("counter");
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::of(global))
                .finish();
        });
        assert_eq!(body(&text), ["movq\tcounter(%rip), %rax"]);
    }

    #[test]
    fn an_address_with_a_label_in_it_is_the_label_and_is_relative_as_well() {
        let text = write(|func, names| {
            let head = func.create_block();
            let there = func.create_block();
            let lea = Opcode::new(names.intern("x64.lea_64"));
            let jump = Opcode::new(names.intern("x64.jmp_reg"));
            func.build(head, lea)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::block(there))
                .finish();
            func.build(head, jump).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
            func.build(there, Opcode::new(names.intern("x64.ret"))).finish();
        });
        // The four bytes a symbol would leave, holding a distance the assembler works out for
        // itself rather than one a relocation asks the linker for, since both ends of it are in
        // the section being written.
        assert_eq!(body(&text), ["leaq\t.Lf_1(%rip), %rax", "jmp\t*%rax", "ret"]);
    }

    #[test]
    fn an_address_that_reads_the_offset_table_says_so_on_the_symbol() {
        let text = write(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("x64.mov_rm_64"));
            let away = names.intern("away");
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::got(away))
                .finish();
        });
        // The same instruction and the same four bytes as the one above. What is different is
        // which relocation those four bytes take, and the suffix on the name is the only place
        // the assembler is told which.
        assert_eq!(body(&text), ["movq\taway@GOTPCREL(%rip), %rax"]);
    }

    #[test]
    fn an_address_that_reads_the_offset_of_a_thread_local_says_so_on_the_symbol_as_well() {
        let text = write(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("x64.mov_rm_64"));
            let away = names.intern("away");
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::thread(away))
                .finish();
        });
        // The third instruction that is the same instruction as the two above it. What comes back
        // this time is not an address at all: it is how far into a thread's own block the variable
        // sits, and what makes it an address is the addition that follows it.
        assert_eq!(body(&text), ["movq\taway@GOTTPOFF(%rip), %rax"]);
    }

    #[test]
    fn a_jump_goes_to_the_label_of_the_block_the_first_arm_names() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let first = func.create_block();
        let second = func.create_block();
        let jmp = Opcode::new(names.intern("x64.jmp"));
        func.build(first, jmp).finish();
        func.succs_mut(first).push(rucc_mir::BlockCall::to(second));
        let text = print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("a function of two blocks");
        assert!(text.contains("\tjmp\t.Lf_1\n"), "{text}");
        assert!(text.contains("\n.Lf_1:\n"), "{text}");
    }

    #[test]
    fn a_symbol_is_spelled_the_way_the_object_format_spells_one() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let call = Opcode::new(names.intern("x64.call"));
        let callee = names.intern("puts");
        func.build(block, call).symbol(callee).finish();

        let elf = print(
            std::slice::from_ref(&func),
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("elf");
        assert!(elf.contains("\tcall\tputs\n"), "{elf}");
        assert!(elf.contains("\n.Lf_0:\n"), "{elf}");

        // The underscore, which is the difference that would fail to link against every library
        // on an Apple machine rather than merely looking odd.
        let macho = print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Darwin),
            true,
            Output::default(),
        )
        .expect("mach-o");
        assert!(macho.contains("\tcall\t_puts\n"), "{macho}");
        assert!(macho.contains("\n_f:\n"), "{macho}");
        assert!(macho.contains("\nLf_0:\n"), "{macho}");
    }

    #[test]
    fn a_function_that_was_never_allocated_is_refused_rather_than_written_wrongly() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let vreg = func.new_vreg(GPR);
        let neg = Opcode::new(names.intern("x64.neg_r_32"));
        func.build(block, neg).operand(Operand::write(vreg, GPR)).finish();
        let error = print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect_err("a virtual register");
        assert_eq!(
            error,
            Error::Virtual { func: "f".to_owned(), opcode: "x64.neg_r_32".to_owned() }
        );
    }

    #[test]
    fn an_opcode_the_target_does_not_describe_is_refused() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let made_up = Opcode::new(names.intern("x64.frobnicate"));
        func.build(block, made_up).finish();
        let error = print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect_err("no such instruction");
        assert_eq!(
            error,
            Error::Opcode { func: "f".to_owned(), opcode: "x64.frobnicate".to_owned() }
        );
    }

    #[test]
    fn a_function_no_other_file_can_see_is_not_announced_to_the_linker() {
        let mut names = Interner::new();
        let mut hidden = Func::new(names.intern("hidden"));
        hidden.binding = rucc_mir::Binding::Local;
        hidden.create_block();
        let text = print(
            &[hidden],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("elf");
        // Still a symbol, and still at the alignment a function gets, because a local name is one
        // the linker keeps and does not let another file reach.
        assert!(text.contains("\nhidden:\n"), "{text}");
        assert!(text.contains("\t.type\thidden, @function\n"), "{text}");
        // What two files each defining their own `static helper` come down to.
        assert!(!text.contains(".globl"), "{text}");
    }

    #[test]
    fn a_function_that_may_lose_to_another_definition_is_written_weak() {
        let mut names = Interner::new();
        let mut shared = Func::new(names.intern("shared"));
        shared.binding = rucc_mir::Binding::Weak;
        shared.create_block();
        let text = print(
            &[shared],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("elf");
        assert!(text.contains("\t.weak\tshared\n"), "{text}");
        assert!(!text.contains(".globl"), "{text}");
    }

    /// The whole of what an assembler is told about one, and none of what it works out itself:
    /// the type and the size of the new name come from the old one, so they are not written
    /// again. gcc 16 writes exactly these two lines for the same input.
    #[test]
    fn a_second_name_is_a_binding_and_a_set_and_nothing_else() {
        let names = Interner::new();
        let aliases = [
            Alias {
                name: "b".to_owned(),
                target: "a".to_owned(),
                binding: Binding::Global,
                visibility: Visibility::Default,
            },
            Alias {
                name: "c".to_owned(),
                target: "a".to_owned(),
                binding: Binding::Weak,
                visibility: Visibility::Default,
            },
            Alias {
                name: "d".to_owned(),
                target: "a".to_owned(),
                binding: Binding::Local,
                visibility: Visibility::Default,
            },
        ];
        let vars = vec![var("a", Place::Written, vec![Piece::Scalar(vec![1, 0, 0, 0])])];
        let text = print(
            &[],
            &Globals { vars },
            &aliases,
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("a machine with a writer");
        assert!(text.contains("\t.globl\tb\n\t.set\tb,a\n"), "{text}");
        assert!(text.contains("\t.weak\tc\n\t.set\tc,a\n"), "{text}");
        // A local one is a name no directive announces, which is still an entry in the symbol
        // table and is what a `static` alias comes down to.
        assert!(text.contains("\t.set\td,a\n"), "{text}");
        assert!(!text.contains("\t.type\tb"), "the type comes from what it points at: {text}");
        assert!(!text.contains("\t.size\tb"), "and so does the size: {text}");
        // Four bytes of image and not sixteen, since three more names for one variable are three
        // more names and not three more variables.
        assert_eq!(text.matches(".long\t1").count(), 1, "{text}");
    }

    #[test]
    fn a_variable_is_a_section_a_name_and_the_bytes_between_them() {
        let text = data(
            vec![var("counter", Place::Written, vec![Piece::Scalar(vec![42, 0, 0, 0])])],
            Os::Linux,
        );
        assert!(text.contains("\t.data\n"), "{text}");
        assert!(text.contains("\t.globl\tcounter\n"), "{text}");
        assert!(text.contains("\t.p2align\t2\n"), "{text}");
        assert!(text.contains("\t.type\tcounter, @object\n"), "{text}");
        // The number at the width it is, rather than the four bytes it is made of, because a
        // listing is a thing to read and the bytes are the object's business.
        assert!(text.contains("\ncounter:\n\t.long\t42\n"), "{text}");
        assert!(text.contains("\t.size\tcounter, .-counter\n"), "{text}");
    }

    /// The flag and the type are the whole of what makes it thread-local in a listing, and they
    /// are what gcc 16.2.0 writes for `_Thread_local int counter = 42;`.
    #[test]
    fn a_thread_local_variable_is_a_section_with_the_flag_on_it_and_a_type_of_its_own() {
        let text = data(
            vec![var(
                "counter",
                Place::Thread { zero: false },
                vec![Piece::Scalar(vec![42, 0, 0, 0])],
            )],
            Os::Linux,
        );
        assert!(text.contains("\t.section\t.tdata,\"awT\",@progbits\n"), "{text}");
        assert!(text.contains("\t.type\tcounter, @tls_object\n"), "{text}");
        assert!(text.contains("\ncounter:\n\t.long\t42\n"), "{text}");
    }

    /// The other half of the pair, which is `.bss` to the one above's `.data`.
    #[test]
    fn a_thread_local_variable_with_no_image_to_carry_goes_in_the_section_that_carries_none() {
        let text = data(
            vec![var("counter", Place::Thread { zero: true }, vec![Piece::Zero(4)])],
            Os::Linux,
        );
        assert!(text.contains("\t.section\t.tbss,\"awT\",@nobits\n"), "{text}");
        assert!(text.contains("\t.type\tcounter, @tls_object\n"), "{text}");
        assert!(text.contains("\ncounter:\n\t.space\t4\n"), "{text}");
    }

    #[test]
    fn a_variable_no_other_file_can_see_is_not_announced_to_the_linker() {
        let mut hidden = var("hidden", Place::Zero, vec![Piece::Zero(4)]);
        hidden.binding = Binding::Local;
        let text = data(vec![hidden], Os::Linux);
        assert!(text.contains("\t.bss\n"), "{text}");
        assert!(text.contains("\nhidden:\n\t.space\t4\n"), "{text}");
        // The whole of what `static` at file scope means, and the one thing a reader would not
        // notice missing until two files each defined their own and the linker took one.
        assert!(!text.contains(".globl"), "{text}");
    }

    #[test]
    fn a_tentative_definition_is_a_request_rather_than_a_section_and_a_label() {
        let text = data(vec![var("x", Place::Merged, vec![Piece::Zero(4)])], Os::Linux);
        assert_eq!(text.lines().find(|line| line.contains(".comm")), Some("\t.comm\tx,4,4"));
        assert!(!text.contains("\nx:\n"), "nothing here says where it is: {text}");
    }

    /// The listing half of `-ffunction-sections`, which is the flag that makes `--gc-sections` able
    /// to drop anything: a linker can leave out a section nothing reaches and cannot leave out half
    /// of one.
    ///
    /// The empty `.text` at the top stays. It is what the file opens with either way, gcc 16 writes
    /// one under the flag too, and a section with nothing in it costs a header and confuses nobody.
    #[test]
    fn every_function_gets_a_section_of_its_own_when_that_is_what_was_asked_for() {
        let text = split_code("first", "second", Os::Linux);
        assert!(text.starts_with("\t.text\n"), "{text}");
        assert!(text.contains("\t.section\t.text.first,\"ax\",@progbits\n"), "{text}");
        assert!(text.contains("\t.section\t.text.second,\"ax\",@progbits\n"), "{text}");
        // In front of the alignment and the name rather than after them, since the padding belongs
        // to the section the function is in and a label in the wrong section is a wrong address.
        let opened = text.find(".section\t.text.first").expect("a section");
        assert!(opened < text.find("\nfirst:\n").expect("a label"), "{text}");
        // And one text section when nothing asked, which is the default.
        let plain = write(|_, _| {});
        assert!(!plain.contains(".text."), "{plain}");
    }

    /// Mach-O takes the flag and writes what it wrote before, because every Mach-O object ends
    /// with `.subsections_via_symbols` and so already tells the linker it may split a section at
    /// each symbol and drop the parts nothing reaches. Clang does the same on an Apple target.
    #[test]
    fn a_format_that_already_lets_the_linker_split_a_section_is_not_asked_to_split_it_again() {
        let text = split_code("first", "second", Os::Darwin);
        assert!(text.contains("\t.subsections_via_symbols\n"), "{text}");
        assert_eq!(text.matches(".section").count(), 1, "the one it opens with: {text}");
        let vars = vec![var("counter", Place::Written, vec![Piece::Scalar(vec![1, 0, 0, 0])])];
        assert_eq!(split(vars.clone(), Os::Darwin), data(vars, Os::Darwin));
    }

    /// The listing half of `-fdata-sections`, where the name of the section is the name of the one
    /// it came out of with the variable's name after it. That is what gcc writes, and the part in
    /// front of the dot is what a linker script and `--gc-sections` both match on.
    #[test]
    fn every_variable_gets_a_section_named_after_it_when_that_is_what_was_asked_for() {
        let vars = vec![
            var("g", Place::Written, vec![Piece::Scalar(vec![1, 0, 0, 0])]),
            var("z", Place::Zero, vec![Piece::Zero(4)]),
            var("r", Place::ReadOnly, vec![Piece::Scalar(vec![3, 0, 0, 0])]),
        ];
        let text = split(vars.clone(), Os::Linux);
        assert!(text.contains("\t.section\t.data.g,\"aw\"\n\t.globl\tg\n"), "{text}");
        assert!(text.contains("\t.section\t.bss.z,\"aw\",@nobits\n"), "{text}");
        assert!(text.contains("\t.section\t.rodata.r,\"a\"\n"), "{text}");
        // Everything else about the variable is what it was: splitting moves which section header
        // the name is in and must not change the image, the size or who can see it.
        assert!(text.contains("\ng:\n\t.long\t1\n"), "{text}");
        assert!(text.contains("\t.size\tg, .-g\n"), "{text}");
        assert!(text.contains("\t.space\t4\n"), "{text}");
        // And the flag reaches the data without reaching the code, since gcc has two flags and a
        // build that asked for one of them measured something.
        assert!(!text.contains(".text."), "{text}");
        let plain = data(vars, Os::Linux);
        assert!(plain.contains("\t.data\n") && plain.contains("\t.bss\n"), "{plain}");
        assert!(!plain.contains(".data.g"), "{plain}");
    }

    #[test]
    fn the_object_format_decides_how_a_variable_is_written_as_much_as_a_function() {
        let text = data(vec![var("x", Place::Zero, vec![Piece::Zero(4)])], Os::Darwin);
        // Mach-O has no way to put bytes in its zero filled section, so a variable that goes
        // there is asked for by size the way a tentative definition is on every format.
        assert!(text.contains("\t.zerofill\t__DATA,__bss,_x,4,2\n"), "{text}");
        let read_only = data(vec![var("x", Place::ReadOnly, vec![Piece::Zero(4)])], Os::Darwin);
        assert!(read_only.contains("\t.section\t__TEXT,__const\n"), "{read_only}");
        assert!(read_only.contains("\n_x:\n"), "the underscore, without which nothing links");
    }

    #[test]
    fn a_run_of_bytes_is_written_so_that_it_reads_back_as_the_same_bytes() {
        let bytes = Piece::Bytes(b"a\"b\\\n\0\x801".to_vec());
        let text = data(vec![var("s", Place::ReadOnly, vec![bytes])], Os::Linux);
        // Three octal digits every time, so that the digit after an escape is not read as part
        // of it, and the quote and the backslash escaped so the string ends where it should.
        assert!(text.contains("\t.ascii\t\"a\\\"b\\\\\\012\\000\\2001\"\n"), "{text}");
    }

    #[test]
    fn the_address_of_a_name_in_an_image_is_written_as_the_name() {
        let addr = Piece::Addr { symbol: "y".to_owned(), addend: 16, bytes: 8 };
        let text = data(vec![var("p", Place::Written, vec![addr])], Os::Linux);
        assert!(text.contains("\np:\n\t.quad\ty+16\n"), "{text}");
    }

    #[test]
    fn a_machine_with_no_writer_here_is_said_so_rather_than_written_as_x86_64() {
        let names = Interner::new();
        let aarch64 = TargetInfo::new(Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu));
        let error = print(&[], &Globals::default(), &[], &names, &aarch64, true, Output::default())
            .expect_err("no writer");
        assert!(matches!(error, Error::Machine { .. }), "{error:?}");
    }
}
