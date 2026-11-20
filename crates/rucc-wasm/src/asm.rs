//! `-S` for wasm: the object in the assembly dialect of LLVM, which clang writes and reads.
//!
//! Design: section 6.6 of the WebAssembly notes, decision D13. The text is written from the
//! object model that [`rucc_object::wasm::write`] encodes, and not from an earlier stage. The
//! printer decodes each function body from the bytes that go into the object, and reads the name
//! of each relocated field from the fixup at that field. So the listing and the object beside it
//! come from one description and cannot disagree, which is the rule of section 11.1 of
//! `spec/11-asm-objects-debug.md` for the native targets.
//!
//! The dialect is the one that `llvm-mc` reads, and the test is that clang assembles the text into
//! an object with the same code, data and symbols. Four facts about that reader decide the form:
//!
//! 1. Each `end` names what it closes: `end_block`, `end_loop`, `end_if` or `end_function`. A
//!    plain `end` does not close anything, so the printer keeps a stack of the open constructs.
//! 2. `select` has the type of its operands in its name, and the reader encodes a `select` with no
//!    type as the typed form `0x1c` with an empty type list. The binary has the untyped `0x1b`,
//!    so the printer follows the types on the operand stack to name the instruction.
//! 3. The reader checks the types of the instructions as it reads them. The text must declare
//!    every function that it names with `.functype`, every global with `.globaltype` and the table
//!    with `.tabletype`, before the first use.
//! 4. A float constant is a hexadecimal float, `infinity` or `nan`. The reader has no text for a
//!    NaN with a payload other than the one of `nan`, so such a constant is written as the
//!    `i32.const` or `i64.const` of its bits and a `reinterpret`. That gives the same value with
//!    the same bits and is the one place where the code that the text assembles to is not the code
//!    in the object.
//!
//! The tree form of `--emit=wasm-tree` is printed by the same decoder. It is not for an assembler,
//! so it indents each construct, and it writes the notes that the translation kept: the IR block
//! where the code of each block starts, the block that follows each `block`, the target of each
//! branch out of a construct, and the IR value in each local.

use std::fmt::Write as _;

use crate::Notes;
use crate::simd::{self, Simd};
use rucc_object::wasm::{
    Custom, EXPORTED, FuncType, Function, HIDDEN, LOCAL, Module, NO_STRIP, RETAIN, RelocKind,
    STRINGS, Segment, SymbolKind, TLS_SEGMENT, ValType, WEAK,
};

/// The text of `object`.
///
/// # Errors
///
/// The name of the function and the reason, when a body has bytes that this does not decode.
/// That is a mistake in the selector or in this printer, and not in the program.
pub(crate) fn print(object: &Module) -> Result<String, (String, String)> {
    let mut printer = Printer { object, out: String::new() };
    printer.declarations();
    for function in &object.functions {
        printer.function(function).map_err(|why| (printer.name(function.symbol), why))?;
    }
    for (index, segment) in object.segments.iter().enumerate() {
        printer.segment(index, segment);
    }
    printer.aliases();
    printer.trailer();
    Ok(printer.out)
}

/// The tree form of `object`, with the notes that the translation wrote for each function. See
/// [`crate::tree`].
///
/// # Errors
///
/// The name of the function and the reason, when a body has bytes that this does not decode.
pub(crate) fn tree(object: &Module, notes: &[Notes]) -> Result<String, (String, String)> {
    let mut printer = Printer { object, out: String::new() };
    for (function, notes) in object.functions.iter().zip(notes) {
        printer.tree(function, notes).map_err(|why| (printer.name(function.symbol), why))?;
    }
    Ok(printer.out)
}

/// The column at which the tree form writes a note.
const NOTE: usize = 40;

/// Where a construct that an `end` closes came from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Function,
    Block,
    Loop,
    If,
    TryTable,
}

/// One open construct and what the operand stack held when it opened.
struct Frame {
    kind: Kind,
    /// The height of the operand stack at the start of the construct.
    height: usize,
    result: Option<ValType>,
    /// Whether the code after the last instruction cannot be reached, after a `br`, a `return`
    /// or an `unreachable`. The stack there can give any type, which is what `None` stands for.
    dead: bool,
}

struct Printer<'a> {
    object: &'a Module,
    out: String,
}

/// A body as it is decoded: the bytes, the position, the types of the locals and of the operand
/// stack, and the open constructs.
struct Body<'a> {
    function: &'a Function,
    at: usize,
    locals: Vec<ValType>,
    stack: Vec<Option<ValType>>,
    frames: Vec<Frame>,
}

impl Body<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let byte = *self.function.code.get(self.at).ok_or("the body ends in an instruction")?;
        self.at += 1;
        Ok(byte)
    }

    fn uleb(&mut self) -> Result<u64, String> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if shift >= 64 {
                return Err("a LEB128 field is longer than ten bytes".into());
            }
        }
    }

    fn u32(&mut self) -> Result<u32, String> {
        u32::try_from(self.uleb()?).map_err(|_| "an index does not fit in 32 bits".to_owned())
    }

    fn sleb(&mut self) -> Result<i64, String> {
        let mut value = 0i64;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            value |= i64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    value |= -1i64 << shift;
                }
                return Ok(value);
            }
            if shift >= 70 {
                return Err("a LEB128 field is longer than ten bytes".into());
            }
        }
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let end = self.at + N;
        let bytes = self.function.code.get(self.at..end).ok_or("the body ends in a constant")?;
        self.at = end;
        Ok(bytes.try_into().expect("the slice has N bytes"))
    }

    /// The fixup of the field that starts here, and the field itself, which is skipped.
    fn fixup(&mut self) -> Result<Option<(RelocKind, u32, i32)>, String> {
        let at = u32::try_from(self.at).unwrap_or(u32::MAX);
        let Some(fixup) = self.function.fixups.iter().find(|f| f.at == at) else {
            return Ok(None);
        };
        self.at += fixup.kind.width();
        Ok(Some((fixup.kind, fixup.target, fixup.addend)))
    }

    fn top(&self) -> &Frame {
        self.frames.last().expect("the frame of the function is open until its last `end`")
    }

    fn pop(&mut self) -> Option<ValType> {
        if self.stack.len() > self.top().height { self.stack.pop().flatten() } else { None }
    }

    fn pops(&mut self, count: usize) {
        for _ in 0..count {
            self.pop();
        }
    }

    fn push(&mut self, ty: ValType) {
        self.stack.push(Some(ty));
    }

    /// The rest of the construct cannot be reached, and its stack can give any type.
    fn dead(&mut self) {
        let height = self.top().height;
        self.stack.truncate(height);
        self.frames.last_mut().expect("a construct is open").dead = true;
    }

    fn open(&mut self, kind: Kind) -> Result<Option<ValType>, String> {
        let result = match self.byte()? {
            0x40 => None,
            0x7f => Some(ValType::I32),
            0x7e => Some(ValType::I64),
            0x7d => Some(ValType::F32),
            0x7c => Some(ValType::F64),
            0x7b => Some(ValType::V128),
            byte => return Err(format!("a block type of {byte:#04x} is not one that rucc writes")),
        };
        if kind == Kind::If {
            self.pop();
        }
        self.frames.push(Frame { kind, height: self.stack.len(), result, dead: false });
        Ok(result)
    }

    fn local(&mut self) -> Result<(u32, Option<ValType>), String> {
        let index = self.u32()?;
        Ok((index, self.locals.get(index as usize).copied()))
    }
}

impl Printer<'_> {
    fn name(&self, symbol: u32) -> String {
        self.object
            .symbols
            .get(symbol as usize)
            .map_or_else(|| format!("<symbol {symbol}>"), |s| s.name.clone())
    }

    fn line(&mut self, text: &str) {
        self.out.push('\t');
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn signature(ty: &FuncType) -> String {
        let list = |types: &[ValType]| {
            types.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
        };
        format!("({}) -> ({})", list(&ty.params), list(&ty.results))
    }

    fn defines(&self, symbol: u32) -> bool {
        self.object.functions.iter().any(|f| f.symbol == symbol)
    }

    fn is_alias(&self, symbol: u32) -> bool {
        self.object.aliases.iter().any(|&(alias, _)| alias == symbol)
    }

    /// The types of the globals and the table, and the signature of every function, before any
    /// code names them, with the module and field of an import that is not the default one. Each
    /// data symbol is named here too, so that the block names every symbol in the order of the
    /// symbol table, and the reader in `read` gives them back in that order. A section symbol is
    /// the label at the start of its custom section, after the code and the data.
    fn declarations(&mut self) {
        let object = self.object;
        for (index, symbol) in object.symbols.iter().enumerate() {
            let name = &symbol.name;
            match &symbol.kind {
                SymbolKind::Global { ty, mutable, .. } => {
                    let mutable = if *mutable { "" } else { ", immutable" };
                    self.line(&format!(".globaltype\t{name}, {ty}{mutable}"));
                }
                SymbolKind::Table { .. } => self.line(&format!(".tabletype\t{name}, funcref")),
                SymbolKind::Tag { ty, .. } => {
                    let params = object.types[*ty as usize].params.iter();
                    let params: Vec<String> = params.map(ToString::to_string).collect();
                    self.line(&format!(".tagtype\t{name} {}", params.join(", ")));
                }
                SymbolKind::Function { ty, import } => {
                    let index = u32::try_from(index).expect("fewer than 2^32 symbols");
                    let signature = Self::signature(&object.types[*ty as usize]);
                    self.line(&format!(".functype\t{name} {signature}"));
                    if let Some(import) = import {
                        let (module, field) = (&import.module, &import.field);
                        self.line(&format!(".import_module\t{name}, \"{module}\""));
                        self.line(&format!(".import_name\t{name}, \"{field}\""));
                    }
                    // A second name of a function says its binding where it is set, after the
                    // code.
                    if !self.defines(index) && !self.is_alias(index) {
                        self.binding(name, symbol.flags, false);
                    }
                }
                SymbolKind::Data { place } => {
                    if place.is_none() {
                        self.binding(name, symbol.flags, false);
                    }
                    self.line(&format!(".type\t{name},@object"));
                }
                SymbolKind::Section { .. } => {}
            }
        }
    }

    /// The visibility and the binding of a symbol. A defined symbol that is not local is global,
    /// and a symbol that the object does not define says only what is not the default.
    fn binding(&mut self, name: &str, flags: u32, defined: bool) {
        if flags & HIDDEN != 0 {
            self.line(&format!(".hidden\t{name}"));
        }
        if flags & WEAK != 0 {
            self.line(&format!(".weak\t{name}"));
        } else if defined && flags & LOCAL == 0 {
            self.line(&format!(".globl\t{name}"));
        }
    }

    fn function(&mut self, function: &Function) -> Result<(), String> {
        let object = self.object;
        let symbol = &object.symbols[function.symbol as usize];
        let name = &symbol.name;
        let SymbolKind::Function { ty, .. } = symbol.kind else {
            return Err("the symbol of a definition is not a function".into());
        };
        let ty = &object.types[ty as usize];
        self.line(&format!(".section\t.text.{name},\"\",@"));
        self.binding(name, symbol.flags, true);
        self.line(&format!(".type\t{name},@function"));
        if let (Some(export), true) = (&function.export, symbol.flags & EXPORTED != 0) {
            self.line(&format!(".export_name\t{name}, \"{export}\""));
        }
        let _ = writeln!(self.out, "{name}:");
        self.line(&format!(".functype\t{name} {}", Self::signature(ty)));
        let mut locals = ty.params.clone();
        for &(count, ty) in &function.locals {
            locals.extend(std::iter::repeat_n(ty, count as usize));
        }
        if locals.len() > ty.params.len() {
            let list: Vec<String> =
                locals[ty.params.len()..].iter().map(|t| t.to_string()).collect();
            self.line(&format!(".local\t{}", list.join(", ")));
        }
        let mut body = Body { function, at: 0, locals, stack: Vec::new(), frames: Vec::new() };
        let result = ty.results.first().copied();
        body.frames.push(Frame { kind: Kind::Function, height: 0, result, dead: false });
        while !body.frames.is_empty() {
            let text = self.instruction(&mut body)?;
            self.line(&text);
        }
        if body.at != function.code.len() {
            return Err("there are bytes after the `end` of the body".into());
        }
        self.out.push('\n');
        Ok(())
    }

    /// One function in the tree form: the signature, each local with the IR value that it holds,
    /// and the body with one more indent in each construct and the notes beside the instructions.
    fn tree(&mut self, function: &Function, notes: &Notes) -> Result<(), String> {
        let object = self.object;
        let symbol = &object.symbols[function.symbol as usize];
        let SymbolKind::Function { ty, .. } = symbol.kind else {
            return Err("the symbol of a definition is not a function".into());
        };
        let ty = &object.types[ty as usize];
        let _ = writeln!(self.out, "function {} {}", symbol.name, Self::signature(ty));
        let mut locals = ty.params.clone();
        for &(count, ty) in &function.locals {
            locals.extend(std::iter::repeat_n(ty, count as usize));
        }
        for (index, local) in locals.iter().enumerate() {
            let kind = if index < ty.params.len() { "param" } else { "local" };
            let _ = write!(self.out, "  {kind} {index} {local}");
            if let Some(name) = u32::try_from(index).ok().and_then(|i| notes.locals.get(&i)) {
                let _ = write!(self.out, " {name}");
            }
            self.out.push('\n');
        }
        let mut body = Body { function, at: 0, locals, stack: Vec::new(), frames: Vec::new() };
        let result = ty.results.first().copied();
        body.frames.push(Frame { kind: Kind::Function, height: 0, result, dead: false });
        let mut marks = notes.marks.iter().peekable();
        while !body.frames.is_empty() {
            let start = body.at;
            let before = body.frames.len();
            let text = self.instruction(&mut body)?;
            let mut depth = before.min(body.frames.len());
            if text == "else" {
                depth -= 1;
            }
            let mut line = format!("{}{}", "  ".repeat(depth), named(&text, notes));
            let mut said = Vec::new();
            while let Some((_, note)) = marks.next_if(|&&(at, _)| at <= start) {
                said.push(note.as_str());
            }
            if !said.is_empty() {
                let width = line.chars().count();
                line.push_str(&" ".repeat(NOTE.saturating_sub(width).max(1)));
                line.push_str("; ");
                line.push_str(&said.join(", "));
            }
            self.out.push_str(&line);
            self.out.push('\n');
        }
        self.out.push('\n');
        Ok(())
    }

    /// One instruction, decoded at the position of `body` and followed on the operand stack.
    #[allow(clippy::too_many_lines)]
    fn instruction(&self, body: &mut Body<'_>) -> Result<String, String> {
        use ValType::{F32, F64, I32, I64};
        let start = body.at;
        let op = body.byte()?;
        let text = match op {
            0x00 => {
                body.dead();
                "unreachable".into()
            }
            0x01 => "nop".into(),
            0x02..=0x04 => {
                let (kind, name) = match op {
                    0x02 => (Kind::Block, "block"),
                    0x03 => (Kind::Loop, "loop"),
                    _ => (Kind::If, "if"),
                };
                match body.open(kind)? {
                    Some(ty) => format!("{name}\t{ty}"),
                    None => name.into(),
                }
            }
            0x05 => {
                let frame = body.frames.last_mut().ok_or("an `else` outside an `if`")?;
                if frame.kind != Kind::If {
                    return Err("an `else` outside an `if`".into());
                }
                frame.dead = false;
                let height = frame.height;
                body.stack.truncate(height);
                "else".into()
            }
            0x0b => {
                let frame = body.frames.pop().ok_or("an `end` with nothing open")?;
                body.stack.truncate(frame.height);
                if let Some(ty) = frame.result {
                    body.push(ty);
                }
                match frame.kind {
                    Kind::Function => "end_function",
                    Kind::Block => "end_block",
                    Kind::Loop => "end_loop",
                    Kind::If => "end_if",
                    Kind::TryTable => "end_try_table",
                }
                .into()
            }
            0x1f => {
                let result = body.open(Kind::TryTable)?;
                let mut text = match result {
                    Some(ty) => format!("try_table\t{ty}"),
                    None => "try_table\t".into(),
                };
                for _ in 0..body.u32()? {
                    let kind = body.byte()?;
                    let name = match kind {
                        0x00 => "catch",
                        0x01 => "catch_ref",
                        0x02 => "catch_all",
                        0x03 => "catch_all_ref",
                        _ => return Err(format!("a catch clause of kind {kind:#04x}")),
                    };
                    let tag = if kind < 0x02 {
                        let Some((RelocKind::TagIndexLeb, symbol, _)) = body.fixup()? else {
                            return Err("a `catch` names no tag symbol".into());
                        };
                        format!(" {}", self.name(symbol))
                    } else {
                        String::new()
                    };
                    let label = body.u32()?;
                    if !text.ends_with('\t') {
                        text.push(' ');
                    }
                    let _ = write!(text, "({name}{tag} {label})");
                }
                text.trim_end().to_owned()
            }
            0x0c => {
                let depth = body.u32()?;
                body.dead();
                format!("br\t{depth}")
            }
            0x0d => {
                let depth = body.u32()?;
                body.pop();
                format!("br_if\t{depth}")
            }
            0x0e => {
                let count = body.u32()?;
                let mut depths = Vec::new();
                for _ in 0..=count {
                    depths.push(body.u32()?.to_string());
                }
                body.dead();
                format!("br_table\t{{{}}}", depths.join(", "))
            }
            0x0f => {
                body.dead();
                "return".into()
            }
            0x10 | 0x12 => {
                let Some((RelocKind::FunctionIndexLeb, symbol, _)) = body.fixup()? else {
                    return Err("a `call` names no function symbol".into());
                };
                let SymbolKind::Function { ty, .. } = self.object.symbols[symbol as usize].kind
                else {
                    return Err("a `call` names a symbol that is not a function".into());
                };
                let ty = &self.object.types[ty as usize];
                body.pops(ty.params.len());
                if op == 0x12 {
                    body.dead();
                    return Ok(format!("return_call\t{}", self.name(symbol)));
                }
                ty.results.iter().for_each(|&t| body.push(t));
                format!("call\t{}", self.name(symbol))
            }
            0x11 | 0x13 => {
                let Some((RelocKind::TypeIndexLeb, ty, _)) = body.fixup()? else {
                    return Err("a `call_indirect` names no type".into());
                };
                let table = match body.fixup()? {
                    Some((RelocKind::TableNumberLeb, table, _)) => {
                        format!("{}, ", self.name(table))
                    }
                    Some(_) => return Err("a `call_indirect` names a table wrongly".into()),
                    None if body.byte()? == 0 => String::new(),
                    None => return Err("a `call_indirect` names a table other than zero".into()),
                };
                let ty = &self.object.types[ty as usize];
                body.pop();
                body.pops(ty.params.len());
                if op == 0x13 {
                    body.dead();
                    return Ok(format!("return_call_indirect\t{table}{}", Self::signature(ty)));
                }
                ty.results.iter().for_each(|&t| body.push(t));
                format!("call_indirect\t{table}{}", Self::signature(ty))
            }
            0x1a => {
                body.pop();
                "drop".into()
            }
            0x1b => {
                body.pop();
                let second = body.pop();
                let first = body.pop();
                let ty = first.or(second).unwrap_or(I32);
                body.push(ty);
                format!("{ty}.select")
            }
            0x20 => {
                let (index, ty) = body.local()?;
                body.push(ty.ok_or("a `local.get` of a local that is not there")?);
                format!("local.get\t{index}")
            }
            0x21 => {
                let (index, _) = body.local()?;
                body.pop();
                format!("local.set\t{index}")
            }
            0x22 => {
                let (index, ty) = body.local()?;
                body.pop();
                body.push(ty.ok_or("a `local.tee` of a local that is not there")?);
                format!("local.tee\t{index}")
            }
            0x23 | 0x24 => {
                let Some((RelocKind::GlobalIndexLeb, symbol, _)) = body.fixup()? else {
                    return Err("a global instruction names no global symbol".into());
                };
                let SymbolKind::Global { ty, .. } = self.object.symbols[symbol as usize].kind
                else {
                    return Err("a global instruction names a symbol that is not a global".into());
                };
                if op == 0x23 {
                    body.push(ty);
                    format!("global.get\t{}", self.name(symbol))
                } else {
                    body.pop();
                    format!("global.set\t{}", self.name(symbol))
                }
            }
            0x28..=0x3e => {
                let (name, natural, ty) = MEMORY[usize::from(op - 0x28)];
                let align = body.u32()?;
                let offset = match body.fixup()? {
                    Some((RelocKind::MemoryAddrLeb, symbol, addend)) => {
                        self.address(symbol, addend)
                    }
                    Some(_) => {
                        return Err("the offset of a load or store is relocated wrongly".into());
                    }
                    None => body.u32()?.to_string(),
                };
                if op <= 0x35 {
                    body.pop();
                    body.push(ty);
                } else {
                    body.pops(2);
                }
                match align == natural {
                    true => format!("{name}\t{offset}"),
                    false => format!("{name}\t{offset}:p2align={align}"),
                }
            }
            0x3f | 0x40 => {
                if body.byte()? != 0 {
                    return Err("a memory instruction names a memory other than zero".into());
                }
                if op == 0x40 {
                    body.pop();
                }
                body.push(I32);
                format!("{}\t0", if op == 0x3f { "memory.size" } else { "memory.grow" })
            }
            0x41 => {
                body.push(I32);
                match body.fixup()? {
                    Some((RelocKind::MemoryAddrSleb, symbol, addend)) => {
                        format!("i32.const\t{}", self.address(symbol, addend))
                    }
                    Some((RelocKind::TableIndexSleb, symbol, _)) => {
                        format!("i32.const\t{}", self.name(symbol))
                    }
                    Some((RelocKind::MemoryAddrTlsSleb, symbol, addend)) => {
                        let name = self.name(symbol);
                        match addend {
                            0 => format!("i32.const\t{name}@TLSREL"),
                            _ => format!("i32.const\t{name}@TLSREL{addend:+}"),
                        }
                    }
                    Some(_) => return Err("an `i32.const` is relocated wrongly".into()),
                    None => format!("i32.const\t{}", body.sleb()?),
                }
            }
            0x42 => {
                body.push(I64);
                format!("i64.const\t{}", body.sleb()?)
            }
            0x43 => {
                body.push(F32);
                let bits = u32::from_le_bytes(body.fixed()?);
                match float32(bits) {
                    Some(text) => format!("f32.const\t{text}"),
                    None => format!("i32.const\t{}\n\tf32.reinterpret_i32", bits as i32),
                }
            }
            0x44 => {
                body.push(F64);
                let bits = u64::from_le_bytes(body.fixed()?);
                match float64(bits) {
                    Some(text) => format!("f64.const\t{text}"),
                    None => format!("i64.const\t{}\n\tf64.reinterpret_i64", bits as i64),
                }
            }
            0x45..=0xc4 => {
                let name = NUMERIC[usize::from(op - 0x45)];
                let unary = matches!(op, 0x45 | 0x50 | 0x67..=0x69 | 0x79..=0x7b)
                    || matches!(op, 0x8b..=0x91 | 0x99..=0x9f | 0xa7..);
                body.pops(if unary { 1 } else { 2 });
                body.push(if op <= 0x66 { I32 } else { result_of(name) });
                name.into()
            }
            0xfc => match body.u32()? {
                sub @ 0..=7 => {
                    body.pop();
                    let name = SATURATING[sub as usize];
                    body.push(result_of(name));
                    name.into()
                }
                10 => {
                    if body.fixed::<2>()? != [0, 0] {
                        return Err("a `memory.copy` names a memory other than zero".into());
                    }
                    body.pops(3);
                    "memory.copy\t0, 0".into()
                }
                11 => {
                    if body.byte()? != 0 {
                        return Err("a `memory.fill` names a memory other than zero".into());
                    }
                    body.pops(3);
                    "memory.fill\t0".into()
                }
                sub => return Err(format!("the instruction 0xfc {sub} is not one rucc writes")),
            },
            0xfd => {
                let opcode = body.u32()?;
                let Some((name, form)) = simd::by_opcode(opcode) else {
                    return Err(format!("the instruction 0xfd {opcode} is not one rucc writes"));
                };
                body.pops(form.pops());
                if let Some(ty) = form.result() {
                    body.push(ty);
                }
                self.simd(body, name, form)?
            }
            _ => return Err(format!("the opcode {op:#04x} at {start} is not one rucc writes")),
        };
        Ok(text)
    }

    /// The text of a SIMD instruction and its immediates, which come after the opcode.
    ///
    /// A `v128.const` is written as four `i32` lanes. The reader of `llvm-mc` takes the width of a
    /// lane from the number of lanes, so the text gives the same sixteen bytes back.
    fn simd(&self, body: &mut Body<'_>, name: &str, form: Simd) -> Result<String, String> {
        let mut operands = Vec::new();
        if let Some(natural) = form.memory() {
            let align = body.u32()?;
            let offset = match body.fixup()? {
                Some((RelocKind::MemoryAddrLeb, symbol, addend)) => self.address(symbol, addend),
                Some(_) => return Err("the offset of a load or store is relocated wrongly".into()),
                None => body.u32()?.to_string(),
            };
            operands.push(match align == natural {
                true => offset,
                false => format!("{offset}:p2align={align}"),
            });
        }
        match form {
            Simd::Const => {
                let bytes = body.fixed::<16>()?;
                operands.extend(bytes.chunks(4).map(|lane| {
                    i32::from_le_bytes(lane.try_into().expect("four bytes")).to_string()
                }));
            }
            Simd::Shuffle => {
                operands.extend(body.fixed::<16>()?.iter().map(ToString::to_string));
            }
            _ if form.lane() => operands.push(body.byte()?.to_string()),
            _ => {}
        }
        Ok(match operands.is_empty() {
            true => name.to_owned(),
            false => format!("{name}\t{}", operands.join(", ")),
        })
    }

    /// A data address: the symbol and the offset from it.
    fn address(&self, symbol: u32, addend: i32) -> String {
        let name = self.name(symbol);
        match addend {
            0 => name,
            _ if addend < 0 => format!("{name}{addend}"),
            _ => format!("{name}+{addend}"),
        }
    }

    /// One data segment, with each symbol in it at its offset.
    fn segment(&mut self, index: usize, segment: &Segment) {
        let object = self.object;
        let mut labels: Vec<(u32, usize)> = object
            .symbols
            .iter()
            .enumerate()
            .filter_map(|(symbol, s)| match s.kind {
                SymbolKind::Data { place: Some(place) } if place.segment as usize == index => {
                    Some((place.offset, symbol))
                }
                _ => None,
            })
            .collect();
        labels.sort_unstable();
        let mut flags = String::new();
        if segment.flags & STRINGS != 0 {
            flags.push('S');
        }
        if segment.flags & TLS_SEGMENT != 0 {
            flags.push('T');
        }
        if segment.flags & RETAIN != 0 {
            flags.push('R');
        }
        self.line(&format!(".section\t{},\"{flags}\",@", segment.name));
        self.line(&format!(".p2align\t{}, 0x0", segment.align));
        let mut fixups = segment.fixups.clone();
        fixups.sort_unstable_by_key(|f| f.at);
        let (mut labels, mut fixups) =
            (labels.into_iter().peekable(), fixups.into_iter().peekable());
        let mut at = 0usize;
        loop {
            while let Some(&(offset, symbol)) = labels.peek() {
                if offset as usize > at {
                    break;
                }
                labels.next();
                let s = &object.symbols[symbol];
                let SymbolKind::Data { place: Some(place) } = s.kind else { unreachable!() };
                self.binding(&s.name, s.flags, true);
                self.line(&format!(".type\t{},@object", s.name));
                let _ = writeln!(self.out, "{}:", s.name);
                self.line(&format!(".size\t{}, {}", s.name, place.size));
            }
            if let Some(fixup) = fixups.next_if(|f| f.at as usize <= at) {
                let target = match fixup.kind {
                    RelocKind::MemoryAddrI32 => self.address(fixup.target, fixup.addend),
                    _ => self.name(fixup.target),
                };
                self.line(&format!(".int32\t{target}"));
                at += fixup.kind.width();
                continue;
            }
            if at >= segment.bytes.len() {
                break;
            }
            let next_label = labels.peek().map_or(usize::MAX, |&(offset, _)| offset as usize);
            let next_fixup = fixups.peek().map_or(usize::MAX, |f| f.at as usize);
            let stop = segment.bytes.len().min(next_label).min(next_fixup);
            self.bytes(&segment.bytes[at..stop]);
            at = stop;
        }
        self.out.push('\n');
    }

    /// Bytes of data: a run of zeros as `.skip` and the rest as `.ascii`, in lines of a width
    /// that a person can read.
    fn bytes(&mut self, bytes: &[u8]) {
        let mut at = 0;
        while at < bytes.len() {
            let zeros = bytes[at..].iter().take_while(|&&b| b == 0).count();
            if zeros >= 4 || zeros == bytes.len() - at {
                self.line(&format!(".skip\t{zeros}"));
                at += zeros;
                continue;
            }
            let mut end = at;
            while end < bytes.len() && end - at < 64 && !bytes[end..].starts_with(&[0; 4]) {
                end += 1;
            }
            let mut text = String::new();
            for &b in &bytes[at..end] {
                match b {
                    b'"' => text.push_str("\\\""),
                    b'\\' => text.push_str("\\\\"),
                    0x20..=0x7e => text.push(char::from(b)),
                    _ => {
                        let _ = write!(text, "\\{b:03o}");
                    }
                }
            }
            self.line(&format!(".ascii\t\"{text}\""));
            at = end;
        }
    }

    /// Each second name of a function, set to the function, as clang writes the `alias`
    /// attribute.
    fn aliases(&mut self) {
        let object = self.object;
        for &(alias, target) in &object.aliases {
            let symbol = &object.symbols[alias as usize];
            self.binding(&symbol.name, symbol.flags, true);
            self.line(&format!(".type\t{},@function", symbol.name));
            let _ = writeln!(self.out, "{} = {}", symbol.name, self.name(target));
        }
    }

    /// One custom section with fixups, as LLVM writes a DWARF section: its section symbol as a
    /// label at the start, and each fixup as `.int32` of a symbol and an offset. A function
    /// symbol there is a code offset, a section symbol is an offset into its section, and a data
    /// symbol is an address.
    fn custom(&mut self, index: usize, custom: &Custom) {
        // The string sections of DWARF have the flag of strings in LLVM, and its assembler refuses
        // them without it.
        if matches!(custom.name.as_str(), ".debug_str" | ".debug_line_str") {
            self.line(&format!(".section\t{},\"S\",@", custom.name));
        } else if custom.name.starts_with(".debug_") {
            self.line(&format!(".section\t{},\"\",@", custom.name));
        } else {
            self.line(&format!(".section\t.custom_section.{},\"\",@", custom.name));
        }
        let start = self.object.symbols.iter().find(|s| match s.kind {
            SymbolKind::Section { custom } => custom as usize == index,
            _ => false,
        });
        if let Some(symbol) = start {
            let _ = writeln!(self.out, "{}:", symbol.name);
        }
        let mut fixups = custom.fixups.clone();
        fixups.sort_unstable_by_key(|f| f.at);
        let mut at = 0usize;
        for fixup in fixups {
            self.bytes(&custom.bytes[at..fixup.at as usize]);
            self.line(&format!(".int32\t{}", self.address(fixup.target, fixup.addend)));
            at = fixup.at as usize + fixup.kind.width();
        }
        self.bytes(&custom.bytes[at..]);
    }

    /// The constructors, the symbols the linker keeps, the custom sections with fixups and the
    /// two custom sections of the toolchain.
    fn trailer(&mut self) {
        let object = self.object;
        for &(priority, symbol) in &object.inits {
            let section = match priority {
                65_535 => ".init_array".to_owned(),
                _ => format!(".init_array.{priority}"),
            };
            self.line(&format!(".section\t{section},\"\",@"));
            self.line(".p2align\t2, 0x0");
            self.line(&format!(".int32\t{}", self.name(symbol)));
        }
        for symbol in object.symbols.iter().filter(|s| s.flags & NO_STRIP != 0) {
            self.line(&format!(".no_dead_strip\t{}", symbol.name));
        }
        for (index, custom) in object.customs.iter().enumerate() {
            self.custom(index, custom);
        }
        let sections = [
            ("producers", rucc_object::wasm::producers(&object.producers)),
            ("target_features", rucc_object::wasm::target_features(object)),
        ];
        for (name, payload) in sections {
            let Some(payload) = payload else { continue };
            self.line(&format!(".section\t.custom_section.{name},\"\",@"));
            self.bytes(&payload);
        }
    }
}

/// The type a numeric instruction gives, which is the type in front of the dot of its name.
/// The text of an instruction in the tree form: a space after the name, and the name of the
/// local in place of its number when the local holds an IR value.
fn named(text: &str, notes: &Notes) -> String {
    let Some((op, operand)) = text.split_once('\t') else { return text.to_owned() };
    let local = op.starts_with("local.").then(|| operand.parse::<u32>().ok()).flatten();
    match local.and_then(|index| notes.locals.get(&index)) {
        Some(name) => format!("{op} {name}"),
        None => format!("{op} {operand}"),
    }
}

fn result_of(name: &str) -> ValType {
    match &name[..3] {
        "i64" => ValType::I64,
        "f32" => ValType::F32,
        "f64" => ValType::F64,
        _ => ValType::I32,
    }
}

/// An `f32` constant as the reader takes it, or nothing for a NaN with a payload of its own.
pub(crate) fn float32(bits: u32) -> Option<String> {
    let sign = if bits >> 31 == 1 { "-" } else { "" };
    let exponent = (bits >> 23) & 0xff;
    let fraction = bits & 0x7f_ffff;
    // The 23 bits of the fraction, shifted to fill six hexadecimal digits.
    let digits = |fraction: u32| trimmed(u64::from(fraction << 1), 6);
    Some(match (exponent, fraction) {
        (0, 0) => format!("{sign}0x0p0"),
        (0, _) => format!("{sign}0x0.{}p-126", digits(fraction)),
        (0xff, 0) => format!("{sign}infinity"),
        (0xff, 0x40_0000) => format!("{sign}nan"),
        (0xff, _) => return None,
        _ => format!("{sign}0x1{}p{}", point(&digits(fraction)), i64::from(exponent) - 127),
    })
}

/// An `f64` constant as the reader takes it, or nothing for a NaN with a payload of its own.
pub(crate) fn float64(bits: u64) -> Option<String> {
    let sign = if bits >> 63 == 1 { "-" } else { "" };
    let exponent = (bits >> 52) & 0x7ff;
    let fraction = bits & 0xf_ffff_ffff_ffff;
    let digits = |fraction: u64| trimmed(fraction, 13);
    Some(match (exponent, fraction) {
        (0, 0) => format!("{sign}0x0p0"),
        (0, _) => format!("{sign}0x0.{}p-1022", digits(fraction)),
        (0x7ff, 0) => format!("{sign}infinity"),
        (0x7ff, 0x8_0000_0000_0000) => format!("{sign}nan"),
        (0x7ff, _) => return None,
        _ => format!("{sign}0x1{}p{}", point(&digits(fraction)), exponent as i64 - 1023),
    })
}

/// `value` as `width` hexadecimal digits, without the zeros at the end.
fn trimmed(value: u64, width: usize) -> String {
    let text = format!("{value:0width$x}");
    text.trim_end_matches('0').to_owned()
}

/// The fraction after the point, or nothing when it is zero.
fn point(digits: &str) -> String {
    if digits.is_empty() { String::new() } else { format!(".{digits}") }
}

/// The loads and the stores from `0x28`: the name, the natural alignment as its log2, and the
/// type that a load gives.
pub(crate) const MEMORY: [(&str, u32, ValType); 23] = {
    use ValType::{F32, F64, I32, I64};
    [
        ("i32.load", 2, I32),
        ("i64.load", 3, I64),
        ("f32.load", 2, F32),
        ("f64.load", 3, F64),
        ("i32.load8_s", 0, I32),
        ("i32.load8_u", 0, I32),
        ("i32.load16_s", 1, I32),
        ("i32.load16_u", 1, I32),
        ("i64.load8_s", 0, I64),
        ("i64.load8_u", 0, I64),
        ("i64.load16_s", 1, I64),
        ("i64.load16_u", 1, I64),
        ("i64.load32_s", 2, I64),
        ("i64.load32_u", 2, I64),
        ("i32.store", 2, I32),
        ("i64.store", 3, I64),
        ("f32.store", 2, F32),
        ("f64.store", 3, F64),
        ("i32.store8", 0, I32),
        ("i32.store16", 1, I32),
        ("i64.store8", 0, I64),
        ("i64.store16", 1, I64),
        ("i64.store32", 2, I64),
    ]
};

/// The comparisons, the arithmetic and the conversions, from `0x45` to `0xc4`.
pub(crate) const NUMERIC: [&str; 128] = [
    "i32.eqz",
    "i32.eq",
    "i32.ne",
    "i32.lt_s",
    "i32.lt_u",
    "i32.gt_s",
    "i32.gt_u",
    "i32.le_s",
    "i32.le_u",
    "i32.ge_s",
    "i32.ge_u",
    "i64.eqz",
    "i64.eq",
    "i64.ne",
    "i64.lt_s",
    "i64.lt_u",
    "i64.gt_s",
    "i64.gt_u",
    "i64.le_s",
    "i64.le_u",
    "i64.ge_s",
    "i64.ge_u",
    "f32.eq",
    "f32.ne",
    "f32.lt",
    "f32.gt",
    "f32.le",
    "f32.ge",
    "f64.eq",
    "f64.ne",
    "f64.lt",
    "f64.gt",
    "f64.le",
    "f64.ge",
    "i32.clz",
    "i32.ctz",
    "i32.popcnt",
    "i32.add",
    "i32.sub",
    "i32.mul",
    "i32.div_s",
    "i32.div_u",
    "i32.rem_s",
    "i32.rem_u",
    "i32.and",
    "i32.or",
    "i32.xor",
    "i32.shl",
    "i32.shr_s",
    "i32.shr_u",
    "i32.rotl",
    "i32.rotr",
    "i64.clz",
    "i64.ctz",
    "i64.popcnt",
    "i64.add",
    "i64.sub",
    "i64.mul",
    "i64.div_s",
    "i64.div_u",
    "i64.rem_s",
    "i64.rem_u",
    "i64.and",
    "i64.or",
    "i64.xor",
    "i64.shl",
    "i64.shr_s",
    "i64.shr_u",
    "i64.rotl",
    "i64.rotr",
    "f32.abs",
    "f32.neg",
    "f32.ceil",
    "f32.floor",
    "f32.trunc",
    "f32.nearest",
    "f32.sqrt",
    "f32.add",
    "f32.sub",
    "f32.mul",
    "f32.div",
    "f32.min",
    "f32.max",
    "f32.copysign",
    "f64.abs",
    "f64.neg",
    "f64.ceil",
    "f64.floor",
    "f64.trunc",
    "f64.nearest",
    "f64.sqrt",
    "f64.add",
    "f64.sub",
    "f64.mul",
    "f64.div",
    "f64.min",
    "f64.max",
    "f64.copysign",
    "i32.wrap_i64",
    "i32.trunc_f32_s",
    "i32.trunc_f32_u",
    "i32.trunc_f64_s",
    "i32.trunc_f64_u",
    "i64.extend_i32_s",
    "i64.extend_i32_u",
    "i64.trunc_f32_s",
    "i64.trunc_f32_u",
    "i64.trunc_f64_s",
    "i64.trunc_f64_u",
    "f32.convert_i32_s",
    "f32.convert_i32_u",
    "f32.convert_i64_s",
    "f32.convert_i64_u",
    "f32.demote_f64",
    "f64.convert_i32_s",
    "f64.convert_i32_u",
    "f64.convert_i64_s",
    "f64.convert_i64_u",
    "f64.promote_f32",
    "i32.reinterpret_f32",
    "i64.reinterpret_f64",
    "f32.reinterpret_i32",
    "f64.reinterpret_i64",
    "i32.extend8_s",
    "i32.extend16_s",
    "i64.extend8_s",
    "i64.extend16_s",
    "i64.extend32_s",
];

/// The saturating conversions, `0xfc 0` to `0xfc 7`.
pub(crate) const SATURATING: [&str; 8] = [
    "i32.trunc_sat_f32_s",
    "i32.trunc_sat_f32_u",
    "i32.trunc_sat_f64_s",
    "i32.trunc_sat_f64_u",
    "i64.trunc_sat_f32_s",
    "i64.trunc_sat_f32_u",
    "i64.trunc_sat_f64_s",
    "i64.trunc_sat_f64_u",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_float_is_the_hexadecimal_form_that_the_reader_takes_back() {
        assert_eq!(float32(1.5f32.to_bits()).as_deref(), Some("0x1.8p0"));
        assert_eq!(float32((-0.0f32).to_bits()).as_deref(), Some("-0x0p0"));
        assert_eq!(float32(1).as_deref(), Some("0x0.000002p-126"));
        assert_eq!(float32(f32::MAX.to_bits()).as_deref(), Some("0x1.fffffep127"));
        assert_eq!(float32(f32::NEG_INFINITY.to_bits()).as_deref(), Some("-infinity"));
        assert_eq!(float32(0x7fc0_0000).as_deref(), Some("nan"));
        assert_eq!(float32(0x7fc0_0001), None);
        assert_eq!(float64(0.1f64.to_bits()).as_deref(), Some("0x1.999999999999ap-4"));
        assert_eq!(float64(1).as_deref(), Some("0x0.0000000000001p-1022"));
        assert_eq!(float64(0xfff8_0000_0000_0000).as_deref(), Some("-nan"));
        assert_eq!(float64(0x7ff0_0000_0000_0001), None);
    }

    #[test]
    fn the_numeric_table_ends_at_the_last_sign_extension() {
        assert_eq!(NUMERIC[0xa7 - 0x45], "i32.wrap_i64");
        assert_eq!(NUMERIC[0xbb - 0x45], "f64.promote_f32");
        assert_eq!(NUMERIC[0xc4 - 0x45], "i64.extend32_s");
        assert_eq!(MEMORY[0x3e - 0x28].0, "i64.store32");
    }
}
