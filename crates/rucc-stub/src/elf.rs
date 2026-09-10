//! The ELF shared object writer.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` sections 9.1 and 9.8.
//!
//! # What a stub is made of
//!
//! Six sections and two program headers, and no code. `.dynsym` and `.dynstr` are the names, their
//! types, their bindings and their sizes. `.hash` is how a loader would find them. `.dynamic`
//! carries the `SONAME` and the `DT_NEEDED` chain, which is the part a program records about the
//! library it was linked against. `.text` is allocated, empty, and exists for one reason: a symbol
//! whose `st_shndx` is `SHN_UNDEF` is a reference rather than a definition, and a stub that
//! referenced every name instead of defining it would satisfy nothing.
//!
//! `.shstrtab` and the section headers are not in the mapped image, because nothing loads this
//! file. Section 9.1 is explicit that at run time the program loads the real library, and the stub
//! exists only for the duration of a link.
//!
//! # Determinism
//!
//! Claim 5 of `spec/cross-compile/02-the-goal.md` is byte identical output from different hosts, so
//! nothing in here may depend on anything but the description and the tuple. The symbols are
//! sorted by name before anything is laid out, the string table is built in one fixed order, and
//! there is no hash map anywhere in the file. Section 9.8 names hash map iteration order as the
//! easy way to violate this, and the way to not violate it is to have none.

use rucc_tuple::{Abi, Arch, Endian, ObjectFormat, Os, TargetTuple};

use crate::describe::{Binding, Error, Kind, Library, Symbol};

/// Section indices, which are fixed because the set of sections is fixed.
mod section {
    pub(super) const TEXT: u16 = 1;
    pub(super) const HASH: u16 = 2;
    pub(super) const DYNSYM: u16 = 3;
    pub(super) const DYNSTR: u16 = 4;
    pub(super) const DYNAMIC: u16 = 5;
    pub(super) const SHSTRTAB: u16 = 6;
    pub(super) const COUNT: u16 = 7;
}

const SHT_STRTAB: u32 = 3;
const SHT_HASH: u32 = 5;
const SHT_DYNAMIC: u32 = 6;
const SHT_NOBITS: u32 = 8;
const SHT_DYNSYM: u32 = 11;

const SHF_ALLOC: u64 = 0x2;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PF_R: u32 = 4;

const STB_GLOBAL: u8 = 1;
const STB_WEAK: u8 = 2;
const STT_OBJECT: u8 = 1;
const STT_FUNC: u8 = 2;

const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_HASH: u64 = 4;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_STRSZ: u64 = 10;
const DT_SYMENT: u64 = 11;
const DT_SONAME: u64 = 14;

/// The page size the single `PT_LOAD` claims to be aligned to.
///
/// Nothing maps this file, so the only thing this has to be is consistent with `p_vaddr` and
/// `p_offset` agreeing modulo it, which they do because both are zero. Four kilobytes is the
/// smallest page any supported architecture has, so it is also the claim that stays true if
/// something ever does map it.
const PAGE: u64 = 0x1000;

/// Turns a description into a stub shared object.
///
/// # Errors
///
/// Every failure is the description or the target being wrong rather than the writing going wrong,
/// and all of them are found before the first byte, so there is no partial result to clean up. See
/// [`Error`].
pub fn write(library: &Library, target: TargetTuple) -> Result<Vec<u8>, Error> {
    if target.object_format() != ObjectFormat::Elf {
        return Err(Error::NotElf {
            target: target.to_canonical_string(),
            format: target.object_format().as_str(),
        });
    }
    if library.soname.is_empty() {
        return Err(Error::NoSoname);
    }
    let machine =
        machine(target.arch()).ok_or(Error::NoMachineFlags { arch: arch_name(target) })?;
    let flags = machine_flags(target)?;

    let symbols = ordered(library)?;
    let wide = target.pointer_width() == 64;
    let little = target.endian() == Endian::Little;

    let strings = Strings::build(library, &symbols);
    let plan = Plan::lay_out(
        wide,
        hash_entry(target.arch()),
        symbols.len(),
        library.needed.len(),
        strings.bytes.len(),
    );

    let mut out = Out { bytes: Vec::with_capacity(plan.file_size), wide, little };
    header(&mut out, &plan, target, machine, flags);
    program_headers(&mut out, &plan);
    out.pad_to(plan.hash);
    hash_table(&mut out, &symbols, plan.hash_entry);
    out.pad_to(plan.dynsym);
    symbol_table(&mut out, &symbols, &strings);
    out.pad_to(plan.dynstr);
    out.raw(&strings.bytes);
    out.pad_to(plan.dynamic);
    dynamic(&mut out, library, &plan, &strings);
    out.pad_to(plan.shstrtab);
    out.raw(SHSTRTAB);
    out.pad_to(plan.shdr);
    section_headers(&mut out, &plan);

    debug_assert_eq!(out.bytes.len(), plan.file_size, "the plan and the writer disagree");
    Ok(out.bytes)
}

/// The symbols, sorted and checked.
///
/// Sorting here rather than asking the caller to is what keeps the order a description was
/// assembled in out of the bytes. The duplicate check comes free once they are sorted, which is
/// the second reason to do it in one place.
fn ordered(library: &Library) -> Result<Vec<&Symbol>, Error> {
    for name in std::iter::once(&library.soname).chain(&library.needed) {
        if name.contains('\0') {
            return Err(Error::NameHasNul { name: name.clone() });
        }
    }
    let mut symbols: Vec<&Symbol> = library.symbols.iter().collect();
    symbols.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    for (at, symbol) in symbols.iter().enumerate() {
        if symbol.name.is_empty() {
            return Err(Error::NameIsEmpty);
        }
        if symbol.name.contains('\0') {
            return Err(Error::NameHasNul { name: symbol.name.clone() });
        }
        if at > 0 && symbols[at - 1].name == symbol.name {
            return Err(Error::Duplicate { name: symbol.name.clone() });
        }
        match symbol.kind {
            Kind::Function if symbol.size != 0 => {
                return Err(Error::SizeDisagrees {
                    name: symbol.name.clone(),
                    because: "is a function and was given a size",
                });
            }
            Kind::Object if symbol.size == 0 => {
                return Err(Error::SizeDisagrees {
                    name: symbol.name.clone(),
                    because: "is an object of no size, which a copy relocation cannot copy",
                });
            }
            _ => {}
        }
    }
    Ok(symbols)
}

/// `e_machine`, or [`None`] for an architecture this writer has no number for.
fn machine(arch: Arch) -> Option<u16> {
    Some(match arch {
        Arch::X86 => 3,
        Arch::PowerPc64 => 21,
        Arch::S390x => 22,
        Arch::Arm => 40,
        Arch::X86_64 => 62,
        Arch::Aarch64 => 183,
        Arch::Riscv32 | Arch::Riscv64 => 243,
        Arch::LoongArch64 => 258,
        // Neither is ELF, so `write` has already refused by the time this is reached. Listing
        // them keeps the match exhaustive without a catch-all that would swallow a new
        // architecture silently.
        Arch::Wasm32 | Arch::Arm64Ec => return None,
    })
}

/// The name to put in a diagnostic about an architecture.
fn arch_name(target: TargetTuple) -> &'static str {
    match target.arch() {
        Arch::X86 => "i386",
        Arch::X86_64 => "x86-64",
        Arch::Aarch64 => "aarch64",
        Arch::Arm => "arm",
        Arch::Arm64Ec => "arm64ec",
        Arch::Riscv32 => "riscv32",
        Arch::Riscv64 => "riscv64",
        Arch::LoongArch64 => "loongarch64",
        Arch::PowerPc64 => "powerpc64",
        Arch::S390x => "s390x",
        Arch::Wasm32 => "wasm32",
    }
}

/// `e_flags`, which is processor specific and is where a wrong answer is expensive.
///
/// The linker compares this field between inputs and refuses the ones that disagree, so a zero
/// guessed for an architecture that uses the field produces a link failure whose message is about
/// an ABI mismatch and points nowhere near the stub. Where the right value is not a reading of the
/// tuple, this answers [`Error::NoMachineFlags`] instead, which is a refusal at the point the stub
/// was asked for.
fn machine_flags(target: TargetTuple) -> Result<u32, Error> {
    // EF_RISCV_FLOAT_ABI, two bits, and nothing else. The compressed instruction bit is off
    // because a stub has no instructions at all, which is a statement about this file rather than
    // a claim about the target.
    const RISCV_FLOAT_SINGLE: u32 = 0x2;
    const RISCV_FLOAT_DOUBLE: u32 = 0x4;
    // EF_ARM_EABI_VER5 in the top byte, and the float ABI in the bottom. `softfp` is ABI
    // compatible with soft float, which is the whole point of it, so it takes the same bit.
    const ARM_EABI_VER5: u32 = 0x0500_0000;
    const ARM_FLOAT_SOFT: u32 = 0x200;
    const ARM_FLOAT_HARD: u32 = 0x400;
    // EF_PPC64_ABI, where 2 is ELFv2.
    const PPC64_ELFV2: u32 = 2;

    let abi = target.resolved_abi();
    Ok(match target.arch() {
        // None of these four uses the field. x86, x86-64 and s390x have no processor specific
        // flags, and the AArch64 ELF supplement reserves the whole word and defines no bit in it.
        Arch::X86 | Arch::X86_64 | Arch::Aarch64 | Arch::S390x => 0,
        Arch::Riscv32 | Arch::Riscv64 => match abi {
            Abi::DoubleFloat => RISCV_FLOAT_DOUBLE,
            Abi::SingleFloat => RISCV_FLOAT_SINGLE,
            Abi::SoftFloat | Abi::SoftFp | Abi::Default => 0,
        },
        Arch::Arm => {
            ARM_EABI_VER5
                | match abi {
                    Abi::DoubleFloat => ARM_FLOAT_HARD,
                    _ => ARM_FLOAT_SOFT,
                }
        }
        // ELFv2 is the little endian ABI and the only one in the target matrix. Big endian
        // PowerPC is ELFv1 on most distributions and ELFv2 on a few, and which one a given
        // sysroot wants is a fact about that distribution rather than about the tuple, so there
        // is nothing here to read it off.
        Arch::PowerPc64 if target.endian() == Endian::Little => PPC64_ELFV2,
        // The base ABI modifier and the object file ABI version both live in this word, and the
        // value current binutils produces for `lp64d` is not something worth reproducing from
        // memory for an architecture with no back end. `spec/cross-compile/06-abis.md` declines
        // LoongArch for its own reasons and this is the same judgement twice.
        Arch::LoongArch64 | Arch::PowerPc64 => {
            return Err(Error::NoMachineFlags { arch: arch_name(target) });
        }
        Arch::Wasm32 | Arch::Arm64Ec => {
            return Err(Error::NotElf {
                target: target.to_canonical_string(),
                format: target.object_format().as_str(),
            });
        }
    })
}

/// `EI_OSABI`, which the kernel and the linker both read.
fn os_abi(os: Os) -> u8 {
    match os {
        // ELFOSABI_NONE, which is what Linux binaries carry. Linux has a number of its own and
        // almost nothing uses it.
        Os::Linux | Os::None | Os::Wasi | Os::Illumos => 0,
        Os::NetBsd => 2,
        Os::FreeBsd => 9,
        Os::OpenBsd => 12,
        // Neither writes ELF, and `write` has refused before this is reached.
        Os::MacOs | Os::IOs | Os::Windows => 0,
    }
}

/// Where everything goes, computed before anything is written.
struct Plan {
    wide: bool,
    hash: usize,
    hash_entry: usize,
    hash_size: usize,
    dynsym: usize,
    dynsym_size: usize,
    dynstr: usize,
    dynstr_size: usize,
    dynamic: usize,
    dynamic_size: usize,
    shstrtab: usize,
    shdr: usize,
    file_size: usize,
    /// The end of the part a `PT_LOAD` covers, which stops at `.dynamic` because nothing after it
    /// is allocated.
    mapped: usize,
}

impl Plan {
    fn lay_out(
        wide: bool,
        hash_entry: usize,
        symbols: usize,
        needed: usize,
        strings: usize,
    ) -> Self {
        let (ehdr, phdr, shdr_size, sym, dyn_size) =
            if wide { (64, 56, 64, 24, 16) } else { (52, 32, 40, 16, 8) };
        // One more than the described count, for the null symbol at index zero that every
        // symbol table starts with.
        let entries = symbols + 1;
        let hash_size = (2 + buckets(entries) + entries) * hash_entry;
        let dynsym_size = entries * sym;
        // The needed chain, the SONAME, the four pointers into the two tables, the two sizes and
        // the terminator.
        let dynamic_size = (needed + 7) * dyn_size;
        let align = if wide { 8 } else { 4 };

        let hash = round_up(ehdr + 2 * phdr, align);
        let dynsym = round_up(hash + hash_size, align);
        let dynstr = dynsym + dynsym_size;
        let dynamic = round_up(dynstr + strings, align);
        let mapped = dynamic + dynamic_size;
        let shstrtab = mapped;
        let shdr = round_up(shstrtab + SHSTRTAB.len(), align);
        Plan {
            wide,
            hash,
            hash_entry,
            hash_size,
            dynsym,
            dynsym_size,
            dynstr,
            dynstr_size: strings,
            dynamic,
            dynamic_size,
            shstrtab,
            shdr,
            file_size: shdr + usize::from(section::COUNT) * shdr_size,
            mapped,
        }
    }
}

/// How many buckets the hash table gets.
///
/// Any count produces a correct table, because a bucket is only ever a starting point for a walk
/// down a chain. One per four symbols is roughly the density GNU ld aims for and keeps the chains
/// short enough that `readelf` output is readable.
fn buckets(entries: usize) -> usize {
    entries.div_ceil(4).max(1)
}

/// How wide one entry of `.hash` is, including its two count words.
///
/// Four bytes everywhere except 64-bit s390, where the ABI supplement makes them eight. This is the
/// one field width in the file that is not a reading of the ELF class, which is exactly why it is
/// easy to miss: binutils carries it per architecture as `sizeof_hash_entry` rather than deriving
/// it, and `llvm-readelf` refuses to parse an s390 hash table at all rather than assume the common
/// width. Writing four byte entries there gives a table the loader walks at the wrong stride, so
/// every lookup misses, and it misses at load time with nothing said during the link.
///
/// It was a real reader that found this and not the round trip test, which had the same wrong idea
/// as the writer and so agreed with it. That is the limit section 9.8 names and the reason the
/// comparison against a real library is the property it calls highest value.
fn hash_entry(arch: Arch) -> usize {
    match arch {
        Arch::S390x => 8,
        _ => 4,
    }
}

fn round_up(value: usize, align: usize) -> usize {
    value.next_multiple_of(align)
}

/// The dynamic string table, and where each name went into it.
struct Strings {
    bytes: Vec<u8>,
    soname: u32,
    needed: Vec<u32>,
    /// One offset per symbol, in the order the symbols were given, which is sorted.
    symbols: Vec<u32>,
}

impl Strings {
    /// Builds the table in one fixed order, with names that repeat stored once.
    ///
    /// The repeats are worth the trouble because they are cheap to find. The `SONAME` and the
    /// needed chain are a handful of names, the symbols are already sorted and already known to be
    /// unique among themselves, so the only collisions possible are between a symbol and one of
    /// that handful. That is a scan over a short list per symbol and not a map.
    fn build(library: &Library, symbols: &[&Symbol]) -> Self {
        let mut bytes = vec![0u8];
        let mut library_names: Vec<(&str, u32)> = Vec::with_capacity(library.needed.len() + 1);

        let soname = push(&mut bytes, &library.soname);
        library_names.push((&library.soname, soname));
        let needed = library
            .needed
            .iter()
            .map(|name| {
                let at = held(&library_names, name).unwrap_or_else(|| push(&mut bytes, name));
                library_names.push((name, at));
                at
            })
            .collect();
        let symbols = symbols
            .iter()
            .map(|symbol| {
                held(&library_names, &symbol.name).unwrap_or_else(|| push(&mut bytes, &symbol.name))
            })
            .collect();
        Strings { bytes, soname, needed, symbols }
    }
}

/// Appends a name and its terminator, and answers where it starts.
fn push(bytes: &mut Vec<u8>, name: &str) -> u32 {
    let at = u32::try_from(bytes.len()).expect("a string table under four gigabytes");
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    at
}

/// Where a name already is, if it is already there.
fn held(names: &[(&str, u32)], name: &str) -> Option<u32> {
    names.iter().find(|(held, _)| *held == name).map(|&(_, at)| at)
}

/// The section name table, written out as one literal because the set of sections is fixed.
const SHSTRTAB: &[u8] = b"\0.text\0.hash\0.dynsym\0.dynstr\0.dynamic\0.shstrtab\0";

/// Where each name starts in [`SHSTRTAB`], indexed by section.
const SH_NAMES: [u32; section::COUNT as usize] = [0, 1, 7, 13, 21, 29, 38];

fn header(out: &mut Out, plan: &Plan, target: TargetTuple, machine: u16, flags: u32) {
    out.raw(&[0x7f, b'E', b'L', b'F']);
    out.u8(if plan.wide { 2 } else { 1 });
    out.u8(if out.little { 1 } else { 2 });
    out.u8(1);
    out.u8(os_abi(target.os()));
    // EI_ABIVERSION and the seven bytes of padding after it.
    out.raw(&[0; 8]);
    // ET_DYN. A stub is a shared object and the linker decides what it will accept from the type.
    out.u16(3);
    out.u16(machine);
    out.u32(1);
    // e_entry. Nothing runs, so there is nowhere to start.
    out.addr(0);
    out.addr(u64::try_from(if plan.wide { 64 } else { 52 }).expect("a header size"));
    out.addr(u64::try_from(plan.shdr).expect("an offset inside the file"));
    out.u32(flags);
    out.u16(if plan.wide { 64 } else { 52 });
    out.u16(if plan.wide { 56 } else { 32 });
    out.u16(2);
    out.u16(if plan.wide { 64 } else { 40 });
    out.u16(section::COUNT);
    out.u16(section::SHSTRTAB);
}

/// The two program headers.
///
/// One `PT_LOAD` over everything that is allocated and one `PT_DYNAMIC` over `.dynamic`. Both
/// describe an image nothing will ever map, and they are here because a shared object without a
/// `PT_DYNAMIC` is not a shared object as far as some linkers are concerned, and because a
/// validator reading the file should find the allocated sections inside a segment rather than
/// loose.
fn program_headers(out: &mut Out, plan: &Plan) {
    let mapped = u64::try_from(plan.mapped).expect("an offset inside the file");
    let dynamic = u64::try_from(plan.dynamic).expect("an offset inside the file");
    let dynamic_size = u64::try_from(plan.dynamic_size).expect("a size inside the file");
    if plan.wide {
        // The 64-bit program header puts the flags word after the type, and the 32-bit one puts
        // it at the end, which is the only field order difference between the two classes in this
        // file.
        out.u32(PT_LOAD);
        out.u32(PF_R);
        out.u64(0);
        out.u64(0);
        out.u64(0);
        out.u64(mapped);
        out.u64(mapped);
        out.u64(PAGE);

        out.u32(PT_DYNAMIC);
        out.u32(PF_R);
        out.u64(dynamic);
        out.u64(dynamic);
        out.u64(dynamic);
        out.u64(dynamic_size);
        out.u64(dynamic_size);
        out.u64(8);
    } else {
        out.u32(PT_LOAD);
        out.u32(0);
        out.u32(0);
        out.u32(0);
        out.u32(u32::try_from(mapped).expect("a 32-bit image under four gigabytes"));
        out.u32(u32::try_from(mapped).expect("a 32-bit image under four gigabytes"));
        out.u32(PF_R);
        out.u32(u32::try_from(PAGE).expect("a page size in a word"));

        let dynamic = u32::try_from(dynamic).expect("a 32-bit offset");
        let dynamic_size = u32::try_from(dynamic_size).expect("a 32-bit size");
        out.u32(PT_DYNAMIC);
        out.u32(dynamic);
        out.u32(dynamic);
        out.u32(dynamic);
        out.u32(dynamic_size);
        out.u32(dynamic_size);
        out.u32(PF_R);
        out.u32(4);
    }
}

/// The SysV hash table, which is two counts and then two arrays of symbol indices.
///
/// Every number in it is one [`hash_entry`] wide, the two counts included.
fn hash_table(out: &mut Out, symbols: &[&Symbol], entry: usize) {
    let entries = symbols.len() + 1;
    let count = buckets(entries);
    let mut bucket = vec![0u32; count];
    let mut chain = vec![0u32; entries];
    for (at, symbol) in symbols.iter().enumerate() {
        // Symbol zero is the null entry, so the described symbols start at one.
        let index = u32::try_from(at + 1).expect("a symbol count inside a word");
        let into = (elf_hash(symbol.name.as_bytes()) as usize) % count;
        chain[index as usize] = bucket[into];
        bucket[into] = index;
    }
    let mut word = |value: u32| match entry {
        8 => out.u64(u64::from(value)),
        _ => out.u32(value),
    };
    word(u32::try_from(count).expect("a bucket count inside a word"));
    word(u32::try_from(entries).expect("a symbol count inside a word"));
    for &value in bucket.iter().chain(&chain) {
        word(value);
    }
}

/// The hash function the SysV ABI defines, which is the one the table above is built with.
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

fn symbol_table(out: &mut Out, symbols: &[&Symbol], strings: &Strings) {
    // The null symbol. Index zero of a symbol table is always this and never anything else.
    if out.wide {
        out.u32(0);
        out.u8(0);
        out.u8(0);
        out.u16(0);
        out.u64(0);
        out.u64(0);
    } else {
        out.u32(0);
        out.u32(0);
        out.u32(0);
        out.u8(0);
        out.u8(0);
        out.u16(0);
    }
    for (symbol, &name) in symbols.iter().zip(&strings.symbols) {
        let kind = match symbol.kind {
            Kind::Function => STT_FUNC,
            Kind::Object => STT_OBJECT,
        };
        let bind = match symbol.binding {
            Binding::Global => STB_GLOBAL,
            Binding::Weak => STB_WEAK,
        };
        let info = (bind << 4) | kind;
        // `st_value` is zero for everything, because a stub says what a library exports and
        // nothing about where any of it is. What makes these definitions rather than references
        // is `st_shndx`, which names a real section.
        if out.wide {
            out.u32(name);
            out.u8(info);
            out.u8(0);
            out.u16(section::TEXT);
            out.u64(0);
            out.u64(symbol.size);
        } else {
            out.u32(name);
            out.u32(0);
            out.u32(u32::try_from(symbol.size).expect("an object smaller than a 32-bit address"));
            out.u8(info);
            out.u8(0);
            out.u16(section::TEXT);
        }
    }
}

fn dynamic(out: &mut Out, library: &Library, plan: &Plan, strings: &Strings) {
    let mut entry = |tag: u64, value: u64| {
        if out.wide {
            out.u64(tag);
            out.u64(value);
        } else {
            out.u32(u32::try_from(tag).expect("a tag inside a word"));
            out.u32(u32::try_from(value).expect("a value inside a word"));
        }
    };
    // The needed chain comes first and in the order it was given, because that is the order the
    // loader searches and it is the one property of the chain that is not a set.
    for (needed, &name) in library.needed.iter().zip(&strings.needed) {
        debug_assert!(!needed.is_empty(), "an empty DT_NEEDED would name no library");
        entry(DT_NEEDED, u64::from(name));
    }
    entry(DT_SONAME, u64::from(strings.soname));
    entry(DT_HASH, u64::try_from(plan.hash).expect("an address inside the image"));
    entry(DT_STRTAB, u64::try_from(plan.dynstr).expect("an address inside the image"));
    entry(DT_SYMTAB, u64::try_from(plan.dynsym).expect("an address inside the image"));
    entry(DT_STRSZ, u64::try_from(plan.dynstr_size).expect("a size inside the image"));
    entry(DT_SYMENT, if plan.wide { 24 } else { 16 });
    entry(DT_NULL, 0);
}

fn section_headers(out: &mut Out, plan: &Plan) {
    let mut header = |name: u32,
                      kind: u32,
                      flags: u64,
                      addr: u64,
                      offset: usize,
                      size: usize,
                      link: u32,
                      info: u32,
                      align: u64,
                      entsize: u64| {
        let offset = u64::try_from(offset).expect("an offset inside the file");
        let size = u64::try_from(size).expect("a size inside the file");
        out.u32(name);
        out.u32(kind);
        out.addr(flags);
        out.addr(addr);
        out.addr(offset);
        out.addr(size);
        out.u32(link);
        out.u32(info);
        out.addr(align);
        out.addr(entsize);
    };
    let align = if plan.wide { 8 } else { 4 };
    let sym = if plan.wide { 24 } else { 16 };
    let dyn_size = if plan.wide { 16 } else { 8 };

    header(SH_NAMES[0], 0, 0, 0, 0, 0, 0, 0, 0, 0);
    // `.text` takes no file bytes and has none to take. A `SHT_NOBITS` section's `sh_offset` is
    // conventionally where it would have started, which is the end of the mapped part.
    header(SH_NAMES[section::TEXT as usize], SHT_NOBITS, SHF_ALLOC, 0, plan.mapped, 0, 0, 0, 1, 0);
    header(
        SH_NAMES[section::HASH as usize],
        SHT_HASH,
        SHF_ALLOC,
        u64::try_from(plan.hash).expect("an address"),
        plan.hash,
        plan.hash_size,
        u32::from(section::DYNSYM),
        0,
        align,
        u64::try_from(plan.hash_entry).expect("a hash entry width"),
    );
    header(
        SH_NAMES[section::DYNSYM as usize],
        SHT_DYNSYM,
        SHF_ALLOC,
        u64::try_from(plan.dynsym).expect("an address"),
        plan.dynsym,
        plan.dynsym_size,
        u32::from(section::DYNSTR),
        // `sh_info` of a symbol table is the index of its first non-local symbol. Only the null
        // entry is local here, so every described symbol is above it.
        1,
        align,
        sym,
    );
    header(
        SH_NAMES[section::DYNSTR as usize],
        SHT_STRTAB,
        SHF_ALLOC,
        u64::try_from(plan.dynstr).expect("an address"),
        plan.dynstr,
        plan.dynstr_size,
        0,
        0,
        1,
        0,
    );
    header(
        SH_NAMES[section::DYNAMIC as usize],
        SHT_DYNAMIC,
        SHF_ALLOC,
        u64::try_from(plan.dynamic).expect("an address"),
        plan.dynamic,
        plan.dynamic_size,
        u32::from(section::DYNSTR),
        0,
        align,
        dyn_size,
    );
    // Not allocated and therefore no address, which is what makes it absent from the image a
    // loader would build and present for anything reading the file.
    header(
        SH_NAMES[section::SHSTRTAB as usize],
        SHT_STRTAB,
        0,
        0,
        plan.shstrtab,
        SHSTRTAB.len(),
        0,
        0,
        1,
        0,
    );
}

/// A byte sink that knows the class and the byte order, so no caller has to.
struct Out {
    bytes: Vec<u8>,
    wide: bool,
    little: bool,
}

impl Out {
    fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        if self.little {
            self.bytes.extend_from_slice(&value.to_le_bytes());
        } else {
            self.bytes.extend_from_slice(&value.to_be_bytes());
        }
    }

    fn u32(&mut self, value: u32) {
        if self.little {
            self.bytes.extend_from_slice(&value.to_le_bytes());
        } else {
            self.bytes.extend_from_slice(&value.to_be_bytes());
        }
    }

    fn u64(&mut self, value: u64) {
        if self.little {
            self.bytes.extend_from_slice(&value.to_le_bytes());
        } else {
            self.bytes.extend_from_slice(&value.to_be_bytes());
        }
    }

    /// A word that is an address, an offset or a size, which is the only thing whose width
    /// changes with the class.
    fn addr(&mut self, value: u64) {
        if self.wide {
            self.u64(value);
        } else {
            self.u32(u32::try_from(value).expect("a 32-bit file's words fit in a word"));
        }
    }

    /// Zero fills up to an offset the plan chose.
    fn pad_to(&mut self, offset: usize) {
        debug_assert!(self.bytes.len() <= offset, "the writer is past where the plan put this");
        self.bytes.resize(offset, 0);
    }
}
