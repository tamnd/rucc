//! The stub, read back, by a reader that knows nothing about the writer.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` section 9.8, the second of its three correctness
//! properties.
//!
//! Everything below reads the bytes at offsets taken from the ELF specification rather than through
//! anything in `rucc_stub`. That is the point of the file. A writer checked against its own reader
//! agrees with itself and says nothing about whether it agrees with a linker, and the field orders
//! here are written out twice, once per class, precisely so that a wrong one in the writer has to be
//! wrong in the same way in two places to go unnoticed.

use rucc_stub::{Binding, Kind, Library, Symbol, write};
use rucc_tuple::TargetTuple;

fn target(spelling: &str) -> TargetTuple {
    spelling.parse().expect("a row in the target table")
}

/// A small libc with one of everything that can vary.
fn libc() -> Library {
    let mut library = Library::new("libc.so");
    library
        .needs("ld-musl-x86_64.so.1")
        .function("printf")
        .function("malloc")
        .object("environ", 8)
        .object("stdout", 8)
        .export(Symbol::function("pthread_cancel").weak())
        .export(Symbol::object("__progname", 8).weak());
    library
}

/// A small glibc, which is the libc that has version nodes.
///
/// `memcpy` is here with two definitions because it is the real example: glibc 2.14 changed what
/// `memcpy` does about overlapping arguments and kept the old one at the old node, so a program built
/// before 2.14 keeps the behaviour it was built against. `gettimeofday` has no node at all, because
/// not everything glibc exports is versioned and a writer that assumes otherwise is wrong about a
/// real library.
fn glibc() -> Library {
    let mut library = Library::new("libc.so.6");
    library
        .needs("ld-linux-x86-64.so.2")
        .export(Symbol::function("memcpy").at("GLIBC_2.14"))
        .export(Symbol::function("memcpy").behind("GLIBC_2.2.5"))
        .export(Symbol::function("printf").at("GLIBC_2.2.5"))
        .export(Symbol::object("environ", 8).at("GLIBC_2.2.5"))
        .export(Symbol::function("pthread_cancel").weak().at("GLIBC_2.34"))
        .function("gettimeofday");
    library
}

/// An ELF file, read by offset.
struct Elf<'a> {
    bytes: &'a [u8],
    little: bool,
    wide: bool,
}

/// One section header, with the fields this file looks at.
struct Section {
    name: String,
    kind: u32,
    flags: u64,
    addr: u64,
    offset: usize,
    size: usize,
    link: u32,
    info: u32,
    entsize: u64,
}

/// One symbol, read back out of `.dynsym`.
#[derive(Debug, PartialEq, Eq)]
struct Read {
    name: String,
    kind: Kind,
    binding: Binding,
    size: u64,
    section: u16,
    value: u64,
}

/// One version definition record, read back out of `.gnu.version_d`.
#[derive(Debug, PartialEq, Eq)]
struct Defined {
    /// The node's name, or the library's for the base record.
    name: String,
    /// `vd_ndx`, the number `.gnu.version` entries use to point here.
    index: u16,
    /// Whether `VER_FLG_BASE` is set, which the first record and only the first record has.
    base: bool,
    /// `vd_hash`, which a loader looks a version up by.
    hash: u32,
}

/// One program header.
struct Segment {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

impl<'a> Elf<'a> {
    fn parse(bytes: &'a [u8]) -> Self {
        assert_eq!(&bytes[..4], b"\x7fELF", "not an ELF file at all");
        let wide = match bytes[4] {
            1 => false,
            2 => true,
            other => panic!("EI_CLASS is {other}"),
        };
        let little = match bytes[5] {
            1 => true,
            2 => false,
            other => panic!("EI_DATA is {other}"),
        };
        assert_eq!(bytes[6], 1, "EI_VERSION");
        Elf { bytes, little, wide }
    }

    fn u16(&self, at: usize) -> u16 {
        let raw = [self.bytes[at], self.bytes[at + 1]];
        if self.little { u16::from_le_bytes(raw) } else { u16::from_be_bytes(raw) }
    }

    fn u32(&self, at: usize) -> u32 {
        let raw: [u8; 4] = self.bytes[at..at + 4].try_into().expect("four bytes");
        if self.little { u32::from_le_bytes(raw) } else { u32::from_be_bytes(raw) }
    }

    fn u64(&self, at: usize) -> u64 {
        let raw: [u8; 8] = self.bytes[at..at + 8].try_into().expect("eight bytes");
        if self.little { u64::from_le_bytes(raw) } else { u64::from_be_bytes(raw) }
    }

    /// A word whose width is the file's class, which is every address, offset and size.
    fn word(&self, at: usize) -> u64 {
        if self.wide { self.u64(at) } else { u64::from(self.u32(at)) }
    }

    fn kind(&self) -> u16 {
        self.u16(16)
    }

    fn machine(&self) -> u16 {
        self.u16(18)
    }

    fn flags(&self) -> u32 {
        self.u32(if self.wide { 48 } else { 36 })
    }

    fn os_abi(&self) -> u8 {
        self.bytes[7]
    }

    fn segments(&self) -> Vec<Segment> {
        let at = self.word(if self.wide { 32 } else { 28 }) as usize;
        let size = usize::from(self.u16(if self.wide { 54 } else { 42 }));
        let count = usize::from(self.u16(if self.wide { 56 } else { 44 }));
        assert_eq!(size, if self.wide { 56 } else { 32 }, "e_phentsize");
        (0..count)
            .map(|n| {
                let p = at + n * size;
                if self.wide {
                    Segment {
                        kind: self.u32(p),
                        flags: self.u32(p + 4),
                        offset: self.u64(p + 8),
                        vaddr: self.u64(p + 16),
                        filesz: self.u64(p + 32),
                        memsz: self.u64(p + 40),
                        align: self.u64(p + 48),
                    }
                } else {
                    Segment {
                        kind: self.u32(p),
                        offset: u64::from(self.u32(p + 4)),
                        vaddr: u64::from(self.u32(p + 8)),
                        filesz: u64::from(self.u32(p + 16)),
                        memsz: u64::from(self.u32(p + 20)),
                        flags: self.u32(p + 24),
                        align: u64::from(self.u32(p + 28)),
                    }
                }
            })
            .collect()
    }

    fn sections(&self) -> Vec<Section> {
        let at = self.word(if self.wide { 40 } else { 32 }) as usize;
        let size = usize::from(self.u16(if self.wide { 58 } else { 46 }));
        let count = usize::from(self.u16(if self.wide { 60 } else { 48 }));
        let names = usize::from(self.u16(if self.wide { 62 } else { 50 }));
        assert_eq!(size, if self.wide { 64 } else { 40 }, "e_shentsize");

        let raw = |n: usize| -> (u32, u32, u64, u64, usize, usize, u32, u32, u64) {
            let p = at + n * size;
            if self.wide {
                (
                    self.u32(p),
                    self.u32(p + 4),
                    self.u64(p + 8),
                    self.u64(p + 16),
                    self.u64(p + 24) as usize,
                    self.u64(p + 32) as usize,
                    self.u32(p + 40),
                    self.u32(p + 44),
                    self.u64(p + 56),
                )
            } else {
                (
                    self.u32(p),
                    self.u32(p + 4),
                    u64::from(self.u32(p + 8)),
                    u64::from(self.u32(p + 12)),
                    self.u32(p + 16) as usize,
                    self.u32(p + 20) as usize,
                    self.u32(p + 24),
                    self.u32(p + 28),
                    u64::from(self.u32(p + 36)),
                )
            }
        };

        let shstrtab = raw(names).4;
        (0..count)
            .map(|n| {
                let (name, kind, flags, addr, offset, size, link, info, entsize) = raw(n);
                Section {
                    name: string_at(self.bytes, shstrtab + name as usize),
                    kind,
                    flags,
                    addr,
                    offset,
                    size,
                    link,
                    info,
                    entsize,
                }
            })
            .collect()
    }

    fn section(&self, name: &str) -> Section {
        self.sections()
            .into_iter()
            .find(|section| section.name == name)
            .unwrap_or_else(|| panic!("no {name} section"))
    }

    /// Every symbol but the null one at index zero, which is asserted to be null.
    fn symbols(&self) -> Vec<Read> {
        let dynsym = self.section(".dynsym");
        let dynstr = self.section(".dynstr");
        let size = if self.wide { 24 } else { 16 };
        assert_eq!(dynsym.entsize, size, "sh_entsize of .dynsym");
        assert_eq!(dynsym.size % size as usize, 0, "a partial symbol at the end of .dynsym");

        let read = |n: usize| -> Read {
            let p = dynsym.offset + n * size as usize;
            let (name, info, section, value, length) = if self.wide {
                (self.u32(p), self.bytes[p + 4], self.u16(p + 6), self.u64(p + 8), self.u64(p + 16))
            } else {
                (
                    self.u32(p),
                    self.bytes[p + 12],
                    self.u16(p + 14),
                    u64::from(self.u32(p + 4)),
                    u64::from(self.u32(p + 8)),
                )
            };
            Read {
                name: string_at(self.bytes, dynstr.offset + name as usize),
                kind: match info & 0xf {
                    1 => Kind::Object,
                    2 => Kind::Function,
                    other => panic!("symbol type {other}"),
                },
                binding: match info >> 4 {
                    1 => Binding::Global,
                    2 => Binding::Weak,
                    other => panic!("symbol binding {other}"),
                },
                size: length,
                section,
                value,
            }
        };

        let count = dynsym.size / size as usize;
        let first = dynsym.offset;
        assert!(
            self.bytes[first..first + size as usize].iter().all(|&b| b == 0),
            "symbol zero is not the null entry"
        );
        (1..count).map(read).collect()
    }

    /// `.gnu.version`, one entry per `.dynsym` entry, the null symbol's included.
    ///
    /// Returned raw, flag bit and all, because the flag is the whole difference between `name@NODE`
    /// and `name@@NODE` and a reader that masked it off here would pass whatever the writer did.
    fn version_indices(&self) -> Vec<u16> {
        let versym = self.section(".gnu.version");
        assert_eq!(versym.entsize, 2, "sh_entsize of .gnu.version");
        assert_eq!(
            versym.link,
            self.index_of(".dynsym"),
            "sh_link of .gnu.version is not .dynsym, so nothing says what these index"
        );
        assert_eq!(versym.size, (self.symbols().len() + 1) * 2, "one entry per symbol table entry");
        (0..versym.size / 2).map(|n| self.u16(versym.offset + n * 2)).collect()
    }

    /// `.gnu.version_d`, walked the way a loader walks it, by following `vd_next`.
    ///
    /// The offsets in this section are from the start of the record holding them rather than from
    /// the section, so a walk is the only way to read it and also the only way to catch a writer
    /// that treated them as section offsets. Stepping by a fixed stride instead would agree with
    /// such a writer.
    fn version_definitions(&self) -> Vec<Defined> {
        let verdef = self.section(".gnu.version_d");
        assert_eq!(
            verdef.link,
            self.index_of(".dynstr"),
            "sh_link of .gnu.version_d is not .dynstr, so the names point nowhere"
        );
        let dynstr = self.section(".dynstr");
        let mut found = Vec::new();
        let mut at = verdef.offset;
        loop {
            assert!(at + 20 <= verdef.offset + verdef.size, "a record past the end of the section");
            assert_eq!(self.u16(at), 1, "vd_version");
            let flags = self.u16(at + 2);
            let index = self.u16(at + 4);
            let count = self.u16(at + 6);
            let hash = self.u32(at + 8);
            let aux = self.u32(at + 12) as usize;
            let next = self.u32(at + 16) as usize;
            assert_eq!(count, 1, "vd_cnt, one auxiliary per record here");
            let name = self.u32(at + aux);
            assert_eq!(self.u32(at + aux + 4), 0, "vda_next, the only auxiliary has no next");
            found.push(Defined {
                name: string_at(self.bytes, dynstr.offset + name as usize),
                index,
                base: flags & 1 != 0,
                hash,
            });
            if next == 0 {
                break;
            }
            at += next;
        }
        assert_eq!(
            usize::try_from(verdef.info).expect("a record count"),
            found.len(),
            "sh_info of .gnu.version_d is not the number of records the chain has"
        );
        found
    }

    fn index_of(&self, name: &str) -> u32 {
        let at = self
            .sections()
            .iter()
            .position(|section| section.name == name)
            .unwrap_or_else(|| panic!("no {name} section"));
        u32::try_from(at).expect("a handful of sections")
    }

    /// The `.dynamic` entries, as tag and value pairs in file order.
    fn dynamic(&self) -> Vec<(u64, u64)> {
        let dynamic = self.section(".dynamic");
        let size = if self.wide { 16 } else { 8 };
        assert_eq!(dynamic.entsize, size, "sh_entsize of .dynamic");
        let half = size as usize / 2;
        (0..dynamic.size / size as usize)
            .map(|n| {
                let p = dynamic.offset + n * size as usize;
                (self.word(p), self.word(p + half))
            })
            .collect()
    }

    /// The string a `.dynamic` value points at, for the tags whose value is a string offset.
    fn dynamic_string(&self, value: u64) -> String {
        string_at(self.bytes, self.section(".dynstr").offset + value as usize)
    }

    /// Looks a name up the way a loader would, through the bucket and the chain.
    ///
    /// This is the part of the file that would otherwise go unchecked. A hash table nobody walks is
    /// a few hundred bytes that could say anything at all.
    fn find_through_hash(&self, name: &str) -> Option<Read> {
        let hash = self.section(".hash");
        // Four bytes everywhere but 64-bit s390, where the ABI supplement makes every entry eight,
        // the two counts included. The width is read out of `sh_entsize` rather than assumed,
        // because assuming it here is what let a four byte s390x table pass this test once.
        let entry = hash.entsize as usize;
        assert!(entry == 4 || entry == 8, "a hash entry is {entry} bytes");
        let at = |n: usize| -> usize {
            let p = hash.offset + n * entry;
            if entry == 8 { self.u64(p) as usize } else { self.u32(p) as usize }
        };
        let buckets = at(0);
        let chains = at(1);
        let bucket = |n: usize| at(2 + n);
        let chain = |n: usize| at(2 + buckets + n);
        assert_eq!(hash.size, (2 + buckets + chains) * entry, "the hash table's own size");

        // Symbol zero is the null entry, so it is not in any chain, and the table counts it.
        let symbols = self.symbols();
        assert_eq!(chains, symbols.len() + 1, "nchain is not the symbol count");

        let mut index = bucket(elf_hash(name.as_bytes()) as usize % buckets);
        let mut hops = 0;
        while index != 0 {
            let symbol = &symbols[index - 1];
            if symbol.name == name {
                return Some(symbol.clone_of());
            }
            index = chain(index);
            hops += 1;
            assert!(hops <= chains, "the chain for {name} loops");
        }
        None
    }
}

impl Read {
    /// Clones, because `Read` holds a `String` and the hash walk wants to hand one back.
    fn clone_of(&self) -> Read {
        Read {
            name: self.name.clone(),
            kind: self.kind,
            binding: self.binding,
            size: self.size,
            section: self.section,
            value: self.value,
        }
    }
}

fn string_at(bytes: &[u8], at: usize) -> String {
    let end = bytes[at..].iter().position(|&b| b == 0).expect("a terminated string");
    String::from_utf8(bytes[at..at + end].to_vec()).expect("a name that is text")
}

/// The hash function the SysV ABI defines, written again rather than reused.
fn elf_hash(name: &[u8]) -> u32 {
    let mut h: u32 = 0;
    for &byte in name {
        h = (h << 4).wrapping_add(u32::from(byte));
        let carry = h & 0xf000_0000;
        if carry != 0 {
            h ^= carry >> 24;
        }
        h &= !carry;
    }
    h
}

/// What `.gnu.version` says about every symbol, as the name, the node it is at, and whether an
/// unversioned reference to the name lands on it.
///
/// Built by pairing the two tables by index, which is the only thing that relates them, and the
/// pairing is checked rather than assumed: an entry for a node with no definition is a file a loader
/// rejects.
fn nodes_of(elf: &Elf<'_>) -> Vec<(String, Option<(String, bool)>)> {
    let defined = elf.version_definitions();
    let indices = elf.version_indices();
    assert_eq!(indices[0], 0, "the null symbol is local, so its version index is zero");
    elf.symbols()
        .iter()
        .zip(&indices[1..])
        .map(|(symbol, &raw)| {
            let index = raw & !0x8000;
            let default = raw & 0x8000 == 0;
            let node = match index {
                // VER_NDX_GLOBAL, which is what a symbol with no node carries.
                1 => None,
                _ => {
                    let record =
                        defined.iter().find(|record| record.index == index).unwrap_or_else(|| {
                            panic!("{} is at version {index}, which nothing defines", symbol.name)
                        });
                    Some((record.name.clone(), default))
                }
            };
            (symbol.name.clone(), node)
        })
        .collect()
}

const DT_NEEDED: u64 = 1;
const DT_HASH: u64 = 4;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_STRSZ: u64 = 10;
const DT_SYMENT: u64 = 11;
const DT_SONAME: u64 = 14;
const DT_VERSYM: u64 = 0x6fff_fff0;
const DT_VERDEF: u64 = 0x6fff_fffc;
const DT_VERDEFNUM: u64 = 0x6fff_fffd;

#[test]
fn every_symbol_comes_back_with_its_kind_its_binding_and_its_size() {
    let library = libc();
    let bytes = write(&library, target("x86_64-linux-musl")).expect("a stub");
    let elf = Elf::parse(&bytes);

    let mut want: Vec<(&str, Kind, Binding, u64)> =
        library.symbols.iter().map(|s| (s.name.as_str(), s.kind, s.binding, s.size)).collect();
    want.sort_unstable();
    let mut got: Vec<(&str, Kind, Binding, u64)> = Vec::new();
    let read = elf.symbols();
    for symbol in &read {
        got.push((symbol.name.as_str(), symbol.kind, symbol.binding, symbol.size));
    }
    got.sort_unstable();
    assert_eq!(got, want);
}

#[test]
fn a_symbol_is_a_definition_and_not_a_reference() {
    let bytes = write(&libc(), target("x86_64-linux-musl")).expect("a stub");
    let elf = Elf::parse(&bytes);
    let text =
        elf.sections().iter().position(|section| section.name == ".text").expect("a .text section");
    for symbol in elf.symbols() {
        // SHN_UNDEF is zero and a symbol carrying it is a reference. A stub whose symbols were
        // references would link and satisfy nothing, which is the failure this asserts against.
        assert_ne!(symbol.section, 0, "{} is a reference", symbol.name);
        assert_eq!(usize::from(symbol.section), text, "{} is somewhere else", symbol.name);
        // A stub says what a library exports and nothing about where any of it is.
        assert_eq!(symbol.value, 0, "{} claims an address", symbol.name);
    }
}

#[test]
fn the_soname_and_the_needed_chain_come_back_in_order() {
    let mut library = Library::new("libc.so.6");
    library.needs("ld-linux-x86-64.so.2").needs("libm.so.6").function("printf");
    let bytes = write(&library, target("x86_64-linux-musl")).expect("a stub");
    let elf = Elf::parse(&bytes);

    let entries = elf.dynamic();
    let soname: Vec<String> = entries
        .iter()
        .filter(|(tag, _)| *tag == DT_SONAME)
        .map(|(_, value)| elf.dynamic_string(*value))
        .collect();
    assert_eq!(soname, ["libc.so.6"], "a stub has exactly one SONAME");

    let needed: Vec<String> = entries
        .iter()
        .filter(|(tag, _)| *tag == DT_NEEDED)
        .map(|(_, value)| elf.dynamic_string(*value))
        .collect();
    // The order is the order a loader searches, which is the one property of the chain that is not
    // a set, so it is the one worth asserting rather than the membership.
    assert_eq!(needed, ["ld-linux-x86-64.so.2", "libm.so.6"]);
}

#[test]
fn the_dynamic_section_points_at_the_tables_that_are_really_there() {
    let bytes = write(&libc(), target("x86_64-linux-musl")).expect("a stub");
    let elf = Elf::parse(&bytes);
    let entries = elf.dynamic();
    let value = |tag: u64| {
        entries
            .iter()
            .find(|(held, _)| *held == tag)
            .map(|&(_, value)| value)
            .unwrap_or_else(|| panic!("no entry for tag {tag}"))
    };
    // Each of these is an address in the image, and the image is mapped at offset zero with a
    // vaddr of zero, so an address and an offset are the same number here. That is a property of
    // the layout and checking it is how a layout change that forgets `.dynamic` gets caught.
    assert_eq!(value(DT_HASH), elf.section(".hash").addr);
    assert_eq!(value(DT_STRTAB), elf.section(".dynstr").addr);
    assert_eq!(value(DT_SYMTAB), elf.section(".dynsym").addr);
    assert_eq!(value(DT_STRSZ), elf.section(".dynstr").size as u64);
    assert_eq!(value(DT_SYMENT), 24);
    assert_eq!(entries.last().expect("a terminator"), &(0, 0), "DT_NULL is not last");
}

#[test]
fn every_symbol_is_findable_through_the_hash_table() {
    let library = libc();
    let bytes = write(&library, target("x86_64-linux-musl")).expect("a stub");
    let elf = Elf::parse(&bytes);
    for symbol in &library.symbols {
        let found = elf
            .find_through_hash(&symbol.name)
            .unwrap_or_else(|| panic!("{} is not in the hash table", symbol.name));
        assert_eq!(found.name, symbol.name);
        assert_eq!(found.size, symbol.size);
    }
    assert!(elf.find_through_hash("a_name_nobody_exported").is_none());
}

#[test]
fn the_same_description_in_a_different_order_is_the_same_bytes() {
    let target = target("aarch64-linux-musl");
    let forwards = write(&libc(), target).expect("a stub");

    let mut backwards = Library::new("libc.so");
    let mut symbols = libc().symbols;
    symbols.reverse();
    backwards.needs("ld-musl-x86_64.so.1");
    for symbol in symbols {
        backwards.export(symbol);
    }
    // Claim 5 of spec/cross-compile/02-the-goal.md, as the only thing about it this crate can
    // check on one host: what reaches the bytes is the description and not the order somebody
    // assembled it in. The caller that will eventually assemble one is a decoder reading a blob.
    assert_eq!(write(&backwards, target).expect("a stub"), forwards);
}

#[test]
fn writing_the_same_thing_twice_gives_the_same_bytes() {
    let target = target("riscv64-linux-gnu");
    assert_eq!(write(&libc(), target).expect("a stub"), write(&libc(), target).expect("a stub"));
}

#[test]
fn a_32_bit_target_gets_a_32_bit_file() {
    let bytes = write(&libc(), target("i686-linux-gnu")).expect("a stub");
    let elf = Elf::parse(&bytes);
    assert!(!elf.wide);
    assert_eq!(elf.machine(), 3, "EM_386");
    assert_eq!(elf.section(".dynsym").entsize, 16);
    assert_eq!(elf.section(".dynamic").entsize, 8);
    // The reader above uses the 32-bit field orders throughout, which are different from the
    // 64-bit ones in the symbol table and in the program header, so this reaching the same answer
    // as the 64-bit test is the thing being checked and not the class byte.
    let mut names: Vec<String> = elf.symbols().into_iter().map(|s| s.name).collect();
    names.sort();
    assert_eq!(names, ["__progname", "environ", "malloc", "printf", "pthread_cancel", "stdout"]);
}

#[test]
fn a_big_endian_target_gets_a_big_endian_file() {
    let bytes = write(&libc(), target("s390x-linux-gnu")).expect("a stub");
    let elf = Elf::parse(&bytes);
    assert!(!elf.little);
    assert_eq!(elf.machine(), 22, "EM_S390");
    assert_eq!(elf.kind(), 3, "ET_DYN");
    let environ = elf.find_through_hash("environ").expect("a symbol");
    assert_eq!(environ.size, 8);
    assert_eq!(environ.kind, Kind::Object);
}

#[test]
fn s390x_gets_the_eight_byte_hash_entries_its_abi_asks_for() {
    // The one field width in the file that is not a reading of the ELF class. binutils carries it
    // per architecture as `sizeof_hash_entry` rather than deriving it, and `llvm-readelf` declines
    // to parse an s390 hash table at all rather than assume the common width. Four bytes here is a
    // table the loader walks at the wrong stride, so every lookup misses, at load time, with
    // nothing said during the link.
    let bytes = write(&libc(), target("s390x-linux-gnu")).expect("a stub");
    let elf = Elf::parse(&bytes);
    let hash = elf.section(".hash");
    assert_eq!(hash.entsize, 8);
    // Seven entries of eight: two counts, two buckets for six symbols, six chain slots. The size
    // is asserted rather than left to the walk, because a walk reads only the entries it needs.
    assert_eq!(hash.size, (2 + 2 + 7) * 8);
    assert!(elf.find_through_hash("pthread_cancel").is_some());

    // Every other architecture keeps four, so the exception stays an exception.
    for spelling in ["x86_64-linux-gnu", "aarch64-linux-musl", "i686-linux-gnu"] {
        let bytes = write(&libc(), target(spelling)).expect("a stub");
        assert_eq!(Elf::parse(&bytes).section(".hash").entsize, 4, "{spelling}");
    }
}

#[test]
fn the_machine_flags_are_the_targets_and_not_a_zero() {
    let flags = |spelling: &str| {
        let bytes = write(&libc(), target(spelling)).expect("a stub");
        Elf::parse(&bytes).flags()
    };
    // x86-64, i386, AArch64 and s390x define no processor specific flags, so zero is the right
    // answer rather than an absent one.
    assert_eq!(flags("x86_64-linux-musl"), 0);
    assert_eq!(flags("i686-linux-gnu"), 0);
    assert_eq!(flags("aarch64-linux-gnu"), 0);
    assert_eq!(flags("s390x-linux-gnu"), 0);
    // EF_RISCV_FLOAT_ABI_DOUBLE, which the linker checks against every other input, and no
    // compressed instruction bit because a stub has no instructions.
    assert_eq!(flags("riscv64-linux-gnu"), 0x4);
    // EF_ARM_EABI_VER5 with the hard float bit, which is what every `eabihf` row is.
    assert_eq!(flags("armv7-linux-gnueabihf"), 0x0500_0400);
    // EF_PPC64_ABI of 2, which is ELFv2 and is what little endian PowerPC is.
    assert_eq!(flags("powerpc64le-linux-gnu"), 2);
}

#[test]
fn loongarch_is_refused_rather_than_given_a_flags_word_from_memory() {
    // The base ABI modifier and the object file ABI version share that word, and guessing it for
    // an architecture with no back end is how a stub ends up failing to link for a reason that
    // points nowhere near the stub. `spec/cross-compile/06-abis.md` declines LoongArch for its own
    // reasons and this is the same judgement in a second place.
    assert!(write(&libc(), target("loongarch64-linux-gnu")).is_err());
}

#[test]
fn a_target_that_does_not_write_elf_is_refused() {
    // Darwin is not a gap. Apple ships `.tbd` files, which are this document's own technique
    // adopted by the platform vendor, so section 9.7 consumes them rather than generating
    // anything. Windows wants import libraries, which are section 9.4 and a different container.
    for spelling in ["aarch64-macos", "x86_64-macos", "x86_64-windows-gnu", "wasm32-wasi"] {
        let said = write(&libc(), target(spelling)).expect_err("not ELF");
        assert!(said.to_string().contains("rather than ELF"), "{said}");
    }
}

#[test]
fn a_description_that_says_two_things_about_one_name_is_refused() {
    let mut library = Library::new("libc.so");
    library.function("printf").object("printf", 8);
    let said = write(&library, target("x86_64-linux-musl")).expect_err("a duplicate");
    assert!(said.to_string().contains("described twice"), "{said}");
}

#[test]
fn a_size_that_disagrees_with_a_kind_is_refused() {
    let mut function_with_a_size = Library::new("libc.so");
    function_with_a_size.export(Symbol { size: 4, ..Symbol::function("printf") });
    assert!(write(&function_with_a_size, target("x86_64-linux-musl")).is_err());

    // An object of no size is the quiet half of section 9.1's table seen from the other end: the
    // copy relocation against it copies nothing, and the program reads whatever was in its own
    // storage.
    let mut object_without_one = Library::new("libc.so");
    object_without_one.export(Symbol { size: 0, ..Symbol::object("environ", 8) });
    assert!(write(&object_without_one, target("x86_64-linux-musl")).is_err());
}

#[test]
fn a_library_with_no_soname_is_refused() {
    let library = Library::new("");
    let said = write(&library, target("x86_64-linux-musl")).expect_err("no SONAME");
    assert!(said.to_string().contains("no SONAME"), "{said}");
}

#[test]
fn a_name_a_string_table_cannot_hold_is_refused() {
    let mut library = Library::new("libc.so");
    library.function("pri\0ntf");
    assert!(write(&library, target("x86_64-linux-musl")).is_err());

    let mut nameless = Library::new("libc.so");
    nameless.function("");
    assert!(write(&nameless, target("x86_64-linux-musl")).is_err());
}

#[test]
fn a_library_that_exports_nothing_is_still_a_library() {
    // Section 9.9. On glibc 2.34 and later `libpthread`, `libdl`, `libutil` and `libanl` are inside
    // `libc.so.6`, and the separate files are kept as stubs that export nothing. Build systems pass
    // `-lpthread` whether or not anything is there and a missing file is a link error, so the empty
    // one has to be a real shared object rather than an absence. `libm` is not one of them and is
    // deliberately not the example here, because an empty `libm` would be a link failure for any
    // program that does arithmetic.
    let bytes =
        write(&Library::new("libpthread.so.0"), target("x86_64-linux-gnu")).expect("a stub");
    let elf = Elf::parse(&bytes);
    assert_eq!(elf.kind(), 3, "ET_DYN");
    assert!(elf.symbols().is_empty());
    let soname = elf
        .dynamic()
        .into_iter()
        .find(|(tag, _)| *tag == DT_SONAME)
        .map(|(_, value)| elf.dynamic_string(value));
    assert_eq!(soname.as_deref(), Some("libpthread.so.0"));
    // An empty hash table still has to be walkable, because the walk is what a loader does before
    // it knows the table is empty.
    assert!(elf.find_through_hash("pthread_create").is_none());
}

#[test]
fn the_sections_say_what_they_are_and_point_at_each_other() {
    let bytes = write(&libc(), target("x86_64-linux-musl")).expect("a stub");
    let elf = Elf::parse(&bytes);
    let sections = elf.sections();
    let names: Vec<&str> = sections.iter().map(|section| section.name.as_str()).collect();
    assert_eq!(names, ["", ".text", ".hash", ".dynsym", ".dynstr", ".dynamic", ".shstrtab"]);

    let index = |name: &str| names.iter().position(|held| *held == name).expect("a section") as u32;
    // `sh_link` of a symbol table is its string table, and of a hash table is the symbol table it
    // hashes. Getting either wrong makes a reader follow an offset into the wrong section and
    // report names that are not names.
    assert_eq!(elf.section(".dynsym").link, index(".dynstr"));
    assert_eq!(elf.section(".hash").link, index(".dynsym"));
    assert_eq!(elf.section(".dynamic").link, index(".dynstr"));
    // `sh_info` of a symbol table is the index of its first non-local symbol. Only the null entry
    // is local here.
    assert_eq!(elf.section(".dynsym").info, 1);

    // SHF_ALLOC on everything that is part of the image and on nothing that is not. `.shstrtab`
    // and the section headers exist for something reading the file, and nothing reads this file at
    // run time, because section 9.1's whole point is that the real library is what gets loaded.
    for name in [".text", ".hash", ".dynsym", ".dynstr", ".dynamic"] {
        assert_eq!(elf.section(name).flags & 0x2, 0x2, "{name} is not allocated");
    }
    assert_eq!(elf.section(".shstrtab").flags, 0);
    assert_eq!(elf.section(".shstrtab").addr, 0);
    // `.text` is `SHT_NOBITS` and empty, because there is no code.
    assert_eq!(elf.section(".text").kind, 8);
    assert_eq!(elf.section(".text").size, 0);
}

#[test]
fn the_segments_cover_what_is_allocated_and_stop_there() {
    let bytes = write(&libc(), target("x86_64-linux-musl")).expect("a stub");
    let elf = Elf::parse(&bytes);
    let segments = elf.segments();
    assert_eq!(segments.len(), 2);

    let load = segments.iter().find(|segment| segment.kind == 1).expect("a PT_LOAD");
    assert_eq!(load.offset, 0);
    assert_eq!(load.vaddr, 0);
    assert_eq!(load.filesz, load.memsz, "a stub has nothing that is not in the file");
    // The mapped part ends where `.dynamic` does, and `.shstrtab` and the section headers are
    // after it. An offset and an address are the same number here, which is what makes this
    // checkable at all.
    let dynamic = elf.section(".dynamic");
    assert_eq!(load.filesz, dynamic.addr + dynamic.size as u64);
    assert!(load.filesz < bytes.len() as u64, "the segment covers the section headers too");
    // `p_vaddr` and `p_offset` have to agree modulo `p_align`, which they do because both are
    // zero, and that is the one thing about the alignment that has to be true.
    assert_eq!(load.vaddr % load.align, load.offset % load.align);
    assert_eq!(load.flags, 4, "PF_R");

    let dyn_segment = segments.iter().find(|segment| segment.kind == 2).expect("a PT_DYNAMIC");
    assert_eq!(dyn_segment.offset, dynamic.offset as u64);
    assert_eq!(dyn_segment.vaddr, dynamic.addr);
    assert_eq!(dyn_segment.filesz, dynamic.size as u64);
}

#[test]
fn every_symbol_comes_back_at_the_node_it_was_described_at() {
    let bytes = write(&glibc(), target("x86_64-linux-gnu")).expect("a stub");
    let elf = Elf::parse(&bytes);
    let at = |name: &str| -> Vec<Option<(String, bool)>> {
        nodes_of(&elf).into_iter().filter(|(held, _)| held == name).map(|(_, node)| node).collect()
    };
    assert_eq!(at("printf"), [Some(("GLIBC_2.2.5".to_string(), true))]);
    assert_eq!(at("environ"), [Some(("GLIBC_2.2.5".to_string(), true))]);
    assert_eq!(at("pthread_cancel"), [Some(("GLIBC_2.34".to_string(), true))]);
    // A symbol with no node is not an error and not a gap. It gets VER_NDX_GLOBAL, which is the same
    // number as the base definition, and that is the arrangement every real glibc has.
    assert_eq!(at("gettimeofday"), [None]);
}

#[test]
fn the_default_definition_and_the_superseded_one_are_told_apart() {
    let bytes = write(&glibc(), target("x86_64-linux-gnu")).expect("a stub");
    let elf = Elf::parse(&bytes);
    // Both definitions of `memcpy` are exported, and exactly one of them is what an unversioned
    // reference binds to. Without the hidden bit this file would say `memcpy` twice and mean it,
    // which is a library no linker can resolve a plain `memcpy` against.
    let memcpy: Vec<Option<(String, bool)>> =
        nodes_of(&elf).into_iter().filter(|(name, _)| name == "memcpy").map(|(_, at)| at).collect();
    assert_eq!(
        memcpy,
        [Some(("GLIBC_2.14".to_string(), true)), Some(("GLIBC_2.2.5".to_string(), false)),],
        "the two definitions of memcpy, the newer one taking unversioned references"
    );
}

#[test]
fn the_version_definitions_name_the_library_first_and_then_its_nodes() {
    let bytes = write(&glibc(), target("x86_64-linux-gnu")).expect("a stub");
    let elf = Elf::parse(&bytes);
    let defined = elf.version_definitions();
    let names: Vec<&str> = defined.iter().map(|record| record.name.as_str()).collect();
    // The base record names the library, not a node, and the nodes come after it in version order.
    // Nothing in the format requires that order, and section 9.8's best test is somebody diffing
    // this against a real libc, so it is the order a real libc has.
    assert_eq!(names, ["libc.so.6", "GLIBC_2.2.5", "GLIBC_2.14", "GLIBC_2.34"]);
    assert!(defined[0].base, "the first record is the base");
    assert!(defined[1..].iter().all(|record| !record.base), "only the base is the base");
    // Indices count from 1, and every node is reachable by the number a `.gnu.version` entry holds.
    let indices: Vec<u16> = defined.iter().map(|record| record.index).collect();
    assert_eq!(indices, [1, 2, 3, 4]);
    // `vd_hash` is a hash of the record's own name. Hashing the SONAME into a node's record, or the
    // other way round, produces a file every reader prints correctly and a loader cannot look a
    // version up in.
    for record in &defined {
        assert_eq!(record.hash, elf_hash(record.name.as_bytes()), "vd_hash of {}", record.name);
    }
}

#[test]
fn the_dynamic_section_points_at_the_version_tables() {
    let bytes = write(&glibc(), target("x86_64-linux-gnu")).expect("a stub");
    let elf = Elf::parse(&bytes);
    let entries = elf.dynamic();
    let value = |tag: u64| entries.iter().find(|(held, _)| *held == tag).map(|&(_, value)| value);
    // The addresses are what a loader uses, and the section headers are not in the image, so these
    // have to agree with the sections or the tables are only there for readers.
    assert_eq!(value(DT_VERSYM), Some(elf.section(".gnu.version").addr));
    assert_eq!(value(DT_VERDEF), Some(elf.section(".gnu.version_d").addr));
    assert_eq!(value(DT_VERDEFNUM), Some(elf.version_definitions().len() as u64));
}

#[test]
fn a_library_with_no_version_nodes_has_no_version_tables() {
    // musl has no symbol versioning at all. An empty `.gnu.version` would be a claim that this
    // library is versioned and happens to have nothing in it, which is not what musl looks like and
    // invites a reader to believe it.
    let bytes = write(&libc(), target("x86_64-linux-musl")).expect("a stub");
    let elf = Elf::parse(&bytes);
    let names: Vec<String> = elf.sections().into_iter().map(|section| section.name).collect();
    assert!(!names.iter().any(|name| name.starts_with(".gnu.version")), "{names:?}");
    for tag in [DT_VERSYM, DT_VERDEF, DT_VERDEFNUM] {
        assert!(!elf.dynamic().iter().any(|(held, _)| *held == tag), "tag {tag:#x} is there");
    }
}

#[test]
fn the_same_versioned_description_in_a_different_order_is_the_same_bytes() {
    let mut shuffled = Library::new("libc.so.6");
    shuffled
        .needs("ld-linux-x86-64.so.2")
        .function("gettimeofday")
        .export(Symbol::function("pthread_cancel").weak().at("GLIBC_2.34"))
        .export(Symbol::object("environ", 8).at("GLIBC_2.2.5"))
        .export(Symbol::function("memcpy").behind("GLIBC_2.2.5"))
        .export(Symbol::function("printf").at("GLIBC_2.2.5"))
        .export(Symbol::function("memcpy").at("GLIBC_2.14"));
    let target = target("x86_64-linux-gnu");
    assert_eq!(
        write(&shuffled, target).expect("a stub"),
        write(&glibc(), target).expect("a stub"),
        "the order the nodes were described in reached the bytes"
    );
}

#[test]
fn a_name_with_two_default_definitions_is_refused() {
    // Both of these answer a plain `memcpy`, so the description does not say which one does. A
    // linker given this file picks one and the program gets whichever it picked.
    let mut library = Library::new("libc.so.6");
    library
        .export(Symbol::function("memcpy").at("GLIBC_2.2.5"))
        .export(Symbol::function("memcpy").at("GLIBC_2.14"));
    let said = write(&library, target("x86_64-linux-gnu")).expect_err("two defaults");
    assert!(said.to_string().contains("two answers"), "{said}");
    assert!(said.to_string().contains("GLIBC_2.2.5"), "{said}");

    // Three definitions with a superseded one in the middle is the case an adjacent pair check
    // misses, because no two neighbours are both defaults.
    let mut three = Library::new("libc.so.6");
    three
        .export(Symbol::function("memcpy").at("GLIBC_2.2.5"))
        .export(Symbol::function("memcpy").behind("GLIBC_2.14"))
        .export(Symbol::function("memcpy").at("GLIBC_2.34"));
    assert!(write(&three, target("x86_64-linux-gnu")).is_err(), "a default on either side of one");
}

#[test]
fn a_name_both_versioned_and_not_is_refused() {
    // The unversioned definition would answer every reference the versioned one exists to answer.
    let mut library = Library::new("libc.so.6");
    library.function("memcpy").export(Symbol::function("memcpy").at("GLIBC_2.14"));
    let said = write(&library, target("x86_64-linux-gnu")).expect_err("mixed");
    assert!(said.to_string().contains("both with a version node and without"), "{said}");
}

#[test]
fn one_name_at_one_node_twice_is_still_a_duplicate() {
    let mut library = Library::new("libc.so.6");
    library
        .export(Symbol::function("memcpy").at("GLIBC_2.14"))
        .export(Symbol::function("memcpy").behind("GLIBC_2.14"));
    let said = write(&library, target("x86_64-linux-gnu")).expect_err("a duplicate");
    assert!(said.to_string().contains("described twice"), "{said}");
}

#[test]
fn a_version_node_with_no_name_is_refused() {
    let mut library = Library::new("libc.so.6");
    library.export(Symbol::function("memcpy").at(""));
    let said = write(&library, target("x86_64-linux-gnu")).expect_err("a nameless node");
    assert!(said.to_string().contains("has no name"), "{said}");
}

#[test]
fn the_version_tables_are_in_the_image_and_in_the_right_place() {
    let bytes = write(&glibc(), target("x86_64-linux-gnu")).expect("a stub");
    let elf = Elf::parse(&bytes);
    for name in [".gnu.version", ".gnu.version_d"] {
        let section = elf.section(name);
        assert_eq!(section.flags & 0x2, 0x2, "{name} is not allocated");
        assert_eq!(section.addr, section.offset as u64, "{name} is not where its address says");
    }
    assert_eq!(elf.section(".gnu.version").kind, 0x6fff_ffff, "SHT_GNU_VERSYM");
    assert_eq!(elf.section(".gnu.version_d").kind, 0x6fff_fffd, "SHT_GNU_VERDEF");
    // The records are a chain of varying length, so there is no entry size to report, and a reader
    // that believed a stride taken from here would walk into the middle of a record.
    assert_eq!(elf.section(".gnu.version_d").entsize, 0, "sh_entsize of a chain");
    // Both tables are inside the one PT_LOAD, because a loader reaches them through DT_VERSYM and
    // DT_VERDEF and an address outside the mapped image is not an address.
    let load = elf.segments().into_iter().find(|segment| segment.kind == 1).expect("a PT_LOAD");
    let verdef = elf.section(".gnu.version_d");
    assert!(verdef.addr + verdef.size as u64 <= load.filesz, "the version tables are not mapped");
}

#[test]
fn the_os_is_in_the_identification_where_a_kernel_looks_for_it() {
    let os_abi = |spelling: &str| {
        let bytes = write(&libc(), target(spelling)).expect("a stub");
        Elf::parse(&bytes).os_abi()
    };
    // ELFOSABI_NONE, which is what a Linux binary carries. Linux has a number of its own and
    // almost nothing uses it.
    assert_eq!(os_abi("x86_64-linux-gnu"), 0);
    assert_eq!(os_abi("x86_64-linux-musl"), 0);
}
