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
//! `wasm-ld` against the wasi-sdk sysroot and runs under Wasmtime. The `-S` text is printed from
//! the same object, in the assembly dialect of LLVM, and clang assembles it back into an object
//! with the same code, data and symbols. See the `asm` module. [`tree()`] prints the same code with
//! the names of the IR, for a person who reads the structure that the translation chose.
//!
//! The translation is direct. Each value of the IR gets a local of its own, each instruction
//! reads its operands with `local.get` and writes its result with `local.set`, and constants are
//! written where they are used. The graph of blocks becomes `block`, `loop` and `if` by the
//! algorithm of Ramsey, "Beyond Relooper" (ICFP 2022), and the arguments of a branch are written
//! to the parameters of its target as a parallel copy just before the branch. There is no
//! register allocation of locals, no folding of an address into the offset of a load, and no use
//! of the operand stack across instructions. Those come in WA4.
//!
//! The integer arithmetic, the shifts, the divisions, the comparisons, `select`, the changes of
//! width and the float arithmetic are selected by the rules of `rules/wasm32.rules`, each of
//! which `rucc-verify` proves against the model of the wasm instructions in
//! `rules/wasm32.model`. The other instructions are written by the code of `select.rs`. See the
//! `rules` module.
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
//! An `i128` and a `long double` are each held in two `i64` locals, the low half first, and they
//! travel as two `i64` parameters, as clang does with them. Addition, subtraction, comparison and
//! the bit operations on an `i128` are written in place, and the other operations on an `i128` and
//! every operation on a `long double` are calls to the compiler runtime, whose answer comes back
//! through a buffer in the frame. That is section 8.6 of the WebAssembly notes.
//!
//! What this refuses, with a message that names the function: a vector, `setjmp`, inline
//! assembly, an alias, and the instructions of the memory safety monitor. Each of these is a later
//! step of #2864 or of the milestones after it. A graph of blocks that is not reducible is not
//! refused: a dispatch node makes it reducible. A computed `goto` is not refused either: the
//! address of a label is a small number, and the `goto` is a `br_table` on it, as in LLVM.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-wasm/0.22.2")]

mod asm;
mod emit;
mod irreducible;
mod rules;
mod select;
mod structure;

use std::fmt;

use rucc_base::hash::Map;
use rucc_base::{Interner, Symbol};
use rucc_ir::{Abi, Block, Datum, Func, Linkage, Module, Opcode, Signature, SymbolRef, Type};
use rucc_object::wasm::{
    self, EXPORTED, FuncType, HIDDEN, Import, LOCAL, NO_STRIP, Place, Producers, RETAIN, RelocKind,
    Segment, SymbolKind, ValType, WEAK, Written,
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
    write(&translate(module, names, features)?)
}

/// The bytes of the object that [`translate`] gave.
///
/// # Errors
///
/// A [`Refusal`] when the object names a symbol or a type that is not in it, which is a mistake in
/// the translation.
pub fn write(object: &wasm::Module) -> Result<Written, Refusal> {
    wasm::write(object).map_err(|error| Refusal {
        function: None,
        why: format!("the object is not valid, {error}"),
    })
}

/// The `-S` text of the object that [`translate`] gave, in the assembly dialect of LLVM. The text
/// comes from the same object that [`write()`] encodes. See the `asm` module.
///
/// # Errors
///
/// A [`Refusal`] that names the function whose body the printer cannot decode, which is a mistake
/// in the translation or in the printer.
pub fn assembly(object: &wasm::Module) -> Result<String, Refusal> {
    asm::print(object).map_err(|(function, why)| Refusal { function: Some(function), why })
}

/// The object model of `module`, which [`write()`] encodes and [`assembly`] prints.
///
/// # Errors
///
/// A [`Refusal`] for the first part of the module that this back end does not translate yet.
///
/// # Panics
///
/// When the module has 2^32 data segments or more, which a module under 4 GiB cannot have.
pub fn translate(
    module: &Module,
    names: &Interner,
    features: Features,
) -> Result<wasm::Module, Refusal> {
    Ok(translate_with(module, names, features, false)?.0)
}

/// The tree form of `module`, which is `--emit=wasm-tree`: the code of each function as the
/// structuring stage made it, with each construct indented and marked with the IR block that it
/// comes from, and each local named by the IR value that it holds, as `--emit=ir` names it. The
/// code is the code of the object, decoded from its bytes, so the form shows what the object
/// does and not a plan of it.
///
/// # Errors
///
/// A [`Refusal`] for the first part of the module that this back end does not translate yet, or
/// for a body that the printer cannot decode.
pub fn tree(module: &Module, names: &Interner, features: Features) -> Result<String, Refusal> {
    let (object, notes) = translate_with(module, names, features, true)?;
    asm::tree(&object, &notes).map_err(|(function, why)| Refusal { function: Some(function), why })
}

/// The object model of `module`, and the notes of the tree form for each function when `notes`
/// is set.
fn translate_with(
    module: &Module,
    names: &Interner,
    features: Features,
    notes: bool,
) -> Result<(wasm::Module, Vec<Notes>), Refusal> {
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
        labels: Map::default(),
        stack_pointer: None,
        table: None,
        notes: notes.then(Vec::new),
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
        let numbers = label_numbers(func);
        for (block, name) in func.named_blocks() {
            unit.labels.insert(name, numbers[&block]);
        }
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
    Ok((unit.out, unit.notes.unwrap_or_default()))
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
    /// The number of each label whose address a variable holds, by the name that the function
    /// gave the label. See [`label_numbers`].
    labels: Map<Symbol, u32>,
    stack_pointer: Option<u32>,
    table: Option<u32>,
    /// The notes of the tree form, one for each function in the order of the object, when the
    /// tree form is asked for.
    pub(crate) notes: Option<Vec<Notes>>,
}

/// What the tree form says about one function beside its code. See [`tree()`].
#[derive(Default)]
pub(crate) struct Notes {
    /// A note and the offset in the body of the instruction that it is written beside.
    pub(crate) marks: Vec<(usize, String)>,
    /// The name of each local that holds an IR value, which is the name that `--emit=ir` gives
    /// the value, and of each local that the translation adds for itself.
    pub(crate) locals: Map<u32, String>,
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
        // A definition that `export_name` names is exported and kept, and a declaration that
        // `import_module` or `import_name` names is imported from there, which are the flags and
        // the import that clang writes.
        let wasm = func.wasm;
        let (flags, import) = if func.is_declaration() {
            let import = (wasm.module.is_some() || wasm.field.is_some()).then(|| Import {
                module: wasm.module.map_or("env", |m| self.names.resolve(m)).to_owned(),
                field: wasm.field.map_or(name.as_str(), |f| self.names.resolve(f)).to_owned(),
            });
            (0, import)
        } else if wasm.export.is_some() {
            (flags(func.linkage) | EXPORTED | NO_STRIP, None)
        } else {
            (flags(func.linkage), None)
        };
        let index = self.out.symbol(name.clone(), SymbolKind::Function { ty, import }, flags);
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
            Some(SymbolRef::Func(_)) => {
                // A slot in the table is an address only when the module has the table. On a
                // target with the long form of `call_indirect`, the object writer of LLVM imports
                // `__indirect_function_table` and keeps it for each relocation to a slot, also in
                // a module that has no `call_indirect`. rucc does the same, so that its object and
                // the object that clang makes from its `-S` text have the same symbols.
                self.table();
                Ok((true, self.function(symbol)?.0))
            }
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
                Datum::Addr(reloc) if self.labels.contains_key(&self.ir[reloc].symbol) => {
                    let reloc = self.ir[reloc];
                    let number = i64::from(self.labels[&reloc.symbol]) + reloc.addend;
                    zero &= number == 0;
                    bytes.extend_from_slice(&label_bytes(number, reloc.size)?);
                }
                Datum::Apart { to, from }
                    if self.labels.contains_key(&self.ir[to].symbol)
                        && self.labels.contains_key(&from) =>
                {
                    let to = self.ir[to];
                    let number = i64::from(self.labels[&to.symbol]) + to.addend
                        - i64::from(self.labels[&from]);
                    zero &= number == 0;
                    bytes.extend_from_slice(&label_bytes(number, to.size)?);
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

/// The number that stands for the address of each label of `func`, which is what `&&label` is
/// on wasm.
///
/// A wasm function has no addresses inside it, so a computed `goto` cannot jump to an address.
/// clang does what LLVM's `IndirectBrExpandPass` does: each label whose address is taken is a
/// small number, and `goto *p` is a `br_table` on that number. rucc does the same. A label is a
/// block that the function names for a variable, a block that a `block_addr` takes, or a target
/// of an `indirect_br`. The labels are numbered from one in block order, so that no label has the
/// address zero and the table of a `goto` is short. A label belongs to one function and its number
/// is only compared inside that function, so two functions use the same numbers. A variable that
/// holds the address and the code that takes it both ask this function, so they agree.
pub(crate) fn label_numbers(func: &Func) -> Map<Block, u32> {
    let mut labels: Vec<Block> = func.named_blocks().map(|(block, _)| block).collect();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if matches!(func[inst].opcode, Opcode::BlockAddr | Opcode::IndirectBr) {
                labels.extend(func.successors(inst).map(|call| call.block));
            }
        }
    }
    labels.sort_by_key(|block| block.raw());
    labels.dedup();
    labels.into_iter().zip(1..).collect()
}

/// The bytes of a label number, or of the distance between two labels, in a variable.
fn label_bytes(number: i64, size: u32) -> Result<Vec<u8>, String> {
    let fits = match size {
        1 => i8::try_from(number).is_ok(),
        2 => i16::try_from(number).is_ok(),
        4 => i32::try_from(number).is_ok(),
        8 => true,
        _ => false,
    };
    if !fits {
        return Err(format!("holds the address of a label in {size} bytes, where it does not fit"));
    }
    Ok(number.to_le_bytes()[..size as usize].to_vec())
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

/// Whether a value of the IR type `ty` is held in two `i64`, the low half first. That is an
/// `i128` and a `long double`, which is IEEE quad on wasm32, and it is what clang does with them.
pub(crate) fn is_pair(ty: Type) -> bool {
    !ty.is_vector()
        && !ty.is_ptr()
        && (ty.is_int() && ty.bits() == 128 || ty.format() == Some(rucc_ir::Float::F128))
}

/// The type of a function with this signature. A variadic function takes the address of the
/// buffer of its extra arguments as one more `i32` after the fixed parameters.
///
/// A pair is two `i64` parameters, and a function that returns a pair returns nothing and takes
/// the address to write the pair to as an `i32` before all the other parameters. That is the
/// return through a hidden pointer of the Basic C ABI, and clang does it for an `i128` and a
/// `long double` too.
pub(crate) fn functype(sig: &Signature) -> Result<FuncType, String> {
    let mut params = Vec::new();
    for param in sig.params.iter().filter(|p| !p.ty.is_mem()) {
        if let Abi::ByVal { .. } = param.abi {
            return Err("a parameter that travels by value in memory is not translated yet".into());
        }
        if is_pair(param.ty) {
            params.extend([ValType::I64, ValType::I64]);
        } else {
            params.push(valtype(param.ty)?);
        }
    }
    if sig.variadic {
        params.push(ValType::I32);
    }
    let returns: Vec<Type> =
        sig.returns.iter().filter(|p| !p.ty.is_mem() && !p.ty.is_void()).map(|p| p.ty).collect();
    if returns.len() > 1 {
        return Err("a function that returns more than one value is not translated yet".into());
    }
    let mut results = Vec::new();
    for ty in returns {
        if is_pair(ty) {
            params.insert(0, ValType::I32);
        } else {
            results.push(valtype(ty)?);
        }
    }
    Ok(FuncType { params, results })
}
