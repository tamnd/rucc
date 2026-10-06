//! Steps 3 and 7 to 10: check the features, number everything that is live, write the functions
//! the linker makes, apply the relocations, and write the module.
//!
//! The index spaces put the imports first, as the binary format requires. The defined functions
//! are `__wasm_call_ctors`, then one trap function for each type that a call to an undefined weak
//! function needs, then the functions of the objects in load order. The globals are the stack
//! pointer, then `__memory_base` and `__table_base` when an object refers to them, then the
//! globals of the objects.
//!
//! A relocated field keeps its width, five bytes for a LEB128 and four for a 32-bit word, so the
//! code does not move. The output has no `linking` and no `reloc.*` section. It has a `name`
//! section unless the options strip it, and the `producers` and `target_features` sections of
//! the inputs, merged.

use std::collections::HashMap;

use crate::bytes::{name, sleb, sleb5, uleb, uleb5};
use crate::layout::Layout;
use crate::live::Live;
use crate::object::{EXPORTED, Kind, Reloc};
use crate::resolve::{Synth, Where, World};
use crate::{Error, Options};

/// The type of `__wasm_call_ctors` and of a constructor.
const VOID: &[u8] = &[0x60, 0, 0];

struct Writer<'w, 'a> {
    world: &'w World<'a>,
    live: &'w Live<'a>,
    layout: &'w Layout,
    types: Vec<&'a [u8]>,
    type_of: HashMap<&'a [u8], u32>,
    /// For each object, for each defined function, its index in the output.
    funcs: Vec<Vec<Option<u32>>>,
    imports: Vec<Option<u32>>,
    /// The imported functions, as indices into the world's list, in output order.
    import_order: Vec<usize>,
    ctors: Option<u32>,
    stubs: HashMap<&'a [u8], u32>,
    /// The defined functions in output order.
    bodies: Vec<Body>,
    globals: Vec<Vec<Option<u32>>>,
    synth_globals: Vec<(Synth, u32)>,
    tags: Vec<Vec<Option<u32>>>,
    global_count: u32,
}

#[derive(Debug, Clone, Copy)]
enum Body {
    Ctors,
    Stub(u32),
    Input(usize, u32),
}

/// Writes the module.
pub(crate) fn write(
    world: &World<'_>,
    live: &Live<'_>,
    layout: &Layout,
    options: &Options,
) -> Result<Vec<u8>, Error> {
    let features = features(world)?;
    let mut writer = Writer {
        world,
        live,
        layout,
        types: Vec::new(),
        type_of: HashMap::new(),
        funcs: world.files.iter().map(|o| vec![None; o.funcs.len()]).collect(),
        imports: vec![None; world.imports.len()],
        import_order: Vec::new(),
        ctors: None,
        stubs: HashMap::new(),
        bodies: Vec::new(),
        globals: world.files.iter().map(|o| vec![None; o.globals.len()]).collect(),
        synth_globals: Vec::new(),
        tags: world.files.iter().map(|o| vec![None; o.tags.len()]).collect(),
        global_count: 0,
    };
    writer.number();
    writer.module(options, &features)
}

/// The union of the features the objects use. A feature one object uses and another disallows
/// is an error.
fn features<'a>(world: &World<'a>) -> Result<Vec<&'a str>, Error> {
    let mut used: Vec<(&'a str, &str)> = Vec::new();
    let mut disallowed: Vec<(&'a str, &str)> = Vec::new();
    for object in &world.files {
        for &(prefix, feature) in &object.features {
            let list = if prefix == b'-' { &mut disallowed } else { &mut used };
            if !list.iter().any(|&(f, _)| f == feature) {
                list.push((feature, &object.name));
            }
        }
    }
    for &(feature, user) in &used {
        if let Some(&(_, other)) = disallowed.iter().find(|&&(f, _)| f == feature) {
            return Err(Error::new(format!(
                "{user} uses the feature {feature}, and {other} disallows it"
            )));
        }
    }
    let mut features: Vec<&str> = used.into_iter().map(|(f, _)| f).collect();
    features.sort_unstable();
    Ok(features)
}

impl<'w, 'a> Writer<'w, 'a> {
    fn ty(&mut self, ty: &'a [u8]) -> u32 {
        if let Some(&index) = self.type_of.get(ty) {
            return index;
        }
        let index = self.types.len() as u32;
        self.types.push(ty);
        self.type_of.insert(ty, index);
        index
    }

    fn number(&mut self) {
        let world = self.world;
        let live = self.live;
        let mut next = 0;
        for (i, &used) in live.imports.iter().enumerate() {
            if used {
                self.imports[i] = Some(next);
                self.import_order.push(i);
                next += 1;
            }
        }
        if live.synths.contains(&Synth::CallCtors) {
            self.ctors = Some(next);
            self.bodies.push(Body::Ctors);
            next += 1;
        }
        for (i, &ty) in live.stubs.iter().enumerate() {
            self.stubs.insert(ty, next);
            self.bodies.push(Body::Stub(i as u32));
            next += 1;
        }
        for (file, used) in live.funcs.iter().enumerate() {
            for (index, &used) in used.iter().enumerate() {
                if used {
                    self.funcs[file][index] = Some(next);
                    self.bodies.push(Body::Input(file, index as u32));
                    next += 1;
                }
            }
        }
        let mut global = 0;
        for synth in [Synth::StackPointer, Synth::MemoryBase, Synth::TableBase] {
            if live.synths.contains(&synth) {
                self.synth_globals.push((synth, global));
                global += 1;
            }
        }
        for (file, used) in live.globals.iter().enumerate() {
            for (index, &used) in used.iter().enumerate() {
                if used {
                    self.globals[file][index] = Some(global);
                    global += 1;
                }
            }
        }
        self.global_count = global;
        let mut tag = 0;
        for (file, used) in live.tags.iter().enumerate() {
            for (index, &used) in used.iter().enumerate() {
                if used {
                    self.tags[file][index] = Some(tag);
                    tag += 1;
                }
            }
        }
        // The types in the order the sections use them.
        for &i in &self.import_order.clone() {
            self.ty(world.imports[i].ty);
        }
        for body in self.bodies.clone() {
            let ty = self.body_type(body);
            self.ty(ty);
        }
        for (file, object) in world.files.iter().enumerate() {
            for (index, &ty) in object.tags.iter().enumerate() {
                if live.tags[file][index] {
                    self.ty(object.types[ty as usize]);
                }
            }
        }
        // A `call_indirect` can name a type that no function in the module has.
        for (file, object) in world.files.iter().enumerate() {
            for (index, ranges) in live.code[file].iter().enumerate() {
                if !live.funcs[file][index] {
                    continue;
                }
                for reloc in &object.code_relocs[ranges.clone()] {
                    if reloc.kind == 6 {
                        self.ty(object.types[reloc.index as usize]);
                    }
                }
            }
        }
    }

    fn body_type(&self, body: Body) -> &'a [u8] {
        match body {
            Body::Ctors => VOID,
            Body::Stub(i) => self.live.stubs[i as usize],
            Body::Input(file, index) => {
                let object = &self.world.files[file];
                object.types[object.funcs[index as usize] as usize]
            }
        }
    }

    /// The output index of the function a place names.
    fn func(&self, place: Where, file: usize, symbol: u32) -> Result<u32, Error> {
        let index = match place {
            Where::Func(f, i) => self.funcs[f][i as usize],
            Where::Import(i) => self.imports[i as usize],
            Where::Synth(Synth::CallCtors) => self.ctors,
            Where::Null => {
                let ty = self.world.func_type(file, symbol as usize);
                ty.and_then(|ty| self.stubs.get(ty).copied())
            }
            _ => None,
        };
        index.ok_or_else(|| self.bad(file, symbol, "a function"))
    }

    fn global(&self, place: Where, file: usize, symbol: u32) -> Result<u32, Error> {
        let index = match place {
            Where::Global(f, i) => self.globals[f][i as usize],
            Where::Synth(synth) => {
                self.synth_globals.iter().find(|&&(s, _)| s == synth).map(|&(_, i)| i)
            }
            _ => None,
        };
        index.ok_or_else(|| self.bad(file, symbol, "a global"))
    }

    fn bad(&self, file: usize, symbol: u32, what: &str) -> Error {
        let object = &self.world.files[file];
        let name = object.symbols.get(symbol as usize).map_or("", |s| s.name);
        Error::new(format!("{}: a relocation needs {what} and {name} is not one", object.name))
    }

    /// Patches one field of `bytes`, which start at offset `base` of their section.
    fn apply(&self, file: usize, reloc: &Reloc, bytes: &mut [u8], base: u32) -> Result<(), Error> {
        let at = (reloc.offset - base) as usize;
        let width = match reloc.kind {
            2 | 5 | 13 | 26 => 4,
            _ => 5,
        };
        if at + width > bytes.len() {
            return Err(Error::new(format!(
                "{}: a relocation at offset {} ends past its function or segment",
                self.world.files[file].name, reloc.offset
            )));
        }
        let object = &self.world.files[file];
        if reloc.kind == 6 {
            let index = self.type_of[object.types[reloc.index as usize]];
            uleb5(bytes, at, index);
            return Ok(());
        }
        let place = self.world.targets[file][reloc.index as usize];
        let word = |bytes: &mut [u8], value: u32| {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        match reloc.kind {
            0 => uleb5(bytes, at, self.func(place, file, reloc.index)?),
            26 => word(bytes, self.func(place, file, reloc.index)?),
            1 | 2 | 12 => {
                let slot = match place {
                    Where::Null => 0,
                    _ => *self
                        .layout
                        .slot_of
                        .get(&place)
                        .ok_or_else(|| self.bad(file, reloc.index, "a function"))?,
                };
                match reloc.kind {
                    1 => sleb5(bytes, at, slot as i32),
                    2 => word(bytes, slot),
                    // Relative to `__table_base`, which is 1.
                    _ => sleb5(bytes, at, slot as i32 - 1),
                }
            }
            3 | 4 | 5 | 11 => {
                let address = self
                    .layout
                    .address(place)
                    .ok_or_else(|| self.bad(file, reloc.index, "data"))?;
                // `__memory_base` is 0, so an address relative to it is the address.
                let value = (i64::from(address) + reloc.addend) as u32;
                match reloc.kind {
                    3 => uleb5(bytes, at, value),
                    5 => word(bytes, value),
                    _ => sleb5(bytes, at, value as i32),
                }
            }
            7 => uleb5(bytes, at, self.global(place, file, reloc.index)?),
            13 => word(bytes, self.global(place, file, reloc.index)?),
            10 => {
                let index = match place {
                    Where::Tag(f, i) => self.tags[f][i as usize],
                    _ => None,
                };
                uleb5(bytes, at, index.ok_or_else(|| self.bad(file, reloc.index, "a tag"))?);
            }
            20 => uleb5(bytes, at, 0),
            kind => {
                return Err(Error::new(format!(
                    "{}: relocation type {kind} is outside what the rucc linker covers; link with \
                     wasm-ld",
                    object.name
                )));
            }
        }
        Ok(())
    }

    fn module(&self, options: &Options, features: &[&str]) -> Result<Vec<u8>, Error> {
        let world = self.world;
        let mut out = b"\0asm\x01\0\0\0".to_vec();

        let mut s = Vec::new();
        uleb(&mut s, self.types.len() as u64);
        for ty in &self.types {
            s.extend_from_slice(ty);
        }
        section(&mut out, 1, &s);

        if !self.import_order.is_empty() {
            let mut s = Vec::new();
            uleb(&mut s, self.import_order.len() as u64);
            for &i in &self.import_order {
                let import = &world.imports[i];
                name(&mut s, import.module);
                name(&mut s, import.field);
                s.push(0);
                uleb(&mut s, u64::from(self.type_of[import.ty]));
            }
            section(&mut out, 2, &s);
        }

        let mut s = Vec::new();
        uleb(&mut s, self.bodies.len() as u64);
        for &body in &self.bodies {
            uleb(&mut s, u64::from(self.type_of[self.body_type(body)]));
        }
        section(&mut out, 3, &s);

        let slots = self.layout.slots.len() as u64 + 1;
        let mut s = vec![1, 0x70, 1];
        uleb(&mut s, slots);
        uleb(&mut s, slots);
        section(&mut out, 4, &s);

        let mut s = vec![1];
        match self.layout.max_pages {
            Some(max) => {
                s.push(1);
                uleb(&mut s, u64::from(self.layout.pages));
                uleb(&mut s, u64::from(max));
            }
            None => {
                s.push(0);
                uleb(&mut s, u64::from(self.layout.pages));
            }
        }
        section(&mut out, 5, &s);

        let tags: Vec<(usize, usize)> = (0..world.files.len())
            .flat_map(|f| (0..world.files[f].tags.len()).map(move |i| (f, i)))
            .filter(|&(f, i)| self.live.tags[f][i])
            .collect();
        if !tags.is_empty() {
            let mut s = Vec::new();
            uleb(&mut s, tags.len() as u64);
            for (file, index) in tags {
                let object = &world.files[file];
                s.push(0);
                uleb(&mut s, u64::from(self.type_of[object.types[object.tags[index] as usize]]));
            }
            section(&mut out, 13, &s);
        }

        let exports = self.exports(options)?;
        let data_exports: Vec<(&str, u32)> = exports
            .iter()
            .filter_map(|(name, export)| match export {
                Export::Data(address) => Some((name.as_str(), *address)),
                Export::Func(_) => None,
            })
            .collect();
        let mut s = Vec::new();
        uleb(&mut s, u64::from(self.global_count) + data_exports.len() as u64);
        for &(synth, _) in &self.synth_globals {
            let (mutable, value) = match synth {
                Synth::StackPointer => (1, self.layout.stack_high as i32),
                Synth::TableBase => (0, 1),
                _ => (0, 0),
            };
            s.extend([0x7f, mutable, 0x41]);
            sleb(&mut s, i64::from(value));
            s.push(0x0b);
        }
        for (file, object) in world.files.iter().enumerate() {
            for (index, global) in object.globals.iter().enumerate() {
                if self.live.globals[file][index] {
                    s.extend_from_slice(global.ty);
                    s.extend_from_slice(global.init);
                }
            }
        }
        for &(_, address) in &data_exports {
            s.extend([0x7f, 0, 0x41]);
            sleb(&mut s, i64::from(address as i32));
            s.push(0x0b);
        }
        section(&mut out, 6, &s);

        let mut s = Vec::new();
        uleb(&mut s, exports.len() as u64 + 1);
        name(&mut s, "memory");
        s.extend([2, 0]);
        let mut data_global = self.global_count;
        for (export, place) in &exports {
            name(&mut s, export);
            match place {
                Export::Func(index) => {
                    s.push(0);
                    uleb(&mut s, u64::from(*index));
                }
                Export::Data(_) => {
                    s.push(3);
                    uleb(&mut s, u64::from(data_global));
                    data_global += 1;
                }
            }
        }
        section(&mut out, 7, &s);

        if !self.layout.slots.is_empty() {
            let mut s = vec![1, 0, 0x41, 1, 0x0b];
            uleb(&mut s, self.layout.slots.len() as u64);
            for &slot in &self.layout.slots {
                let index = match slot {
                    Where::Func(f, i) => self.funcs[f][i as usize],
                    Where::Import(i) => self.imports[i as usize],
                    Where::Synth(Synth::CallCtors) => self.ctors,
                    _ => None,
                };
                let index = index.ok_or_else(|| {
                    Error::new("a table relocation names something that is not a function".into())
                })?;
                uleb(&mut s, u64::from(index));
            }
            section(&mut out, 9, &s);
        }

        let mut s = Vec::new();
        uleb(&mut s, self.bodies.len() as u64);
        for &body in &self.bodies {
            let code = self.code(body)?;
            uleb(&mut s, code.len() as u64);
            s.extend_from_slice(&code);
        }
        section(&mut out, 10, &s);

        let segments: Vec<_> = self.layout.segments.iter().filter(|s| !s.is_bss()).collect();
        if !segments.is_empty() {
            let mut s = Vec::new();
            uleb(&mut s, segments.len() as u64);
            for segment in segments {
                s.extend([0, 0x41]);
                sleb(&mut s, i64::from(segment.start as i32));
                s.push(0x0b);
                let bytes = self.data(segment)?;
                uleb(&mut s, bytes.len() as u64);
                s.extend_from_slice(&bytes);
            }
            section(&mut out, 11, &s);
        }

        if !options.strip {
            custom(&mut out, "name", &self.names());
        }
        let producers = self.producers();
        if !producers.is_empty() {
            custom(&mut out, "producers", &producers);
        }
        if !features.is_empty() {
            let mut s = Vec::new();
            uleb(&mut s, features.len() as u64);
            for feature in features {
                s.push(b'+');
                name(&mut s, feature);
            }
            custom(&mut out, "target_features", &s);
        }
        Ok(out)
    }

    fn code(&self, body: Body) -> Result<Vec<u8>, Error> {
        match body {
            Body::Ctors => {
                let mut inits = Vec::new();
                for (file, object) in self.world.files.iter().enumerate() {
                    for init in &object.inits {
                        inits.push((init.priority, file, init.symbol));
                    }
                }
                inits.sort_by_key(|&(priority, _, _)| priority);
                let mut code = vec![0];
                for (_, file, symbol) in inits {
                    let place = self.world.targets[file][symbol as usize];
                    code.push(0x10);
                    uleb(&mut code, u64::from(self.func(place, file, symbol)?));
                    let ty = self.world.func_type(file, symbol as usize).unwrap_or(VOID);
                    // A constructor that returns values has them dropped. The counts are one
                    // byte each, because a constructor has few parameters and results.
                    let results = ty.get(2 + ty.get(1).copied().unwrap_or(0) as usize);
                    code.extend(std::iter::repeat_n(0x1a, results.copied().unwrap_or(0).into()));
                }
                code.push(0x0b);
                Ok(code)
            }
            Body::Stub(_) => Ok(vec![0, 0x00, 0x0b]),
            Body::Input(file, index) => {
                let object = &self.world.files[file];
                let input = &object.code[index as usize];
                let mut code = input.bytes.to_vec();
                let range = self.live.code[file][index as usize].clone();
                for reloc in &object.code_relocs[range] {
                    self.apply(file, reloc, &mut code, input.offset)?;
                }
                Ok(code)
            }
        }
    }

    fn data(&self, segment: &crate::layout::Segment) -> Result<Vec<u8>, Error> {
        let mut bytes = vec![0; (segment.end - segment.start) as usize];
        for part in &segment.parts {
            let object = &self.world.files[part.file];
            let input = &object.data[part.segment as usize];
            let at = (part.address - segment.start) as usize;
            let len = (part.to - part.from) as usize;
            let out = &mut bytes[at..at + len];
            out.copy_from_slice(&input.bytes[part.from as usize..part.to as usize]);
            // Only a segment that is not cut has relocations, so its part is the whole of it.
            let range = self.live.data[part.file][part.segment as usize].clone();
            for reloc in &object.data_relocs[range] {
                self.apply(part.file, reloc, out, input.offset)?;
            }
        }
        Ok(bytes)
    }

    fn exports(&self, options: &Options) -> Result<Vec<(String, Export)>, Error> {
        let world = self.world;
        let mut exports: Vec<(String, Export)> = Vec::new();
        let mut add = |name: &str, export: Export| {
            if !exports.iter().any(|(n, _)| n == name) {
                exports.push((name.to_owned(), export));
            }
        };
        if let (Some(name), Some(place)) = (&options.entry, world.entry) {
            add(name, Export::Func(self.func(place, 0, 0)?));
        }
        for (name, place) in &world.exports {
            let export = match *place {
                Where::Data(..) => Export::Data(self.layout.address(*place).unwrap_or(0)),
                place => Export::Func(self.func(place, 0, 0)?),
            };
            add(name, export);
        }
        for (file, object) in world.files.iter().enumerate() {
            for (i, symbol) in object.symbols.iter().enumerate() {
                if symbol.flags & EXPORTED == 0 || symbol.is_undefined() {
                    continue;
                }
                let place = world.targets[file][i];
                let export = match symbol.kind {
                    Kind::Function => Export::Func(self.func(place, file, i as u32)?),
                    Kind::Data => Export::Data(self.layout.address(place).unwrap_or(0)),
                    _ => continue,
                };
                let name = object
                    .exports
                    .iter()
                    .find(|&&(_, kind, index)| kind == 0 && index == symbol.index)
                    .filter(|_| symbol.kind == Kind::Function)
                    .map_or(symbol.name, |&(name, _, _)| name);
                add(name, export);
            }
        }
        Ok(exports)
    }

    /// The `name` section: the function names, the global names and the data segment names.
    fn names(&self) -> Vec<u8> {
        let world = self.world;
        let mut funcs: Vec<(u32, &str)> = Vec::new();
        for &i in &self.import_order {
            funcs.push((self.imports[i].unwrap_or(0), world.imports[i].name));
        }
        if let Some(index) = self.ctors {
            funcs.push((index, "__wasm_call_ctors"));
        }
        for &index in self.stubs.values() {
            funcs.push((index, "undefined_weak"));
        }
        for (file, object) in world.files.iter().enumerate() {
            let imported = object.func_imports.len() as u32;
            let mut named = vec![None; object.funcs.len()];
            for symbol in &object.symbols {
                if symbol.kind != Kind::Function || symbol.is_undefined() {
                    continue;
                }
                let slot = &mut named[(symbol.index - imported) as usize];
                if slot.is_none() || !symbol.is_local() {
                    *slot = Some(symbol.name);
                }
            }
            for (index, name) in named.into_iter().enumerate() {
                if let (Some(out), Some(name)) = (self.funcs[file][index], name) {
                    funcs.push((out, name));
                }
            }
        }
        funcs.sort_unstable_by_key(|&(index, _)| index);
        let mut s = Vec::new();
        let mut sub = Vec::new();
        uleb(&mut sub, funcs.len() as u64);
        for (index, func) in funcs {
            uleb(&mut sub, u64::from(index));
            name(&mut sub, func);
        }
        subsection(&mut s, 1, &sub);
        let mut globals: Vec<(u32, &str)> = Vec::new();
        for &(synth, index) in &self.synth_globals {
            let name = match synth {
                Synth::StackPointer => "__stack_pointer",
                Synth::MemoryBase => "__memory_base",
                _ => "__table_base",
            };
            globals.push((index, name));
        }
        for (file, object) in world.files.iter().enumerate() {
            let imported = object.global_imports.len() as u32;
            for symbol in &object.symbols {
                if symbol.kind == Kind::Global && !symbol.is_undefined() {
                    if let Some(index) = self.globals[file][(symbol.index - imported) as usize] {
                        globals.push((index, symbol.name));
                    }
                }
            }
        }
        globals.sort_unstable_by_key(|&(index, _)| index);
        globals.dedup_by_key(|&mut (index, _)| index);
        let mut sub = Vec::new();
        uleb(&mut sub, globals.len() as u64);
        for (index, global) in globals {
            uleb(&mut sub, u64::from(index));
            name(&mut sub, global);
        }
        subsection(&mut s, 7, &sub);
        let segments: Vec<_> = self.layout.segments.iter().filter(|s| !s.is_bss()).collect();
        let mut sub = Vec::new();
        uleb(&mut sub, segments.len() as u64);
        for (index, segment) in segments.iter().enumerate() {
            uleb(&mut sub, index as u64);
            name(&mut sub, &segment.name);
        }
        subsection(&mut s, 9, &sub);
        s
    }

    /// The `producers` sections of the inputs, merged: each field once, and each value once in
    /// it, in the order they are first seen.
    fn producers(&self) -> Vec<u8> {
        let mut fields: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
        for object in &self.world.files {
            for (field, values) in &object.producers {
                let at = match fields.iter().position(|(f, _)| f == field) {
                    Some(at) => at,
                    None => {
                        fields.push((field, Vec::new()));
                        fields.len() - 1
                    }
                };
                for value in values {
                    if !fields[at].1.iter().any(|(n, _)| *n == value.0) {
                        fields[at].1.push(*value);
                    }
                }
            }
        }
        if fields.is_empty() {
            return Vec::new();
        }
        let mut s = Vec::new();
        uleb(&mut s, fields.len() as u64);
        for (field, values) in fields {
            name(&mut s, field);
            uleb(&mut s, values.len() as u64);
            for (value, version) in values {
                name(&mut s, value);
                name(&mut s, version);
            }
        }
        s
    }
}

#[derive(Debug, Clone, Copy)]
enum Export {
    Func(u32),
    Data(u32),
}

fn section(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
    out.push(id);
    uleb(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

fn subsection(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
    section(out, id, payload);
}

fn custom(out: &mut Vec<u8>, title: &str, payload: &[u8]) {
    let mut s = Vec::new();
    name(&mut s, title);
    s.extend_from_slice(payload);
    section(out, 0, &s);
}
