//! Step 2: read the inputs and resolve every symbol to the one place that defines it.
//!
//! The archive rule is LLD's, which is GNU ld's without the dependence on order. A member of an
//! archive is loaded when a strong reference to a name it defines is seen, before or after the
//! archive on the command line. A weak reference does not load a member. A strong definition
//! takes the place of a weak one, two strong definitions of one name are an error with both
//! objects in it, and a function must have the same type at its definition and at every
//! reference.
//!
//! What is left undefined at the end is one of four things. A name the linker defines, such as
//! `__stack_pointer` or `__heap_base`, is synthesized. A function with an explicit import name or
//! module, such as `fd_write` of `wasi_snapshot_preview1`, is an import of the module. A weak
//! reference is null, and a call to it traps. Anything else is an undefined symbol and an error.

use std::collections::{HashMap, VecDeque};

use crate::object::{EXPLICIT_NAME, Kind, Object};
use crate::{Error, Input, Options, archive};

/// A name the linker defines when an input refers to it and no input defines it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Synth {
    /// The stack pointer, a mutable `i32` global.
    StackPointer,
    /// The base of the data, a constant `i32` global that is zero in a static module.
    MemoryBase,
    /// The base of the table, a constant `i32` global that is one in a static module.
    TableBase,
    /// The function table.
    Table,
    /// The function that calls each constructor.
    CallCtors,
    /// Data addresses: the end of the data, the start of the heap, and the end of the memory.
    DataEnd,
    HeapBase,
    HeapEnd,
    /// The start of the data, which is where `__dso_handle` is too.
    GlobalBase,
    StackLow,
    StackHigh,
    FirstPageEnd,
}

impl Synth {
    fn of(name: &str, kind: Kind) -> Option<Synth> {
        let synth = match name {
            "__stack_pointer" => Synth::StackPointer,
            "__memory_base" => Synth::MemoryBase,
            "__table_base" => Synth::TableBase,
            "__indirect_function_table" => Synth::Table,
            "__wasm_call_ctors" => Synth::CallCtors,
            "__data_end" => Synth::DataEnd,
            "__heap_base" => Synth::HeapBase,
            "__heap_end" => Synth::HeapEnd,
            "__global_base" | "__dso_handle" => Synth::GlobalBase,
            "__stack_low" => Synth::StackLow,
            "__stack_high" => Synth::StackHigh,
            "__wasm_first_page_end" => Synth::FirstPageEnd,
            _ => return None,
        };
        let expected = match synth {
            Synth::StackPointer | Synth::MemoryBase | Synth::TableBase => Kind::Global,
            Synth::Table => Kind::Table,
            Synth::CallCtors => Kind::Function,
            _ => Kind::Data,
        };
        (kind == expected).then_some(synth)
    }
}

/// Where a symbol of an object goes in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Where {
    /// A defined function: the object and the index among its defined functions.
    Func(usize, u32),
    /// Defined data: the object, the segment and the offset in it.
    Data(usize, u32, u32),
    /// A defined global: the object and the index among its defined globals.
    Global(usize, u32),
    /// A defined tag: the object and the index among its defined tags.
    Tag(usize, u32),
    /// A function that the module imports, as an index into [`World::imports`].
    Import(u32),
    /// A name the linker defines.
    Synth(Synth),
    /// A weak reference that nothing defines: address zero, and a call that traps.
    Null,
    /// A section symbol, which only debug sections refer to.
    Nothing,
}

/// A function the output imports: the module, the field, and its type as encoded.
#[derive(Debug, Clone)]
pub(crate) struct Imported<'a> {
    pub(crate) name: &'a str,
    pub(crate) module: &'a str,
    pub(crate) field: &'a str,
    pub(crate) ty: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
enum State {
    /// Referenced and not defined.
    Undefined,
    /// Defined by a member of an archive that is not loaded: the archive and the member.
    Lazy(usize, usize),
    /// Defined: the object, the symbol, and whether the definition is weak.
    Defined(usize, usize, bool),
}

#[derive(Debug, Clone)]
struct Entry {
    kind: Kind,
    state: State,
    /// The first reference: the object and the symbol.
    first: Option<(usize, usize)>,
    /// Whether some reference is strong.
    strong: bool,
}

/// Every object that the link loaded, and where each of their symbols goes.
#[derive(Debug)]
pub(crate) struct World<'a> {
    pub(crate) files: Vec<Object<'a>>,
    /// For each object, for each symbol, where it goes.
    pub(crate) targets: Vec<Vec<Where>>,
    pub(crate) imports: Vec<Imported<'a>>,
    /// The function an option names as the entry, when there is one.
    pub(crate) entry: Option<Where>,
    /// The names `--export` gives, and where each goes.
    pub(crate) exports: Vec<(String, Where)>,
}

struct Loader<'a> {
    files: Vec<Object<'a>>,
    archives: Vec<Vec<Option<Object<'a>>>>,
    table: HashMap<&'a str, Entry>,
    queue: VecDeque<(usize, usize)>,
    /// The COMDAT groups seen, so that a second copy of a group is dropped.
    comdats: HashMap<&'a str, usize>,
}

impl<'a> World<'a> {
    /// Loads the inputs and resolves every symbol.
    pub(crate) fn load(options: &Options, inputs: &[Input<'a>]) -> Result<Self, Error> {
        let mut loader = Loader {
            files: Vec::new(),
            archives: Vec::new(),
            table: HashMap::new(),
            queue: VecDeque::new(),
            comdats: HashMap::new(),
        };
        for input in inputs {
            if archive::is_archive(input.bytes) {
                let members = archive::members(input.bytes).map_err(|e| e.within(input.name))?;
                let index = loader.archives.len();
                let mut objects = Vec::with_capacity(members.len());
                for member in members {
                    let name = format!("{}({})", input.name, member.name);
                    objects.push(Some(Object::parse(&name, member.bytes)?));
                }
                loader.archives.push(objects);
                for member in 0..loader.archives[index].len() {
                    loader.lazy(index, member)?;
                }
            } else {
                loader.object(Object::parse(input.name, input.bytes)?)?;
            }
            loader.drain()?;
        }
        let roots = options.entry.iter().chain(&options.exports).chain(&options.undefined);
        for name in roots {
            loader.root(name);
        }
        loader.drain()?;
        loader.finish(options)
    }
}

impl<'a> Loader<'a> {
    /// Notes the names a member of an archive defines.
    fn lazy(&mut self, archive: usize, member: usize) -> Result<(), Error> {
        let object = self.archives[archive][member].as_ref().expect("a member not yet loaded");
        let mut fetch = false;
        for symbol in &object.symbols {
            if symbol.is_undefined() || symbol.is_local() || symbol.kind == Kind::Section {
                continue;
            }
            match self.table.get_mut(symbol.name) {
                None => {
                    let entry = Entry {
                        kind: symbol.kind,
                        state: State::Lazy(archive, member),
                        first: None,
                        strong: false,
                    };
                    self.table.insert(symbol.name, entry);
                }
                Some(entry) => {
                    if let State::Undefined = entry.state {
                        entry.state = State::Lazy(archive, member);
                        fetch |= entry.strong;
                    }
                }
            }
        }
        if fetch {
            self.queue.push_back((archive, member));
        }
        Ok(())
    }

    /// Loads the members that strong references asked for, and the ones those ask for.
    fn drain(&mut self) -> Result<(), Error> {
        while let Some((archive, member)) = self.queue.pop_front() {
            if let Some(object) = self.archives[archive][member].take() {
                self.object(object)?;
            }
        }
        Ok(())
    }

    /// A name an option makes a strong reference to.
    fn root(&mut self, name: &str) {
        if let Some(entry) = self.table.get_mut(name) {
            entry.strong = true;
            if let State::Lazy(archive, member) = entry.state {
                self.queue.push_back((archive, member));
            }
        }
    }

    fn object(&mut self, object: Object<'a>) -> Result<(), Error> {
        let file = self.files.len();
        let mut dropped = vec![false; object.symbols.len()];
        let mut funcs = vec![false; object.funcs.len()];
        let mut segments = vec![false; object.data.len()];
        for comdat in &object.comdats {
            if self.comdats.contains_key(comdat.name) {
                for &(kind, index) in &comdat.members {
                    let slot = match kind {
                        0 => segments.get_mut(index as usize),
                        1 => funcs.get_mut(index as usize),
                        _ => None,
                    };
                    if let Some(slot) = slot {
                        *slot = true;
                    }
                }
            } else {
                self.comdats.insert(comdat.name, file);
            }
        }
        let imported = object.func_imports.len() as u32;
        for (i, symbol) in object.symbols.iter().enumerate() {
            if symbol.is_undefined() {
                continue;
            }
            dropped[i] = match symbol.kind {
                Kind::Function => funcs[(symbol.index - imported) as usize],
                Kind::Data => segments[symbol.index as usize],
                _ => false,
            };
        }
        for (i, symbol) in object.symbols.iter().enumerate() {
            if symbol.is_local() || symbol.kind == Kind::Section {
                continue;
            }
            let entry = self.table.entry(symbol.name).or_insert(Entry {
                kind: symbol.kind,
                state: State::Undefined,
                first: None,
                strong: false,
            });
            if entry.kind != symbol.kind {
                let other = match entry.state {
                    State::Defined(f, _, _) => self.files[f].name.clone(),
                    _ => entry.first.map_or_else(String::new, |(f, _)| self.files[f].name.clone()),
                };
                return Err(Error::new(format!(
                    "{} is {:?} in {} and {:?} in {other}",
                    symbol.name, symbol.kind, object.name, entry.kind
                )));
            }
            if symbol.is_undefined() || dropped[i] {
                entry.first = entry.first.or(Some((file, i)));
                if !symbol.is_weak() {
                    entry.strong = true;
                    if let State::Lazy(archive, member) = entry.state {
                        self.queue.push_back((archive, member));
                    }
                }
                continue;
            }
            match entry.state {
                State::Undefined | State::Lazy(..) => {
                    entry.state = State::Defined(file, i, symbol.is_weak());
                }
                State::Defined(_, _, true) if !symbol.is_weak() => {
                    entry.state = State::Defined(file, i, false);
                }
                State::Defined(other, _, false) if !symbol.is_weak() => {
                    return Err(Error::new(format!(
                        "{} is defined in {} and in {}",
                        symbol.name, self.files[other].name, object.name
                    )));
                }
                State::Defined(..) => {}
            }
        }
        self.files.push(object);
        Ok(())
    }

    fn finish(self, options: &Options) -> Result<World<'a>, Error> {
        let Loader { files, table, .. } = self;
        let mut imports: Vec<Imported<'a>> = Vec::new();
        let mut import_of: HashMap<&'a str, u32> = HashMap::new();
        let mut undefined = Vec::new();
        // Where each global name goes.
        let mut names: HashMap<&'a str, Where> = HashMap::new();
        // In the order of the first reference, which is the order of the imports in the module.
        // The order of a hash table changes from one run to the next, and the module must not.
        let mut entries: Vec<(&'a str, &Entry)> = table.iter().map(|(&n, e)| (n, e)).collect();
        entries.sort_unstable_by_key(|&(name, entry)| (entry.first, name));
        for (name, entry) in entries {
            let place = match entry.state {
                State::Defined(file, symbol, _) => own(&files[file], file, symbol),
                State::Undefined | State::Lazy(..) => {
                    let Some((file, symbol)) = entry.first else { continue };
                    if let Some(synth) = Synth::of(name, entry.kind) {
                        Where::Synth(synth)
                    } else if let Some(import) = import(&files[file], symbol) {
                        let index = *import_of.entry(name).or_insert_with(|| {
                            imports.push(import);
                            imports.len() as u32 - 1
                        });
                        Where::Import(index)
                    } else if !entry.strong {
                        Where::Null
                    } else {
                        undefined.push((name, files[file].name.as_str()));
                        continue;
                    }
                }
            };
            names.insert(name, place);
        }
        // The linker defines these whether or not an input refers to them.
        names.entry("__stack_pointer").or_insert(Where::Synth(Synth::StackPointer));
        names.entry("__wasm_call_ctors").or_insert(Where::Synth(Synth::CallCtors));
        if !undefined.is_empty() {
            undefined.sort_unstable();
            let lines: Vec<String> = undefined
                .iter()
                .map(|(name, file)| format!("undefined symbol: {name} (referenced by {file})"))
                .collect();
            return Err(Error::new(lines.join("\n")));
        }
        let mut targets = Vec::with_capacity(files.len());
        for (file, object) in files.iter().enumerate() {
            let mut places = Vec::with_capacity(object.symbols.len());
            for (i, symbol) in object.symbols.iter().enumerate() {
                let place = if symbol.kind == Kind::Section {
                    Where::Nothing
                } else if symbol.is_local() {
                    own(object, file, i)
                } else {
                    names.get(symbol.name).copied().unwrap_or(Where::Null)
                };
                places.push(place);
            }
            targets.push(places);
        }
        let mut world = World { files, targets, imports, entry: None, exports: Vec::new() };
        if let Some(name) = &options.entry {
            match names.get(name.as_str()) {
                Some(&place @ Where::Func(..)) => world.entry = Some(place),
                _ => return Err(Error::new(format!("the entry symbol {name} is not defined"))),
            }
        }
        for name in &options.exports {
            match names.get(name.as_str()) {
                Some(&place @ (Where::Func(..) | Where::Data(..))) => {
                    world.exports.push((name.clone(), place));
                }
                _ => return Err(Error::new(format!("the export {name} is not defined"))),
            }
        }
        world.check_signatures()?;
        Ok(world)
    }
}

/// Where a symbol goes when its own object defines it.
fn own(object: &Object<'_>, file: usize, symbol: usize) -> Where {
    let symbol = &object.symbols[symbol];
    match symbol.kind {
        Kind::Function => Where::Func(file, symbol.index - object.func_imports.len() as u32),
        Kind::Data => Where::Data(file, symbol.index, symbol.offset),
        Kind::Global => Where::Global(file, symbol.index - object.global_imports.len() as u32),
        Kind::Tag => Where::Tag(file, symbol.index - object.tag_imports.len() as u32),
        Kind::Table | Kind::Section => Where::Nothing,
    }
}

/// The import an undefined function becomes, when it has an explicit import name or a module
/// other than `env`.
fn import<'a>(object: &Object<'a>, symbol: usize) -> Option<Imported<'a>> {
    let symbol = &object.symbols[symbol];
    if symbol.kind != Kind::Function {
        return None;
    }
    let import = &object.func_imports[symbol.index as usize];
    let explicit = symbol.flags & EXPLICIT_NAME != 0 || import.module != "env";
    explicit.then(|| Imported {
        name: symbol.name,
        module: import.module,
        field: import.field,
        ty: object.types[import.ty as usize],
    })
}

impl<'a> World<'a> {
    /// The type of the function a symbol refers to, as encoded, when it is a function.
    pub(crate) fn func_type(&self, file: usize, symbol: usize) -> Option<&'a [u8]> {
        let object = &self.files[file];
        let symbol = &object.symbols[symbol];
        if symbol.kind != Kind::Function {
            return None;
        }
        let imported = object.func_imports.len() as u32;
        let ty = if symbol.index < imported {
            object.func_imports[symbol.index as usize].ty
        } else {
            object.funcs[(symbol.index - imported) as usize]
        };
        Some(object.types[ty as usize])
    }

    /// The type of a defined function.
    pub(crate) fn defined_type(&self, file: usize, index: u32) -> &'a [u8] {
        let object = &self.files[file];
        object.types[object.funcs[index as usize] as usize]
    }

    /// Every reference to a function has the type of the definition it resolved to.
    fn check_signatures(&self) -> Result<(), Error> {
        for (file, object) in self.files.iter().enumerate() {
            for (i, symbol) in object.symbols.iter().enumerate() {
                if symbol.kind != Kind::Function || !symbol.is_undefined() {
                    continue;
                }
                let have = self.func_type(file, i).expect("a function symbol");
                let (want, place) = match self.targets[file][i] {
                    Where::Func(f, index) => (self.defined_type(f, index), &self.files[f].name),
                    Where::Import(index) => (self.imports[index as usize].ty, &object.name),
                    _ => continue,
                };
                if have != want {
                    return Err(Error::new(format!(
                        "function signature mismatch: {} is {} in {} and {} in {place}",
                        symbol.name,
                        signature(have),
                        object.name,
                        signature(want)
                    )));
                }
            }
        }
        Ok(())
    }
}

/// A function type in the text format's words, for a message.
pub(crate) fn signature(ty: &[u8]) -> String {
    let name = |byte: u8| match byte {
        0x7f => "i32",
        0x7e => "i64",
        0x7d => "f32",
        0x7c => "f64",
        0x7b => "v128",
        0x70 => "funcref",
        0x6f => "externref",
        _ => "?",
    };
    let params = ty.get(1).copied().unwrap_or(0) as usize;
    let list = |bytes: &[u8]| bytes.iter().map(|&b| name(b)).collect::<Vec<_>>().join(", ");
    let results = ty.get(2 + params + 1..).unwrap_or(&[]);
    format!("({}) -> ({})", list(ty.get(2..2 + params).unwrap_or(&[])), list(results))
}
