//! The module: the target it is for, its functions, its globals, its aliases and its metadata.
//!
//! Design: `spec/08-ir.md` sections 8.1 and 8.8.
//!
//! A module is one translation unit, or after LTO the several that were linked into one. It
//! owns the functions rather than pointing at them, so the whole of a compilation is one value
//! that is dropped in one go, and a reference to anything in it is a four-byte index.
//!
//! # Globals are bytes, not values
//!
//! There are no aggregate types in the IR, so a global's initializer cannot be a typed
//! constant the way it is in LLVM. It is a sized, aligned image described by a run of
//! [`Datum`]s: zero bytes, literal bytes, a scalar of a given IR type, or the address of
//! another symbol. That is what an object file wants anyway, it needs no type the type system
//! does not have, and a large `static const` table costs one [`Datum`] rather than one per
//! element.
//!
//! # What the module does not hold
//!
//! It does not hold an [`Interner`](rucc_base::Interner). Every name in here is a
//! [`Symbol`], and resolving one back to text needs the interner it came from, which the
//! printer takes as an argument the way `rucc_ast::print` does. A module that owned one could
//! not be built from the same session as the AST it was lowered from.
//!
//! Function attributes are not here yet. They arrive with the printer, which is where their
//! spelling has to be settled.

use std::collections::HashMap;
use std::fmt;
use std::ops::{Index, IndexMut};

use rucc_base::float::Format;
use rucc_base::{Idx, IdxRange, Symbol};
use rucc_target::{TargetInfo, Triple};

use crate::func::Func;
use crate::inst::{Imm, Meta, MetaNode};
use crate::ty::Type;

/// A function in a module.
pub type FuncId = Idx<Func>;

/// A global variable in a module.
pub type GlobalId = Idx<Global>;

/// An alias in a module.
pub type AliasId = Idx<Alias>;

/// A run of [`Datum`]s in a module's data pool, which is what a global's initializer is.
pub type DataList = IdxRange<Datum>;

/// Marker for the byte pool, so that a range into it cannot be confused with any other range.
#[derive(Debug)]
pub struct Byte;

/// A run of literal bytes in a module's byte pool.
pub type ByteRange = IdxRange<Byte>;

/// How a symbol is seen outside the object it is defined in.
///
/// The set is the one C needs and no more. C++ vague linkage and the ODR variants are not
/// here because nothing produces them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Linkage {
    /// Defined here and visible to every other object. The default, and what a plain
    /// definition at file scope gets.
    #[default]
    External,
    /// Defined here and invisible outside it, which is what `static` at file scope means.
    Internal,
    /// Defined here, visible, and allowed to be replaced by a strong definition elsewhere.
    /// `__attribute__((weak))`. A reference to one that nothing defines is a null address
    /// rather than a link error.
    Weak,
    /// Defined here, visible, and allowed to be identical to a definition in another object,
    /// with one of them kept and the rest discarded. What `extern inline` under the GNU
    /// semantics and a compiler-generated helper get.
    LinkOnce,
    /// A tentative definition, which the linker merges with any other tentative definition of
    /// the same name and any real definition. `int x;` at file scope under `-fcommon`.
    Common,
}

impl Linkage {
    /// The spelling in the textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Internal => "internal",
            Self::Weak => "weak",
            Self::LinkOnce => "linkonce",
            Self::Common => "common",
        }
    }

    /// The linkage that spelling names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|linkage| linkage.name() == name)
    }

    /// Every linkage, in declaration order.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::External, Self::Internal, Self::Weak, Self::LinkOnce, Self::Common].into_iter()
    }

    /// Whether the symbol is invisible outside this object, so that a pass may rewrite every
    /// use of it because it can see every use of it.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Internal)
    }

    /// Whether the definition here may lose to one in another object at link time.
    ///
    /// The optimizer must not fold a use against the definition it can see when this is true,
    /// because the definition that wins may be a different one.
    #[must_use]
    pub const fn may_be_replaced(self) -> bool {
        matches!(self, Self::Weak | Self::LinkOnce | Self::Common)
    }
}

/// What the dynamic linker is allowed to do with a symbol.
///
/// Orthogonal to [`Linkage`], which is about the static linker. A hidden symbol is still
/// external as far as the object file is concerned; it just does not go in the dynamic symbol
/// table, so nothing outside the shared object can interpose it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Visibility {
    /// Exported and interposable, which is what a symbol in a shared library gets unless
    /// something says otherwise.
    #[default]
    Default,
    /// Not in the dynamic symbol table at all. `__attribute__((visibility("hidden")))` and
    /// `-fvisibility=hidden`.
    Hidden,
    /// In the dynamic symbol table, but a reference from inside this shared object always
    /// binds to the definition inside it.
    Protected,
}

impl Visibility {
    /// The spelling in the textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Hidden => "hidden",
            Self::Protected => "protected",
        }
    }

    /// The visibility that spelling names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|visibility| visibility.name() == name)
    }

    /// Every visibility, in declaration order.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::Default, Self::Hidden, Self::Protected].into_iter()
    }
}

/// How a thread-local variable is reached.
///
/// The models are ordered from the most general to the fastest, and a model may always be
/// replaced by a more general one. The frontend picks from the storage class and the
/// visibility, `-ftls-model=` overrides it, and the linker may relax a general one into a
/// faster one when it turns out the definition is in the executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum TlsModel {
    /// Works for any variable in any object, at the cost of a call to `__tls_get_addr`.
    #[default]
    GlobalDynamic,
    /// One call to `__tls_get_addr` for several variables that are known to share a module.
    LocalDynamic,
    /// The offset is loaded from the GOT. Needs the variable to be in a module loaded at
    /// program start rather than by `dlopen`.
    InitialExec,
    /// The offset is a link-time constant. Only for a variable in the executable itself.
    LocalExec,
}

impl TlsModel {
    /// The spelling in the textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::GlobalDynamic => "global_dynamic",
            Self::LocalDynamic => "local_dynamic",
            Self::InitialExec => "initial_exec",
            Self::LocalExec => "local_exec",
        }
    }

    /// The model that spelling names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|model| model.name() == name)
    }

    /// Every model, from the most general to the fastest.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::GlobalDynamic, Self::LocalDynamic, Self::InitialExec, Self::LocalExec].into_iter()
    }
}

/// One piece of a global's initial image.
///
/// Sixteen bytes, so an initializer built out of them is a flat array and a table of a
/// million bytes is one of these rather than a million.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Datum {
    /// That many zero bytes. What `.bss` is made of, and what the tail of a partly
    /// initialized array is.
    Zero(u64),
    /// Those literal bytes, from the module's byte pool. String literals and anything the
    /// frontend has already laid out.
    Bytes(ByteRange),
    /// One scalar of that IR type, from the module's immediate pool. An integer holds its
    /// value and a float holds its bit pattern, both target-independently: which byte comes
    /// first is decided by the datalayout when the object file is written, not here.
    Scalar {
        /// The type of the scalar, which gives its width.
        ty: Type,
        /// Its value, in the module's immediate pool.
        value: Idx<Imm>,
    },
    /// The address of another symbol, from the module's relocation pool. `&x` in an
    /// initializer, which the linker fills in.
    Addr(Idx<Reloc>),
}

impl Datum {
    /// How many bytes it contributes to the image.
    ///
    /// The module is an argument because three of the four kinds keep what they are made of in
    /// one of its pools, and a datum on its own is four words that mean nothing without it.
    #[must_use]
    pub fn size(self, module: &Module) -> u64 {
        match self {
            Self::Zero(bytes) => bytes,
            Self::Bytes(range) => range.len() as u64,
            // Rounded up, so that an `i1` in an image is a byte and a `_BitInt(24)` is three.
            Self::Scalar { ty, .. } => u64::from(ty.bits().div_ceil(8)) * u64::from(ty.lanes()),
            Self::Addr(reloc) => u64::from(module[reloc].size),
        }
    }
}

/// The address of a symbol, written into a global's image by the linker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reloc {
    /// The symbol whose address this is.
    pub symbol: Symbol,
    /// What to add to that address. `&array[2]` is the address of `array` plus eight.
    pub addend: i64,
    /// How many bytes the address occupies, which is the pointer width except where a target
    /// has a smaller relocation for it.
    pub size: u32,
}

/// A global variable.
///
/// A size and an alignment and an image, which is what the object writer needs. `init` is
/// `None` for a declaration of something defined in another object, which is the only thing
/// that distinguishes the two.
#[derive(Debug, Clone)]
pub struct Global {
    /// The name it is reached by.
    pub name: Symbol,
    /// Its size in bytes, which the image must add up to.
    pub size: u64,
    /// Its required alignment in bytes, always a power of two.
    pub align: u32,
    /// How the linker sees it.
    pub linkage: Linkage,
    /// How the dynamic linker sees it.
    pub visibility: Visibility,
    /// The model to reach it by if it is thread-local, and `None` if it is not.
    pub tls: Option<TlsModel>,
    /// Whether writing through a pointer to it is undefined, which is what puts it in
    /// `.rodata` rather than `.data`.
    pub constant: bool,
    /// The section to put it in, from `__attribute__((section(...)))`, or `None` to let the
    /// object writer choose from the other fields.
    pub section: Option<Symbol>,
    /// Its initial image, or `None` if it is only declared here.
    pub init: Option<DataList>,
}

impl Global {
    /// A definition-less global of that size and alignment, external and not thread-local.
    #[must_use]
    pub fn new(name: Symbol, size: u64, align: u32) -> Self {
        Self {
            name,
            size,
            align,
            linkage: Linkage::External,
            visibility: Visibility::Default,
            tls: None,
            constant: false,
            section: None,
            init: None,
        }
    }

    /// Whether this only says the variable exists somewhere.
    #[must_use]
    pub fn is_declaration(&self) -> bool {
        self.init.is_none()
    }
}

/// What an alias resolves to at link time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum AliasKind {
    /// A second name for a symbol in this same object, resolved by the assembler.
    /// `__attribute__((alias("real")))`.
    #[default]
    Alias,
    /// A name resolved once at program start by calling a resolver function in this object,
    /// which picks an implementation from what the processor turns out to support.
    /// `__attribute__((ifunc("resolver")))`, which is how glibc dispatches `memcpy`.
    IFunc,
}

impl AliasKind {
    /// The spelling in the textual form.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::IFunc => "ifunc",
        }
    }

    /// The kind that spelling names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "alias" => Some(Self::Alias),
            "ifunc" => Some(Self::IFunc),
            _ => None,
        }
    }
}

/// A second name for something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alias {
    /// The name being defined.
    pub name: Symbol,
    /// What it resolves to: the aliased symbol, or for an ifunc the resolver to call.
    pub target: Symbol,
    /// Which of those two it is.
    pub kind: AliasKind,
    /// How the linker sees the new name.
    pub linkage: Linkage,
    /// How the dynamic linker sees the new name.
    pub visibility: Visibility,
}

impl Alias {
    /// An external alias of `target`.
    #[must_use]
    pub fn new(name: Symbol, target: Symbol) -> Self {
        Self {
            name,
            target,
            kind: AliasKind::Alias,
            linkage: Linkage::External,
            visibility: Visibility::Default,
        }
    }
}

/// What a name in a module refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolRef {
    /// A function, defined or declared.
    Func(FuncId),
    /// A global variable, defined or declared.
    Global(GlobalId),
    /// An alias or an ifunc.
    Alias(AliasId),
}

/// The layout facts a printed module carries so it can be compiled without the command line
/// that produced it.
///
/// A subset of the string LLVM writes, in the same syntax, because that syntax is what tools
/// around the ecosystem already read. It says what the module was built assuming, and the
/// verifier is what checks it against the target actually being compiled for: a module built
/// for a 64-bit pointer cannot be finished for a 32-bit one, and finding that out here is
/// better than finding it out as wrong output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataLayout {
    /// Whether the low byte of a scalar is stored first.
    pub little_endian: bool,
    /// The width of a pointer in bits.
    pub pointer_bits: u32,
    /// The alignment of a pointer in bits.
    pub pointer_align: u32,
    /// The alignment of a 64-bit integer in bits, which is the one integer alignment that
    /// varies across the targets anybody still builds for.
    pub i64_align: u32,
    /// The alignment of the x87 eighty bit format in bits, and `None` on a target that does
    /// not have it.
    pub f80_align: Option<u32>,
    /// The alignment the stack is kept at in bits, which is 128 on every target here.
    pub stack_align: u32,
}

impl DataLayout {
    /// The layout of that target.
    #[must_use]
    pub fn for_target(target: &TargetInfo) -> Self {
        Self {
            little_endian: target.little_endian,
            pointer_bits: target.pointer_width,
            pointer_align: target.pointer_width,
            // Every target here is one where a 64-bit integer is 64-bit aligned. The field
            // exists because a 32-bit x86 target, if one is ever added, aligns it to 32 and
            // that changes the layout of every struct with a `long long` in it.
            i64_align: 64,
            f80_align: match target.long_double_format {
                Format::X87Extended => Some(128),
                _ => None,
            },
            stack_align: 128,
        }
    }

    /// The layout back from the string [`Display`](fmt::Display) wrote, or `None` if the
    /// string is not one.
    ///
    /// The fields may come in any order, because a string written by hand will not have them
    /// in ours. A string this crate printed round-trips byte for byte, which is what
    /// `spec/03-architecture.md` asks of the textual form.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut little_endian = None;
        let mut pointer = None;
        let mut i64_align = None;
        let mut f80_align = None;
        let mut stack_align = None;
        for field in text.split('-') {
            let seen = match field {
                "e" => little_endian.replace(true).is_some(),
                "E" => little_endian.replace(false).is_some(),
                _ if field.starts_with("p:") => {
                    let (bits, align) = field[2..].split_once(':')?;
                    pointer.replace((number(bits)?, number(align)?)).is_some()
                }
                _ if field.starts_with("i64:") => i64_align.replace(number(&field[4..])?).is_some(),
                _ if field.starts_with("f80:") => f80_align.replace(number(&field[4..])?).is_some(),
                _ if field.starts_with('S') => stack_align.replace(number(&field[1..])?).is_some(),
                _ => return None,
            };
            if seen {
                return None;
            }
        }
        let (pointer_bits, pointer_align) = pointer?;
        Some(Self {
            little_endian: little_endian?,
            pointer_bits,
            pointer_align,
            i64_align: i64_align?,
            f80_align,
            stack_align: stack_align?,
        })
    }
}

impl fmt::Display for DataLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", if self.little_endian { "e" } else { "E" })?;
        write!(f, "-p:{}:{}", self.pointer_bits, self.pointer_align)?;
        write!(f, "-i64:{}", self.i64_align)?;
        if let Some(align) = self.f80_align {
            write!(f, "-f80:{align}")?;
        }
        write!(f, "-S{}", self.stack_align)
    }
}

/// A number in the textual form: digits, no sign, and no leading zero.
///
/// `p:64:064` would otherwise parse and then print back as `p:64:64`, which breaks the
/// round-trip for no benefit to anybody.
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// One translation unit, or after LTO the several that were linked into one.
#[derive(Debug)]
pub struct Module {
    /// What it is called, which is the source file name for a module from the frontend. It
    /// appears in the textual form and in the debug info and nothing branches on it.
    pub name: Symbol,
    /// The target it is for.
    pub triple: Triple,
    /// The layout it was built assuming.
    pub datalayout: DataLayout,

    funcs: Vec<Func>,
    globals: Vec<Global>,
    aliases: Vec<Alias>,
    metadata: Vec<MetaNode>,

    data: Vec<Datum>,
    bytes: Vec<u8>,
    imms: Vec<Imm>,
    relocs: Vec<Reloc>,

    symbols: HashMap<Symbol, SymbolRef>,
}

impl Module {
    /// An empty module for that target.
    #[must_use]
    pub fn new(name: Symbol, target: &TargetInfo) -> Self {
        Self {
            name,
            triple: target.triple,
            datalayout: DataLayout::for_target(target),
            funcs: Vec::new(),
            globals: Vec::new(),
            aliases: Vec::new(),
            metadata: Vec::new(),
            data: Vec::new(),
            bytes: Vec::new(),
            imms: Vec::new(),
            relocs: Vec::new(),
            symbols: HashMap::new(),
        }
    }

    // Symbols.

    /// Adds a function, which is a declaration if it has no blocks.
    ///
    /// # Panics
    ///
    /// Panics if the module already has a symbol of that name. Merging a declaration with a
    /// definition is the frontend's job and it has the declarations to do it with; by the time
    /// something is in the IR a name means one thing.
    pub fn add_func(&mut self, func: Func) -> FuncId {
        let id = Idx::from_usize(self.funcs.len());
        self.claim(func.name, SymbolRef::Func(id));
        self.funcs.push(func);
        id
    }

    /// Adds a global variable, which is a declaration if it has no image.
    ///
    /// # Panics
    ///
    /// Panics if the module already has a symbol of that name.
    pub fn add_global(&mut self, global: Global) -> GlobalId {
        let id = Idx::from_usize(self.globals.len());
        self.claim(global.name, SymbolRef::Global(id));
        self.globals.push(global);
        id
    }

    /// Adds an alias.
    ///
    /// The target is not resolved here, and it need not be in this module: an alias of
    /// something in another object is a thing people write.
    ///
    /// # Panics
    ///
    /// Panics if the module already has a symbol of that name.
    pub fn add_alias(&mut self, alias: Alias) -> AliasId {
        let id = Idx::from_usize(self.aliases.len());
        self.claim(alias.name, SymbolRef::Alias(id));
        self.aliases.push(alias);
        id
    }

    /// What that name refers to, or `None` if this module does not define or declare it.
    #[must_use]
    pub fn lookup(&self, name: Symbol) -> Option<SymbolRef> {
        self.symbols.get(&name).copied()
    }

    /// Every function, in the order they were added.
    pub fn funcs(&self) -> impl Iterator<Item = FuncId> + use<> {
        (0..self.funcs.len()).map(Idx::from_usize)
    }

    /// Every global variable, in the order they were added.
    pub fn globals(&self) -> impl Iterator<Item = GlobalId> + use<> {
        (0..self.globals.len()).map(Idx::from_usize)
    }

    /// Every alias, in the order they were added.
    pub fn aliases(&self) -> impl Iterator<Item = AliasId> + use<> {
        (0..self.aliases.len()).map(Idx::from_usize)
    }

    fn claim(&mut self, name: Symbol, what: SymbolRef) {
        assert!(
            self.symbols.insert(name, what).is_none(),
            "a module cannot have two symbols with the same name"
        );
    }

    // Metadata.

    /// Adds a metadata node and gives back the reference an instruction holds.
    ///
    /// The nodes live here rather than in a function because a TBAA tree is shared by every
    /// memory operation in the module and duplicating it per function would make two accesses
    /// to the same type look unrelated.
    pub fn add_meta(&mut self, node: MetaNode) -> Meta {
        self.metadata.push(node);
        Idx::from_usize(self.metadata.len() - 1)
    }

    /// Every metadata node, in the order they were added.
    pub fn metadata(&self) -> impl Iterator<Item = Meta> + use<> {
        (0..self.metadata.len()).map(Idx::from_usize)
    }

    // Pools.

    /// Records a run of data and gives back the list a global holds.
    pub fn push_data(&mut self, data: &[Datum]) -> DataList {
        let start = self.data.len();
        self.data.extend_from_slice(data);
        DataList::new(Idx::from_usize(start), Idx::from_usize(self.data.len()))
    }

    /// Records literal bytes and gives back the range a [`Datum::Bytes`] holds.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> ByteRange {
        let start = self.bytes.len();
        self.bytes.extend_from_slice(bytes);
        ByteRange::new(Idx::from_usize(start), Idx::from_usize(self.bytes.len()))
    }

    /// Records a scalar value and gives back the index a [`Datum::Scalar`] holds.
    pub fn add_imm(&mut self, imm: Imm) -> Idx<Imm> {
        self.imms.push(imm);
        Idx::from_usize(self.imms.len() - 1)
    }

    /// Records a relocation and gives back the index a [`Datum::Addr`] holds.
    pub fn add_reloc(&mut self, reloc: Reloc) -> Idx<Reloc> {
        self.relocs.push(reloc);
        Idx::from_usize(self.relocs.len() - 1)
    }

    /// How much is in it, for the `-fstats` output and for a test that wants to say a pass
    /// deleted something without saying which.
    #[must_use]
    pub fn counts(&self) -> ModuleCounts {
        ModuleCounts {
            funcs: self.funcs.len(),
            globals: self.globals.len(),
            aliases: self.aliases.len(),
            metadata: self.metadata.len(),
            data_bytes: self.bytes.len(),
        }
    }
}

/// How much is in a module, from [`Module::counts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleCounts {
    /// Functions, defined and declared.
    pub funcs: usize,
    /// Global variables, defined and declared.
    pub globals: usize,
    /// Aliases and ifuncs.
    pub aliases: usize,
    /// Metadata nodes.
    pub metadata: usize,
    /// Bytes in the byte pool, which is the bulk of what a module with large initializers
    /// weighs.
    pub data_bytes: usize,
}

impl Index<FuncId> for Module {
    type Output = Func;

    fn index(&self, id: FuncId) -> &Func {
        &self.funcs[id.index()]
    }
}

impl IndexMut<FuncId> for Module {
    fn index_mut(&mut self, id: FuncId) -> &mut Func {
        &mut self.funcs[id.index()]
    }
}

impl Index<GlobalId> for Module {
    type Output = Global;

    fn index(&self, id: GlobalId) -> &Global {
        &self.globals[id.index()]
    }
}

impl IndexMut<GlobalId> for Module {
    fn index_mut(&mut self, id: GlobalId) -> &mut Global {
        &mut self.globals[id.index()]
    }
}

impl Index<AliasId> for Module {
    type Output = Alias;

    fn index(&self, id: AliasId) -> &Alias {
        &self.aliases[id.index()]
    }
}

impl Index<Meta> for Module {
    type Output = MetaNode;

    fn index(&self, meta: Meta) -> &MetaNode {
        &self.metadata[meta.index()]
    }
}

impl Index<Idx<Imm>> for Module {
    type Output = Imm;

    fn index(&self, imm: Idx<Imm>) -> &Imm {
        &self.imms[imm.index()]
    }
}

impl Index<Idx<Reloc>> for Module {
    type Output = Reloc;

    fn index(&self, reloc: Idx<Reloc>) -> &Reloc {
        &self.relocs[reloc.index()]
    }
}

impl Index<DataList> for Module {
    type Output = [Datum];

    fn index(&self, list: DataList) -> &[Datum] {
        &self.data[list.as_usize_range()]
    }
}

impl Index<ByteRange> for Module {
    type Output = [u8];

    fn index(&self, range: ByteRange) -> &[u8] {
        &self.bytes[range.as_usize_range()]
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_target::{Arch, Env, Os};

    use super::*;
    use crate::inst::Signature;

    fn target(arch: Arch, os: Os, env: Env) -> TargetInfo {
        TargetInfo::new(Triple::new(arch, os, env))
    }

    fn linux() -> TargetInfo {
        target(Arch::X86_64, Os::Linux, Env::Gnu)
    }

    #[test]
    fn a_datum_is_sixteen_bytes() {
        // A global with a large initializer is a flat array of these, so this is the tripwire
        // on somebody adding a field that doubles the weight of every one.
        assert_eq!(size_of::<Datum>(), 16);
    }

    #[test]
    fn the_layout_of_x86_64_linux_is_the_one_in_the_spec() {
        let layout = DataLayout::for_target(&linux());
        assert_eq!(layout.to_string(), "e-p:64:64-i64:64-f80:128-S128");
    }

    #[test]
    fn only_x86_has_the_eighty_bit_format() {
        assert_eq!(DataLayout::for_target(&linux()).f80_align, Some(128));
        let arm = DataLayout::for_target(&target(Arch::Aarch64, Os::Linux, Env::Gnu));
        assert_eq!(arm.f80_align, None);
        assert_eq!(arm.to_string(), "e-p:64:64-i64:64-S128");
    }

    #[test]
    fn a_layout_round_trips() {
        for triple in [
            Triple::new(Arch::X86_64, Os::Linux, Env::Gnu),
            Triple::new(Arch::X86_64, Os::Darwin, Env::None),
            Triple::new(Arch::Aarch64, Os::Darwin, Env::None),
            Triple::new(Arch::Riscv64, Os::Linux, Env::Musl),
        ] {
            let layout = DataLayout::for_target(&TargetInfo::new(triple));
            let text = layout.to_string();
            assert_eq!(DataLayout::parse(&text), Some(layout), "{text}");
        }
    }

    #[test]
    fn a_layout_may_be_written_in_any_order() {
        let text = "S128-i64:64-f80:128-p:64:64-e";
        assert_eq!(DataLayout::parse(text), Some(DataLayout::for_target(&linux())));
    }

    #[test]
    fn a_layout_needs_every_field_it_prints() {
        for text in ["", "e", "e-p:64:64-S128", "e-i64:64-S128", "e-p:64:64-i64:64"] {
            assert_eq!(DataLayout::parse(text), None, "{text}");
        }
    }

    #[test]
    fn a_layout_refuses_a_second_spelling() {
        // Each of these would print back as something else, which breaks the round-trip.
        for text in ["e-p:64:064-i64:64-S128", "e-e-p:64:64-i64:64-S128", "e-p:64:64-i64:64-S128-x"]
        {
            assert_eq!(DataLayout::parse(text), None, "{text}");
        }
    }

    #[test]
    fn a_module_finds_what_it_holds() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("test.c"), &linux());

        let counter = names.intern("counter");
        let sum = names.intern("sum");
        let total = names.intern("total");

        let global = module.add_global(Global::new(counter, 4, 4));
        let func = module.add_func(Func::new(sum, Signature::new()));
        let alias = module.add_alias(Alias::new(total, counter));

        assert_eq!(module.lookup(counter), Some(SymbolRef::Global(global)));
        assert_eq!(module.lookup(sum), Some(SymbolRef::Func(func)));
        assert_eq!(module.lookup(total), Some(SymbolRef::Alias(alias)));
        assert_eq!(module.lookup(names.intern("nothing")), None);
        assert_eq!(module[alias].target, counter);
        assert!(module[global].is_declaration());
        assert!(module[func].is_declaration());
    }

    #[test]
    #[should_panic(expected = "two symbols with the same name")]
    fn a_name_means_one_thing() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("test.c"), &linux());
        let name = names.intern("x");
        module.add_global(Global::new(name, 4, 4));
        module.add_func(Func::new(name, Signature::new()));
    }

    #[test]
    fn an_initializer_adds_up_to_the_size() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("test.c"), &linux());

        // struct { int n; const char *name; char pad[6]; } = { 7, "hi", { 0 } };
        let text = names.intern("hi.str");
        let seven = module.add_imm(Imm::int(7, Type::int(32)));
        let bytes = module.push_bytes(b"hi\0");
        let addr = module.add_reloc(Reloc { symbol: text, addend: 0, size: 8 });
        let init = module.push_data(&[
            Datum::Scalar { ty: Type::int(32), value: seven },
            Datum::Zero(4),
            Datum::Addr(addr),
            // The six bytes of `pad` and the two the struct is tailed out with. Padding is
            // the frontend's arithmetic, and the image is what it came out as.
            Datum::Zero(8),
        ]);

        let mut global = Global::new(names.intern("entry"), 24, 8);
        global.init = Some(init);
        global.constant = true;
        let id = module.add_global(global);

        assert!(!module[id].is_declaration());
        let size: u64 = module[init].iter().map(|datum| datum.size(&module)).sum();
        assert_eq!(size, module[id].size);
        assert_eq!(&module[bytes], b"hi\0");
        assert_eq!(module[seven].unsigned(), 7);
        assert_eq!(module.counts().data_bytes, 3);
    }

    #[test]
    fn a_scalar_datum_is_as_wide_as_its_type() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("test.c"), &linux());
        let value = module.add_imm(Imm::int(0, Type::int(32)));
        assert_eq!(Datum::Scalar { ty: Type::int(32), value }.size(&module), 4);
        // Rounded up to whole bytes, one lane at a time.
        assert_eq!(Datum::Scalar { ty: Type::I1, value }.size(&module), 1);
        assert_eq!(Datum::Scalar { ty: Type::int(24), value }.size(&module), 3);
        assert_eq!(Datum::Scalar { ty: Type::vector(Type::int(8), 16), value }.size(&module), 16);
    }

    #[test]
    fn the_names_round_trip() {
        for linkage in Linkage::all() {
            assert_eq!(Linkage::from_name(linkage.name()), Some(linkage));
        }
        for visibility in Visibility::all() {
            assert_eq!(Visibility::from_name(visibility.name()), Some(visibility));
        }
        for model in TlsModel::all() {
            assert_eq!(TlsModel::from_name(model.name()), Some(model));
        }
        for kind in [AliasKind::Alias, AliasKind::IFunc] {
            assert_eq!(AliasKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(Linkage::from_name("static"), None);
        assert_eq!(Visibility::from_name("internal"), None);
    }

    #[test]
    fn only_internal_linkage_is_local() {
        for linkage in Linkage::all() {
            assert_eq!(linkage.is_local(), linkage == Linkage::Internal);
            assert_eq!(
                linkage.may_be_replaced(),
                !matches!(linkage, Linkage::External | Linkage::Internal)
            );
        }
    }

    #[test]
    fn metadata_is_shared_by_the_whole_module() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("test.c"), &linux());
        let char_node = module.add_meta(MetaNode {
            name: names.intern("omnipotent char"),
            parent: None,
            offset: 0,
        });
        let int_node = module.add_meta(MetaNode {
            name: names.intern("int"),
            parent: Some(char_node),
            offset: 0,
        });
        assert_eq!(module[int_node].parent, Some(char_node));
        assert_eq!(module.metadata().count(), 2);
    }
}
