//! Step 2: read the inputs and resolve every symbol to the one place that defines it.
//!
//! The archive rule is LLD's, which is GNU ld's without the dependence on order. A member of an
//! archive is loaded when a strong reference to a name it defines is seen, before or after the
//! archive on the command line. A weak reference does not load a member. A strong definition
//! takes the place of a weak one, two strong definitions of one name are an error with both
//! objects in it, and a function must have the same type at its definition and at every
//! reference.
//!
//! The order is LLD's too, because the order of the objects is the order of the output. An object
//! is read symbol by symbol, and a strong reference to a name that an archive member defines loads
//! that member at once, depth first. An archive is read member by member, and a member that
//! defines a name a strong reference waits for loads at once. An object goes in the output when
//! it has been read to the end, so after the members that it loaded.
//!
//! What is left undefined at the end is one of four things. A name the linker defines, such as
//! `__stack_pointer` or `__heap_base`, is synthesized. A function with an explicit import name or
//! module, such as `fd_write` of `wasi_snapshot_preview1`, is an import of the module. A weak
//! reference is null, and a call to it traps. Anything else is an undefined symbol and an error.

use std::collections::HashMap;

use crate::object::{EXPLICIT_NAME, Kind, Object, Symbol};
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
    /// The number of names in the table before this one, which is the order LLD walks its table
    /// in, and so the order of the imports.
    seq: usize,
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
    /// The undefined weak symbols, in the order of the symbol table, each with the object and the
    /// symbol that first named it. A call to one of these functions goes to a stub that traps.
    pub(crate) weak: Vec<(&'a str, usize, usize)>,
}

struct Loader<'a> {
    /// The objects in the order their parse started, which is the index each one is known by
    /// while the inputs load.
    files: Vec<Object<'a>>,
    /// The objects in the order their parse ended, which is LLD's order and the order of the
    /// output.
    order: Vec<usize>,
    archives: Vec<Vec<Option<Object<'a>>>>,
    table: HashMap<&'a str, Entry>,
    /// The COMDAT groups seen, so that a second copy of a group is dropped.
    comdats: HashMap<&'a str, usize>,
}

/// An object whose symbols are being added: its index, the next symbol, and which of its
/// definitions are in a COMDAT group that an earlier object kept.
struct Frame {
    file: usize,
    next: usize,
    dropped: Vec<bool>,
}

impl<'a> World<'a> {
    /// Loads the inputs and resolves every symbol.
    pub(crate) fn load(options: &Options, inputs: &[Input<'a>]) -> Result<Self, Error> {
        let mut loader = Loader {
            files: Vec::new(),
            order: Vec::new(),
            archives: Vec::new(),
            table: HashMap::new(),
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
                    if loader.lazy(index, member) {
                        loader.extract(index, member)?;
                    }
                }
            } else {
                loader.load(Object::parse(input.name, input.bytes)?)?;
            }
        }
        // In LLD's order, which loads the members for `-u` before the ones for the exports and
        // those before the one for the entry.
        let roots = options.undefined.iter().chain(&options.exports).chain(&options.entry);
        for name in roots {
            loader.root(name)?;
        }
        loader.finish(options)
    }
}

impl<'a> Loader<'a> {
    /// Adds a name to the table, numbered in the order the names were first seen.
    fn entry(&mut self, name: &'a str, kind: Kind, state: State) -> &mut Entry {
        let seq = self.table.len();
        self.table.entry(name).or_insert(Entry { kind, state, seq, first: None, strong: false })
    }

    /// Notes the names a member of an archive defines, and says whether the member must load,
    /// which is when a strong reference already waits for one of them.
    ///
    /// The names after the one that loads the member are not noted, because the member defines
    /// them as it loads. That is what LLD does, and it decides the order of the names.
    fn lazy(&mut self, archive: usize, member: usize) -> bool {
        let object = self.archives[archive][member].as_ref().expect("a member not yet loaded");
        let symbols: Vec<(&'a str, Kind)> = object
            .symbols
            .iter()
            .filter(|s| !s.is_undefined() && !s.is_local() && s.kind != Kind::Section)
            .map(|s| (s.name, s.kind))
            .collect();
        for (name, kind) in symbols {
            let entry = self.entry(name, kind, State::Lazy(archive, member));
            if let State::Undefined = entry.state {
                entry.state = State::Lazy(archive, member);
                if entry.strong {
                    return true;
                }
            }
        }
        false
    }

    /// Loads a member of an archive, unless it is loaded already.
    fn extract(&mut self, archive: usize, member: usize) -> Result<(), Error> {
        match self.archives[archive][member].take() {
            Some(object) => self.load(object),
            None => Ok(()),
        }
    }

    /// A name an option makes a strong reference to.
    fn root(&mut self, name: &str) -> Result<(), Error> {
        let Some(entry) = self.table.get_mut(name) else { return Ok(()) };
        entry.strong = true;
        match entry.state {
            State::Lazy(archive, member) => self.extract(archive, member),
            _ => Ok(()),
        }
    }

    /// Loads an object and, depth first, each member that a strong reference in it needs, at the
    /// reference, as LLD does. An object is in the output after the members it loaded, so the
    /// order of the output is LLD's.
    ///
    /// The stack is a vector and not the call stack, because a chain of members can be as long as
    /// the archive, and rucc as wasm has a stack of one megabyte.
    fn load(&mut self, object: Object<'a>) -> Result<(), Error> {
        let mut stack = vec![self.start(object)];
        while let Some(frame) = stack.last_mut() {
            let file = frame.file;
            let Some(symbol) = self.files[file].symbols.get(frame.next).cloned() else {
                stack.pop();
                self.order.push(file);
                continue;
            };
            let i = frame.next;
            frame.next += 1;
            let dropped = frame.dropped[i];
            if let Some((archive, member)) = self.symbol(file, i, &symbol, dropped)? {
                if let Some(object) = self.archives[archive][member].take() {
                    stack.push(self.start(object));
                }
            }
        }
        Ok(())
    }

    /// Takes an object in, keeps the COMDAT groups no earlier object has, and returns the frame
    /// its symbols are added from.
    fn start(&mut self, object: Object<'a>) -> Frame {
        let file = self.files.len();
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
        let dropped = object
            .symbols
            .iter()
            .map(|symbol| match symbol.kind {
                _ if symbol.is_undefined() => false,
                Kind::Function => funcs[(symbol.index - imported) as usize],
                Kind::Data => segments[symbol.index as usize],
                _ => false,
            })
            .collect();
        self.files.push(object);
        Frame { file, next: 0, dropped }
    }

    /// Adds one symbol of an object to the table. Returns the member of an archive that a strong
    /// reference in it needs, if any.
    fn symbol(
        &mut self,
        file: usize,
        i: usize,
        symbol: &Symbol<'a>,
        dropped: bool,
    ) -> Result<Option<(usize, usize)>, Error> {
        if symbol.is_local() || symbol.kind == Kind::Section {
            return Ok(None);
        }
        let files = &self.files;
        let seq = self.table.len();
        let entry = self.table.entry(symbol.name).or_insert(Entry {
            kind: symbol.kind,
            state: State::Undefined,
            seq,
            first: None,
            strong: false,
        });
        let object = &files[file].name;
        if entry.kind != symbol.kind {
            let other = match entry.state {
                State::Defined(f, _, _) => files[f].name.clone(),
                _ => entry.first.map_or_else(String::new, |(f, _)| files[f].name.clone()),
            };
            return Err(Error::new(format!(
                "{} is {:?} in {object} and {:?} in {other}",
                symbol.name, symbol.kind, entry.kind
            )));
        }
        if symbol.is_undefined() || dropped {
            entry.first = entry.first.or(Some((file, i)));
            if !symbol.is_weak() {
                entry.strong = true;
                if let State::Lazy(archive, member) = entry.state {
                    return Ok(Some((archive, member)));
                }
            }
            return Ok(None);
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
                    "{} is defined in {} and in {object}",
                    symbol.name, files[other].name
                )));
            }
            State::Defined(..) => {}
        }
        Ok(None)
    }

    fn finish(self, options: &Options) -> Result<World<'a>, Error> {
        let Loader { files, order, mut table, .. } = self;
        // The objects go in the order their parse ended, and every index into them moves with them.
        let mut moved = vec![0; files.len()];
        for (to, &from) in order.iter().enumerate() {
            moved[from] = to;
        }
        let mut slots: Vec<Option<Object<'a>>> = files.into_iter().map(Some).collect();
        let files: Vec<Object<'a>> =
            order.iter().map(|&from| slots[from].take().expect("each object once")).collect();
        for entry in table.values_mut() {
            if let State::Defined(file, symbol, weak) = entry.state {
                entry.state = State::Defined(moved[file], symbol, weak);
            }
            entry.first = entry.first.map(|(file, symbol)| (moved[file], symbol));
        }
        let mut imports: Vec<Imported<'a>> = Vec::new();
        let mut import_of: HashMap<&'a str, u32> = HashMap::new();
        let mut undefined = Vec::new();
        let mut weak = Vec::new();
        // Where each global name goes.
        let mut names: HashMap<&'a str, Where> = HashMap::new();
        // In the order the names were first seen, which is the order of the imports in the module.
        // The order of a hash table changes from one run to the next, and the module must not.
        let mut entries: Vec<(&'a str, &Entry)> = table.iter().map(|(&n, e)| (n, e)).collect();
        entries.sort_unstable_by_key(|&(_, entry)| entry.seq);
        for (name, entry) in entries {
            let place = match entry.state {
                State::Defined(file, symbol, _) => own(&files[file], file, symbol),
                State::Undefined | State::Lazy(..) => {
                    let Some((file, symbol)) = entry.first else { continue };
                    if let Some(synth) = Synth::of(name, entry.kind) {
                        Where::Synth(synth)
                    } else if let Some(import) = import(&files[file], symbol, options) {
                        let index = *import_of.entry(name).or_insert_with(|| {
                            imports.push(import);
                            imports.len() as u32 - 1
                        });
                        Where::Import(index)
                    } else if !entry.strong {
                        weak.push((name, file, symbol));
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
        let mut world = World { files, targets, imports, entry: None, exports: Vec::new(), weak };
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
/// other than `env`, or when the options allow an undefined function.
fn import<'a>(object: &Object<'a>, symbol: usize, options: &Options) -> Option<Imported<'a>> {
    let symbol = &object.symbols[symbol];
    if symbol.kind != Kind::Function {
        return None;
    }
    let import = &object.func_imports[symbol.index as usize];
    let explicit = symbol.flags & EXPLICIT_NAME != 0 || import.module != "env";
    (explicit || options.allow_undefined).then(|| Imported {
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
