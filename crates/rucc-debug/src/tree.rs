//! The entries in `.debug_info`: the types a unit describes and the functions it defines.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! A line table says where an address came from and this says what is there. The two go in the same
//! unit and are written in one pass over it, and they are separate files because they are separate
//! questions: the table is right or wrong against `addr2line`, and an entry here is right or wrong
//! against a debugger that stops inside a function and prints something.
//!
//! # Two passes over the table of types
//!
//! Every entry is created with its tag first and filled in afterwards. A type can name a type that
//! comes later in the table, and `struct node { struct node *next; }` names itself, so the
//! identifier an attribute has to hold may not exist yet when the entry that wants it is reached.
//! Creating the tags first means every identifier exists before any attribute is set, which turns
//! the recursive case into the ordinary one.
//!
//! The order the entries come out in is the order the table was built in, which is the order the
//! driver walked the types in. Nothing about DWARF depends on it: an entry is found by its offset
//! through an attribute rather than by being in any particular place.
//!
//! # A function with no signature gets no entry
//!
//! [`Function::sig`] is [`None`] for a function whose signature this compiler cannot yet fully
//! describe, and such a function gets no `DW_TAG_subprogram` at all. The alternative is an entry
//! with no `DW_AT_type`, and in DWARF that is not silence, it is the word `void`. A debugger given
//! no entry falls back to the symbol table, which is what it does for every function compiled by
//! this compiler today, and a debugger given a wrong return type has no way to find out.
//!
//! [`Function::sig`]: crate::Function::sig

use crate::line::{Error, Function};
use crate::shape::{Encoding, Member, Qualifier, Shape, Sig};

use gimli::write::{AttributeValue, FileId, UnitEntryId};

/// Everything a unit says about what its addresses mean, added to a unit that already has a line
/// program.
///
/// The type entries first and the functions after, so that a subprogram's `DW_AT_type` names an
/// entry that is already there.
///
/// # Errors
///
/// [`Error::Refused`] when something names a type the table does not have. Every index here was
/// built by this compiler, so that is a bug here rather than a program's mistake.
pub(crate) fn describe(
    dwarf: &mut gimli::write::DwarfUnit,
    shapes: &[Shape],
    files: &[FileId],
    funcs: &[Function],
) -> Result<(), Error> {
    let ids = kinds(dwarf, shapes);
    for (shape, &id) in shapes.iter().zip(&ids) {
        fill(dwarf, shape, id, &ids)?;
    }
    for (index, func) in funcs.iter().enumerate() {
        let Some(sig) = &func.sig else { continue };
        defined(dwarf, func, sig, index, files, &ids)?;
    }
    Ok(())
}

/// An entry for every type, holding its tag and nothing else yet.
fn kinds(dwarf: &mut gimli::write::DwarfUnit, shapes: &[Shape]) -> Vec<UnitEntryId> {
    let root = dwarf.unit.root();
    shapes.iter().map(|shape| dwarf.unit.add(root, tag(shape))).collect()
}

/// Which DWARF tag a shape is written as.
fn tag(shape: &Shape) -> gimli::DwTag {
    match shape {
        Shape::Base { .. } => gimli::DW_TAG_base_type,
        Shape::Pointer { .. } => gimli::DW_TAG_pointer_type,
        Shape::Array { .. } => gimli::DW_TAG_array_type,
        Shape::Record { union: false, .. } => gimli::DW_TAG_structure_type,
        Shape::Record { union: true, .. } => gimli::DW_TAG_union_type,
        Shape::Enumeration { .. } => gimli::DW_TAG_enumeration_type,
        Shape::Alias { .. } => gimli::DW_TAG_typedef,
        Shape::Qualified { which: Qualifier::Const, .. } => gimli::DW_TAG_const_type,
        Shape::Qualified { which: Qualifier::Volatile, .. } => gimli::DW_TAG_volatile_type,
        Shape::Qualified { which: Qualifier::Restrict, .. } => gimli::DW_TAG_restrict_type,
        Shape::Qualified { which: Qualifier::Atomic, .. } => gimli::DW_TAG_atomic_type,
        Shape::Subroutine(_) => gimli::DW_TAG_subroutine_type,
    }
}

/// The attributes and the children of one type's entry.
fn fill(
    dwarf: &mut gimli::write::DwarfUnit,
    shape: &Shape,
    at: UnitEntryId,
    ids: &[UnitEntryId],
) -> Result<(), Error> {
    match shape {
        Shape::Base { name, encoding, size } => {
            title(dwarf, at, name);
            let read = AttributeValue::Encoding(reading(*encoding));
            dwarf.unit.get_mut(at).set(gimli::DW_AT_encoding, read);
            bytes(dwarf, at, *size);
        }
        Shape::Pointer { to, size } => {
            bytes(dwarf, at, *size);
            points(dwarf, at, *to, ids)?;
        }
        Shape::Array { of, count } => elements(dwarf, at, *of, *count, ids)?,
        Shape::Record { name, size, members, .. } => {
            if let Some(name) = name {
                title(dwarf, at, name);
            }
            if let Some(size) = size {
                bytes(dwarf, at, *size);
            }
            match members {
                // An incomplete record says so, rather than saying it is a record with no members.
                // The two are different types in C and a debugger that could not tell them apart
                // would print `{}` for a pointer to something it has never been shown.
                None => flag(dwarf, at, gimli::DW_AT_declaration),
                Some(members) => {
                    for member in members {
                        held(dwarf, at, member, ids)?;
                    }
                }
            }
        }
        Shape::Enumeration { name, of, size } => {
            if let Some(name) = name {
                title(dwarf, at, name);
            }
            bytes(dwarf, at, *size);
            points(dwarf, at, Some(*of), ids)?;
        }
        Shape::Alias { name, of } => {
            title(dwarf, at, name);
            points(dwarf, at, *of, ids)?;
        }
        Shape::Qualified { of, .. } => points(dwarf, at, *of, ids)?,
        Shape::Subroutine(sig) => takes(dwarf, at, sig, ids)?,
    }
    Ok(())
}

/// The entry for one function this unit defines.
///
/// `DW_AT_low_pc` asks the linker where the function went, the same way the line program's sequence
/// for it does and against the same symbol. `DW_AT_high_pc` is a length rather than an address,
/// which is DWARF 4 and later and is what lets one relocation do for both.
///
/// No `DW_AT_frame_base`, because nothing here needs one yet. A frame base is what a variable's
/// location is measured from, there are no variables yet, and the two answers worth having are the
/// call frame address, which needs the unwind tables this compiler does not write, and a register,
/// which is only right if the frame really is laid out that way. That choice belongs with the
/// locations it would be read through rather than here.
fn defined(
    dwarf: &mut gimli::write::DwarfUnit,
    func: &Function,
    sig: &Sig,
    index: usize,
    files: &[FileId],
    ids: &[UnitEntryId],
) -> Result<(), Error> {
    let root = dwarf.unit.root();
    let at = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
    title(dwarf, at, &func.name);
    if func.external {
        flag(dwarf, at, gimli::DW_AT_external);
    }
    if let Some(place) = func.decl {
        let Some(&file) = files.get(place.file) else {
            let why = format!("{} names file {}, which is not one", func.name, place.file);
            return Err(Error::Refused { why });
        };
        let entry = dwarf.unit.get_mut(at);
        entry.set(gimli::DW_AT_decl_file, AttributeValue::FileIndex(Some(file)));
        entry.set(gimli::DW_AT_decl_line, AttributeValue::Udata(u64::from(place.line)));
    }
    let entry = dwarf.unit.get_mut(at);
    let start = gimli::write::Address::Symbol { symbol: index, addend: 0 };
    entry.set(gimli::DW_AT_low_pc, AttributeValue::Address(start));
    entry.set(gimli::DW_AT_high_pc, AttributeValue::Udata(func.len));
    takes(dwarf, at, sig, ids)
}

/// What a signature says, which is the same attributes on a subprogram and on a function type.
fn takes(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    sig: &Sig,
    ids: &[UnitEntryId],
) -> Result<(), Error> {
    if sig.prototyped {
        flag(dwarf, at, gimli::DW_AT_prototyped);
    }
    points(dwarf, at, sig.returns, ids)?;
    for param in &sig.params {
        let child = dwarf.unit.add(at, gimli::DW_TAG_formal_parameter);
        if let Some(name) = &param.name {
            title(dwarf, child, name);
        }
        points(dwarf, child, Some(param.ty), ids)?;
    }
    if sig.variadic {
        dwarf.unit.add(at, gimli::DW_TAG_unspecified_parameters);
    }
    Ok(())
}

/// One member of a record, as a child of the record's own entry.
fn held(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    member: &Member,
    ids: &[UnitEntryId],
) -> Result<(), Error> {
    let child = dwarf.unit.add(at, gimli::DW_TAG_member);
    if let Some(name) = &member.name {
        title(dwarf, child, name);
    }
    points(dwarf, child, Some(member.ty), ids)?;
    let entry = dwarf.unit.get_mut(child);
    match member.bits {
        // Bits from the start of the record rather than from the start of a storage unit, which is
        // the DWARF 4 spelling and needs a reader to work out which unit was meant and which way
        // round the target is. This one says where the bits are and nothing else.
        Some(bits) => {
            entry.set(gimli::DW_AT_data_bit_offset, AttributeValue::Udata(bits.at));
            entry.set(gimli::DW_AT_bit_size, AttributeValue::Udata(bits.width));
        }
        None => entry.set(gimli::DW_AT_data_member_location, AttributeValue::Udata(member.at)),
    }
    Ok(())
}

/// What an array holds and how many, the count being a child entry rather than an attribute.
///
/// An array with no count it can state writes the child with no bound on it, which is what a
/// flexible array member and an array of unknown length both look like and is what gcc writes for
/// them. The array whose length is an expression ends up here too, and DWARF could describe that
/// one properly, which is worth doing and is not done yet.
fn elements(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    of: usize,
    count: Option<u64>,
    ids: &[UnitEntryId],
) -> Result<(), Error> {
    points(dwarf, at, Some(of), ids)?;
    let child = dwarf.unit.add(at, gimli::DW_TAG_subrange_type);
    if let Some(count) = count.filter(|&count| count > 0) {
        let last = AttributeValue::Udata(count - 1);
        dwarf.unit.get_mut(child).set(gimli::DW_AT_upper_bound, last);
    }
    Ok(())
}

/// A `DW_AT_type` naming another entry, and nothing at all for `void`.
fn points(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    of: Option<usize>,
    ids: &[UnitEntryId],
) -> Result<(), Error> {
    let Some(of) = of else { return Ok(()) };
    let Some(&target) = ids.get(of) else {
        let why = format!("an entry names type {of}, which is not one");
        return Err(Error::Refused { why });
    };
    dwarf.unit.get_mut(at).set(gimli::DW_AT_type, AttributeValue::UnitRef(target));
    Ok(())
}

/// A `DW_AT_name`, through `.debug_str` so that a name used twice is written once.
fn title(dwarf: &mut gimli::write::DwarfUnit, at: UnitEntryId, name: &str) {
    let id = dwarf.strings.add(name.to_owned());
    dwarf.unit.get_mut(at).set(gimli::DW_AT_name, AttributeValue::StringRef(id));
}

/// A `DW_AT_byte_size`.
fn bytes(dwarf: &mut gimli::write::DwarfUnit, at: UnitEntryId, size: u64) {
    dwarf.unit.get_mut(at).set(gimli::DW_AT_byte_size, AttributeValue::Udata(size));
}

/// An attribute whose being there is the whole of what it says.
fn flag(dwarf: &mut gimli::write::DwarfUnit, at: UnitEntryId, which: gimli::DwAt) {
    dwarf.unit.get_mut(at).set(which, AttributeValue::FlagPresent);
}

/// How a base type's bits are read, as DWARF spells it.
fn reading(encoding: Encoding) -> gimli::DwAte {
    match encoding {
        Encoding::Boolean => gimli::DW_ATE_boolean,
        Encoding::Signed => gimli::DW_ATE_signed,
        Encoding::Unsigned => gimli::DW_ATE_unsigned,
        Encoding::SignedChar => gimli::DW_ATE_signed_char,
        Encoding::UnsignedChar => gimli::DW_ATE_unsigned_char,
        Encoding::Float => gimli::DW_ATE_float,
        Encoding::Complex => gimli::DW_ATE_complex_float,
    }
}

#[cfg(test)]
mod tests {
    use crate::line::{Row, Unit, write};
    use crate::shape::{Member, Param, Place, Shape, Sig};

    use rucc_object::{Info, Reference};

    use super::*;

    /// A unit with one function in it, one type, and a row so that anything is written at all.
    fn one() -> Unit {
        Unit {
            name: "a.c".to_owned(),
            dir: "/tmp".to_owned(),
            producer: "rucc".to_owned(),
            files: vec!["a.c".to_owned()],
            types: vec![Shape::Base {
                name: "int".to_owned(),
                encoding: Encoding::Signed,
                size: 4,
            }],
            funcs: vec![Function {
                name: "f".to_owned(),
                len: 16,
                rows: vec![Row { at: 0, file: 0, line: 3, column: 1 }],
                decl: Some(Place { file: 0, line: 3 }),
                sig: Some(Sig {
                    returns: Some(0),
                    params: vec![Param { name: Some("n".to_owned()), ty: 0 }],
                    variadic: false,
                    prototyped: true,
                }),
                external: true,
            }],
            pointer: 8,
        }
    }

    /// Every name that went into `.debug_str`, which is where a name on an entry goes.
    ///
    /// The section is names with a null byte after each, so splitting on the byte is reading it.
    /// Nothing else writes there, so what is in it is exactly what the entries named.
    fn named(info: &Info) -> Vec<String> {
        let Some(chunk) = info.chunks.iter().find(|chunk| chunk.name == ".debug_str") else {
            return Vec::new();
        };
        chunk
            .bytes
            .split(|&byte| byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect()
    }

    /// A function with a signature gets an entry, and the entry asks the linker where it went.
    ///
    /// The relocation is what says an entry is there at all: `DW_AT_low_pc` is the one attribute
    /// on it that no compilation can fill in, so an address relocation in `.debug_info` naming the
    /// function exists if and only if a subprogram was written for it.
    #[test]
    fn a_function_with_a_signature_gets_an_entry_the_linker_fills_in() {
        let info = write(&one()).expect("sections");
        let unit = info.chunks.iter().find(|chunk| chunk.name == ".debug_info").expect("a unit");
        let at = unit.relocs.iter().find(|reloc| reloc.symbol == "f").expect("an address");
        assert_eq!(at.kind, Reference::Address { bytes: 8 });
        assert_eq!(at.addend, 0);
        let names = named(&info);
        assert!(names.contains(&"f".to_owned()), "the function, {names:?}");
        assert!(names.contains(&"n".to_owned()), "its parameter, {names:?}");
        assert!(names.contains(&"int".to_owned()), "the type, {names:?}");
    }

    /// A function whose signature could not be described gets no entry rather than half of one.
    #[test]
    fn a_function_with_no_signature_gets_no_entry() {
        let mut unit = one();
        unit.types.clear();
        unit.funcs[0].sig = None;
        unit.funcs[0].decl = None;
        let info = write(&unit).expect("sections");
        let held = info.chunks.iter().find(|chunk| chunk.name == ".debug_info").expect("a unit");
        assert!(held.relocs.iter().all(|reloc| reloc.symbol != "f"));
        assert!(named(&info).is_empty());
    }

    /// A record holding a pointer to itself is one entry and terminates.
    ///
    /// The table is indices rather than a tree for exactly this, and the two passes over it are
    /// what let the pointer at index one name the record at index zero while the record at index
    /// zero names the pointer.
    #[test]
    fn a_record_that_holds_a_pointer_to_itself_is_written_once() {
        let mut unit = one();
        unit.types = vec![
            Shape::Record {
                union: false,
                name: Some("node".to_owned()),
                size: Some(8),
                members: Some(vec![Member {
                    name: Some("next".to_owned()),
                    ty: 1,
                    at: 0,
                    bits: None,
                }]),
            },
            Shape::Pointer { to: Some(0), size: 8 },
        ];
        unit.funcs[0].sig =
            Some(Sig { returns: Some(1), params: Vec::new(), variadic: false, prototyped: true });
        let names = named(&write(&unit).expect("sections"));
        assert_eq!(names.iter().filter(|name| *name == "node").count(), 1, "{names:?}");
        assert!(names.contains(&"next".to_owned()), "{names:?}");
    }

    /// An entry naming a type the table does not have is refused rather than written as something.
    #[test]
    fn an_entry_naming_a_type_that_is_not_there_is_refused() {
        let mut unit = one();
        unit.types = vec![Shape::Pointer { to: Some(9), size: 8 }];
        assert!(write(&unit).is_err());
    }

    /// A declaration naming a file the unit does not have is refused the way a row is.
    #[test]
    fn a_declaration_naming_a_file_that_is_not_there_is_refused() {
        let mut unit = one();
        unit.funcs[0].decl = Some(Place { file: 4, line: 3 });
        assert!(write(&unit).is_err());
    }
}
