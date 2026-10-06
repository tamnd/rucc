//! Step 4: keep what the roots reach and drop the rest.
//!
//! The roots are the entry, the exports, the symbols marked `EXPORTED` or `NO_STRIP`, and the
//! segments marked `RETAIN`. From each live function and segment the step follows the
//! relocations in it to what they name. When `__wasm_call_ctors` is live, every constructor of a
//! loaded object is live too, because that function calls them. This is the step that makes the
//! module for an empty `main` about 20 KB when `libc.a` is 3 MB.

use std::ops::Range;

use crate::object::{EXPORTED, Kind, NO_STRIP, RETAIN, Reloc};
use crate::resolve::{Synth, Where, World};

/// What the roots reach.
#[derive(Debug)]
pub(crate) struct Live<'a> {
    /// For each object, for each defined function, whether it is live.
    pub(crate) funcs: Vec<Vec<bool>>,
    pub(crate) segments: Vec<Vec<bool>>,
    pub(crate) globals: Vec<Vec<bool>>,
    pub(crate) tags: Vec<Vec<bool>>,
    pub(crate) imports: Vec<bool>,
    pub(crate) synths: Vec<Synth>,
    /// The types of the trap functions that calls to undefined weak functions need.
    pub(crate) stubs: Vec<&'a [u8]>,
    /// For each object, for each body, the range of its relocations in `code_relocs`.
    pub(crate) code: Vec<Vec<Range<usize>>>,
    /// For each object, for each segment, the range of its relocations in `data_relocs`.
    pub(crate) data: Vec<Vec<Range<usize>>>,
}

/// The relocations of each chunk, as ranges of `relocs`, which must be sorted by offset.
fn ranges(relocs: &[Reloc], chunks: impl Iterator<Item = (u32, usize)>) -> Vec<Range<usize>> {
    chunks
        .map(|(offset, len)| {
            let end = offset as usize + len;
            let start = relocs.partition_point(|r| r.offset < offset);
            start..start + relocs[start..].partition_point(|r| (r.offset as usize) < end)
        })
        .collect()
}

impl<'a> Live<'a> {
    pub(crate) fn mark(world: &mut World<'a>) -> Self {
        for object in &mut world.files {
            object.code_relocs.sort_by_key(|r| r.offset);
            object.data_relocs.sort_by_key(|r| r.offset);
        }
        let world = &*world;
        let files = &world.files;
        let mut live = Live {
            funcs: files.iter().map(|o| vec![false; o.funcs.len()]).collect(),
            segments: files.iter().map(|o| vec![false; o.data.len()]).collect(),
            globals: files.iter().map(|o| vec![false; o.globals.len()]).collect(),
            tags: files.iter().map(|o| vec![false; o.tags.len()]).collect(),
            imports: vec![false; world.imports.len()],
            synths: vec![Synth::StackPointer],
            stubs: Vec::new(),
            code: files
                .iter()
                .map(|o| ranges(&o.code_relocs, o.code.iter().map(|b| (b.offset, b.bytes.len()))))
                .collect(),
            data: files
                .iter()
                .map(|o| ranges(&o.data_relocs, o.data.iter().map(|s| (s.offset, s.bytes.len()))))
                .collect(),
        };
        let mut work = Vec::new();
        work.extend(world.entry);
        work.extend(world.exports.iter().map(|&(_, place)| place));
        for (file, object) in files.iter().enumerate() {
            for (i, symbol) in object.symbols.iter().enumerate() {
                let root = symbol.flags & (EXPORTED | NO_STRIP) != 0;
                if root && !symbol.is_undefined() && symbol.kind != Kind::Table {
                    work.push(world.targets[file][i]);
                }
            }
            for (segment, data) in object.data.iter().enumerate() {
                if data.flags & RETAIN != 0 {
                    work.push(Where::Data(file, segment as u32, 0));
                }
            }
        }
        while let Some(place) = work.pop() {
            live.visit(world, place, &mut work);
        }
        live
    }

    fn visit(&mut self, world: &World<'a>, place: Where, work: &mut Vec<Where>) {
        let (file, relocs) = match place {
            Where::Func(file, index) => {
                let seen = &mut self.funcs[file][index as usize];
                if *seen {
                    return;
                }
                *seen = true;
                let range = self.code[file][index as usize].clone();
                (file, &world.files[file].code_relocs[range])
            }
            Where::Data(file, segment, _) => {
                let seen = &mut self.segments[file][segment as usize];
                if *seen {
                    return;
                }
                *seen = true;
                let range = self.data[file][segment as usize].clone();
                (file, &world.files[file].data_relocs[range])
            }
            Where::Global(file, index) => {
                self.globals[file][index as usize] = true;
                return;
            }
            Where::Tag(file, index) => {
                self.tags[file][index as usize] = true;
                return;
            }
            Where::Import(index) => {
                self.imports[index as usize] = true;
                return;
            }
            Where::Synth(synth) => {
                if !self.synths.contains(&synth) {
                    self.synths.push(synth);
                    if synth == Synth::CallCtors {
                        for (file, object) in world.files.iter().enumerate() {
                            for init in &object.inits {
                                work.push(world.targets[file][init.symbol as usize]);
                            }
                        }
                    }
                }
                return;
            }
            Where::Null | Where::Nothing => return,
        };
        for reloc in relocs {
            if reloc.kind == 6 {
                continue;
            }
            let target = world.targets[file][reloc.index as usize];
            if target == Where::Null && matches!(reloc.kind, 0 | 26) {
                let ty = world.func_type(file, reloc.index as usize).expect("a function symbol");
                if !self.stubs.contains(&ty) {
                    self.stubs.push(ty);
                }
            }
            work.push(target);
        }
    }
}
