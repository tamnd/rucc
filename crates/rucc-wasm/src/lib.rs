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
//! written where they are used. When [`Options::optimize`] is set, a value with one use in its own
//! block stays on the operand stack and has no local, where the instruction that makes it can move
//! to its use. See the `stackify` module of the selector. The graph of blocks becomes `block`, `loop` and `if` by the
//! algorithm of Ramsey, "Beyond Relooper" (ICFP 2022), and the arguments of a branch are written
//! to the parameters of its target as a parallel copy just before the branch. There is no
//! register allocation of locals and no folding of an address into the offset of a load. Those
//! come later in WA4.
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
//! `setjmp` and `longjmp` are the calls of the wasi-libc library `libsetjmp`, and each call that
//! a `longjmp` can come out of is in a `try_table` that catches the exception that `longjmp`
//! throws. [`prepare`] changes the IR for that before the translation. See the `sjlj` module.
//!
//! An alias is a second symbol, as in clang. The second name of a function has the index of the
//! function, and the second name of a variable is a data symbol at the place of the variable.
//!
//! With `-g`, [`translate_with_lines`] also gives the code offset and the source span of each run
//! of code, and the driver makes the DWARF of the unit from them with rucc-debug, as for the native
//! targets. [`describe`] puts that DWARF in the object as custom sections with their relocations,
//! which is the form that clang writes and that Wasmtime and the debuggers read. The locals are not
//! in it yet. See #3149.
//!
//! What this refuses, with a message that names the function: a vector, inline assembly with a
//! template that is not blank, a function other than `setjmp` that returns twice, unwinding to a
//! cleanup with `-fexceptions`, an `ifunc`, and the instructions of the memory safety monitor. Each
//! of these is a later step of #2864 or of the milestones after it, except the `ifunc`, which wasm
//! does not have. A graph of blocks that is not reducible is not refused: a dispatch node makes it
//! reducible. A computed `goto` is not refused either: the address of a label is a small number,
//! and the `goto` is a `br_table` on it, as in LLVM.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-wasm/0.29.5")]

mod asm;
mod emit;
mod irreducible;
mod libcall;
mod read;
mod rules;
mod select;
mod sjlj;
mod structure;
mod switch;

use std::fmt;

use rucc_base::hash::Map;
use rucc_base::{Interner, Symbol};
use rucc_diag::Span;
use rucc_ir::{
    Abi, AliasId, AliasKind, Block, Datum, Func, FuncId, Linkage, Module, Opcode, Signature,
    SymbolRef, Type,
};
use rucc_object::wasm::{
    self, EXPORTED, Fixup, FuncType, HIDDEN, Import, LOCAL, NO_STRIP, Place, Producers, RETAIN,
    RelocKind, STRINGS, Segment, SymbolKind, TLS, TLS_SEGMENT, ValType, WEAK, Written,
};
use rucc_target::wasm::{Feature, Features};

pub use libcall::bulk;
pub use sjlj::prepare;
pub use switch::switches;

/// What a translation is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The instructions that the code can use.
    pub features: Features,
    /// Whether the code is made smaller and faster where that does not change what it does, as
    /// `-O1` and above ask. Now that is the values that stay on the operand stack, which is
    /// section 8.2 of the WebAssembly notes.
    pub optimize: bool,
    /// Whether the stack pointer and the TLS base are in the context slots of the component model
    /// and not in globals, as on wasm32-wasip3, where clang 23 defines
    /// `__wasm_libcall_thread_context__`. The code then reaches them through calls to
    /// `__wasm_get_stack_pointer`, `__wasm_set_stack_pointer` and `__wasm_get_tls_base`, and a
    /// thread-local variable is an offset from the TLS base and not a place in memory. That is
    /// section 5.8 of the WebAssembly notes.
    pub thread_context: bool,
    /// Whether `sqrt` and `sqrtf` must set `errno` for a negative operand, as `-fmath-errno`
    /// asks. A call to them then stays a call and is not `f64.sqrt` or `f32.sqrt`. The other
    /// maths functions that have an instruction never set `errno`.
    pub math_errno: bool,
}

impl From<Features> for Options {
    /// The options of `-O0` with `features`.
    fn from(features: Features) -> Self {
        Options { features, optimize: false, thread_context: false, math_errno: false }
    }
}

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

/// Translate `module` into a relocatable wasm object, with the instructions and the optimization
/// that `options` asks for.
///
/// # Errors
///
/// A [`Refusal`] for the first part of the module that this back end does not translate yet.
///
/// # Panics
///
/// When the module has 2^32 data segments or more, which a module under 4 GiB cannot have.
pub fn generate(
    module: &Module,
    names: &Interner,
    options: impl Into<Options>,
) -> Result<Written, Refusal> {
    write(&translate(module, names, options)?)
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

/// The object of a file of `-S` text for wasm, in the dialect that [`assembly`] prints. For the
/// text of `rucc -S`, the bytes are the bytes that `rucc -c` writes for the same source. See the
/// `read` module.
///
/// # Errors
///
/// The number of the line, from 1, and the reason, for the first line that the reader does not
/// take, or for the last line when the object that the text describes is not valid.
pub fn assemble(text: &str) -> Result<Written, (usize, String)> {
    let object = read::read(text)?;
    wasm::write(&object)
        .map_err(|error| (text.lines().count(), format!("the object is not valid, {error}")))
}

/// The object model of `module`, which [`write()`] encodes and [`assembly`] prints. A module
/// that calls `setjmp` or `longjmp` goes through [`prepare`] first.
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
    options: impl Into<Options>,
) -> Result<wasm::Module, Refusal> {
    Ok(translate_with(module, names, options.into(), false, false)?.0)
}

/// The rows of the line table of one function of the object, which is what `-g` asks of the back
/// end. The driver makes the DWARF from them with rucc-debug, and [`describe`] puts it in the
/// object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lines {
    /// The offset of each run of code from the start of the function body, after the size of the
    /// body, and the span of the IR instruction that the run comes from. The offsets go up. An
    /// offset counts the declarations of the locals too, because the address of a function in
    /// DWARF is the start of its body, which is where `R_WASM_FUNCTION_OFFSET_I32` points.
    pub rows: Vec<(u32, Span)>,
    /// The size of the body in bytes, with the declarations of the locals and the last `end`.
    pub len: u32,
    /// The local that holds the bottom of the frame, for a function with a frame. It is the frame
    /// base of the function in DWARF, and [`Spot::Frame`] counts up from it.
    pub frame: Option<u32>,
    /// Where the declarations of the source are, in no particular order. See [`Kept`].
    pub kept: Vec<Kept>,
}

/// Where one declaration of the source is, over the code of one function or over all of it.
///
/// A declaration in the frame is there over all of the function. A declaration in a value is in
/// the local of the value from where the value is given to it to where the next value is, and only
/// at `-O0`, where each value has a local of its own and nothing else writes it. Above `-O0`, the
/// coloring gives one local to many values, and the stretch where a local holds one value is not
/// known here, so those declarations get no place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kept {
    /// Which declaration, as the opaque number that the IR function carried.
    pub decl: u32,
    /// Where it is.
    pub at: Spot,
    /// The offset of the code from the start of the body, counted as [`Lines::rows`] counts it,
    /// and the length of the code, or `None` for all of the function.
    pub over: Option<(u32, u32)>,
}

/// A place a declaration can be in a wasm function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// This many bytes above the bottom of the frame, which is the value of [`Lines::frame`].
    Frame(u32),
    /// In this local.
    Local(u32),
    /// Nowhere, because the value is this number. A constant has no local at `-O0`, because it is
    /// written where it is used.
    Constant(u64),
}

/// The object model of `module`, as [`translate`] gives it, and the rows of the line table of
/// each function, in the order of [`wasm::Module::functions`].
///
/// # Errors
///
/// A [`Refusal`] for the first part of the module that this back end does not translate yet.
///
/// # Panics
///
/// When the module has 2^32 data segments or more, which a module under 4 GiB cannot have.
pub fn translate_with_lines(
    module: &Module,
    names: &Interner,
    options: impl Into<Options>,
) -> Result<(wasm::Module, Vec<Lines>), Refusal> {
    let (object, _, lines) = translate_with(module, names, options.into(), false, true)?;
    Ok((object, lines))
}

/// Put the DWARF sections of `info`, which rucc-debug made for the object, in the object as custom
/// sections. A relocation in a section is `R_WASM_FUNCTION_OFFSET_I32` when it names a function,
/// `R_WASM_SECTION_OFFSET_I32` when it names a section of `info`, and `R_WASM_MEMORY_ADDR_I32`
/// when it names data. Each section that a relocation names gets a section symbol, after the other
/// symbols, with the name of the section after `.L` in place of its dot, which is the label of the
/// section in the `-S` text.
///
/// # Errors
///
/// A [`Refusal`] for a relocation that is not 4 bytes or that names a symbol that the object does
/// not define, which is a mistake in the translation or in rucc-debug.
///
/// # Panics
///
/// When the object has 2^32 custom sections or symbols or more, which a module under 4 GiB
/// cannot have.
pub fn describe(object: &mut wasm::Module, info: &rucc_object::Info) -> Result<(), Refusal> {
    let refused = |why: String| Refusal { function: None, why };
    let first = u32::try_from(object.customs.len()).expect("fewer than 2^32 custom sections");
    let named = |name: &str| info.chunks.iter().position(|chunk| chunk.name == name);
    // The section symbols come in the order of their sections, which is the order in which the
    // reader of the `-S` text finds their labels.
    let mut sections: Vec<Option<u32>> = vec![None; info.chunks.len()];
    for (index, chunk) in info.chunks.iter().enumerate() {
        let named = info.chunks.iter().flat_map(|chunk| &chunk.relocs);
        if named.into_iter().any(|reloc| reloc.symbol == chunk.name) {
            let custom = first + u32::try_from(index).expect("fewer than 2^32 sections");
            let label = format!(".L{}", chunk.name.trim_start_matches('.'));
            sections[index] = Some(object.symbol(label, SymbolKind::Section { custom }, LOCAL));
        }
    }
    for chunk in &info.chunks {
        let mut fixups = Vec::with_capacity(chunk.relocs.len());
        for reloc in &chunk.relocs {
            if !matches!(reloc.kind, rucc_object::Reference::Address { bytes: 4 }) {
                return Err(refused(format!(
                    "a relocation in {} is {:?} and not an address of 4 bytes",
                    chunk.name, reloc.kind
                )));
            }
            let at = u32::try_from(reloc.at)
                .map_err(|_| refused(format!("{} is larger than 4 GiB", chunk.name)))?;
            let addend = i32::try_from(reloc.addend).map_err(|_| {
                refused(format!("an addend in {} does not fit 32 bits", chunk.name))
            })?;
            let (kind, target) =
                if let Some(symbol) = named(&reloc.symbol).and_then(|i| sections[i]) {
                    (RelocKind::SectionOffsetI32, symbol)
                } else {
                    let defined = object.symbols.iter().position(|symbol| {
                        symbol.name == reloc.symbol
                            && matches!(
                                symbol.kind,
                                SymbolKind::Function { import: None, .. }
                                    | SymbolKind::Data { place: Some(_) }
                            )
                    });
                    let Some(symbol) = defined else {
                        return Err(refused(format!(
                            "{} names `{}`, which the object does not define",
                            chunk.name, reloc.symbol
                        )));
                    };
                    let kind = match object.symbols[symbol].kind {
                        SymbolKind::Function { .. } => RelocKind::FunctionOffsetI32,
                        _ => RelocKind::MemoryAddrI32,
                    };
                    (kind, u32::try_from(symbol).expect("fewer than 2^32 symbols"))
                };
            fixups.push(Fixup { at, kind, target, addend });
        }
        object.customs.push(wasm::Custom {
            name: chunk.name.clone(),
            bytes: chunk.bytes.clone(),
            fixups,
        });
    }
    Ok(())
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
pub fn tree(
    module: &Module,
    names: &Interner,
    options: impl Into<Options>,
) -> Result<String, Refusal> {
    let (object, notes, _) = translate_with(module, names, options.into(), true, false)?;
    asm::tree(&object, &notes).map_err(|(function, why)| Refusal { function: Some(function), why })
}

/// The object model of `module`, the notes of the tree form for each function when `notes` is
/// set, and the rows of the line table of each function when `lines` is set.
fn translate_with(
    module: &Module,
    names: &Interner,
    options: Options,
    notes: bool,
    lines: bool,
) -> Result<(wasm::Module, Vec<Notes>, Vec<Lines>), Refusal> {
    let unit_wide = |why: String| Refusal { function: None, why };
    let features = options.features;
    let mut unit = Unit {
        ir: module,
        names,
        features,
        optimize: options.optimize,
        thread_context: options.thread_context,
        math_errno: options.math_errno,
        out: wasm::Module::default(),
        functions: Map::default(),
        data: Map::default(),
        labels: Map::default(),
        numbers: Map::default(),
        globals: Map::default(),
        table: None,
        tag: None,
        notes: notes.then(Vec::new),
        lines: lines.then(Vec::new),
    };

    // Every definition gets its symbol first, so that a reference from a function or from data
    // finds the symbol whatever order the definitions come in.
    let mut bodies = Vec::new();
    let mut next_label = 0;
    for id in module.funcs() {
        let func = &module[id];
        if func.is_declaration() {
            continue;
        }
        let symbol = unit.function(func.name).map_err(unit_wide)?.0;
        bodies.push((id, symbol));
        let numbers = label_numbers(func, next_label);
        next_label += numbers.len() as u32;
        for (block, name) in func.named_blocks() {
            unit.labels.insert(name, numbers[&block]);
        }
        unit.numbers.insert(id, numbers);
    }
    let mut variables = Vec::new();
    for id in module.globals() {
        let global = &module[id];
        if global.is_declaration() {
            continue;
        }
        let name = names.resolve(global.name).to_owned();
        if let Some(priority) = init_priority(global, names).map_err(&unit_wide)? {
            let called = init_function(module, global)
                .ok_or_else(|| unit_wide(format!("`{name}` is not the address of a function")))?;
            let symbol = unit.function(called).map_err(&unit_wide)?.0;
            unit.out.inits.push((priority, symbol));
            continue;
        }
        let segment = u32::try_from(unit.out.segments.len()).expect("fewer than 2^32 segments");
        let size = u32::try_from(global.size)
            .map_err(|_| unit_wide(format!("`{name}` is larger than the 4 GiB of wasm32")))?;
        let place = Some(Place { segment, offset: 0, size });
        let flags = flags(global.linkage) | unit.tls(global);
        let symbol = unit.out.symbol(name.clone(), SymbolKind::Data { place }, flags);
        unit.data.insert(name.clone(), symbol);
        unit.out.segments.push(Segment::default());
        variables.push((id, name));
    }

    // A second name of a variable is a data symbol at the place of the variable, and a second
    // name of a function is a function symbol with the index of the function, which is what
    // clang writes for the `alias` attribute. The data symbols come before the initial values
    // and the code, which can refer to them by the second name.
    for id in module.aliases() {
        let alias = &module[id];
        let name = names.resolve(alias.name).to_owned();
        match unit.root(id).map_err(&unit_wide)? {
            SymbolRef::Global(global) => {
                let target = names.resolve(module[global].name);
                let place = match unit.data.get(target).map(|&s| &unit.out.symbols[s as usize].kind)
                {
                    Some(&SymbolKind::Data { place: Some(place) }) => place,
                    _ => {
                        return Err(unit_wide(format!(
                            "`{name}` is an alias of `{target}`, which is not a variable that this unit defines"
                        )));
                    }
                };
                let kind = SymbolKind::Data { place: Some(place) };
                let flags = flags(alias.linkage) | unit.tls(&module[global]);
                let symbol = unit.out.symbol(name.clone(), kind, flags);
                unit.data.insert(name, symbol);
            }
            _ => {
                unit.function(alias.name).map_err(&unit_wide)?;
            }
        }
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
    if let Some(function) = main_caller(&mut unit).map_err(&unit_wide)? {
        unit.out.functions.push(function);
        if let Some(notes) = &mut unit.notes {
            notes.push(Notes::default());
        }
        if let Some(lines) = &mut unit.lines {
            lines.push(Lines::default());
        }
    }

    // A `try_table` needs exception handling, and `libsetjmp` is built with reference types too,
    // so an object that catches a `longjmp` says both, as clang's does.
    let used = if unit.tag.is_some() {
        features.with(Feature::ExceptionHandling).with(Feature::ReferenceTypes)
    } else {
        features
    };
    unit.out.features = used.iter().map(|f| f.name().to_owned()).collect();
    // A unit with the thread context keeps its thread-local variables, and clang does not
    // disallow shared memory in its object then.
    if !features.has(Feature::Atomics) && !options.thread_context {
        unit.out.disallowed = vec!["shared-mem".into()];
    }
    unit.out.producers = Producers {
        language: None,
        processed_by: vec![("rucc".into(), env!("CARGO_PKG_VERSION").into())],
    };
    Ok((unit.out, unit.notes.unwrap_or_default(), unit.lines.unwrap_or_default()))
}

/// The function `__main_argc_argv` that the start code of wasi-libc calls, when the `main` of the
/// unit takes one parameter or three. The start code calls it with the count of the arguments
/// and the vector of the arguments, and it calls `main` with the count, then the vector, then the
/// environment from `__wasilibc_get_environ`, as many of them as `main` takes. A `main` that
/// gives nothing back gives 0. clang keeps the name `main` for such a `main` and gives it no
/// caller, so its program traps when it starts.
fn main_caller(unit: &mut Unit<'_>) -> Result<Option<wasm::Function>, String> {
    let module = unit.ir;
    let main = module.funcs().find(|&id| {
        let func = &module[id];
        !func.is_declaration() && unit.names.resolve(func.name) == "main"
    });
    let Some(main) = main else { return Ok(None) };
    let ty = functype(module[main].signature())?;
    let words = |types: &[ValType]| types.iter().all(|&t| t == ValType::I32);
    let fits = matches!(ty.params.len(), 1 | 3) && words(&ty.params) && ty.results.len() <= 1;
    if !fits || !words(&ty.results) {
        return Ok(None);
    }

    let (called, _) = unit.function(module[main].name)?;
    let start = FuncType { params: vec![ValType::I32, ValType::I32], results: vec![ValType::I32] };
    let start = unit.out.intern(start);
    let kind = SymbolKind::Function { ty: start, import: None };
    let symbol = unit.out.symbol("__main_argc_argv", kind, HIDDEN);
    unit.functions.insert("__main_argc_argv".to_owned(), (symbol, start));

    let mut code = Vec::new();
    let mut fixups = Vec::new();
    let mut call = |code: &mut Vec<u8>, target: u32| {
        code.push(0x10);
        let at = u32::try_from(code.len()).expect("a short body");
        fixups.push(Fixup { at, kind: RelocKind::FunctionIndexLeb, target, addend: 0 });
        code.extend_from_slice(&wasm::uleb_padded(0));
    };
    // local.get 0, and local.get 1 and a call for the environment when `main` takes three.
    code.extend_from_slice(&[0x20, 0x00]);
    if ty.params.len() == 3 {
        code.extend_from_slice(&[0x20, 0x01]);
        let environ = FuncType { params: Vec::new(), results: vec![ValType::I32] };
        let (environ, _) = unit.libcall("__wasilibc_get_environ", environ);
        call(&mut code, environ);
    }
    call(&mut code, called);
    if ty.results.is_empty() {
        // i32.const 0
        code.extend_from_slice(&[0x41, 0x00]);
    }
    code.push(0x0b);
    Ok(Some(wasm::Function { symbol, code, fixups, ..wasm::Function::default() }))
}

/// The priority of the constructor that `global` is the entry of, or nothing for a variable.
///
/// rucc-lower puts the entry of a constructor in `.init_array` or `.init_array.NNNNN`, as on ELF.
/// A wasm object has no such section: the constructors are a list in the `linking` section, each
/// with its priority, and `wasm-ld` calls them from `__wasm_call_ctors` in the order of the
/// priorities. A constructor with no number has the priority 65535, which is what clang gives it.
/// A destructor has no list on wasm, and rucc-lower registers it with `atexit` from a constructor,
/// so an entry in `.fini_array` is an error of the lowering.
fn init_priority(global: &rucc_ir::Global, names: &Interner) -> Result<Option<u32>, String> {
    let Some(section) = global.section.map(|s| names.resolve(s)) else { return Ok(None) };
    if section.starts_with(".fini_array") {
        return Err(format!("a destructor entry is in `{section}`, and wasm has no such list"));
    }
    match section.strip_prefix(".init_array") {
        None => Ok(None),
        Some("") => Ok(Some(65_535)),
        Some(number) => number
            .strip_prefix('.')
            .and_then(|n| n.parse().ok())
            .map(Some)
            .ok_or_else(|| format!("the constructor section `{section}` has no priority")),
    }
}

/// The function that the constructor entry `global` holds the address of.
fn init_function(module: &Module, global: &rucc_ir::Global) -> Option<Symbol> {
    match module[global.init?] {
        [Datum::Addr(reloc)] if module[reloc].addend == 0 => {
            let symbol = module[reloc].symbol;
            matches!(module.lookup(symbol), Some(SymbolRef::Func(_))).then_some(symbol)
        }
        _ => None,
    }
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

/// The flags of an undefined symbol with this linkage. A weak declaration is a weak undefined
/// symbol, which the linker resolves to a null address when nothing defines it, as clang writes it.
fn weak(linkage: Linkage) -> u32 {
    if linkage == Linkage::Weak { WEAK } else { 0 }
}

/// The state of one translation: the module that is being read, the object that is being
/// written, and the symbols that are already in the object.
pub(crate) struct Unit<'a> {
    pub(crate) ir: &'a Module,
    pub(crate) names: &'a Interner,
    pub(crate) features: Features,
    /// Whether the values with one use stay on the operand stack. See [`Options::optimize`].
    pub(crate) optimize: bool,
    /// Whether the stack pointer and the TLS base are reached through calls. See
    /// [`Options::thread_context`].
    pub(crate) thread_context: bool,
    /// Whether `sqrt` stays a call. See [`Options::math_errno`].
    pub(crate) math_errno: bool,
    pub(crate) out: wasm::Module,
    /// The function symbols, by the name in the object.
    functions: Map<String, (u32, u32)>,
    /// The data symbols, by the name in the object.
    data: Map<String, u32>,
    /// The number of each label whose address a variable holds, by the name that the function
    /// gave the label. See [`label_numbers`].
    labels: Map<Symbol, u32>,
    /// The number of each label of each function. See [`label_numbers`].
    numbers: Map<FuncId, Map<Block, u32>>,
    /// The globals that the linker defines, such as `__stack_pointer`, by name.
    globals: Map<&'static str, u32>,
    table: Option<u32>,
    /// The tag `__c_longjmp`, when a function catches a `longjmp`.
    tag: Option<u32>,
    /// The notes of the tree form, one for each function in the order of the object, when the
    /// tree form is asked for.
    pub(crate) notes: Option<Vec<Notes>>,
    /// The rows of the line table, one for each function in the order of the object, when `-g`
    /// asks for them.
    pub(crate) lines: Option<Vec<Lines>>,
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
    /// clang gives `main` the one of those two names that fits. A `main` with another number of
    /// parameters keeps its name, and [`main_caller`] gives the start code a function to call.
    pub(crate) fn name(&self, symbol: Symbol) -> String {
        let name = self.names.resolve(symbol);
        if let ("main", Some(SymbolRef::Func(id))) = (name, self.ir.lookup(symbol)) {
            let params = self.ir[id].signature().params.iter().filter(|p| !p.ty.is_mem()).count();
            match params {
                0 => return "__main_void".to_owned(),
                2 => return "__main_argc_argv".to_owned(),
                _ => {}
            }
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
        let (id, alias) = match self.ir.lookup(symbol) {
            Some(SymbolRef::Func(id)) => (id, None),
            Some(SymbolRef::Alias(alias)) => match self.root(alias)? {
                SymbolRef::Func(id) => (id, Some(alias)),
                _ => return Err(format!("`{name}` is called and is a second name of a variable")),
            },
            _ => return Err(format!("`{name}` is called and is not a function of this unit")),
        };
        let func = &self.ir[id];
        let ty = self.out.intern(functype(func.signature())?);
        if let Some(alias) = alias {
            // The second name of a function has the type and the index of the function, and the
            // flags of its own linkage.
            if func.is_declaration() {
                let target = self.names.resolve(func.name);
                return Err(format!(
                    "`{name}` is an alias of `{target}`, which is not a function that this unit defines"
                ));
            }
            let target = self.function(func.name)?.0;
            let kind = SymbolKind::Function { ty, import: None };
            let index = self.out.symbol(name.clone(), kind, flags(self.ir[alias].linkage));
            self.out.aliases.push((index, target));
            self.functions.insert(name, (index, ty));
            return Ok((index, ty));
        }
        // A definition that `export_name` names is exported and kept, and a declaration that
        // `import_module` or `import_name` names is imported from there, which are the flags and
        // the import that clang writes.
        let wasm = func.wasm;
        let (flags, import) = if func.is_declaration() {
            let import = (wasm.module.is_some() || wasm.field.is_some()).then(|| Import {
                module: wasm.module.map_or("env", |m| self.names.resolve(m)).to_owned(),
                field: wasm.field.map_or(name.as_str(), |f| self.names.resolve(f)).to_owned(),
            });
            (weak(func.linkage), import)
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
        if self.is_function(symbol)? {
            // A slot in the table is an address only when the module has the table. On a target
            // with the long form of `call_indirect`, the object writer of LLVM imports
            // `__indirect_function_table` and keeps it for each relocation to a slot, also in a
            // module that has no `call_indirect`. rucc does the same, so that its object and the
            // object that clang makes from its `-S` text have the same symbols.
            self.table();
            return Ok((true, self.function(symbol)?.0));
        }
        let name = self.name(symbol);
        if let Some(&found) = self.data.get(&name) {
            return Ok((false, found));
        }
        let linkage = match self.ir.lookup(symbol) {
            Some(SymbolRef::Global(id)) => self.ir[id].linkage,
            _ => Linkage::External,
        };
        let flags = weak(linkage) | if self.is_tls(symbol) { TLS } else { 0 };
        let index = self.out.symbol(name.clone(), SymbolKind::Data { place: None }, flags);
        self.data.insert(name, index);
        Ok((false, index))
    }

    /// [`TLS`] for a thread-local variable when the unit has the thread context, and nothing
    /// otherwise. Without the thread context there is one thread, and a thread-local variable is
    /// a variable like any other, which is what clang makes of it when it strips the thread-local
    /// variables of a unit with no atomics.
    pub(crate) fn tls(&self, global: &rucc_ir::Global) -> u32 {
        if self.thread_context && global.tls.is_some() { TLS } else { 0 }
    }

    /// Whether the address of `symbol` is an offset from the TLS base, which is so for a
    /// thread-local variable in a unit with the thread context.
    pub(crate) fn is_tls(&self, symbol: Symbol) -> bool {
        match self.ir.lookup(symbol) {
            Some(SymbolRef::Global(id)) => self.tls(&self.ir[id]) != 0,
            _ => false,
        }
    }

    /// Whether the address of `symbol` is a slot in the function table and not a place in memory.
    pub(crate) fn is_function(&self, symbol: Symbol) -> Result<bool, String> {
        Ok(match self.ir.lookup(symbol) {
            Some(SymbolRef::Func(_)) => true,
            Some(SymbolRef::Alias(alias)) => matches!(self.root(alias)?, SymbolRef::Func(_)),
            Some(SymbolRef::Global(_)) | None => false,
        })
    }

    /// The function or the variable that the alias `id` names, past any alias that it names.
    /// wasm has no `ifunc`, because a module cannot choose its code when it starts, and clang
    /// refuses it on wasm too.
    fn root(&self, mut id: AliasId) -> Result<SymbolRef, String> {
        let name = self.names.resolve(self.ir[id].name);
        for _ in self.ir.aliases() {
            let alias = &self.ir[id];
            if alias.kind == AliasKind::IFunc {
                return Err(format!("`{name}` is an ifunc, which wasm does not have"));
            }
            match self.ir.lookup(alias.target) {
                Some(SymbolRef::Alias(next)) => id = next,
                Some(found) => return Ok(found),
                None => {
                    let target = self.names.resolve(alias.target);
                    return Err(format!(
                        "`{name}` is an alias of `{target}`, which this unit does not have"
                    ));
                }
            }
        }
        Err(format!("`{name}` is an alias of itself"))
    }

    /// The global `__stack_pointer`, which the linker defines.
    pub(crate) fn stack_pointer(&mut self) -> u32 {
        self.linker_global("__stack_pointer", ValType::I32, true)
    }

    /// The library function that gives the stack pointer or the TLS base, or that
    /// sets the stack pointer, when the unit has the thread context. wasm-ld writes these from
    /// `context.get` and `context.set` when it links with `--cooperative-threading`.
    pub(crate) fn context_call(&mut self, name: &str) -> u32 {
        let ty = if name.starts_with("__wasm_set_") {
            FuncType { params: vec![ValType::I32], results: Vec::new() }
        } else {
            FuncType { params: Vec::new(), results: vec![ValType::I32] }
        };
        self.libcall(name, ty).0
    }

    /// The symbol of a global that the linker defines, such as `__stack_pointer` and `__tls_base`.
    pub(crate) fn linker_global(&mut self, name: &'static str, ty: ValType, mutable: bool) -> u32 {
        if let Some(&symbol) = self.globals.get(name) {
            return symbol;
        }
        let kind = SymbolKind::Global { ty, mutable, import: None };
        let symbol = self.out.symbol(name, kind, 0);
        self.globals.insert(name, symbol);
        symbol
    }

    /// The tag `__c_longjmp` of the exception that the `longjmp` of `libsetjmp` throws, which
    /// carries the address of the buffer and the value. `libsetjmp` defines it.
    pub(crate) fn longjmp_tag(&mut self) -> u32 {
        if let Some(tag) = self.tag {
            return tag;
        }
        let ty = self.out.intern(FuncType { params: vec![ValType::I32], results: Vec::new() });
        let kind = SymbolKind::Tag { ty, import: None };
        *self.tag.insert(self.out.symbol("__c_longjmp", kind, 0))
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
                    fixups.push(Fixup { at, kind, target, addend });
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
        let tls = self.tls(global) != 0;
        let prefix = match (tls, global.constant, zero) {
            (true, _, true) => ".tbss.",
            (true, _, false) => ".tdata.",
            (false, true, _) => ".rodata.",
            (false, false, true) => ".bss.",
            (false, false, false) => ".data.",
        };
        let name = match global.section {
            Some(section) => self.names.resolve(section).to_owned(),
            None => format!("{prefix}{name}"),
        };
        let align = global.align.max(1).trailing_zeros();
        let mut flags = if global.retain { RETAIN } else { 0 };
        if tls {
            flags |= TLS_SEGMENT;
        }
        if mergeable(global, align, &bytes, &fixups) {
            flags |= STRINGS;
        }
        Ok(Segment { name, align, flags, bytes, fixups })
    }
}

/// Whether wasm-ld may merge the segment of `global` with each other segment that holds the same
/// string, which is what the `STRINGS` flag tells it.
///
/// The rule is the native one, which `rucc_asm` applies for `.rodata.str1.`: a string literal with
/// no section of its own whose bytes are one string, so the first zero is the last byte. wasm-ld
/// reads a merged segment up to each zero, so a literal with a zero inside it would become two
/// strings. wasm-ld also merges only a segment aligned to one byte (`shouldMerge` in
/// `lld/wasm/InputFiles.cpp`), so a literal the program aligned to more keeps its own segment.
fn mergeable(global: &rucc_ir::Global, align: u32, bytes: &[u8], fixups: &[Fixup]) -> bool {
    global.literal
        && global.constant
        && global.section.is_none()
        && align == 0
        && fixups.is_empty()
        && bytes.iter().position(|&byte| byte == 0) == Some(bytes.len().wrapping_sub(1))
}

/// The number that stands for the address of each label of `func`, which is what `&&label` is
/// on wasm.
///
/// A wasm function has no addresses inside it, so a computed `goto` cannot jump to an address.
/// clang does what LLVM's `IndirectBrExpandPass` does: each label whose address is taken is a
/// small number, and `goto *p` is a `br_table` on that number. rucc does the same. A label is a
/// block that the function names for a variable, a block that a `block_addr` takes, or a target
/// of an `indirect_br`. The labels are numbered in block order from one more than `before`, which
/// is the count of the labels of the functions before this one in the unit. So no label has the
/// address zero, the table of a `goto` is short, and two labels of one unit have two addresses, as
/// on the native rows. A copy of an inline function keeps its own labels, and gcc's torture test
/// `990208-1.c` compares the addresses of two copies. The `goto` subtracts the smallest number of
/// its targets before the `br_table`, so the numbers can start anywhere. The unit asks this
/// function once for each function, and a variable that holds the address and the code that takes
/// it both read that answer, so they agree.
pub(crate) fn label_numbers(func: &Func, before: u32) -> Map<Block, u32> {
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
    labels.into_iter().zip(before + 1..).collect()
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
