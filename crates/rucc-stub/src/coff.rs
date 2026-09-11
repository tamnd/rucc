//! Writing the import library a Windows link line wants, from a module definition.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` section 9.4. [`crate::def`] is the reading half and
//! this is the writing half, so between them a `.def` file out of mingw-w64 becomes a file `link` or
//! `ld` can resolve `__imp_GetProcAddress` against.
//!
//! It is the same idea as [`crate::elf`] and it is not the same file. A Windows program does not link
//! against a stub DLL, it links against an archive of tiny objects, one per export, and the linker
//! turns each one it actually used into an entry in the import table of the image it is building. So
//! there is no ELF here with different constants: there is an `ar` archive, two linker symbol indexes
//! that a Windows archive carries and a Unix one does not, three objects of boilerplate, and one
//! 20-byte record per export.
//!
//! # What goes in it
//!
//! Three long format COFF objects, then one short import record per export:
//!
//! - The import descriptor, which is the 20-byte `IMAGE_IMPORT_DESCRIPTOR` for this DLL in
//!   `.idata$2` with the DLL's name in `.idata$6`, three relocations pointing the descriptor at its
//!   own name and at the two thunk arrays, and `__IMPORT_DESCRIPTOR_<library>` defined on it.
//! - The null descriptor, which is 20 zero bytes in `.idata$3` defining
//!   `__NULL_IMPORT_DESCRIPTOR`. Every import library in a link contributes its descriptor to
//!   `.idata$2` and they are terminated by one zero entry, and `$3` sorts after `$2`, so this member
//!   is how the terminator gets written exactly once however many DLLs a program imports from.
//! - The null thunk, which is one zero pointer in each of `.idata$5` and `.idata$4` defining
//!   `\x7f<library>_NULL_THUNK_DATA`. Same trick, per DLL rather than per program, because each
//!   DLL's thunk array needs its own terminator. The leading `\x7f` is deliberate and is in the name
//!   both tools write: it sorts after every letter, which is how the terminator lands at the end of
//!   the array.
//! - Then the exports. Each is 20 bytes of header and two or three strings, and a linker reading one
//!   synthesizes the `__imp_` pointer, the jump thunk and the name table entry itself rather than
//!   reading them out of the file.
//! - Then the aliases, for the few exports that are a second spelling of another export in the same
//!   library. Those are real objects rather than records, two per code export and one per data export,
//!   and they are after the records because an alias has to point at the record that claimed the name.
//!
//! # The two things section 9.4 says must be right
//!
//! Whether an export is code or data. A code import gets `__imp_<name>` and a plain `<name>` that is
//! a jump through it, a data import gets `__imp_<name>` and no plain name at all. Writing a data
//! export as code gives a program that links, because the plain name resolves, and then reads the
//! thunk's instruction bytes as the value of the variable. That is why [`crate::def::Form`] exists and
//! why nothing here defaults it.
//!
//! The i386 decoration. mingw-w64's `.def` files name `GetProcAddress@8`, because that is the symbol
//! an i386 compiler emits a reference to, and `KERNEL32.dll` exports `GetProcAddress`. So the record
//! has to carry both halves: the symbol to define is `_GetProcAddress@8`, with the leading underscore
//! an i386 COFF symbol has and the file does not write, and the name to look for in the DLL is the one
//! with the underscore stripped and the `@8` cut off. That is what `IMPORT_NAME_UNDECORATE` means, it
//! is what mingw-w64's own build asks dlltool for with `-k`, and getting it wrong gives a library that
//! links and then fails at load time against a name the DLL does not export.
//!
//! # What a `== name` means and the three answers to it
//!
//! 108 lines in the corpus say `name == other`, which is the `.def` saying what the DLL exports under
//! its own name. Three quarters of the difficulty of this module is in those 108 lines, because the
//! record has three ways to say it and only one of them is a rename:
//!
//! - A name type reaches it. `X3DAudioCalculate@20 == _X3DAudioCalculate@20` on i386 is the symbol
//!   spelled out, so the record is a plain `NAME`, and `UpdateDriverForPlugAndPlayDevicesA@20 ==
//!   UpdateDriverForPlugAndPlayDevicesA` is the undecorated spelling, which is `UNDECORATE`. Neither
//!   carries a third string, and both are what every other import library for the DLL holds.
//! - This library imports that name anyway. The CRT's `getch == _getch` sits next to `_getch`, so the
//!   answer is a weak alias: `getch` falls back on the symbol `_getch` already has, and nothing imports
//!   the same function twice.
//! - Nobody here imports it, so the record asks the DLL for the name itself with `EXPORTAS` and a
//!   third string.
//!
//! The order matters and is LLVM's. Getting it wrong does not give a broken library, it gives a
//! different one, which is why this was only settled by comparing against the real tool.
//!
//! # Held against the real thing
//!
//! Every structure here was read out of a reference library rather than written from the
//! specification and hoped over. `llvm-dlltool` built an import library for a five export `.def` for
//! x86-64 and for i386, and the bytes of all ten members of each were decoded field by field: the
//! archive headers, both linker members, the seven symbols of the descriptor object with their two
//! different storage classes, the three relocations and their order, the section flags down to the
//! alignment bits, and the `TypeInfo` word of every short record.
//!
//! Then every `.def` file in mingw-w64 was built both ways, ours and `llvm-dlltool`'s, for each of the
//! four architectures, and the 8496 libraries are byte for byte the same file. That comparison is what
//! found the `?` rule, the three answers to a `== name` and the newline the long names member is padded
//! with, none of which a specification mentions. `xtask implib` is the same comparison and runs
//! wherever `llvm-dlltool` is installed.
//!
//! ARM64EC is refused rather than guessed at. Its records carry the mangled spelling of every function
//! and export the plain one, so an ARM64EC import library written like the others would link and then
//! fail to load, and section 6.8 has the target at tier 4 anyway.
//!
//! Reading binutils' own reader settled the half a reference cannot show, which is what a linker does
//! with the bytes. `pe_ILF_build_a_bfd` in `bfd/peicode.h` is where GNU ld turns a short record into
//! symbols, and it is the authority for there being no plain name for a data import, for the
//! `__imp_` spelling, and for `__IMPORT_DESCRIPTOR_` taking the DLL name cut at its last dot, which
//! is the same cut LLVM makes on the writing side.
//!
//! # Where the two tools differ and which one this follows
//!
//! GNU dlltool names its members after the output file and defines `_head_<output file>` rather than
//! `__IMPORT_DESCRIPTOR_<library>`, so its descriptor symbol depends on what the library was called
//! on disk. LLVM's depends on the DLL, which is the thing the descriptor is about, and it is what
//! BFD's reader synthesizes a reference to, so this follows LLVM. A program can link against a
//! mixture either way: the two spellings mean two descriptors for one DLL, which the loader handles
//! and which costs a duplicate import table entry.

use crate::def::{Export, Form, Module};
use core::fmt;
use rucc_tuple::{Arch, ObjectFormat, TargetTuple};

/// Writes an import library for a target.
///
/// The result is a complete archive, ready to be written to a file and put on a link line. Nothing
/// is left to a caller: the linker members are built, the members are padded, and the offsets are
/// resolved.
///
/// `PRIVATE` exports are left out, which is what `PRIVATE` is for. Microsoft's page says it keeps a
/// name out of the import library `LINK` generates, so a program that refers to one gets an
/// undefined symbol, which is the intent.
///
/// # Errors
///
/// Every failure is the description or the target being wrong rather than the writing going wrong,
/// and all of them are found before the first byte. See [`Error`].
pub fn write(module: &Module, target: TargetTuple) -> Result<Vec<u8>, Error> {
    if target.object_format() != ObjectFormat::Coff {
        return Err(Error::NotCoff {
            target: target.to_canonical_string(),
            format: target.object_format().as_str(),
        });
    }
    if target.arch() == Arch::Arm64Ec {
        return Err(Error::Ec);
    }
    let machine =
        machine(target.arch()).ok_or(Error::NoMachine { arch: target.arch().as_str() })?;
    let wide = target.pointer_width() == 64;
    // Only i386 has a leading underscore on a C symbol. Every other Windows architecture dropped it,
    // which is why `__imp_GetProcAddress` is the spelling everywhere else.
    let lead = if target.arch() == Arch::X86 { "_" } else { "" };

    let dll = module.dll();
    if dll.is_empty() {
        return Err(Error::NoLibrary);
    }
    if dll.contains('\0') {
        return Err(Error::NameHasNul { name: dll });
    }
    // `__IMPORT_DESCRIPTOR_windows.ai` for `windows.ai.machinelearning`, which looks wrong and is
    // what both tools do: BFD cuts at the last dot with `strrchr` when it synthesizes the reference,
    // LLVM cuts at the same place when it defines it, and a spelling of our own would simply not
    // resolve.
    let library = match dll.rsplit_once('.') {
        Some((stem, _)) => stem,
        None => dll.as_str(),
    };

    let mut members = vec![descriptor(machine, &dll, library), null_descriptor(machine, library)?];
    members.push(null_thunk(machine, library, wide));

    // The records first and the aliases after, which is the order `llvm-dlltool` writes and the only
    // order that can work. An alias points at the record that claimed the name it wants, so every
    // record has to be in hand before the first alias is written.
    let mut claimed: Vec<(String, String)> = Vec::new();
    let mut aliases: Vec<(String, &Export)> = Vec::new();
    for export in &module.exports {
        if export.private {
            continue;
        }
        let symbol = symbol(&export.name, lead);
        check(export, &symbol)?;
        match kind(export, &symbol, lead) {
            Some(kind) => {
                claimed.push((looked_for(kind, &symbol).to_owned(), symbol.clone()));
                members.push(short(machine, &dll, export, &symbol, kind));
            }
            None => aliases.push((symbol, export)),
        }
    }
    for (symbol, export) in aliases {
        let wanted = export.exported.as_deref().unwrap_or_default();
        // The last record to claim a name wins, which is what assigning into a map gives LLVM and
        // what a linker reading the index would see anyway, since the index holds one member per name.
        match claimed.iter().rev().find(|(name, _)| name == wanted) {
            Some((_, real)) => {
                if export.form != Form::Data {
                    members.push(weak(machine, real, &symbol, false));
                }
                members.push(weak(machine, real, &symbol, true));
            }
            // Nothing in this library imports that name, so there is nobody to alias and the record
            // has to ask the DLL for the name itself.
            None => members.push(short(machine, &dll, export, &symbol, EXPORTAS)),
        }
    }

    Ok(archive(&dll, &members))
}

/// One archive member: the bytes, and the names the linker index has to point at it.
struct Member {
    /// The member's contents, which is a whole COFF object or one short import record.
    body: Vec<u8>,
    /// The external symbols this member defines, in the order it defines them.
    ///
    /// Only definitions. The descriptor object refers to two symbols it does not define, and
    /// indexing those would tell the linker this member answers a question it only asks.
    defines: Vec<String>,
}

/// `IMAGE_FILE_MACHINE_*`, or [`None`] for an architecture with no Windows ABI here.
fn machine(arch: Arch) -> Option<u16> {
    Some(match arch {
        Arch::X86 => 0x014c,
        Arch::Arm => 0x01c4,
        Arch::X86_64 => 0x8664,
        Arch::Aarch64 => 0xaa64,
        // Section 6.8 declines to implement ARM64EC and the matrix keeps it at tier 4, so the number
        // is here because an import library is the one part of it that is only data, and nothing in
        // this workspace generates code for it.
        Arch::Arm64Ec => 0xa641,
        // The rest have no Windows ABI at all, and for most of them no Windows. `write` has refused
        // before here on the object format for every target that is not Windows, so this arm is
        // reached only by a Windows tuple naming one of these, which is a tuple that should not
        // parse.
        Arch::Riscv32
        | Arch::Riscv64
        | Arch::LoongArch64
        | Arch::PowerPc64
        | Arch::S390x
        | Arch::Wasm32 => return None,
    })
}

/// The relocation that puts an image relative address in a 32-bit field, which is the only kind an
/// import descriptor needs.
///
/// Every architecture has its own number for it and they are not interchangeable, so this is a table
/// rather than a constant. The name is `ADDR32NB` or `DIR32NB` depending on whose header you read,
/// and the NB is "no base", meaning the value is relative to the image base rather than absolute,
/// which is what lets a DLL be loaded anywhere.
fn image_relative(machine: u16) -> u16 {
    match machine {
        0x014c => 7,
        // ARM, ARM64 and ARM64EC all spell it 2, and AMD64 spells it 3.
        0x01c4 | 0xaa64 | 0xa641 => 2,
        _ => 3,
    }
}

/// The import descriptor object, which is what names the DLL.
fn descriptor(machine: u16, dll: &str, library: &str) -> Member {
    let name = format!("__IMPORT_DESCRIPTOR_{library}");
    let null = "__NULL_IMPORT_DESCRIPTOR".to_owned();
    let thunk = format!("\x7f{library}_NULL_THUNK_DATA");
    let strings = strings(&[&name, &null, &thunk]);

    // The descriptor itself is 20 zero bytes and three relocations. Every field in it is an address
    // the linker fills in, so the data carries no information at all and the relocations carry all
    // of it.
    let descriptor = 20u32;
    let relocations = 3u32;
    let dll_bytes = dll.len() as u32 + 1;
    let raw = 20 + 2 * 40;
    let relocation_table = raw + descriptor;
    let names = relocation_table + relocations * 10;
    let symbols = names + dll_bytes;

    let mut body = Vec::new();
    header(&mut body, machine, 2, symbols, 7, bits(machine));
    section(&mut body, ".idata$2", descriptor, raw, relocation_table, 3, ALIGN_4);
    section(&mut body, ".idata$6", dll_bytes, names, 0, 0, ALIGN_2);
    body.extend(std::iter::repeat_n(0, descriptor as usize));
    // Field 0xc is `Name` and field 0 is `OriginalFirstThunk`, and they are written in this order
    // because that is the order `llvm-dlltool` writes them in and the file is compared against it.
    // A linker does not care.
    relocation(&mut body, 0x0c, 2, image_relative(machine));
    relocation(&mut body, 0x00, 3, image_relative(machine));
    relocation(&mut body, 0x10, 4, image_relative(machine));
    body.extend_from_slice(dll.as_bytes());
    body.push(0);

    // The two section symbols that are `IMAGE_SYM_CLASS_SECTION` and the one that is
    // `IMAGE_SYM_CLASS_STATIC` are not a mistake being copied. `.idata$6` holds this object's own
    // data, so its symbol is static and defined here; the other three name sections that other
    // members contribute to, so they are section references rather than definitions.
    long_symbol(&mut body, 4, 1, CLASS_EXTERNAL, 0);
    short_symbol(&mut body, ".idata$2", 1, CLASS_SECTION, 0);
    short_symbol(&mut body, ".idata$6", 2, CLASS_STATIC, 0);
    short_symbol(&mut body, ".idata$4", 0, CLASS_SECTION, 0);
    short_symbol(&mut body, ".idata$5", 0, CLASS_SECTION, 0);
    long_symbol(&mut body, 4 + name.len() as u32 + 1, 0, CLASS_EXTERNAL, 0);
    long_symbol(&mut body, 4 + name.len() as u32 + 1 + null.len() as u32 + 1, 0, CLASS_EXTERNAL, 0);
    body.extend_from_slice(&strings);

    Member { body, defines: vec![name] }
}

/// The object holding the one zero entry that ends the descriptor array.
fn null_descriptor(machine: u16, library: &str) -> Result<Member, Error> {
    let _ = library;
    let name = "__NULL_IMPORT_DESCRIPTOR";
    let descriptor = 20u32;
    let raw = 20 + 40;
    let symbols = raw + descriptor;

    let mut body = Vec::new();
    header(&mut body, machine, 1, symbols, 1, bits(machine));
    section(&mut body, ".idata$3", descriptor, raw, 0, 0, ALIGN_4);
    body.extend(std::iter::repeat_n(0, descriptor as usize));
    long_symbol(&mut body, 4, 1, CLASS_EXTERNAL, 0);
    body.extend_from_slice(&strings(&[name]));

    Ok(Member { body, defines: vec![name.to_owned()] })
}

/// The object holding the zero pointer that ends this DLL's thunk arrays.
fn null_thunk(machine: u16, library: &str, wide: bool) -> Member {
    let name = format!("\x7f{library}_NULL_THUNK_DATA");
    let pointer = if wide { 8 } else { 4 };
    let align = if wide { ALIGN_8 } else { ALIGN_4 };
    let raw = 20 + 2 * 40;
    let symbols = raw + 2 * pointer;

    let mut body = Vec::new();
    header(&mut body, machine, 2, symbols, 1, bits(machine));
    section(&mut body, ".idata$5", pointer, raw, 0, 0, align);
    section(&mut body, ".idata$4", pointer, raw + pointer, 0, 0, align);
    body.extend(std::iter::repeat_n(0, 2 * pointer as usize));
    long_symbol(&mut body, 4, 1, CLASS_EXTERNAL, 0);
    body.extend_from_slice(&strings(&[&name]));

    Member { body, defines: vec![name] }
}

/// The symbol an object refers to an export by, which on i386 is the name in the file with an
/// underscore in front of it unless the name already carries its own decoration.
///
/// `isDecorated` in LLVM's `.def` parser, with the mingw rules, and the comment there is worth
/// keeping: a name starting with `@` is fastcall and a name starting with `?` or holding `@@` is a C++
/// mangled name, and all three are already spelled the way an object refers to them. Everything else
/// is a name a compiler would have put an underscore in front of, `GetProcAddress@8` included, because
/// a mingw `.def` writes a stdcall name without the underscore and with the stack size.
fn symbol(name: &str, lead: &str) -> String {
    let decorated = name.starts_with('@') || name.starts_with('?') || name.contains("@@");
    if lead.is_empty() || decorated { name.to_owned() } else { format!("{lead}{name}") }
}

/// Everything that can be wrong with one export, which is checked before anything is written.
fn check(export: &Export, symbol: &str) -> Result<(), Error> {
    if symbol.contains('\0') {
        return Err(Error::NameHasNul { name: export.name.clone() });
    }
    if let Some(exported) = &export.exported {
        if exported.contains('\0') {
            return Err(Error::NameHasNul { name: exported.clone() });
        }
    }
    if export.noname && export.ordinal.is_none() {
        return Err(Error::NoName { name: export.name.clone() });
    }
    Ok(())
}

/// Which name type a record gets, or [`None`] for an export that has to become an alias.
///
/// The order is LLVM's in `writeImportLibrary` and none of it is arbitrary. `NONAME` wins outright,
/// since a record asking for an ordinal has no name to resolve. A `== name` target is not a rule about
/// decoration, it is the answer about the DLL, so the only question left is whether a name type can
/// reach it from the symbol: `UNDECORATE` and `NOPREFIX` are tried first because a record that can say
/// it that way is the record every other library for the DLL holds, then the identity, and a target
/// that none of the three reaches is not a name type at all.
///
/// The last block is what `dlltool -k` does, which is what mingw-w64 asks for when it builds its own
/// import libraries. A mangled name is left alone, which is the one case where a name holding an `@`
/// is not a stack size.
fn kind(export: &Export, symbol: &str, lead: &str) -> Option<u16> {
    if export.noname {
        return Some(ORDINAL);
    }
    let i386 = !lead.is_empty();
    if let Some(wanted) = &export.exported {
        if i386 && looked_for(UNDECORATE, symbol) == wanted.as_str() {
            return Some(UNDECORATE);
        }
        if i386 && looked_for(NOPREFIX, symbol) == wanted.as_str() {
            return Some(NOPREFIX);
        }
        if symbol == wanted.as_str() {
            return Some(NAME);
        }
        return None;
    }
    if !i386 || symbol.starts_with('?') {
        return Some(NAME);
    }
    if symbol.match_indices('@').any(|(at, _)| at > 0) {
        // `GetProcAddress@8` in the file, `_GetProcAddress@8` defined, `GetProcAddress` looked for.
        return Some(UNDECORATE);
    }
    if symbol.starts_with('_') {
        // `_ordinary` defined, `ordinary` looked for.
        return Some(NOPREFIX);
    }
    Some(NAME)
}

/// The name a record asks the DLL for, which is the whole of what a name type means.
///
/// `applyNameType` in LLVM, and the same three rules are in BFD's reader. The character that comes off
/// is any one of `?`, `@` or `_` rather than the underscore alone, and `UNDECORATE` then cuts at the
/// first `@` that is left.
fn looked_for(kind: u16, symbol: &str) -> &str {
    match kind {
        NOPREFIX => undecorated(symbol),
        UNDECORATE => {
            let rest = undecorated(symbol);
            match rest.find('@') {
                Some(at) => &rest[..at],
                None => rest,
            }
        }
        _ => symbol,
    }
}

/// One leading decoration character, where there is one.
fn undecorated(symbol: &str) -> &str {
    match symbol.as_bytes().first() {
        Some(b'?' | b'@' | b'_') => &symbol[1..],
        _ => symbol,
    }
}

/// One export, as the 20-byte record a linker expands into symbols itself.
fn short(machine: u16, dll: &str, export: &Export, symbol: &str, kind: u16) -> Member {
    let form = match export.form {
        Form::Code => 0u16,
        Form::Data => 1,
        Form::Constant => 2,
    };

    let mut body = Vec::new();
    let mut strings = Vec::new();
    strings.extend_from_slice(symbol.as_bytes());
    strings.push(0);
    strings.extend_from_slice(dll.as_bytes());
    strings.push(0);
    // The third string, which only an `EXPORTAS` record carries. A `== name` that a name type could
    // say instead says it that way and the string is not written at all.
    if kind == EXPORTAS {
        if let Some(exported) = &export.exported {
            strings.extend_from_slice(exported.as_bytes());
            strings.push(0);
        }
    }

    u16le(&mut body, 0);
    u16le(&mut body, 0xffff);
    u16le(&mut body, 0);
    u16le(&mut body, machine);
    u32le(&mut body, 0);
    u32le(&mut body, strings.len() as u32);
    u16le(&mut body, export.ordinal.unwrap_or(0));
    u16le(&mut body, form | (kind << 2));
    body.extend_from_slice(&strings);

    // A data import has no plain name, and that is the whole of section 9.4's warning. There is
    // nothing to jump to for a variable, so a linker that let `globalvar` resolve would be handing
    // out the address of a thunk it never wrote.
    let mut defines = vec![format!("__imp_{symbol}")];
    if export.form != Form::Data {
        defines.push(symbol.to_owned());
    }
    Member { body, defines }
}

/// One object making a name a weak alias of another name, which is how an export gets a second
/// spelling that no name type can reach.
///
/// `getch == _getch` in the CRT's `.def` is the case: the DLL exports both and the library has a
/// regular record for `_getch`, so `getch` is written as an alias of it rather than as a second import
/// of the same thing. A code export needs two of these, one for the plain name and one for the
/// `__imp_` pointer, and a data export needs only the pointer, for the same reason a data record has no
/// plain name at all.
///
/// The object itself is almost empty: an empty `.drectve` the linker drops, then five symbols. Two are
/// the `@comp.id` and `@feat.00` absolutes every Microsoft object carries, then the real name as an
/// undefined external, then the alias as a weak external whose one auxiliary record says which symbol
/// index to fall back on and that the search is an alias rather than a library lookup.
fn weak(machine: u16, real: &str, alias: &str, imp: bool) -> Member {
    let lead = if imp { "__imp_" } else { "" };
    let real = format!("{lead}{real}");
    let alias = format!("{lead}{alias}");
    let symbols = 20 + 40;

    let mut body = Vec::new();
    // No `IMAGE_FILE_32BIT_MACHINE` here, where the three objects with data in them carry it. That is
    // what the reference writes and there is no reason in it, so it is copied rather than explained.
    header(&mut body, machine, 1, symbols, 5, 0);
    section(&mut body, ".drectve", 0, 0, 0, 0, DISCARD);
    short_symbol(&mut body, "@comp.id", ABSOLUTE, CLASS_STATIC, 0);
    short_symbol(&mut body, "@feat.00", ABSOLUTE, CLASS_STATIC, 0);
    long_symbol(&mut body, 4, 0, CLASS_EXTERNAL, 0);
    long_symbol(&mut body, 4 + real.len() as u32 + 1, 0, CLASS_WEAK, 1);
    // The auxiliary record: symbol 2 is the undefined external above, and 3 is
    // `IMAGE_WEAK_EXTERN_SEARCH_ALIAS`.
    u32le(&mut body, 2);
    u32le(&mut body, 3);
    body.extend(std::iter::repeat_n(0, 10));
    body.extend_from_slice(&strings(&[&real, &alias]));

    Member { body, defines: vec![alias] }
}

/// `IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_ALIGN_2BYTES | IMAGE_SCN_MEM_READ | MEM_WRITE`.
const ALIGN_2: u32 = 0x0020_0040 | 0xc000_0000;
/// The same with `IMAGE_SCN_ALIGN_4BYTES`.
const ALIGN_4: u32 = 0x0030_0040 | 0xc000_0000;
/// The same with `IMAGE_SCN_ALIGN_8BYTES`.
const ALIGN_8: u32 = 0x0040_0040 | 0xc000_0000;
/// `IMAGE_SCN_LNK_INFO | IMAGE_SCN_LNK_REMOVE`, which is a section the linker reads and does not keep.
const DISCARD: u32 = 0x0000_0a00;

/// `IMAGE_SYM_CLASS_EXTERNAL`.
const CLASS_EXTERNAL: u8 = 2;
/// `IMAGE_SYM_CLASS_STATIC`.
const CLASS_STATIC: u8 = 3;
/// `IMAGE_SYM_CLASS_SECTION`.
const CLASS_SECTION: u8 = 104;
/// `IMAGE_SYM_CLASS_WEAK_EXTERNAL`.
const CLASS_WEAK: u8 = 105;

/// `IMAGE_SYM_ABSOLUTE`, a section number meaning the symbol is a value rather than a place.
const ABSOLUTE: i16 = -1;

/// `IMPORT_OBJECT_ORDINAL`, where the DLL is asked for a number rather than a name.
const ORDINAL: u16 = 0;
/// `IMPORT_OBJECT_NAME`, where the name in the record is the name in the DLL.
const NAME: u16 = 1;
/// `IMPORT_OBJECT_NAME_NO_PREFIX`, where the leading decoration character comes off.
const NOPREFIX: u16 = 2;
/// `IMPORT_OBJECT_NAME_UNDECORATE`, where the prefix comes off and the `@` and what follows it go.
const UNDECORATE: u16 = 3;
/// `IMPORT_OBJECT_NAME_EXPORTAS`, where a third string says what the DLL exports.
const EXPORTAS: u16 = 4;

/// The 20-byte COFF file header. Nothing here has a timestamp, which is claim 5's requirement.
fn header(out: &mut Vec<u8>, machine: u16, sections: u16, symbols: u32, count: u32, flags: u16) {
    u16le(out, machine);
    u16le(out, sections);
    u32le(out, 0);
    u32le(out, symbols);
    u32le(out, count);
    u16le(out, 0);
    u16le(out, flags);
}

/// `IMAGE_FILE_32BIT_MACHINE` on the two 32-bit machines and nothing on the others, which is what the
/// reference writes in the three objects that hold data.
///
/// The flag says the image expects 32-bit addresses and the machine number already says so, which is
/// why it reads as redundant and is still what a Windows toolchain puts there.
fn bits(machine: u16) -> u16 {
    if machine == 0x014c || machine == 0x01c4 { 0x0100 } else { 0 }
}

/// One 40-byte section header. Every section here is initialized data with no virtual address, since
/// an object file has no addresses yet.
fn section(
    out: &mut Vec<u8>,
    name: &str,
    size: u32,
    raw: u32,
    relocations: u32,
    count: u16,
    flags: u32,
) {
    let mut eight = [0u8; 8];
    eight[..name.len()].copy_from_slice(name.as_bytes());
    out.extend_from_slice(&eight);
    u32le(out, 0);
    u32le(out, 0);
    u32le(out, size);
    u32le(out, raw);
    u32le(out, relocations);
    u32le(out, 0);
    u16le(out, count);
    u16le(out, 0);
    u32le(out, flags);
}

/// One 10-byte relocation.
fn relocation(out: &mut Vec<u8>, at: u32, symbol: u32, kind: u16) {
    u32le(out, at);
    u32le(out, symbol);
    u16le(out, kind);
}

/// One 18-byte symbol whose name fits in the eight bytes a symbol record has for it.
fn short_symbol(out: &mut Vec<u8>, name: &str, section: i16, class: u8, aux: u8) {
    let mut eight = [0u8; 8];
    eight[..name.len()].copy_from_slice(name.as_bytes());
    out.extend_from_slice(&eight);
    u32le(out, 0);
    out.extend_from_slice(&section.to_le_bytes());
    u16le(out, 0);
    out.push(class);
    out.push(aux);
}

/// One 18-byte symbol whose name is in the string table, which is four zero bytes and an offset.
fn long_symbol(out: &mut Vec<u8>, at: u32, section: i16, class: u8, aux: u8) {
    u32le(out, 0);
    u32le(out, at);
    u32le(out, 0);
    out.extend_from_slice(&section.to_le_bytes());
    u16le(out, 0);
    out.push(class);
    out.push(aux);
}

/// A COFF string table, which counts its own four byte length.
fn strings(names: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    u32le(&mut out, 0);
    for name in names {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
    }
    let size = out.len() as u32;
    out[..4].copy_from_slice(&size.to_le_bytes());
    out
}

/// The `ar` archive, with the two symbol indexes a Windows linker reads.
///
/// The first is the one `ar` has always had, with its counts and offsets big-endian whatever the
/// machine is, and its names in member order. The second is Microsoft's, little-endian, with the
/// names sorted so a linker can bisect them and an index per name saying which member to pull. Both
/// are present in every import library either tool produces and a linker may read either, so writing
/// one and not the other is a file that works until it meets the other linker.
fn archive(dll: &str, members: &[Member]) -> Vec<u8> {
    // Every member is named after the DLL, which is what `llvm-dlltool` does. It means a long DLL
    // name needs the long names member, since 16 bytes is what a header has for a name.
    let long = dll.len() + 1 > 16;
    let name = if long { "/0".to_owned() } else { format!("{dll}/") };

    let mut flat: Vec<(&str, usize)> = Vec::new();
    for (at, member) in members.iter().enumerate() {
        for define in &member.defines {
            flat.push((define, at));
        }
    }

    let mut first = Vec::new();
    u32be(&mut first, flat.len() as u32);
    for _ in &flat {
        u32be(&mut first, 0);
    }
    for (define, _) in &flat {
        first.extend_from_slice(define.as_bytes());
        first.push(0);
    }
    pad(&mut first, 0);

    let mut sorted: Vec<&(&str, usize)> = flat.iter().collect();
    sorted.sort_by(|one, two| one.0.as_bytes().cmp(two.0.as_bytes()));
    let mut second = Vec::new();
    u32le(&mut second, members.len() as u32);
    for _ in members {
        u32le(&mut second, 0);
    }
    u32le(&mut second, flat.len() as u32);
    for (_, at) in &sorted {
        // One based, and it is an index into the member list rather than an offset.
        u16le(&mut second, *at as u16 + 1);
    }
    for (define, _) in &sorted {
        second.extend_from_slice(define.as_bytes());
        second.push(0);
    }
    pad(&mut second, 0);

    let names = if long {
        let mut names = Vec::from(dll.as_bytes());
        names.push(0);
        pad(&mut names, b'\n');
        Some(names)
    } else {
        None
    };

    // Now that every length is known the offsets can be worked out, which is why the two indexes
    // were built with zeroes in them rather than in one pass.
    let mut at = 8 + 60 + even(first.len()) + 60 + even(second.len());
    if let Some(names) = &names {
        at += 60 + even(names.len());
    }
    let mut offsets = Vec::new();
    for member in members {
        offsets.push(at as u32);
        at += 60 + even(member.body.len());
    }

    for (index, (_, member)) in flat.iter().enumerate() {
        let to = 4 + 4 * index;
        first[to..to + 4].copy_from_slice(&offsets[*member].to_be_bytes());
    }
    for (index, offset) in offsets.iter().enumerate() {
        let to = 4 + 4 * index;
        second[to..to + 4].copy_from_slice(&offset.to_le_bytes());
    }

    let mut out = Vec::from(&b"!<arch>\n"[..]);
    member(&mut out, "/", Mode::Zero, &first);
    member(&mut out, "/", Mode::Zero, &second);
    if let Some(names) = &names {
        member(&mut out, "//", Mode::Blank, names);
    }
    for body in members {
        member(&mut out, &name, Mode::Object, &body.body);
    }
    out
}

/// One byte on the end of a linker member, where one is needed to make its length even.
///
/// The length it declares is the padded one rather than the archive's own padding to an even
/// boundary, which is a distinction with no effect on any reader and is what `llvm-dlltool` writes.
/// The object members are not padded this way and take the archive's `\n` instead.
///
/// The byte is a caller's choice because the two kinds of linker member do not agree on it. The two
/// indexes pad with a zero, which reads as one more empty string, and the long names member pads
/// with a newline, which is what a reader of that member skips over anyway. Neither choice means
/// anything to a reader, so the only reason to tell them apart is byte equality with the tool we are
/// compared against.
fn pad(member: &mut Vec<u8>, with: u8) {
    if member.len() % 2 == 1 {
        member.push(with);
    }
}

/// What goes in the four numeric fields of a member header.
///
/// Three shapes rather than a number, because the reference writes three: zeroes on the linker
/// members, blanks on the long names member, and a mode on the objects.
enum Mode {
    /// A zero in every field, which the two linker members carry.
    Zero,
    /// Nothing at all, which is what the long names member carries.
    Blank,
    /// Zero for the time and the owner, and 644 for the mode, which every object carries.
    Object,
}

/// One member header and its body, padded to an even length.
///
/// Every numeric field is zero, and that is the determinism requirement rather than laziness: a real
/// timestamp or a real uid would put the machine that built the library into the library. The mode is
/// the one exception, because both tools write 0 on the linker members and 644 on the objects and the
/// output is compared against one of them.
fn member(out: &mut Vec<u8>, name: &str, mode: Mode, body: &[u8]) {
    let (time, owner, mode) = match mode {
        Mode::Zero => ("0", "0", "0"),
        Mode::Blank => ("", "", ""),
        Mode::Object => ("0", "0", "644"),
    };
    let header =
        format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", name, time, owner, owner, mode, body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        // `\n` rather than a zero, which is what `ar` has always written and what keeps a text
        // member readable when somebody looks at the file with a pager.
        out.push(b'\n');
    }
}

/// A length rounded up to the even boundary a member starts on.
fn even(length: usize) -> usize {
    length + length % 2
}

fn u16le(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn u32le(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn u32be(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Why an import library could not be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The target's object format is not COFF, so this is the wrong writer for it.
    NotCoff {
        /// The target that was asked for.
        target: String,
        /// What it writes instead.
        format: &'static str,
    },
    /// The target is ARM64EC, whose records need name mangling this module does not do.
    Ec,
    /// The architecture has no Windows ABI, so there is no machine number to put in the records.
    NoMachine {
        /// The architecture that was asked for.
        arch: &'static str,
    },
    /// The module names no DLL, so the records would say which library to import from.
    NoLibrary,
    /// A name contains a zero byte, which the strings after a record cannot hold.
    NameHasNul {
        /// The name.
        name: String,
    },
    /// An export is by ordinal only and has no ordinal.
    ///
    /// [`crate::def::read`] refuses this too, so reaching it means a description was assembled in
    /// code rather than read from a file.
    NoName {
        /// The name.
        name: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotCoff { target, format } => write!(
                f,
                "{target} uses {format} rather than COFF, and an import library is a COFF archive"
            ),
            Error::Ec => write!(
                f,
                "an ARM64EC import library needs every function name mangled and exported as its \
                 plain spelling, which this writer does not do, so it would link and then not load"
            ),
            Error::NoMachine { arch } => {
                write!(f, "there is no Windows machine number for {arch}")
            }
            Error::NoLibrary => {
                write!(f, "the module names no DLL, so an import has nothing to name as its source")
            }
            Error::NameHasNul { name } => {
                write!(f, "`{name}` contains a zero byte, which a name in an import library cannot")
            }
            Error::NoName { name } => write!(
                f,
                "`{name}` is imported by ordinal and has no ordinal, so nothing can reach it"
            ),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::def;

    /// The same five exports the reference library was built from, one of each kind that changes a
    /// record.
    const SAMPLE: &str = "\
LIBRARY sample.dll
EXPORTS
ordinary
decorated@8
byordinal @42 NONAME
globalvar DATA
renamed == realname
";

    fn target(text: &str) -> TargetTuple {
        text.parse().unwrap()
    }

    fn written(text: &str, tuple: &str) -> Vec<u8> {
        write(&def::read(text).unwrap(), target(tuple)).unwrap()
    }

    /// The members of an archive, as the offset, the name and the body.
    fn members(archive: &[u8]) -> Vec<(usize, String, Vec<u8>)> {
        assert_eq!(&archive[..8], b"!<arch>\n");
        let mut out = Vec::new();
        let mut at = 8;
        while at + 60 <= archive.len() {
            let header = &archive[at..at + 60];
            assert_eq!(&header[58..60], b"`\n");
            let name = String::from_utf8_lossy(&header[..16]).trim_end().to_owned();
            let size: usize = String::from_utf8_lossy(&header[48..58]).trim_end().parse().unwrap();
            out.push((at, name, archive[at + 60..at + 60 + size].to_vec()));
            at += 60 + size + size % 2;
        }
        assert_eq!(at, archive.len());
        out
    }

    #[test]
    fn an_import_library_is_three_objects_and_a_record_per_export() {
        let archive = written(SAMPLE, "x86_64-windows-gnu");
        let members = members(&archive);
        assert_eq!(members.len(), 2 + 3 + 5);
        assert_eq!(members[0].1, "/");
        assert_eq!(members[1].1, "/");
        for member in &members[2..] {
            assert_eq!(member.1, "sample.dll/");
        }
        // The three objects, by the section each one contributes to.
        assert!(members[2].2.windows(8).any(|at| at == b".idata$2"));
        assert!(members[3].2.windows(8).any(|at| at == b".idata$3"));
        assert!(members[4].2.windows(8).any(|at| at == b".idata$4"));
        // And the five records, which are the ones that start with the short import signature.
        for member in &members[5..] {
            assert_eq!(&member.2[..6], &[0, 0, 0xff, 0xff, 0, 0]);
        }
    }

    #[test]
    fn a_data_export_has_no_plain_name_and_a_function_has_both() {
        // Section 9.4's one quiet failure. A plain `globalvar` would resolve to a thunk, and the
        // program would read the bytes of a jump instruction as the value of a variable.
        let archive = written(SAMPLE, "x86_64-windows-gnu");
        let index = members(&archive)[0].2.clone();
        let names: Vec<&[u8]> = index[4 + 4 * 12..].split(|byte| *byte == 0).collect();
        let has = |name: &[u8]| names.contains(&name);
        assert!(has(b"__imp_globalvar"));
        assert!(!has(b"globalvar"));
        assert!(has(b"__imp_ordinary"));
        assert!(has(b"ordinary"));
    }

    #[test]
    fn the_i386_records_carry_both_halves_of_the_decoration() {
        let archive = written(SAMPLE, "i686-windows-gnu");
        let members = members(&archive);
        let kind = |member: &Vec<u8>| u16::from_le_bytes([member[18], member[19]]);
        let strings = |member: &Vec<u8>| {
            String::from_utf8_lossy(&member[20..]).trim_end_matches('\0').replace('\0', " ")
        };

        // `_ordinary` is defined and `ordinary` is what the DLL exports, so the record says to take
        // the prefix off.
        assert_eq!(kind(&members[5].2), NOPREFIX << 2);
        assert_eq!(strings(&members[5].2), "_ordinary sample.dll");
        // `_decorated@8` is defined and `decorated` is what the DLL exports, which needs both the
        // prefix and everything from the `@` taken off.
        assert_eq!(kind(&members[6].2), UNDECORATE << 2);
        assert_eq!(strings(&members[6].2), "_decorated@8 sample.dll");
        // The DLL has no name for this one at all.
        assert_eq!(kind(&members[7].2), ORDINAL << 2);
        assert_eq!(u16::from_le_bytes([members[7].2[16], members[7].2[17]]), 42);
        // Data, which is the low two bits rather than the name type.
        assert_eq!(kind(&members[8].2), 1 | NOPREFIX << 2);
        // And the one the file answered outright, where no rule about decoration applies.
        assert_eq!(kind(&members[9].2), EXPORTAS << 2);
        assert_eq!(strings(&members[9].2), "_renamed sample.dll realname");

        // Every record names the machine, and it is i386 here rather than whatever the host is.
        for member in &members[5..] {
            assert_eq!(u16::from_le_bytes([member.2[6], member.2[7]]), 0x014c);
        }
    }

    #[test]
    fn x86_64_takes_the_name_as_written_and_no_underscore() {
        let archive = written(SAMPLE, "x86_64-windows-gnu");
        let members = members(&archive);
        let kind = |member: &Vec<u8>| u16::from_le_bytes([member[18], member[19]]);
        assert_eq!(kind(&members[5].2), NAME << 2);
        assert_eq!(&members[5].2[20..29], b"ordinary\0");
        // Still a name rather than something undecorated, because `@8` is part of the name on a
        // machine that never decorated anything.
        assert_eq!(kind(&members[6].2), NAME << 2);
        assert_eq!(&members[6].2[20..32], b"decorated@8\0");
    }

    #[test]
    fn the_descriptor_symbol_takes_the_dll_name_cut_at_its_last_dot() {
        let defines = |text: &str| {
            let archive = written(text, "x86_64-windows-gnu");
            let index = members(&archive)[0].2.clone();
            let count = u32::from_be_bytes([index[0], index[1], index[2], index[3]]) as usize;
            String::from_utf8_lossy(&index[4 + 4 * count..]).split('\0').next().unwrap().to_owned()
        };
        assert_eq!(defines("LIBRARY sample.dll\nEXPORTS\nf\n"), "__IMPORT_DESCRIPTOR_sample");
        // No dot, so `Module::dll` put one there and the cut takes it back off.
        assert_eq!(
            defines("LIBRARY api-ms-win-core-apiquery-l2-1-0\nEXPORTS\nf\n"),
            "__IMPORT_DESCRIPTOR_api-ms-win-core-apiquery-l2-1-0"
        );
        // And the one that reads wrongly and is right. BFD cuts at the last dot when it synthesizes
        // the reference, so this is the spelling that resolves.
        assert_eq!(
            defines("LIBRARY windows.ai.machinelearning\nEXPORTS\nf\n"),
            "__IMPORT_DESCRIPTOR_windows.ai"
        );
    }

    #[test]
    fn a_long_dll_name_gets_the_long_names_member() {
        let archive =
            written("LIBRARY api-ms-win-core-apiquery-l2-1-0\nEXPORTS\nf\n", "x86_64-windows-gnu");
        let members = members(&archive);
        assert_eq!(members[2].1, "//");
        assert_eq!(members[2].2, b"api-ms-win-core-apiquery-l2-1-0.dll\0");
        for member in &members[3..] {
            assert_eq!(member.1, "/0");
        }

        // That name and its terminator come to an even length. One that does not has a newline put
        // on the end and declares the longer size, which is the one thing about this member that a
        // specification does not tell you and a reference library does.
        let odd =
            written("LIBRARY windows.ai.machinelearning.dll\nEXPORTS\nf\n", "x86_64-windows-gnu");
        let odd = self::members(&odd);
        assert_eq!(odd[2].2, b"windows.ai.machinelearning.dll\0\n");
    }

    #[test]
    fn both_linker_members_point_at_the_members_that_define_the_names() {
        let archive = written(SAMPLE, "x86_64-windows-gnu");
        let members = members(&archive);
        let first = &members[0].2;
        let count = u32::from_be_bytes([first[0], first[1], first[2], first[3]]) as usize;
        assert_eq!(count, 12);

        // The first index is in member order and its numbers are offsets, so every one of them has
        // to land on a member header.
        let offsets: Vec<usize> = (0..count)
            .map(|at| {
                let to = 4 + 4 * at;
                u32::from_be_bytes(first[to..to + 4].try_into().unwrap()) as usize
            })
            .collect();
        let starts: Vec<usize> = members.iter().map(|member| member.0).collect();
        for offset in &offsets {
            assert!(starts.contains(offset), "{offset} is not where a member starts");
        }

        // The second is sorted and its numbers are one based member indexes, so the two have to
        // agree about which member every name is in.
        let second = &members[1].2;
        let many = u32::from_le_bytes(second[0..4].try_into().unwrap()) as usize;
        assert_eq!(many, members.len() - 2);
        let at = 4 + 4 * many;
        assert_eq!(u32::from_le_bytes(second[at..at + 4].try_into().unwrap()) as usize, count);
        let names: Vec<&str> = std::str::from_utf8(&second[at + 4 + 2 * count..])
            .unwrap()
            .split_terminator('\0')
            .collect();
        let mut ordered = names.clone();
        ordered.sort_by_key(|name| name.as_bytes());
        assert_eq!(names, ordered, "the second linker member is bisected, so it has to be sorted");

        for (index, name) in names.iter().enumerate() {
            let to = at + 4 + 2 * index;
            let member = u16::from_le_bytes([second[to], second[to + 1]]) as usize;
            let body = &members[member + 1].2;
            let offset = members[member + 1].0;
            let _ = body;
            let mut seen = None;
            for (at, candidate) in (0..count).zip(first_names(first, count)) {
                if candidate == *name {
                    seen = Some(offsets[at]);
                }
            }
            assert_eq!(seen, Some(offset), "the two indexes disagree about `{name}`");
        }
    }

    /// The names in the first linker member, in its order.
    fn first_names(first: &[u8], count: usize) -> Vec<String> {
        std::str::from_utf8(&first[4 + 4 * count..])
            .unwrap()
            .split_terminator('\0')
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_private_export_is_left_out() {
        // Microsoft's page says PRIVATE keeps a name out of the import library, so a program that
        // calls it gets an undefined symbol rather than an import.
        let archive =
            written("LIBRARY k.dll\nEXPORTS\nf\nDllRegisterServer PRIVATE\n", "x86_64-windows-gnu");
        let members = members(&archive);
        assert_eq!(members.len(), 2 + 3 + 1);
        let first = &members[0].2;
        let count = u32::from_be_bytes(first[0..4].try_into().unwrap()) as usize;
        assert!(!first_names(first, count).iter().any(|name| name.contains("DllRegisterServer")));
    }

    #[test]
    fn the_thunk_terminator_is_a_pointer_wide() {
        let size = |tuple: &str| {
            let archive = written(SAMPLE, tuple);
            let body = members(&archive)[4].2.clone();
            // The first section header starts at 20 and its raw size is 16 bytes into it.
            u32::from_le_bytes(body[36..40].try_into().unwrap())
        };
        assert_eq!(size("x86_64-windows-gnu"), 8);
        assert_eq!(size("i686-windows-gnu"), 4);
        assert_eq!(size("aarch64-windows-gnu"), 8);
    }

    #[test]
    fn the_same_module_twice_is_the_same_bytes_and_nothing_carries_a_time() {
        let once = written(SAMPLE, "x86_64-windows-gnu");
        let twice = written(SAMPLE, "x86_64-windows-gnu");
        assert_eq!(once, twice);
        // An `ar` header has a timestamp field and so does a COFF header and so does a short import
        // record, and a build that fills any of them in is a build whose output depends on when it
        // ran. The two linker members are not objects, so they are not looked at here, and their
        // headers are checked by the member reader every test goes through.
        for (_, _, body) in members(&once).into_iter().skip(2) {
            let short = body.starts_with(&[0, 0, 0xff, 0xff]);
            let at = if short { 8 } else { 4 };
            assert_eq!(&body[at..at + 4], &[0, 0, 0, 0], "something in here carries a time");
        }
    }

    #[test]
    fn a_rename_this_library_imports_anyway_becomes_an_alias_of_it() {
        // The CRT's own shape: the DLL exports both spellings, so `llvm-dlltool` writes one record for
        // `_getch` and makes `getch` a weak alias of it rather than a second import of the same thing.
        // Two objects, because the plain name and the `__imp_` pointer each need one.
        let archive =
            written("LIBRARY c.dll\nEXPORTS\n_getch\ngetch == _getch\n", "x86_64-windows-gnu");
        let members = members(&archive);
        assert_eq!(members.len(), 2 + 3 + 1 + 2);
        let aliases = &members[6..];
        for (_, _, body) in aliases {
            // A weak external is an object rather than a record, and it holds one `.drectve` section
            // the linker drops and five symbols.
            assert!(!body.starts_with(&[0, 0, 0xff, 0xff]));
            assert_eq!(&body[20..28], b".drectve");
            assert_eq!(u32::from_le_bytes(body[12..16].try_into().unwrap()), 5);
        }
        // The strings are the name it falls back on and then the name it defines, in that order.
        let strings = |body: &Vec<u8>| {
            String::from_utf8_lossy(&body[60 + 5 * 18 + 4..])
                .trim_end_matches('\0')
                .replace('\0', " ")
        };
        assert_eq!(strings(&aliases[0].2), "_getch getch");
        assert_eq!(strings(&aliases[1].2), "__imp__getch __imp_getch");

        // And the index has to name the alias, since the alias is the only thing these two members
        // define and a linker finds them by looking a name up.
        let first = &members[0].2;
        let count = u32::from_be_bytes(first[0..4].try_into().unwrap()) as usize;
        let names = first_names(first, count);
        assert!(names.contains(&"getch".to_owned()));
        assert!(names.contains(&"__imp_getch".to_owned()));
    }

    #[test]
    fn a_renamed_data_export_gets_the_pointer_alias_and_no_other() {
        // Same reason a data record has no plain name: there is no thunk to jump to, so the plain
        // spelling has to stay undefined.
        let archive =
            written("LIBRARY c.dll\nEXPORTS\n_v DATA\nv == _v DATA\n", "x86_64-windows-gnu");
        let members = members(&archive);
        assert_eq!(members.len(), 2 + 3 + 1 + 1);
        let body = &members[6].2;
        let strings = String::from_utf8_lossy(&body[60 + 5 * 18 + 4..])
            .trim_end_matches('\0')
            .replace('\0', " ");
        assert_eq!(strings, "__imp__v __imp_v");
    }

    #[test]
    fn a_rename_nothing_here_imports_asks_the_dll_for_the_name() {
        // No record in this library claims `realname`, so there is nothing to alias and the record has
        // to carry the name itself, which is what the third string of an EXPORTAS record is for.
        let archive = written(SAMPLE, "x86_64-windows-gnu");
        let members = members(&archive);
        assert_eq!(members.len(), 2 + 3 + 5);
        let body = &members[9].2;
        assert_eq!(u16::from_le_bytes([body[18], body[19]]), EXPORTAS << 2);
        assert_eq!(
            String::from_utf8_lossy(&body[20..]).trim_end_matches('\0').replace('\0', " "),
            "renamed sample.dll realname"
        );
    }

    #[test]
    fn a_rename_a_name_type_can_say_is_said_that_way_instead() {
        // Two real files. `X3DAudio1_2.def` names the export with the underscore the i386 symbol has
        // anyway, so the record is a plain NAME and carries no third string at all.
        let archive = written(
            "LIBRARY X3DAudio1_2.dll\nEXPORTS\nX3DAudioCalculate@20 == _X3DAudioCalculate@20\n",
            "i686-windows-gnu",
        );
        let body = &members(&archive)[5].2;
        assert_eq!(u16::from_le_bytes([body[18], body[19]]), NAME << 2);
        assert_eq!(
            String::from_utf8_lossy(&body[20..]).trim_end_matches('\0').replace('\0', " "),
            "_X3DAudioCalculate@20 X3DAudio1_2.dll"
        );

        // `newdev.def` names the undecorated export, which is exactly what UNDECORATE means, so that
        // is what it gets rather than a rename nobody needs to read.
        let archive = written(
            "LIBRARY newdev.dll\nEXPORTS\nUpdateDriverA@20==UpdateDriverA\n",
            "i686-windows-gnu",
        );
        let body = &members(&archive)[5].2;
        assert_eq!(u16::from_le_bytes([body[18], body[19]]), UNDECORATE << 2);
        assert_eq!(
            String::from_utf8_lossy(&body[20..]).trim_end_matches('\0').replace('\0', " "),
            "_UpdateDriverA@20 newdev.dll"
        );
    }

    #[test]
    fn a_name_that_carries_its_own_decoration_gets_no_underscore_on_i386() {
        // 9648 names in the mingw-w64 32-bit definitions start with `?`, which is a C++ mangled name.
        // It already says everything about itself, so nothing is added to it and nothing is taken off
        // it, and the `@` in the middle of it is not a stack size.
        let archive = written(
            "LIBRARY p.dll\nEXPORTS\n?mangled@@YAXXZ\n@fastcall@8\nplain@8\nplain\n",
            "i686-windows-gnu",
        );
        let members = members(&archive);
        let say = |at: usize| {
            let body = &members[at].2;
            (
                u16::from_le_bytes([body[18], body[19]]) >> 2,
                String::from_utf8_lossy(&body[20..])
                    .split('\0')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            )
        };
        assert_eq!(say(5), (NAME, "?mangled@@YAXXZ".to_owned()));
        // Fastcall keeps its `@` prefix and loses the stack size, which is the one case where the
        // prefix that comes off is not an underscore.
        assert_eq!(say(6), (UNDECORATE, "@fastcall@8".to_owned()));
        assert_eq!(say(7), (UNDECORATE, "_plain@8".to_owned()));
        assert_eq!(say(8), (NOPREFIX, "_plain".to_owned()));
    }

    #[test]
    fn arm64ec_is_refused_rather_than_written_wrong() {
        let module = def::read(SAMPLE).unwrap();
        let error = write(&module, target("arm64ec-windows-msvc")).unwrap_err();
        assert_eq!(error, Error::Ec);
    }

    #[test]
    fn a_target_that_is_not_windows_is_refused() {
        let module = def::read(SAMPLE).unwrap();
        let error = write(&module, target("x86_64-linux-gnu")).unwrap_err();
        assert!(matches!(error, Error::NotCoff { .. }));
        assert!(error.to_string().contains("elf"));
    }

    #[test]
    fn every_error_says_something_a_person_can_act_on() {
        let messages = [
            Error::NotCoff { target: "x86_64-linux-gnu".to_owned(), format: "elf" },
            Error::Ec,
            Error::NoMachine { arch: "riscv64" },
            Error::NoLibrary,
            Error::NameHasNul { name: "f".to_owned() },
            Error::NoName { name: "f".to_owned() },
        ];
        for error in messages {
            let said = error.to_string();
            assert!(said.len() > 30, "`{said}` is too short to tell anybody anything");
        }
    }
}
