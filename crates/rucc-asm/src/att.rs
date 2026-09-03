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
//! # What is not written
//!
//! An opcode that is not an instruction is written as nothing. Three of them exist to hold a
//! value in a register until something reads it, which is a fact the allocator needed and the
//! machine does not, and by here it has been acted on: the register in the operand is the answer.

use std::fmt::Write as _;

use rucc_base::Interner;
use rucc_mir::{Amode, Block, Func, Inst, Operand};
use rucc_target::x86_64::{self, Arg, Width};
use rucc_target::{Arch, PhysReg, RegClass, TargetInfo};

use crate::Error;
use crate::format::Directives;

/// The prefix every x86-64 opcode carries in the machine IR.
///
/// An opcode is a name and a machine IR that holds two machines' instructions would otherwise
/// have two `add_rr_32` in it. The description in `rucc-target` is indexed without it, because
/// there it is already known which machine is being described.
const PREFIX: &str = "x64.";

/// Every function, as assembly text.
///
/// # Errors
///
/// [`Error::Machine`] for an architecture nothing here writes, and the two internal errors for a
/// function that should not have got this far. See [`Error`].
pub fn print(funcs: &[Func], names: &Interner, target: &TargetInfo) -> Result<String, Error> {
    if target.triple.arch != Arch::X86_64 {
        return Err(Error::Machine { triple: target.triple.to_string() });
    }
    let mut writer = Writer {
        names,
        directives: Directives::of(target.object_format),
        out: String::new(),
        labels: Vec::new(),
    };
    writer.out.push_str(writer.directives.text());
    writer.out.push('\n');
    for func in funcs {
        writer.func(func)?;
    }
    writer.directives.end(&mut writer.out);
    Ok(writer.out)
}

/// A file being written out.
struct Writer<'a> {
    names: &'a Interner,
    directives: Directives,
    out: String,
    /// The number each block is written as, indexed by its own, which is its place in the layout
    /// rather than the order somebody happened to create the blocks in.
    labels: Vec<u32>,
}

impl Writer<'_> {
    /// One function: what the assembler is told about it, then its blocks.
    fn func(&mut self, func: &Func) -> Result<(), Error> {
        let name = self.names.resolve(func.name).to_owned();
        self.number(func);
        self.directives.open(&mut self.out, &name);
        for (index, block) in func.blocks().enumerate() {
            let _ = writeln!(self.out, "{}{name}_{index}:", self.directives.local());
            for inst in func.insts(block) {
                self.inst(func, block, inst, &name)?;
            }
        }
        self.directives.close(&mut self.out, &name);
        Ok(())
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
                    Arg::Named(register) => format!("%{register}"),
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
    /// reaches one.
    fn amode(
        &self,
        operands: &[Operand],
        amode: &Amode,
        func_name: &str,
        opcode: &str,
    ) -> Result<String, Error> {
        let mut out = String::new();
        if let Some(symbol) = amode.symbol {
            let _ = write!(out, "{}{}", self.directives.symbol(), self.names.resolve(symbol));
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
        } else if amode.symbol.is_some() {
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
    use rucc_target::x86_64::{GPR, RAX, RCX, RDX};
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
        print(&[func], &names, &target(Os::Linux)).expect("a function that was allocated")
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
            let add = rucc_mir::Opcode::new(names.intern("x64.add_rr_32"));
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
            let cmp = rucc_mir::Opcode::new(names.intern("x64.cmp_set_l_64"));
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
            let ret = rucc_mir::Opcode::new(names.intern("x64.ret_val_32"));
            func.build(block, ret).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        });
        assert_eq!(body(&text), Vec::<&str>::new());
    }

    #[test]
    fn an_address_is_a_displacement_and_then_the_registers_it_names() {
        let text = write(|func, names| {
            let block = func.create_block();
            let lea = rucc_mir::Opcode::new(names.intern("x64.lea_64"));
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
    fn an_address_with_nothing_but_a_symbol_in_it_is_relative_to_the_instruction_pointer() {
        let text = write(|func, names| {
            let block = func.create_block();
            let load = rucc_mir::Opcode::new(names.intern("x64.mov_rm_64"));
            let global = names.intern("counter");
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::of(global))
                .finish();
        });
        assert_eq!(body(&text), ["movq\tcounter(%rip), %rax"]);
    }

    #[test]
    fn a_jump_goes_to_the_label_of_the_block_the_first_arm_names() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let first = func.create_block();
        let second = func.create_block();
        let jmp = rucc_mir::Opcode::new(names.intern("x64.jmp"));
        func.build(first, jmp).finish();
        func.succs_mut(first).push(rucc_mir::BlockCall::to(second));
        let text = print(&[func], &names, &target(Os::Linux)).expect("a function of two blocks");
        assert!(text.contains("\tjmp\t.Lf_1\n"), "{text}");
        assert!(text.contains("\n.Lf_1:\n"), "{text}");
    }

    #[test]
    fn a_symbol_is_spelled_the_way_the_object_format_spells_one() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let call = rucc_mir::Opcode::new(names.intern("x64.call"));
        let callee = names.intern("puts");
        func.build(block, call).symbol(callee).finish();

        let elf = print(std::slice::from_ref(&func), &names, &target(Os::Linux)).expect("elf");
        assert!(elf.contains("\tcall\tputs\n"), "{elf}");
        assert!(elf.contains("\n.Lf_0:\n"), "{elf}");

        // The underscore, which is the difference that would fail to link against every library
        // on an Apple machine rather than merely looking odd.
        let macho = print(&[func], &names, &target(Os::Darwin)).expect("mach-o");
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
        let neg = rucc_mir::Opcode::new(names.intern("x64.neg_r_32"));
        func.build(block, neg).operand(Operand::write(vreg, GPR)).finish();
        let error = print(&[func], &names, &target(Os::Linux)).expect_err("a virtual register");
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
        let made_up = rucc_mir::Opcode::new(names.intern("x64.frobnicate"));
        func.build(block, made_up).finish();
        let error = print(&[func], &names, &target(Os::Linux)).expect_err("no such instruction");
        assert_eq!(
            error,
            Error::Opcode { func: "f".to_owned(), opcode: "x64.frobnicate".to_owned() }
        );
    }

    #[test]
    fn a_machine_with_no_writer_here_is_said_so_rather_than_written_as_x86_64() {
        let names = Interner::new();
        let aarch64 = TargetInfo::new(Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu));
        let error = print(&[], &names, &aarch64).expect_err("no writer");
        assert!(matches!(error, Error::Machine { .. }), "{error:?}");
    }
}
