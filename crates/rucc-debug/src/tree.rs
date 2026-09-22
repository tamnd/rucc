//! The entries in `.debug_info`: the types a unit describes, the functions it defines, the
//! variables it defines at file scope, and the locals of those functions.
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
//! A variable is the other way round, and `held_at` says why: a `DW_TAG_variable` with no
//! `DW_AT_type` is not a variable of type `void`, because there is no such thing, so it is a
//! variable whose type was not recorded and the name and the address are still worth having.
//!
//! [`Function::sig`]: crate::Function::sig

use crate::line::{Error, Function};
use crate::shape::{
    Constant, Encoding, Global, Held, Local, Member, Place, Qualifier, Shape, Sig, Spot,
};

use gimli::write::{AttributeValue, FileId, UnitEntryId};

/// Everything a unit says about what its addresses mean, added to a unit that already has a line
/// program.
///
/// The type entries first and the things that name them after, so that a `DW_AT_type` names an
/// entry that is already there.
///
/// The symbol number a relocation carries is a position in the functions followed by the globals,
/// which is the one index space `line.rs` turns back into a name.
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
    globals: &[Global],
    frames: bool,
) -> Result<(), Error> {
    let ids = kinds(dwarf, shapes);
    for (shape, &id) in shapes.iter().zip(&ids) {
        fill(dwarf, shape, id, &ids)?;
    }
    for (index, func) in funcs.iter().enumerate() {
        let Some(sig) = &func.sig else { continue };
        defined(dwarf, func, sig, index, files, &ids, frames)?;
    }
    for (index, global) in globals.iter().enumerate() {
        held_at(dwarf, global, funcs.len() + index, files, &ids)?;
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
        Shape::Enumeration { name, of, size, values } => {
            if let Some(name) = name {
                title(dwarf, at, name);
            }
            bytes(dwarf, at, *size);
            points(dwarf, at, Some(*of), ids)?;
            for value in values {
                counted(dwarf, at, value);
            }
        }
        Shape::Alias { name, of } => {
            title(dwarf, at, name);
            points(dwarf, at, *of, ids)?;
        }
        Shape::Qualified { of, .. } => points(dwarf, at, *of, ids)?,
        Shape::Subroutine(sig) => takes(dwarf, at, sig, ids, None, false)?,
    }
    Ok(())
}

/// The entry for one function this unit defines.
///
/// `DW_AT_low_pc` asks the linker where the function went, the same way the line program's sequence
/// for it does and against the same symbol. `DW_AT_high_pc` is a length rather than an address,
/// which is DWARF 4 and later and is what lets one relocation do for both.
///
/// `DW_AT_frame_base` is `DW_OP_call_frame_cfa`, the call frame address, which is the stack pointer
/// the caller had at the call. The other answer available is a named register, and the register it
/// would have to be is the frame pointer, which this compiler leaves out of every function it can,
/// so most functions would have no right answer to give. Even the ones that keep it would be
/// described wrongly over their first few instructions, since a frame pointer is not a frame pointer
/// until the prologue has set it up, and the front of a function is exactly where a breakpoint on
/// the function lands. The call frame address has neither problem: it is the same value at every
/// program counter in the function, prologue and epilogue included, and it is already written down
/// for every function on every target, since the unwind table says what it is at each address and
/// this compiler writes one for every function including the leaves. It costs a reader having to
/// read that table, which is a thing every debugger does before it prints a frame at all.
///
/// A build that asked for no unwind table gets no frame base, because there would then be nothing to
/// resolve the operation against and an expression a reader cannot evaluate is worse than an
/// attribute that is not there. gcc's answer for that case is to write the same table into
/// `.debug_frame` instead, which this compiler does not write yet.
///
/// A file-scope variable needs none of it: its address is its own symbol and the linker knows where
/// that went.
///
/// The locals that have a frame slot hang off it, each one a `DW_OP_fbreg` at its own offset, and
/// they are written only where a frame base was, since an offset from an attribute that is not
/// there resolves to nothing. A parameter with a slot gets its location on the entry the signature
/// already wrote for it rather than an entry of its own, because two entries of one name in one
/// scope is a debugger's problem rather than a reader's.
fn defined(
    dwarf: &mut gimli::write::DwarfUnit,
    func: &Function,
    sig: &Sig,
    index: usize,
    files: &[FileId],
    ids: &[UnitEntryId],
    frames: bool,
) -> Result<(), Error> {
    let root = dwarf.unit.root();
    let at = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
    title(dwarf, at, &func.name);
    if func.external {
        flag(dwarf, at, gimli::DW_AT_external);
    }
    came_from(dwarf, at, &func.name, func.decl, files)?;
    let entry = dwarf.unit.get_mut(at);
    let start = gimli::write::Address::Symbol { symbol: index, addend: 0 };
    entry.set(gimli::DW_AT_low_pc, AttributeValue::Address(start));
    entry.set(gimli::DW_AT_high_pc, AttributeValue::Udata(func.len));
    if frames {
        let mut expr = gimli::write::Expression::new();
        expr.op(gimli::DW_OP_call_frame_cfa);
        entry.set(gimli::DW_AT_frame_base, AttributeValue::Exprloc(expr));
    }
    takes(dwarf, at, sig, ids, Some(index), frames)?;
    for local in &func.locals {
        kept(dwarf, at, local, files, ids, index, frames)?;
    }
    Ok(())
}

/// One local the program declared, as a child of its function.
///
/// Nothing at all for a local a build with no frame base has nothing to say about, which is a
/// local in the frame of a build that asked for no unwind table. See [`sayable`].
///
/// A local with no type still gets an entry, for the reason [`held_at`] gives.
fn kept(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    local: &Local,
    files: &[FileId],
    ids: &[UnitEntryId],
    which: usize,
    frames: bool,
) -> Result<(), Error> {
    if !sayable(&local.spot, frames) {
        return Ok(());
    }
    let child = dwarf.unit.add(at, gimli::DW_TAG_variable);
    title(dwarf, child, &local.name);
    came_from(dwarf, child, &local.name, local.decl, files)?;
    points(dwarf, child, local.ty, ids)?;
    somewhere(dwarf, child, &local.name, &local.spot, which, frames)
}

/// Whether anything can be said about where a local is in this build.
///
/// A place in the frame is an offset from `DW_AT_frame_base`, and a build that writes no call frame
/// table has no frame base for it to be an offset from. A name with an unreadable location is worse
/// than a name a debugger says it cannot find: one of them is a wrong answer and the other is an
/// honest one, so the entry is left off rather than written with a location nothing can evaluate.
///
/// A place in a register is not measured from anything, so it is as good in a build with no frame
/// base as in any other, and that is the whole of the difference. A local that is in a register
/// over part of a function and in the frame over the rest keeps the part that can be said and
/// loses the rest, which leaves a debugger telling the truth at both kinds of address.
fn sayable(spot: &Spot, frames: bool) -> bool {
    if frames {
        return true;
    }
    match spot {
        Spot::Always(held) => matches!(held, Held::Reg(_)),
        Spot::Over(spans) => spans.iter().any(|span| matches!(span.held, Held::Reg(_))),
    }
}

/// `DW_AT_location`, which is one expression where the place never changes and a reference into
/// `.debug_loclists` where it does.
///
/// A local that has a frame slot is the easy one and the one written first: the frame layout hands
/// the slot out once and nothing moves it afterwards, so one `DW_OP_fbreg` is right at every program
/// counter in the function. A local the register allocator was left to place is the other, and one
/// of those has to be said where it is at the address the debugger stopped at rather than once for
/// the whole run.
///
/// A stretch names its addresses by the function's own symbol plus how far into the function it
/// starts, so the linker resolves it the same way it resolves the function's low PC. A pair of plain
/// numbers would have been wrong under `-ffunction-sections`, which puts every function in a section
/// of its own that a linker may place anywhere.
///
/// # Errors
///
/// [`Error::Refused`] on a stretch of no length or one that starts further into the function than a
/// signed offset can reach. Neither is something a caller can mean: an empty stretch covers no
/// address at all, and the writer underneath would read the second as a stretch somewhere else.
fn somewhere(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    name: &str,
    spot: &Spot,
    which: usize,
    frames: bool,
) -> Result<(), Error> {
    let value = match spot {
        Spot::Always(held) if !frames && matches!(held, Held::Frame(_)) => return Ok(()),
        Spot::Always(held) => AttributeValue::Exprloc(saying(*held)),
        // A list of nothing is a local that is nowhere at every address, and the attribute is left
        // off rather than written empty. Both say the same thing to a reader and one of them is
        // fewer bytes.
        Spot::Over(spans) if spans.is_empty() => return Ok(()),
        Spot::Over(spans) => {
            let mut list = Vec::with_capacity(spans.len());
            for span in spans {
                // A stretch this build cannot measure is left out of the list rather than left in
                // with a location nothing can read, which leaves the addresses it covers as
                // addresses no stretch does, and a debugger says unavailable there. See [`sayable`].
                if !frames && matches!(span.held, Held::Frame(_)) {
                    continue;
                }
                let Ok(addend) = i64::try_from(span.from) else {
                    let why = format!("{name} is somewhere {} bytes into its function", span.from);
                    return Err(Error::Refused { why });
                };
                if span.len == 0 {
                    let why = format!("{name} is somewhere over no addresses at all");
                    return Err(Error::Refused { why });
                }
                list.push(gimli::write::Location::StartLength {
                    begin: gimli::write::Address::Symbol { symbol: which, addend },
                    length: span.len,
                    data: saying(span.held),
                });
            }
            if list.is_empty() {
                return Ok(());
            }
            let id = dwarf.unit.locations.add(gimli::write::LocationList(list));
            AttributeValue::LocationListRef(id)
        }
    };
    dwarf.unit.get_mut(at).set(gimli::DW_AT_location, value);
    Ok(())
}

/// The one operation that says a place, as the expression a location is written as.
fn saying(held: Held) -> gimli::write::Expression {
    let mut expr = gimli::write::Expression::new();
    match held {
        Held::Frame(at) => expr.op_fbreg(at),
        Held::Reg(number) => expr.op_reg(gimli::Register(number)),
    }
    expr
}

/// The entry for one variable this unit defines at file scope.
///
/// `DW_AT_location` is an expression of one operation, `DW_OP_addr` over the variable's own symbol,
/// and the linker fills the address in the same way it fills in a function's low PC. That is the
/// whole of why a file-scope variable is easy and a local is not: this address is the same for the
/// whole run of the program, so one expression says it, where a local is wherever the code was
/// keeping it at the program counter the debugger stopped at.
///
/// A variable with no type still gets an entry, which is the opposite of the rule for a function
/// above, and the reason is that the missing attribute means something different on each. There is
/// no such thing as a variable of type `void`, so a reader of a `DW_TAG_variable` with no
/// `DW_AT_type` has nothing to be misled into believing, and the name and the address on their own
/// are what lets a debugger resolve the name at all.
fn held_at(
    dwarf: &mut gimli::write::DwarfUnit,
    global: &Global,
    symbol: usize,
    files: &[FileId],
    ids: &[UnitEntryId],
) -> Result<(), Error> {
    let root = dwarf.unit.root();
    let at = dwarf.unit.add(root, gimli::DW_TAG_variable);
    title(dwarf, at, &global.name);
    if global.external {
        flag(dwarf, at, gimli::DW_AT_external);
    }
    came_from(dwarf, at, &global.name, global.decl, files)?;
    points(dwarf, at, global.ty, ids)?;
    let mut expr = gimli::write::Expression::new();
    expr.op_addr(gimli::write::Address::Symbol { symbol, addend: 0 });
    dwarf.unit.get_mut(at).set(gimli::DW_AT_location, AttributeValue::Exprloc(expr));
    Ok(())
}

/// `DW_AT_decl_file` and `DW_AT_decl_line`, for whatever knows where it was written.
///
/// # Errors
///
/// [`Error::Refused`] on a file index the unit's line program does not have. Writing one anyway
/// would be a `DW_AT_decl_file` a reader resolves to whatever file happens to be at that index.
fn came_from(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    name: &str,
    place: Option<Place>,
    files: &[FileId],
) -> Result<(), Error> {
    let Some(place) = place else { return Ok(()) };
    let Some(&file) = files.get(place.file) else {
        let why = format!("{name} names file {}, which is not one", place.file);
        return Err(Error::Refused { why });
    };
    let entry = dwarf.unit.get_mut(at);
    entry.set(gimli::DW_AT_decl_file, AttributeValue::FileIndex(Some(file)));
    entry.set(gimli::DW_AT_decl_line, AttributeValue::Udata(u64::from(place.line)));
    Ok(())
}

/// What a signature says, which is the same attributes on a subprogram and on a function type.
///
/// A parameter that has a place carries where it is, written the same way a local's is and under
/// the same condition. A function type's parameters never have one, so `which` is [`None`] there,
/// since a type is not a piece of code and has no frame or registers to be in.
fn takes(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    sig: &Sig,
    ids: &[UnitEntryId],
    which: Option<usize>,
    frames: bool,
) -> Result<(), Error> {
    if sig.prototyped {
        flag(dwarf, at, gimli::DW_AT_prototyped);
    }
    points(dwarf, at, sig.returns, ids)?;
    for param in &sig.params {
        let child = dwarf.unit.add(at, gimli::DW_TAG_formal_parameter);
        let name = param.name.clone().unwrap_or_default();
        if let Some(name) = &param.name {
            title(dwarf, child, name);
        }
        points(dwarf, child, Some(param.ty), ids)?;
        if let (Some(spot), Some(which)) = (param.spot.as_ref(), which) {
            somewhere(dwarf, child, &name, spot, which, frames)?;
        }
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

/// One enumerator, which is a child of the enumeration rather than an attribute on it.
///
/// Which form the value goes in follows the value rather than the enumeration's underlying type: a
/// negative one is signed data and anything else is unsigned data, and a reader that wants the
/// number in the program's own type has the underlying type on the parent to read it through. A
/// value wider than 64 bits has no form to go in, so the enumerator is left out, which is the same
/// answer a record gives for a member it cannot describe and for the same reason: what is left is
/// still true.
fn counted(dwarf: &mut gimli::write::DwarfUnit, at: UnitEntryId, value: &Constant) {
    let held = match value.value {
        held if held < 0 => match i64::try_from(held) {
            Ok(held) => AttributeValue::Sdata(held),
            Err(_) => return,
        },
        held => match u64::try_from(held) {
            Ok(held) => AttributeValue::Udata(held),
            Err(_) => return,
        },
    };
    let child = dwarf.unit.add(at, gimli::DW_TAG_enumerator);
    title(dwarf, child, &value.name);
    dwarf.unit.get_mut(child).set(gimli::DW_AT_const_value, held);
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
    use crate::shape::{Held, Local, Member, Param, Place, Shape, Sig, Span, Spot};

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
                    params: vec![Param { name: Some("n".to_owned()), ty: 0, spot: None }],
                    variadic: false,
                    prototyped: true,
                }),
                external: true,
                locals: Vec::new(),
            }],
            globals: Vec::new(),
            pointer: 8,
            frames: true,
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

    /// Whether a section holds these bytes, in this order, somewhere in it.
    fn holds(info: &Info, name: &str, want: &[u8]) -> bool {
        let Some(chunk) = info.chunks.iter().find(|chunk| chunk.name == name) else {
            return false;
        };
        chunk.bytes.windows(want.len()).any(|seen| seen == want)
    }

    /// The attribute and the form a frame base is written as, which is what the abbreviation says.
    fn base() -> [u8; 2] {
        [
            u8::try_from(gimli::DW_AT_frame_base.0).expect("a one byte attribute"),
            u8::try_from(gimli::DW_FORM_exprloc.0).expect("a one byte form"),
        ]
    }

    /// A function says what its locals are measured from, and the answer is the call frame address.
    ///
    /// The abbreviation is what is read here rather than the entry, because the pair in it is the
    /// attribute and its form together and two bytes in that order are not something the table holds
    /// by accident. The expression in the unit is checked after it and is a length of one followed
    /// by the one operation, which on its own would be a byte pair a search could find anywhere.
    #[test]
    fn a_function_says_its_frame_base_is_the_call_frame_address() {
        let info = write(&one()).expect("sections");
        assert!(holds(&info, ".debug_abbrev", &base()), "no frame base on the subprogram");
        let expr = [1, gimli::DW_OP_call_frame_cfa.0];
        assert!(holds(&info, ".debug_info", &expr), "the frame base is not the call frame address");
    }

    /// A build with no unwind table gets no frame base, because there is nothing to resolve it
    /// against.
    #[test]
    fn a_build_that_writes_no_unwind_table_gets_no_frame_base() {
        let mut unit = one();
        unit.frames = false;
        let info = write(&unit).expect("sections");
        assert!(!holds(&info, ".debug_abbrev", &base()), "a frame base nothing answers");
    }

    /// The attribute and the form a location is written as, read the same way a frame base is.
    fn spot() -> [u8; 2] {
        [
            u8::try_from(gimli::DW_AT_location.0).expect("a one byte attribute"),
            u8::try_from(gimli::DW_FORM_exprloc.0).expect("a one byte form"),
        ]
    }

    /// An expression of one `DW_OP_fbreg` at this offset, as the bytes it is written as.
    ///
    /// The offset is a signed LEB128, so the two offsets the tests below use are one byte each: the
    /// low seven bits of the number with its sign bit already in place. Writing them out rather
    /// than encoding them keeps the test from agreeing with a mistake in the encoder.
    fn away(offset: u8) -> [u8; 3] {
        [2, gimli::DW_OP_fbreg.0, offset]
    }

    /// A place that is a frame slot and never changes, which is what a local with one has.
    fn fixed(offset: i64) -> Spot {
        Spot::Always(Held::Frame(offset))
    }

    /// The same, for a parameter, which may have no place at all.
    fn slot(offset: i64) -> Option<Spot> {
        Some(fixed(offset))
    }

    /// A local with a frame slot says where it is, and where is an offset from the frame base.
    #[test]
    fn a_local_with_a_slot_says_how_far_below_the_frame_base_it_is() {
        let mut unit = one();
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: Some(Place { file: 0, line: 4 }),
            spot: Spot::Always(Held::Frame(-16)),
        }];
        let info = write(&unit).expect("sections");
        assert!(holds(&info, ".debug_abbrev", &spot()), "no location on the local");
        assert!(holds(&info, ".debug_info", &away(0x70)), "the local is not 16 below the base");
        assert!(named(&info).contains(&"total".to_owned()), "the local is not named");
    }

    /// A parameter with a frame slot says where it is on the entry its signature already wrote.
    ///
    /// The count is the point of the test: a parameter is a local, so the obvious way to write it
    /// would put a second entry of the same name in the same scope, and a debugger asked for `n`
    /// would then have two answers to pick between.
    #[test]
    fn a_parameter_with_a_slot_gets_its_location_and_not_a_second_entry() {
        let mut unit = one();
        unit.funcs[0].sig.as_mut().expect("a signature").params[0].spot = slot(-8);
        let info = write(&unit).expect("sections");
        assert!(holds(&info, ".debug_abbrev", &spot()), "no location on the parameter");
        assert!(holds(&info, ".debug_info", &away(0x78)), "the parameter is not 8 below the base");
        let names = named(&info);
        assert_eq!(names.iter().filter(|name| *name == "n").count(), 1, "twice over, {names:?}");
    }

    /// The attribute and the form a location that is a list is written as.
    ///
    /// A different form from the one above and that is the whole point: what is at a section offset
    /// is a list, and a list has no one answer to write inline.
    fn listed() -> [u8; 2] {
        [
            u8::try_from(gimli::DW_AT_location.0).expect("a one byte attribute"),
            u8::try_from(gimli::DW_FORM_sec_offset.0).expect("a one byte form"),
        ]
    }

    /// A local the register allocator moved around says where it is stretch by stretch.
    ///
    /// The list itself is read out of `.debug_loclists`: an entry that says start and length, the
    /// address the linker has yet to fill in, how many bytes the stretch covers, and then the
    /// expression, which for a value in a register is one operation naming the register and for one
    /// in the frame is the same operation a slot gets.
    #[test]
    fn a_local_that_moves_says_where_it_is_over_each_stretch_of_its_function() {
        let mut unit = one();
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Over(vec![
                Span { from: 0, len: 8, held: Held::Reg(3) },
                Span { from: 8, len: 8, held: Held::Frame(-16) },
            ]),
        }];
        let info = write(&unit).expect("sections");
        assert!(holds(&info, ".debug_abbrev", &listed()), "the location is not a list");
        let start = gimli::DW_LLE_start_length.0;
        let reg = [start, 0, 0, 0, 0, 0, 0, 0, 0, 8, 1, gimli::DW_OP_reg3.0];
        assert!(holds(&info, ".debug_loclists", &reg), "the first stretch is not in a register");
        let mem = [start, 0, 0, 0, 0, 0, 0, 0, 0, 8, 2, gimli::DW_OP_fbreg.0, 0x70];
        assert!(holds(&info, ".debug_loclists", &mem), "the second stretch is not in the frame");
    }

    /// Every stretch asks the linker where its function went, the same way a line sequence does.
    ///
    /// A pair of plain numbers would have been wrong under `-ffunction-sections`, which puts every
    /// function in a section of its own that a linker may place anywhere. The addend is how far into
    /// the function the stretch starts, so the two relocations here are the two starts.
    #[test]
    fn a_stretch_names_the_function_it_is_measured_into() {
        let mut unit = one();
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Over(vec![
                Span { from: 0, len: 8, held: Held::Reg(3) },
                Span { from: 8, len: 8, held: Held::Reg(4) },
            ]),
        }];
        let info = write(&unit).expect("sections");
        let list = info.chunks.iter().find(|chunk| chunk.name == ".debug_loclists");
        let list = list.expect("a location list");
        let asked: Vec<i64> = list
            .relocs
            .iter()
            .filter(|reloc| reloc.symbol == "f")
            .map(|reloc| reloc.addend)
            .collect();
        assert_eq!(asked, [0, 8], "the stretches do not start where they were said to");
    }

    /// A local that is nowhere at every address gets no location rather than an empty list.
    ///
    /// The name is still worth writing. A debugger that knows the variable exists and says it is
    /// not available is telling the truth, and one that has never heard of it cannot.
    #[test]
    fn a_local_that_is_nowhere_at_all_gets_no_location_and_keeps_its_name() {
        let mut unit = one();
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Over(Vec::new()),
        }];
        let info = write(&unit).expect("sections");
        assert!(!holds(&info, ".debug_abbrev", &listed()), "a list of nothing");
        assert!(!holds(&info, ".debug_abbrev", &spot()), "an expression out of nothing");
        assert!(
            info.chunks.iter().all(|chunk| chunk.name != ".debug_loclists"),
            "an empty section"
        );
        assert!(named(&info).contains(&"total".to_owned()), "the local lost its name too");
    }

    /// A stretch of no length covers no address, so it is refused rather than written.
    #[test]
    fn a_stretch_that_covers_no_addresses_is_refused() {
        let mut unit = one();
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Over(vec![Span { from: 0, len: 0, held: Held::Reg(3) }]),
        }];
        assert!(write(&unit).is_err());
    }

    /// A build with no unwind table says nothing about where a local is, for the same reason it
    /// says nothing about what a local would be measured from.
    #[test]
    fn a_build_that_writes_no_unwind_table_says_nothing_about_where_a_local_is() {
        let mut unit = one();
        unit.frames = false;
        unit.funcs[0].sig.as_mut().expect("a signature").params[0].spot = slot(-8);
        unit.funcs[0].locals =
            vec![Local { name: "total".to_owned(), ty: Some(0), decl: None, spot: fixed(-16) }];
        let info = write(&unit).expect("sections");
        assert!(!holds(&info, ".debug_abbrev", &spot()), "a location nothing can resolve");
        assert!(!named(&info).contains(&"total".to_owned()), "a name with nowhere to be");
    }

    /// A register is not measured from anything, so a build with no unwind table still says so.
    ///
    /// The frame base is what an offset into the frame is counted from and a register location
    /// counts from nothing, which is the whole of the difference. A build that drops the unwind
    /// table loses the answers that needed one and keeps the rest.
    #[test]
    fn a_build_with_no_frame_base_still_says_which_register_a_local_is_in() {
        let mut unit = one();
        unit.frames = false;
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Over(vec![Span { from: 0, len: 8, held: Held::Reg(3) }]),
        }];
        let info = write(&unit).expect("sections");
        assert!(holds(&info, ".debug_abbrev", &listed()), "the register went with the frame base");
        assert!(named(&info).contains(&"total".to_owned()), "the local lost its name");
    }

    /// And a local that is in a register over part of a function and in the frame over the rest
    /// keeps the part that can be said.
    ///
    /// The addresses the dropped stretch covered become addresses no stretch does, which is a
    /// debugger saying the variable is unavailable there. That is the honest answer, and it beats
    /// both of the others: a location nothing can evaluate is a wrong answer, and leaving the name
    /// off altogether throws away the half of the function that was fine.
    #[test]
    fn a_build_with_no_frame_base_keeps_the_stretches_that_do_not_need_one() {
        let mut unit = one();
        unit.frames = false;
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Over(vec![
                Span { from: 0, len: 8, held: Held::Reg(3) },
                Span { from: 8, len: 8, held: Held::Frame(-16) },
            ]),
        }];
        let info = write(&unit).expect("sections");
        let start = gimli::DW_LLE_start_length.0;
        let reg = [start, 0, 0, 0, 0, 0, 0, 0, 0, 8, 1, gimli::DW_OP_reg3.0];
        assert!(holds(&info, ".debug_loclists", &reg), "the register stretch went too");
        let mem = [start, 0, 0, 0, 0, 0, 0, 0, 0, 8, 2, gimli::DW_OP_fbreg.0, 0x70];
        assert!(!holds(&info, ".debug_loclists", &mem), "an offset from nothing");
    }

    /// A local that is only ever in the frame in such a build gets no location and no name, which
    /// is the whole entry gone rather than an empty list.
    #[test]
    fn a_build_with_no_frame_base_drops_a_local_that_is_only_ever_in_the_frame() {
        let mut unit = one();
        unit.frames = false;
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Over(vec![Span { from: 0, len: 8, held: Held::Frame(-16) }]),
        }];
        let info = write(&unit).expect("sections");
        assert!(!named(&info).contains(&"total".to_owned()), "a name with nowhere to be");
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

    /// A file-scope variable gets an entry whose address the linker fills in, against its own name.
    ///
    /// The symbol number in a relocation is a position in the functions followed by the globals, so
    /// this is also what says the two lists share one index space: getting the offset wrong would
    /// put the function's name on the variable's address or run off the end of the list.
    #[test]
    fn a_file_scope_variable_gets_an_entry_the_linker_fills_in() {
        let mut unit = one();
        unit.globals = vec![Global {
            name: "counter".to_owned(),
            ty: Some(0),
            decl: Some(Place { file: 0, line: 1 }),
            external: true,
        }];
        let info = write(&unit).expect("sections");
        let held = info.chunks.iter().find(|chunk| chunk.name == ".debug_info").expect("a unit");
        let at = held.relocs.iter().find(|reloc| reloc.symbol == "counter").expect("an address");
        assert_eq!(at.kind, Reference::Address { bytes: 8 });
        assert_eq!(at.addend, 0);
        assert!(held.relocs.iter().any(|reloc| reloc.symbol == "f"), "and the function still");
        assert!(named(&info).contains(&"counter".to_owned()));
    }

    /// A variable whose type could not be described keeps its name and its address.
    ///
    /// The opposite of the rule for a function, and on purpose: nothing in C is a variable of type
    /// `void`, so a missing `DW_AT_type` here misleads nobody, and a debugger that can resolve the
    /// name and be told the address can be told the type by whoever is reading.
    #[test]
    fn a_variable_with_no_type_still_gets_an_entry() {
        let mut unit = one();
        unit.globals =
            vec![Global { name: "opaque".to_owned(), ty: None, decl: None, external: false }];
        let info = write(&unit).expect("sections");
        let held = info.chunks.iter().find(|chunk| chunk.name == ".debug_info").expect("a unit");
        assert!(held.relocs.iter().any(|reloc| reloc.symbol == "opaque"));
        assert!(named(&info).contains(&"opaque".to_owned()));
    }

    /// An enumeration carries its enumerators, which is what lets a debugger print the name.
    #[test]
    fn an_enumeration_names_its_enumerators() {
        let mut unit = one();
        unit.types.push(Shape::Enumeration {
            name: Some("color".to_owned()),
            of: 0,
            size: 4,
            values: vec![
                Constant { name: "red".to_owned(), value: 0 },
                Constant { name: "green".to_owned(), value: -1 },
            ],
        });
        let names = named(&write(&unit).expect("sections"));
        assert!(names.contains(&"color".to_owned()), "{names:?}");
        assert!(names.contains(&"red".to_owned()), "{names:?}");
        assert!(names.contains(&"green".to_owned()), "{names:?}");
    }

    /// An enumerator whose value is wider than any DWARF form is left out and the rest stand.
    #[test]
    fn an_enumerator_too_wide_for_a_form_is_left_out() {
        let mut unit = one();
        unit.types.push(Shape::Enumeration {
            name: Some("wide".to_owned()),
            of: 0,
            size: 16,
            values: vec![
                Constant { name: "small".to_owned(), value: 1 },
                Constant { name: "huge".to_owned(), value: i128::from(u64::MAX) + 1 },
            ],
        });
        let names = named(&write(&unit).expect("sections"));
        assert!(names.contains(&"small".to_owned()), "{names:?}");
        assert!(!names.contains(&"huge".to_owned()), "{names:?}");
    }
}
