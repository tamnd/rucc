//! The parser: text back into machine functions.
//!
//! Design: `spec/10-backend.md` section 10.1.
//!
//! The other half of the round trip. What the printer wrote, this reads, and printing the
//! result gives the same bytes back. That is what makes a `--emit=mir` dump worth trusting,
//! what lets a test state a machine function directly instead of running a front end to get
//! one, and what will let the allocator be tested on inputs written by hand.
//!
//! # Forward references
//!
//! A terminator names a block further down the text, and an instruction can read a virtual
//! register that a later block writes, which is what a loop looks like once its header has
//! parameters. So a function is read in three passes over what the text said. The blocks are
//! created first, so that every label exists. Then the virtual registers are handed out in the
//! order the text writes them, which is the order the printer numbered them in, and the parser
//! checks that the numbering adds up rather than assuming it. Only then are the instructions
//! built, by which point everything either of them names exists.
//!
//! # What the reader is given
//!
//! The same register file the printer was given. Physical registers are written by name, and a
//! name is what a target's register file says it is, so reading a dump of one target's MIR
//! against another target's file is not a thing that can be made to work and is refused at the
//! first register rather than half way through.
//!
//! A virtual register says its class only where the text writes it, so reading one is what the
//! second pass is for: by the time an instruction is built, every register the function has
//! exists and knows its class, and a register that is read and never written is the error that
//! falls out of that rather than a case anybody had to look for.

use std::fmt;

use rucc_base::{Interner, Symbol};
use rucc_target::{Constraint, PhysReg, RegClass, RegFile, Role};

use crate::func::Func;
use crate::inst::{BlockCall, Mem, Opcode, Operand, Param, Reg};

/// Why a text could not be read.
///
/// One error and then nothing, rather than a list. A malformed dump is a bug in whatever wrote
/// it or a file somebody edited by hand, and in both cases the first thing that does not add up
/// is the thing worth reporting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// Which line of the text, counting from one.
    pub line: u32,
    /// What was wrong with it.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Reads every function in the text the printer writes.
///
/// Names are interned into `names`, and physical registers are the ones `regs` describes, which
/// are the two things the printer was given.
///
/// # Errors
///
/// Gives back the first thing in the text that does not add up, with the line it is on.
pub fn parse(text: &str, names: &mut Interner, regs: &RegFile) -> Result<Vec<Func>, ParseError> {
    Parser { text, pos: 0, line: 1, names, regs }.funcs()
}

/// A register as the text writes one.
///
/// A virtual register carries a class only where the text is writing the register, which is a
/// block parameter or an operand to the left of an `=`. Everywhere else the class is not in the
/// text and is the one the function already gave the register.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Written {
    /// A virtual register, by the number it was printed as.
    Virtual { number: u32, class: Option<RegClass> },
    /// A physical register, by its name, which says its class as well.
    Physical { reg: PhysReg, class: RegClass },
}

/// An operand, read but not yet resolved.
#[derive(Clone, Copy)]
struct PendingOperand {
    reg: Written,
    role: Role,
    constraint: Constraint,
}

/// A memory operand, read but not yet resolved.
#[derive(Clone, Copy, Default)]
struct PendingMem {
    base: Option<PendingOperand>,
    index: Option<PendingOperand>,
    scale: u8,
    disp: i32,
    symbol: Option<Symbol>,
}

/// One arm of a terminator, read but not yet resolved.
struct PendingCall {
    block: u32,
    args: Vec<Written>,
}

/// An instruction, read but not yet built.
struct PendingInst {
    opcode: Symbol,
    operands: Vec<PendingOperand>,
    mem: Option<PendingMem>,
    imm: Option<i64>,
    symbol: Option<Symbol>,
    succs: Vec<PendingCall>,
    line: u32,
}

/// A block, read but not yet built.
struct PendingBlock {
    number: u32,
    params: Vec<Written>,
    insts: Vec<PendingInst>,
    line: u32,
}

/// Reading one text.
struct Parser<'a, 'n> {
    text: &'a str,
    pos: usize,
    line: u32,
    names: &'n mut Interner,
    regs: &'n RegFile,
}

impl<'a> Parser<'a, '_> {
    // The text.

    fn funcs(mut self) -> Result<Vec<Func>, ParseError> {
        let mut funcs = Vec::new();
        loop {
            self.skip_blank_lines();
            if self.at_end() {
                return Ok(funcs);
            }
            funcs.push(self.func()?);
        }
    }

    fn func(&mut self) -> Result<Func, ParseError> {
        self.expect("mfunc")?;
        let name = self.symbol()?;
        self.expect("{")?;
        self.end_of_line()?;

        let mut blocks: Vec<PendingBlock> = Vec::new();
        loop {
            self.skip_blank_lines();
            if self.eat("}") {
                self.end_of_line()?;
                break;
            }
            if self.at_end() {
                return self.fail("the function is not closed");
            }
            blocks.push(self.block()?);
        }
        self.build(name, blocks)
    }

    fn block(&mut self) -> Result<PendingBlock, ParseError> {
        let line = self.line;
        let number = self.label()?;
        let mut params = Vec::new();
        if self.eat("(") {
            loop {
                params.push(self.written(true)?);
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(")")?;
        }
        self.expect(":")?;
        self.end_of_line()?;

        let mut insts = Vec::new();
        loop {
            self.spaces();
            if self.at_end() || self.at("\n") || self.at("}") || self.at_label() {
                break;
            }
            insts.push(self.inst()?);
        }
        Ok(PendingBlock { number, params, insts, line })
    }

    fn inst(&mut self) -> Result<PendingInst, ParseError> {
        let line = self.line;
        let mut operands = Vec::new();
        if self.at("%") || self.at("$") || self.peek_word() == "early" {
            loop {
                operands.push(self.operand(true)?);
                if !self.eat(",") {
                    break;
                }
            }
            self.expect("=")?;
        }
        let opcode = self.opcode()?;

        let mut inst = PendingInst {
            opcode,
            operands,
            mem: None,
            imm: None,
            symbol: None,
            succs: Vec::new(),
            line,
        };
        self.spaces();
        if !self.at("\n") && !self.at_end() {
            loop {
                self.item(&mut inst)?;
                if !self.eat(",") {
                    break;
                }
            }
        }
        self.end_of_line()?;
        Ok(inst)
    }

    /// One of the things that can appear to the right of an opcode.
    fn item(&mut self, inst: &mut PendingInst) -> Result<(), ParseError> {
        self.spaces();
        if self.at("%") || self.at("$") || self.peek_word() == "early" {
            inst.operands.push(self.operand(false)?);
            return Ok(());
        }
        if self.at("@") {
            let symbol = self.symbol()?;
            if inst.symbol.is_some() {
                return self.fail("the instruction already names a symbol");
            }
            inst.symbol = Some(symbol);
            return Ok(());
        }
        if self.at("[") {
            let mem = self.mem()?;
            if inst.mem.is_some() {
                return self.fail("the instruction already has a memory operand");
            }
            inst.mem = Some(mem);
            return Ok(());
        }
        if self.at_label() {
            let call = self.block_call()?;
            inst.succs.push(call);
            return Ok(());
        }
        let value = self.i64()?;
        if inst.imm.is_some() {
            return self.fail("the instruction already has an immediate");
        }
        inst.imm = Some(value);
        Ok(())
    }

    /// One operand: a register, and whatever is true of it besides.
    fn operand(&mut self, written: bool) -> Result<PendingOperand, ParseError> {
        let early = self.eat_word("early");
        if early && !written {
            return self.fail("only an operand an instruction writes can be early");
        }
        let reg = self.written(written)?;
        let role = match (written, early) {
            (false, _) => Role::Use,
            (true, false) => Role::Def,
            (true, true) => Role::EarlyDef,
        };
        let mut constraint = Constraint::Reg;
        if self.eat("(") {
            constraint = self.constraint()?;
            self.expect(")")?;
        }
        Ok(PendingOperand { reg, role, constraint })
    }

    fn constraint(&mut self) -> Result<Constraint, ParseError> {
        if self.at("$") {
            let Written::Physical { reg, .. } = self.written(false)? else {
                return self.fail("a register is fixed to a physical register");
            };
            return Ok(Constraint::Fixed(reg));
        }
        match self.word() {
            "any" => Ok(Constraint::Any),
            "stack" => Ok(Constraint::Stack),
            "reuse" => {
                let at = self.u32()?;
                match u8::try_from(at) {
                    Ok(at) => Ok(Constraint::Reuse(at)),
                    Err(_) => self.fail(format!("no instruction has {at} operands")),
                }
            }
            other => self.fail(format!("`{other}` is not something an operand can be")),
        }
    }

    /// One register, as the text writes one.
    ///
    /// `declared` says whether this is the place the register is written, which is where a
    /// virtual one says its class and the only place it is allowed to.
    fn written(&mut self, declared: bool) -> Result<Written, ParseError> {
        self.spaces();
        if self.eat("$") {
            let name = self.glued_word();
            return match self.regs.reg_named(name) {
                Some((class, reg)) => Ok(Written::Physical { reg, class }),
                None => self.fail(format!("this target has no register called `{name}`")),
            };
        }
        self.expect("%")?;
        let number = self.u32()?;
        if !declared {
            return Ok(Written::Virtual { number, class: None });
        }
        self.expect(":")?;
        let name = self.word();
        match self.regs.class_named(name) {
            Some(class) => Ok(Written::Virtual { number, class: Some(class) }),
            None => self.fail(format!("this target has no register class called `{name}`")),
        }
    }

    /// A memory operand, in brackets.
    ///
    /// The pieces are read as a sum rather than against a fixed shape, so the first register is
    /// the base, the second is the index, and a number is the displacement. A target whose
    /// addressing modes are narrower than that is the encoder's business, and one whose modes
    /// are wider is a reason to widen this, not a reason for the reader to be strict about a
    /// shape it cannot check anyway.
    fn mem(&mut self) -> Result<PendingMem, ParseError> {
        self.expect("[")?;
        let mut mem = PendingMem { scale: 1, ..PendingMem::default() };
        let mut negative = false;
        loop {
            self.spaces();
            if self.at("@") {
                if mem.symbol.is_some() {
                    return self.fail("an address names one symbol");
                }
                mem.symbol = Some(self.symbol()?);
            } else if self.at("%") || self.at("$") {
                let operand = PendingOperand {
                    reg: self.written(false)?,
                    role: Role::Use,
                    constraint: Constraint::Reg,
                };
                if mem.base.is_none() {
                    mem.base = Some(operand);
                } else if mem.index.is_none() {
                    mem.index = Some(operand);
                    if self.eat("*") {
                        let scale = self.u32()?;
                        match u8::try_from(scale) {
                            Ok(scale) => mem.scale = scale,
                            Err(_) => return self.fail(format!("{scale} is not a scale")),
                        }
                    }
                } else {
                    return self.fail("an address names at most two registers");
                }
            } else {
                let disp = self.i64()?;
                let disp = if negative { -disp } else { disp };
                match i32::try_from(disp) {
                    Ok(disp) => mem.disp = disp,
                    Err(_) => return self.fail(format!("{disp} is too far for a displacement")),
                }
            }
            if self.eat("+") {
                negative = false;
            } else if self.eat("-") {
                negative = true;
            } else {
                break;
            }
        }
        self.expect("]")?;
        Ok(mem)
    }

    /// One arm of a terminator.
    fn block_call(&mut self) -> Result<PendingCall, ParseError> {
        let block = self.label()?;
        let mut args = Vec::new();
        if self.eat("(") {
            loop {
                // An argument is a register being read, so it carries no class: the parameter it
                // arrives as is what declares one, and that is what it is checked against.
                args.push(self.written(false)?);
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(")")?;
        }
        Ok(PendingCall { block, args })
    }

    // Building what was read.

    fn build(&mut self, name: Symbol, pending: Vec<PendingBlock>) -> Result<Func, ParseError> {
        let mut func = Func::new(name);
        for (index, block) in pending.iter().enumerate() {
            if block.number as usize != index {
                self.line = block.line;
                return self.fail(format!(
                    "this is block {index} of the function and the text calls it block{}",
                    block.number
                ));
            }
            func.create_block();
        }
        let blocks: Vec<_> = func.blocks().collect();

        // The virtual registers, in the order the text writes them, which is the order the
        // printer numbered them in.
        let mut next = 0;
        for (block, read) in blocks.iter().zip(&pending) {
            for param in &read.params {
                match *param {
                    Written::Virtual { number, class } => {
                        self.line = read.line;
                        self.expect_number(number, next)?;
                        let Some(class) = class else {
                            return self
                                .fail(format!("%{number} arrives without saying its class"));
                        };
                        func.append_param(*block, class);
                        next += 1;
                    }
                    Written::Physical { reg, class } => {
                        func.append_given_param(*block, Param { reg: Reg::physical(reg), class });
                    }
                }
            }
            for inst in &read.insts {
                for operand in &inst.operands {
                    if operand.role == Role::Use {
                        continue;
                    }
                    if let Written::Virtual { number, class } = operand.reg {
                        self.line = inst.line;
                        self.expect_number(number, next)?;
                        let Some(class) = class else {
                            return self.fail(format!("%{number} is written without a class"));
                        };
                        func.new_vreg(class);
                        next += 1;
                    }
                }
            }
        }

        for (block, read) in blocks.iter().zip(&pending) {
            let last = read.insts.len().saturating_sub(1);
            for (at, inst) in read.insts.iter().enumerate() {
                self.line = inst.line;
                if !inst.succs.is_empty() && at != last {
                    return self.fail("only the last instruction of a block says where it goes");
                }
                // Every register is resolved before the builder exists, because resolving one
                // reads the function and the builder is holding it.
                let mut operands = Vec::new();
                for operand in &inst.operands {
                    operands.push(self.resolve(&func, operand)?);
                }
                let mem = match inst.mem {
                    Some(mem) => Some(Mem {
                        base: match mem.base {
                            Some(operand) => Some(self.resolve(&func, &operand)?),
                            None => None,
                        },
                        index: match mem.index {
                            Some(operand) => Some(self.resolve(&func, &operand)?),
                            None => None,
                        },
                        scale: mem.scale,
                        disp: mem.disp,
                        symbol: mem.symbol,
                    }),
                    None => None,
                };
                let mut builder = func.build(*block, Opcode::new(inst.opcode));
                for operand in operands {
                    builder = builder.operand(operand);
                }
                if let Some(mem) = mem {
                    builder = builder.mem(mem);
                }
                if let Some(symbol) = inst.symbol {
                    builder = builder.symbol(symbol);
                }
                if let Some(value) = inst.imm {
                    builder = builder.imm(value);
                }
                builder.finish();
            }
            let Some(terminator) = read.insts.last() else { continue };
            self.line = terminator.line;
            let mut succs = Vec::new();
            for call in &terminator.succs {
                let Some(&target) = blocks.get(call.block as usize) else {
                    return self
                        .fail(format!("block{} is branched to and never begins", call.block));
                };
                let mut args = Vec::new();
                for (at, arg) in call.args.iter().enumerate() {
                    args.push(match *arg {
                        Written::Physical { reg, .. } => Reg::physical(reg),
                        Written::Virtual { number, .. } => {
                            let class = func[target].params.get(at).map(|param| param.class);
                            let Some(class) = class else {
                                return self.fail(format!(
                                    "block{} takes {} arguments and is given more",
                                    call.block,
                                    func[target].params.len()
                                ));
                            };
                            self.virtual_reg(&func, number, class)?
                        }
                    });
                }
                if args.len() != func[target].params.len() {
                    return self.fail(format!(
                        "block{} takes {} arguments and is given {}",
                        call.block,
                        func[target].params.len(),
                        args.len()
                    ));
                }
                succs.push(BlockCall::with(target, args));
            }
            *func.succs_mut(*block) = succs;
        }
        Ok(func)
    }

    /// One operand, once every register the function has exists.
    ///
    /// A register the instruction reads has to be one something writes, and the class it is in
    /// is the one it was written as. That is checked here rather than left to a verifier,
    /// because the alternative is a function that was read successfully and means something
    /// else.
    fn resolve(&self, func: &Func, operand: &PendingOperand) -> Result<Operand, ParseError> {
        let (reg, class) = match operand.reg {
            Written::Physical { reg, class } => (Reg::physical(reg), class),
            Written::Virtual { number, class } => {
                let reg = Reg::virtual_reg(number);
                match (class, func.class_of(reg)) {
                    (Some(class), _) => (reg, class),
                    (None, Some(class)) => (reg, class),
                    (None, None) => {
                        return self.fail(format!("%{number} is read and never written"));
                    }
                }
            }
        };
        Ok(Operand { reg, class, role: operand.role, constraint: operand.constraint })
    }

    /// The register a branch argument names, checked against the class it arrives as.
    fn virtual_reg(&self, func: &Func, number: u32, class: RegClass) -> Result<Reg, ParseError> {
        let reg = Reg::virtual_reg(number);
        match func.class_of(reg) {
            None => self.fail(format!("%{number} is read and never written")),
            Some(found) if found != class => {
                self.fail(format!("%{number} is passed to a parameter of another class"))
            }
            Some(_) => Ok(reg),
        }
    }

    /// Checks that a register is numbered the way the printer numbers them.
    fn expect_number(&self, number: u32, next: u32) -> Result<(), ParseError> {
        if number == next {
            return Ok(());
        }
        self.fail(format!("this is %{next} of the function and the text calls it %{number}"))
    }

    // Words, numbers and the rest of the text.

    fn fail<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError { line: self.line, message: message.into() })
    }

    fn at(&self, text: &str) -> bool {
        self.text[self.pos..].starts_with(text)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    /// Whether a block label begins here, which is what says an instruction does not.
    fn at_label(&self) -> bool {
        let rest = self.text[self.pos..].trim_start_matches([' ', '\t']);
        let Some(rest) = rest.strip_prefix("block") else { return false };
        rest.starts_with(|c: char| c.is_ascii_digit())
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, text: &str) -> bool {
        self.spaces();
        if self.at(text) {
            self.pos += text.len();
            return true;
        }
        false
    }

    fn expect(&mut self, text: &str) -> Result<(), ParseError> {
        if self.eat(text) {
            return Ok(());
        }
        self.fail(format!("expected `{text}`"))
    }

    fn word(&mut self) -> &'a str {
        self.spaces();
        self.glued_word()
    }

    /// The word starting exactly here, with no space skipped first.
    fn glued_word(&mut self) -> &'a str {
        let start = self.pos;
        while self.peek().is_some_and(is_name_byte) {
            self.pos += 1;
        }
        &self.text[start..self.pos]
    }

    fn peek_word(&self) -> &'a str {
        let rest = self.text[self.pos..].trim_start_matches([' ', '\t']);
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .unwrap_or(rest.len());
        &rest[..end]
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if self.peek_word() != word {
            return false;
        }
        self.word();
        true
    }

    /// A `blockN` label, giving back the number.
    fn label(&mut self) -> Result<u32, ParseError> {
        self.expect("block")?;
        self.u32()
    }

    /// An `@name`.
    fn symbol(&mut self) -> Result<Symbol, ParseError> {
        self.expect("@")?;
        let name = self.glued_word();
        if name.is_empty() {
            return self.fail("a symbol has a name");
        }
        Ok(self.names.intern(name))
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        let word = self.word();
        match word.parse::<u32>() {
            Ok(number) => Ok(number),
            Err(_) => self.fail(format!("`{word}` is not a number")),
        }
    }

    fn i64(&mut self) -> Result<i64, ParseError> {
        self.spaces();
        let start = self.pos;
        if self.at("-") {
            self.pos += 1;
        }
        self.glued_word();
        let word = &self.text[start..self.pos];
        match word.parse::<i64>() {
            Ok(number) => Ok(number),
            Err(_) => self.fail(format!("`{word}` is not a number")),
        }
    }

    /// The name of an instruction, which is a word with the dots targets put in theirs.
    fn opcode(&mut self) -> Result<Symbol, ParseError> {
        let word = self.word();
        if word.is_empty() {
            return self.fail("expected an instruction");
        }
        Ok(self.names.intern(word))
    }

    fn end_of_line(&mut self) -> Result<(), ParseError> {
        self.spaces();
        if self.at_end() {
            return Ok(());
        }
        if !self.at("\n") {
            let rest = self.text[self.pos..].lines().next().unwrap_or_default();
            return self.fail(format!("`{rest}` is left over at the end of the line"));
        }
        self.pos += 1;
        self.line += 1;
        Ok(())
    }

    fn skip_blank_lines(&mut self) {
        loop {
            let held = self.pos;
            self.spaces();
            if self.at("\n") {
                self.pos += 1;
                self.line += 1;
                continue;
            }
            self.pos = held;
            return;
        }
    }
}

/// Whether the byte can be part of a name, a class, an opcode or a number.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{AFTER, BEFORE, REGS};
    use crate::print::{print, print_func};

    /// Reads the text and writes it back, which has to give the same bytes.
    fn round_trip(text: &str) {
        let mut names = Interner::new();
        let funcs = parse(text, &mut names, &REGS).expect("the fixture is what the printer writes");
        let [func] = &funcs[..] else { panic!("the text is one function") };
        assert_eq!(print_func(func, &names, &REGS), text);
    }

    /// The first thing the text says that does not add up.
    fn error(text: &str) -> String {
        let mut names = Interner::new();
        parse(text, &mut names, &REGS).expect_err("the text does not add up").to_string()
    }

    #[test]
    fn a_function_before_allocation_round_trips() {
        round_trip(BEFORE);
    }

    #[test]
    fn a_function_after_allocation_round_trips() {
        round_trip(AFTER);
    }

    #[test]
    fn two_functions_round_trip() {
        let text = format!("{BEFORE}\n{AFTER}");
        let mut names = Interner::new();
        let funcs = parse(&text, &mut names, &REGS).expect("both fixtures are readable");
        assert_eq!(funcs.len(), 2);
        assert_eq!(print(&funcs, &names, &REGS), text);
    }

    #[test]
    fn an_empty_text_is_no_functions() {
        let mut names = Interner::new();
        assert!(parse("\n\n", &mut names, &REGS).expect("nothing is readable").is_empty());
    }

    #[test]
    fn a_register_nothing_writes_is_refused() {
        let text = "mfunc @f {\nblock0:\n    x64.ret %3\n}\n";
        assert_eq!(error(text), "line 3: %3 is read and never written");
    }

    #[test]
    fn a_register_numbered_out_of_order_is_refused() {
        let text = "mfunc @f {\nblock0:\n    %1:gpr = x64.mov_ri 4\n    x64.ret %1\n}\n";
        assert_eq!(error(text), "line 3: this is %0 of the function and the text calls it %1");
    }

    #[test]
    fn a_block_numbered_out_of_order_is_refused() {
        let text = "mfunc @f {\nblock1:\n    x64.ret $rax\n}\n";
        assert_eq!(
            error(text),
            "line 2: this is block 0 of the function and the text calls it block1"
        );
    }

    #[test]
    fn a_register_the_target_does_not_have_is_refused() {
        let text = "mfunc @f {\nblock0:\n    x64.ret $r13\n}\n";
        assert_eq!(error(text), "line 3: this target has no register called `r13`");
    }

    #[test]
    fn a_class_the_target_does_not_have_is_refused() {
        let text = "mfunc @f {\nblock0:\n    %0:vec = x64.mov_ri 4\n    x64.ret $rax\n}\n";
        assert_eq!(error(text), "line 3: this target has no register class called `vec`");
    }

    #[test]
    fn an_argument_of_another_class_is_refused() {
        let text = "\
mfunc @f {
block0:
    %0:xmm = x64.movd_xr $rax
    x64.jmp block1(%0)

block1(%1:gpr):
    x64.ret $rax
}
";
        assert_eq!(error(text), "line 4: %0 is passed to a parameter of another class");
    }

    #[test]
    fn a_block_given_the_wrong_number_of_arguments_is_refused() {
        let text = "\
mfunc @f {
block0(%0:gpr):
    x64.jmp block1(%0)

block1(%1:gpr, %2:gpr):
    x64.ret $rax
}
";
        assert_eq!(error(text), "line 3: block1 takes 2 arguments and is given 1");
    }

    #[test]
    fn a_branch_to_a_block_that_never_begins_is_refused() {
        let text = "mfunc @f {\nblock0:\n    x64.jmp block4\n}\n";
        assert_eq!(error(text), "line 3: block4 is branched to and never begins");
    }

    #[test]
    fn a_branch_before_the_end_of_a_block_is_refused() {
        let text = "mfunc @f {\nblock0:\n    x64.jmp block0\n    x64.ret $rax\n}\n";
        assert_eq!(error(text), "line 3: only the last instruction of a block says where it goes");
    }

    #[test]
    fn a_read_operand_cannot_be_early() {
        let text = "mfunc @f {\nblock0:\n    x64.ret early $rax\n}\n";
        assert_eq!(error(text), "line 3: only an operand an instruction writes can be early");
    }

    #[test]
    fn a_second_immediate_is_refused() {
        let text = "mfunc @f {\nblock0:\n    x64.ret 1, 2\n}\n";
        assert_eq!(error(text), "line 3: the instruction already has an immediate");
    }

    #[test]
    fn a_function_that_does_not_close_is_refused() {
        let text = "mfunc @f {\nblock0:\n    x64.ret $rax\n";
        assert_eq!(error(text), "line 4: the function is not closed");
    }

    #[test]
    fn what_is_left_over_on_a_line_is_reported() {
        let text = "mfunc @f {\nblock0:\n    x64.ret $rax ]\n}\n";
        assert_eq!(error(text), "line 3: `]` is left over at the end of the line");
    }
}
