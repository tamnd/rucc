//! Steps 5 and 6: give each live segment its address and each function whose address is taken
//! its slot in the table.
//!
//! The memory is laid out as LLD 23 lays it out by default (`--stack-first`). The stack comes
//! first, from address 0 up to its size, and `__stack_pointer` starts at its top. Then the data,
//! in the order `.rodata`, `.data`, the segments with other names, and `.bss`, each the input
//! segments of that name in input order at their alignment. A string segment is cut after each
//! NUL into its strings, and a string that is already in the output segment, or that is the end of
//! a string in it, is not written again, as LLD does. `__heap_base` is the end of the data aligned to 16, and the memory starts with
//! the pages that cover it.
//!
//! The segments that are not strings are in load order. This linker loads the archive members in
//! the order it first needs them, from a queue, and LLD loads each one when it needs it, depth
//! first. So the order of the members, and the padding between their segments, can be different
//! from the order and the padding in the module that `wasm-ld` makes. For a small program that
//! calls `printf` the difference is 4 bytes, and the program does the same thing.
//!
//! The table starts at slot 1, so that a null function pointer traps when it is called. Slots go
//! to functions in the order their first table relocation is seen.

use std::collections::HashMap;

use crate::live::Live;
use crate::object::STRINGS;
use crate::resolve::{Where, World};
use crate::{Error, Options};

/// The size of a wasm page.
pub(crate) const PAGE: u32 = 65_536;

/// An output data segment.
#[derive(Debug)]
pub(crate) struct Segment {
    pub(crate) name: String,
    pub(crate) start: u32,
    pub(crate) end: u32,
    /// The pieces of the input segments in it, in address order.
    pub(crate) parts: Vec<Part>,
}

/// The bytes `from..to` of an input segment, at an address in the output.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Part {
    pub(crate) file: usize,
    pub(crate) segment: u32,
    pub(crate) from: u32,
    pub(crate) to: u32,
    pub(crate) address: u32,
}

impl Segment {
    /// Whether the segment is zeros that the output does not write, because the memory the
    /// module defines starts as zeros.
    pub(crate) fn is_bss(&self) -> bool {
        self.name == ".bss"
    }
}

/// Where everything in the memory and the table goes.
#[derive(Debug)]
pub(crate) struct Layout {
    /// For each object, for each segment, its address when it is live.
    pub(crate) addresses: Vec<Vec<Option<u32>>>,
    /// For a string segment that was cut into strings, where each string starts in the input
    /// and in the output, in input order.
    pub(crate) strings: HashMap<(usize, u32), Vec<(u32, u32)>>,
    pub(crate) segments: Vec<Segment>,
    pub(crate) stack_high: u32,
    pub(crate) global_base: u32,
    pub(crate) data_end: u32,
    pub(crate) heap_base: u32,
    pub(crate) pages: u32,
    pub(crate) max_pages: Option<u32>,
    /// The function in each slot of the table from slot 1 on.
    pub(crate) slots: Vec<Where>,
    pub(crate) slot_of: HashMap<Where, u32>,
}

/// The name of the output segment an input segment goes in.
fn output_name(name: &str) -> &str {
    for prefix in [".rodata", ".data", ".bss", ".tdata", ".tbss", ".init_array"] {
        if name == prefix || name.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.')) {
            return prefix;
        }
    }
    name
}

/// The order of an output segment, which is LLD's.
fn rank(name: &str) -> u8 {
    match name {
        ".tdata" => 0,
        ".rodata" => 1,
        ".data" => 2,
        ".bss" | ".tbss" => 4,
        _ => 3,
    }
}

/// The strings of a string segment, each with its NUL, as ranges. A last string with no NUL is
/// one too.
fn strings(bytes: &[u8]) -> impl Iterator<Item = (u32, u32)> + '_ {
    let mut from = 0;
    std::iter::from_fn(move || {
        let rest = bytes.get(from..).filter(|rest| !rest.is_empty())?;
        let to = from + rest.iter().position(|&b| b == 0).map_or(rest.len(), |n| n + 1);
        let range = (from as u32, to as u32);
        from = to;
        Some(range)
    })
}

/// Lays out distinct strings so that a string that is the end of another one is not written
/// again, as LLD does at `-O1` and up. The strings are sorted by their bytes from the end, with
/// the longer first, so a string comes just after each string it is the end of. The result gives
/// each string its offset and whether it is written, and the size of the whole.
fn merge(mut strings: Vec<&[u8]>) -> (HashMap<&[u8], (u32, bool)>, u32) {
    strings.sort_unstable_by(|a, b| b.iter().rev().cmp(a.iter().rev()));
    let mut offsets = HashMap::with_capacity(strings.len());
    let mut size = 0;
    let mut previous: Option<(&[u8], u32)> = None;
    for string in strings {
        let offset = match previous {
            Some((longer, at)) if longer.ends_with(string) => {
                offsets.insert(string, (at + (longer.len() - string.len()) as u32, false));
                continue;
            }
            _ => size,
        };
        offsets.insert(string, (offset, true));
        previous = Some((string, offset));
        size += string.len() as u32;
    }
    (offsets, size)
}

fn align(value: u64, log2: u32) -> u64 {
    let mask = (1u64 << log2.min(31)) - 1;
    (value + mask) & !mask
}

impl Layout {
    pub(crate) fn new(
        world: &World<'_>,
        live: &Live<'_>,
        options: &Options,
    ) -> Result<Self, Error> {
        let files = &world.files;
        // The output segments in order of first appearance, then sorted by rank.
        let mut order: Vec<&str> = Vec::new();
        let mut members: HashMap<&str, Vec<(usize, u32)>> = HashMap::new();
        for (file, object) in files.iter().enumerate() {
            for (index, segment) in object.data.iter().enumerate() {
                if !live.segments[file][index] {
                    continue;
                }
                let name = output_name(segment.name);
                if name.starts_with(".tdata") || name.starts_with(".tbss") {
                    return Err(Error::new(format!(
                        "{}: the thread-local segment {} needs threads, which the rucc linker \
                         does not cover; link with wasm-ld",
                        object.name, segment.name
                    )));
                }
                if !members.contains_key(name) {
                    order.push(name);
                }
                members.entry(name).or_default().push((file, index as u32));
            }
        }
        order.sort_by_key(|name| rank(name));
        let mut addresses: Vec<Vec<Option<u32>>> =
            files.iter().map(|o| vec![None; o.data.len()]).collect();
        let stack = u64::from(options.stack_size);
        let mut ptr = align(stack, 4);
        let stack_high = ptr;
        let global_base = ptr;
        let mut segments = Vec::new();
        let mut cut = HashMap::new();
        for name in order {
            let parts = &members[name];
            let log2 = parts.iter().map(|&(f, s)| files[f].data[s as usize].align).max();
            ptr = align(ptr, log2.unwrap_or(0));
            let start = ptr;
            let mut placed = Vec::with_capacity(parts.len());
            // The strings of the segment are merged first, and go where the first string segment
            // is, as LLD does.
            let cuttable = |file: usize, index: u32| {
                let input = &files[file].data[index as usize];
                input.flags & STRINGS != 0
                    && input.align == 0
                    && live.data[file][index as usize].is_empty()
            };
            let mut first: HashMap<&[u8], (usize, u32, u32, u32)> = HashMap::new();
            for &(file, index) in parts.iter().filter(|&&(f, i)| cuttable(f, i)) {
                let bytes = files[file].data[index as usize].bytes;
                for (from, to) in strings(bytes) {
                    let string = &bytes[from as usize..to as usize];
                    first.entry(string).or_insert((file, index, from, to));
                }
            }
            let (offsets, size) = merge(first.keys().copied().collect());
            let mut base = None;
            for &(file, index) in parts {
                let input = &files[file].data[index as usize];
                if !cuttable(file, index) {
                    let len = input.bytes.len() as u32;
                    ptr = align(ptr, input.align);
                    let address = u32::try_from(ptr).map_err(|_| too_large())?;
                    addresses[file][index as usize] = Some(address);
                    let part = Part { file, segment: index, from: 0, to: len, address };
                    placed.push(part);
                    ptr += u64::from(len);
                    continue;
                }
                let base = match base {
                    Some(base) => base,
                    None => {
                        let at = u32::try_from(ptr).map_err(|_| too_large())?;
                        for (string, &(file, segment, from, to)) in &first {
                            if let Some(&(offset, true)) = offsets.get(string) {
                                let address = at + offset;
                                placed.push(Part { file, segment, from, to, address });
                            }
                        }
                        ptr += u64::from(size);
                        *base.insert(at)
                    }
                };
                let starts: Vec<(u32, u32)> = strings(input.bytes)
                    .map(|(from, to)| {
                        (from, base + offsets[&input.bytes[from as usize..to as usize]].0)
                    })
                    .collect();
                addresses[file][index as usize] = Some(starts.first().map_or(base, |&(_, a)| a));
                cut.insert((file, index), starts);
            }
            placed.sort_unstable_by_key(|part| part.address);
            let start = u32::try_from(start).map_err(|_| too_large())?;
            let end = u32::try_from(ptr).map_err(|_| too_large())?;
            segments.push(Segment { name: name.to_owned(), start, end, parts: placed });
        }
        let data_end = ptr;
        let heap_base = align(ptr, 4);
        let needed = heap_base.div_ceil(u64::from(PAGE));
        let pages = match options.initial_memory {
            Some(bytes) => {
                if u64::from(bytes) < heap_base || bytes % PAGE != 0 {
                    return Err(Error::new(format!(
                        "the initial memory of {bytes} bytes is not a whole number of pages of at \
                         least {heap_base} bytes"
                    )));
                }
                u64::from(bytes / PAGE)
            }
            None => needed,
        };
        if pages > 65_536 {
            return Err(too_large());
        }
        let max_pages = match options.max_memory {
            Some(bytes) if bytes % PAGE != 0 || u64::from(bytes / PAGE) < pages => {
                return Err(Error::new(format!(
                    "the maximum memory of {bytes} bytes is not a whole number of pages of at \
                     least the initial memory"
                )));
            }
            Some(bytes) => Some(bytes / PAGE),
            None => None,
        };
        let (slots, slot_of) = Self::table(world, live);
        Ok(Layout {
            addresses,
            strings: cut,
            segments,
            stack_high: stack_high as u32,
            global_base: global_base as u32,
            data_end: data_end as u32,
            heap_base: heap_base as u32,
            pages: pages as u32,
            max_pages,
            slots,
            slot_of,
        })
    }

    fn table(world: &World<'_>, live: &Live<'_>) -> (Vec<Where>, HashMap<Where, u32>) {
        let mut slots = Vec::new();
        let mut slot_of = HashMap::new();
        for (file, object) in world.files.iter().enumerate() {
            let code = (0..object.code.len())
                .filter(|&i| live.funcs[file][i])
                .flat_map(|i| &object.code_relocs[live.code[file][i].clone()]);
            let data = (0..object.data.len())
                .filter(|&i| live.segments[file][i])
                .flat_map(|i| &object.data_relocs[live.data[file][i].clone()]);
            for reloc in code.chain(data) {
                if !matches!(reloc.kind, 1 | 2 | 12 | 18 | 19 | 24) {
                    continue;
                }
                let target = world.targets[file][reloc.index as usize];
                if target == Where::Null || slot_of.contains_key(&target) {
                    continue;
                }
                slots.push(target);
                slot_of.insert(target, slots.len() as u32);
            }
        }
        (slots, slot_of)
    }

    /// The address of a data symbol, or of a name the linker defines.
    pub(crate) fn address(&self, place: Where) -> Option<u32> {
        use crate::resolve::Synth;
        match place {
            Where::Data(file, segment, offset) => {
                if let Some(starts) = self.strings.get(&(file, segment)) {
                    let at = starts.partition_point(|&(from, _)| from <= offset).checked_sub(1)?;
                    let (from, address) = starts[at];
                    return Some(address + (offset - from));
                }
                self.addresses[file][segment as usize].map(|base| base + offset)
            }
            Where::Synth(synth) => match synth {
                Synth::DataEnd => Some(self.data_end),
                Synth::HeapBase => Some(self.heap_base),
                Synth::HeapEnd => Some(self.pages * PAGE),
                Synth::GlobalBase => Some(self.global_base),
                Synth::StackLow => Some(0),
                Synth::StackHigh => Some(self.stack_high),
                Synth::FirstPageEnd => Some(PAGE),
                _ => None,
            },
            Where::Null => Some(0),
            _ => None,
        }
    }
}

fn too_large() -> Error {
    Error::new("the data does not fit in a 32-bit memory".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_segment_is_cut_after_each_nul() {
        let cut: Vec<_> = strings(b"ab\0\0cd").collect();
        assert_eq!(cut, [(0, 3), (3, 4), (4, 6)]);
        assert_eq!(strings(b"").count(), 0);
    }

    #[test]
    fn a_string_that_ends_another_shares_its_bytes() {
        let (offsets, size) = merge(vec![b"insert\0", b"array_insert\0", b"ert\0", b"nan\0"]);
        // "array_insert\0", then "nan\0". "insert\0" and "ert\0" are the end of the first.
        assert_eq!(size, 13 + 4);
        assert_eq!(offsets[&b"array_insert\0"[..]], (0, true));
        assert_eq!(offsets[&b"insert\0"[..]], (6, false));
        assert_eq!(offsets[&b"ert\0"[..]], (9, false));
        assert_eq!(offsets[&b"nan\0"[..]], (13, true));
    }
}
