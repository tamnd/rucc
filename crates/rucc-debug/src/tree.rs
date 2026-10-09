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
//! # Tags
//!
//! A `btf_decl_tag` on a function, a variable, a parameter or a member is a `DW_TAG_GNU_annotation`
//! under the unit, named `btf_decl_tag` and holding the tag's string, and the entry it tags points
//! at it with `DW_AT_GNU_annotation`. Several tags on one entry are a chain, the first the entry
//! points at and each pointing at the next, and a chain is written once and shared by everything
//! with the same tags, the way gcc writes them. These are gcc's own codes, which pahole reads into
//! the BTF.
//!
//! [`Function::sig`]: crate::Function::sig

use std::collections::HashMap;

use crate::line::{Error, Function, Unit, Version};
use crate::shape::{
    Abstract, Constant, Encoding, Global, Held, Local, Member, Place, Qualifier, Reach, Shape, Sig,
    Spot,
};

use gimli::write::{AttributeValue, FileId, UnitEntryId};

/// gcc's `DW_TAG_GNU_annotation`, one tag in a chain of them.
const DW_TAG_GNU_ANNOTATION: gimli::DwTag = gimli::DwTag(0x6001);

/// gcc's `DW_AT_GNU_annotation`, which points at the first tag of an entry's chain and, on a tag,
/// at the next.
const DW_AT_GNU_ANNOTATION: gimli::DwAt = gimli::DwAt(0x2139);

/// What carries tags, as the entry it was written as and its tags.
type Tagged<'a> = Vec<(UnitEntryId, &'a [Vec<u8>])>;

/// The tags already written, by the string each holds and the tag it points at next, so that a
/// chain is written once however many entries carry it.
#[derive(Default)]
struct Annotations {
    made: HashMap<(Vec<u8>, Option<UnitEntryId>), UnitEntryId>,
}

impl Annotations {
    /// Points the entry at the chain of these tags, written from its end so that each points at
    /// the one after it, which is already there.
    fn annotate(&mut self, dwarf: &mut gimli::write::DwarfUnit, at: UnitEntryId, tags: &[Vec<u8>]) {
        let mut next = None;
        for tag in tags.iter().rev() {
            let key = (tag.clone(), next);
            let id = match self.made.get(&key) {
                Some(&id) => id,
                None => {
                    let root = dwarf.unit.root();
                    let id = dwarf.unit.add(root, DW_TAG_GNU_ANNOTATION);
                    title(dwarf, id, "btf_decl_tag");
                    let value = AttributeValue::StringRef(dwarf.strings.add(tag.clone()));
                    let entry = dwarf.unit.get_mut(id);
                    entry.set(gimli::DW_AT_const_value, value);
                    if let Some(next) = next {
                        entry.set(DW_AT_GNU_ANNOTATION, AttributeValue::UnitRef(next));
                    }
                    self.made.insert(key, id);
                    id
                }
            };
            next = Some(id);
        }
        if let Some(first) = next {
            dwarf.unit.get_mut(at).set(DW_AT_GNU_ANNOTATION, AttributeValue::UnitRef(first));
        }
    }
}

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
    unit: &Unit,
    files: &[FileId],
) -> Result<(), Error> {
    let (shapes, funcs, globals, frames) = (&unit.types, &unit.funcs, &unit.globals, unit.frames);
    let wanted = wanted(shapes, funcs, &unit.abstracts, globals, frames);
    let ids = kinds(dwarf, shapes, &wanted);
    let mut tagged = Tagged::new();
    for (shape, &id) in shapes.iter().zip(&ids) {
        if let Some(id) = id {
            tagged.extend(fill(dwarf, shape, id, &ids, shapes)?);
        }
    }
    let origins = abstracted(dwarf, &unit.abstracts, files, &ids)?;
    let mut made: Vec<(usize, UnitEntryId)> = Vec::with_capacity(funcs.len());
    for (index, func) in funcs.iter().enumerate() {
        let Some(sig) = &func.sig else { continue };
        let (said, at, nests) = defined(dwarf, func, sig, index, files, &ids, frames)?;
        tagged.extend(said);
        let refs = Refs { files, ids: &ids, frames: frames || func.frame_local.is_some() };
        tagged.extend(copies(dwarf, func, (at, &nests), index, &refs, &origins)?);
        made.push((index, at));
    }
    // And the calls, once every function has its entry, since a call names the entry of the
    // function it calls and that can be further down. Only DWARF 5 has the tags.
    if unit.version == Version::Five {
        let mut named: HashMap<&str, UnitEntryId> =
            made.iter().map(|&(index, at)| (funcs[index].name.as_str(), at)).collect();
        for &(index, at) in &made {
            sites(dwarf, &funcs[index], at, index, &mut named)?;
        }
    }
    for (index, global) in globals.iter().enumerate() {
        let at = held_at(dwarf, global, funcs.len() + index, files, &ids)?;
        tagged.push((at, &global.tags[..]));
    }
    let mut annotations = Annotations::default();
    for (at, tags) in tagged {
        annotations.annotate(dwarf, at, tags);
    }
    Ok(())
}

/// Which types something written in the unit names, directly or through another type.
///
/// The table has a type for everything the program declared, and gcc describes only the ones a
/// function or a variable it emitted reaches. The rest are bytes nothing reads, and in the kernel
/// they are worse than that: pahole writes every named type it finds into the BTF, so a `static
/// inline` in a header that this unit never called would put its types in the kernel's BTF where
/// the build with gcc has none.
///
/// An array of arrays is followed straight to its element, since [`elements`] writes it as one
/// entry and the arrays in between have none of their own.
fn wanted(
    shapes: &[Shape],
    funcs: &[Function],
    abstracts: &[Abstract],
    globals: &[Global],
    frames: bool,
) -> Vec<bool> {
    let mut wanted = vec![false; shapes.len()];
    let mut work: Vec<usize> = Vec::new();
    let signature = |sig: &Sig, work: &mut Vec<usize>| {
        work.extend(sig.returns);
        work.extend(sig.params.iter().map(|param| param.ty));
    };
    for func in funcs {
        let Some(sig) = &func.sig else { continue };
        signature(sig, &mut work);
        let frames = frames || func.frame_local.is_some();
        let said = func.locals.iter().filter(|local| sayable(&local.spot, frames));
        work.extend(said.filter_map(|local| local.ty));
        // And the locals of a copy that gets an entry, which is a copy with addresses left.
        let copied = func.inlined.iter().filter(|copy| !copy.over.is_empty());
        let said =
            copied.flat_map(|copy| &copy.locals).filter(|local| sayable(&local.spot, frames));
        work.extend(said.filter_map(|local| local.ty));
    }
    for one in abstracts {
        signature(&one.sig, &mut work);
    }
    work.extend(globals.iter().filter_map(|global| global.ty));
    while let Some(at) = work.pop() {
        let Some(shape) = shapes.get(at) else { continue };
        if std::mem::replace(&mut wanted[at], true) {
            continue;
        }
        match shape {
            Shape::Base { .. } => {}
            Shape::Pointer { to, .. } => work.extend(*to),
            Shape::Array { of, .. } => {
                let mut of = *of;
                while let Some(Shape::Array { of: inner, .. }) = shapes.get(of) {
                    of = *inner;
                }
                work.push(of);
            }
            Shape::Record { members, .. } => {
                work.extend(members.iter().flatten().map(|member| member.ty));
            }
            Shape::Enumeration { of, .. }
            | Shape::Alias { of, .. }
            | Shape::Qualified { of, .. } => work.extend(*of),
            Shape::Subroutine(sig) => signature(sig, &mut work),
        }
    }
    wanted
}

/// An entry for every type something names, holding its tag and nothing else yet.
fn kinds(
    dwarf: &mut gimli::write::DwarfUnit,
    shapes: &[Shape],
    wanted: &[bool],
) -> Vec<Option<UnitEntryId>> {
    let root = dwarf.unit.root();
    let entries = shapes.iter().zip(wanted);
    entries.map(|(shape, &wanted)| wanted.then(|| dwarf.unit.add(root, tag(shape)))).collect()
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

/// The attributes and the children of one type's entry, and the members among them that carry
/// tags.
fn fill<'a>(
    dwarf: &mut gimli::write::DwarfUnit,
    shape: &'a Shape,
    at: UnitEntryId,
    ids: &[Option<UnitEntryId>],
    shapes: &[Shape],
) -> Result<Tagged<'a>, Error> {
    let mut tagged = Tagged::new();
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
        Shape::Array { of, count } => elements(dwarf, at, *of, *count, ids, shapes)?,
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
                        let child = held(dwarf, at, member, ids)?;
                        tagged.push((child, &member.tags[..]));
                    }
                }
            }
        }
        Shape::Enumeration { name, of, size, values } => {
            if let Some(name) = name {
                title(dwarf, at, name);
            }
            match size {
                Some(size) => bytes(dwarf, at, *size),
                None => flag(dwarf, at, gimli::DW_AT_declaration),
            }
            points(dwarf, at, *of, ids)?;
            for value in values {
                counted(dwarf, at, value);
            }
        }
        Shape::Alias { name, of } => {
            title(dwarf, at, name);
            points(dwarf, at, *of, ids)?;
        }
        Shape::Qualified { of, .. } => points(dwarf, at, *of, ids)?,
        Shape::Subroutine(sig) => {
            takes(dwarf, at, sig, ids, None, false)?;
        }
    }
    Ok(tagged)
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
/// A build that asked for no unwind table has the same table written into `.debug_frame` instead,
/// which is gcc's answer too, and the frame base is read through that. Only a build with neither
/// gets no frame base, because there would then be nothing to resolve the operation against and an
/// expression a reader cannot evaluate is worse than an attribute that is not there. That is a
/// target with no calling convention written down, or a format with no `.debug_frame`.
///
/// A wasm function has no call frame table, and its frame is in linear memory at the address one of
/// its locals holds. Its frame base is that local, which is what clang writes, and the offsets of
/// the locals in its frame are measured up from it.
///
/// A file-scope variable needs none of it: its address is its own symbol and the linker knows where
/// that went.
///
/// The locals that have a frame slot hang off it, each one a `DW_OP_fbreg` at its own offset, and
/// they are written only where a frame base was, since an offset from an attribute that is not
/// there resolves to nothing. A parameter with a slot gets its location on the entry the signature
/// already wrote for it rather than an entry of its own, because two entries of one name in one
/// scope is a debugger's problem rather than a reader's.
///
/// A local declared inside a `{ ... }` of its own hangs off a `DW_TAG_lexical_block` rather than off
/// the subprogram, so that two blocks each declaring an `i` are two variables a reader can tell
/// apart by where the program counter is. See [`nested`].
///
/// What it hands back is the subprogram, its parameters and its locals, each with its tags, and then
/// the subprogram and its scopes again, which is where the copies inlined into it go.
fn defined<'a>(
    dwarf: &mut gimli::write::DwarfUnit,
    func: &'a Function,
    sig: &'a Sig,
    index: usize,
    files: &[FileId],
    ids: &[Option<UnitEntryId>],
    frames: bool,
) -> Result<Defined<'a>, Error> {
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
    if let Some(local) = func.frame_local {
        let mut expr = gimli::write::Expression::new();
        expr.op_wasm_local(local);
        expr.op(gimli::DW_OP_stack_value);
        entry.set(gimli::DW_AT_frame_base, AttributeValue::Exprloc(expr));
    } else if frames {
        let mut expr = gimli::write::Expression::new();
        expr.op(gimli::DW_OP_call_frame_cfa);
        entry.set(gimli::DW_AT_frame_base, AttributeValue::Exprloc(expr));
    }
    // A wasm function with a frame has a frame base of its own, so the offsets in it can be said
    // whether or not the unit writes a call frame table.
    let frames = frames || func.frame_local.is_some();
    let mut tagged: Tagged<'a> = vec![(at, &func.tags[..])];
    let params = takes(dwarf, at, sig, ids, Some(index), frames)?;
    tagged.extend(params.into_iter().zip(&sig.params).map(|(at, param)| (at, &param.tags[..])));
    let nests = nested(dwarf, func, at, index, frames)?;
    for local in &func.locals {
        // Under the scope it was declared in, or under the function itself for one written straight
        // into the body. A scope index nothing was made for is a local this build says nothing about
        // anyway, so the function is as good a parent as any and the entry is never written.
        let under = local.scope.and_then(|scope| nests.get(scope).copied().flatten()).unwrap_or(at);
        if let Some(child) = kept(dwarf, under, local, files, ids, index, frames)? {
            tagged.push((child, &local.tags[..]));
        }
    }
    Ok((tagged, at, nests))
}

/// What [`defined`] hands back.
type Defined<'a> = (Tagged<'a>, UnitEntryId, Vec<Option<UnitEntryId>>);

/// What an entry of a function refers to outside itself: the files, the entries of the types, and
/// whether a place in the frame can be said, which [`sayable`] explains.
struct Refs<'a> {
    files: &'a [FileId],
    ids: &'a [Option<UnitEntryId>],
    frames: bool,
}

/// The entry [`abstracted`] wrote for a function, and the entries of its parameters, which the
/// parameters of each copy name as their origin.
struct Origin {
    at: UnitEntryId,
    params: Vec<UnitEntryId>,
}

/// The entry each function the inliner copied gets, which every copy of it names as its origin.
///
/// A subprogram with no addresses and `DW_AT_inline`, which is what gcc writes. Its parameters have
/// names and types and nothing about where they are, since that is a question about one copy.
fn abstracted(
    dwarf: &mut gimli::write::DwarfUnit,
    abstracts: &[Abstract],
    files: &[FileId],
    ids: &[Option<UnitEntryId>],
) -> Result<Vec<Origin>, Error> {
    let root = dwarf.unit.root();
    let mut out = Vec::with_capacity(abstracts.len());
    for one in abstracts {
        let at = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
        title(dwarf, at, &one.name);
        if one.external {
            flag(dwarf, at, gimli::DW_AT_external);
        }
        came_from(dwarf, at, &one.name, one.decl, files)?;
        let params = takes(dwarf, at, &one.sig, ids, None, false)?;
        let inline = AttributeValue::Inline(gimli::DW_INL_inlined);
        dwarf.unit.get_mut(at).set(gimli::DW_AT_inline, inline);
        out.push(Origin { at, params });
    }
    Ok(out)
}

/// A `DW_TAG_inlined_subroutine` for each body the inliner copied into a function.
///
/// Each one goes inside the copy it was copied into, or inside the scope the call was written in,
/// or under the function itself, which is how a reader knows which frames to show for an address
/// in it. It names its origin, says where the call was, and covers the addresses the copy ended up
/// at. A copy whose code all went away gets no entry, and a copy inside it goes where it would
/// have gone.
///
/// The parameters of a copy that are somewhere name the parameters of the origin, and its locals
/// are entries of their own inside it. The entries of the locals are what it hands back, each with
/// its tags.
fn copies<'a>(
    dwarf: &mut gimli::write::DwarfUnit,
    func: &'a Function,
    (at, nests): (UnitEntryId, &[Option<UnitEntryId>]),
    which: usize,
    refs: &Refs<'_>,
    origins: &[Origin],
) -> Result<Tagged<'a>, Error> {
    let files = refs.files;
    let mut tagged: Tagged<'a> = Vec::new();
    let mut made: Vec<UnitEntryId> = Vec::with_capacity(func.inlined.len());
    for copy in &func.inlined {
        let under = match copy.parent {
            Some(parent) => made.get(parent).copied(),
            None => copy.scope.and_then(|scope| nests.get(scope).copied().flatten()),
        }
        .unwrap_or(at);
        if copy.over.is_empty() {
            made.push(under);
            continue;
        }
        let Some(origin) = origins.get(copy.of) else {
            let why = format!("a copy in {} names origin {}, which is not one", func.name, copy.of);
            return Err(Error::Refused { why });
        };
        let Some(&file) = files.get(copy.call.file) else {
            let why =
                format!("a copy in {} names file {}, which is not one", func.name, copy.call.file);
            return Err(Error::Refused { why });
        };
        let child = dwarf.unit.add(under, gimli::DW_TAG_inlined_subroutine);
        dwarf
            .unit
            .get_mut(child)
            .set(gimli::DW_AT_abstract_origin, AttributeValue::UnitRef(origin.at));
        covers(dwarf, child, &copy.over, which)?;
        let entry = dwarf.unit.get_mut(child);
        entry.set(gimli::DW_AT_call_file, AttributeValue::FileIndex(Some(file)));
        entry.set(gimli::DW_AT_call_line, AttributeValue::Udata(u64::from(copy.call.line)));
        if copy.column > 0 {
            entry.set(gimli::DW_AT_call_column, AttributeValue::Udata(u64::from(copy.column)));
        }
        for (spot, &param) in copy.params.iter().zip(&origin.params) {
            let Some(spot) = spot.as_ref().filter(|spot| sayable(spot, refs.frames)) else {
                continue;
            };
            let named = dwarf.unit.add(child, gimli::DW_TAG_formal_parameter);
            dwarf
                .unit
                .get_mut(named)
                .set(gimli::DW_AT_abstract_origin, AttributeValue::UnitRef(param));
            somewhere(dwarf, named, &func.name, spot, which, refs.frames)?;
        }
        for local in &copy.locals {
            if let Some(entry) = kept(dwarf, child, local, files, refs.ids, which, refs.frames)? {
                tagged.push((entry, &local.tags[..]));
            }
        }
        made.push(child);
    }
    Ok(tagged)
}

/// A `DW_TAG_call_site` for each call a function makes that a debugger can learn an argument from.
///
/// Each one says the address the callee returns to and names the function it calls: the entry of
/// that function when the unit has one, and otherwise a declaration of it, made once for the unit,
/// which is what gcc writes. A debugger finds the function by that name and checks it against the
/// frame it is stopped in. Each argument is a `DW_TAG_call_site_parameter` that names the register
/// it was passed in and says the value as an expression, which for a register is the register's
/// contents and not the register.
fn sites<'a>(
    dwarf: &mut gimli::write::DwarfUnit,
    func: &'a Function,
    at: UnitEntryId,
    which: usize,
    named: &mut HashMap<&'a str, UnitEntryId>,
) -> Result<(), Error> {
    for call in &func.calls {
        let Ok(addend) = i64::try_from(call.returns) else {
            let why = format!("a call returns {} bytes into {}", call.returns, func.name);
            return Err(Error::Refused { why });
        };
        let callee = match named.get(call.callee.as_str()) {
            Some(&callee) => callee,
            None => {
                let root = dwarf.unit.root();
                let callee = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
                title(dwarf, callee, &call.callee);
                flag(dwarf, callee, gimli::DW_AT_external);
                flag(dwarf, callee, gimli::DW_AT_declaration);
                named.insert(&call.callee, callee);
                callee
            }
        };
        let site = dwarf.unit.add(at, gimli::DW_TAG_call_site);
        let back = gimli::write::Address::Symbol { symbol: which, addend };
        let entry = dwarf.unit.get_mut(site);
        entry.set(gimli::DW_AT_call_return_pc, AttributeValue::Address(back));
        entry.set(gimli::DW_AT_call_origin, AttributeValue::UnitRef(callee));
        for &(reg, held) in &call.args {
            let mut value = gimli::write::Expression::new();
            match held {
                Held::Reg(number) => value.op_breg(gimli::Register(number), 0),
                Held::Constant(number) => value.op_constu(number),
                Held::Frame(_) | Held::Local(_) | Held::Entry(_) => continue,
            }
            let mut location = gimli::write::Expression::new();
            location.op_reg(gimli::Register(reg));
            let param = dwarf.unit.add(site, gimli::DW_TAG_call_site_parameter);
            let entry = dwarf.unit.get_mut(param);
            entry.set(gimli::DW_AT_location, AttributeValue::Exprloc(location));
            entry.set(gimli::DW_AT_call_value, AttributeValue::Exprloc(value));
        }
    }
    Ok(())
}

/// A `DW_TAG_lexical_block` for each of a function's inner scopes that has something to hold, and
/// which entry each of them got.
///
/// Only the ones worth writing. A scope is written when a local declared in it has an entry, or when
/// a scope inside it is written, and a scope that ends up with neither is a nest with nothing in it,
/// which costs bytes and tells a reader nothing it did not already know. That is most of them in a
/// real program: every `if` body and every loop body is a scope and few of them declare anything.
///
/// Parents first, which is what lets each entry be added under the one it belongs to as the walk
/// goes. A scope is always written down after the one it is inside, since a scope is opened before
/// anything inside it is reached, so one pass backwards marks every ancestor of everything wanted
/// and one pass forwards builds the tree.
fn nested(
    dwarf: &mut gimli::write::DwarfUnit,
    func: &Function,
    at: UnitEntryId,
    which: usize,
    frames: bool,
) -> Result<Vec<Option<UnitEntryId>>, Error> {
    let mut wanted = vec![false; func.scopes.len()];
    for local in &func.locals {
        let Some(scope) = local.scope else { continue };
        if sayable(&local.spot, frames) {
            if let Some(seen) = wanted.get_mut(scope) {
                *seen = true;
            }
        }
    }
    // And a scope a call was inlined in, so that the copy can go inside it.
    for copy in &func.inlined {
        let Some(scope) = copy.scope.filter(|_| copy.parent.is_none()) else { continue };
        if let Some(seen) = wanted.get_mut(scope) {
            *seen = true;
        }
    }
    for index in (0..wanted.len()).rev() {
        if let (true, Some(parent)) = (wanted[index], func.scopes[index].parent) {
            if let Some(seen) = wanted.get_mut(parent) {
                *seen = true;
            }
        }
    }
    let mut nests: Vec<Option<UnitEntryId>> = vec![None; func.scopes.len()];
    for (index, scope) in func.scopes.iter().enumerate() {
        if !wanted[index] {
            continue;
        }
        let under = scope.parent.and_then(|parent| nests[parent]).unwrap_or(at);
        let nest = dwarf.unit.add(under, gimli::DW_TAG_lexical_block);
        covers(dwarf, nest, &scope.over, which)?;
        nests[index] = Some(nest);
    }
    Ok(nests)
}

/// Which of a function's addresses a scope covers.
///
/// One stretch is a low and a high, which is two attributes and no section to hold them, and that is
/// what most scopes are. Several is `DW_AT_ranges` and a list, which is what a scope whose code the
/// back end laid out in more than one piece needs.
///
/// A scope with no stretches at all gets neither, which DWARF 5 section 3.5 allows and a reader
/// takes as a block covering whatever its parent does. That is a scope whose code all went away and
/// whose names are the only thing left of it, and saying nothing about where it is beats inventing
/// an answer.
///
/// # Errors
///
/// [`Error::Refused`] on a stretch of no length or one that starts further into the function than a
/// signed offset can reach, for the reasons `somewhere` gives about the same two.
fn covers(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    over: &[Reach],
    which: usize,
) -> Result<(), Error> {
    let mut list = Vec::with_capacity(over.len());
    for reach in over {
        list.push(gimli::write::Range::StartLength {
            begin: where_it_starts(reach, which)?,
            length: reach.len,
        });
    }
    match list.as_slice() {
        [] => {}
        &[gimli::write::Range::StartLength { begin, length }] => {
            let entry = dwarf.unit.get_mut(at);
            entry.set(gimli::DW_AT_low_pc, AttributeValue::Address(begin));
            entry.set(gimli::DW_AT_high_pc, AttributeValue::Udata(length));
        }
        _ => {
            let id = dwarf.unit.ranges.add(gimli::write::RangeList(list));
            dwarf.unit.get_mut(at).set(gimli::DW_AT_ranges, AttributeValue::RangeListRef(id));
        }
    }
    Ok(())
}

/// Where a stretch of a function's addresses begins, as the function's own symbol plus a distance
/// into it, which is how the linker is asked the same question a subprogram's low PC asks it.
///
/// # Errors
///
/// [`Error::Refused`] on a stretch of no length, which covers no address at all, or one starting
/// further into the function than a signed offset reaches, which the writer underneath would read as
/// a stretch somewhere else entirely.
fn where_it_starts(reach: &Reach, which: usize) -> Result<gimli::write::Address, Error> {
    if reach.len == 0 {
        let why = "a scope covers no addresses at all".to_owned();
        return Err(Error::Refused { why });
    }
    let Ok(addend) = i64::try_from(reach.from) else {
        let why = format!("a scope starts {} bytes into its function", reach.from);
        return Err(Error::Refused { why });
    };
    Ok(gimli::write::Address::Symbol { symbol: which, addend })
}

/// One local the program declared, as a child of its function.
///
/// Nothing at all for a local a build with no frame base has nothing to say about, which is a
/// local in the frame of a build that asked for no unwind table. See [`sayable`].
///
/// A local with no type still gets an entry, for the reason [`held_at`] gives. The entry is what
/// it hands back.
fn kept(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    local: &Local,
    files: &[FileId],
    ids: &[Option<UnitEntryId>],
    which: usize,
    frames: bool,
) -> Result<Option<UnitEntryId>, Error> {
    if !sayable(&local.spot, frames) {
        return Ok(None);
    }
    let child = dwarf.unit.add(at, gimli::DW_TAG_variable);
    title(dwarf, child, &local.name);
    came_from(dwarf, child, &local.name, local.decl, files)?;
    points(dwarf, child, local.ty, ids)?;
    somewhere(dwarf, child, &local.name, &local.spot, which, frames)?;
    Ok(Some(child))
}

/// Whether anything can be said about where a local is in this build.
///
/// A place in the frame is an offset from `DW_AT_frame_base`, and a build that writes no call frame
/// table has no frame base for it to be an offset from. A name with an unreadable location is worse
/// than a name a debugger says it cannot find: one of them is a wrong answer and the other is an
/// honest one, so the entry is left off rather than written with a location nothing can evaluate.
///
/// A place in a register is not measured from anything, so it is as good in a build with no frame
/// base as in any other, and that is the whole of the difference. A wasm local and a constant are
/// not measured from anything either. A local that is in a register
/// over part of a function and in the frame over the rest keeps the part that can be said and
/// loses the rest, which leaves a debugger telling the truth at both kinds of address.
fn sayable(spot: &Spot, frames: bool) -> bool {
    if frames {
        return true;
    }
    match spot {
        Spot::Always(held) => !matches!(held, Held::Frame(_)),
        Spot::Over(spans) => spans.iter().any(|span| !matches!(span.held, Held::Frame(_))),
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
        Held::Local(index) => {
            expr.op_wasm_local(index);
            expr.op(gimli::DW_OP_stack_value);
        }
        Held::Constant(number) => {
            expr.op_constu(number);
            expr.op(gimli::DW_OP_stack_value);
        }
        Held::Entry(number) => {
            let mut entry = gimli::write::Expression::new();
            entry.op_reg(gimli::Register(number));
            expr.op_entry_value(entry);
            expr.op(gimli::DW_OP_stack_value);
        }
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
///
/// The entry is what it hands back.
fn held_at(
    dwarf: &mut gimli::write::DwarfUnit,
    global: &Global,
    symbol: usize,
    files: &[FileId],
    ids: &[Option<UnitEntryId>],
) -> Result<UnitEntryId, Error> {
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
    Ok(at)
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
///
/// The parameters' entries are what it hands back, one for each, in order.
fn takes(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    sig: &Sig,
    ids: &[Option<UnitEntryId>],
    which: Option<usize>,
    frames: bool,
) -> Result<Vec<UnitEntryId>, Error> {
    if sig.prototyped {
        flag(dwarf, at, gimli::DW_AT_prototyped);
    }
    points(dwarf, at, sig.returns, ids)?;
    let mut children = Vec::with_capacity(sig.params.len());
    for param in &sig.params {
        let child = dwarf.unit.add(at, gimli::DW_TAG_formal_parameter);
        children.push(child);
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
    Ok(children)
}

/// One member of a record, as a child of the record's own entry, which is what it hands back.
fn held(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    member: &Member,
    ids: &[Option<UnitEntryId>],
) -> Result<UnitEntryId, Error> {
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
    Ok(child)
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
///
/// An array of arrays is one entry with a child per dimension, outermost first, over the element
/// the innermost holds, which is how gcc writes `int m[3][4]`. pahole reads that as an array of
/// twelve, BTF having no way to say two dimensions, and reads an array of arrays as two arrays, so
/// the kernel's BTF has a different type for every such member unless both compilers write the
/// same thing. An array under a typedef name is not looked through, since gcc does not either.
fn elements(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    of: usize,
    count: Option<u64>,
    ids: &[Option<UnitEntryId>],
    shapes: &[Shape],
) -> Result<(), Error> {
    let mut counts = vec![count];
    let mut of = of;
    while let Some(Shape::Array { of: inner, count }) = shapes.get(of) {
        counts.push(*count);
        of = *inner;
    }
    points(dwarf, at, Some(of), ids)?;
    for count in counts {
        let child = dwarf.unit.add(at, gimli::DW_TAG_subrange_type);
        if let Some(count) = count.filter(|&count| count > 0) {
            let last = AttributeValue::Udata(count - 1);
            dwarf.unit.get_mut(child).set(gimli::DW_AT_upper_bound, last);
        }
    }
    Ok(())
}

/// A `DW_AT_type` naming another entry, and nothing at all for `void`.
fn points(
    dwarf: &mut gimli::write::DwarfUnit,
    at: UnitEntryId,
    of: Option<usize>,
    ids: &[Option<UnitEntryId>],
) -> Result<(), Error> {
    let Some(of) = of else { return Ok(()) };
    let Some(&Some(target)) = ids.get(of) else {
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
    use crate::shape::{
        Abstract, Call, Held, Inlined, Local, Member, Param, Place, Scope, Shape, Sig, Span, Spot,
    };

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
                symbol: None,
                len: 16,
                prologue_end: None,
                rows: vec![Row { at: 0, file: 0, line: 3, column: 1 }],
                decl: Some(Place { file: 0, line: 3 }),
                sig: Some(Sig {
                    returns: Some(0),
                    params: vec![Param {
                        name: Some("n".to_owned()),
                        ty: 0,
                        spot: None,
                        tags: Vec::new(),
                    }],
                    variadic: false,
                    prototyped: true,
                }),
                external: true,
                locals: Vec::new(),
                frame_local: None,
                scopes: Vec::new(),
                tags: Vec::new(),
                inlined: Vec::new(),
                calls: Vec::new(),
            }],
            abstracts: Vec::new(),
            globals: Vec::new(),
            pointer: 8,
            frames: true,
            mach_o: false,
            version: Version::Five,
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

    /// `f` with one call to this function in it, returning five bytes in, with a register and a
    /// constant for arguments.
    fn calling(callee: &str) -> Unit {
        let mut unit = one();
        unit.funcs[0].calls = vec![Call {
            returns: 5,
            callee: callee.to_owned(),
            args: vec![(5, Held::Reg(3)), (4, Held::Constant(7))],
        }];
        unit
    }

    /// A call says where it returns to, names a declaration of a function the unit does not
    /// define, and says each argument as the register it went in and a value.
    #[test]
    fn a_call_says_where_it_returns_and_what_its_arguments_were() {
        let info = write(&calling("g")).expect("sections");
        let unit = info.chunks.iter().find(|chunk| chunk.name == ".debug_info").expect("a unit");
        let back = unit.relocs.iter().find(|reloc| reloc.symbol == "f" && reloc.addend == 5);
        assert!(back.is_some(), "no return address, {:?}", unit.relocs);
        assert!(named(&info).contains(&"g".to_owned()), "no declaration of the callee");
        let abbrev = ".debug_abbrev";
        assert!(holds(&info, abbrev, &pair(gimli::DW_AT_declaration, gimli::DW_FORM_flag_present)));
        assert!(holds(&info, abbrev, &pair(gimli::DW_AT_call_value, gimli::DW_FORM_exprloc)));
        // The first argument is in rdi and its value is what rbx holds.
        assert!(holds(&info, ".debug_info", &[1, gimli::DW_OP_reg5.0]));
        assert!(holds(&info, ".debug_info", &[2, gimli::DW_OP_breg3.0, 0]));
    }

    /// A call to a function the unit defines names the entry of that function, and the unit gets
    /// no declaration.
    #[test]
    fn a_call_to_a_function_the_unit_defines_names_its_entry() {
        let info = write(&calling("f")).expect("sections");
        let abbrev = ".debug_abbrev";
        assert!(holds(&info, abbrev, &pair(gimli::DW_AT_call_origin, gimli::DW_FORM_ref4)));
        assert!(!holds(
            &info,
            abbrev,
            &pair(gimli::DW_AT_declaration, gimli::DW_FORM_flag_present)
        ));
    }

    /// DWARF 4 has no tag for a call, so a unit of that version says nothing about one.
    #[test]
    fn a_dwarf_4_unit_says_nothing_about_calls() {
        let mut unit = calling("g");
        unit.version = Version::Four;
        let info = write(&unit).expect("sections");
        assert!(!named(&info).contains(&"g".to_owned()));
        assert!(!holds(
            &info,
            ".debug_abbrev",
            &pair(gimli::DW_AT_call_value, gimli::DW_FORM_exprloc)
        ));
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

    /// A build with no table of frame rules in either section gets no frame base, because there is
    /// nothing to resolve it against.
    #[test]
    fn a_build_that_writes_no_frame_table_gets_no_frame_base() {
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
            scope: None,
            tags: Vec::new(),
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
            scope: None,
            tags: Vec::new(),
        }];
        let info = write(&unit).expect("sections");
        assert!(holds(&info, ".debug_abbrev", &listed()), "the location is not a list");
        let start = gimli::DW_LLE_start_length.0;
        let reg = [start, 0, 0, 0, 0, 0, 0, 0, 0, 8, 1, gimli::DW_OP_reg3.0];
        assert!(holds(&info, ".debug_loclists", &reg), "the first stretch is not in a register");
        let mem = [start, 0, 0, 0, 0, 0, 0, 0, 0, 8, 2, gimli::DW_OP_fbreg.0, 0x70];
        assert!(holds(&info, ".debug_loclists", &mem), "the second stretch is not in the frame");
    }

    /// Under DWARF 4 the same list goes in `.debug_loc`, as a pair of addresses per stretch rather
    /// than a start and a length, and both ends ask the linker where the function went.
    #[test]
    fn a_dwarf_4_local_that_moves_is_listed_in_the_older_section() {
        let mut unit = one();
        unit.version = Version::Four;
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Over(vec![
                Span { from: 0, len: 8, held: Held::Reg(3) },
                Span { from: 8, len: 8, held: Held::Frame(-16) },
            ]),
            scope: None,
            tags: Vec::new(),
        }];
        let info = write(&unit).expect("sections");
        assert!(holds(&info, ".debug_abbrev", &listed()), "the location is not a list");
        assert!(info.chunks.iter().all(|chunk| chunk.name != ".debug_loclists"));
        let list = info.chunks.iter().find(|chunk| chunk.name == ".debug_loc").expect("a list");
        let mut asked: Vec<i64> = list
            .relocs
            .iter()
            .filter(|reloc| reloc.symbol == "f")
            .map(|reloc| reloc.addend)
            .collect();
        asked.sort_unstable();
        assert_eq!(
            asked,
            [0, 8, 8, 16],
            "the stretches do not begin and end where they were said to"
        );
        // The expression after each pair is a two byte length and then the operations.
        assert!(holds(&info, ".debug_loc", &[1, 0, gimli::DW_OP_reg3.0]), "no register stretch");
        assert!(
            holds(&info, ".debug_loc", &[2, 0, gimli::DW_OP_fbreg.0, 0x70]),
            "no frame stretch"
        );
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
            scope: None,
            tags: Vec::new(),
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
            scope: None,
            tags: Vec::new(),
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
            scope: None,
            tags: Vec::new(),
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
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: fixed(-16),
            scope: None,
            tags: Vec::new(),
        }];
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
            scope: None,
            tags: Vec::new(),
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
            scope: None,
            tags: Vec::new(),
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
            scope: None,
            tags: Vec::new(),
        }];
        let info = write(&unit).expect("sections");
        assert!(!named(&info).contains(&"total".to_owned()), "a name with nowhere to be");
    }

    /// A wasm function with a frame says that its frame base is the local that holds the bottom of
    /// the frame, and a local in the frame is an offset from it, in a unit with no call frame table.
    ///
    /// The frame base is a length of four, `DW_OP_WASM_location` with kind nought and local five,
    /// and `DW_OP_stack_value`, which are the bytes clang writes for the same function.
    #[test]
    fn a_wasm_function_measures_its_frame_from_the_local_that_holds_it() {
        let mut unit = one();
        unit.frames = false;
        unit.funcs[0].frame_local = Some(5);
        unit.funcs[0].locals = vec![Local {
            name: "total".to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Always(Held::Frame(8)),
            scope: None,
            tags: Vec::new(),
        }];
        let info = write(&unit).expect("sections");
        let local = [4, 0xed, 0, 5, gimli::DW_OP_stack_value.0];
        assert!(holds(&info, ".debug_info", &local), "the frame base is not local five");
        assert!(holds(&info, ".debug_info", &away(8)), "the local is not 8 above the base");
        assert!(named(&info).contains(&"total".to_owned()), "the local lost its name");
    }

    /// A value in a wasm local is that local and `DW_OP_stack_value`, and a constant is the number
    /// and `DW_OP_stack_value`. Neither is measured from a frame base, so a unit with no call frame
    /// table keeps both.
    #[test]
    fn a_wasm_local_and_a_constant_need_no_frame_base() {
        let mut unit = one();
        unit.frames = false;
        let local = |name: &str, held| Local {
            name: name.to_owned(),
            ty: Some(0),
            decl: None,
            spot: Spot::Always(held),
            scope: None,
            tags: Vec::new(),
        };
        unit.funcs[0].locals = vec![local("k", Held::Local(3)), local("i", Held::Constant(300))];
        let info = write(&unit).expect("sections");
        let k = [4, 0xed, 0, 3, gimli::DW_OP_stack_value.0];
        assert!(holds(&info, ".debug_info", &k), "k is not in local three");
        // Three hundred is two bytes of LEB128, and a number under 32 would be `DW_OP_lit`.
        let i = [4, gimli::DW_OP_constu.0, 0xac, 0x02, gimli::DW_OP_stack_value.0];
        assert!(holds(&info, ".debug_info", &i), "i is not three hundred");
        assert!(named(&info).contains(&"k".to_owned()), "k lost its name");
        assert!(named(&info).contains(&"i".to_owned()), "i lost its name");
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
                    tags: Vec::new(),
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
            tags: Vec::new(),
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
        unit.globals = vec![Global {
            name: "opaque".to_owned(),
            ty: None,
            external: false,
            ..Global::default()
        }];
        let info = write(&unit).expect("sections");
        let held = info.chunks.iter().find(|chunk| chunk.name == ".debug_info").expect("a unit");
        assert!(held.relocs.iter().any(|reloc| reloc.symbol == "opaque"));
        assert!(named(&info).contains(&"opaque".to_owned()));
    }

    /// An enumeration carries its enumerators, which is what lets a debugger print the name.
    #[test]
    fn an_enumeration_names_its_enumerators() {
        let mut unit = one();
        let at = unit.types.len();
        unit.globals = vec![Global {
            name: "paint".to_owned(),
            ty: Some(at),
            external: true,
            ..Global::default()
        }];
        unit.types.push(Shape::Enumeration {
            name: Some("color".to_owned()),
            of: Some(0),
            size: Some(4),
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
        let at = unit.types.len();
        unit.globals = vec![Global {
            name: "span".to_owned(),
            ty: Some(at),
            external: true,
            ..Global::default()
        }];
        unit.types.push(Shape::Enumeration {
            name: Some("wide".to_owned()),
            of: Some(0),
            size: Some(16),
            values: vec![
                Constant { name: "small".to_owned(), value: 1 },
                Constant { name: "huge".to_owned(), value: i128::from(u64::MAX) + 1 },
            ],
        });
        let names = named(&write(&unit).expect("sections"));
        assert!(names.contains(&"small".to_owned()), "{names:?}");
        assert!(!names.contains(&"huge".to_owned()), "{names:?}");
    }

    /// A type that no function, local or variable reaches is not written, as gcc leaves it out.
    #[test]
    fn a_type_nothing_reaches_is_left_out() {
        let mut unit = one();
        unit.types.push(Shape::Enumeration {
            name: Some("unused".to_owned()),
            of: Some(0),
            size: Some(4),
            values: vec![Constant { name: "never".to_owned(), value: 0 }],
        });
        let names = named(&write(&unit).expect("sections"));
        assert!(!names.contains(&"unused".to_owned()), "{names:?}");
        assert!(!names.contains(&"never".to_owned()), "{names:?}");
    }

    /// Where in a function each relocation against it asks the linker for, in the order they were
    /// written.
    ///
    /// A subprogram's low PC and a lexical block's are the same relocation against the same symbol
    /// and differ in the addend, which is the distance into the function, so the addends are what
    /// says which entries were written and where each of them starts.
    fn asked(info: &Info, section: &str) -> Vec<i64> {
        let Some(chunk) = info.chunks.iter().find(|chunk| chunk.name == section) else {
            return Vec::new();
        };
        chunk.relocs.iter().filter(|reloc| reloc.symbol == "f").map(|reloc| reloc.addend).collect()
    }

    /// The unit of `one` with one inner scope over the given stretches and one local declared in it.
    fn inside(over: Vec<Reach>) -> Unit {
        let mut unit = one();
        unit.funcs[0].scopes = vec![Scope { parent: None, over }];
        unit.funcs[0].locals = vec![Local {
            name: "inner".to_owned(),
            ty: Some(0),
            decl: Some(Place { file: 0, line: 5 }),
            spot: fixed(-16),
            scope: Some(0),
            tags: Vec::new(),
        }];
        unit
    }

    /// A local declared in a `{ ... }` of its own hangs off a block that says which addresses it is.
    #[test]
    fn a_local_declared_in_an_inner_scope_gets_a_block_around_it() {
        let info = write(&inside(vec![Reach { from: 4, len: 8 }])).expect("sections");

        // The function itself and the one block inside it, which begins four bytes into it.
        assert_eq!(asked(&info, ".debug_info"), vec![0, 4]);
        assert!(named(&info).contains(&"inner".to_owned()), "the local is not named");
    }

    /// A scope the back end laid out in more than one piece says so with a list rather than a pair.
    #[test]
    fn a_scope_laid_out_in_two_pieces_gets_a_list_of_them() {
        let over = vec![Reach { from: 4, len: 8 }, Reach { from: 24, len: 4 }];
        let info = write(&inside(over)).expect("sections");

        // The subprogram's low PC is the only thing left in the unit itself, and both pieces are in
        // the range list, which is a section of its own. The nothing in front of them there is the
        // unit's own list, which says the unit covers the whole of this one function.
        assert_eq!(asked(&info, ".debug_info"), vec![0]);
        assert_eq!(asked(&info, ".debug_rnglists"), vec![0, 4, 24]);
    }

    /// A scope whose code all went away keeps its names and says nothing about where they were.
    #[test]
    fn a_scope_with_no_addresses_left_still_holds_its_names() {
        let info = write(&inside(Vec::new())).expect("sections");
        assert_eq!(asked(&info, ".debug_info"), vec![0], "a block that says where it is not");
        assert!(named(&info).contains(&"inner".to_owned()), "the local went with it");
    }

    /// A scope that declared nothing gets no block, which is most of the scopes a program writes.
    #[test]
    fn a_scope_with_nothing_declared_in_it_gets_no_block() {
        let mut unit = inside(vec![Reach { from: 4, len: 8 }]);
        unit.funcs[0].locals[0].scope = None;
        let info = write(&unit).expect("sections");
        assert_eq!(asked(&info, ".debug_info"), vec![0], "an empty nest");
    }

    /// A scope that holds only another scope is written too, since the tree has to reach the inner
    /// one and a reader builds the tree from where the entries are.
    #[test]
    fn a_scope_whose_only_child_is_a_scope_with_a_local_is_written() {
        let mut unit = inside(vec![Reach { from: 4, len: 20 }]);
        unit.funcs[0].scopes.push(Scope { parent: Some(0), over: vec![Reach { from: 8, len: 8 }] });
        unit.funcs[0].locals[0].scope = Some(1);
        let info = write(&unit).expect("sections");
        assert_eq!(asked(&info, ".debug_info"), vec![0, 4, 8], "the outer nest is missing");
    }

    /// The unit of `one` with a function `twice` inlined into it twice over: once into the body, and
    /// once more into that copy.
    fn inlined() -> Unit {
        let mut unit = one();
        let sig = unit.funcs[0].sig.clone().expect("a signature");
        unit.abstracts = vec![Abstract {
            name: "twice".to_owned(),
            decl: Some(Place { file: 0, line: 1 }),
            sig,
            external: false,
        }];
        let copy = |parent, line, over| Inlined {
            of: 0,
            parent,
            scope: None,
            call: Place { file: 0, line },
            column: 12,
            over,
            params: Vec::new(),
            locals: Vec::new(),
        };
        unit.funcs[0].inlined = vec![
            copy(None, 4, vec![Reach { from: 4, len: 8 }]),
            copy(Some(0), 1, vec![Reach { from: 6, len: 2 }]),
        ];
        unit
    }

    /// The attribute and the form, as the abbreviation writes them.
    fn pair(at: gimli::DwAt, form: gimli::DwForm) -> [u8; 2] {
        [
            u8::try_from(at.0).expect("a one byte attribute"),
            u8::try_from(form.0).expect("a one byte form"),
        ]
    }

    /// A body inlined into a function is an entry that names the function it is a copy of, says
    /// where the call was and covers the addresses the copy ended up at.
    #[test]
    fn an_inlined_copy_names_its_origin_and_its_call() {
        let info = write(&inlined()).expect("sections");
        // The function, then each copy where it starts. The origin has no addresses.
        assert_eq!(asked(&info, ".debug_info"), vec![0, 4, 6]);
        assert!(named(&info).contains(&"twice".to_owned()), "the origin is not named");
        let wants = [
            pair(gimli::DW_AT_inline, gimli::DW_FORM_udata),
            pair(gimli::DW_AT_abstract_origin, gimli::DW_FORM_ref4),
            pair(gimli::DW_AT_call_file, gimli::DW_FORM_udata),
            pair(gimli::DW_AT_call_line, gimli::DW_FORM_udata),
            pair(gimli::DW_AT_call_column, gimli::DW_FORM_udata),
        ];
        for want in wants {
            assert!(holds(&info, ".debug_abbrev", &want), "no {want:x?}");
        }
    }

    /// A copy whose code all went away gets no entry, and the copy inside it still gets one.
    #[test]
    fn a_copy_with_no_addresses_left_gets_no_entry() {
        let mut unit = inlined();
        unit.funcs[0].inlined[0].over.clear();
        let info = write(&unit).expect("sections");
        assert_eq!(asked(&info, ".debug_info"), vec![0, 6]);
    }

    /// A scope a call was inlined in is written, so that the copy can go inside it, though nothing
    /// was declared in it.
    #[test]
    fn a_scope_with_a_copy_in_it_gets_a_block() {
        let mut unit = inside(vec![Reach { from: 4, len: 8 }]);
        unit.funcs[0].locals[0].scope = None;
        let copied = inlined();
        unit.abstracts = copied.abstracts;
        unit.funcs[0].inlined = copied.funcs[0].inlined.clone();
        unit.funcs[0].inlined[0].scope = Some(0);
        let info = write(&unit).expect("sections");
        assert_eq!(asked(&info, ".debug_info"), vec![0, 4, 4, 6]);
    }

    /// A parameter of a copy names the parameter of the origin and says where it is, and a local
    /// of a copy is an entry inside the copy.
    #[test]
    fn a_copy_says_where_its_parameters_and_locals_are() {
        let mut unit = inlined();
        let copy = &mut unit.funcs[0].inlined[0];
        copy.params = vec![Some(Spot::Over(vec![Span { from: 4, len: 4, held: Held::Reg(5) }]))];
        copy.locals = vec![Local {
            name: "half".to_owned(),
            ty: Some(0),
            decl: Some(Place { file: 0, line: 2 }),
            spot: Spot::Always(Held::Reg(3)),
            scope: None,
            tags: Vec::new(),
        }];
        let info = write(&unit).expect("sections");
        assert_eq!(
            asked(&info, ".debug_loclists"),
            vec![4],
            "the parameter is not in its register"
        );
        assert!(named(&info).contains(&"half".to_owned()), "the local is not there");
        assert_eq!(pointed(&info, "n"), 2, "a parameter of a copy takes its name from the origin");
    }

    /// A parameter of a copy that is nowhere gets no entry, since the origin already names it.
    #[test]
    fn a_parameter_of_a_copy_that_is_nowhere_gets_no_entry() {
        let mut unit = inlined();
        unit.funcs[0].inlined[0].params = vec![None];
        let info = write(&unit).expect("sections");
        assert!(asked(&info, ".debug_loclists").is_empty());
    }

    /// A copy that names an origin the unit does not have is refused.
    #[test]
    fn a_copy_of_nothing_is_refused() {
        let mut unit = inlined();
        unit.funcs[0].inlined[0].of = 3;
        assert!(matches!(write(&unit), Err(Error::Refused { .. })));
    }

    /// A scope over no addresses at all is a mistake here rather than something a program can write.
    #[test]
    fn a_scope_over_a_stretch_of_no_length_is_refused() {
        let over = vec![Reach { from: 4, len: 0 }];
        assert!(matches!(write(&inside(over)), Err(Error::Refused { .. })));
    }

    /// How many attributes in `.debug_info` hold this string, which is how many relocations
    /// there point at it in `.debug_str`, since the table holds each string once.
    fn pointed(info: &Info, text: &str) -> usize {
        let strings = info.chunks.iter().find(|chunk| chunk.name == ".debug_str").expect("strings");
        let mut offset = 0;
        let mut at = None;
        for part in strings.bytes.split(|&byte| byte == 0) {
            if part == text.as_bytes() {
                at = Some(offset);
            }
            offset += part.len() + 1;
        }
        let Some(at) = at else { return 0 };
        let unit = info.chunks.iter().find(|chunk| chunk.name == ".debug_info").expect("a unit");
        let at = i64::try_from(at).expect("a small table");
        unit.relocs
            .iter()
            .filter(|reloc| reloc.symbol == ".debug_str" && reloc.addend == at)
            .count()
    }

    /// The tags of the function, its parameter and a variable are chains of
    /// `DW_TAG_GNU_annotation` under the unit, and a chain the same as another, or the same as the
    /// end of another, is the one already written.
    #[test]
    fn a_tag_is_an_annotation_written_once_for_everything_carrying_it() {
        let mut unit = one();
        let tags = || vec![b"first".to_vec(), b"last".to_vec()];
        unit.funcs[0].tags = tags();
        unit.funcs[0].sig.as_mut().expect("a signature").params[0].tags = tags();
        unit.globals = vec![Global {
            name: "g".to_owned(),
            ty: Some(0),
            tags: vec![b"last".to_vec()],
            ..Global::default()
        }];
        let info = write(&unit).expect("sections");
        // Two tags, `last` with nothing after it and `first` with `last` after it, and everything
        // points at one or the other.
        assert_eq!(pointed(&info, "btf_decl_tag"), 2, "in {:?}", named(&info));
        assert_eq!(pointed(&info, "first"), 1);
        assert_eq!(pointed(&info, "last"), 1);
        // The tag's code and the attribute's, as the LEB128 the abbreviation writes them in.
        assert!(holds(&info, ".debug_abbrev", &[0x81, 0xc0, 0x01]), "no DW_TAG_GNU_annotation");
        assert!(holds(&info, ".debug_abbrev", &[0xb9, 0x42]), "no DW_AT_GNU_annotation");
    }

    /// A member and a local are tagged the same way as everything else.
    #[test]
    fn a_member_and_a_local_carry_their_tags() {
        let mut unit = one();
        unit.types.push(Shape::Record {
            union: false,
            name: Some("s".to_owned()),
            size: Some(4),
            members: Some(vec![Member {
                name: Some("x".to_owned()),
                ty: 0,
                at: 0,
                bits: None,
                tags: vec![b"member".to_vec()],
            }]),
        });
        unit.funcs[0].locals = vec![Local {
            name: "l".to_owned(),
            ty: Some(1),
            decl: None,
            spot: fixed(-8),
            scope: None,
            tags: vec![b"local".to_vec(), b"member".to_vec()],
        }];
        let info = write(&unit).expect("sections");
        assert_eq!(pointed(&info, "btf_decl_tag"), 2, "in {:?}", named(&info));
        assert_eq!(pointed(&info, "member"), 1);
        assert_eq!(pointed(&info, "local"), 1);
    }

    /// A unit with no tags says nothing about them.
    #[test]
    fn a_unit_with_no_tags_has_no_annotation() {
        let info = write(&one()).expect("sections");
        assert_eq!(pointed(&info, "btf_decl_tag"), 0);
        assert!(!holds(&info, ".debug_abbrev", &[0x81, 0xc0, 0x01]));
    }
}
