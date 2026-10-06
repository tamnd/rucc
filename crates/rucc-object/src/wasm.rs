//! The relocatable wasm object, which the `object` crate reads and does not write.
//!
//! Design: section 9.3 of the WebAssembly notes, and the object format of the tool conventions
//! (`Linking.md`) as LLD 23 reads it. The relocation numbers are the ones in LLVM's
//! `WasmRelocs.def`, because the conventions text does not list all of them.
//!
//! # What the caller gives and what this decides
//!
//! The caller gives the function bodies as bytes, the data as segments, and one list of symbols.
//! A field in a body or a segment that names something the linker places is a [`Fixup`]: the
//! caller reserves its bytes and says what it refers to, and this module writes the field. So the
//! caller never needs to know an index or an address. That is the point of the split, because
//! the numbers depend on the whole file. A function index counts the imported functions first,
//! and a data address counts every segment before the one it is in.
//!
//! A function symbol is defined when a [`Function`] names it, and imported when none does. A data
//! symbol is defined when it has a place. Globals, tables and tags are always imported, because
//! the only ones a C object refers to are the stack pointer, the function table and the tag of
//! `longjmp`, and the linker makes all three. This module sets the `UNDEFINED` and
//! `EXPLICIT_NAME` flags itself from these facts, so a symbol cannot say that it is defined and
//! have no definition.
//!
//! # The fields the linker patches
//!
//! Every relocated field in code is a LEB128 at its widest, five bytes, and every one in data is
//! four bytes. The linker then patches in place and does not move code. The value written here
//! is the one the field would have if this object were the whole program, which is what clang
//! writes too, so `llvm-objdump` shows the same thing for both compilers.

use std::fmt;

/// `\0asm`, then version 1.
const MAGIC: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

/// The version of the `linking` section that LLD reads.
const LINKING_VERSION: u32 = 2;

/// The width of a padded LEB128 field for a 32-bit value.
pub const PADDED: usize = 5;

/// The size of a wasm page.
const PAGE: u64 = 65_536;

/// A symbol is weak.
pub const WEAK: u32 = 0x1;
/// A symbol is local to the object: `static`, or a name the compiler made.
pub const LOCAL: u32 = 0x2;
/// A symbol is hidden. Every non-static symbol is, unless `-fvisibility=default` says otherwise.
pub const HIDDEN: u32 = 0x4;
/// A symbol is not defined in this object. This module sets it.
const UNDEFINED: u32 = 0x10;
/// A symbol is exported from the final module.
pub const EXPORTED: u32 = 0x20;
/// An import has a field name that is not the symbol name. This module sets it.
const EXPLICIT_NAME: u32 = 0x40;
/// The linker keeps a symbol that nothing refers to.
pub const NO_STRIP: u32 = 0x80;
/// A data symbol is thread-local: its address is an offset from the TLS base of the thread.
pub const TLS: u32 = 0x100;

/// A segment holds strings that the linker can merge.
pub const STRINGS: u32 = 0x1;
/// A segment is a part of the image of the thread-local variables, which the linker copies for
/// each thread.
pub const TLS_SEGMENT: u32 = 0x2;
/// The linker keeps a segment that nothing refers to.
pub const RETAIN: u32 = 0x4;

/// A value type of the core specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
}

impl ValType {
    /// The byte the binary format gives the type.
    #[must_use]
    pub fn byte(self) -> u8 {
        match self {
            ValType::I32 => 0x7f,
            ValType::I64 => 0x7e,
            ValType::F32 => 0x7d,
            ValType::F64 => 0x7c,
        }
    }
}

impl fmt::Display for ValType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ValType::I32 => "i32",
            ValType::I64 => "i64",
            ValType::F32 => "f32",
            ValType::F64 => "f64",
        })
    }
}

/// The type of a function.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FuncType {
    pub params: Vec<ValType>,
    pub results: Vec<ValType>,
}

/// What a symbol is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    /// A function of the type at this index. It is imported when no [`Function`] defines it.
    Function { ty: u32, import: Option<Import> },
    /// A data object. It is defined when it has a place.
    Data { place: Option<Place> },
    /// A global, always imported.
    Global { ty: ValType, mutable: bool, import: Option<Import> },
    /// A table of function references, always imported.
    Table { import: Option<Import> },
    /// An exception tag of the function type at this index, always imported.
    Tag { ty: u32, import: Option<Import> },
}

/// The module and the field of an import when they are not `env` and the symbol name, which is
/// what `import_module` and `import_name` ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub module: String,
    pub field: String,
}

/// Where a data symbol is: its segment, its offset in that segment and its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub segment: u32,
    pub offset: u32,
    pub size: u32,
}

/// One entry of the symbol table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Any of [`WEAK`], [`LOCAL`], [`HIDDEN`], [`EXPORTED`], [`NO_STRIP`] and [`TLS`].
    pub flags: u32,
}

/// What a relocated field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelocKind {
    /// The index of a function, in `call`.
    FunctionIndexLeb,
    /// The table slot of a function, in `i32.const`.
    TableIndexSleb,
    /// The table slot of a function, in data.
    TableIndexI32,
    /// A data address, in the offset of a load or a store.
    MemoryAddrLeb,
    /// A data address, in `i32.const`.
    MemoryAddrSleb,
    /// A data address, in data.
    MemoryAddrI32,
    /// The offset of a thread-local variable from the TLS base, in `i32.const`.
    MemoryAddrTlsSleb,
    /// The index of a type, in `call_indirect`.
    TypeIndexLeb,
    /// The index of a global, in `global.get` and `global.set`.
    GlobalIndexLeb,
    /// The index of a tag, in `throw` and `try_table`.
    TagIndexLeb,
    /// The number of a table, in `call_indirect`.
    TableNumberLeb,
}

impl RelocKind {
    /// The number LLVM gives the relocation.
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            RelocKind::FunctionIndexLeb => 0,
            RelocKind::TableIndexSleb => 1,
            RelocKind::TableIndexI32 => 2,
            RelocKind::MemoryAddrLeb => 3,
            RelocKind::MemoryAddrSleb => 4,
            RelocKind::MemoryAddrI32 => 5,
            RelocKind::TypeIndexLeb => 6,
            RelocKind::GlobalIndexLeb => 7,
            RelocKind::TagIndexLeb => 10,
            RelocKind::TableNumberLeb => 20,
            RelocKind::MemoryAddrTlsSleb => 21,
        }
    }

    /// The number of bytes the field takes.
    #[must_use]
    pub fn width(self) -> usize {
        match self {
            RelocKind::TableIndexI32 | RelocKind::MemoryAddrI32 => 4,
            _ => PADDED,
        }
    }

    /// Whether the entry carries an addend. Only the address relocations do.
    fn has_addend(self) -> bool {
        matches!(
            self,
            RelocKind::MemoryAddrLeb
                | RelocKind::MemoryAddrSleb
                | RelocKind::MemoryAddrI32
                | RelocKind::MemoryAddrTlsSleb
        )
    }

    /// Whether the entry names a type rather than a symbol.
    fn names_type(self) -> bool {
        self == RelocKind::TypeIndexLeb
    }
}

/// A field that names something the linker places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixup {
    /// Where the field starts, from the start of the code of its function or of the bytes of its
    /// segment. The caller reserves [`RelocKind::width`] bytes there.
    pub at: u32,
    pub kind: RelocKind,
    /// A symbol index, or a type index for [`RelocKind::TypeIndexLeb`].
    pub target: u32,
    /// Added to a data address. Zero for the other kinds.
    pub addend: i32,
}

/// A function that this object defines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Function {
    /// The index of its symbol, which is a function symbol.
    pub symbol: u32,
    /// The locals after the parameters, as runs of one type.
    pub locals: Vec<(u32, ValType)>,
    /// The instructions, with the `end` that closes the body.
    pub code: Vec<u8>,
    pub fixups: Vec<Fixup>,
    /// A name for the export section of this object, from `export_name`.
    pub export: Option<String>,
}

/// One data segment. Each data object has its own, which is how the linker can drop one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Segment {
    /// `.data.<symbol>`, `.rodata.<symbol>`, `.bss.<symbol>`, `.tdata.<symbol>` or
    /// `.tbss.<symbol>`, as clang names them.
    pub name: String,
    /// The alignment, as its log2.
    pub align: u32,
    /// Any of [`STRINGS`], [`TLS_SEGMENT`] and [`RETAIN`].
    pub flags: u32,
    pub bytes: Vec<u8>,
    pub fixups: Vec<Fixup>,
}

/// The `producers` section: the language and the compiler.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Producers {
    /// `C11`, `C17` or `C23`.
    pub language: Option<String>,
    /// The name and the version of each tool that made the object.
    pub processed_by: Vec<(String, String)>,
}

/// Everything in one object.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    pub types: Vec<FuncType>,
    pub symbols: Vec<Symbol>,
    pub functions: Vec<Function>,
    /// The function symbols that are second names of a function in this object, each as the
    /// symbol of the second name and the symbol of the function. This is the `alias` attribute of
    /// GCC on a function. The two symbols have the same function index, and each one keeps its
    /// own name and flags. A data symbol needs no entry here, because a second name of a variable
    /// is a data symbol with the same place.
    pub aliases: Vec<(u32, u32)>,
    pub segments: Vec<Segment>,
    /// The constructors, as a priority and a symbol index. 65535 is the priority of one that gave
    /// none.
    pub inits: Vec<(u32, u32)>,
    /// The LLVM names of the features the code uses.
    pub features: Vec<String>,
    /// The LLVM names of the features the object must not be linked with.
    pub disallowed: Vec<String>,
    pub producers: Producers,
}

impl Module {
    /// The index of `ty` in the type list, added when it is not there.
    ///
    /// # Panics
    ///
    /// When the list has 2^32 types, which no object has.
    pub fn intern(&mut self, ty: FuncType) -> u32 {
        let index = match self.types.iter().position(|t| *t == ty) {
            Some(index) => index,
            None => {
                self.types.push(ty);
                self.types.len() - 1
            }
        };
        u32::try_from(index).expect("fewer than 2^32 types")
    }

    /// The index of a new symbol.
    ///
    /// # Panics
    ///
    /// When the table has 2^32 symbols, which no object has.
    pub fn symbol(&mut self, name: impl Into<String>, kind: SymbolKind, flags: u32) -> u32 {
        self.symbols.push(Symbol { name: name.into(), kind, flags });
        u32::try_from(self.symbols.len() - 1).expect("fewer than 2^32 symbols")
    }
}

/// Why an object could not be written. Each one is a mistake in the caller, not in the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A fixup names a symbol or a type that is not there, or a symbol of the wrong kind.
    Target { fixup: Fixup, why: &'static str },
    /// A fixup is outside the bytes it patches.
    Outside { fixup: Fixup },
    /// A function names a symbol that is not a function symbol, or two functions name one.
    Definition { symbol: u32 },
    /// A data symbol is outside its segment.
    Place { symbol: u32 },
    /// An alias is not a function symbol, is defined twice, or names a function that this object
    /// does not define.
    Alias { symbol: u32 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Target { fixup, why } => write!(
                f,
                "the fixup at {} names {} {}, which {why}",
                fixup.at,
                if fixup.kind.names_type() { "type" } else { "symbol" },
                fixup.target
            ),
            Error::Outside { fixup } => {
                write!(f, "the fixup at {} is outside the bytes it patches", fixup.at)
            }
            Error::Definition { symbol } => {
                write!(f, "symbol {symbol} is not a function that one definition names")
            }
            Error::Place { symbol } => write!(f, "symbol {symbol} is outside its segment"),
            Error::Alias { symbol } => {
                write!(f, "symbol {symbol} is not a second name of a function that is defined here")
            }
        }
    }
}

impl std::error::Error for Error {}

/// What [`write()`] gives: the bytes, and the names a linker can find in them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub bytes: Vec<u8>,
    /// Every symbol that is defined and not local, in symbol table order. This is what the index
    /// of an archive lists for the member.
    pub defines: Vec<String>,
}

/// Push `value` as an unsigned LEB128 at its shortest.
pub fn uleb(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Push `value` as a signed LEB128 at its shortest.
pub fn sleb(out: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// `value` as an unsigned LEB128 of five bytes.
#[must_use]
pub fn uleb_padded(value: u32) -> [u8; PADDED] {
    let mut out = [0; PADDED];
    let mut rest = value;
    for (n, byte) in out.iter_mut().enumerate() {
        *byte = (rest & 0x7f) as u8;
        rest >>= 7;
        if n + 1 < PADDED {
            *byte |= 0x80;
        }
    }
    out
}

/// `value` as a signed LEB128 of five bytes.
#[must_use]
pub fn sleb_padded(value: i32) -> [u8; PADDED] {
    let mut out = [0; PADDED];
    let mut rest = value;
    for (n, byte) in out.iter_mut().enumerate() {
        *byte = (rest & 0x7f) as u8;
        rest >>= 7;
        if n + 1 < PADDED {
            *byte |= 0x80;
        }
    }
    out
}

/// Push a name: its length and its bytes.
pub fn name(out: &mut Vec<u8>, text: &str) {
    uleb(out, text.len() as u64);
    out.extend_from_slice(text.as_bytes());
}

/// The numbers that the fixups need, worked out once from the whole module.
struct Layout {
    /// The function index of each symbol that is a function.
    function: Vec<Option<u32>>,
    /// The global, table and tag index of each symbol of that kind.
    other: Vec<Option<u32>>,
    /// The address each segment starts at, if this object were the whole program.
    segment: Vec<u32>,
    /// The table slot of each function that a table index fixup names. Slot 0 stays empty, as in
    /// the linker's output, so that a null function pointer traps.
    slot: Vec<Option<u32>>,
    /// Which function symbols a definition names.
    defined: Vec<bool>,
}

impl Layout {
    fn of(module: &Module) -> Result<Layout, Error> {
        let count = module.symbols.len();
        let mut defined = vec![false; count];
        for function in &module.functions {
            let index = function.symbol as usize;
            let is_function = matches!(
                module.symbols.get(index).map(|s| &s.kind),
                Some(SymbolKind::Function { .. })
            );
            if !is_function || defined[index] {
                return Err(Error::Definition { symbol: function.symbol });
            }
            defined[index] = true;
        }
        // An alias is defined when the function that it names is defined, by a definition or by
        // an alias that comes before it in the list.
        for &(alias, target) in &module.aliases {
            let is_function = matches!(
                module.symbols.get(alias as usize).map(|s| &s.kind),
                Some(SymbolKind::Function { .. })
            );
            let named = defined.get(target as usize) == Some(&true);
            if !is_function || defined[alias as usize] || !named {
                return Err(Error::Alias { symbol: alias });
            }
            defined[alias as usize] = true;
        }

        // The imported functions come first in the index space, in symbol order, then the
        // definitions in the order they are written.
        let mut function = vec![None; count];
        let mut next = 0u32;
        for (index, symbol) in module.symbols.iter().enumerate() {
            if matches!(symbol.kind, SymbolKind::Function { .. }) && !defined[index] {
                function[index] = Some(next);
                next += 1;
            }
        }
        for f in &module.functions {
            function[f.symbol as usize] = Some(next);
            next += 1;
        }
        for &(alias, target) in &module.aliases {
            function[alias as usize] = function[target as usize];
        }

        // Globals, tables and tags each have their own index space, in symbol order.
        let mut other = vec![None; count];
        let (mut globals, mut tables, mut tags) = (0u32, 0u32, 0u32);
        for (index, symbol) in module.symbols.iter().enumerate() {
            let counter = match symbol.kind {
                SymbolKind::Global { .. } => &mut globals,
                SymbolKind::Table { .. } => &mut tables,
                SymbolKind::Tag { .. } => &mut tags,
                _ => continue,
            };
            other[index] = Some(*counter);
            *counter += 1;
        }

        let mut segment = Vec::with_capacity(module.segments.len());
        let mut address = 0u64;
        for s in &module.segments {
            let align = 1u64 << s.align.min(31);
            address = address.div_ceil(align) * align;
            segment.push(u32::try_from(address).unwrap_or(u32::MAX));
            address += s.bytes.len() as u64;
        }
        for (index, symbol) in module.symbols.iter().enumerate() {
            if let SymbolKind::Data { place: Some(place) } = symbol.kind {
                let fits = module.segments.get(place.segment as usize).is_some_and(|s| {
                    u64::from(place.offset) + u64::from(place.size) <= s.bytes.len() as u64
                });
                if !fits {
                    return Err(Error::Place { symbol: u32::try_from(index).unwrap_or(u32::MAX) });
                }
            }
        }

        let mut slot = vec![None; count];
        let mut slots = 1u32;
        let fixups = module
            .functions
            .iter()
            .flat_map(|f| &f.fixups)
            .chain(module.segments.iter().flat_map(|s| &s.fixups));
        for fixup in fixups {
            if !matches!(fixup.kind, RelocKind::TableIndexSleb | RelocKind::TableIndexI32) {
                continue;
            }
            if let Some(entry @ None) = slot.get_mut(fixup.target as usize) {
                *entry = Some(slots);
                slots += 1;
            }
        }

        Ok(Layout { function, other, segment, slot, defined })
    }

    /// Whether the symbol at `index` is defined in this object.
    fn is_defined(&self, module: &Module, index: usize) -> bool {
        match module.symbols[index].kind {
            SymbolKind::Function { .. } => self.defined[index],
            SymbolKind::Data { place } => place.is_some(),
            SymbolKind::Global { .. } | SymbolKind::Table { .. } | SymbolKind::Tag { .. } => false,
        }
    }

    /// The value a fixup's field gets, or why it cannot have one.
    fn value(&self, module: &Module, fixup: Fixup) -> Result<i64, Error> {
        let fail = |why| Err(Error::Target { fixup, why });
        if fixup.kind.names_type() {
            return if (fixup.target as usize) < module.types.len() {
                Ok(i64::from(fixup.target))
            } else {
                fail("is not in the type list")
            };
        }
        let index = fixup.target as usize;
        let Some(symbol) = module.symbols.get(index) else {
            return fail("is not in the symbol table");
        };
        match (fixup.kind, &symbol.kind) {
            (RelocKind::FunctionIndexLeb, SymbolKind::Function { .. }) => {
                Ok(i64::from(self.function[index].unwrap_or(0)))
            }
            (RelocKind::TableIndexSleb | RelocKind::TableIndexI32, SymbolKind::Function { .. }) => {
                Ok(i64::from(self.slot[index].unwrap_or(0)))
            }
            (
                RelocKind::MemoryAddrLeb
                | RelocKind::MemoryAddrSleb
                | RelocKind::MemoryAddrI32
                | RelocKind::MemoryAddrTlsSleb,
                SymbolKind::Data { place },
            ) => {
                let base = place.map_or(0, |p| {
                    i64::from(self.segment[p.segment as usize]) + i64::from(p.offset)
                });
                Ok(base + i64::from(fixup.addend))
            }
            (RelocKind::GlobalIndexLeb, SymbolKind::Global { .. })
            | (RelocKind::TableNumberLeb, SymbolKind::Table { .. })
            | (RelocKind::TagIndexLeb, SymbolKind::Tag { .. }) => {
                Ok(i64::from(self.other[index].unwrap_or(0)))
            }
            _ => fail("is not of the kind the relocation wants"),
        }
    }
}

/// Write the field of `fixup` into `bytes`, which start at the start of the function code or the
/// segment the fixup is in.
fn patch(bytes: &mut [u8], fixup: Fixup, value: i64) -> Result<(), Error> {
    let at = fixup.at as usize;
    let Some(field) = bytes.get_mut(at..at + fixup.kind.width()) else {
        return Err(Error::Outside { fixup });
    };
    // The values are indices and 32-bit addresses, so they fit. An address past 4 GiB in an
    // object for a 32-bit memory is a bug in the layout above.
    let value32 = value as i32;
    match fixup.kind {
        RelocKind::TableIndexI32 | RelocKind::MemoryAddrI32 => {
            field.copy_from_slice(&value32.to_le_bytes());
        }
        RelocKind::TableIndexSleb | RelocKind::MemoryAddrSleb | RelocKind::MemoryAddrTlsSleb => {
            field.copy_from_slice(&sleb_padded(value32));
        }
        _ => field.copy_from_slice(&uleb_padded(value as u32)),
    }
    Ok(())
}

/// One relocation entry, with its offset already from the start of the section payload.
struct Entry {
    offset: u32,
    fixup: Fixup,
}

/// Push one section: its id, its size and its payload.
fn section(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
    out.push(id);
    uleb(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

/// Push one custom section with this name.
fn custom(out: &mut Vec<u8>, label: &str, payload: &[u8]) {
    let mut body = Vec::with_capacity(label.len() + 1 + payload.len());
    name(&mut body, label);
    body.extend_from_slice(payload);
    section(out, 0, &body);
}

/// The module and the field a symbol is imported from.
fn import_of(symbol: &Symbol) -> (String, String) {
    let import = match &symbol.kind {
        SymbolKind::Function { import, .. }
        | SymbolKind::Global { import, .. }
        | SymbolKind::Table { import }
        | SymbolKind::Tag { import, .. } => import.as_ref(),
        SymbolKind::Data { .. } => None,
    };
    match import {
        Some(i) => (i.module.clone(), i.field.clone()),
        None => ("env".to_owned(), symbol.name.clone()),
    }
}

/// A length or a count, as the LEB128 functions take it.
fn len32(n: usize) -> u64 {
    n as u64
}

/// The object for `module`.
///
/// # Errors
///
/// When a fixup or a symbol refers to something that is not in the module. See [`Error`].
pub fn write(module: &Module) -> Result<Written, Error> {
    let layout = Layout::of(module)?;
    let mut out = Vec::from(MAGIC);
    // The index of each section in the file, which a reloc section names.
    let mut sections = 0u32;

    if !module.types.is_empty() {
        let mut payload = Vec::new();
        uleb(&mut payload, len32(module.types.len()));
        for ty in &module.types {
            payload.push(0x60);
            uleb(&mut payload, len32(ty.params.len()));
            payload.extend(ty.params.iter().map(|t| t.byte()));
            uleb(&mut payload, len32(ty.results.len()));
            payload.extend(ty.results.iter().map(|t| t.byte()));
        }
        section(&mut out, 1, &payload);
        sections += 1;
    }

    // The imports: the memory, then each global, table, tag and function that the object
    // refers to and does not define, in symbol order inside each kind. The memory always comes,
    // because the linker defines it and every object shares it.
    {
        let data: u64 = layout.segment.last().map_or(0, |start| {
            u64::from(*start) + module.segments.last().map_or(0, |s| s.bytes.len() as u64)
        });
        let mut entries: Vec<Vec<u8>> = Vec::new();
        let mut memory = Vec::new();
        name(&mut memory, "env");
        name(&mut memory, "__linear_memory");
        memory.push(0x02);
        memory.push(0x00);
        uleb(&mut memory, data.div_ceil(PAGE));
        entries.push(memory);
        let kinds: [fn(&SymbolKind) -> bool; 4] = [
            |k| matches!(k, SymbolKind::Global { .. }),
            |k| matches!(k, SymbolKind::Table { .. }),
            |k| matches!(k, SymbolKind::Tag { .. }),
            |k| matches!(k, SymbolKind::Function { .. }),
        ];
        for wanted in kinds {
            for (index, symbol) in module.symbols.iter().enumerate() {
                if !wanted(&symbol.kind) || layout.is_defined(module, index) {
                    continue;
                }
                let (from, field) = import_of(symbol);
                let mut entry = Vec::new();
                name(&mut entry, &from);
                name(&mut entry, &field);
                match symbol.kind {
                    SymbolKind::Function { ty, .. } => {
                        entry.push(0x00);
                        uleb(&mut entry, u64::from(ty));
                    }
                    SymbolKind::Table { .. } => {
                        entry.extend_from_slice(&[0x01, 0x70, 0x00]);
                        uleb(&mut entry, len32(layout.slot.iter().flatten().count()));
                    }
                    SymbolKind::Global { ty, mutable, .. } => {
                        entry.extend_from_slice(&[0x03, ty.byte(), u8::from(mutable)]);
                    }
                    SymbolKind::Tag { ty, .. } => {
                        entry.extend_from_slice(&[0x04, 0x00]);
                        uleb(&mut entry, u64::from(ty));
                    }
                    SymbolKind::Data { .. } => unreachable!("data is never imported"),
                }
                entries.push(entry);
            }
        }
        let mut payload = Vec::new();
        uleb(&mut payload, len32(entries.len()));
        for entry in entries {
            payload.extend_from_slice(&entry);
        }
        section(&mut out, 2, &payload);
        sections += 1;
    }

    if !module.functions.is_empty() {
        let mut payload = Vec::new();
        uleb(&mut payload, len32(module.functions.len()));
        for f in &module.functions {
            let SymbolKind::Function { ty, .. } = module.symbols[f.symbol as usize].kind else {
                unreachable!("the layout checked that a definition names a function symbol");
            };
            uleb(&mut payload, u64::from(ty));
        }
        section(&mut out, 3, &payload);
        sections += 1;
    }

    let exports: Vec<&Function> = module.functions.iter().filter(|f| f.export.is_some()).collect();
    if !exports.is_empty() {
        let mut payload = Vec::new();
        uleb(&mut payload, len32(exports.len()));
        for f in exports {
            name(&mut payload, f.export.as_deref().unwrap_or_default());
            payload.push(0x00);
            uleb(&mut payload, u64::from(layout.function[f.symbol as usize].unwrap_or(0)));
        }
        section(&mut out, 7, &payload);
        sections += 1;
    }

    // The code. Each body is its size, its locals and its instructions, and a fixup's offset is
    // counted from the start of the payload, which is where the linker counts from.
    let mut code_relocs = Vec::new();
    let mut code_section = None;
    if !module.functions.is_empty() {
        let mut payload = Vec::new();
        uleb(&mut payload, len32(module.functions.len()));
        for f in &module.functions {
            let mut body = Vec::new();
            uleb(&mut body, len32(f.locals.len()));
            for (count, ty) in &f.locals {
                uleb(&mut body, u64::from(*count));
                body.push(ty.byte());
            }
            let start = body.len();
            body.extend_from_slice(&f.code);
            for fixup in &f.fixups {
                let value = layout.value(module, *fixup)?;
                patch(&mut body[start..], *fixup, value)?;
            }
            uleb(&mut payload, len32(body.len()));
            let base = payload.len() + start;
            for fixup in &f.fixups {
                let offset = u32::try_from(base + fixup.at as usize).unwrap_or(u32::MAX);
                code_relocs.push(Entry { offset, fixup: *fixup });
            }
            payload.extend_from_slice(&body);
        }
        section(&mut out, 10, &payload);
        code_section = Some(sections);
        sections += 1;
    }

    // The data. Each segment is active in memory 0 at the address it would have if this were the
    // whole program. The linker moves it.
    let mut data_relocs = Vec::new();
    let mut data_section = None;
    if !module.segments.is_empty() {
        let mut payload = Vec::new();
        uleb(&mut payload, len32(module.segments.len()));
        for (s, start) in module.segments.iter().zip(&layout.segment) {
            payload.extend_from_slice(&[0x00, 0x41]);
            sleb(&mut payload, i64::from(*start as i32));
            payload.push(0x0b);
            uleb(&mut payload, len32(s.bytes.len()));
            let base = payload.len();
            let mut bytes = s.bytes.clone();
            for fixup in &s.fixups {
                let value = layout.value(module, *fixup)?;
                patch(&mut bytes, *fixup, value)?;
                let offset = u32::try_from(base + fixup.at as usize).unwrap_or(u32::MAX);
                data_relocs.push(Entry { offset, fixup: *fixup });
            }
            payload.extend_from_slice(&bytes);
        }
        section(&mut out, 11, &payload);
        data_section = Some(sections);
    }

    custom(&mut out, "linking", &linking(module, &layout));
    for (label, target, mut entries) in
        [("reloc.CODE", code_section, code_relocs), ("reloc.DATA", data_section, data_relocs)]
    {
        let Some(target) = target else { continue };
        if entries.is_empty() {
            continue;
        }
        entries.sort_by_key(|e| e.offset);
        let mut payload = Vec::new();
        uleb(&mut payload, u64::from(target));
        uleb(&mut payload, len32(entries.len()));
        for entry in &entries {
            payload.push(entry.fixup.kind.number());
            uleb(&mut payload, u64::from(entry.offset));
            uleb(&mut payload, u64::from(entry.fixup.target));
            if entry.fixup.kind.has_addend() {
                sleb(&mut payload, i64::from(entry.fixup.addend));
            }
        }
        custom(&mut out, label, &payload);
    }

    if let Some(payload) = producers(&module.producers) {
        custom(&mut out, "producers", &payload);
    }
    if let Some(payload) = target_features(module) {
        custom(&mut out, "target_features", &payload);
    }

    let defines = module
        .symbols
        .iter()
        .enumerate()
        .filter(|(index, s)| layout.is_defined(module, *index) && s.flags & LOCAL == 0)
        .map(|(_, s)| s.name.clone())
        .collect();
    Ok(Written { bytes: out, defines })
}

/// The payload of the `producers` section, or nothing when there is nothing to say. The `-S` text
/// writes the same bytes into a custom section, so the two outputs cannot disagree about it.
#[must_use]
pub fn producers(producers: &Producers) -> Option<Vec<u8>> {
    let fields: Vec<(&str, Vec<(&str, &str)>)> = [
        ("language", producers.language.iter().map(|l| (l.as_str(), "")).collect::<Vec<_>>()),
        (
            "processed-by",
            producers.processed_by.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect(),
        ),
    ]
    .into_iter()
    .filter(|(_, values)| !values.is_empty())
    .collect();
    if fields.is_empty() {
        return None;
    }
    let mut payload = Vec::new();
    uleb(&mut payload, len32(fields.len()));
    for (field, values) in fields {
        name(&mut payload, field);
        uleb(&mut payload, len32(values.len()));
        for (value, version) in values {
            name(&mut payload, value);
            name(&mut payload, version);
        }
    }
    Some(payload)
}

/// The payload of the `target_features` section, or nothing when the module names no feature.
/// The `-S` text writes these bytes too.
#[must_use]
pub fn target_features(module: &Module) -> Option<Vec<u8>> {
    if module.features.is_empty() && module.disallowed.is_empty() {
        return None;
    }
    let mut payload = Vec::new();
    uleb(&mut payload, len32(module.features.len() + module.disallowed.len()));
    for (prefix, list) in [(b'+', &module.features), (b'-', &module.disallowed)] {
        for feature in list {
            payload.push(prefix);
            name(&mut payload, feature);
        }
    }
    Some(payload)
}

/// The payload of the `linking` section: the version, then the symbol table, the segment names
/// and the constructors, in that fixed order so that two objects compare byte for byte.
fn linking(module: &Module, layout: &Layout) -> Vec<u8> {
    let mut payload = Vec::new();
    uleb(&mut payload, u64::from(LINKING_VERSION));

    let mut table = Vec::new();
    uleb(&mut table, len32(module.symbols.len()));
    for (index, symbol) in module.symbols.iter().enumerate() {
        let defined = layout.is_defined(module, index);
        let mut flags = symbol.flags & !(UNDEFINED | EXPLICIT_NAME);
        if !defined {
            flags |= UNDEFINED;
        }
        let explicit = !defined
            && !matches!(symbol.kind, SymbolKind::Data { .. })
            && import_of(symbol).1 != symbol.name;
        if explicit {
            flags |= EXPLICIT_NAME;
        }
        let kind = match symbol.kind {
            SymbolKind::Function { .. } => 0,
            SymbolKind::Data { .. } => 1,
            SymbolKind::Global { .. } => 2,
            SymbolKind::Tag { .. } => 4,
            SymbolKind::Table { .. } => 5,
        };
        table.push(kind);
        uleb(&mut table, u64::from(flags));
        match symbol.kind {
            SymbolKind::Data { place } => {
                name(&mut table, &symbol.name);
                if let Some(place) = place {
                    uleb(&mut table, u64::from(place.segment));
                    uleb(&mut table, u64::from(place.offset));
                    uleb(&mut table, u64::from(place.size));
                }
            }
            _ => {
                let number = match symbol.kind {
                    SymbolKind::Function { .. } => layout.function[index],
                    _ => layout.other[index],
                };
                uleb(&mut table, u64::from(number.unwrap_or(0)));
                if defined || explicit {
                    name(&mut table, &symbol.name);
                }
            }
        }
    }
    subsection(&mut payload, 8, &table);

    if !module.segments.is_empty() {
        let mut info = Vec::new();
        uleb(&mut info, len32(module.segments.len()));
        for s in &module.segments {
            name(&mut info, &s.name);
            uleb(&mut info, u64::from(s.align));
            uleb(&mut info, u64::from(s.flags));
        }
        subsection(&mut payload, 5, &info);
    }

    if !module.inits.is_empty() {
        let mut inits = Vec::new();
        uleb(&mut inits, len32(module.inits.len()));
        for (priority, symbol) in &module.inits {
            uleb(&mut inits, u64::from(*priority));
            uleb(&mut inits, u64::from(*symbol));
        }
        subsection(&mut payload, 6, &inits);
    }
    payload
}

fn subsection(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
    out.push(id);
    uleb(out, len32(payload.len()));
    out.extend_from_slice(payload);
}
