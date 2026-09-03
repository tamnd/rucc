//! The printer: machine functions as text.
//!
//! Design: `spec/10-backend.md` section 10.1, which asks that `--emit=mir` and
//! `--emit=mir-final` both round-trip.
//!
//! One form, printed before allocation and after it. Before, the registers are virtual and
//! carry the class they are drawn from, because nothing else says it. After, they are physical
//! and carry no class, because the register file says which class each register is in and a
//! second copy of that is a thing that can disagree with the first.
//!
//! ```text
//! mfunc @scale {
//! block0(%0:gpr, %1:gpr):
//!     %2:gpr = x64.mov_ri 4
//!     %3:gpr = x64.imul_rr %1, %2
//!     x64.jmp block1(%3)
//!
//! block1(%4:gpr):
//!     %5:gpr = x64.lea [%4 + %1*4 + 16]
//!     x64.ret $rax
//! }
//! ```
//!
//! What an instruction writes is to the left of the `=` and what it reads is to the right, in
//! the order the operand vector holds them, and the registers a memory operand names appear
//! only inside its brackets. So the text says the operand vector exactly, which is what lets
//! the parser rebuild it, and it does not say anything twice.
//!
//! A virtual register says its class where it is written and nowhere else, which is at a block
//! parameter or to the left of an `=`. Reading one is not the place to repeat it: the class is
//! a fact about the register rather than about the reading of it, and a text that could say it
//! twice is a text that could say it two ways.
//!
//! Virtual registers are numbered in the order they are defined rather than by the number they
//! have, for the reason `rucc-ir`'s printer gives: the text is then a fact about the shape of
//! the function rather than about which order somebody's pass happened to fill the tables in.
//! Blocks are numbered by their place in the layout for the same reason.
//!
//! Spans are not printed. Debug information has its own form, and the round trip is a claim
//! about the text rather than about the source locations behind it.

use std::fmt::Write as _;

use rucc_base::Interner;
use rucc_target::{Constraint, PhysReg, RegClass, RegFile, Role};

use crate::func::{Func, defs};
use crate::inst::{Amode, Block, BlockCall, Inst, Operand, Param, Reg};

/// Every function, as text, which is what `--emit=mir` writes.
#[must_use]
pub fn print(funcs: &[Func], names: &Interner, regs: &RegFile) -> String {
    let mut printer = Printer::new(names, regs);
    for (index, func) in funcs.iter().enumerate() {
        if index > 0 {
            printer.gap();
        }
        printer.func(func);
    }
    printer.finish()
}

/// One function, as text.
#[must_use]
pub fn print_func(func: &Func, names: &Interner, regs: &RegFile) -> String {
    let mut printer = Printer::new(names, regs);
    printer.func(func);
    printer.finish()
}

/// A function being written out.
#[derive(Debug)]
pub struct Printer<'a> {
    names: &'a Interner,
    regs: &'a RegFile,
    out: String,
    /// The number each virtual register is printed as, in print order, indexed by the number it
    /// has. `u32::MAX` for one that is read and never written, which is a function nothing
    /// should have produced and which prints as `%?` so that the text does not claim otherwise.
    numbers: Vec<u32>,
    /// The number each block is printed as, indexed by its own.
    labels: Vec<u32>,
}

impl<'a> Printer<'a> {
    /// A printer whose names are in `names` and whose registers are those of `regs`.
    #[must_use]
    pub fn new(names: &'a Interner, regs: &'a RegFile) -> Printer<'a> {
        Printer { names, regs, out: String::new(), numbers: Vec::new(), labels: Vec::new() }
    }

    /// The text written so far.
    #[must_use]
    pub fn finish(self) -> String {
        self.out
    }

    /// A blank line, which is what separates one function from the next.
    pub fn gap(&mut self) {
        self.out.push('\n');
    }

    /// One function: its name, then its blocks.
    pub fn func(&mut self, func: &Func) {
        self.number(func);
        let _ = writeln!(self.out, "mfunc @{} {{", self.names.resolve(func.name));
        for (index, block) in func.blocks().enumerate() {
            if index > 0 {
                self.out.push('\n');
            }
            self.block(func, block, index);
        }
        self.out.push_str("}\n");
    }

    /// Gives every virtual register and every block the number it is printed as.
    fn number(&mut self, func: &Func) {
        self.numbers.clear();
        self.numbers.resize(func.vregs(), u32::MAX);
        self.labels.clear();
        self.labels.resize(func.block_count(), u32::MAX);
        let mut next = 0;
        for (index, block) in func.blocks().enumerate() {
            self.labels[block.index()] = index as u32;
            for param in &func[block].params {
                self.give(param.reg, &mut next);
            }
            for inst in func.insts(block) {
                let operands = &func[func[inst].operands];
                for operand in &operands[..defs(operands)] {
                    self.give(operand.reg, &mut next);
                }
            }
        }
    }

    /// Gives one register the next number, if it is virtual and has none yet.
    fn give(&mut self, reg: Reg, next: &mut u32) {
        let Some(number) = reg.number() else { return };
        let Some(slot) = self.numbers.get_mut(number as usize) else { return };
        if *slot == u32::MAX {
            *slot = *next;
            *next += 1;
        }
    }

    /// One block: its label with its parameters, then its instructions.
    fn block(&mut self, func: &Func, block: Block, index: usize) {
        let _ = write!(self.out, "block{index}");
        let params = &func[block].params;
        if !params.is_empty() {
            self.out.push('(');
            for (at, param) in params.iter().enumerate() {
                if at > 0 {
                    self.out.push_str(", ");
                }
                self.param(*param);
            }
            self.out.push(')');
        }
        self.out.push_str(":\n");
        let last = func.terminator(block);
        for inst in func.insts(block) {
            self.inst(func, block, inst, Some(inst) == last);
        }
        // Where a block goes is on the block, so a block with nothing in it still has somewhere to
        // go, and splitting a critical edge makes exactly that: a block that is an edge and no
        // instructions. The arms go on a line of their own, since there is no last instruction to
        // put them after and printing nothing would lose them.
        if last.is_none() && !func[block].succs.is_empty() {
            let arms: Vec<String> = func[block]
                .succs
                .iter()
                .map(|succ| self.text(|printer| printer.block_call(func, succ)))
                .collect();
            let _ = writeln!(self.out, "    {}", arms.join(", "));
        }
    }

    /// One parameter, which is a register and the class it arrives in.
    fn param(&mut self, param: Param) {
        self.reg(param.reg, param.class, true);
    }

    /// One instruction, indented, on one line.
    ///
    /// The successors are printed on the terminator, which is where a reader looks for them,
    /// although the block is what holds them.
    fn inst(&mut self, func: &Func, block: Block, inst: Inst, terminator: bool) {
        let data = func[inst];
        let operands = &func[data.operands];
        let written = defs(operands);
        self.out.push_str("    ");
        for (at, operand) in operands[..written].iter().enumerate() {
            if at > 0 {
                self.out.push_str(", ");
            }
            self.operand(*operand);
        }
        if written > 0 {
            self.out.push_str(" = ");
        }
        self.out.push_str(self.names.resolve(data.opcode.name()));

        // Everything to the right of the opcode is one comma-separated list, however many
        // different kinds of thing are in it. A fixed order and one separator is what makes the
        // text unambiguous to read back without the reader having to know what the opcode is.
        let mut rest: Vec<String> = Vec::new();
        let addressed = data.mem.map(|mem| func[mem]);
        for (at, operand) in operands.iter().enumerate().skip(written) {
            if names_operand(addressed.as_ref(), at) {
                continue;
            }
            rest.push(self.text(|printer| printer.operand(*operand)));
        }
        if let Some(symbol) = data.symbol {
            rest.push(format!("@{}", self.names.resolve(symbol)));
        }
        if let Some(amode) = addressed {
            rest.push(self.text(|printer| printer.amode(operands, &amode)));
        }
        if let Some(imm) = data.imm {
            rest.push(func[imm].0.to_string());
        }
        if terminator {
            for succ in &func[block].succs {
                rest.push(self.text(|printer| printer.block_call(func, succ)));
            }
        }
        for (at, text) in rest.iter().enumerate() {
            self.out.push_str(if at > 0 { ", " } else { " " });
            self.out.push_str(text);
        }
        self.out.push('\n');
    }

    /// One operand: its register, and whatever is true of it besides.
    fn operand(&mut self, operand: Operand) {
        if operand.role == Role::EarlyDef {
            self.out.push_str("early ");
        }
        self.reg(operand.reg, operand.class, operand.role.is_def());
        match operand.constraint {
            Constraint::Reg => {}
            Constraint::Any => self.out.push_str("(any)"),
            Constraint::Stack => self.out.push_str("(stack)"),
            Constraint::Fixed(phys) => {
                self.out.push('(');
                self.phys(operand.class, phys);
                self.out.push(')');
            }
            Constraint::Reuse(at) => {
                let _ = write!(self.out, "(reuse {at})");
            }
        }
    }

    /// One register: virtual, with its class where it is being written, or physical with the
    /// name the register file gives it.
    fn reg(&mut self, reg: Reg, class: RegClass, declared: bool) {
        if let Some(phys) = reg.phys() {
            self.phys(class, phys);
            return;
        }
        match self.printed(reg) {
            Some(number) => {
                let _ = write!(self.out, "%{number}");
            }
            None => self.out.push_str("%?"),
        }
        if declared {
            let name = self.regs.class(class).map_or("?", |info| info.name);
            let _ = write!(self.out, ":{name}");
        }
    }

    /// One physical register, by the name the register file gives it.
    fn phys(&mut self, class: RegClass, reg: PhysReg) {
        let _ = write!(self.out, "${}", self.regs.name(class, reg).unwrap_or("?"));
    }

    /// The number a virtual register is printed as, or `None` for one nothing defines.
    fn printed(&self, reg: Reg) -> Option<u32> {
        let number = reg.number()?;
        match self.numbers.get(number as usize).copied() {
            Some(u32::MAX) | None => None,
            Some(number) => Some(number),
        }
    }

    /// One addressing mode, in brackets.
    fn amode(&mut self, operands: &[Operand], amode: &Amode) {
        self.out.push('[');
        let mut written = false;
        if let Some(symbol) = amode.symbol {
            let _ = write!(self.out, "@{}", self.names.resolve(symbol));
            written = true;
        }
        if let Some(operand) = amode.base.and_then(|at| operands.get(usize::from(at))) {
            if written {
                self.out.push_str(" + ");
            }
            self.reg(operand.reg, operand.class, false);
            written = true;
        }
        if let Some(operand) = amode.index.and_then(|at| operands.get(usize::from(at))) {
            if written {
                self.out.push_str(" + ");
            }
            self.reg(operand.reg, operand.class, false);
            if amode.scale != 1 {
                let _ = write!(self.out, "*{}", amode.scale);
            }
            written = true;
        }
        // A mode that names nothing at all still prints a number, because an empty pair of
        // brackets would say less than the mode does.
        if amode.disp != 0 || !written {
            if written {
                let sign = if amode.disp < 0 { '-' } else { '+' };
                let _ = write!(self.out, " {sign} {}", i64::from(amode.disp).abs());
            } else {
                let _ = write!(self.out, "{}", amode.disp);
            }
        }
        self.out.push(']');
    }

    /// One arm of a terminator: where it goes, and what it takes.
    ///
    /// The arguments carry no class, because the parameters they arrive as are declared at the
    /// block they arrive in.
    fn block_call(&mut self, func: &Func, call: &BlockCall) {
        match self.labels.get(call.block.index()).copied() {
            Some(u32::MAX) | None => self.out.push_str("block?"),
            Some(number) => {
                let _ = write!(self.out, "block{number}");
            }
        }
        if call.args.is_empty() {
            return;
        }
        self.out.push('(');
        for (at, &arg) in call.args.iter().enumerate() {
            if at > 0 {
                self.out.push_str(", ");
            }
            // Which class an argument is in is the class of the parameter it arrives as, which
            // the block it goes to is what declares. It is needed only to name a physical
            // register, which is named per class.
            let class = func[call.block]
                .params
                .get(at)
                .map_or_else(|| RegClass::new(0), |param| param.class);
            self.reg(arg, class, false);
        }
        self.out.push(')');
    }

    /// What one of the printing methods writes, on its own, for a list that is joined later.
    fn text(&mut self, write: impl FnOnce(&mut Self)) -> String {
        let held = std::mem::take(&mut self.out);
        write(self);
        std::mem::replace(&mut self.out, held)
    }
}

/// Whether the operand at that index is one an addressing mode names, and so is printed inside
/// its brackets rather than in the operand list.
fn names_operand(amode: Option<&Amode>, at: usize) -> bool {
    let Some(amode) = amode else { return false };
    let at = u8::try_from(at).ok();
    amode.base == at || amode.index == at
}

#[cfg(test)]
mod tests {
    use rucc_target::PhysReg;

    use super::*;
    use crate::fixtures::{BEFORE, REGS};
    use crate::inst::{BlockCall, Mem, Opcode};

    /// The function `BEFORE` is the text of, built by hand.
    ///
    /// Written out rather than parsed, because a printer checked against text its own parser
    /// produced is a printer checked against itself.
    fn scale() -> (Interner, Func) {
        let mut names = Interner::new();
        let gpr = REGS.class_named("gpr").expect("the fixture file has a gpr class");
        let xmm = REGS.class_named("xmm").expect("the fixture file has an xmm class");
        let rax = named("rax");
        let rdx = named("rdx");
        let mut func = Func::new(names.intern("scale"));
        let op = |names: &mut Interner, text: &str| Opcode::new(names.intern(text));

        let entry = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();

        let n = func.append_param(entry, gpr);
        let stride = func.append_param(entry, gpr);
        let four = func.new_vreg(gpr);
        let scaled = func.new_vreg(gpr);
        let opcode = op(&mut names, "x64.mov_ri");
        func.build(entry, opcode).def(four, gpr).imm(4).finish();
        let opcode = op(&mut names, "x64.imul_rr");
        func.build(entry, opcode)
            .operand(Operand::write(scaled, gpr).with(Constraint::Reuse(1)))
            .uses(stride, gpr)
            .uses(four, gpr)
            .finish();
        let opcode = op(&mut names, "x64.cmp_ri");
        func.build(entry, opcode).uses(n, gpr).imm(0).finish();
        let opcode = op(&mut names, "x64.jle");
        func.build(entry, opcode).finish();
        *func.succs_mut(entry) =
            vec![BlockCall::with(exit, vec![n]), BlockCall::with(body, vec![scaled, stride])];

        let base = func.append_param(body, gpr);
        let index = func.append_param(body, gpr);
        let addr = func.new_vreg(gpr);
        let loaded = func.new_vreg(gpr);
        let quotient = func.new_vreg(gpr);
        let remainder = func.new_vreg(gpr);
        let opcode = op(&mut names, "x64.lea");
        func.build(body, opcode)
            .def(addr, gpr)
            .mem(Mem::at(Operand::read(base, gpr)).indexed(Operand::read(index, gpr), 4).plus(16))
            .finish();
        let counter = names.intern("counter");
        let opcode = op(&mut names, "x64.mov_rm");
        func.build(body, opcode).def(loaded, gpr).mem(Mem::of(counter).plus(8)).finish();
        let opcode = op(&mut names, "x64.mov_mi");
        func.build(body, opcode).mem(Mem::at(Operand::read(addr, gpr)).plus(-4)).imm(1).finish();
        let opcode = op(&mut names, "x64.idiv_rr");
        func.build(body, opcode)
            .operand(Operand::write(quotient, gpr).with(Constraint::Fixed(rax)))
            .operand(Operand::write_early(remainder, gpr).with(Constraint::Fixed(rdx)))
            .operand(Operand::read(loaded, gpr).with(Constraint::Fixed(rax)))
            .operand(Operand::read(addr, gpr).with(Constraint::Any))
            .finish();
        let opcode = op(&mut names, "x64.cmp_rr");
        func.build(body, opcode)
            .uses(quotient, gpr)
            .operand(Operand::read(remainder, gpr).with(Constraint::Stack))
            .finish();
        let opcode = op(&mut names, "x64.jmp");
        func.build(body, opcode).finish();
        *func.succs_mut(body) = vec![BlockCall::with(exit, vec![quotient])];

        let result = func.append_param(exit, gpr);
        let moved = func.new_vreg(xmm);
        let opcode = op(&mut names, "x64.movd_xr");
        func.build(exit, opcode).def(moved, xmm).uses(result, gpr).finish();
        let opcode = op(&mut names, "x64.ret");
        func.build(exit, opcode).uses(Reg::physical(rax), gpr).finish();

        (names, func)
    }

    /// One physical register of the fixture file, by name.
    fn named(name: &str) -> PhysReg {
        REGS.reg_named(name).expect("the fixture file has that register").1
    }

    #[test]
    fn a_function_prints_as_the_fixture_says() {
        let (names, func) = scale();
        assert_eq!(print_func(&func, &names, &REGS), BEFORE);
    }

    #[test]
    fn two_functions_are_printed_with_a_blank_line_between_them() {
        let (names, func) = scale();
        let empty = Func::new(func.name);
        let text = print(&[empty, func], &names, &REGS);
        assert_eq!(text, format!("mfunc @scale {{\n}}\n\n{BEFORE}"));
    }

    #[test]
    fn a_register_nothing_writes_prints_as_one_nothing_writes() {
        let mut names = Interner::new();
        let gpr = REGS.class_named("gpr").expect("the fixture file has a gpr class");
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let missing = func.new_vreg(gpr);
        let opcode = Opcode::new(names.intern("x64.ret"));
        func.build(block, opcode).uses(missing, gpr).finish();
        assert_eq!(print_func(&func, &names, &REGS), "mfunc @f {\nblock0:\n    x64.ret %?\n}\n");
    }

    #[test]
    fn a_block_with_nothing_in_it_still_prints_where_it_goes() {
        let mut names = Interner::new();
        let gpr = REGS.class_named("gpr").expect("the fixture file has a gpr class");
        let mut func = Func::new(names.intern("f"));
        let entry = func.create_block();
        let exit = func.create_block();
        let value = func.append_param(entry, gpr);
        func.append_param(exit, gpr);
        *func.succs_mut(entry) = vec![BlockCall::with(exit, vec![value])];

        // Splitting a critical edge makes exactly this: a block that is an edge and nothing else.
        // There is no last instruction to hang the arm off, so it goes on a line of its own, and
        // printing nothing would lose the only thing the block is.
        assert_eq!(
            print_func(&func, &names, &REGS),
            "mfunc @f {\nblock0(%0:gpr):\n    block1(%0)\n\nblock1(%1:gpr):\n}\n"
        );
    }
}
