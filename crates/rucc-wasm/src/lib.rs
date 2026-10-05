//! The WebAssembly back end.
//!
//! Design: the WebAssembly notes, documents 06 and 07. Layer rank 12, see
//! `spec/18-package-layout.md`.
//!
//! # Status
//!
//! This is the first step of #2864. It takes a module of IR for `wasm32` and writes a relocatable
//! object of the form that `wasm-ld` reads, which is the form clang writes: one function for each
//! defined function, one data segment for each defined variable, a symbol table, relocations, and
//! the custom sections `producers` and `target_features`. An object that this writes links with
//! `wasm-ld` against the wasi-sdk sysroot and runs under Wasmtime.
//!
//! The translation is direct. Each value of the IR gets a local of its own, each instruction
//! reads its operands with `local.get` and writes its result with `local.set`, and constants are
//! written where they are used. The graph of blocks becomes `block`, `loop` and `if` by the
//! algorithm of Ramsey, "Beyond Relooper" (ICFP 2022), and the arguments of a branch are written
//! to the parameters of its target as a parallel copy just before the branch. There is no
//! register allocation of locals, no folding of an address into the offset of a load, and no use
//! of the operand stack across instructions. Those come with the selection rules of a later step,
//! and the code that this writes is the reference that they are checked against.
//!
//! The calling convention is the Basic C ABI of `tool-conventions`, which `rucc-abi` has already
//! applied by the time the IR is here. An aggregate travels by reference or as its one scalar, and
//! the extra arguments of a variadic call go in a buffer on the stack, whose address is one more
//! `i32` parameter after the fixed ones. The stack is the linear memory below the global
//! `__stack_pointer`, which the linker defines.
//!
//! A value of a type narrower than 32 bits is held in an `i32` whose upper bits are undefined, and
//! the instructions that read the upper bits extend the value first. That is the convention of
//! LLVM, and it makes arithmetic on narrow types cost nothing.
//!
//! What this refuses, with a message that names the function: an `i128`, a `long double`, a
//! vector, a graph of blocks that is not reducible, `setjmp`, inline assembly, a computed `goto`,
//! an alias, and the instructions of the memory safety monitor. Each of these is a later step of
//! #2864 or of the milestones after it.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-wasm/0.22.0")]

mod emit;
mod select;
mod structure;

use std::fmt;

use rucc_base::hash::Map;
use rucc_base::{Interner, Symbol};
use rucc_ir::{Abi, Datum, Linkage, Module, Signature, SymbolRef, Type};
use rucc_object::wasm::{
    self, FuncType, HIDDEN, LOCAL, NO_STRIP, Place, Producers, RETAIN, RelocKind, Segment,
    SymbolKind, ValType, WEAK, Written,
};
use rucc_target::wasm::{Feature, Features};

/// Why a module could not be translated. Each one is a part of C that this back end does not
/// translate yet, and not a mistake in the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The function where it was found, when it was found in one.
    pub function: Option<String>,
    pub why: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.function {
            Some(function) => {
                write!(f, "the wasm backend cannot translate `{function}` yet: {}", self.why)
            }
            None => write!(f, "the wasm backend cannot translate this unit yet: {}", self.why),
        }
    }
}

impl std::error::Error for Refusal {}

/// Translate `module` into a relocatable wasm object, with the instructions that `features`
/// allows.
///
/// # Errors
///
/// A [`Refusal`] for the first part of the module that this back end does not translate yet.
///
/// # Panics
///
/// When the module has 2^32 data segments or more, which a module under 4 GiB cannot have.
pub fn generate(module: &Module, names: &Interner, features: Features) -> Result<Written, Refusal> {
    let unit_wide = |why: String| Refusal { function: None, why };
    if module.aliases().next().is_some() {
        return Err(unit_wide("an alias is not written for wasm yet".into()));
    }
    let mut unit = Unit {
        ir: module,
        names,
        features,
        out: wasm::Module::default(),
        functions: Map::default(),
        data: Map::default(),
        stack_pointer: None,
        table: None,
    };

    // Every definition gets its symbol first, so that a reference from a function or from data
    // finds the symbol whatever order the definitions come in.
    let mut bodies = Vec::new();
    for id in module.funcs() {
        let func = &module[id];
        if func.is_declaration() {
            continue;
        }
        let symbol = unit.function(func.name).map_err(unit_wide)?.0;
        bodies.push((id, symbol));
    }
    let mut variables = Vec::new();
    for id in module.globals() {
        let global = &module[id];
        if global.is_declaration() {
            continue;
        }
        let name = names.resolve(global.name).to_owned();
        let segment = u32::try_from(unit.out.segments.len()).expect("fewer than 2^32 segments");
        let size = u32::try_from(global.size)
            .map_err(|_| unit_wide(format!("`{name}` is larger than the 4 GiB of wasm32")))?;
        let place = Some(Place { segment, offset: 0, size });
        let flags = flags(global.linkage);
        let symbol = unit.out.symbol(name.clone(), SymbolKind::Data { place }, flags);
        unit.data.insert(name.clone(), symbol);
        unit.out.segments.push(Segment::default());
        variables.push((id, name));
    }

    for (id, name) in variables {
        let segment = unit.segment(&module[id], &name).map_err(|why| Refusal {
            function: None,
            why: format!("the initial value of `{name}` {why}"),
        })?;
        let at = segment_index(&unit, &name);
        unit.out.segments[at] = segment;
    }
    for (id, symbol) in bodies {
        let function = select::function(&mut unit, id, symbol).map_err(|why| Refusal {
            function: Some(names.resolve(module[id].name).to_owned()),
            why,
        })?;
        unit.out.functions.push(function);
    }

    unit.out.features = features.iter().map(|f| f.name().to_owned()).collect();
    if !features.has(Feature::Atomics) {
        unit.out.disallowed = vec!["shared-mem".into()];
    }
    unit.out.producers = Producers {
        language: None,
        processed_by: vec![("rucc".into(), env!("CARGO_PKG_VERSION").into())],
    };
    wasm::write(&unit.out).map_err(|error| unit_wide(format!("the object is not valid, {error}")))
}

/// The segment that the data symbol `name` points at.
fn segment_index(unit: &Unit<'_>, name: &str) -> usize {
    let symbol = unit.data[name] as usize;
    match &unit.out.symbols[symbol].kind {
        SymbolKind::Data { place: Some(place) } => place.segment as usize,
        _ => unreachable!("a defined variable has a place"),
    }
}

/// The flags of a defined symbol with this linkage.
///
/// A symbol that is not local is hidden, which is what clang does on wasm: a module that the
/// linker makes is one image, and nothing outside it looks a symbol up by its name.
fn flags(linkage: Linkage) -> u32 {
    match linkage {
        Linkage::Internal => LOCAL,
        Linkage::Weak | Linkage::LinkOnce | Linkage::Common => WEAK | HIDDEN,
        Linkage::External => HIDDEN,
    }
}

/// The state of one translation: the module that is being read, the object that is being
/// written, and the symbols that are already in the object.
pub(crate) struct Unit<'a> {
    pub(crate) ir: &'a Module,
    pub(crate) names: &'a Interner,
    pub(crate) features: Features,
    pub(crate) out: wasm::Module,
    /// The function symbols, by the name in the object.
    functions: Map<String, (u32, u32)>,
    /// The data symbols, by the name in the object.
    data: Map<String, u32>,
    stack_pointer: Option<u32>,
    table: Option<u32>,
}

impl Unit<'_> {
    /// The name of a symbol in the object.
    ///
    /// `main` is the one name that changes. The start code of wasi-libc calls `__main_void` when
    /// the program's `main` takes no arguments and `__main_argc_argv` when it takes two, and
    /// clang gives `main` the one of those two names that fits.
    pub(crate) fn name(&self, symbol: Symbol) -> String {
        let name = self.names.resolve(symbol);
        if let ("main", Some(SymbolRef::Func(id))) = (name, self.ir.lookup(symbol)) {
            let params = self.ir[id].signature().params.iter().filter(|p| !p.ty.is_mem()).count();
            return if params == 0 { "__main_void" } else { "__main_argc_argv" }.to_owned();
        }
        name.to_owned()
    }

    /// The symbol of the function `symbol` and the index of its type.
    ///
    /// A function that the module declares has the type of its declaration. A name that the
    /// module does not declare at all is a call that the lowering made up, and has the type of
    /// that call, which the caller gives with [`Unit::libcall`].
    pub(crate) fn function(&mut self, symbol: Symbol) -> Result<(u32, u32), String> {
        let name = self.name(symbol);
        if let Some(&found) = self.functions.get(&name) {
            return Ok(found);
        }
        let Some(SymbolRef::Func(id)) = self.ir.lookup(symbol) else {
            return Err(format!("`{name}` is called and is not a function of this unit"));
        };
        let func = &self.ir[id];
        let ty = self.out.intern(functype(func.signature())?);
        let flags = if func.is_declaration() { 0 } else { flags(func.linkage) };
        let index = self.out.symbol(name.clone(), SymbolKind::Function { ty, import: None }, flags);
        self.functions.insert(name, (index, ty));
        Ok((index, ty))
    }

    /// The symbol of a function of the C library or of the compiler runtime that the translation
    /// calls, such as `memcpy` or `fmod`, with the type it is called with.
    pub(crate) fn libcall(&mut self, name: &str, ty: FuncType) -> (u32, u32) {
        if let Some(&found) = self.functions.get(name) {
            return found;
        }
        let ty = self.out.intern(ty);
        let index = self.out.symbol(name, SymbolKind::Function { ty, import: None }, 0);
        self.functions.insert(name.to_owned(), (index, ty));
        (index, ty)
    }

    /// The relocation and the symbol of the address of `symbol`, which is a slot in the function
    /// table for a function and a place in memory for data.
    pub(crate) fn address(&mut self, symbol: Symbol) -> Result<(bool, u32), String> {
        match self.ir.lookup(symbol) {
            Some(SymbolRef::Func(_)) => Ok((true, self.function(symbol)?.0)),
            Some(SymbolRef::Global(_)) | None => {
                let name = self.name(symbol);
                if let Some(&found) = self.data.get(&name) {
                    return Ok((false, found));
                }
                let index = self.out.symbol(name.clone(), SymbolKind::Data { place: None }, 0);
                self.data.insert(name, index);
                Ok((false, index))
            }
            Some(SymbolRef::Alias(_)) => Err(format!(
                "`{}` is an alias, which is not written for wasm yet",
                self.name(symbol)
            )),
        }
    }

    /// The global `__stack_pointer`, which the linker defines.
    pub(crate) fn stack_pointer(&mut self) -> u32 {
        *self.stack_pointer.get_or_insert_with(|| {
            let kind = SymbolKind::Global { ty: ValType::I32, mutable: true, import: None };
            self.out.symbol("__stack_pointer", kind, 0)
        })
    }

    /// The symbol of the function table for the long form of `call_indirect`, or nothing when the
    /// target has only the short form.
    pub(crate) fn table(&mut self) -> Option<u32> {
        if !self.features.has(Feature::CallIndirectOverlong) {
            return None;
        }
        Some(*self.table.get_or_insert_with(|| {
            let kind = SymbolKind::Table { import: None };
            self.out.symbol("__indirect_function_table", kind, NO_STRIP)
        }))
    }

    /// The data segment of one defined variable.
    fn segment(&mut self, global: &rucc_ir::Global, name: &str) -> Result<Segment, String> {
        let mut bytes = Vec::new();
        let mut fixups = Vec::new();
        let mut zero = true;
        for datum in global.init.map(|list| &self.ir[list]).unwrap_or_default() {
            match *datum {
                Datum::Zero(count) => {
                    let count = usize::try_from(count).map_err(|_| "is too large".to_owned())?;
                    bytes.resize(bytes.len() + count, 0);
                }
                Datum::Bytes(range) => {
                    let run = &self.ir[range];
                    zero &= run.iter().all(|&b| b == 0);
                    bytes.extend_from_slice(run);
                }
                Datum::Scalar { ty, value } => {
                    let width = scalar_width(ty)?;
                    let bits = self.ir[value].bits().to_le_bytes();
                    zero &= bits[..width].iter().all(|&b| b == 0);
                    bytes.extend_from_slice(&bits[..width]);
                }
                Datum::Addr(reloc) => {
                    let reloc = self.ir[reloc];
                    if reloc.size != 4 {
                        return Err(format!(
                            "holds an address in {} bytes, and an address on wasm32 is 4",
                            reloc.size
                        ));
                    }
                    let (function, target) = self.address(reloc.symbol)?;
                    let addend = i32::try_from(reloc.addend)
                        .map_err(|_| "holds an address with an offset over 2 GiB".to_owned())?;
                    let kind = if function {
                        if addend != 0 {
                            return Err("holds the address of a function plus an offset".into());
                        }
                        RelocKind::TableIndexI32
                    } else {
                        RelocKind::MemoryAddrI32
                    };
                    let at = u32::try_from(bytes.len()).expect("a segment under 4 GiB");
                    fixups.push(wasm::Fixup { at, kind, target, addend });
                    bytes.extend_from_slice(&[0; 4]);
                    zero = false;
                }
                Datum::Away(_) | Datum::Apart { .. } => {
                    return Err("holds the difference of two addresses".into());
                }
            }
        }
        let size = usize::try_from(global.size).map_err(|_| "is too large".to_owned())?;
        if bytes.len() > size {
            return Err(format!("is {} bytes and the variable is {size}", bytes.len()));
        }
        bytes.resize(size, 0);
        let prefix = if global.constant {
            ".rodata."
        } else if zero {
            ".bss."
        } else {
            ".data."
        };
        let name = match global.section {
            Some(section) => self.names.resolve(section).to_owned(),
            None => format!("{prefix}{name}"),
        };
        Ok(Segment {
            name,
            align: global.align.max(1).trailing_zeros(),
            flags: if global.retain { RETAIN } else { 0 },
            bytes,
            fixups,
        })
    }
}

/// How many bytes a scalar in the initial value of a variable takes.
fn scalar_width(ty: Type) -> Result<usize, String> {
    let bits = ty.bits();
    if ty.is_vector() || bits == 0 || bits > 128 {
        return Err(format!("holds a {ty}, which is not written for wasm yet"));
    }
    Ok(bits.div_ceil(8) as usize)
}

/// The value type that holds a value of the IR type `ty`.
pub(crate) fn valtype(ty: Type) -> Result<ValType, String> {
    if ty.is_ptr() {
        return Ok(ValType::I32);
    }
    if ty.is_int() && !ty.is_vector() {
        return match ty.bits() {
            1..=32 => Ok(ValType::I32),
            64 => Ok(ValType::I64),
            _ => Err(format!("a value of type {ty} is not translated for wasm yet")),
        };
    }
    match ty.format() {
        Some(rucc_ir::Float::F32) if !ty.is_vector() => Ok(ValType::F32),
        Some(rucc_ir::Float::F64) if !ty.is_vector() => Ok(ValType::F64),
        _ => Err(format!("a value of type {ty} is not translated for wasm yet")),
    }
}

/// The type of a function with this signature. A variadic function takes the address of the
/// buffer of its extra arguments as one more `i32` after the fixed parameters.
pub(crate) fn functype(sig: &Signature) -> Result<FuncType, String> {
    let mut params = Vec::new();
    for param in sig.params.iter().filter(|p| !p.ty.is_mem()) {
        if let Abi::ByVal { .. } = param.abi {
            return Err("a parameter that travels by value in memory is not translated yet".into());
        }
        params.push(valtype(param.ty)?);
    }
    if sig.variadic {
        params.push(ValType::I32);
    }
    let mut results = Vec::new();
    for ret in sig.returns.iter().filter(|p| !p.ty.is_mem() && !p.ty.is_void()) {
        results.push(valtype(ret.ty)?);
    }
    if results.len() > 1 {
        return Err("a function that returns more than one value is not translated yet".into());
    }
    Ok(FuncType { params, results })
}
