//! What Mach-O answers, where it answers differently from the other two formats.
//!
//! Design: `spec/cross-compile/07-object-formats.md` section 7.3. The sibling of [`crate::elf`] and
//! [`crate::coff`], and for now only for AArch64, which is every Mac sold since 2023 and every
//! iPhone. An x86-64 Mac is the same format with its own table of relocations, which is not
//! written yet.
//!
//! # A section is two names
//!
//! Every section is in a segment and says so: code is `__TEXT,__text` and writable data is
//! `__DATA,__data`. The listing writes both halves and so does anyone who writes assembly for a
//! Mac, so the name of a part here is the pair with the comma between them. What a section is,
//! zero filled, strings, constructors or a thread's image, is a type in the low byte of its
//! flags rather than something the name implies, and the attributes above that byte say whether it
//! holds instructions. [`section_flags`] is the one place both are worked out.
//!
//! # A relocation keeps its addend somewhere else
//!
//! A Mach-O relocation has no addend field. An address in data keeps it in the bytes the address
//! goes in, which the writer underneath puts there. An instruction has no room for one, so the
//! relocation is preceded by an `ARM64_RELOC_ADDEND` that carries it in the field that would
//! otherwise name a symbol, which has twenty four bits. The writer underneath makes that pair
//! too, and what is left here is refusing an addend that does not fit, and one on a load through
//! a table slot, where the addend would be an offset into the slot and means nothing.
//!
//! # A name the linker splits at
//!
//! A file that ends with `.subsections_via_symbols` tells the linker every symbol starts a piece it
//! may move or throw away, which is how a Mac drops a function nothing calls. A label inside a
//! function must then not be a symbol, or the linker would cut the function in two. An `L` in
//! front is how the format says a name is only the assembler's, and a relocation against one is
//! written against the symbol before it with the distance added, the way Apple's assembler does
//! it.

use object::macho;
use object::write::SymbolSection;
use object::write::{MachOBuildVersion, Mangling, Object as Writer, Relocation, Symbol};
use object::{
    Architecture, BinaryFormat, Endianness, RelocationEncoding, RelocationFlags, RelocationKind,
    SectionFlags, SectionKind, SymbolFlags,
};
use rucc_base::hash::{Map, Set};
use rucc_target::TargetInfo;
use rucc_target::aarch64::Fixup;
use rucc_tuple::{Env, Os, Version};

use crate::file::{Error, Flavour, scope_of};
use crate::section::{Binding, Info, Reference};
use crate::source::{Assembled, Held, Name, Sort};

/// A file of assembly for AArch64, as a Mach-O object.
///
/// The same three passes [`crate::assembled`] makes for the other two formats: every section, then
/// every name, then every relocation, since each wants the one before it finished. What differs is
/// in the module comment above, and where the debug information in `info` goes, which is the
/// last section below.
pub(crate) fn write(input: &Assembled, target: &TargetInfo, info: &Info) -> Result<Vec<u8>, Error> {
    let refused = |why: String| Error::Refused { why };
    let mut obj = Writer::new(BinaryFormat::MachO, Architecture::Aarch64, Endianness::Little);
    // The listing already wrote the underscore every C name has on this format, so the writer
    // underneath must not add a second one.
    obj.set_mangling(Mangling::None);
    if input.subsections {
        obj.set_subsections_via_symbols();
    }
    build_version(&mut obj, target);

    let mut made = Vec::with_capacity(input.parts.len());
    for part in &input.parts {
        let (segment, section) = split(&part.name).map_err(refused)?;
        let kind = kind(segment, part.shape.mach);
        let id = obj.add_section(segment.as_bytes().to_vec(), section.as_bytes().to_vec(), kind);
        let flags = macho::SectionFlags(part.shape.mach);
        obj.section_mut(id).flags = SectionFlags::MachO { flags, reserved2: 0 };
        let align = part.align.max(1);
        if part.shape.bits {
            obj.append_section_data(id, &part.bytes, align);
        } else {
            obj.append_section_bss(id, part.size, align);
        }
        made.push(id);
    }

    // Which name each relocation is written against. A temporary is replaced by the last real
    // name at or before it in the same section, with the distance between them added, and one
    // with no real name in front of it is kept as a symbol after all, which is what the linker
    // then cuts the section at. That is at the start of what it was in front of, so nothing is cut
    // in two.
    let defined: Map<&str, &Name> =
        input.names.iter().map(|name| (name.name.as_str(), name)).collect();
    let mut atoms: Vec<Vec<(u64, &str)>> = vec![Vec::new(); input.parts.len()];
    for name in &input.names {
        if let Held::In { part, offset } = name.at {
            if !temporary(&name.name) && name.sort != Sort::File {
                atoms[part].push((offset, name.name.as_str()));
            }
        }
    }
    for list in &mut atoms {
        list.sort_unstable();
    }
    let target_of = |symbol: &str, addend: i64| -> (String, i64) {
        let Some(Held::In { part, offset }) = defined.get(symbol).map(|name| name.at) else {
            return (symbol.to_owned(), addend);
        };
        if !temporary(symbol) {
            return (symbol.to_owned(), addend);
        }
        let list = &atoms[part];
        match list.partition_point(|&(at, _)| at <= offset) {
            0 => (symbol.to_owned(), addend),
            after => {
                let (at, name) = list[after - 1];
                (name.to_owned(), addend + (offset - at) as i64)
            }
        }
    };
    let kept: Set<String> = input
        .parts
        .iter()
        .flat_map(|part| &part.relocs)
        .map(|reloc| target_of(&reloc.symbol, reloc.addend).0)
        .filter(|name| temporary(name))
        .collect();

    let mut symbols = Map::default();
    for name in &input.names {
        if temporary(&name.name) && !kept.contains(&name.name) {
            continue;
        }
        let (section, value, size) = match name.at {
            Held::In { part, offset } => {
                let Some(id) = made.get(part) else {
                    let why = format!(
                        "'{}' is in section {part} and there is no such section",
                        name.name
                    );
                    return Err(Error::Refused { why });
                };
                (SymbolSection::Section(*id), offset, name.size)
            }
            Held::Absolute(value) => (SymbolSection::Absolute, value, 0),
            // The boundary goes where an ELF writer puts it and the writer underneath moves it to
            // the few bits of the description Mach-O keeps it in.
            Held::Common { size, align } => (SymbolSection::Common, align, size),
            Held::Undefined => (SymbolSection::Undefined, 0, 0),
        };
        let id = obj.add_symbol(Symbol {
            name: name.name.clone().into_bytes(),
            value,
            size,
            kind: Flavour::MachO.sort(name.sort, name.binding),
            scope: scope_of(name.binding),
            weak: name.binding == Binding::Weak,
            section,
            flags: SymbolFlags::None,
        });
        Flavour::MachO.see(&mut obj, id, name.binding, name.visibility);
        symbols.insert(name.name.as_str(), id);
    }

    for (part, id) in input.parts.iter().zip(&made) {
        for reloc in &part.relocs {
            let (name, addend) = target_of(&reloc.symbol, reloc.addend);
            let Some(&symbol) = symbols.get(name.as_str()) else {
                let why = format!("'{name}' is named by a relocation and by nothing else");
                return Err(Error::Refused { why });
            };
            let flags = self::reloc(reloc.kind, addend).map_err(refused)?;
            let record = Relocation { offset: reloc.at as u64, symbol, addend, flags };
            obj.add_relocation(*id, record).map_err(|why| refused(why.to_string()))?;
        }
    }
    described(&mut obj, &symbols, info).map_err(refused)?;
    obj.write().map_err(|why| refused(why.to_string()))
}

/// The debug sections, in `__DWARF`, which is the segment the linker leaves out of the image.
///
/// A Mac does not link DWARF. ld64 writes a map into the image that says which object each
/// function came from and where it went, and `dsymutil` reads each object's own sections through
/// that map, or a debugger does the same thing without it. So a place in one debug section that
/// names another is never moved by a linker, and is written as the offset it is with no relocation,
/// which is what clang does. What names a function or a variable is an address and keeps its
/// relocation, since that is how the map is matched to the object. The debug information knows
/// those by their C names, and the symbol is the same name with the underscore in front.
fn described(
    obj: &mut Writer<'_>,
    symbols: &Map<&str, object::write::SymbolId>,
    info: &Info,
) -> Result<(), String> {
    let debug = info.chunks.iter().map(|chunk| chunk.name.as_str()).collect::<Set<_>>();
    for chunk in &info.chunks {
        let section = debug_section(&chunk.name)?;
        let id = obj.add_section(b"__DWARF".to_vec(), section.into_bytes(), SectionKind::Debug);
        // An ordinary section, which is a type of zero, holding debug information.
        let flags = macho::S_ATTR_DEBUG;
        obj.section_mut(id).flags = SectionFlags::MachO { flags, reserved2: 0 };
        let mut bytes = chunk.bytes.clone();
        let mut relocs = Vec::new();
        for reloc in &chunk.relocs {
            let Reference::Address { bytes: width } = reloc.kind else {
                return Err(format!("{:?} in the debug information", reloc.kind));
            };
            if debug.contains(reloc.symbol.as_str()) {
                let at = reloc.at;
                let Some(place) = bytes.get_mut(at..at + usize::from(width)) else {
                    return Err(format!("a place past the end of {}", chunk.name));
                };
                place.copy_from_slice(&reloc.addend.to_le_bytes()[..usize::from(width)]);
                continue;
            }
            let name = format!("_{}", reloc.symbol);
            let Some(&symbol) = symbols.get(name.as_str()) else {
                return Err(format!("'{name}' is named by the debug information and not defined"));
            };
            let flags = self::reloc(reloc.kind, reloc.addend)?;
            let addend = reloc.addend;
            relocs.push(Relocation { offset: reloc.at as u64, symbol, addend, flags });
        }
        obj.append_section_data(id, &bytes, 1);
        for record in relocs {
            obj.add_relocation(id, record).map_err(|why| why.to_string())?;
        }
    }
    Ok(())
}

/// The Mach-O name of a DWARF section, which is its ELF name with two underscores for the dot, cut
/// to the sixteen bytes a section name has. `.debug_str_offsets` is `__debug_str_offs`, the same
/// as clang's.
fn debug_section(name: &str) -> Result<String, String> {
    let Some(rest) = name.strip_prefix(".debug_") else {
        return Err(format!("'{name}' is not a DWARF section"));
    };
    let mut spelled = format!("__debug_{rest}");
    spelled.truncate(16);
    Ok(spelled)
}

/// The largest addend an `ARM64_RELOC_ADDEND` can carry either way, which is what fits in the
/// twenty four bits it has for one.
const ADDEND: i64 = 1 << 23;

/// Which relocation one reference is, or why there is none.
///
/// The branches, the page of an address and the low twelve bits of it are the four the code
/// generator uses for a name it can reach directly, and the loads through a table slot are the two
/// it uses for one another image may define. The low twelve bits are one relocation whatever size
/// the access is, since the linker reads the size off the instruction it is patching rather than
/// being told it, which is the difference from ELF's five. A conditional branch, `tbz` and a
/// literal load have no relocation on this format at all, and the assembler resolves every one of
/// those that stays inside a section.
pub(crate) fn reloc(reference: Reference, addend: i64) -> Result<RelocationFlags, String> {
    // A four byte distance from where it is written, which is how an unwind record says where its
    // function and its common record are. arm64 has no relocation for a distance in data, and the
    // way the format says one is an `ARM64_RELOC_SUBTRACTOR` naming the place, followed by an
    // `ARM64_RELOC_UNSIGNED` naming the target, with what is added kept in the bytes. The writer
    // underneath makes that pair from a generic distance, against a name it puts at the front of
    // the section the distance is written in, which is what clang's records do as well.
    if reference == Reference::Data {
        return Ok(RelocationFlags::Generic {
            kind: RelocationKind::Relative,
            encoding: RelocationEncoding::Generic,
            size: 32,
        });
    }
    let (r_type, r_pcrel, r_length) = match reference {
        Reference::Field(Fixup::Call26 | Fixup::Jump26) => (macho::ARM64_RELOC_BRANCH26, true, 2),
        Reference::Field(Fixup::AdrPage21) => (macho::ARM64_RELOC_PAGE21, true, 2),
        Reference::Field(
            Fixup::AddLo12
            | Fixup::Ldst8Lo12
            | Fixup::Ldst16Lo12
            | Fixup::Ldst32Lo12
            | Fixup::Ldst64Lo12
            | Fixup::Ldst128Lo12,
        ) => (macho::ARM64_RELOC_PAGEOFF12, false, 2),
        Reference::Field(Fixup::GotPage21) => (macho::ARM64_RELOC_GOT_LOAD_PAGE21, true, 2),
        Reference::Field(Fixup::GotLo12) => (macho::ARM64_RELOC_GOT_LOAD_PAGEOFF12, false, 2),
        Reference::Field(Fixup::GotTprelPage21) => (macho::ARM64_RELOC_TLVP_LOAD_PAGE21, true, 2),
        Reference::Field(Fixup::GotTprelLo12Nc) => {
            (macho::ARM64_RELOC_TLVP_LOAD_PAGEOFF12, false, 2)
        }
        Reference::Address { bytes: 8 } => (macho::ARM64_RELOC_UNSIGNED, false, 3),
        Reference::Address { bytes: 4 } => (macho::ARM64_RELOC_UNSIGNED, false, 2),
        Reference::Field(field) => {
            return Err(format!(
                "{} has no Mach-O relocation, and a branch that leaves its section has to be a \
                 `b` or a `bl`",
                field.name()
            ));
        }
        other => return Err(format!("no Mach-O relocation for arm64 is {other:?}")),
    };
    let slot = matches!(
        r_type,
        macho::ARM64_RELOC_GOT_LOAD_PAGE21
            | macho::ARM64_RELOC_GOT_LOAD_PAGEOFF12
            | macho::ARM64_RELOC_TLVP_LOAD_PAGE21
            | macho::ARM64_RELOC_TLVP_LOAD_PAGEOFF12
    );
    if slot && addend != 0 {
        return Err(format!("a load through a table slot with {addend} added to it"));
    }
    if r_type != macho::ARM64_RELOC_UNSIGNED && !(-ADDEND..ADDEND).contains(&addend) {
        return Err(format!("{addend} added to a name, which is more than Mach-O can say"));
    }
    Ok(RelocationFlags::MachO { r_type, r_pcrel, r_length })
}

/// The segment and the section a part's name says, split at the comma.
pub(crate) fn split(name: &str) -> Result<(&str, &str), String> {
    let why = || {
        format!(
            "'{name}' is not a Mach-O section name, which is a segment and a section of up to \
             sixteen bytes each with a comma between them"
        )
    };
    let (segment, section) = name.split_once(',').ok_or_else(why)?;
    let fits = |half: &str| !half.is_empty() && half.len() <= 16 && !half.contains(',');
    if !fits(segment) || !fits(section) {
        return Err(why());
    }
    Ok((segment, section))
}

/// The type and attributes a section has, from what `.section` said after its name or from the
/// name alone when it said nothing.
///
/// `kind` is the third field of the directive and `attributes` the fourth, split at each `+`. The
/// names with a type of their own are the ones Apple's assembler knows without being told, so
/// `.section __TEXT,__cstring` is strings whether or not `cstring_literals` follows.
///
/// # Errors
///
/// A type or an attribute Apple's assembler does not take, as a sentence.
pub(crate) fn section_flags(
    segment: &str,
    section: &str,
    kind: Option<&str>,
    attributes: &[&str],
) -> Result<u32, String> {
    let typ = match kind.map(str::trim) {
        Some(kind) => match kind {
            "regular" => macho::S_REGULAR,
            "zerofill" => macho::S_ZEROFILL,
            "cstring_literals" => macho::S_CSTRING_LITERALS,
            "4byte_literals" => macho::S_4BYTE_LITERALS,
            "8byte_literals" => macho::S_8BYTE_LITERALS,
            "16byte_literals" => macho::S_16BYTE_LITERALS,
            "literal_pointers" => macho::S_LITERAL_POINTERS,
            "non_lazy_symbol_pointers" => macho::S_NON_LAZY_SYMBOL_POINTERS,
            "mod_init_funcs" => macho::S_MOD_INIT_FUNC_POINTERS,
            "mod_term_funcs" => macho::S_MOD_TERM_FUNC_POINTERS,
            "coalesced" => macho::S_COALESCED,
            "interposing" => macho::S_INTERPOSING,
            "thread_local_regular" => macho::S_THREAD_LOCAL_REGULAR,
            "thread_local_zerofill" => macho::S_THREAD_LOCAL_ZEROFILL,
            "thread_local_variables" => macho::S_THREAD_LOCAL_VARIABLES,
            "thread_local_variable_pointers" => macho::S_THREAD_LOCAL_VARIABLE_POINTERS,
            "thread_local_init_function_pointers" => macho::S_THREAD_LOCAL_INIT_FUNCTION_POINTERS,
            other => return Err(format!("'{other}' is not a Mach-O section type")),
        },
        None => match (segment, section) {
            (_, "__cstring") => macho::S_CSTRING_LITERALS,
            (_, "__bss" | "__common") => macho::S_ZEROFILL,
            (_, "__thread_bss") => macho::S_THREAD_LOCAL_ZEROFILL,
            (_, "__thread_data") => macho::S_THREAD_LOCAL_REGULAR,
            (_, "__thread_vars") => macho::S_THREAD_LOCAL_VARIABLES,
            (_, "__mod_init_func") => macho::S_MOD_INIT_FUNC_POINTERS,
            (_, "__mod_term_func") => macho::S_MOD_TERM_FUNC_POINTERS,
            (_, "__literal4") => macho::S_4BYTE_LITERALS,
            (_, "__literal8") => macho::S_8BYTE_LITERALS,
            (_, "__literal16") => macho::S_16BYTE_LITERALS,
            _ => macho::S_REGULAR,
        },
    };
    let mut flags = u32::from(typ.0);
    // `__TEXT,__text` holds instructions whatever the directive said, and a file that opened it
    // with no attributes still has its code in it.
    let code = (segment, section) == ("__TEXT", "__text");
    for attribute in attributes.iter().map(|attribute| attribute.trim()) {
        flags |= match attribute {
            "pure_instructions" => macho::S_ATTR_PURE_INSTRUCTIONS.0,
            "no_toc" => macho::S_ATTR_NO_TOC.0,
            "strip_static_syms" => macho::S_ATTR_STRIP_STATIC_SYMS.0,
            "no_dead_strip" => macho::S_ATTR_NO_DEAD_STRIP.0,
            "live_support" => macho::S_ATTR_LIVE_SUPPORT.0,
            "self_modifying_code" => macho::S_ATTR_SELF_MODIFYING_CODE.0,
            "debug" => macho::S_ATTR_DEBUG.0,
            "" => 0,
            other => return Err(format!("'{other}' is not a Mach-O section attribute")),
        };
    }
    if code {
        flags |= macho::S_ATTR_PURE_INSTRUCTIONS.0;
    }
    // The assembler's own attribute, which says the linker may find code here. Every section the
    // program said is instructions has some.
    if flags & macho::S_ATTR_PURE_INSTRUCTIONS.0 != 0 {
        flags |= macho::S_ATTR_SOME_INSTRUCTIONS.0;
    }
    Ok(flags)
}

/// Whether a section with these flags carries no bytes in the file.
#[must_use]
pub(crate) fn zero_filled(flags: u32) -> bool {
    let typ = macho::SectionFlags(flags).typ();
    matches!(typ, macho::S_ZEROFILL | macho::S_GB_ZEROFILL | macho::S_THREAD_LOCAL_ZEROFILL)
}

/// What the writer underneath calls the nearest thing to a section with these flags.
///
/// It is told the flags in full afterwards, so this only decides whether the writer counts bytes
/// or holds them, and nothing it does with a kind for thread-local data is wanted: the listing
/// writes the descriptors itself.
pub(crate) fn kind(segment: &str, flags: u32) -> SectionKind {
    if zero_filled(flags) {
        SectionKind::UninitializedData
    } else if flags & macho::S_ATTR_PURE_INSTRUCTIONS.0 != 0 {
        SectionKind::Text
    } else if segment == "__TEXT" {
        SectionKind::ReadOnlyData
    } else {
        SectionKind::Data
    }
}

/// Whether a name is one only the assembler sees, which is what an `L` in front says on Mach-O.
///
/// A name with a `\u{1}` in it is one the assembler made for a numbered label or a frame, which is
/// the same kind of name under a spelling no program can write.
pub(crate) fn temporary(name: &str) -> bool {
    name.starts_with('L') || name.contains('\u{1}')
}

/// The platform and the oldest version of it the file is for, which is `LC_BUILD_VERSION`.
///
/// The linker decides from the version how to lay out constructors and fixups, so it is not
/// cosmetic. The defaults are the oldest release that runs on Apple silicon for a Mac and the one
/// the SDK's own default is for an iPhone, the same two the preprocessor predefines.
pub(crate) fn build_version(obj: &mut Writer<'_>, target: &TargetInfo) {
    let (platform, default) = match (target.tuple.os(), target.tuple.env()) {
        (Os::MacOs, _) => (macho::PLATFORM_MACOS, Version::new(11, 0)),
        (Os::IOs, Env::Simulator) => (macho::PLATFORM_IOSSIMULATOR, Version::new(14, 0)),
        (Os::IOs, Env::MacAbi) => (macho::PLATFORM_MACCATALYST, Version::new(14, 0)),
        (Os::IOs, _) => (macho::PLATFORM_IOS, Version::new(14, 0)),
        _ => return,
    };
    let version = target.tuple.os_version().unwrap_or(default);
    let part = |part: Option<u32>| part.unwrap_or(0).min(255) as u8;
    let minos = macho::Version::new(
        version.major_part().min(u32::from(u16::MAX)) as u16,
        part(version.minor_part()),
        part(version.patch_part()),
    );
    let mut build = MachOBuildVersion::default();
    (build.platform, build.minos) = (platform, minos);
    obj.set_macho_build_version(build);
}

#[cfg(test)]
mod tests {
    use super::*;

    use object::read::{Object as _, ObjectSection as _, ObjectSymbol as _};

    use crate::section::{Reloc, Visibility};
    use crate::source::{Part, Shape};

    fn part(segment: &str, section: &str, bytes: Vec<u8>, size: u64) -> Part {
        Part {
            name: format!("{segment},{section}"),
            bytes,
            size,
            align: 8,
            shape: Shape::mach(segment, section, None, &[]).unwrap(),
            relocs: Vec::new(),
            group: None,
        }
    }

    fn name(name: &str, part: usize, offset: u64, binding: Binding) -> Name {
        Name {
            name: name.to_owned(),
            at: Held::In { part, offset },
            size: 0,
            sort: Sort::Untyped,
            binding,
            visibility: Visibility::Default,
        }
    }

    #[test]
    fn a_file_reads_back_as_the_sections_names_and_relocations_it_was_given() {
        let mut text = part("__TEXT", "__text", vec![0; 12], 12);
        for (at, fixup) in [(0, Fixup::AdrPage21), (4, Fixup::AddLo12)] {
            let kind = Reference::Field(fixup);
            text.relocs.push(Reloc { at, symbol: "_counter".into(), kind, addend: 0, after: 0 });
        }
        let mut data = part("__DATA", "__data", vec![0; 24], 24);
        let kind = Reference::Address { bytes: 8 };
        data.relocs.push(Reloc { at: 8, symbol: "Lstr".into(), kind, addend: 0, after: 0 });
        let strings = part("__TEXT", "__cstring", b"no\0hi\0".to_vec(), 6);
        let bss = part("__DATA", "__bss", Vec::new(), 64);
        let mut main = name("_main", 0, 0, Binding::Global);
        main.sort = Sort::Func;
        let input = Assembled {
            parts: vec![text, data, strings, bss],
            names: vec![
                main,
                name("Lmain_0", 0, 0, Binding::Local),
                name("_counter", 1, 0, Binding::Global),
                name("_s", 2, 0, Binding::Local),
                name("Lstr", 2, 3, Binding::Local),
                name("_zeros", 3, 0, Binding::Global),
            ],
            subsections: true,
        };
        let target = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        let bytes = write(&input, &target, &Info::default()).unwrap();
        let file = object::File::parse(&bytes[..]).unwrap();
        assert_eq!(file.format(), BinaryFormat::MachO);
        assert_eq!(file.architecture(), Architecture::Aarch64);
        let names: Vec<_> = file.symbols().filter_map(|sym| sym.name().ok()).collect();
        assert!(names.contains(&"_main") && names.contains(&"_zeros"), "{names:?}");
        assert!(!names.iter().any(|name| name.starts_with('L')), "{names:?}");
        let text = file.section_by_name("__text").unwrap();
        assert_eq!(text.relocations().count(), 2);
        let bss = file.section_by_name("__bss").unwrap();
        assert_eq!((bss.size(), bss.data().unwrap().len()), (64, 0));
        // The temporary is gone and the address is the string before it with the distance on.
        let data = file.section_by_name("__data").unwrap();
        let (_, reloc) = data.relocations().next().unwrap();
        let object::RelocationTarget::Symbol(index) = reloc.target() else { panic!() };
        assert_eq!(file.symbol_by_index(index).unwrap().name(), Ok("_s"));
        assert_eq!(data.data().unwrap()[8], 3);
    }

    /// A distance an unwind record holds, which arm64 says as the place taken from the target:
    /// a `SUBTRACTOR` naming the front of the table, then an `UNSIGNED` naming the function, with
    /// the place's own offset taken off what the bytes hold. That is the pair clang writes and the
    /// only one ld64 reads a record's distances as.
    #[test]
    fn a_distance_in_data_is_a_subtractor_and_an_unsigned() {
        use object::read::macho::MachOFile64;

        let text = part("__TEXT", "__text", vec![0; 8], 8);
        let attributes = ["no_toc", "strip_static_syms", "live_support"];
        let shape = Shape::mach("__TEXT", "__eh_frame", Some("coalesced"), &attributes).unwrap();
        let mut frames = Part { shape, ..part("__TEXT", "__eh_frame", vec![0; 32], 32) };
        let kind = Reference::Data;
        frames.relocs.push(Reloc { at: 8, symbol: "_f".into(), kind, addend: 0, after: 0 });
        let mut f = name("_f", 0, 0, Binding::Global);
        f.sort = Sort::Func;
        let input = Assembled { parts: vec![text, frames], names: vec![f], subsections: true };
        let target = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        let bytes = write(&input, &target, &Info::default()).unwrap();
        let file = MachOFile64::<Endianness>::parse(&bytes[..]).unwrap();
        let section = file.section_by_name("__eh_frame").unwrap();
        let pairs: Vec<_> = section
            .macho_relocations()
            .unwrap()
            .iter()
            .map(|reloc| reloc.info(Endianness::Little))
            .map(|info| (info.r_address, info.r_type, info.r_extern, info.r_length))
            .collect();
        let subtractor = (8, macho::ARM64_RELOC_SUBTRACTOR, true, 2);
        assert_eq!(pairs, [subtractor, (8, macho::ARM64_RELOC_UNSIGNED, true, 2)]);
        assert_eq!(section.data().unwrap()[8..12], (-8i32).to_le_bytes());
    }

    /// A place in one debug section that names another is written as the offset it is, since no
    /// linker moves either, and an address keeps its relocation, against the function's symbol
    /// with the underscore the debug information leaves off.
    #[test]
    fn debug_information_goes_in_the_dwarf_segment_with_only_its_addresses_relocated() {
        use crate::section::Chunk;

        let text = part("__TEXT", "__text", vec![0; 8], 8);
        let mut f = name("_f", 0, 0, Binding::Local);
        f.sort = Sort::Func;
        let input = Assembled { parts: vec![text], names: vec![f], subsections: true };
        let reloc = |at, symbol: &str, bytes, addend| Reloc {
            at,
            symbol: symbol.to_owned(),
            kind: Reference::Address { bytes },
            addend,
            after: 0,
        };
        let unit = Chunk {
            name: ".debug_info".to_owned(),
            bytes: vec![0; 24],
            relocs: vec![reloc(8, ".debug_abbrev", 4, 0x10), reloc(12, "f", 8, 4)],
        };
        let abbrev = Chunk { name: ".debug_abbrev".to_owned(), bytes: vec![0; 32], relocs: vec![] };
        let offsets =
            Chunk { name: ".debug_str_offsets".to_owned(), bytes: vec![0; 8], relocs: vec![] };
        let info = Info { chunks: vec![unit, abbrev, offsets], ..Info::default() };
        let target = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        let bytes = write(&input, &target, &info).unwrap();
        let file = object::File::parse(&bytes[..]).unwrap();
        let unit = file.section_by_name("__debug_info").unwrap();
        assert_eq!(unit.segment_name(), Ok(Some("__DWARF")));
        let SectionFlags::MachO { flags, .. } = unit.flags() else { panic!() };
        assert_eq!(flags, macho::S_ATTR_DEBUG);
        let data = unit.data().unwrap();
        assert_eq!(data[8..12], 0x10u32.to_le_bytes());
        assert_eq!(data[12..20], 4u64.to_le_bytes());
        let relocs: Vec<_> = unit.relocations().collect();
        let [(12, reloc)] = relocs.as_slice() else { panic!("{relocs:?}") };
        let object::RelocationTarget::Symbol(index) = reloc.target() else { panic!() };
        assert_eq!(file.symbol_by_index(index).unwrap().name(), Ok("_f"));
        assert!(file.section_by_name("__debug_str_offs").is_some());
    }

    #[test]
    fn the_low_bits_are_one_relocation_whatever_the_access_is() {
        for fixup in [Fixup::AddLo12, Fixup::Ldst8Lo12, Fixup::Ldst64Lo12, Fixup::Ldst128Lo12] {
            let flags = reloc(Reference::Field(fixup), 8).unwrap();
            assert_eq!(
                flags,
                RelocationFlags::MachO {
                    r_type: macho::ARM64_RELOC_PAGEOFF12,
                    r_pcrel: false,
                    r_length: 2
                }
            );
        }
    }

    #[test]
    fn what_this_format_cannot_say_is_refused() {
        assert!(reloc(Reference::Field(Fixup::CondBr19), 0).is_err());
        assert!(reloc(Reference::Field(Fixup::GotLo12), 8).is_err());
        assert!(reloc(Reference::Field(Fixup::AdrPage21), 1 << 23).is_err());
        assert!(reloc(Reference::Field(Fixup::AdrPage21), -(1 << 23)).is_ok());
        assert!(reloc(Reference::Address { bytes: 8 }, 1 << 40).is_ok());
    }

    #[test]
    fn a_section_is_what_its_type_says_or_what_its_name_implies() {
        let text = section_flags("__TEXT", "__text", Some("regular"), &["pure_instructions"]);
        let text = text.unwrap();
        assert_eq!(text & 0xff, 0);
        assert_ne!(text & macho::S_ATTR_SOME_INSTRUCTIONS.0, 0);
        assert_eq!(section_flags("__TEXT", "__text", None, &[]).unwrap(), text);
        let bss = section_flags("__DATA", "__bss", None, &[]).unwrap();
        assert!(zero_filled(bss));
        let strings = section_flags("__TEXT", "__cstring", Some("cstring_literals"), &[]);
        assert_eq!(strings.unwrap(), u32::from(macho::S_CSTRING_LITERALS.0));
        assert!(section_flags("__DATA", "__data", Some("sideways"), &[]).is_err());
        assert_eq!(split("__DATA,__const"), Ok(("__DATA", "__const")));
        assert!(split(".data").is_err());
        assert!(split("__DATA,__a_name_longer_than_16").is_err());
    }
}
