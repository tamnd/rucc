//! A section of bytes put into a finished object, and read back out of one.
//!
//! What `-flto` keeps beside the machine code, which is the module the code was made from. The
//! writers underneath are done with the file by the time that is known to be wanted, and the link
//! that reads it back is handed objects it did not write, so both halves work on the bytes of a
//! finished file rather than on the writer's tables.
//!
//! ELF only, and only little endian, which is every ELF target this compiler writes. A file in
//! any other format is left alone and has nothing in it to find, and an object nothing was put in
//! links the ordinary way, which is the answer a link with no module to read has to give anyway.

/// `SHT_PROGBITS`, bytes that mean whatever the section's name says they mean.
const PROGBITS: u64 = 1;

/// `SHF_EXCLUDE`, which tells the linker to leave the section out of what it writes. The module is
/// for the link to read, and an executable has no use for it.
const EXCLUDE: u64 = 0x8000_0000;

/// The first section index that means something other than an index. A file with that many
/// sections says how many in its first header instead, which nothing here writes.
const RESERVED: u64 = 0xff00;

/// Where the parts of a file's header that matter here are, which is in different places in the
/// two classes because every field after the first few is a different width.
struct Layout {
    /// Whether the file is a 64 bit one.
    wide: bool,
    /// `e_shoff`, where the section headers start.
    table: usize,
    /// `e_shentsize`, how far apart they are.
    each: usize,
    /// `e_shnum`, how many there are.
    count: usize,
    /// `e_shstrndx`, which one holds the names of all of them.
    names: usize,
}

impl Layout {
    /// The layout of `bytes`, or nothing for a file that is not a little endian ELF object or
    /// whose headers are not all inside it.
    fn of(bytes: &[u8]) -> Option<Layout> {
        if bytes.get(..4)? != b"\x7fELF" || *bytes.get(5)? != 1 {
            return None;
        }
        let wide = match bytes.get(4)? {
            1 => false,
            2 => true,
            _ => return None,
        };
        let (table, each, count, names) = if wide {
            (get(bytes, 0x28, 8)?, 0x3a, 0x3c, 0x3e)
        } else {
            (get(bytes, 0x20, 4)?, 0x2e, 0x30, 0x32)
        };
        let layout = Layout {
            wide,
            table: usize::try_from(table).ok()?,
            each: usize::try_from(get(bytes, each, 2)?).ok()?,
            count: usize::try_from(get(bytes, count, 2)?).ok()?,
            names: usize::try_from(get(bytes, names, 2)?).ok()?,
        };
        let size = if wide { 64 } else { 40 };
        let end = layout.count.checked_mul(layout.each)?.checked_add(layout.table)?;
        (layout.each == size && layout.names < layout.count && end <= bytes.len()).then_some(layout)
    }

    /// Where the `nth` header starts.
    fn header(&self, nth: usize) -> usize {
        self.table + nth * self.each
    }

    /// Where `sh_offset` and `sh_size` are in a header, and how wide each is.
    fn place(&self) -> (usize, usize, usize) {
        if self.wide { (24, 32, 8) } else { (16, 20, 4) }
    }

    /// Where a section's contents are in the file, checked to be inside it.
    fn contents(&self, bytes: &[u8], nth: usize) -> Option<std::ops::Range<usize>> {
        let (offset, size, width) = self.place();
        let at = self.header(nth);
        let from = usize::try_from(get(bytes, at + offset, width)?).ok()?;
        let to = from.checked_add(usize::try_from(get(bytes, at + size, width)?).ok()?)?;
        (to <= bytes.len()).then_some(from..to)
    }

    /// What the `nth` section is called.
    fn name<'a>(&self, bytes: &'a [u8], nth: usize) -> Option<&'a [u8]> {
        let strings = &bytes[self.contents(bytes, self.names)?];
        let from = usize::try_from(get(bytes, self.header(nth), 4)?).ok()?;
        let rest = strings.get(from..)?;
        Some(&rest[..rest.iter().position(|byte| *byte == 0)?])
    }
}

/// The little endian number `width` bytes long at `at`, or nothing past the end of the file.
fn get(bytes: &[u8], at: usize, width: usize) -> Option<u64> {
    let field = bytes.get(at..at.checked_add(width)?)?;
    Some(field.iter().rev().fold(0, |sum, &byte| sum << 8 | u64::from(byte)))
}

/// Writes `value` as a little endian number `width` bytes long at `at`.
fn put(bytes: &mut [u8], at: usize, width: usize, value: u64) {
    bytes[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
}

/// Puts `payload` into `object` as a section called `name`, which the linker leaves out of what it
/// writes. Whether it did, which it does not for a file that is not little endian ELF or that has
/// too many sections to add one to.
///
/// Everything goes on the end. The new section header table is the old one with one more header
/// after it, so no section changes its index and nothing that points at one by number has to be
/// told, and the names of the sections are copied there too with the new one after them, since
/// the old table of names has no room. The old copies of both are left where they were, which is
/// bytes nothing points at any more.
pub fn attach(object: &mut Vec<u8>, name: &str, payload: &[u8]) -> bool {
    let Some(layout) = Layout::of(object) else { return false };
    let Some(strings) = layout.contents(object, layout.names) else { return false };
    if layout.count == 0 || layout.count as u64 + 1 >= RESERVED || name.contains('\0') {
        return false;
    }
    let mut names = object[strings].to_vec();
    let named = names.len() as u64;
    names.extend_from_slice(name.as_bytes());
    names.push(0);
    let mut headers = object[layout.table..layout.header(layout.count)].to_vec();

    let contents = object.len();
    object.extend_from_slice(payload);
    let strings = object.len();
    object.extend_from_slice(&names);
    object.resize(object.len().next_multiple_of(8), 0);
    let table = object.len();

    // The table of names is somewhere else now and longer.
    let (offset, size, width) = layout.place();
    let at = layout.names * layout.each;
    put(&mut headers, at + offset, width, strings as u64);
    put(&mut headers, at + size, width, names.len() as u64);
    // And the new header, whose fields are in the same order in both classes and only some of
    // them wider in the 64 bit one: name, type, flags, address, offset, size, link, info,
    // alignment and the size of an entry.
    let word = if layout.wide { 8 } else { 4 };
    let fields = [
        (4, named),
        (4, PROGBITS),
        (word, EXCLUDE),
        (word, 0),
        (word, contents as u64),
        (word, payload.len() as u64),
        (4, 0),
        (4, 0),
        (word, 1),
        (word, 0),
    ];
    for (width, value) in fields {
        headers.extend_from_slice(&value.to_le_bytes()[..width]);
    }
    object.extend_from_slice(&headers);

    let (shoff, shnum) = if layout.wide { (0x28, 0x3c) } else { (0x20, 0x30) };
    put(object, shoff, word, table as u64);
    put(object, shnum, 2, layout.count as u64 + 1);
    true
}

/// The contents of the section called `name` in `object`, or nothing when there is no such
/// section or the file is not one [`attach`] could have put it in.
#[must_use]
pub fn carried<'a>(object: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let layout = Layout::of(object)?;
    let nth = (0..layout.count).find(|&nth| layout.name(object, nth) == Some(name.as_bytes()))?;
    Some(&object[layout.contents(object, nth)?])
}

#[cfg(test)]
mod tests {
    use super::{attach, carried};
    use object::elf;
    use object::write::{Object, Relocation, StandardSection, Symbol, SymbolSection};
    use object::{
        Architecture, BinaryFormat, Endianness, Object as _, ObjectSection as _, ObjectSymbol as _,
        RelocationFlags, SectionFlags, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
    };

    /// A small object with a function in it that calls another, so that there is a symbol table,
    /// a relocation section pointing at it by index and a table of names to move.
    fn object(architecture: Architecture, call: elf::RelocationType) -> Vec<u8> {
        let mut obj = Object::new(BinaryFormat::Elf, architecture, Endianness::Little);
        let text = obj.section_id(StandardSection::Text);
        let at = obj.append_section_data(text, &[0xe8, 0, 0, 0, 0, 0xc3], 16);
        obj.add_symbol(Symbol {
            name: b"f".to_vec(),
            value: at,
            size: 6,
            kind: SymbolKind::Text,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Section(text),
            flags: SymbolFlags::None,
        });
        let g = obj.add_symbol(Symbol {
            name: b"g".to_vec(),
            value: 0,
            size: 0,
            kind: SymbolKind::Text,
            scope: SymbolScope::Unknown,
            weak: false,
            section: SymbolSection::Undefined,
            flags: SymbolFlags::None,
        });
        obj.add_relocation(
            text,
            Relocation {
                offset: at + 1,
                symbol: g,
                addend: -4,
                flags: RelocationFlags::Elf { r_type: call },
            },
        )
        .expect("the relocation is one the writer takes");
        obj.write().expect("the object is written")
    }

    /// A section's name and contents.
    type Section = (String, Vec<u8>);

    /// A symbol's name and the index of the section it is in.
    type Named = (String, Option<usize>);

    /// What `object` reads in a file: each section's name and contents, and each symbol's name and
    /// section index, which is everything adding a section must leave as it was. The table of
    /// names is the one section that does change, and its contents are the names read here.
    fn read(bytes: &[u8]) -> (Vec<Section>, Vec<Named>, usize) {
        let file = object::File::parse(bytes).expect("the file still parses");
        let sections = file
            .sections()
            .filter(|s| s.name().ok() != Some(".shstrtab"))
            .map(|s| (s.name().unwrap().to_string(), s.data().unwrap().to_vec()))
            .collect();
        let symbols = file
            .symbols()
            .map(|s| (s.name().unwrap().to_string(), s.section_index().map(|i| i.0)))
            .collect();
        let relocs = file.sections().map(|s| s.relocations().count()).sum();
        (sections, symbols, relocs)
    }

    #[test]
    fn a_section_put_in_is_read_back_and_nothing_else_moves() {
        for (architecture, call) in [
            (Architecture::X86_64, elf::R_X86_64_PLT32),
            (Architecture::I386, elf::R_386_PC32),
            (Architecture::Aarch64, elf::R_AARCH64_CALL26),
        ] {
            let before = object(architecture, call);
            let mut after = before.clone();
            assert!(attach(&mut after, ".rucc.lto", b"the module"), "{architecture:?}");
            assert_eq!(carried(&after, ".rucc.lto"), Some(&b"the module"[..]), "{architecture:?}");
            assert_eq!(carried(&after, ".rucc.other"), None);
            assert_eq!(carried(&before, ".rucc.lto"), None);

            let (old, symbols, relocs) = read(&before);
            let (mut new, moved, kept) = read(&after);
            let added = new.pop().expect("the new section is the last one");
            assert_eq!(added, (".rucc.lto".to_string(), b"the module".to_vec()));
            assert_eq!(new, old, "{architecture:?}: every other section is as it was");
            assert_eq!(moved, symbols, "{architecture:?}: every symbol is where it was");
            assert_eq!(kept, relocs, "{architecture:?}: and so is every relocation");

            let file = object::File::parse(&*after).unwrap();
            let section = file.section_by_name(".rucc.lto").unwrap();
            assert_eq!(section.kind(), SectionKind::Other, "{architecture:?}: not loaded");
            let SectionFlags::Elf { sh_type, sh_flags } = section.flags() else {
                panic!("not ELF")
            };
            assert_eq!(
                (sh_type, sh_flags),
                (elf::SHT_PROGBITS, elf::SHF_EXCLUDE),
                "{architecture:?}"
            );
        }
    }

    #[test]
    fn a_file_that_is_not_elf_is_left_alone() {
        let mut obj = Object::new(BinaryFormat::Coff, Architecture::X86_64, Endianness::Little);
        let text = obj.section_id(StandardSection::Text);
        obj.append_section_data(text, &[0xc3], 16);
        let before = obj.write().unwrap();
        let mut after = before.clone();
        assert!(!attach(&mut after, ".rucc.lto", b"the module"));
        assert_eq!(after, before);
        assert_eq!(carried(&after, ".rucc.lto"), None);

        // Nor is anything read out of a file that is not an object, or of one cut short.
        assert_eq!(carried(b"!<arch>\n", ".rucc.lto"), None);
        let mut whole = object(Architecture::X86_64, elf::R_X86_64_PLT32);
        assert!(attach(&mut whole, ".rucc.lto", b"the module"));
        for len in [0, 4, 16, 64, whole.len() / 2, whole.len() - 1] {
            assert_eq!(carried(&whole[..len], ".rucc.lto"), None, "{len} bytes");
        }
    }
}
