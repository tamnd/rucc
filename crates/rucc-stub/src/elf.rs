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
//! A library whose symbols carry version nodes has two more, `.gnu.version` and `.gnu.version_d`.
//! They are absent rather than empty when nothing is at a node, because musl has no symbol versioning
//! and a musl stub with an empty version table is a claim about musl that is not true.
//!
//! `.shstrtab` and the section headers are not in the mapped image, because nothing loads this
//! file. Section 9.1 is explicit that at run time the program loads the real library, and the stub
//! exists only for the duration of a link.
//!
//! # Determinism
//!
//! Claim 5 of `spec/cross-compile/02-the-goal.md` is byte identical output from different hosts, so
//! nothing in here may depend on anything but the description and the tuple. The symbols are
//! sorted by name and then by node before anything is laid out, the version nodes are derived from
//! the symbols and sorted, the string table is built in one fixed order, and there is no hash map
//! anywhere in the file. Section 9.8 names hash map iteration order as the easy way to violate this,
//! and the way to not violate it is to have none.

use rucc_tuple::{Abi, Arch, Endian, ObjectFormat, Os, TargetTuple};

use crate::describe::{Binding, Error, Kind, Library, Symbol};

/// Which sections this file has and what index each one is at.
///
/// Fixed constants would be simpler and were, until version nodes arrived. The two version sections
/// are there only for a library that has nodes, because a musl stub with an empty `.gnu.version` is
/// not what musl looks like and an unversioned library claiming a version table invites a reader to
/// believe the claim. So the indices after `.dynstr` move, and since `sh_link` fields point at them
/// by index, they are computed in one place rather than spelled at each use.
struct Sections {
    text: u16,
    hash: u16,
    dynsym: u16,
    dynstr: u16,
    versym: Option<u16>,
    verdef: Option<u16>,
    dynamic: u16,
    shstrtab: u16,
    count: u16,
    /// The section header string table, and where each section's name starts in it.
    names: Vec<u32>,
    strings: Vec<u8>,
}

impl Sections {
    fn plan(versioned: bool) -> Self {
        let mut names = vec![""; 1];
        names.extend([".text", ".hash", ".dynsym", ".dynstr"]);
        let (versym, verdef) = if versioned {
            names.extend([".gnu.version", ".gnu.version_d"]);
            (Some(5), Some(6))
        } else {
            (None, None)
        };
        names.extend([".dynamic", ".shstrtab"]);

        let mut strings = Vec::new();
        let at = names
            .iter()
            .map(|name| {
                let at = u32::try_from(strings.len()).expect("a short table of section names");
                strings.extend_from_slice(name.as_bytes());
                strings.push(0);
                at
            })
            .collect();
        let count = u16::try_from(names.len()).expect("a handful of sections");
        Sections {
            text: 1,
            hash: 2,
            dynsym: 3,
            dynstr: 4,
            versym,
            verdef,
            dynamic: count - 2,
            shstrtab: count - 1,
            count,
            names: at,
            strings,
        }
    }
}

const SHT_STRTAB: u32 = 3;
const SHT_HASH: u32 = 5;
const SHT_DYNAMIC: u32 = 6;
const SHT_NOBITS: u32 = 8;
const SHT_DYNSYM: u32 = 11;
const SHT_GNU_VERDEF: u32 = 0x6fff_fffd;
const SHT_GNU_VERSYM: u32 = 0x6fff_ffff;

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
const DT_VERSYM: u64 = 0x6fff_fff0;
const DT_VERDEF: u64 = 0x6fff_fffc;
const DT_VERDEFNUM: u64 = 0x6fff_fffd;

/// `VER_NDX_GLOBAL`, the version index of a symbol that has no version node.
///
/// It is also the index of the base definition, which is the arrangement every real library has: the
/// base names the library itself rather than a node anything binds to, so nothing is lost by the two
/// sharing a number.
const VER_NDX_GLOBAL: u16 = 1;

/// `VER_FLG_BASE`, the flag on the definition that names the library rather than a node.
const VER_FLG_BASE: u16 = 1;

/// `VERSYM_HIDDEN`, the bit that says a definition is not the one an unversioned reference takes.
const VERSYM_HIDDEN: u16 = 0x8000;

/// The size of one `Elf_Verdef` record, which is the same in both classes because every field in it
/// is a half or a word.
const VERDEF_SIZE: usize = 20;

/// The size of one `Elf_Verdaux` record, two words.
const VERDAUX_SIZE: usize = 8;

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

    let nodes = Nodes::of(&symbols);
    let sections = Sections::plan(!nodes.is_empty());
    let strings = Strings::build(library, &symbols, &nodes);
    let plan = Plan::lay_out(
        wide,
        hash_entry(target.arch()),
        &sections,
        symbols.len(),
        library.needed.len(),
        strings.bytes.len(),
        nodes.len(),
    );

    let mut out = Out { bytes: Vec::with_capacity(plan.file_size), wide, little };
    header(&mut out, &plan, &sections, target, machine, flags);
    program_headers(&mut out, &plan);
    out.pad_to(plan.hash);
    hash_table(&mut out, &symbols, plan.hash_entry);
    out.pad_to(plan.dynsym);
    symbol_table(&mut out, &symbols, &strings, &sections);
    out.pad_to(plan.dynstr);
    out.raw(&strings.bytes);
    if !nodes.is_empty() {
        out.pad_to(plan.versym);
        version_symbols(&mut out, &symbols, &nodes);
        out.pad_to(plan.verdef);
        version_definitions(&mut out, library, &nodes, &strings);
    }
    out.pad_to(plan.dynamic);
    dynamic(&mut out, library, &plan, &strings, &nodes);
    out.pad_to(plan.shstrtab);
    out.raw(&sections.strings);
    out.pad_to(plan.shdr);
    section_headers(&mut out, &plan, &sections);

    debug_assert_eq!(out.bytes.len(), plan.file_size, "the plan and the writer disagree");
    Ok(out.bytes)
}

/// The node a symbol is at, if it is at one.
///
/// A function rather than a closure at each use, because a closure taking a reference and returning
/// one borrowed from it cannot name the lifetime that relates them.
fn node(symbol: &Symbol) -> Option<&str> {
    symbol.version.as_ref().map(|version| version.node.as_str())
}

/// The symbols, sorted and checked.
///
/// Sorting here rather than asking the caller to is what keeps the order a description was
/// assembled in out of the bytes. It also puts every definition of one name together, which is what
/// [`one_name`] needs to say whether the description is self consistent.
fn ordered(library: &Library) -> Result<Vec<&Symbol>, Error> {
    for name in std::iter::once(&library.soname).chain(&library.needed) {
        if name.contains('\0') {
            return Err(Error::NameHasNul { name: name.clone() });
        }
    }
    let mut symbols: Vec<&Symbol> = library.symbols.iter().collect();
    // A name may legitimately appear more than once now, once per version node, so the order is by
    // name and then by node for the result to be the same on every host.
    symbols.sort_unstable_by(|a, b| a.name.cmp(&b.name).then_with(|| node(a).cmp(&node(b))));
    for symbol in &symbols {
        if symbol.name.is_empty() {
            return Err(Error::NameIsEmpty);
        }
        if symbol.name.contains('\0') {
            return Err(Error::NameHasNul { name: symbol.name.clone() });
        }
        if let Some(version) = &symbol.version {
            if version.node.is_empty() {
                return Err(Error::EmptyNode { name: symbol.name.clone() });
            }
            if version.node.contains('\0') {
                return Err(Error::NameHasNul { name: version.node.clone() });
            }
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
    for group in symbols.chunk_by(|a, b| a.name == b.name) {
        one_name(group)?;
    }
    Ok(symbols)
}

/// Whether every definition of one name agrees with the others about how the name resolves.
///
/// Checked per name rather than per adjacent pair, which is the easy way to write this and is wrong:
/// a name with a default node, then a superseded one, then another default has no adjacent pair that
/// is a problem and still leaves an unversioned reference with two answers.
fn one_name(group: &[&Symbol]) -> Result<(), Error> {
    let [first, rest @ ..] = group else { return Ok(()) };
    if rest.is_empty() {
        return Ok(());
    }
    let name = || first.name.clone();
    let versioned = group.iter().filter(|symbol| symbol.version.is_some()).count();
    if versioned != 0 && versioned != group.len() {
        // One unversioned definition would answer every reference the versioned ones exist to
        // answer, so this is a contradiction rather than a list. All of them unversioned is not this
        // error though, it is the plain duplicate the windows below finds, and saying `mixed` about
        // two unversioned definitions of one name would send a reader looking for a version node
        // that is not there.
        return Err(Error::MixedVersioning { name: name() });
    }
    let mut defaults =
        group.iter().filter_map(|symbol| symbol.version.as_ref()).filter(|v| v.default);
    if let (Some(one), Some(two)) = (defaults.next(), defaults.next()) {
        return Err(Error::TwoDefaults {
            name: name(),
            nodes: (one.node.clone(), two.node.clone()),
        });
    }
    for pair in group.windows(2) {
        let [before, after] = pair else { unreachable!("windows(2) gives pairs") };
        if node(before) == node(after) {
            return Err(Error::Duplicate { name: name() });
        }
    }
    Ok(())
}

/// The version nodes a library defines, in the order their records are written.
///
/// Derived from the symbols rather than declared, because a node nothing is at would be a record no
/// reference can reach and a node a symbol names has to exist either way. That also means the caller
/// cannot get the two out of step.
struct Nodes {
    /// Node names, deduplicated and ordered. Position zero is version index 2, since 1 is the base.
    names: Vec<String>,
}

impl Nodes {
    fn of(symbols: &[&Symbol]) -> Self {
        let mut names: Vec<String> = symbols
            .iter()
            .filter_map(|symbol| symbol.version.as_ref())
            .map(|version| version.node.clone())
            .collect();
        names.sort_unstable_by(|a, b| by_version(a, b));
        names.dedup();
        Nodes { names }
    }

    fn len(&self) -> usize {
        self.names.len()
    }

    fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The version index of a node, which is what goes into `.gnu.version`.
    fn index(&self, node: &str) -> u16 {
        let at =
            self.names.iter().position(|held| held == node).expect("a node taken from a symbol");
        // Index 0 is local, 1 is the base, so the nodes start at 2.
        u16::try_from(at + 2).expect("a library with fewer than sixty thousand version nodes")
    }

    /// How many records `.gnu.version_d` holds, the base definition included.
    fn records(&self) -> usize {
        self.names.len() + 1
    }
}

/// Orders two version node names the way a person reading `readelf -V` would expect.
///
/// Nothing in the format depends on this. Each record carries its own index and a reader follows
/// `vd_next`, so any order produces a correct file, and the reason not to just sort the strings is
/// that `GLIBC_2.10` sorts before `GLIBC_2.2.5` as text and after it as a version. Section 9.8's
/// highest value test is a person diffing our table against a real `libc.so`, and a chain in an order
/// no real library would use makes that diff harder to read than it needs to be.
///
/// Digit runs compare as numbers and everything else compares as bytes, which is enough for
/// `GLIBC_x.y`, `GCC_x.y` and `FBSD_1.x` without knowing anything about any of them.
///
/// Names that come out equal run by run fall back to comparing their bytes, so this is a total order
/// and not just nearly one. Without that, `A1` and `A01` are equal here and unequal to [`str`], which
/// lets an unstable sort put a third name between two copies of one name and a [`Vec::dedup`] after
/// it keep both. Two records for one node is not a reading anybody would enjoy tracking down.
fn by_version(a: &str, b: &str) -> std::cmp::Ordering {
    let mut left = runs(a);
    let mut right = runs(b);
    loop {
        match (left.next(), right.next()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(one), Some(two)) => {
                let order = match (one.parse::<u64>(), two.parse::<u64>()) {
                    (Ok(one), Ok(two)) => one.cmp(&two),
                    _ => one.cmp(two),
                };
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// Splits a name into runs of digits and runs of everything else.
fn runs(name: &str) -> impl Iterator<Item = &str> {
    let mut rest = name;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let digits = rest.starts_with(|c: char| c.is_ascii_digit());
        let end = rest.find(|c: char| c.is_ascii_digit() != digits).unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some(run)
    })
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
    versym: usize,
    versym_size: usize,
    verdef: usize,
    verdef_size: usize,
    /// How many records `.gnu.version_d` holds, which is its `sh_info` and its `DT_VERDEFNUM`.
    verdef_records: usize,
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
        sections: &Sections,
        symbols: usize,
        needed: usize,
        strings: usize,
        nodes: usize,
    ) -> Self {
        let (ehdr, phdr, shdr_size, sym, dyn_size) =
            if wide { (64, 56, 64, 24, 16) } else { (52, 32, 40, 16, 8) };
        // One more than the described count, for the null symbol at index zero that every
        // symbol table starts with.
        let entries = symbols + 1;
        let hash_size = (2 + buckets(entries) + entries) * hash_entry;
        let dynsym_size = entries * sym;
        let versioned = nodes > 0;
        // One half per symbol table entry, the null one included, because the two tables are read
        // in step by index.
        let versym_size = if versioned { entries * 2 } else { 0 };
        // One record per node plus the base, each with a single auxiliary naming it.
        let verdef_records = if versioned { nodes + 1 } else { 0 };
        let verdef_size = verdef_records * (VERDEF_SIZE + VERDAUX_SIZE);
        // The needed chain, the SONAME, the four pointers into the two tables, the two sizes and
        // the terminator, and three more when there are version nodes to point at.
        let dynamic_size = (needed + 7 + if versioned { 3 } else { 0 }) * dyn_size;
        let align = if wide { 8 } else { 4 };

        let hash = round_up(ehdr + 2 * phdr, align);
        let dynsym = round_up(hash + hash_size, align);
        let dynstr = dynsym + dynsym_size;
        // `.gnu.version` is an array of halves and `.gnu.version_d` starts with one, so their
        // alignments are their own rather than the file's class.
        let versym = round_up(dynstr + strings, 2);
        let verdef = round_up(versym + versym_size, 4);
        let dynamic = round_up(verdef + verdef_size, align);
        let mapped = dynamic + dynamic_size;
        let shstrtab = mapped;
        let shdr = round_up(shstrtab + sections.strings.len(), align);
        Plan {
            wide,
            hash,
            hash_entry,
            hash_size,
            dynsym,
            dynsym_size,
            dynstr,
            dynstr_size: strings,
            versym,
            versym_size,
            verdef,
            verdef_size,
            verdef_records,
            dynamic,
            dynamic_size,
            shstrtab,
            shdr,
            file_size: shdr + usize::from(sections.count) * shdr_size,
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
    /// One offset per version node, in [`Nodes`] order.
    nodes: Vec<u32>,
}

impl Strings {
    /// Builds the table in one fixed order, with names that repeat stored once.
    ///
    /// The repeats are worth the trouble because they are cheap to find. The `SONAME` and the
    /// needed chain are a handful of names, the symbols are already sorted and already known to be
    /// unique among themselves, so the only collisions possible are between a symbol and one of
    /// that handful. That is a scan over a short list per symbol and not a map.
    fn build(library: &Library, symbols: &[&Symbol], nodes: &Nodes) -> Self {
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
        // The node names last, deduplicated only against each other, which `Nodes` has already
        // done. A symbol named the same thing as a version node would be stored twice, and that
        // costs a few bytes in a file nothing loads rather than anything a reader would notice.
        let nodes = nodes.names.iter().map(|node| push(&mut bytes, node)).collect();
        Strings { bytes, soname, needed, symbols, nodes }
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

/// `.gnu.version`, one half per symbol table entry saying which node that entry belongs to.
///
/// Read in step with `.dynsym` by index, so the null symbol gets an entry too. A symbol with no node
/// gets [`VER_NDX_GLOBAL`], and a definition something newer has taken the default away from gets its
/// index with [`VERSYM_HIDDEN`] set, which is the whole of how a linker tells `memcpy@GLIBC_2.2.5`
/// from `memcpy@@GLIBC_2.14`.
fn version_symbols(out: &mut Out, symbols: &[&Symbol], nodes: &Nodes) {
    // The null symbol is local, and index zero is what says so.
    out.u16(0);
    for symbol in symbols {
        match &symbol.version {
            None => out.u16(VER_NDX_GLOBAL),
            Some(version) => {
                let index = nodes.index(&version.node);
                out.u16(if version.default { index } else { index | VERSYM_HIDDEN });
            }
        }
    }
}

/// `.gnu.version_d`, the chain of version definitions.
///
/// The first record is the base, which names the library itself rather than a node, and every record
/// after it is one node with a single auxiliary holding its name. `vd_next` and `vd_aux` are byte
/// offsets from the start of the record they appear in rather than from the section, which is the
/// part of this format that is easy to get wrong and produces a chain a reader walks off the end of.
fn version_definitions(out: &mut Out, library: &Library, nodes: &Nodes, strings: &Strings) {
    let records = nodes.records();
    let both = u32::try_from(VERDEF_SIZE + VERDAUX_SIZE).expect("a record and its auxiliary");
    let names = std::iter::once((strings.soname, VER_FLG_BASE))
        .chain(strings.nodes.iter().map(|&at| (at, 0)));
    for (at, (name, flags)) in names.enumerate() {
        // vd_version, which is 1 and has only ever been 1.
        out.u16(1);
        out.u16(flags);
        // The base is index 1 and the nodes follow it, which is the same arithmetic `Nodes::index`
        // does and the reason that function is the only place it is written.
        out.u16(u16::try_from(at + 1).expect("a library with few enough version nodes"));
        // One auxiliary per record. A node that inherited from another would have more, and nothing
        // in a link depends on the inheritance, so there is one.
        out.u16(1);
        out.u32(elf_hash(node_name(library, nodes, at).as_bytes()));
        out.u32(u32::try_from(VERDEF_SIZE).expect("a record header"));
        out.u32(if at + 1 == records { 0 } else { both });
        // The auxiliary: the name, and no next.
        out.u32(name);
        out.u32(0);
    }
}

/// The name a version definition record carries, which the base takes from the `SONAME`.
///
/// Spelled out because `vd_hash` is a hash of that name, and hashing the wrong one of the two gives a
/// file every reader prints correctly and a loader cannot look a version up in.
fn node_name<'a>(library: &'a Library, nodes: &'a Nodes, record: usize) -> &'a str {
    match record.checked_sub(1) {
        None => &library.soname,
        Some(node) => &nodes.names[node],
    }
}

fn header(
    out: &mut Out,
    plan: &Plan,
    sections: &Sections,
    target: TargetTuple,
    machine: u16,
    flags: u32,
) {
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
    out.u16(sections.count);
    out.u16(sections.shstrtab);
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

fn symbol_table(out: &mut Out, symbols: &[&Symbol], strings: &Strings, sections: &Sections) {
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
            out.u16(sections.text);
            out.u64(0);
            out.u64(symbol.size);
        } else {
            out.u32(name);
            out.u32(0);
            out.u32(u32::try_from(symbol.size).expect("an object smaller than a 32-bit address"));
            out.u8(info);
            out.u8(0);
            out.u16(sections.text);
        }
    }
}

fn dynamic(out: &mut Out, library: &Library, plan: &Plan, strings: &Strings, nodes: &Nodes) {
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
    // The version tables, and only when there are any. A `DT_VERSYM` pointing at nothing would have
    // the loader read version indices out of whatever followed the string table.
    if !nodes.is_empty() {
        entry(DT_VERSYM, u64::try_from(plan.versym).expect("an address inside the image"));
        entry(DT_VERDEF, u64::try_from(plan.verdef).expect("an address inside the image"));
        entry(DT_VERDEFNUM, u64::try_from(plan.verdef_records).expect("a record count"));
    }
    entry(DT_NULL, 0);
}

fn section_headers(out: &mut Out, plan: &Plan, sections: &Sections) {
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

    let name = |index: u16| sections.names[usize::from(index)];

    header(sections.names[0], 0, 0, 0, 0, 0, 0, 0, 0, 0);
    // `.text` takes no file bytes and has none to take. A `SHT_NOBITS` section's `sh_offset` is
    // conventionally where it would have started, which is the end of the mapped part.
    header(name(sections.text), SHT_NOBITS, SHF_ALLOC, 0, plan.mapped, 0, 0, 0, 1, 0);
    header(
        name(sections.hash),
        SHT_HASH,
        SHF_ALLOC,
        u64::try_from(plan.hash).expect("an address"),
        plan.hash,
        plan.hash_size,
        u32::from(sections.dynsym),
        0,
        align,
        u64::try_from(plan.hash_entry).expect("a hash entry width"),
    );
    header(
        name(sections.dynsym),
        SHT_DYNSYM,
        SHF_ALLOC,
        u64::try_from(plan.dynsym).expect("an address"),
        plan.dynsym,
        plan.dynsym_size,
        u32::from(sections.dynstr),
        // `sh_info` of a symbol table is the index of its first non-local symbol. Only the null
        // entry is local here, so every described symbol is above it.
        1,
        align,
        sym,
    );
    header(
        name(sections.dynstr),
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
    if let Some(versym) = sections.versym {
        header(
            name(versym),
            SHT_GNU_VERSYM,
            SHF_ALLOC,
            u64::try_from(plan.versym).expect("an address"),
            plan.versym,
            plan.versym_size,
            // The table this one is read in step with.
            u32::from(sections.dynsym),
            0,
            2,
            2,
        );
    }
    if let Some(verdef) = sections.verdef {
        header(
            name(verdef),
            SHT_GNU_VERDEF,
            SHF_ALLOC,
            u64::try_from(plan.verdef).expect("an address"),
            plan.verdef,
            plan.verdef_size,
            // Where the node names live.
            u32::from(sections.dynstr),
            // `sh_info` of a version definition section is how many records it holds, not an index
            // into anything, which is the one place in a section header that field means a count.
            u32::try_from(plan.verdef_records).expect("a record count"),
            4,
            // The records are a chain rather than an array and the last one has no `vd_next`, so
            // there is no fixed entry size to state.
            0,
        );
    }
    header(
        name(sections.dynamic),
        SHT_DYNAMIC,
        SHF_ALLOC,
        u64::try_from(plan.dynamic).expect("an address"),
        plan.dynamic,
        plan.dynamic_size,
        u32::from(sections.dynstr),
        0,
        align,
        dyn_size,
    );
    // Not allocated and therefore no address, which is what makes it absent from the image a
    // loader would build and present for anything reading the file.
    header(
        name(sections.shstrtab),
        SHT_STRTAB,
        0,
        0,
        plan.shstrtab,
        sections.strings.len(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_nodes_order_the_way_a_person_reads_them() {
        use std::cmp::Ordering;
        // The case that makes this worth having at all. As text `GLIBC_2.10` is less than
        // `GLIBC_2.2.5`, because `1` is less than `2`, and as a version it is greater.
        assert_eq!(by_version("GLIBC_2.10", "GLIBC_2.2.5"), Ordering::Greater);
        assert_eq!(by_version("GLIBC_2.2.5", "GLIBC_2.14"), Ordering::Less);
        assert_eq!(by_version("GLIBC_2.34", "GLIBC_2.4"), Ordering::Greater);
        // A prefix sorts before what extends it, so a node and a point release of it stay in order.
        assert_eq!(by_version("GLIBC_2.2", "GLIBC_2.2.5"), Ordering::Less);
        // Different families sort by their names, which is all anybody needs of them.
        assert_eq!(by_version("GCC_3.0", "GLIBC_2.2.5"), Ordering::Less);
        assert_eq!(by_version("GLIBC_2.17", "GLIBC_2.17"), Ordering::Equal);
    }

    #[test]
    fn two_spellings_of_one_number_are_not_equal() {
        use std::cmp::Ordering;
        // Equal here and unequal to `str` is the combination that breaks the dedup in `Nodes::of`,
        // since an unstable sort may then put a third name between two copies of one name.
        assert_ne!(by_version("A01", "A1"), Ordering::Equal);
        assert_ne!(by_version("GLIBC_2.02", "GLIBC_2.2"), Ordering::Equal);
    }

    #[test]
    fn a_node_index_counts_from_two() {
        let symbols = [
            Symbol::function("memcpy").at("GLIBC_2.14"),
            Symbol::function("printf").at("GLIBC_2.2.5"),
            Symbol::function("strlen").behind("GLIBC_2.2.5"),
        ];
        let borrowed: Vec<&Symbol> = symbols.iter().collect();
        let nodes = Nodes::of(&borrowed);
        // Zero is local and one is the base, so the first node a library defines is two. One node
        // per name however many symbols are at it.
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes.index("GLIBC_2.2.5"), 2);
        assert_eq!(nodes.index("GLIBC_2.14"), 3);
        // The base record is one of the records, so the count is one more than the node count.
        assert_eq!(nodes.records(), 3);
    }

    #[test]
    fn a_library_with_no_nodes_has_none() {
        let symbols = [Symbol::function("printf")];
        let borrowed: Vec<&Symbol> = symbols.iter().collect();
        assert!(Nodes::of(&borrowed).is_empty());
    }
}
