//! The reader of the `-S` text for wasm: the dialect that the `asm` module prints, back into the
//! object model that [`rucc_object::wasm::write`] encodes.
//!
//! Design: tamnd/rucc#3141, and section 6.6 of the WebAssembly notes. `rucc -S` and then `rucc -c`
//! of the text give the object that `rucc -c` gives for the C source, byte for byte. The reader can
//! give the same bytes because the printer writes every fact of the object model into the text:
//!
//! 1. The block of declarations at the top names every symbol in the order of the symbol table,
//!    with `.functype`, `.globaltype`, `.tabletype`, `.tagtype` or `.type name,@object`. The reader
//!    makes the symbols in the order in which it first sees their names.
//! 2. A defined symbol with no `.globl` and no `.weak` is local, as in the reader of LLVM.
//! 3. A relocated field in the code is padded to five bytes, and every other immediate has the
//!    shortest LEB128 form, as the translation writes them.
//! 4. The object writer gives the types in the order of their first use, so the order in which the
//!    reader interns them does not change the bytes.
//! 5. A custom section with fixups, which is a DWARF section of `-g`, has the label of its section
//!    symbol at its start. The reader makes the section symbols at their labels, after every
//!    symbol of the declarations, and the producer puts them at the end of the symbol table.
//!
//! A float constant that is a NaN with a payload of its own is printed as an integer constant and
//! a `reinterpret`. The reader makes the pair into the float constant again, which gives the same
//! value and the same bytes as the object.
//!
//! The reader takes the dialect as the printer writes it, one statement on each line. It is not a
//! general assembler for the dialect of LLVM, and it refuses with the line and the reason a
//! directive or an instruction that the printer does not write.

use std::collections::HashMap;

use crate::asm::{MEMORY, NUMERIC, SATURATING, float32, float64};
use rucc_object::wasm::{
    Custom, EXPORTED, Fixup, FuncType, Function, HIDDEN, Import, LOCAL, Module, NO_STRIP, Place,
    Producers, RETAIN, RelocKind, STRINGS, Segment, SymbolKind, TLS, TLS_SEGMENT, ValType, WEAK,
    sleb, sleb_padded, uleb, uleb_padded,
};

/// The object model of `text`.
///
/// # Errors
///
/// The number of the line, from 1, and the reason, for the first line that the reader does not
/// take.
pub(crate) fn read(text: &str) -> Result<Module, (usize, String)> {
    let mut reader = Reader::default();
    for (index, line) in text.lines().enumerate() {
        reader.line(line).map_err(|why| (index + 1, why))?;
    }
    let end = text.lines().count();
    reader.finish().map_err(|why| (end, why))
}

/// The section that the lines after a `.section` go into.
#[derive(Default)]
enum Section {
    #[default]
    None,
    /// The code of one function, `.text.<name>`.
    Text,
    /// A data segment, as its index.
    Data(usize),
    /// The constructors of this priority.
    Init(u32),
    /// A custom section of the toolchain and its payload so far, which the reader decodes.
    Custom(String, Vec<u8>),
    /// A custom section with fixups, as its index in the custom sections of the module.
    Relocated(usize),
}

/// The function that the reader is in, from its label to its `end_function`.
struct Body {
    function: Function,
    /// The number of constructs that are open, with the function itself.
    open: usize,
    /// Where the last instruction starts, and its value when it is an integer constant with no
    /// fixup, for the `reinterpret` that can follow it.
    constant: Option<(usize, i64)>,
}

#[derive(Default)]
struct Reader {
    out: Module,
    names: HashMap<String, u32>,
    /// The symbols with a `.globl` or a `.weak`.
    global: Vec<bool>,
    section: Section,
    body: Option<Body>,
    /// The names of the exports that `.export_name` gave before the function.
    exports: HashMap<u32, String>,
    custom: Vec<(String, Vec<u8>)>,
    /// The `.int32` fields of the custom sections with fixups: the section, the offset, the name
    /// and the addend. A field can name the label of a section that comes later in the text, so
    /// the fixups are made at the end.
    pending: Vec<(usize, u32, String, i32)>,
}

impl Reader {
    fn line(&mut self, line: &str) -> Result<(), String> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return Ok(());
        }
        let (op, rest) = match line.split_once(char::is_whitespace) {
            Some((op, rest)) => (op, rest.trim()),
            None => (line, ""),
        };
        if let Some(name) = op.strip_suffix(':').filter(|_| rest.is_empty()) {
            return self.label(name);
        }
        if let Some(target) = rest.strip_prefix("= ") {
            return self.alias(op, target.trim());
        }
        if op.starts_with('.') {
            return self.directive(op, rest);
        }
        let Some(body) = self.body.as_mut() else {
            return Err(format!("`{op}` is an instruction outside a function"));
        };
        let constant = body.constant.take();
        self.instruction(op, rest, constant)?;
        if self.body.as_ref().is_some_and(|body| body.open == 0) {
            let body = self.body.take().expect("the function is open");
            self.out.functions.push(body.function);
        }
        Ok(())
    }

    /// The index of the symbol `name`, which is made as an undefined data symbol when the text
    /// has not named it before.
    fn symbol(&mut self, name: &str) -> u32 {
        if let Some(&index) = self.names.get(name) {
            return index;
        }
        self.declare(name, SymbolKind::Data { place: None })
    }

    fn declare(&mut self, name: &str, kind: SymbolKind) -> u32 {
        let index = self.out.symbol(name, kind, 0);
        self.names.insert(name.to_owned(), index);
        self.global.push(false);
        index
    }

    /// The symbol `name` as a function of type `ty`, made when the text has not named it.
    fn function(&mut self, name: &str, ty: FuncType) -> Result<u32, String> {
        let ty = self.out.intern(ty);
        match self.names.get(name) {
            None => Ok(self.declare(name, SymbolKind::Function { ty, import: None })),
            Some(&index) => match &self.out.symbols[index as usize].kind {
                SymbolKind::Function { ty: had, .. } if *had == ty => Ok(index),
                SymbolKind::Function { .. } => Err(format!("`{name}` has two signatures")),
                _ => Err(format!("`{name}` is not a function")),
            },
        }
    }

    /// The function symbol `name`, which the text has declared.
    fn known_function(&self, name: &str) -> Result<u32, String> {
        match self.names.get(name) {
            Some(&index)
                if matches!(self.out.symbols[index as usize].kind, SymbolKind::Function { .. }) =>
            {
                Ok(index)
            }
            Some(_) => Err(format!("`{name}` is not a function")),
            None => Err(format!("`{name}` is not declared with `.functype`")),
        }
    }

    fn label(&mut self, name: &str) -> Result<(), String> {
        match self.section {
            Section::Text => {
                if self.body.is_some() {
                    return Err(format!("`{name}` starts a function inside a function"));
                }
                let symbol = self.known_function(name)?;
                let defined = self.out.functions.iter().any(|f| f.symbol == symbol);
                if defined || self.out.aliases.iter().any(|&(alias, _)| alias == symbol) {
                    return Err(format!("`{name}` is defined twice"));
                }
                let export = self.exports.remove(&symbol);
                let function = Function { symbol, export, ..Function::default() };
                self.body = Some(Body { function, open: 1, constant: None });
                Ok(())
            }
            Section::Data(segment) => {
                let symbol = self.symbol(name);
                let offset = self.out.segments[segment].bytes.len();
                let tls = self.out.segments[segment].flags & TLS_SEGMENT != 0;
                let entry = &mut self.out.symbols[symbol as usize];
                let SymbolKind::Data { place: place @ None } = &mut entry.kind else {
                    return Err(format!("`{name}` is not data, or is defined twice"));
                };
                *place = Some(Place {
                    segment: u32::try_from(segment).map_err(|_| "too many segments")?,
                    offset: u32::try_from(offset).map_err(|_| "a segment is too large")?,
                    size: 0,
                });
                if tls {
                    entry.flags |= TLS;
                }
                Ok(())
            }
            Section::Relocated(custom) => {
                let start = self.out.customs[custom].bytes.is_empty();
                let taken = self.out.symbols.iter().any(|s| match s.kind {
                    SymbolKind::Section { custom: c } => c as usize == custom,
                    _ => false,
                });
                if !start || taken || self.names.contains_key(name) {
                    return Err(format!("the label `{name}` is not the start of its section"));
                }
                let custom = u32::try_from(custom).map_err(|_| "too many custom sections")?;
                let symbol = self.declare(name, SymbolKind::Section { custom });
                self.out.symbols[symbol as usize].flags = LOCAL;
                Ok(())
            }
            _ => Err(format!("the label `{name}` is in no code and no data section")),
        }
    }

    fn alias(&mut self, name: &str, target: &str) -> Result<(), String> {
        let alias = self.known_function(name)?;
        let target = self.known_function(target)?;
        self.out.aliases.push((alias, target));
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn directive(&mut self, op: &str, rest: &str) -> Result<(), String> {
        match op {
            ".functype" => {
                let (name, signature) = rest.split_once(' ').ok_or("`.functype` has no type")?;
                let ty = signature_of(signature.trim())?;
                self.function(name, ty)?;
            }
            ".globaltype" => {
                let mut parts = rest.split(',').map(str::trim);
                let name = parts.next().unwrap_or_default();
                let ty = valtype(parts.next().ok_or("`.globaltype` has no type")?)?;
                let mutable = match parts.next() {
                    None => true,
                    Some("immutable") => false,
                    Some(other) => return Err(format!("`{other}` is not `immutable`")),
                };
                if self.names.contains_key(name) {
                    return Err(format!("`{name}` is declared twice"));
                }
                self.declare(name, SymbolKind::Global { ty, mutable, import: None });
            }
            ".tabletype" => {
                let (name, ty) = rest.split_once(',').ok_or("`.tabletype` has no type")?;
                if ty.trim() != "funcref" {
                    return Err("rucc writes no table other than one of `funcref`".into());
                }
                if self.names.contains_key(name) {
                    return Err(format!("`{name}` is declared twice"));
                }
                self.declare(name, SymbolKind::Table { import: None });
            }
            ".tagtype" => {
                let (name, params) = rest.split_once(' ').unwrap_or((rest, ""));
                let params = list(params)?;
                let ty = self.out.intern(FuncType { params, results: Vec::new() });
                if self.names.contains_key(name) {
                    return Err(format!("`{name}` is declared twice"));
                }
                self.declare(name, SymbolKind::Tag { ty, import: None });
            }
            ".import_module" | ".import_name" => {
                let (name, value) = rest.split_once(',').ok_or("the directive has no value")?;
                let value = quoted(value.trim())?;
                let symbol = self.known_function(name)?;
                let entry = &mut self.out.symbols[symbol as usize];
                let SymbolKind::Function { import, .. } = &mut entry.kind else { unreachable!() };
                let import = import.get_or_insert_with(|| Import {
                    module: "env".into(),
                    field: entry.name.clone(),
                });
                match op {
                    ".import_module" => import.module = value,
                    _ => import.field = value,
                }
            }
            ".export_name" => {
                let (name, value) = rest.split_once(',').ok_or("the directive has no value")?;
                let symbol = self.known_function(name)?;
                self.exports.insert(symbol, quoted(value.trim())?);
                self.out.symbols[symbol as usize].flags |= EXPORTED;
            }
            ".hidden" | ".weak" | ".globl" | ".no_dead_strip" => {
                let symbol = self.symbol(rest);
                let flag = match op {
                    ".hidden" => HIDDEN,
                    ".weak" => WEAK,
                    ".no_dead_strip" => NO_STRIP,
                    _ => 0,
                };
                self.out.symbols[symbol as usize].flags |= flag;
                if matches!(op, ".weak" | ".globl") {
                    self.global[symbol as usize] = true;
                }
            }
            ".type" => {
                let (name, kind) = rest.split_once(',').ok_or("`.type` has no kind")?;
                match kind.trim() {
                    "@function" => {
                        self.known_function(name)?;
                    }
                    "@object" => {
                        let symbol = self.symbol(name);
                        let kind = &self.out.symbols[symbol as usize].kind;
                        if !matches!(kind, SymbolKind::Data { .. }) {
                            return Err(format!("`{name}` is not data"));
                        }
                    }
                    other => return Err(format!("a symbol of type `{other}`")),
                }
            }
            ".size" => {
                let (name, size) = rest.split_once(',').ok_or("`.size` has no size")?;
                let size = u32::try_from(integer(size.trim())?).map_err(|_| "a wrong size")?;
                let symbol = self.symbol(name);
                match &mut self.out.symbols[symbol as usize].kind {
                    SymbolKind::Data { place: Some(place) } => place.size = size,
                    _ => return Err(format!("`.size` of `{name}`, which is not defined data")),
                }
            }
            ".local" => {
                let body = self.body.as_mut().ok_or("`.local` outside a function")?;
                for ty in list(rest)? {
                    match body.function.locals.last_mut() {
                        Some((count, last)) if *last == ty => *count += 1,
                        _ => body.function.locals.push((1, ty)),
                    }
                }
            }
            ".section" => self.section(rest)?,
            ".p2align" => {
                let align = rest.split(',').next().unwrap_or_default().trim();
                let align = u32::try_from(integer(align)?).map_err(|_| "a wrong alignment")?;
                match self.section {
                    Section::Data(segment) => {
                        let segment = &mut self.out.segments[segment];
                        let step = 1usize.checked_shl(align).ok_or("a wrong alignment")?;
                        segment.bytes.resize(segment.bytes.len().next_multiple_of(step), 0);
                        segment.align = segment.align.max(align);
                    }
                    Section::Init(_) if align == 2 => {}
                    _ => return Err("`.p2align` where rucc writes none".into()),
                }
            }
            ".int32" => match self.section {
                Section::Data(segment) => {
                    let at = self.out.segments[segment].bytes.len();
                    let at = u32::try_from(at).map_err(|_| "a segment is too large")?;
                    let bytes = match integer(rest) {
                        Ok(value) => i32::try_from(value)
                            .map(|v| v.to_le_bytes())
                            .or_else(|_| u32::try_from(value).map(|v| v.to_le_bytes()))
                            .map_err(|_| "the value does not fit in 32 bits")?,
                        Err(_) => {
                            let (symbol, addend) = self.address(rest)?;
                            let kind = match self.out.symbols[symbol as usize].kind {
                                SymbolKind::Function { .. } if addend == 0 => {
                                    RelocKind::TableIndexI32
                                }
                                SymbolKind::Data { .. } => RelocKind::MemoryAddrI32,
                                _ => return Err(format!("`{rest}` is not an address")),
                            };
                            let fixup = Fixup { at, kind, target: symbol, addend };
                            self.out.segments[segment].fixups.push(fixup);
                            [0; 4]
                        }
                    };
                    self.out.segments[segment].bytes.extend_from_slice(&bytes);
                }
                Section::Init(priority) => {
                    let symbol = self.known_function(rest)?;
                    self.out.inits.push((priority, symbol));
                }
                Section::Relocated(custom) => {
                    let bytes = &mut self.out.customs[custom].bytes;
                    match integer(rest) {
                        Ok(value) => {
                            let value = u32::try_from(value)
                                .or_else(|_| i32::try_from(value).map(|v| v as u32))
                                .map_err(|_| "the value does not fit in 32 bits")?;
                            bytes.extend_from_slice(&value.to_le_bytes());
                        }
                        Err(_) => {
                            let at =
                                u32::try_from(bytes.len()).map_err(|_| "a section too large")?;
                            let (name, addend) = split(rest)?;
                            self.pending.push((custom, at, name.to_owned(), addend));
                            bytes.extend_from_slice(&[0; 4]);
                        }
                    }
                }
                _ => return Err("`.int32` outside data".into()),
            },
            ".skip" | ".ascii" | ".asciz" => {
                let bytes = if op == ".skip" {
                    let count = usize::try_from(integer(rest)?).map_err(|_| "a wrong count")?;
                    vec![0; count]
                } else {
                    let mut bytes = string(rest)?;
                    if op == ".asciz" {
                        bytes.push(0);
                    }
                    bytes
                };
                match &mut self.section {
                    Section::Data(segment) => {
                        self.out.segments[*segment].bytes.extend_from_slice(&bytes);
                    }
                    Section::Custom(_, payload) => payload.extend_from_slice(&bytes),
                    Section::Relocated(custom) => {
                        self.out.customs[*custom].bytes.extend_from_slice(&bytes);
                    }
                    _ => return Err(format!("`{op}` outside data")),
                }
            }
            _ => return Err(format!("the directive `{op}` is not one that rucc writes")),
        }
        Ok(())
    }

    /// `.section name,"flags",@`, which starts the code of a function, a data segment, the
    /// constructors of one priority or a custom section.
    fn section(&mut self, rest: &str) -> Result<(), String> {
        if self.body.is_some() {
            return Err("a `.section` inside a function".into());
        }
        self.close();
        let (name, rest) = rest.split_once(',').ok_or("`.section` has no flags")?;
        let flags = rest.strip_suffix(",@").map(str::trim).ok_or("`.section` is not of `@`")?;
        let flags = quoted(flags)?;
        self.section = if name.starts_with(".text.") {
            Section::Text
        } else if let Some(priority) = name.strip_prefix(".init_array") {
            let priority = match priority.strip_prefix('.') {
                Some(number) => u32::try_from(integer(number)?).map_err(|_| "a wrong priority")?,
                None if priority.is_empty() => 65_535,
                None => return Err(format!("the section `{name}`")),
            };
            Section::Init(priority)
        } else if let Some(custom) = name
            .strip_prefix(".custom_section.")
            .filter(|&n| matches!(n, "producers" | "target_features"))
        {
            Section::Custom(custom.to_owned(), Vec::new())
        } else if name.starts_with(".debug_") || name.starts_with(".custom_section.") {
            let name = name.strip_prefix(".custom_section.").unwrap_or(name);
            let custom = Custom { name: name.to_owned(), ..Custom::default() };
            self.out.customs.push(custom);
            Section::Relocated(self.out.customs.len() - 1)
        } else {
            let mut bits = 0;
            for flag in flags.chars() {
                bits |= match flag {
                    'S' => STRINGS,
                    'T' => TLS_SEGMENT,
                    'R' => RETAIN,
                    _ => return Err(format!("the section flag `{flag}`")),
                };
            }
            let found = self.out.segments.iter().position(|s| s.name == name);
            let index = found.unwrap_or_else(|| {
                let segment = Segment { name: name.to_owned(), flags: bits, ..Segment::default() };
                self.out.segments.push(segment);
                self.out.segments.len() - 1
            });
            Section::Data(index)
        };
        Ok(())
    }

    /// Ends the section that is open, which keeps the payload of a custom section.
    fn close(&mut self) {
        if let Section::Custom(name, payload) = std::mem::take(&mut self.section) {
            self.custom.push((name, payload));
        }
    }

    /// A symbol and the offset from it: `name`, `name+N` or `name-N`.
    fn address(&mut self, text: &str) -> Result<(u32, i32), String> {
        let (name, addend) = split(text)?;
        Ok((self.symbol(name), addend))
    }

    /// One instruction of the body that is open. `constant` is the integer constant just before
    /// it, which a `reinterpret` turns back into the float constant.
    #[allow(clippy::too_many_lines)]
    fn instruction(
        &mut self,
        op: &str,
        rest: &str,
        constant: Option<(usize, i64)>,
    ) -> Result<(), String> {
        let mut code = Vec::new();
        let mut fixups: Vec<(usize, RelocKind, u32, i32)> = Vec::new();
        let mut open = 0isize;
        let mut kept = None;
        match op {
            "unreachable" => code.push(0x00),
            "nop" => code.push(0x01),
            "block" | "loop" | "if" => {
                code.push(match op {
                    "block" => 0x02,
                    "loop" => 0x03,
                    _ => 0x04,
                });
                code.push(blocktype(rest)?);
                open = 1;
            }
            "else" => code.push(0x05),
            "end_block" | "end_loop" | "end_if" | "end_try_table" | "end_function" => {
                code.push(0x0b);
                open = -1;
            }
            "try_table" => {
                let (ty, clauses) = match rest.split_once('(') {
                    Some((ty, _)) => (ty.trim(), &rest[ty.len()..]),
                    None => (rest, ""),
                };
                code.push(0x1f);
                code.push(blocktype(ty)?);
                let clauses: Vec<&str> = clauses
                    .split(')')
                    .map(|c| c.trim().trim_start_matches('('))
                    .filter(|c| !c.is_empty())
                    .collect();
                uleb(&mut code, clauses.len() as u64);
                for clause in clauses {
                    let words: Vec<&str> = clause.split_whitespace().collect();
                    let (kind, tag, label) = match words[..] {
                        ["catch", tag, label] => (0x00, Some(tag), label),
                        ["catch_ref", tag, label] => (0x01, Some(tag), label),
                        ["catch_all", label] => (0x02, None, label),
                        ["catch_all_ref", label] => (0x03, None, label),
                        _ => return Err(format!("the catch clause `({clause})`")),
                    };
                    code.push(kind);
                    if let Some(tag) = tag {
                        let symbol = self.symbol(tag);
                        if !matches!(self.out.symbols[symbol as usize].kind, SymbolKind::Tag { .. })
                        {
                            return Err(format!("`{tag}` is not a tag"));
                        }
                        fixups.push((code.len(), RelocKind::TagIndexLeb, symbol, 0));
                        code.extend_from_slice(&uleb_padded(0));
                    }
                    uleb(&mut code, unsigned(label)?);
                }
                open = 1;
            }
            "br" | "br_if" => {
                code.push(if op == "br" { 0x0c } else { 0x0d });
                uleb(&mut code, unsigned(rest)?);
            }
            "br_table" => {
                let inner = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}'));
                let depths = inner.ok_or("`br_table` has no list in braces")?;
                let depths: Vec<&str> = depths.split(',').map(str::trim).collect();
                code.push(0x0e);
                uleb(&mut code, depths.len() as u64 - 1);
                for depth in depths {
                    uleb(&mut code, unsigned(depth)?);
                }
            }
            "return" => code.push(0x0f),
            "call" | "return_call" => {
                code.push(if op == "call" { 0x10 } else { 0x12 });
                let symbol = self.known_function(rest)?;
                fixups.push((code.len(), RelocKind::FunctionIndexLeb, symbol, 0));
                code.extend_from_slice(&uleb_padded(0));
            }
            "call_indirect" | "return_call_indirect" => {
                code.push(if op == "call_indirect" { 0x11 } else { 0x13 });
                let (table, signature) = match rest.strip_prefix('(') {
                    Some(_) => (None, rest),
                    None => {
                        let (table, signature) =
                            rest.split_once(',').ok_or("`call_indirect` has no type")?;
                        (Some(table), signature.trim())
                    }
                };
                let ty = self.out.intern(signature_of(signature)?);
                fixups.push((code.len(), RelocKind::TypeIndexLeb, ty, 0));
                code.extend_from_slice(&uleb_padded(0));
                match table {
                    Some(name) => {
                        let symbol = self.symbol(name);
                        let kind = &self.out.symbols[symbol as usize].kind;
                        if !matches!(kind, SymbolKind::Table { .. }) {
                            return Err(format!("`{name}` is not a table"));
                        }
                        fixups.push((code.len(), RelocKind::TableNumberLeb, symbol, 0));
                        code.extend_from_slice(&uleb_padded(0));
                    }
                    None => code.push(0),
                }
            }
            "drop" => code.push(0x1a),
            "i32.select" | "i64.select" | "f32.select" | "f64.select" => code.push(0x1b),
            "local.get" | "local.set" | "local.tee" => {
                code.push(match op {
                    "local.get" => 0x20,
                    "local.set" => 0x21,
                    _ => 0x22,
                });
                uleb(&mut code, unsigned(rest)?);
            }
            "global.get" | "global.set" => {
                code.push(if op == "global.get" { 0x23 } else { 0x24 });
                let symbol = self.symbol(rest);
                if !matches!(self.out.symbols[symbol as usize].kind, SymbolKind::Global { .. }) {
                    return Err(format!("`{rest}` is not a global"));
                }
                fixups.push((code.len(), RelocKind::GlobalIndexLeb, symbol, 0));
                code.extend_from_slice(&uleb_padded(0));
            }
            "memory.size" | "memory.grow" => {
                if rest != "0" {
                    return Err(format!("`{op}` of a memory other than zero"));
                }
                code.extend_from_slice(if op == "memory.size" { &[0x3f, 0] } else { &[0x40, 0] });
            }
            "memory.copy" | "memory.fill" => {
                code.push(0xfc);
                if op == "memory.copy" {
                    if rest != "0, 0" {
                        return Err("`memory.copy` of a memory other than zero".into());
                    }
                    code.extend_from_slice(&[10, 0, 0]);
                } else {
                    if rest != "0" {
                        return Err("`memory.fill` of a memory other than zero".into());
                    }
                    code.extend_from_slice(&[11, 0]);
                }
            }
            "i32.const" => {
                code.push(0x41);
                if let Ok(value) = integer(rest) {
                    let value = i32::try_from(value)
                        .or_else(|_| u32::try_from(value).map(|v| v as i32))
                        .map_err(|_| "the constant does not fit in 32 bits")?;
                    sleb(&mut code, i64::from(value));
                    kept = Some(i64::from(value));
                } else if let Some((name, offset)) = tlsrel(rest) {
                    let symbol = self.symbol(name);
                    self.out.symbols[symbol as usize].flags |= TLS;
                    let addend = match offset {
                        "" => 0,
                        offset => i32::try_from(integer(offset.trim_start_matches('+'))?)
                            .map_err(|_| "the offset does not fit in 32 bits")?,
                    };
                    fixups.push((code.len(), RelocKind::MemoryAddrTlsSleb, symbol, addend));
                    code.extend_from_slice(&sleb_padded(0));
                } else {
                    let (symbol, addend) = self.address(rest)?;
                    let kind = match self.out.symbols[symbol as usize].kind {
                        SymbolKind::Function { .. } if addend == 0 => RelocKind::TableIndexSleb,
                        SymbolKind::Data { .. } => RelocKind::MemoryAddrSleb,
                        _ => return Err(format!("`{rest}` is not an address")),
                    };
                    fixups.push((code.len(), kind, symbol, addend));
                    code.extend_from_slice(&sleb_padded(0));
                }
            }
            "i64.const" => {
                code.push(0x42);
                let value = integer(rest)?;
                let value = i64::try_from(value)
                    .or_else(|_| u64::try_from(value).map(|v| v as i64))
                    .map_err(|_| "the constant does not fit in 64 bits")?;
                sleb(&mut code, value);
                kept = Some(value);
            }
            "f32.const" => {
                code.push(0x43);
                code.extend_from_slice(&(float(rest, 23, 8)? as u32).to_le_bytes());
            }
            "f64.const" => {
                code.push(0x44);
                code.extend_from_slice(&float(rest, 52, 11)?.to_le_bytes());
            }
            "f32.reinterpret_i32" | "f64.reinterpret_i64" if self.folds(op, constant) => {
                let (start, value) = constant.expect("`folds` checked it");
                let body = self.body.as_mut().expect("the function is open");
                body.function.code.truncate(start);
                if op == "f32.reinterpret_i32" {
                    body.function.code.push(0x43);
                    body.function.code.extend_from_slice(&(value as u32).to_le_bytes());
                } else {
                    body.function.code.push(0x44);
                    body.function.code.extend_from_slice(&(value as u64).to_le_bytes());
                }
                return Ok(());
            }
            _ => {
                if let Some(at) = MEMORY.iter().position(|&(name, ..)| name == op) {
                    let (offset, align) = match rest.split_once(":p2align=") {
                        Some((offset, align)) => (offset, unsigned(align)?),
                        None => (rest, u64::from(MEMORY[at].1)),
                    };
                    code.push(0x28 + u8::try_from(at).expect("23 loads and stores"));
                    uleb(&mut code, align);
                    if let Ok(value) = integer(offset) {
                        let value = u32::try_from(value).map_err(|_| "a wrong offset")?;
                        uleb(&mut code, u64::from(value));
                    } else {
                        let (symbol, addend) = self.address(offset)?;
                        if !matches!(
                            self.out.symbols[symbol as usize].kind,
                            SymbolKind::Data { .. }
                        ) {
                            return Err(format!("`{offset}` is not an address"));
                        }
                        fixups.push((code.len(), RelocKind::MemoryAddrLeb, symbol, addend));
                        code.extend_from_slice(&uleb_padded(0));
                    }
                } else if let Some(at) = NUMERIC.iter().position(|&name| name == op) {
                    code.push(0x45 + u8::try_from(at).expect("128 numeric instructions"));
                } else if let Some(at) = SATURATING.iter().position(|&name| name == op) {
                    code.push(0xfc);
                    uleb(&mut code, at as u64);
                } else {
                    return Err(format!("the instruction `{op}` is not one that rucc writes"));
                }
            }
        }
        if !rest.is_empty() && !takes_operand(op) {
            return Err(format!("`{op}` takes no operand"));
        }
        let body = self.body.as_mut().expect("the function is open");
        let start = body.function.code.len();
        for (at, kind, target, addend) in fixups {
            let at = u32::try_from(start + at).map_err(|_| "a function is too large")?;
            body.function.fixups.push(Fixup { at, kind, target, addend });
        }
        body.function.code.extend_from_slice(&code);
        body.open = body.open.checked_add_signed(open).ok_or("an `end` with nothing open")?;
        if op == "end_function" && body.open != 0 {
            return Err("`end_function` with a construct open".into());
        }
        if body.open == 0 && op != "end_function" {
            return Err(format!("`{op}` closes the function, which `end_function` does"));
        }
        body.constant = kept.map(|value| (start, value));
        Ok(())
    }

    /// Whether `op` with the integer constant before it is the pair that the printer writes for a
    /// NaN with a payload of its own.
    fn folds(&self, op: &str, constant: Option<(usize, i64)>) -> bool {
        let Some((start, value)) = constant else { return false };
        let code = &self.body.as_ref().expect("the function is open").function.code;
        match op {
            "f32.reinterpret_i32" => code[start] == 0x41 && float32(value as u32).is_none(),
            _ => code[start] == 0x42 && float64(value as u64).is_none(),
        }
    }

    /// The module, once every line is read: the local symbols and the custom sections.
    fn finish(mut self) -> Result<Module, String> {
        if self.body.is_some() {
            return Err("the text ends inside a function".into());
        }
        self.close();
        if let Some((&symbol, _)) = self.exports.iter().next() {
            let name = &self.out.symbols[symbol as usize].name;
            return Err(format!("`.export_name` of `{name}`, which the text does not define"));
        }
        let mut defined = vec![false; self.out.symbols.len()];
        for function in &self.out.functions {
            defined[function.symbol as usize] = true;
        }
        for &(alias, _) in &self.out.aliases {
            defined[alias as usize] = true;
        }
        for (index, symbol) in self.out.symbols.iter_mut().enumerate() {
            let data = matches!(symbol.kind, SymbolKind::Data { place: Some(_) });
            if (defined[index] || data) && !self.global[index] {
                symbol.flags |= LOCAL;
            }
        }
        // A field of a custom section with fixups is an offset into the code for a function, an
        // offset into a section for the label of a section, and an address for data.
        for (custom, at, name, addend) in std::mem::take(&mut self.pending) {
            let Some(&target) = self.names.get(&name) else {
                return Err(format!("`{name}` in a custom section is not declared"));
            };
            let kind = match self.out.symbols[target as usize].kind {
                SymbolKind::Function { .. } => RelocKind::FunctionOffsetI32,
                SymbolKind::Section { .. } => RelocKind::SectionOffsetI32,
                SymbolKind::Data { .. } => RelocKind::MemoryAddrI32,
                _ => return Err(format!("`{name}` in a custom section is not an address")),
            };
            self.out.customs[custom].fixups.push(Fixup { at, kind, target, addend });
        }
        for (name, payload) in std::mem::take(&mut self.custom) {
            let mut at = Payload { bytes: &payload, at: 0 };
            match name.as_str() {
                "producers" => self.out.producers = at.producers()?,
                "target_features" => {
                    for _ in 0..at.uleb()? {
                        let prefix = at.byte()?;
                        let feature = at.name()?;
                        match prefix {
                            b'+' => self.out.features.push(feature),
                            b'-' => self.out.disallowed.push(feature),
                            _ => return Err(format!("the feature prefix {prefix:#04x}")),
                        }
                    }
                }
                _ => return Err(format!("the custom section `{name}` is not one rucc writes")),
            }
            if at.at != payload.len() {
                return Err(format!("the custom section `{name}` has bytes after its end"));
            }
        }
        Ok(self.out)
    }
}

/// A symbol and the offset from it, `name`, `name+N` or `name-N`, as the name and the offset.
fn split(text: &str) -> Result<(&str, i32), String> {
    let at = text.rfind(['+', '-']).filter(|&at| at > 0);
    let (name, addend) = match at.map(|at| text.split_at(at)) {
        Some((name, offset)) if integer(offset.trim_start_matches('+')).is_ok() => {
            let offset = integer(offset.trim_start_matches('+'))?;
            (name, i32::try_from(offset).map_err(|_| "the offset does not fit in 32 bits")?)
        }
        _ => (text, 0),
    };
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        return Err(format!("`{text}` is not a symbol"));
    }
    Ok((name, addend))
}

/// The payload of a custom section, as it is decoded.
struct Payload<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Payload<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let byte = *self.bytes.get(self.at).ok_or("a custom section ends too early")?;
        self.at += 1;
        Ok(byte)
    }

    fn uleb(&mut self) -> Result<u32, String> {
        let mut value = 0u64;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return u32::try_from(value).map_err(|_| "a count does not fit in 32 bits".into());
            }
        }
        Err("a LEB128 field is longer than five bytes".into())
    }

    fn name(&mut self) -> Result<String, String> {
        let len = self.uleb()? as usize;
        let end = self.at.checked_add(len).filter(|&end| end <= self.bytes.len());
        let end = end.ok_or("a custom section ends inside a name")?;
        let name = std::str::from_utf8(&self.bytes[self.at..end]).map_err(|_| "a name in UTF-8")?;
        self.at = end;
        Ok(name.to_owned())
    }

    fn producers(&mut self) -> Result<Producers, String> {
        let mut producers = Producers::default();
        for _ in 0..self.uleb()? {
            let field = self.name()?;
            for _ in 0..self.uleb()? {
                let (value, version) = (self.name()?, self.name()?);
                match field.as_str() {
                    "language" if producers.language.is_none() && version.is_empty() => {
                        producers.language = Some(value);
                    }
                    "processed-by" => producers.processed_by.push((value, version)),
                    _ => return Err(format!("the producers field `{field}`")),
                }
            }
        }
        Ok(producers)
    }
}

/// Whether the instruction `op` has an operand in the text.
fn takes_operand(op: &str) -> bool {
    matches!(
        op,
        "block"
            | "loop"
            | "if"
            | "try_table"
            | "br"
            | "br_if"
            | "br_table"
            | "call"
            | "return_call"
            | "call_indirect"
            | "return_call_indirect"
            | "local.get"
            | "local.set"
            | "local.tee"
            | "global.get"
            | "global.set"
            | "memory.size"
            | "memory.grow"
            | "memory.copy"
            | "memory.fill"
            | "i32.const"
            | "i64.const"
            | "f32.const"
            | "f64.const"
    ) || MEMORY.iter().any(|&(name, ..)| name == op)
}

/// `name@TLSREL` and the offset after it, as the name and the text of the offset.
fn tlsrel(text: &str) -> Option<(&str, &str)> {
    let (name, rest) = text.split_once("@TLSREL")?;
    Some((name, rest))
}

fn valtype(text: &str) -> Result<ValType, String> {
    Ok(match text {
        "i32" => ValType::I32,
        "i64" => ValType::I64,
        "f32" => ValType::F32,
        "f64" => ValType::F64,
        _ => return Err(format!("`{text}` is not a value type")),
    })
}

/// A list of value types with commas between them, which can be empty.
fn list(text: &str) -> Result<Vec<ValType>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    text.split(',').map(|ty| valtype(ty.trim())).collect()
}

/// `(params) -> (results)`.
fn signature_of(text: &str) -> Result<FuncType, String> {
    let wrong = || format!("`{text}` is not a signature");
    let (params, results) = text.split_once("->").ok_or_else(wrong)?;
    let inner = |part: &str| {
        let part = part.trim().strip_prefix('(').and_then(|p| p.strip_suffix(')'));
        part.ok_or_else(wrong).and_then(list)
    };
    Ok(FuncType { params: inner(params)?, results: inner(results)? })
}

/// The block type byte of the type after `block`, `loop`, `if` or `try_table`.
fn blocktype(text: &str) -> Result<u8, String> {
    if text.is_empty() { Ok(0x40) } else { Ok(valtype(text)?.byte()) }
}

/// A number in decimal or in hexadecimal after `0x`, with a sign or not.
fn integer(text: &str) -> Result<i128, String> {
    let wrong = || format!("`{text}` is not a number");
    let (negative, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text),
    };
    let value = match digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) {
        Some(hex) => i128::from_str_radix(hex, 16).map_err(|_| wrong())?,
        None if digits.starts_with(|c: char| c.is_ascii_digit()) => {
            digits.parse::<i128>().map_err(|_| wrong())?
        }
        None => return Err(wrong()),
    };
    Ok(if negative { -value } else { value })
}

fn unsigned(text: &str) -> Result<u64, String> {
    u64::try_from(integer(text)?).map_err(|_| format!("`{text}` is not an unsigned number"))
}

/// The bits of a float constant with `fraction` bits of fraction and `exponent` bits of
/// exponent: a hexadecimal float, which must be exact, `infinity`, `nan` or a decimal number,
/// which is rounded to the nearest float.
fn float(text: &str, fraction: u32, exponent: u32) -> Result<u64, String> {
    let wrong = || format!("`{text}` is not a float constant that rucc reads");
    let (sign, magnitude) = match text.strip_prefix('-') {
        Some(rest) => (1u64 << (fraction + exponent), rest),
        None => (0, text),
    };
    let ones = (1u64 << exponent) - 1;
    let bits = match magnitude {
        "infinity" | "inf" => ones << fraction,
        "nan" => (ones << fraction) | (1 << (fraction - 1)),
        _ if magnitude.starts_with("0x") || magnitude.starts_with("0X") => {
            hexadecimal(&magnitude[2..], fraction, exponent).ok_or_else(wrong)?
        }
        _ if fraction == 23 => u64::from(magnitude.parse::<f32>().map_err(|_| wrong())?.to_bits()),
        _ => magnitude.parse::<f64>().map_err(|_| wrong())?.to_bits(),
    };
    Ok(sign | bits)
}

/// The bits of the hexadecimal float `digits` after the `0x`, with no sign, or nothing when it
/// is not exact in the format.
fn hexadecimal(digits: &str, fraction: u32, exponent: u32) -> Option<u64> {
    let (mantissa, power) = digits.split_once(['p', 'P'])?;
    let power: i64 = power.parse().ok()?;
    let (whole, part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.len() + part.len() > 30 || whole.len() + part.len() == 0 {
        return None;
    }
    let value = u128::from_str_radix(&format!("{whole}{part}"), 16).ok()?;
    if value == 0 {
        return Some(0);
    }
    // The value is `value * 2^scale`.
    let scale = power - 4 * part.len() as i64;
    let top = i64::from(127 - value.leading_zeros());
    let bias = (1i64 << (exponent - 1)) - 1;
    let unbiased = top + scale;
    let (biased, shift) = if unbiased >= 1 - bias {
        (unbiased + bias, top - i64::from(fraction))
    } else {
        (0, -(scale + bias - 1 + i64::from(fraction)))
    };
    if biased >= (1i64 << exponent) - 1 {
        return None;
    }
    let field = if shift > 0 {
        let shift = u32::try_from(shift).ok().filter(|&s| s < 128)?;
        if value & ((1u128 << shift) - 1) != 0 {
            return None;
        }
        value >> shift
    } else {
        value.checked_shl(u32::try_from(-shift).ok()?)?
    };
    let field = u64::try_from(field & ((1u128 << fraction) - 1)).ok()?;
    Some((u64::try_from(biased).ok()? << fraction) | field)
}

/// The text between the quotes of `"..."`, which has no escapes.
fn quoted(text: &str) -> Result<String, String> {
    let inner = text.strip_prefix('"').and_then(|t| t.strip_suffix('"'));
    let inner = inner.ok_or_else(|| format!("`{text}` is not in quotes"))?;
    String::from_utf8(string(text)?).map_err(|_| format!("`{inner}` is not UTF-8"))
}

/// The bytes of a string in quotes, with the escapes of the assembler: a backslash and one to
/// three octal digits, `\\`, `\"`, `\n`, `\t`, `\r` and `\xNN`.
fn string(text: &str) -> Result<Vec<u8>, String> {
    let inner = text.strip_prefix('"').and_then(|t| t.strip_suffix('"'));
    let inner = inner.ok_or_else(|| format!("`{text}` is not in quotes"))?;
    let mut out = Vec::new();
    let mut bytes = inner.bytes().peekable();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let escape = bytes.next().ok_or("a string ends in a backslash")?;
        match escape {
            b'0'..=b'7' => {
                let mut value = u32::from(escape - b'0');
                for _ in 0..2 {
                    match bytes.next_if(|b| (b'0'..=b'7').contains(b)) {
                        Some(digit) => value = value * 8 + u32::from(digit - b'0'),
                        None => break,
                    }
                }
                out.push(u8::try_from(value).map_err(|_| "an octal escape over 255")?);
            }
            b'x' => {
                let mut value = 0u32;
                while let Some(digit) = bytes.next_if(u8::is_ascii_hexdigit) {
                    value = (value * 16 + char::from(digit).to_digit(16).unwrap_or(0)) & 0xff;
                }
                out.push(value as u8);
            }
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'\\' | b'"' => out.push(escape),
            _ => return Err(format!("the escape `\\{}`", char::from(escape))),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A function, a variable and two DWARF sections, one with a field of each kind of fixup. The
    /// section symbol of the second section comes first in the text and is named before its label.
    fn described() -> Module {
        let mut m = Module::default();
        let ty = m.intern(FuncType::default());
        let f = m.symbol("f", SymbolKind::Function { ty, import: None }, HIDDEN);
        m.functions.push(Function {
            symbol: f,
            locals: vec![(1, ValType::I32)],
            code: vec![0x01, 0x0b],
            ..Function::default()
        });
        m.segments.push(Segment {
            name: ".bss.v".into(),
            align: 2,
            bytes: vec![0; 4],
            ..Segment::default()
        });
        let place = Some(Place { segment: 0, offset: 0, size: 4 });
        let v = m.symbol("v", SymbolKind::Data { place }, LOCAL);
        let fixups = vec![
            Fixup { at: 4, kind: RelocKind::SectionOffsetI32, target: 3, addend: 6 },
            Fixup { at: 8, kind: RelocKind::FunctionOffsetI32, target: f, addend: 2 },
            Fixup { at: 14, kind: RelocKind::MemoryAddrI32, target: v, addend: 0 },
        ];
        let info = Custom {
            name: ".debug_info".into(),
            bytes: vec![1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9, 9, 0, 0, 0, 0, 5],
            fixups,
        };
        m.customs.push(info);
        m.customs.push(Custom {
            name: ".debug_str".into(),
            bytes: b"\0int\0f\0".to_vec(),
            fixups: vec![],
        });
        m.symbol(".Ldebug_info", SymbolKind::Section { custom: 0 }, LOCAL);
        m.symbol(".Ldebug_str", SymbolKind::Section { custom: 1 }, LOCAL);
        m
    }

    #[test]
    fn a_dwarf_section_reads_back_with_its_fixups() {
        let m = described();
        let text = crate::asm::print(&m).unwrap();
        assert!(text.contains("\t.section\t.debug_str,\"S\",@\n.Ldebug_str:\n"), "{text}");
        assert!(text.contains("\t.int32\t.Ldebug_str+6\n\t.int32\tf+2\n"), "{text}");
        assert_eq!(read(&text), Ok(m));
    }

    #[test]
    fn a_label_inside_a_dwarf_section_is_refused() {
        let text = "\t.section\t.debug_info,\"\",@\n\t.ascii\t\"a\"\n.La:\n";
        assert_eq!(read(text), Err((3, "the label `.La` is not the start of its section".into())));
        let text = "\t.section\t.debug_info,\"\",@\n\t.int32\tg\n";
        assert_eq!(read(text), Err((2, "`g` in a custom section is not declared".into())));
    }

    #[test]
    fn a_hexadecimal_float_is_read_exactly() {
        let f32 = |text| float(text, 23, 8).map(|bits| bits as u32);
        assert_eq!(f32("0x1.8p0"), Ok(1.5f32.to_bits()));
        assert_eq!(f32("-0x0p0"), Ok((-0.0f32).to_bits()));
        assert_eq!(f32("0x0.000002p-126"), Ok(1));
        assert_eq!(f32("0x1.fffffep127"), Ok(f32::MAX.to_bits()));
        assert_eq!(f32("-infinity"), Ok(f32::NEG_INFINITY.to_bits()));
        assert_eq!(f32("nan"), Ok(0x7fc0_0000));
        assert_eq!(f32("2.5"), Ok(2.5f32.to_bits()));
        assert!(f32("0x1.0000001p0").is_err());
        assert!(f32("0x1p128").is_err());
        assert_eq!(float("0x1.999999999999ap-4", 52, 11), Ok(0.1f64.to_bits()));
        assert_eq!(float("0x0.0000000000001p-1022", 52, 11), Ok(1));
        assert_eq!(float("-nan", 52, 11), Ok(0xfff8_0000_0000_0000));
    }

    #[test]
    fn every_float_that_the_printer_writes_is_read_back() {
        let samples = [0u32, 1, 0x7f_ffff, 0x80_0000, 0x3f80_0000, 0x7f7f_ffff, 0x8000_0001];
        for bits in samples.into_iter().chain((0..4096).map(|i: u32| i.wrapping_mul(0x9e37_79b9))) {
            if let Some(text) = float32(bits) {
                assert_eq!(float(&text, 23, 8), Ok(u64::from(bits)), "{text}");
            }
            let wide = u64::from(bits) << 32 | u64::from(bits.rotate_left(7));
            if let Some(text) = float64(wide) {
                assert_eq!(float(&text, 52, 11), Ok(wide), "{text}");
            }
        }
    }

    #[test]
    fn a_string_has_the_escapes_of_the_assembler() {
        assert_eq!(string(r#""hi\012\000\"\\""#), Ok(b"hi\n\0\"\\".to_vec()));
        assert_eq!(string(r#""\x41\n""#), Ok(b"A\n".to_vec()));
        assert!(string("hi").is_err());
    }

    #[test]
    fn an_address_is_a_symbol_and_an_offset() {
        let mut reader = Reader::default();
        assert_eq!(reader.address("table"), Ok((0, 0)));
        assert_eq!(reader.address("table+8"), Ok((0, 8)));
        assert_eq!(reader.address(".Lstr.1-4"), Ok((1, -4)));
        assert!(reader.address("12").is_err());
    }
}
