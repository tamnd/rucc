//! The parser: text back into a module.
//!
//! Design: `spec/08-ir.md` section 8.8.
//!
//! The other half of the round trip. What the printer wrote, this reads, and printing the
//! result gives the same bytes back. That is what makes the IR testable without the front end,
//! what makes a dump worth trusting, and what lets a fuzzer make IR directly.
//!
//! # Forward references
//!
//! A branch names a block that has not been read yet, and, less obviously, an instruction can
//! use a value defined further down the text: the blocks are printed in layout order, and
//! layout order is not required to be an order in which every definition comes before its uses.
//!
//! Blocks are easy, because a block is an index and the index is the number in the text. Values
//! are not, because creating an instruction needs the types of the values it produces, and for
//! most opcodes that type is the type of the first operand, which may be one of the values that
//! have not been read yet.
//!
//! So a function is read in two passes. The first turns the text into a list of blocks holding
//! instructions whose operands are still just the numbers they were written as. The second
//! works out the type of every value and then builds the function. Working the types out
//! terminates because the only cycles in the definition graph run through block parameters, and
//! a block parameter has its type written at the block.
//!
//! # Numbering
//!
//! Values are numbered from zero in print order and so are blocks, so building the function in
//! print order gives every value and every block the index its number in the text says. The
//! parser checks that as it goes rather than assuming it, which is what catches a text whose
//! numbering does not add up.

use std::fmt;

use rucc_base::{Idx, Interner, Symbol};
use rucc_diag::Span;
use rucc_target::{TargetInfo, Triple};

use crate::attrs::{AttrSet, Attrs, FpContract};
use crate::func::Func;
use crate::inst::{
    AsmInfo, Block, BlockCall, CallInfo, Imm, Inst, InstData, MemInfo, Meta, MetaNode, Signature,
    SwitchInfo, Value,
};
use crate::module::{
    Alias, AliasKind, DataLayout, Datum, Global, Linkage, Module, Reloc, TlsModel, Visibility,
};
use crate::{
    Extra, ExtraKind, FORMAT_VERSION, Flags, FloatPred, IntPred, MemOrder, Opcode, RmwOp, Type,
};

/// Why a module could not be read.
///
/// One error and then nothing, rather than a list. A malformed IR dump is a bug in whatever
/// wrote it or a file somebody edited by hand, and in both cases the first thing that does not
/// add up is the thing worth reporting.
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

/// Reads a module from the text the printer writes.
///
/// Names are interned into `names`, which is the same interner the printer will be given when
/// the module is written back out.
///
/// # Errors
///
/// Gives back the first thing in the text that does not add up, with the line it is on.
pub fn parse(text: &str, names: &mut Interner) -> Result<Module, ParseError> {
    Parser::new(text, names).module()
}

/// Reading one text.
struct Parser<'a, 'n> {
    text: &'a str,
    pos: usize,
    line: u32,
    names: &'n mut Interner,
    /// The highest metadata node any instruction referred to, so that a reference to one that
    /// is never defined is caught rather than left as an index into nothing.
    meta_used: Option<(u32, u32)>,
}

/// A block, read but not yet built.
struct PendingBlock<'a> {
    params: Vec<(u32, Type)>,
    insts: Vec<PendingInst<'a>>,
}

/// An instruction, read but not yet built. Every value is still the number it was written as.
struct PendingInst<'a> {
    opcode: Opcode,
    flags: Flags,
    results: Vec<u32>,
    /// The types written after the opcode, which is none when the operands say them.
    written: Vec<Type>,
    args: Vec<u32>,
    extra: PendingExtra<'a>,
    line: u32,
}

/// A branch target, read but not yet built.
struct PendingCall {
    block: u32,
    args: Vec<u32>,
}

/// The payload of an instruction, read but not yet built.
enum PendingExtra<'a> {
    None,
    /// The text of the constant, which cannot be read until the result type is known.
    Imm(&'a str),
    Symbol(Symbol),
    IntPred(IntPred),
    FloatPred(FloatPred),
    Mem(MemInfo),
    Rmw(RmwOp, MemInfo),
    Order(MemOrder),
    Targets(Vec<PendingCall>),
    Call {
        callee: Option<Symbol>,
        signature: Signature,
    },
    Switch {
        targets: Vec<PendingCall>,
        cases: Vec<&'a str>,
    },
    Asm {
        template: Symbol,
        constraints: Symbol,
        clobbers: Symbol,
        targets: Vec<PendingCall>,
    },
}

impl<'a, 'n> Parser<'a, 'n> {
    fn new(text: &'a str, names: &'n mut Interner) -> Self {
        Parser { text, pos: 0, line: 1, names, meta_used: None }
    }

    // The whole module.

    fn module(mut self) -> Result<Module, ParseError> {
        let name = self.header_name()?;
        self.expect("; format ")?;
        let version = self.u32()?;
        if version != FORMAT_VERSION {
            return self.fail(format!(
                "this build reads format {FORMAT_VERSION} and the text says format {version}"
            ));
        }
        self.end_of_line()?;

        self.expect("target triple = ")?;
        let triple = self.quoted_str()?;
        let Ok(triple) = triple.parse::<Triple>() else {
            return self.fail(format!("`{triple}` is not a target triple"));
        };
        self.end_of_line()?;

        self.expect("target datalayout = ")?;
        let layout = self.quoted_str()?;
        let Some(datalayout) = DataLayout::parse(&layout) else {
            return self.fail(format!("`{layout}` is not a data layout"));
        };
        self.end_of_line()?;

        let name = self.names.intern(&name);
        let mut module = Module::new(name, &TargetInfo::new(triple));
        module.datalayout = datalayout;

        loop {
            self.skip_blank_lines();
            if self.at_end() {
                break;
            }
            match self.peek_word() {
                "global" => self.global(&mut module)?,
                "alias" | "ifunc" => self.alias(&mut module)?,
                "func" => self.func(&mut module)?,
                _ if self.at("!") => self.meta(&mut module)?,
                other => return self.fail(format!("`{other}` does not start anything")),
            }
        }

        if let Some((used, line)) = self.meta_used {
            let count = module.counts().metadata as u32;
            if used >= count {
                self.line = line;
                return self.fail(format!("!{used} is used and never defined"));
            }
        }
        Ok(module)
    }

    /// The `; ModuleID = 'name'` line, whose name is not quoted the way everything else is.
    fn header_name(&mut self) -> Result<String, ParseError> {
        self.expect("; ModuleID = '")?;
        let start = self.pos;
        let Some(end) = self.text[start..].find('\'') else {
            return self.fail("the module name is not closed");
        };
        let name = self.text[start..start + end].to_string();
        self.pos = start + end + 1;
        self.end_of_line()?;
        Ok(name)
    }

    // Globals, aliases and metadata.

    fn global(&mut self, module: &mut Module) -> Result<(), ParseError> {
        self.expect("global")?;
        let name = self.symbol()?;
        self.expect(":")?;

        let mut global = Global::new(name, 0, 1);
        if self.eat_word("bytes") {
            global.size = self.u64()?;
            if self.eat("=") {
                self.expect("{")?;
                let mut data = Vec::new();
                loop {
                    data.push(self.datum(module)?);
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect("}")?;
                global.init = Some(module.push_data(&data));
            }
        } else {
            let ty = self.ty()?;
            self.expect("=")?;
            let text = self.imm_text()?;
            let value = self.imm(text, ty)?;
            let datum = Datum::Scalar { ty, value: module.add_imm(value) };
            global.size = datum.size(module);
            global.init = Some(module.push_data(&[datum]));
        }

        while self.eat(",") {
            match self.word() {
                "align" => global.align = self.u32()?,
                "linkage" => {
                    global.linkage = self.parenthesised(Linkage::from_name, "a linkage")?
                }
                "visibility" => {
                    global.visibility =
                        self.parenthesised(Visibility::from_name, "a visibility")?;
                }
                "tls" => {
                    global.tls =
                        Some(self.parenthesised(TlsModel::from_name, "a thread-local model")?);
                }
                "constant" => global.constant = true,
                "section" => global.section = Some(self.symbol_from_string()?),
                other => return self.fail(format!("a global has no `{other}`")),
            }
        }
        self.end_of_line()?;
        module.add_global(global);
        Ok(())
    }

    /// One piece of a global's image.
    fn datum(&mut self, module: &mut Module) -> Result<Datum, ParseError> {
        match self.peek_word() {
            "zero" => {
                self.expect("zero")?;
                Ok(Datum::Zero(self.u64()?))
            }
            "bytes" => {
                self.expect("bytes")?;
                let bytes = self.string()?;
                Ok(Datum::Bytes(module.push_bytes(&bytes)))
            }
            "addr" => {
                self.expect("addr")?;
                self.expect(".")?;
                let size = self.u32()?;
                let symbol = self.symbol()?;
                let addend = if self.eat("+") {
                    self.i64()?
                } else if self.eat("-") {
                    let amount = self.i64()?;
                    match amount.checked_neg() {
                        Some(negated) => negated,
                        None => return self.fail("that offset has no negative"),
                    }
                } else {
                    0
                };
                Ok(Datum::Addr(module.add_reloc(Reloc { symbol, addend, size })))
            }
            _ => {
                let ty = self.ty()?;
                let text = self.imm_text()?;
                let value = self.imm(text, ty)?;
                Ok(Datum::Scalar { ty, value: module.add_imm(value) })
            }
        }
    }

    fn alias(&mut self, module: &mut Module) -> Result<(), ParseError> {
        let word = self.word();
        let Some(kind) = AliasKind::from_name(word) else {
            return self.fail(format!("`{word}` is not a kind of alias"));
        };
        let name = self.symbol()?;
        self.expect("=")?;
        let target = self.symbol()?;
        let mut alias = Alias::new(name, target);
        alias.kind = kind;
        while self.eat(",") {
            match self.word() {
                "linkage" => alias.linkage = self.parenthesised(Linkage::from_name, "a linkage")?,
                "visibility" => {
                    alias.visibility = self.parenthesised(Visibility::from_name, "a visibility")?;
                }
                other => return self.fail(format!("an alias has no `{other}`")),
            }
        }
        self.end_of_line()?;
        module.add_alias(alias);
        Ok(())
    }

    fn meta(&mut self, module: &mut Module) -> Result<(), ParseError> {
        let index = self.meta_ref()?;
        let expected = module.counts().metadata as u32;
        if index.raw() != expected {
            return self.fail(format!("metadata is numbered in order and !{expected} comes next"));
        }
        self.expect("=")?;
        self.expect("tbaa")?;
        let name = self.symbol_from_string()?;
        let mut node = MetaNode { name, parent: None, offset: 0 };
        while self.eat(",") {
            match self.word() {
                "parent" => node.parent = Some(self.meta_ref()?),
                "offset" => node.offset = self.u64()?,
                other => return self.fail(format!("a metadata node has no `{other}`")),
            }
        }
        if node.parent.is_some_and(|parent| parent.raw() >= index.raw()) {
            return self.fail("a metadata node's parent comes before it");
        }
        self.end_of_line()?;
        module.add_meta(node);
        Ok(())
    }

    // Functions.

    fn func(&mut self, module: &mut Module) -> Result<(), ParseError> {
        self.expect("func")?;
        let name = self.symbol()?;
        let signature = self.signature()?;
        let mut func = Func::new(name, signature);
        while self.eat(",") {
            match self.word() {
                "linkage" => func.linkage = self.parenthesised(Linkage::from_name, "a linkage")?,
                "visibility" => {
                    func.visibility = self.parenthesised(Visibility::from_name, "a visibility")?;
                }
                "attrs" => func.attrs = self.attrs()?,
                "section" => func.section = Some(self.symbol_from_string()?),
                other => return self.fail(format!("a function has no `{other}`")),
            }
        }
        if self.eat(";") {
            self.end_of_line()?;
            module.add_func(func);
            return Ok(());
        }
        self.expect("{")?;
        self.end_of_line()?;
        let blocks = self.body()?;
        self.build(&mut func, &blocks)?;
        module.add_func(func);
        Ok(())
    }

    /// The parameter and result types, in parentheses and after an arrow.
    fn signature(&mut self) -> Result<Signature, ParseError> {
        self.expect("(")?;
        let mut signature = Signature::new();
        if !self.eat(")") {
            loop {
                if self.eat("...") {
                    signature.variadic = true;
                    break;
                }
                signature.params.push(self.ty()?);
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(")")?;
        }
        if self.eat("->") {
            if self.eat("(") {
                loop {
                    signature.returns.push(self.ty()?);
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect(")")?;
            } else {
                signature.returns.push(self.ty()?);
            }
        }
        Ok(signature)
    }

    fn attrs(&mut self) -> Result<Attrs, ParseError> {
        self.expect("(")?;
        let mut attrs = Attrs::NONE;
        loop {
            let word = self.word();
            if self.eat("=") {
                let value = self.word();
                match word {
                    "fp_contract" => match FpContract::from_name(value) {
                        Some(contract) => attrs.fp_contract = contract,
                        None => return self.fail(format!("`{value}` is not a contraction")),
                    },
                    other => return self.fail(format!("`{other}` takes no value")),
                }
            } else {
                match AttrSet::from_name(word) {
                    Some(attr) => attrs.set |= attr,
                    None => return self.fail(format!("`{word}` is not an attribute")),
                }
            }
            if !self.eat(",") {
                break;
            }
        }
        self.expect(")")?;
        Ok(attrs)
    }

    /// The blocks of a function, up to the closing brace.
    fn body(&mut self) -> Result<Vec<PendingBlock<'a>>, ParseError> {
        let mut blocks: Vec<PendingBlock<'a>> = Vec::new();
        loop {
            self.skip_blank_lines();
            if self.eat("}") {
                self.end_of_line()?;
                return Ok(blocks);
            }
            if self.at_end() {
                return self.fail("the function is not closed");
            }
            if self.peek_word().starts_with("block") {
                let number = self.block_ref()?;
                if number as usize != blocks.len() {
                    return self.fail(format!(
                        "blocks are numbered in order and block{} comes next",
                        blocks.len()
                    ));
                }
                let mut params = Vec::new();
                if self.eat("(") {
                    loop {
                        let value = self.value_ref()?;
                        self.expect(":")?;
                        params.push((value, self.ty()?));
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect(")")?;
                }
                self.expect(":")?;
                self.end_of_line()?;
                blocks.push(PendingBlock { params, insts: Vec::new() });
                continue;
            }
            let inst = self.inst()?;
            match blocks.last_mut() {
                Some(block) => block.insts.push(inst),
                None => return self.fail("an instruction before any block"),
            }
        }
    }

    /// One instruction line.
    fn inst(&mut self) -> Result<PendingInst<'a>, ParseError> {
        let line = self.line;
        let mut results = Vec::new();
        if self.peek_is("%") {
            results = self.value_list()?;
            self.expect("=")?;
        }

        let word = self.word();
        let Some(opcode) = Opcode::from_name(word) else {
            return self.fail(format!("`{word}` is not an opcode"));
        };
        let (written, flags) = self.suffixes()?;

        let mut args = Vec::new();
        let extra = match opcode.extra_kind() {
            ExtraKind::None => {
                args = self.value_list()?;
                PendingExtra::None
            }
            ExtraKind::Imm => PendingExtra::Imm(self.imm_text()?),
            ExtraKind::Symbol => {
                let symbol = self.symbol()?;
                if self.eat("(") {
                    args = self.value_list()?;
                    self.expect(")")?;
                }
                PendingExtra::Symbol(symbol)
            }
            ExtraKind::IntPred => {
                let word = self.word();
                let Some(pred) = IntPred::from_name(word) else {
                    return self.fail(format!("`{word}` is not an integer comparison"));
                };
                args = self.value_list()?;
                PendingExtra::IntPred(pred)
            }
            ExtraKind::FloatPred => {
                let word = self.word();
                let Some(pred) = FloatPred::from_name(word) else {
                    return self.fail(format!("`{word}` is not a floating point comparison"));
                };
                args = self.value_list()?;
                PendingExtra::FloatPred(pred)
            }
            ExtraKind::Mem => {
                if matches!(opcode, Opcode::Store | Opcode::AtomicStore) {
                    // A store reads left to right like the assignment it came from.
                    let value = self.value_ref()?;
                    self.expect("->")?;
                    args = vec![value, self.value_ref()?];
                } else {
                    args = self.value_list()?;
                }
                PendingExtra::Mem(self.mem()?)
            }
            ExtraKind::Rmw => {
                let word = self.word();
                let Some(op) = RmwOp::from_name(word) else {
                    return self.fail(format!("`{word}` is not a read-modify-write"));
                };
                args = self.value_list()?;
                PendingExtra::Rmw(op, self.mem()?)
            }
            ExtraKind::Order => {
                let word = self.word();
                let Some(order) = MemOrder::from_name(word) else {
                    return self.fail(format!("`{word}` is not an ordering"));
                };
                PendingExtra::Order(order)
            }
            ExtraKind::Targets => {
                args = self.value_list()?;
                if !args.is_empty() {
                    self.expect(",")?;
                }
                let mut targets = Vec::new();
                loop {
                    targets.push(self.block_call()?);
                    if !self.eat(",") {
                        break;
                    }
                }
                PendingExtra::Targets(targets)
            }
            ExtraKind::Call => {
                let callee = if self.peek_is("@") {
                    Some(self.symbol()?)
                } else {
                    args.push(self.value_ref()?);
                    None
                };
                self.expect("(")?;
                args.extend(self.value_list()?);
                self.expect(")")?;
                self.expect(":")?;
                PendingExtra::Call { callee, signature: self.signature()? }
            }
            ExtraKind::Switch => {
                args = self.value_list()?;
                let mut targets = Vec::new();
                let mut cases = Vec::new();
                if self.eat(",") {
                    targets.push(self.block_call()?);
                    self.expect(",")?;
                    self.expect("[")?;
                    if !self.eat("]") {
                        loop {
                            cases.push(self.imm_text()?);
                            self.expect("=>")?;
                            targets.push(self.block_call()?);
                            if !self.eat(",") {
                                break;
                            }
                        }
                        self.expect("]")?;
                    }
                }
                PendingExtra::Switch { targets, cases }
            }
            ExtraKind::Asm => {
                let template = self.symbol_from_string()?;
                self.expect(",")?;
                let constraints = self.symbol_from_string()?;
                self.expect(",")?;
                let clobbers = self.symbol_from_string()?;
                self.expect("(")?;
                args = self.value_list()?;
                self.expect(")")?;
                let mut targets = Vec::new();
                if self.eat(",") {
                    self.expect("labels")?;
                    self.expect("[")?;
                    loop {
                        targets.push(self.block_call()?);
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect("]")?;
                }
                PendingExtra::Asm { template, constraints, clobbers, targets }
            }
        };
        self.end_of_line()?;
        Ok(PendingInst { opcode, flags, results, written, args, extra, line })
    }

    /// The dotted things after an opcode: the result types, then the flags.
    fn suffixes(&mut self) -> Result<(Vec<Type>, Flags), ParseError> {
        let mut written = Vec::new();
        let mut flags = Flags::NONE;
        while self.at(".") {
            self.pos += 1;
            if self.at("(") {
                self.pos += 1;
                loop {
                    written.push(self.ty()?);
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect(")")?;
                continue;
            }
            let word = self.glued_word();
            if let Some(flag) = Flags::from_name(word) {
                flags |= flag;
            } else if let Some(ty) = Type::parse(word) {
                written.push(ty);
            } else {
                return self.fail(format!("`{word}` is neither a type nor a flag"));
            }
        }
        Ok((written, flags))
    }

    /// What an access carries beyond its address.
    fn mem(&mut self) -> Result<MemInfo, ParseError> {
        let mut info = MemInfo { size: 0, align: 1, order: MemOrder::NotAtomic, tbaa: None };
        while self.eat(",") {
            let word = self.word();
            match word {
                "size" => info.size = self.u64()?,
                "align" => info.align = self.u32()?,
                "tbaa" => info.tbaa = Some(self.meta_ref()?),
                _ => match MemOrder::from_name(word) {
                    Some(order) => info.order = order,
                    None => return self.fail(format!("an access has no `{word}`")),
                },
            }
        }
        Ok(info)
    }

    /// A branch target and the values it passes.
    fn block_call(&mut self) -> Result<PendingCall, ParseError> {
        let block = self.block_ref()?;
        let mut args = Vec::new();
        if self.eat("(") {
            args = self.value_list()?;
            self.expect(")")?;
        }
        Ok(PendingCall { block, args })
    }

    // Building the function.

    /// Works out every value's type, then creates the blocks and instructions in print order.
    fn build(&mut self, func: &mut Func, blocks: &[PendingBlock<'a>]) -> Result<(), ParseError> {
        let count = self.check_numbering(blocks)?;
        let types = self.value_types(blocks, count)?;

        let mut next = 0;
        for pending in blocks {
            let block = func.create_block();
            for &(number, ty) in &pending.params {
                if number != next {
                    self.line = self.line_of(blocks, number);
                    return self
                        .fail(format!("values are numbered in order and %{next} comes next"));
                }
                func.append_param(block, ty);
                next += 1;
            }
            for inst in &pending.insts {
                if inst.results.first().is_some_and(|&first| first != next) {
                    self.line = inst.line;
                    return self
                        .fail(format!("values are numbered in order and %{next} comes next"));
                }
                let built = self.build_inst(func, inst, &types)?;
                func.append_inst(block, built);
                next += inst.results.len() as u32;
            }
        }
        Ok(())
    }

    /// Checks that every number the text uses is one the text also defines.
    ///
    /// The answer is how many values there are, which is what everything after this is sized by.
    fn check_numbering(&mut self, blocks: &[PendingBlock<'a>]) -> Result<u32, ParseError> {
        let mut count = 0;
        for pending in blocks {
            count += pending.params.len();
            for inst in &pending.insts {
                count += inst.results.len();
            }
        }
        let count = count as u32;
        let total = blocks.len() as u32;
        for pending in blocks {
            for inst in &pending.insts {
                self.line = inst.line;
                for &arg in &inst.args {
                    if arg >= count {
                        return self.fail(format!("%{arg} is used and never defined"));
                    }
                }
                for call in inst_calls(inst) {
                    if call.block >= total {
                        return self.fail(format!("block{} is used and never defined", call.block));
                    }
                    for &arg in &call.args {
                        if arg >= count {
                            return self.fail(format!("%{arg} is used and never defined"));
                        }
                    }
                }
            }
        }
        Ok(count)
    }

    /// The type of every value, worked out from the ones the text writes down.
    ///
    /// A block parameter has its type at the block and an instruction either has its written
    /// after the opcode or takes it from an operand, so this is a fixed point over a graph whose
    /// only cycles run through parameters. It converges in as many rounds as the longest chain
    /// of instructions each taking its type from the one below it in the text.
    fn value_types(
        &mut self,
        blocks: &[PendingBlock<'a>],
        count: u32,
    ) -> Result<Vec<Option<Type>>, ParseError> {
        let mut types = vec![None; count as usize];
        for pending in blocks {
            for &(number, ty) in &pending.params {
                types[number as usize] = Some(ty);
            }
        }
        loop {
            let mut progress = false;
            for pending in blocks {
                for inst in &pending.insts {
                    let Some(&first) = inst.results.first() else { continue };
                    if types[first as usize].is_some() {
                        continue;
                    }
                    let Some(resolved) = result_types(inst, &types) else { continue };
                    if resolved.len() != inst.results.len() {
                        self.line = inst.line;
                        return self.fail(format!(
                            "{} produces {} values and the text names {}",
                            inst.opcode.name(),
                            resolved.len(),
                            inst.results.len()
                        ));
                    }
                    for (&number, ty) in inst.results.iter().zip(resolved) {
                        types[number as usize] = Some(ty);
                    }
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }
        for pending in blocks {
            for inst in &pending.insts {
                if inst.results.first().is_some_and(|&first| types[first as usize].is_none()) {
                    self.line = inst.line;
                    return self.fail(format!(
                        "nothing in the text says what {} produces",
                        inst.opcode.name()
                    ));
                }
            }
        }
        Ok(types)
    }

    fn build_inst(
        &mut self,
        func: &mut Func,
        pending: &PendingInst<'a>,
        types: &[Option<Type>],
    ) -> Result<Inst, ParseError> {
        let results: Vec<Type> = pending
            .results
            .iter()
            .map(|&number| types[number as usize].unwrap_or(Type::VOID))
            .collect();
        let args: Vec<Value> =
            pending.args.iter().map(|&number| Value::from_usize(number as usize)).collect();
        let arg_list = func.push_values(&args);

        let extra = match &pending.extra {
            PendingExtra::None => Extra::None,
            PendingExtra::Imm(text) => {
                let ty = results.first().copied().unwrap_or(Type::VOID);
                self.line = pending.line;
                let imm = self.imm(text, ty)?;
                Extra::Imm(func.add_imm(imm))
            }
            PendingExtra::Symbol(symbol) => Extra::Symbol(*symbol),
            PendingExtra::IntPred(pred) => Extra::IntPred(*pred),
            PendingExtra::FloatPred(pred) => Extra::FloatPred(*pred),
            PendingExtra::Mem(info) => Extra::Mem(func.add_mem(*info)),
            PendingExtra::Rmw(op, info) => Extra::Rmw(*op, func.add_mem(*info)),
            PendingExtra::Order(order) => Extra::Order(*order),
            PendingExtra::Targets(targets) => {
                let calls = build_calls(func, targets);
                Extra::Targets(func.push_block_calls(&calls))
            }
            PendingExtra::Call { callee, signature } => {
                let sig = func.add_signature(signature.clone());
                Extra::Call(func.add_call(CallInfo { callee: *callee, signature: sig }))
            }
            PendingExtra::Switch { targets, cases } => {
                let ty = pending
                    .args
                    .first()
                    .and_then(|&number| types[number as usize])
                    .unwrap_or(Type::VOID);
                self.line = pending.line;
                let mut imms = Vec::with_capacity(cases.len());
                for case in cases {
                    imms.push(self.imm(case, ty)?);
                }
                let calls = build_calls(func, targets);
                let targets = func.push_block_calls(&calls);
                let cases = func.push_imms(&imms);
                Extra::Switch(func.add_switch(SwitchInfo { targets, cases }))
            }
            PendingExtra::Asm { template, constraints, clobbers, targets } => {
                let calls = build_calls(func, targets);
                let targets = func.push_block_calls(&calls);
                Extra::Asm(func.add_asm(AsmInfo {
                    template: *template,
                    constraints: *constraints,
                    clobbers: *clobbers,
                    targets,
                }))
            }
        };

        let data = InstData {
            opcode: pending.opcode,
            flags: pending.flags,
            args: arg_list,
            extra,
            ..InstData::new(pending.opcode)
        };
        Ok(func.create_inst(data, &results, Span::DUMMY))
    }

    /// The line a value number is defined on, for a message about the numbering.
    fn line_of(&self, blocks: &[PendingBlock<'a>], number: u32) -> u32 {
        for pending in blocks {
            for inst in &pending.insts {
                if inst.results.contains(&number) {
                    return inst.line;
                }
            }
        }
        self.line
    }

    // Tokens.

    /// A constant, read as the type it is a constant of.
    fn imm(&mut self, text: &str, ty: Type) -> Result<Imm, ParseError> {
        let scalar = if ty.is_vector() { ty.lane() } else { ty };
        if scalar.is_int() {
            match parse_i128(text) {
                Some(value) => Ok(Imm::int(value, scalar)),
                None => self.fail(format!("`{text}` is not an {scalar}")),
            }
        } else {
            match text.strip_prefix("0x").and_then(|rest| u128::from_str_radix(rest, 16).ok()) {
                Some(bits) => Ok(Imm::from_bits(bits)),
                None => self.fail(format!("`{text}` is not the bits of a {scalar}")),
            }
        }
    }

    /// The text of a constant, which is a sign, digits, and the letters a hexadecimal one has.
    fn imm_text(&mut self) -> Result<&'a str, ParseError> {
        self.spaces();
        let start = self.pos;
        if self.at("-") {
            self.pos += 1;
        }
        while self.peek().is_some_and(|byte| byte.is_ascii_alphanumeric()) {
            self.pos += 1;
        }
        if self.pos == start {
            return self.fail("a constant was expected");
        }
        Ok(&self.text[start..self.pos])
    }

    fn ty(&mut self) -> Result<Type, ParseError> {
        let word = self.word();
        match Type::parse(word) {
            Some(ty) => Ok(ty),
            None => self.fail(format!("`{word}` is not a type")),
        }
    }

    /// A `%n`, giving back the number.
    fn value_ref(&mut self) -> Result<u32, ParseError> {
        self.expect("%")?;
        self.u32()
    }

    /// A run of `%n` separated by commas, stopping at the first comma not followed by one.
    fn value_list(&mut self) -> Result<Vec<u32>, ParseError> {
        let mut values = Vec::new();
        if !self.peek_is("%") {
            return Ok(values);
        }
        loop {
            values.push(self.value_ref()?);
            let save = self.pos;
            if !self.eat(",") {
                return Ok(values);
            }
            if !self.peek_is("%") {
                self.pos = save;
                return Ok(values);
            }
        }
    }

    /// A `blockN`, giving back the number.
    fn block_ref(&mut self) -> Result<u32, ParseError> {
        let word = self.word();
        match word.strip_prefix("block").and_then(parse_u32) {
            Some(number) => Ok(number),
            None => self.fail(format!("`{word}` is not a block")),
        }
    }

    /// A `!n`, remembering it so that one nothing defines is reported.
    fn meta_ref(&mut self) -> Result<Meta, ParseError> {
        self.expect("!")?;
        let index = self.u32()?;
        let seen = self.meta_used.is_none_or(|(used, _)| index > used);
        if seen {
            self.meta_used = Some((index, self.line));
        }
        Ok(Idx::from_usize(index as usize))
    }

    /// An `@name`, interned.
    fn symbol(&mut self) -> Result<Symbol, ParseError> {
        self.expect("@")?;
        let start = self.pos;
        while self.peek().is_some_and(is_name_byte) {
            self.pos += 1;
        }
        if self.pos == start {
            return self.fail("a name was expected");
        }
        let name = &self.text[start..self.pos];
        Ok(self.names.intern(name))
    }

    /// A quoted string, interned, which is what a section and a template are.
    fn symbol_from_string(&mut self) -> Result<Symbol, ParseError> {
        let text = self.quoted_str()?;
        Ok(self.names.intern(&text))
    }

    /// A quoted string that has to be text rather than arbitrary bytes.
    fn quoted_str(&mut self) -> Result<String, ParseError> {
        let bytes = self.string()?;
        match String::from_utf8(bytes) {
            Ok(text) => Ok(text),
            Err(_) => self.fail("that string is not text"),
        }
    }

    /// A quoted string, which may hold any bytes at all.
    fn string(&mut self) -> Result<Vec<u8>, ParseError> {
        self.expect("\"")?;
        let mut bytes = Vec::new();
        loop {
            let Some(byte) = self.peek() else {
                return self.fail("that string is not closed");
            };
            self.pos += 1;
            match byte {
                b'"' => return Ok(bytes),
                b'\n' => return self.fail("that string is not closed"),
                b'\\' => {
                    let escape = match self.peek() {
                        Some(b'"') => b'"',
                        Some(b'\\') => b'\\',
                        _ => {
                            let hex = self.text.get(self.pos..self.pos + 2);
                            match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                                Some(byte) => {
                                    self.pos += 2;
                                    bytes.push(byte);
                                    continue;
                                }
                                None => return self.fail("that is not an escape"),
                            }
                        }
                    };
                    self.pos += 1;
                    bytes.push(escape);
                }
                _ => bytes.push(byte),
            }
        }
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        let word = self.word();
        match parse_u32(word) {
            Some(number) => Ok(number),
            None => self.fail(format!("`{word}` is not a number")),
        }
    }

    fn u64(&mut self) -> Result<u64, ParseError> {
        let word = self.word();
        match parse_u64(word) {
            Some(number) => Ok(number),
            None => self.fail(format!("`{word}` is not a number")),
        }
    }

    fn i64(&mut self) -> Result<i64, ParseError> {
        let word = self.word();
        match parse_u64(word).and_then(|number| i64::try_from(number).ok()) {
            Some(number) => Ok(number),
            None => self.fail(format!("`{word}` is not an offset")),
        }
    }

    /// One of the keyword parenthesised after a name, as in `linkage(internal)`.
    fn parenthesised<T>(
        &mut self,
        from_name: impl Fn(&str) -> Option<T>,
        what: &str,
    ) -> Result<T, ParseError> {
        self.expect("(")?;
        let word = self.word();
        let Some(value) = from_name(word) else {
            return self.fail(format!("`{word}` is not {what}"));
        };
        self.expect(")")?;
        Ok(value)
    }

    // The cursor.

    fn fail<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError { line: self.line, message: message.into() })
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn at(&self, text: &str) -> bool {
        self.text[self.pos..].starts_with(text)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    /// Whether that text comes next, ignoring the spaces in front of it.
    fn peek_is(&self, text: &str) -> bool {
        self.text[self.pos..].trim_start_matches([' ', '\t']).starts_with(text)
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
        self.fail(format!("`{text}` was expected"))
    }

    /// A word of letters, digits and underscores, after the spaces in front of it.
    fn word(&mut self) -> &'a str {
        self.spaces();
        self.glued_word()
    }

    /// The same, with nothing skipped, which is what a suffix after a dot is.
    fn glued_word(&mut self) -> &'a str {
        let start = self.pos;
        while self.peek().is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            self.pos += 1;
        }
        &self.text[start..self.pos]
    }

    /// The next word without consuming it, for deciding what a line is.
    fn peek_word(&self) -> &'a str {
        let rest = self.text[self.pos..].trim_start_matches([' ', '\t']);
        let end =
            rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
        &rest[..end]
    }

    /// Whether that word comes next, consuming it if so.
    fn eat_word(&mut self, word: &str) -> bool {
        if self.peek_word() == word {
            self.word();
            return true;
        }
        false
    }

    fn end_of_line(&mut self) -> Result<(), ParseError> {
        self.spaces();
        if self.at_end() {
            return Ok(());
        }
        if self.at("\n") {
            self.pos += 1;
            self.line += 1;
            return Ok(());
        }
        let rest = &self.text[self.pos..];
        let end = rest.find('\n').unwrap_or(rest.len());
        self.fail(format!("`{}` is left over at the end of the line", &rest[..end]))
    }

    fn skip_blank_lines(&mut self) {
        loop {
            let save = self.pos;
            self.spaces();
            if self.at("\n") {
                self.pos += 1;
                self.line += 1;
            } else {
                self.pos = save;
                return;
            }
        }
    }
}

/// Every branch target an instruction has, whichever payload holds them.
fn inst_calls<'p>(inst: &'p PendingInst<'_>) -> &'p [PendingCall] {
    match &inst.extra {
        PendingExtra::Targets(targets)
        | PendingExtra::Switch { targets, .. }
        | PendingExtra::Asm { targets, .. } => targets,
        _ => &[],
    }
}

/// The branch targets of an instruction, with their arguments put in the function's pool.
fn build_calls(func: &mut Func, targets: &[PendingCall]) -> Vec<BlockCall> {
    targets
        .iter()
        .map(|call| {
            let args: Vec<Value> =
                call.args.iter().map(|&number| Value::from_usize(number as usize)).collect();
            BlockCall {
                block: Block::from_usize(call.block as usize),
                args: func.push_values(&args),
            }
        })
        .collect()
}

/// What an instruction produces, or `None` while an operand's type is still unknown.
///
/// This is the reading half of the rule the printer writes by, which is why the two of them
/// name the same opcodes: a type is in the text only where the operands do not say it.
fn result_types(inst: &PendingInst<'_>, types: &[Option<Type>]) -> Option<Vec<Type>> {
    if inst.results.is_empty() {
        return Some(Vec::new());
    }
    if !inst.written.is_empty() {
        return Some(inst.written.clone());
    }
    match inst.opcode {
        Opcode::GlobalAddr | Opcode::Alloca => Some(vec![Type::PTR]),
        Opcode::ICmp | Opcode::FCmp => {
            let ty = arg_type(inst, types)?;
            Some(vec![ty.with_lane(Type::I1)])
        }
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => match &inst.extra {
            PendingExtra::Call { signature, .. } => Some(signature.returns.clone()),
            _ => None,
        },
        _ => Some(vec![arg_type(inst, types)?]),
    }
}

/// The type of an instruction's first operand, if it is known yet.
fn arg_type(inst: &PendingInst<'_>, types: &[Option<Type>]) -> Option<Type> {
    let &first = inst.args.first()?;
    *types.get(first as usize)?
}

/// A decimal number with no sign and no leading zero, which is the only form the printer writes.
fn parse_u64(text: &str) -> Option<u64> {
    if text.is_empty() || (text.starts_with('0') && text.len() > 1) {
        return None;
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_u32(text: &str) -> Option<u32> {
    parse_u64(text).and_then(|number| u32::try_from(number).ok())
}

/// A decimal integer, with a minus sign for a negative one, as the printer writes them.
fn parse_i128(text: &str) -> Option<i128> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    if digits.is_empty() || (digits.starts_with('0') && digits.len() > 1) {
        return None;
    }
    if negative && digits == "0" {
        return None;
    }
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let magnitude: u128 = digits.parse().ok()?;
    if negative {
        // The most negative number has no positive counterpart, so it is built by negating the
        // wrapped value rather than by converting first.
        (magnitude <= 1 << 127).then(|| (magnitude as i128).wrapping_neg())
    } else {
        i128::try_from(magnitude).ok()
    }
}

/// Whether a byte can appear in a symbol name.
///
/// Dots are in, because a compiler names things `hi.str` and `memcpy.resolve` and the assembler
/// takes them.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'$')
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;
    use crate::print;

    /// Reads a module and writes it back out, which is the whole claim this file makes.
    fn round_trip(text: &str) -> String {
        let mut names = Interner::new();
        let module = match parse(text, &mut names) {
            Ok(module) => module,
            Err(error) => panic!("{error}"),
        };
        print(&module, &names)
    }

    fn error(text: &str) -> String {
        let mut names = Interner::new();
        match parse(text, &mut names) {
            Ok(_) => panic!("that was expected to be turned down"),
            Err(error) => error.to_string(),
        }
    }

    const HEADER: &str = "\
; ModuleID = 'example.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
";

    /// The example from the spec, which is what `print` produces for it.
    const EXAMPLE: &str = "\
; ModuleID = 'example.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

global @counter : i32 = 0, align 4, linkage(internal)

func @sum(i32) -> i32, linkage(external), attrs(nounwind, fp_contract=on) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp sle %0, %1
    br_if %2, block2(%1), block1(%1, %1)

block1(%3: i32, %4: i32):
    %5 = iconst.i32 1
    %6 = add.nsw %4, %5
    %7 = add.nsw %3, %6
    %8 = icmp sge %6, %0
    br_if %8, block2(%7), block1(%7, %6)

block2(%9: i32):
    %10 = global_addr @counter
    store %9 -> %10, align 4, tbaa !1
    return %9
}

!0 = tbaa \"omnipotent char\", offset 0
!1 = tbaa \"int\", parent !0, offset 0
";

    /// One function holding very nearly every opcode, which is what `print` produces for it.
    const ZOO: &str = "\
; ModuleID = 'zoo.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

func @zoo(i32, ptr), linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = iconst.i64 -1
    %3 = fconst.f64 0x3ff8000000000000
    %4 = splat.i32x4 7
    %5 = alloca, size 16, align 8
    %6 = ptr_add %5, %2
    %7 = load.i32 %6, align 4, tbaa !0
    store.volatile %7 -> %6, align 4, tbaa !0
    %8 = atomic_rmw.i32 add %6, %0, align 4, seq_cst
    %9, %10 = cmpxchg.(i32, i1) %6, %8, %0, align 4, seq_cst
    fence seq_cst
    %11 = sext.i64 %0
    %12 = fcmp oeq %3, %3
    %13, %14 = sadd_overflow.(i32, i1) %0, %0
    %15 = call @puts(%1) : (ptr, ...) -> i32
    %16 = call_indirect %1(%0) : (i32) -> i32
    memcpy %5, %1, size 16, align 8
    inline_asm.volatile \"pause\", \"\", \"memory\"()
    %17 = target_intrinsic.i32 @x86.sse2.pmovmskb(%4)
    jump block1

block1:
    switch %0, block2, [0 => block3(%0), -1 => block2]

block2:
    inline_asm \"jmp %l0\", \"\", \"\"(), labels [block3(%0)]

block3(%18: i32):
    return %18
}

!0 = tbaa \"int\", offset 0
";

    /// Every shape a symbol comes in, which is what `print` produces for them.
    const SYMBOLS: &str = "\
; ModuleID = 'data.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

global @table : bytes 28 = { bytes \"hi\\00\\ff\\\"\\\\\", zero 2, i32 7, addr.8 @hi.str + 8, addr.8 @hi.str - 8 }, align 8, linkage(external), constant, section \".rodata.rel\"
global @errno : bytes 4, align 4, linkage(external), visibility(hidden), tls(initial_exec)

alias @total = @table, linkage(weak)
ifunc @memcpy = @memcpy.resolve, linkage(external), visibility(protected)

func @puts(ptr) -> i32, linkage(external), attrs(nounwind, willreturn);

func @helper() -> (i32, i32), linkage(internal), attrs(always_inline, readnone), section \".text.hot\" {
block0:
    %0 = iconst.i32 1
    return %0, %0
}
";

    #[test]
    fn the_example_in_the_spec_comes_back_byte_for_byte() {
        assert_eq!(round_trip(EXAMPLE), EXAMPLE);
    }

    #[test]
    fn one_of_almost_everything_comes_back_byte_for_byte() {
        assert_eq!(round_trip(ZOO), ZOO);
    }

    #[test]
    fn the_shapes_a_symbol_comes_in_come_back_byte_for_byte() {
        assert_eq!(round_trip(SYMBOLS), SYMBOLS);
    }

    #[test]
    fn a_value_defined_after_it_is_used() {
        // Block layout order is not required to put a definition before its uses, so the type
        // of %2 is only known after the whole function has been read. This is the case the two
        // passes exist for.
        let text = format!(
            "{HEADER}
func @late() -> i64, linkage(external) {{
block0:
    jump block2

block1(%0: i64):
    %1 = add %2, %0
    return %1

block2:
    %2 = iconst.i64 7
    jump block1(%2)
}}
"
        );
        assert_eq!(round_trip(&text), text);
    }

    #[test]
    fn an_empty_module_is_its_header() {
        assert_eq!(round_trip(HEADER), HEADER);
    }

    #[test]
    fn the_format_version_is_checked_before_anything_else() {
        let text = "; ModuleID = 'a.c'\n; format 99\n";
        assert_eq!(error(text), "line 2: this build reads format 0 and the text says format 99");
    }

    #[test]
    fn an_opcode_nobody_has_is_reported_with_its_line() {
        let text = format!(
            "{HEADER}
func @f(), linkage(external) {{
block0:
    getelementptr %0
}}
"
        );
        assert_eq!(error(&text), "line 8: `getelementptr` is not an opcode");
    }

    #[test]
    fn a_value_nothing_defines_is_reported() {
        let text = format!(
            "{HEADER}
func @f(i32) -> i32, linkage(external) {{
block0(%0: i32):
    %1 = add %0, %9
    return %1
}}
"
        );
        assert_eq!(error(&text), "line 8: %9 is used and never defined");
    }

    #[test]
    fn a_block_nothing_defines_is_reported() {
        let text = format!(
            "{HEADER}
func @f(), linkage(external) {{
block0:
    jump block7
}}
"
        );
        assert_eq!(error(&text), "line 8: block7 is used and never defined");
    }

    #[test]
    fn values_have_to_be_numbered_in_print_order() {
        let text = format!(
            "{HEADER}
func @f() -> i32, linkage(external) {{
block0:
    %1 = iconst.i32 0
    %0 = iconst.i32 1
    return %1
}}
"
        );
        assert_eq!(error(&text), "line 8: values are numbered in order and %0 comes next");
    }

    #[test]
    fn blocks_have_to_be_numbered_in_print_order() {
        let text = format!(
            "{HEADER}
func @f(), linkage(external) {{
block1:
    return
}}
"
        );
        assert_eq!(error(&text), "line 7: blocks are numbered in order and block0 comes next");
    }

    #[test]
    fn metadata_nobody_defines_is_reported() {
        let text = format!(
            "{HEADER}
func @f(ptr), linkage(external) {{
block0(%0: ptr):
    %1 = load.i32 %0, align 4, tbaa !3
    return
}}
"
        );
        assert_eq!(error(&text), "line 8: !3 is used and never defined");
    }

    #[test]
    fn a_type_nothing_says_is_reported_rather_than_guessed() {
        let text = format!(
            "{HEADER}
func @f(), linkage(external) {{
block0:
    %0 = add
    return
}}
"
        );
        assert_eq!(error(&text), "line 8: nothing in the text says what add produces");
    }

    #[test]
    fn a_line_with_something_left_on_it_is_turned_down() {
        let text = format!(
            "{HEADER}
global @x : i32 = 0, align 4, linkage(internal) and then some
"
        );
        assert_eq!(error(&text), "line 6: `and then some` is left over at the end of the line");
    }

    #[test]
    fn a_constant_that_is_not_one_is_turned_down() {
        let text = format!(
            "{HEADER}
global @x : i32 = 007, align 4, linkage(internal)
"
        );
        assert_eq!(error(&text), "line 6: `007` is not an i32");
    }

    #[test]
    fn a_number_is_read_the_way_the_printer_writes_it() {
        assert_eq!(parse_i128("-1"), Some(-1));
        assert_eq!(parse_i128("0"), Some(0));
        assert_eq!(parse_i128("-0"), None);
        assert_eq!(parse_i128("+1"), None);
        assert_eq!(parse_i128("01"), None);
        assert_eq!(parse_i128(""), None);
        assert_eq!(parse_i128("170141183460469231731687303715884105728"), None);
        assert_eq!(parse_i128("-170141183460469231731687303715884105728"), Some(i128::MIN));
    }

    #[test]
    fn every_opcode_says_which_payload_it_carries() {
        // A payload the parser does not expect for that opcode is text it cannot read back, so
        // the two of them agreeing is what this file rests on.
        for opcode in Opcode::all() {
            let kind = opcode.extra_kind();
            let expected = match opcode {
                Opcode::IConst | Opcode::FConst | Opcode::Splat => ExtraKind::Imm,
                Opcode::GlobalAddr | Opcode::TargetIntrinsic => ExtraKind::Symbol,
                Opcode::ICmp => ExtraKind::IntPred,
                Opcode::FCmp => ExtraKind::FloatPred,
                Opcode::Fence => ExtraKind::Order,
                Opcode::AtomicRmw => ExtraKind::Rmw,
                Opcode::Switch => ExtraKind::Switch,
                Opcode::InlineAsm => ExtraKind::Asm,
                Opcode::Jump | Opcode::BrIf => ExtraKind::Targets,
                Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => ExtraKind::Call,
                Opcode::Alloca
                | Opcode::Load
                | Opcode::Store
                | Opcode::Memcpy
                | Opcode::Memmove
                | Opcode::Memset
                | Opcode::AtomicLoad
                | Opcode::AtomicStore
                | Opcode::Cmpxchg => ExtraKind::Mem,
                _ => ExtraKind::None,
            };
            assert_eq!(kind, expected, "{}", opcode.name());
        }
    }
}
