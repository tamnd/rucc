//! The acid declarations that kencc writes for `-a`, which is `cc/acid.c`.
//!
//! kencc writes them as it parses. A structure or a union writes its `aggr` and its `defn` when
//! its closing brace is seen. A declarator of an object whose type is a structure, or a pointer
//! to one, writes a `complex` line. An enumerator writes its value. The text here is collected in
//! the same order, and each piece keeps the place in the source that caused it, so that the
//! driver can keep only the pieces of the main file for `-aa`.
//!
//! The text depends on what is complete at the time. A pointer to a structure that has no body
//! yet writes nothing, because kencc finds the tag of a structure by its member list and an
//! incomplete one has none.

use rucc_base::Symbol;
use rucc_base::hash::Map;
use rucc_diag::Span;
use rucc_types::{FloatKind, IntKind, RecordId, TypeId, TypeKind};

use crate::DeclId;
use crate::decl::{DeclKind, Linkage, StorageDuration};
use crate::scope::Binding;

use super::Checker;

/// The acid text of one unit, and what the writer must remember to make it.
#[derive(Debug, Default)]
pub(crate) struct Acid {
    /// The text, in pieces, with the place that caused each piece.
    pub(crate) out: Vec<(Span, String)>,
    /// The acid name of each complete record that has members. This is the tag, or `_N_` for a
    /// record that has no tag.
    names: Map<RecordId, String>,
    /// How many records with no tag have closed, which is `taggen` in kencc.
    generated: u32,
    /// How many blocks have opened, which is `blockno` in kencc.
    blocks: u32,
    /// The number of the block that is open now, which is `autobn` in kencc.
    block: u32,
    /// The numbers of the blocks around the one that is open now.
    outer: Vec<u32>,
    /// The block number of each static object in a function, which is in its acid name.
    statics: Map<DeclId, u32>,
    /// The function whose body is being checked.
    function: Option<Symbol>,
}

/// The words of acid that a name must not be. kencc puts a `$` in front of one of these.
const KEYWORDS: [&str; 20] = [
    "adt", "aggr", "append", "builtin", "complex", "defn", "delete", "do", "else", "eval", "head",
    "if", "local", "loop", "return", "tail", "then", "union", "whatis", "while",
];

/// The name as acid can read it, which is `amap` in kencc.
fn amap(name: &str) -> String {
    if KEYWORDS.contains(&name) { format!("${name}") } else { name.to_owned() }
}

impl Checker<'_> {
    /// Opens a block: the body of a function, a compound statement in it, or a `for` statement.
    /// kencc gives each one a number, and a static object declared in it has the number in its
    /// name.
    pub(in crate::check) fn acid_open(&mut self) {
        if let Some(acid) = self.acid.as_deref_mut() {
            acid.blocks += 1;
            acid.outer.push(acid.block);
            acid.block = acid.blocks;
        }
    }

    /// Closes the block that [`Checker::acid_open`] opened.
    pub(in crate::check) fn acid_close(&mut self) {
        if let Some(acid) = self.acid.as_deref_mut() {
            acid.block = acid.outer.pop().unwrap_or(0);
        }
    }

    /// Sets the function whose body is checked next, and returns the one before it.
    pub(in crate::check) fn acid_function(&mut self, name: Option<Symbol>) -> Option<Symbol> {
        let acid = self.acid.as_deref_mut()?;
        std::mem::replace(&mut acid.function, name)
    }

    /// Writes the line of an enumerator.
    pub(in crate::check) fn acid_enumerator(&mut self, name: Symbol, value: i128, span: Span) {
        if self.acid.is_none() {
            return;
        }
        let line = format!("{} = {value};\n", amap(self.text(name)));
        self.acid_push(span, line);
    }

    /// Writes the line of a declarator, if the object has a structure type or a pointer to one.
    pub(in crate::check) fn acid_decl(&mut self, decl: DeclId, span: Span) {
        if self.acid.is_none() {
            return;
        }
        let node = &self.tast[decl];
        if node.kind != DeclKind::Object {
            return;
        }
        let Some(name) = node.name else { return };
        let in_function = !self.scopes.at_file_scope();
        if in_function && node.duration == StorageDuration::Static && node.linkage == Linkage::None
        {
            if let Some(acid) = self.acid.as_deref_mut() {
                let block = acid.block;
                acid.statics.insert(decl, block);
            }
        }
        self.acid_object(decl, name, span);
    }

    /// Writes the line that kencc writes for a member name.
    ///
    /// kencc calls `acidvar` for the symbol of each member declarator, and the symbol of a member
    /// is the symbol of the ordinary name. Thus the line is about the object or the enumerator
    /// that the name refers to at the member, if there is one.
    pub(in crate::check) fn acid_member(&mut self, name: Symbol, span: Span) {
        if self.acid.is_none() {
            return;
        }
        match self.scopes.lookup(name) {
            Some(Binding::Decl(decl)) => {
                if self.tast[decl].kind == DeclKind::Object {
                    self.acid_object(decl, name, span);
                }
            }
            Some(Binding::Enumerator { value, .. }) => self.acid_enumerator(name, value, span),
            _ => {}
        }
    }

    /// The `complex` line of an object, which is `acidvar` in kencc.
    fn acid_object(&mut self, decl: DeclId, name: Symbol, span: Span) {
        let node = &self.tast[decl];
        let (ty, duration, linkage) = (node.ty, node.duration, node.linkage);
        // An `extern register` variable is of the class `CEXREG` in kencc, which has no line.
        let Some(acid) = self.acid.as_deref() else { return };
        if duration == StorageDuration::Static
            && !acid.statics.contains_key(&decl)
            && self.extern_registers.iter().any(|(had, _)| *had == name)
        {
            return;
        }
        let Some(tag) = self.acid_pointee(ty).map(|tag| amap(&tag)) else { return };
        let spelled = self.text(name);
        let line = if duration == StorageDuration::Automatic {
            // A parameter or an automatic object, named by the function it is in.
            let Some(function) = acid.function else { return };
            format!("complex {tag} {}:{};\n", amap(self.text(function)), amap(spelled))
        } else if linkage == Linkage::None && acid.statics.contains_key(&decl) {
            let block = acid.statics[&decl];
            format!("complex {tag} {};\n", amap(&format!("{spelled}${block}")))
        } else {
            format!("complex {tag} {};\n", amap(spelled))
        };
        self.acid_push(span, line);
    }

    /// The acid name of the record that `ty` is, after any pointers are taken off.
    fn acid_pointee(&self, ty: TypeId) -> Option<String> {
        let mut ty = self.types.canonical(ty);
        while let TypeKind::Pointer(to) = self.types.kind(ty) {
            ty = self.types.canonical(to);
        }
        self.acid_record_name(ty)
    }

    /// The acid name of `ty` if it is a record that has one.
    fn acid_record_name(&self, ty: TypeId) -> Option<String> {
        let TypeKind::Record(id) = self.types.kind(self.types.canonical(ty)) else { return None };
        self.acid.as_deref()?.names.get(&id).cloned()
    }

    /// Writes the `aggr` and the `defn` of a record that has just closed, which is `acidtype`.
    pub(in crate::check) fn acid_record(&mut self, id: RecordId, span: Span) {
        let Some(acid) = self.acid.as_deref_mut() else { return };
        let info = self.types.record_info(id);
        let name = match info.tag {
            Some(tag) => self.cx.names.resolve(tag).to_owned(),
            None => {
                acid.generated += 1;
                format!("_{}_", acid.generated)
            }
        };
        // kencc finds the tag by the member list, and an empty record has none.
        if info.fields.is_empty() {
            return;
        }
        acid.names.insert(id, name.clone());
        let an = amap(&name);
        let size = info.layout.map_or(0, |layout| layout.size);
        let mut text = format!("sizeof{an} = {size};\naggr {an}\n{{\n");
        self.acid_members(id, 0, true, &mut text);
        text.push_str("};\n\n");
        text.push_str(&format!("defn\n{an}(addr) {{\n\tcomplex {an} addr;\n"));
        self.acid_members(id, 0, false, &mut text);
        text.push_str("};\n\n");
        self.acid_push(span, text);
    }

    /// The member lines of a record, which is `acidmember` over each member. `aggr` is true for
    /// the lines of the `aggr` and false for the lines of the `defn`.
    fn acid_members(&self, id: RecordId, base: u64, aggr: bool, text: &mut String) {
        let fields = &self.types.record_info(id).fields;
        for field in fields {
            // kencc puts the offset of a bit-field at the start of its `int` unit.
            let offset = match field.bits {
                Some(_) => (field.offset * 8 + u64::from(field.bit)) / 32 * 4,
                None => field.offset,
            } + base;
            let name = field.name.map(|name| amap(self.text(name)));
            self.acid_member_line(field.ty, name.as_deref(), offset, aggr, text);
        }
    }

    /// The lines of one member.
    fn acid_member_line(
        &self,
        ty: TypeId,
        name: Option<&str>,
        offset: u64,
        aggr: bool,
        text: &mut String,
    ) {
        let ty = self.types.canonical(ty);
        let pointer = if self.cx.target.pointer_width == 64 { 'Y' } else { 'X' };
        let letter = match self.types.kind(ty) {
            TypeKind::Pointer(_) => {
                let Some(name) = name else { return };
                if !aggr {
                    text.push_str(&format!(
                        "\tprint(\"\t{name}\t\", addr.{name}\\{pointer}, \"\\n\");\n"
                    ));
                    return;
                }
                if let Some(tag) = self.acid_pointee(ty) {
                    text.push_str(&format!("\t'A' {} {offset} {name};\n", amap(&tag)));
                    return;
                }
                pointer
            }
            TypeKind::Record(inner) => {
                let Some(tag) = self.acid_record_name(ty) else { return };
                let tag = amap(&tag);
                match (name, aggr) {
                    (Some(name), true) => text.push_str(&format!("\t{tag} {offset} {name};\n")),
                    (None, true) => {
                        text.push_str("\t{\n");
                        self.acid_members(inner, offset, true, text);
                        text.push_str("\t};\n");
                    }
                    (Some(name), false) => text.push_str(&format!(
                        "\tprint(\"{tag} {name} {{\\n\");\n\t{tag}(addr.{name});\n\tprint(\"}}\\n\");\n"
                    )),
                    (None, false) => text.push_str(&format!(
                        "\tprint(\"{tag} {{\\n\");\n\t\t{tag}(addr+{offset});\n\tprint(\"}}\\n\");\n"
                    )),
                }
                return;
            }
            TypeKind::Int(kind) => match kind {
                IntKind::Char | IntKind::SChar => 'C',
                IntKind::UChar => 'b',
                IntKind::Short => 'd',
                IntKind::UShort => 'u',
                IntKind::Int | IntKind::Long => 'D',
                IntKind::UInt | IntKind::ULong => 'U',
                IntKind::LongLong => 'V',
                IntKind::ULongLong => 'W',
                IntKind::Int128 | IntKind::UInt128 => return,
            },
            TypeKind::Enum(_) => 'D',
            TypeKind::Float(FloatKind::Float) => 'f',
            TypeKind::Float(FloatKind::Double | FloatKind::LongDouble) => 'F',
            TypeKind::Array { .. } => 'a',
            _ => return,
        };
        let Some(name) = name else { return };
        if aggr {
            text.push_str(&format!("\t'{letter}' {offset} {name};\n"));
        } else {
            text.push_str(&format!("\tprint(\"\t{name}\t\", addr.{name}, \"\\n\");\n"));
        }
    }

    /// Keeps one piece of text.
    fn acid_push(&mut self, span: Span, text: String) {
        if let Some(acid) = self.acid.as_deref_mut() {
            acid.out.push((span, text));
        }
    }
}
