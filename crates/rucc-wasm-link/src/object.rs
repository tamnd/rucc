//! The relocatable object, as rucc and clang write it.
//!
//! Design: section 9.3 of the WebAssembly notes, and the object format of the tool conventions
//! (`Linking.md`) as LLD 23 reads it. The reader keeps every byte it does not need to change as a
//! slice of the input, so a function body is copied once, when the output is written, and the
//! relocations are applied to that copy.
//!
//! An object holds what a static C link needs: types, imports, functions, globals and tags, the
//! code, the data, the `linking` section and the `reloc.CODE` and `reloc.DATA` sections. A defined
//! memory or table, a start function and a `dylink.0` section are for a shared library or for an
//! input this linker does not cover, and the reader refuses them with a message that names wasm-ld.
//! The DWARF sections and their relocations, the element section and the data count are read past
//! and dropped.

use crate::Error;
use crate::bytes::Cursor;

/// The symbol is weak.
pub const WEAK: u32 = 0x1;
/// The symbol is local to its object.
pub const LOCAL: u32 = 0x2;
/// The symbol is not defined in its object.
pub const UNDEFINED: u32 = 0x10;
/// The symbol is exported from the module.
pub const EXPORTED: u32 = 0x20;
/// The symbol of an import has its own name, and the import field is not the symbol name.
pub const EXPLICIT_NAME: u32 = 0x40;
/// The linker keeps the symbol when nothing refers to it.
pub const NO_STRIP: u32 = 0x80;

/// The segment holds strings that the linker can merge.
pub const STRINGS: u32 = 0x1;
/// The segment is thread-local.
pub const TLS: u32 = 0x2;
/// The linker keeps the segment when nothing refers to it.
pub const RETAIN: u32 = 0x4;

/// The version of the `linking` section that this reader reads.
const LINKING_VERSION: u32 = 2;

/// The kind of a symbol, with the number the symbol table gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Function,
    Data,
    Global,
    Section,
    Tag,
    Table,
}

/// One entry of the symbol table.
#[derive(Debug, Clone)]
pub struct Symbol<'a> {
    pub kind: Kind,
    pub flags: u32,
    /// The name. A section symbol has none.
    pub name: &'a str,
    /// The index in the space of the kind: the function, global, tag or table index, the
    /// segment of a defined data symbol, or the section of a section symbol.
    pub index: u32,
    /// The offset of a defined data symbol in its segment.
    pub offset: u32,
    /// The size of a defined data symbol.
    pub size: u32,
}

impl Symbol<'_> {
    #[must_use]
    pub fn is_undefined(&self) -> bool {
        self.flags & UNDEFINED != 0
    }

    #[must_use]
    pub fn is_weak(&self) -> bool {
        self.flags & WEAK != 0
    }

    #[must_use]
    pub fn is_local(&self) -> bool {
        self.flags & LOCAL != 0
    }
}

/// An import: the module and field names, and the type.
#[derive(Debug, Clone)]
pub struct Import<'a, T> {
    pub module: &'a str,
    pub field: &'a str,
    pub ty: T,
}

/// A defined global: its type (the value type and the mutability byte) and its initializer.
#[derive(Debug, Clone)]
pub struct Global<'a> {
    pub ty: &'a [u8],
    pub init: &'a [u8],
}

/// A function body, without its size, and its offset in the payload of the code section.
#[derive(Debug, Clone)]
pub struct Body<'a> {
    pub offset: u32,
    pub bytes: &'a [u8],
}

/// A data segment: its bytes, their offset in the payload of the data section, and the facts
/// the `linking` section gives about it.
#[derive(Debug, Clone)]
pub struct Segment<'a> {
    pub offset: u32,
    pub bytes: &'a [u8],
    pub name: &'a str,
    /// The alignment as a power of two.
    pub align: u32,
    pub flags: u32,
}

/// One relocation: the type, the offset of the field in the payload of its section, the symbol
/// (or the type, for `R_WASM_TYPE_INDEX_LEB`), and the addend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reloc {
    pub kind: u8,
    pub offset: u32,
    pub index: u32,
    pub addend: i64,
}

/// A constructor: its priority and its symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Init {
    pub priority: u32,
    pub symbol: u32,
}

/// A COMDAT group: its name and its members, each a kind (0 a data segment, 1 a function) and an
/// index.
#[derive(Debug, Clone)]
pub struct Comdat<'a> {
    pub name: &'a str,
    pub members: Vec<(u8, u32)>,
}

/// A parsed object. Every index space puts the imports first, as the binary format does.
#[derive(Debug, Clone, Default)]
pub struct Object<'a> {
    /// The name in messages: the path, or the archive and the member.
    pub name: String,
    /// Each function type as it is encoded, from the `0x60` on.
    pub types: Vec<&'a [u8]>,
    pub func_imports: Vec<Import<'a, u32>>,
    pub global_imports: Vec<Import<'a, &'a [u8]>>,
    pub tag_imports: Vec<Import<'a, u32>>,
    pub table_imports: Vec<Import<'a, &'a [u8]>>,
    /// The type of each defined function.
    pub funcs: Vec<u32>,
    pub globals: Vec<Global<'a>>,
    /// The type of each defined tag.
    pub tags: Vec<u32>,
    /// The export section: the name, the kind byte and the index.
    pub exports: Vec<(&'a str, u8, u32)>,
    pub code: Vec<Body<'a>>,
    pub data: Vec<Segment<'a>>,
    pub code_relocs: Vec<Reloc>,
    pub data_relocs: Vec<Reloc>,
    pub symbols: Vec<Symbol<'a>>,
    pub inits: Vec<Init>,
    pub comdats: Vec<Comdat<'a>>,
    /// The `target_features` section: the prefix byte and the feature name.
    pub features: Vec<(u8, &'a str)>,
    /// The `producers` section: the field, then the name and the version of each value.
    pub producers: Vec<(&'a str, Vec<(&'a str, &'a str)>)>,
}

/// Whether a relocation type has an addend.
#[must_use]
pub fn has_addend(kind: u8) -> bool {
    matches!(kind, 3 | 4 | 5 | 8 | 9 | 11 | 14 | 15 | 16 | 17 | 21 | 22 | 23 | 25)
}

/// The section ids of the core specification that an object has.
mod id {
    pub(super) const CUSTOM: u8 = 0;
    pub(super) const TYPE: u8 = 1;
    pub(super) const IMPORT: u8 = 2;
    pub(super) const FUNCTION: u8 = 3;
    pub(super) const TABLE: u8 = 4;
    pub(super) const MEMORY: u8 = 5;
    pub(super) const GLOBAL: u8 = 6;
    pub(super) const EXPORT: u8 = 7;
    pub(super) const START: u8 = 8;
    pub(super) const ELEM: u8 = 9;
    pub(super) const CODE: u8 = 10;
    pub(super) const DATA: u8 = 11;
    pub(super) const DATACOUNT: u8 = 12;
    pub(super) const TAG: u8 = 13;
}

/// Whether `bytes` start as a wasm module of version 1.
#[must_use]
pub fn is_wasm(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\0asm\x01\0\0\0")
}

impl<'a> Object<'a> {
    /// Reads the object in `bytes`. `name` is what messages call it.
    ///
    /// # Errors
    ///
    /// When the bytes are not a relocatable wasm object, or when the object has a part that is
    /// outside what this linker covers.
    pub fn parse(name: &str, bytes: &'a [u8]) -> Result<Self, Error> {
        let mut object = Object { name: name.to_owned(), ..Object::default() };
        object.read(bytes).map_err(|error| error.within(name))?;
        Ok(object)
    }

    fn read(&mut self, bytes: &'a [u8]) -> Result<(), Error> {
        if !is_wasm(bytes) {
            return Err(Error::new("not a wasm object of version 1".to_owned()));
        }
        let mut file = Cursor::new(&bytes[8..]);
        let mut linking = false;
        // The index of each section in file order, which is how a `reloc.*` section names the
        // section it patches.
        let mut index = 0u32;
        let mut code_index = None;
        let mut data_index = None;
        while !file.is_empty() {
            let id = file.byte()?;
            let len = file.u32()? as usize;
            let payload = file.take(len)?;
            let mut s = Cursor::new(payload);
            match id {
                id::CUSTOM => {
                    let name = s.name()?;
                    if name == "linking" {
                        self.linking(&mut s)?;
                        linking = true;
                    } else if let Some(target) = name.strip_prefix("reloc.") {
                        let section = s.u32()?;
                        let relocs = Self::relocs(&mut s)?;
                        if Some(section) == code_index {
                            self.code_relocs = relocs;
                        } else if Some(section) == data_index {
                            self.data_relocs = relocs;
                        } else if !target.starts_with(".debug") {
                            return Err(unsupported(&format!("the section {name}")));
                        }
                    } else if name == "target_features" {
                        for _ in 0..s.count()? {
                            let prefix = s.byte()?;
                            self.features.push((prefix, s.name()?));
                        }
                    } else if name == "producers" {
                        for _ in 0..s.count()? {
                            let field = s.name()?;
                            let mut values = Vec::new();
                            for _ in 0..s.count()? {
                                values.push((s.name()?, s.name()?));
                            }
                            self.producers.push((field, values));
                        }
                    } else if name.starts_with("dylink") {
                        return Err(unsupported("a shared library (dylink.0)"));
                    }
                }
                id::TYPE => {
                    for _ in 0..s.count()? {
                        let start = s.pos();
                        if s.byte()? != 0x60 {
                            return Err(unsupported("a type that is not a function type"));
                        }
                        for _ in 0..2 {
                            for _ in 0..s.count()? {
                                valtype(&mut s)?;
                            }
                        }
                        self.types.push(&payload[start..s.pos()]);
                    }
                }
                id::IMPORT => self.imports(&mut s, payload)?,
                id::FUNCTION => {
                    for _ in 0..s.count()? {
                        self.funcs.push(s.u32()?);
                    }
                }
                id::GLOBAL => {
                    for _ in 0..s.count()? {
                        let start = s.pos();
                        valtype(&mut s)?;
                        s.byte()?;
                        let ty = &payload[start..s.pos()];
                        let init = s.expr()?;
                        self.globals.push(Global { ty, init });
                    }
                }
                id::EXPORT => {
                    for _ in 0..s.count()? {
                        self.exports.push((s.name()?, s.byte()?, s.u32()?));
                    }
                }
                id::TAG => {
                    for _ in 0..s.count()? {
                        if s.byte()? != 0 {
                            return Err(unsupported("a tag that is not an exception"));
                        }
                        self.tags.push(s.u32()?);
                    }
                }
                id::CODE => {
                    code_index = Some(index);
                    for _ in 0..s.count()? {
                        let len = s.u32()? as usize;
                        let offset = s.pos() as u32;
                        self.code.push(Body { offset, bytes: s.take(len)? });
                    }
                }
                id::DATA => {
                    data_index = Some(index);
                    for _ in 0..s.count()? {
                        if s.u32()? != 0 {
                            return Err(unsupported("a passive data segment"));
                        }
                        s.expr()?;
                        let len = s.u32()? as usize;
                        let offset = s.pos() as u32;
                        let bytes = s.take(len)?;
                        self.data.push(Segment { offset, bytes, name: "", align: 0, flags: 0 });
                    }
                }
                // LLVM writes an element section with the functions whose address the object
                // takes. The table index relocations say the same, and they are what the
                // linker reads, as LLD does.
                id::DATACOUNT | id::ELEM => s = Cursor::new(&[]),
                id::TABLE => return Err(unsupported("a defined table")),
                id::MEMORY => return Err(unsupported("a defined memory")),
                id::START => return Err(unsupported("a start function")),
                _ => return Err(Error::new(format!("an unknown section with id {id}"))),
            }
            if id != id::CUSTOM && !s.is_empty() {
                return Err(Error::new(format!("section {id} is longer than its entries")));
            }
            index += 1;
        }
        if !linking {
            return Err(Error::new(
                "the object has no linking section, so it is not relocatable".to_owned(),
            ));
        }
        if self.code.len() != self.funcs.len() {
            return Err(Error::new(format!(
                "{} functions and {} bodies",
                self.funcs.len(),
                self.code.len()
            )));
        }
        self.check()
    }

    fn imports(&mut self, s: &mut Cursor<'a>, payload: &'a [u8]) -> Result<(), Error> {
        for _ in 0..s.count()? {
            let module = s.name()?;
            let field = s.name()?;
            match s.byte()? {
                0 => self.func_imports.push(Import { module, field, ty: s.u32()? }),
                1 => {
                    let start = s.pos();
                    valtype(s)?;
                    s.limits()?;
                    let ty = &payload[start..s.pos()];
                    self.table_imports.push(Import { module, field, ty });
                }
                2 => {
                    s.limits()?;
                    if (module, field) != ("env", "__linear_memory") {
                        return Err(unsupported(&format!("the memory import {module}.{field}")));
                    }
                }
                3 => {
                    let start = s.pos();
                    valtype(s)?;
                    s.byte()?;
                    let ty = &payload[start..s.pos()];
                    self.global_imports.push(Import { module, field, ty });
                }
                4 => {
                    if s.byte()? != 0 {
                        return Err(unsupported("a tag that is not an exception"));
                    }
                    self.tag_imports.push(Import { module, field, ty: s.u32()? });
                }
                kind => return Err(Error::new(format!("an import of unknown kind {kind}"))),
            }
        }
        Ok(())
    }

    fn linking(&mut self, s: &mut Cursor<'a>) -> Result<(), Error> {
        let version = s.u32()?;
        if version != LINKING_VERSION {
            return Err(Error::new(format!(
                "the linking section has version {version}, and this linker reads version 2"
            )));
        }
        while !s.is_empty() {
            let id = s.byte()?;
            let len = s.u32()? as usize;
            let mut sub = Cursor::new(s.take(len)?);
            match id {
                5 => {
                    for i in 0..sub.count()? {
                        let name = sub.name()?;
                        let align = sub.u32()?;
                        let flags = sub.u32()?;
                        let segment = self.data.get_mut(i).ok_or_else(|| {
                            Error::new("the segment info has more entries than the data".to_owned())
                        })?;
                        segment.name = name;
                        segment.align = align;
                        segment.flags = flags;
                    }
                }
                6 => {
                    for _ in 0..sub.count()? {
                        self.inits.push(Init { priority: sub.u32()?, symbol: sub.u32()? });
                    }
                }
                7 => {
                    for _ in 0..sub.count()? {
                        let name = sub.name()?;
                        if sub.u32()? != 0 {
                            return Err(unsupported("a COMDAT group with flags"));
                        }
                        let mut members = Vec::new();
                        for _ in 0..sub.count()? {
                            members.push((sub.byte()?, sub.u32()?));
                        }
                        self.comdats.push(Comdat { name, members });
                    }
                }
                8 => {
                    for _ in 0..sub.count()? {
                        let symbol = self.symbol(&mut sub)?;
                        self.symbols.push(symbol);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn symbol(&self, s: &mut Cursor<'a>) -> Result<Symbol<'a>, Error> {
        let kind = match s.byte()? {
            0 => Kind::Function,
            1 => Kind::Data,
            2 => Kind::Global,
            3 => Kind::Section,
            4 => Kind::Tag,
            5 => Kind::Table,
            kind => return Err(Error::new(format!("a symbol of unknown kind {kind}"))),
        };
        let flags = s.u32()?;
        let undefined = flags & UNDEFINED != 0;
        let mut symbol = Symbol { kind, flags, name: "", index: 0, offset: 0, size: 0 };
        match kind {
            Kind::Data => {
                symbol.name = s.name()?;
                if !undefined {
                    symbol.index = s.u32()?;
                    symbol.offset = s.u32()?;
                    symbol.size = s.u32()?;
                }
            }
            Kind::Section => symbol.index = s.u32()?,
            _ => {
                symbol.index = s.u32()?;
                if !undefined || flags & EXPLICIT_NAME != 0 {
                    symbol.name = s.name()?;
                } else {
                    symbol.name = self.import_field(kind, symbol.index).ok_or_else(|| {
                        Error::new(format!("an undefined symbol names import {}", symbol.index))
                    })?;
                }
            }
        }
        Ok(symbol)
    }

    /// The field name of import `index` of a kind, when there is one.
    fn import_field(&self, kind: Kind, index: u32) -> Option<&'a str> {
        let index = index as usize;
        match kind {
            Kind::Function => self.func_imports.get(index).map(|i| i.field),
            Kind::Global => self.global_imports.get(index).map(|i| i.field),
            Kind::Tag => self.tag_imports.get(index).map(|i| i.field),
            Kind::Table => self.table_imports.get(index).map(|i| i.field),
            Kind::Data | Kind::Section => None,
        }
    }

    fn relocs(s: &mut Cursor<'a>) -> Result<Vec<Reloc>, Error> {
        let mut relocs = Vec::new();
        for _ in 0..s.count()? {
            let kind = s.byte()?;
            let offset = s.u32()?;
            let index = s.u32()?;
            let addend = if has_addend(kind) { s.i64()? } else { 0 };
            relocs.push(Reloc { kind, offset, index, addend });
        }
        Ok(relocs)
    }

    /// The checks that need the whole file: every index in range, so that the linker can index
    /// without a check of its own.
    fn check(&self) -> Result<(), Error> {
        let range = |what: &str, index: u32, len: usize| {
            if (index as usize) < len {
                Ok(())
            } else {
                Err(Error::new(format!("{what} {index} is out of range")))
            }
        };
        for symbol in &self.symbols {
            let defined = !symbol.is_undefined();
            match symbol.kind {
                Kind::Function if defined => {
                    let first = self.func_imports.len() as u32;
                    if symbol.index < first {
                        return Err(Error::new(format!(
                            "the defined function {} is an import",
                            symbol.name
                        )));
                    }
                    range("function", symbol.index - first, self.funcs.len())?;
                }
                Kind::Function => range("function import", symbol.index, self.func_imports.len())?,
                Kind::Global if defined => {
                    let first = self.global_imports.len() as u32;
                    if symbol.index < first {
                        return Err(Error::new(format!(
                            "the defined global {} is an import",
                            symbol.name
                        )));
                    }
                    range("global", symbol.index - first, self.globals.len())?;
                }
                Kind::Global => range("global import", symbol.index, self.global_imports.len())?,
                Kind::Tag if defined => {
                    let first = self.tag_imports.len() as u32;
                    if symbol.index < first {
                        return Err(Error::new(format!(
                            "the defined tag {} is an import",
                            symbol.name
                        )));
                    }
                    range("tag", symbol.index - first, self.tags.len())?;
                }
                Kind::Tag => range("tag import", symbol.index, self.tag_imports.len())?,
                Kind::Table if defined => return Err(unsupported("a defined table")),
                Kind::Table => range("table import", symbol.index, self.table_imports.len())?,
                Kind::Data if defined => {
                    range("segment", symbol.index, self.data.len())?;
                    let segment = &self.data[symbol.index as usize];
                    let end = u64::from(symbol.offset) + u64::from(symbol.size);
                    if end > segment.bytes.len() as u64 {
                        return Err(Error::new(format!(
                            "the data symbol {} ends past its segment",
                            symbol.name
                        )));
                    }
                }
                Kind::Data | Kind::Section => {}
            }
        }
        for import in &self.func_imports {
            range("type", import.ty, self.types.len())?;
        }
        for &ty in self.funcs.iter().chain(&self.tags) {
            range("type", ty, self.types.len())?;
        }
        for import in &self.tag_imports {
            range("type", import.ty, self.types.len())?;
        }
        for init in &self.inits {
            range("constructor symbol", init.symbol, self.symbols.len())?;
        }
        for reloc in self.code_relocs.iter().chain(&self.data_relocs) {
            if reloc.kind == 6 {
                range("type", reloc.index, self.types.len())?;
            } else {
                range("relocation symbol", reloc.index, self.symbols.len())?;
            }
        }
        Ok(())
    }
}

/// Reads past a value type. The types a C object uses are the four numbers, `v128`, and the two
/// reference types.
fn valtype(s: &mut Cursor<'_>) -> Result<(), Error> {
    match s.byte()? {
        0x7f | 0x7e | 0x7d | 0x7c | 0x7b | 0x70 | 0x6f | 0x69 => Ok(()),
        ty => Err(Error::new(format!("the value type {ty:#04x} is not supported"))),
    }
}

fn unsupported(what: &str) -> Error {
    Error::new(format!("{what} is outside what the rucc linker covers; link with wasm-ld"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::caller;

    #[test]
    fn an_object_reads_back_as_it_was_written() {
        let bytes = caller("g");
        let object = Object::parse("a.o", &bytes).unwrap();
        assert_eq!(object.types.len(), 2);
        assert_eq!(object.func_imports.len(), 1);
        assert_eq!((object.func_imports[0].field, object.func_imports[0].ty), ("f", 1));
        assert_eq!(object.funcs, [0]);
        assert_eq!(object.code[0].offset, 2);
        assert_eq!(object.code[0].bytes.len(), 15);
        assert_eq!(object.data[0].bytes, b"abcd");
        assert_eq!((object.data[0].name, object.data[0].align), (".data.d", 2));
        let names: Vec<_> = object.symbols.iter().map(|s| (s.kind, s.name, s.index)).collect();
        assert_eq!(
            names,
            [(Kind::Function, "g", 1), (Kind::Function, "f", 0), (Kind::Data, "d", 0)]
        );
        assert!(object.symbols[1].is_undefined());
        let relocs = [
            Reloc { kind: 4, offset: 4, index: 2, addend: 0 },
            Reloc { kind: 0, offset: 10, index: 1, addend: 0 },
        ];
        assert_eq!(object.code_relocs, relocs);
        assert_eq!(object.features, [(b'+', "sign-ext")]);
    }

    #[test]
    fn a_bad_object_is_an_error_with_its_name() {
        let mut bytes = caller("g");
        bytes.truncate(bytes.len() - 3);
        let error = Object::parse("a.o", &bytes).unwrap_err();
        assert!(error.message().starts_with("a.o: "), "{error}");
        let error = Object::parse("b.o", b"\0asm\x01\0\0\0").unwrap_err();
        assert!(error.message().contains("no linking section"), "{error}");
    }
}
