//! The types and the signatures the debug information describes, from the checker's own types.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! `rucc-debug` writes a table of [`Shape`], which is DWARF with its questions already answered,
//! and this is where they get answered. It is in the driver for the same reason the line table's
//! file names are: the checker's types are the driver's to hand out and the crate that writes the
//! bytes is below the one that holds them, so a dependency the other way round would put the C type
//! system underneath the object writer. The IR was never a candidate for carrying them, because
//! what the IR needs about a type is its size and its lanes and it has never held anything else.
//!
//! # When it runs
//!
//! Before the back end, because the checker's types are borrowed for the whole of it and the walk
//! here wants them while the back end wants the interner. What comes out is plain data keyed by the
//! name a function will have in the object file, so the back end hands it straight on and nothing
//! has to be looked up twice.
//!
//! Only when `-g` asked for it. A translation unit the size of the SQLite amalgamation has tens of
//! thousands of types in it and a build that wants none of them should not walk one.
//!
//! # What cannot be described yet
//!
//! Three kinds, and each of them needs something [`Shape`] has no field for: `_BitInt(N)` wants a
//! base type with a bit size on it, a GNU vector wants `DW_AT_GNU_vector`, and `_Complex` over an
//! integer half has no DWARF encoding that means it. A function whose signature mentions one of
//! them, at any depth, gets no entry at all rather than an entry that says something else, which
//! is the rule the `tree.rs` module documentation in `rucc-debug` is about.
//!
//! A record is the one thing that bends rather than breaking. A member whose type cannot be
//! described is left out of the member list and everything else about the record stays, so its size
//! is still right and every other member is still where it says it is. Losing the whole record
//! would lose every function that takes a pointer to it, which in a real program is most of them,
//! and a record with a field missing is less than the truth rather than other than it.
//!
//! A record C calls variably modified, meaning a variable length array is among its members, has
//! no member list here either. Its members are at offsets that are not numbers, and the attribute
//! DWARF has for that is an expression, which is the same thing the variable length array itself
//! is waiting for.
//!
//! The typedef names come from the list beside the type table rather than from the types, because
//! an ordinary typedef binds its name to the very type the name stood for and interns nothing of
//! its own. Every name the program wrote at file scope gets a `DW_TAG_typedef` saying what it
//! stands for, which is what lets a debugger answer a question asked in the program's own words.
//! What it does not do is make a declaration point at the name it was written with: `types.kind`
//! for a parameter declared `size_type` answers `unsigned long` and always did, so the parameter's
//! `DW_AT_type` is the underlying type and a debugger printing that parameter says `unsigned long`.
//! Which of the names a declaration used is a fact about the declaration rather than about the
//! type, and nothing between the checker and here carries it. That half is still open on
//! tamnd/rucc#1640.

use std::collections::HashMap;

use rucc_base::{Interner, Symbol};
use rucc_debug::{Bits, Constant, Encoding, Member, Param, Qualifier, Shape, Sig};
use rucc_diag::SourceMap;
use rucc_sema::{DeclId, DeclKind, Linkage, StorageDuration, Tast};
use rucc_target::TargetInfo;
use rucc_types::{
    ArrayLen, IntKind, Qualifiers, RecordId, RecordKind, Type, TypeId, TypeKind, Types,
};

/// Everything the debug information says about what a unit's addresses mean.
#[derive(Debug, Default)]
pub(crate) struct Meaning {
    /// Every type anything below refers to, in the order they refer to them by.
    pub types: Vec<Shape>,
    /// What is known about each function, by the name it will have in the object file.
    ///
    /// Keyed by name rather than paired up by position, because what the back end ends up
    /// emitting is decided after this runs: a `static` function nothing calls is dropped, and the
    /// order the text section comes out in is not the order of the source.
    pub funcs: HashMap<String, Known>,
    /// What is known about each file-scope variable, by the name it will have in the object file.
    ///
    /// Keyed by name for the same reason the functions are, and read the same way: the back end
    /// hands over the objects it actually laid out and each of them is looked up here. A `static`
    /// nothing reads is not among them and so is never asked for.
    pub objects: HashMap<String, Held>,
}

/// What is known about one function.
#[derive(Debug, Clone)]
pub(crate) struct Known {
    /// The file its definition was written in, as the source map spells it.
    ///
    /// A name rather than an index, because the index is into the unit's file table and that table
    /// is built where the line rows are walked. Handing over the name leaves the one place that
    /// builds it building it.
    pub file: String,
    /// The line the definition starts on, counting from one.
    ///
    /// Where the declaration starts, which is the line the return type is written on. That is the
    /// same line the name is on for all but the definition whose return type is on a line of its
    /// own, which is a style gcc reports the second line for and this reports the first.
    pub line: u32,
    /// What it takes and gives back, and [`None`] when something in it cannot be described.
    pub sig: Option<Sig>,
    /// Whether anything outside the unit can see it.
    pub external: bool,
}

/// What is known about one file-scope variable.
#[derive(Debug, Clone)]
pub(crate) struct Held {
    /// The file its definition was written in, as the source map spells it, for the reason
    /// [`Known::file`] is a name.
    pub file: String,
    /// The line it is declared on, counting from one.
    pub line: u32,
    /// Which entry in [`Meaning::types`] it is, and [`None`] when it cannot be described.
    ///
    /// Unlike a function, a variable with nothing here still gets an entry, because a
    /// `DW_TAG_variable` with no `DW_AT_type` says nothing where a `DW_TAG_subprogram` with none
    /// says `void`. See `held_at` in `rucc-debug`.
    pub ty: Option<usize>,
    /// Whether anything outside the unit can see it.
    pub external: bool,
}

/// The types, the signatures and the file-scope variables of everything this unit defines.
pub(crate) fn collect(
    tast: &Tast,
    types: &Types,
    target: &TargetInfo,
    names: &Interner,
    sources: &SourceMap,
) -> Meaning {
    let mut walk =
        Walk { types, target, names, out: Vec::new(), memo: HashMap::new(), tags: HashMap::new() };
    let mut funcs = HashMap::new();
    let mut objects = HashMap::new();
    for &id in tast.top_level() {
        let decl = &tast[id];
        let Some(at) = sources.presumed(tast.decl_span(id).lo) else { continue };
        let Some(symbol) = symbol(tast, names, id) else { continue };
        let external = decl.linkage == Linkage::External;
        match decl.kind {
            // A definition rather than a declaration, since what is being described is the code in
            // this object. `int f(int);` on its own defines nothing for an address to be inside of.
            DeclKind::Function if decl.body.is_some() => {
                let known = Known {
                    file: at.name.to_owned(),
                    line: at.line,
                    sig: walk.signature(tast, id),
                    external,
                };
                funcs.insert(symbol, known);
            }
            // A file-scope object, whether or not it has an initializer, because a `static` one
            // with none is still in the file as zeroed bytes. Whether the back end laid it out is
            // not decided here and does not need to be: a name nothing emitted is a name nothing
            // looks up. A block-scope `static` is not here at all, because the top level does not
            // hold one, and neither is a `Tentative` definition of a name another unit defines,
            // which the linker resolves to somebody else's address.
            DeclKind::Object if decl.duration == StorageDuration::Static => {
                let held = Held {
                    file: at.name.to_owned(),
                    line: at.line,
                    ty: walk.told(decl.ty),
                    external,
                };
                objects.insert(symbol, held);
            }
            _ => {}
        }
    }
    // The typedef names last, so that nothing else waits behind a name that may turn out to stand
    // for a type nothing else mentions. A name whose type cannot be described is left out rather
    // than written with no `DW_AT_type`, since that is how DWARF spells a name for `void` and a
    // program that wrote `typedef void none;` is entitled to have that one come out right.
    for &alias in types.aliases() {
        let Some(of) = walk.told_or_void(alias.of) else { continue };
        let name = walk.spelled(alias.name);
        walk.out.push(Shape::Alias { name, of });
    }
    Meaning { types: walk.out, funcs, objects }
}

/// The name a declaration will have in the object file.
///
/// The assembler name where a declaration wrote one, because the symbol is what the entry has to be
/// found by. `extern int f(int) __asm__("g");` is how the C library redirects a name, and matching
/// on the C name alone would leave every `_FORTIFY_SOURCE` wrapper and every `_FILE_OFFSET_BITS=64`
/// definition without an entry. The other renaming `rucc-lower` does is for `__builtin_` names,
/// which nothing defines, so it is not here.
fn symbol(tast: &Tast, names: &Interner, id: DeclId) -> Option<String> {
    let decl = &tast[id];
    match decl.asm_label {
        Some(label) => {
            Some(tast[label].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect())
        }
        None => Some(names.resolve(decl.name?).to_owned()),
    }
}

/// The walk over the checker's types, building the table as it goes.
struct Walk<'a> {
    types: &'a Types,
    target: &'a TargetInfo,
    names: &'a Interner,
    out: Vec<Shape>,
    /// What each type came out as, [`None`] for one that cannot be described.
    ///
    /// Keyed by the type itself rather than by its identifier so that the inner levels of a
    /// qualifier chain are shared too: the `volatile int` inside `const volatile int` is a type
    /// with no identifier of its own unless the program also wrote it, and either way it is one
    /// entry.
    memo: HashMap<Type, Option<usize>>,
    /// Which entry each record went in.
    ///
    /// Records are memoized a second way, by the declaration rather than by the type, and this is
    /// the one that stops a record holding a pointer to itself from being walked forever. It has
    /// to be the declaration because the pointer inside may be to a qualified version of it, which
    /// is a different type and the same record, and writing the members twice is what keying on
    /// the type alone would do.
    tags: HashMap<RecordId, usize>,
}

impl Walk<'_> {
    /// What one function definition takes and gives back.
    fn signature(&mut self, tast: &Tast, id: DeclId) -> Option<Sig> {
        let declared = tast[id].ty;
        let TypeKind::Function(which) = self.types.kind(self.types.canonical(declared)) else {
            return None;
        };
        let signature = self.types.signature(which).clone();
        let returns = self.told_or_void(signature.ret)?;
        // The names off the definition's own parameter list and the types off the function type,
        // which is a pairing the checker already guarantees: a definition has one declaration per
        // parameter whether or not it was written with a prototype. A name is missing only where
        // the program left a parameter unnamed, which C23 allows.
        let written = tast[tast[id].params].to_vec();
        let mut params = Vec::with_capacity(signature.params.len());
        for (index, &ty) in signature.params.iter().enumerate() {
            let ty = self.told(ty)?;
            let name = written.get(index).and_then(|&param| tast[param].name);
            params.push(Param { name: name.map(|name| self.spelled(name)), ty });
        }
        Some(Sig {
            returns,
            params,
            variadic: signature.variadic,
            prototyped: signature.prototyped,
        })
    }

    /// A type, where `void` is an answer rather than a failure.
    ///
    /// The two are the same absence in DWARF and different ones here, so a return type and a
    /// pointee go through this and a parameter type does not: `void f(void)` returns nothing and
    /// `void *` points at nothing, while a parameter of a type nothing can describe is a function
    /// to leave alone. Qualified `void` is not this case and goes the long way round, because
    /// `const void *` is a pointer to a const entry with no type under it.
    fn told_or_void(&mut self, id: TypeId) -> Option<Option<usize>> {
        let ty = self.types.get(id);
        if matches!(ty.kind, TypeKind::Void) && ty.quals.is_none() {
            return Some(None);
        }
        self.told(id).map(Some)
    }

    /// Which entry a type is, adding it and everything it names to the table.
    fn told(&mut self, id: TypeId) -> Option<usize> {
        self.shaped(self.types.get(id), id)
    }

    /// The same, for a type that may have no identifier of its own.
    ///
    /// The identifier is carried alongside because it is what a size is asked of, and a size never
    /// depends on the qualifiers, so the type a level of the chain stands for and the identifier
    /// the chain started from agree about every number here.
    fn shaped(&mut self, ty: Type, id: TypeId) -> Option<usize> {
        if let Some(&known) = self.memo.get(&ty) {
            return known;
        }
        let answer = self.layered(ty, id);
        self.memo.insert(ty, answer);
        answer
    }

    /// One qualifier at a time, innermost last, which is how DWARF spells a qualified type.
    ///
    /// `const volatile int` is a const entry over a volatile entry over `int`, and the order the
    /// two come in is the order gcc writes rather than anything DWARF asks for: both orders
    /// describe the same type and a reader that cared would be a reader that is wrong.
    fn layered(&mut self, ty: Type, id: TypeId) -> Option<usize> {
        for (mask, which) in [
            (Qualifiers::CONST, Qualifier::Const),
            (Qualifiers::VOLATILE, Qualifier::Volatile),
            (Qualifiers::RESTRICT, Qualifier::Restrict),
        ] {
            if !ty.quals.has(mask) {
                continue;
            }
            let inner = Type { kind: ty.kind, quals: ty.quals.without(mask) };
            let of = match inner.kind {
                TypeKind::Void if inner.quals.is_none() => None,
                _ => Some(self.shaped(inner, id)?),
            };
            let at = self.out.len();
            self.out.push(Shape::Qualified { which, of });
            return Some(at);
        }
        match ty.kind {
            TypeKind::Record(record) => self.record(id, record),
            kind => {
                let shape = self.bare(id, kind)?;
                let at = self.out.len();
                self.out.push(shape);
                Some(at)
            }
        }
    }

    /// A type that cannot reach itself, which is everything but a record.
    ///
    /// C has no way to write a type that reaches itself except through a record tag: an array of
    /// itself and a function taking itself are both refused, and a typedef naming itself is not a
    /// declaration. So this is where the failures live, and the record below is the one place a
    /// half-built entry has to exist.
    fn bare(&mut self, id: TypeId, kind: TypeKind) -> Option<Shape> {
        match kind {
            // `void` on its own is not an entry, and every caller that can accept it has been
            // through `told_or_void`, so arriving here means something wanted a type and there is
            // none. The other two are the kinds nothing here can spell yet.
            TypeKind::Void | TypeKind::BitInt { .. } | TypeKind::Vector { .. } => None,
            TypeKind::Bool => {
                Some(Shape::Base { name: "_Bool".to_owned(), encoding: Encoding::Boolean, size: 1 })
            }
            TypeKind::Int(int) => Some(Shape::Base {
                name: int.as_str().to_owned(),
                encoding: reading(int, self.target),
                size: self.size(id)?,
            }),
            TypeKind::Float(float) => Some(Shape::Base {
                name: float.as_str().to_owned(),
                encoding: Encoding::Float,
                size: self.size(id)?,
            }),
            // Complex is a base type of its own in DWARF rather than a pair of halves, so the
            // encoding says what the bytes are and the size says how many of them there are.
            // `_Complex int` is a GNU extension with no encoding that means it, and it is left
            // alone rather than written as the floating one: a debugger reading two integers as
            // two doubles prints nonsense and has no way to find out.
            TypeKind::Complex(half) => {
                let TypeKind::Float(float) = self.types.kind(self.types.canonical(half)) else {
                    return None;
                };
                Some(Shape::Base {
                    name: format!("complex {}", float.as_str()),
                    encoding: Encoding::Complex,
                    size: self.size(id)?,
                })
            }
            TypeKind::Pointer(to) => {
                let size = self.size(id)?;
                Some(Shape::Pointer { to: self.told_or_void(to)?, size })
            }
            TypeKind::Atomic(inner) => {
                let of = self.told_or_void(inner)?;
                Some(Shape::Qualified { which: Qualifier::Atomic, of })
            }
            TypeKind::Array { elem, len } => {
                let of = self.told(elem)?;
                // A count only where there is a number for one. A variable length array has a
                // size and it is an expression, which DWARF can describe and this does not yet,
                // and the other two spellings have no size at all.
                let count = match len {
                    ArrayLen::Fixed(count) => Some(count),
                    ArrayLen::Unknown | ArrayLen::Star | ArrayLen::Variable(_) => None,
                };
                Some(Shape::Array { of, count })
            }
            TypeKind::Function(which) => {
                let signature = self.types.signature(which).clone();
                let returns = self.told_or_void(signature.ret)?;
                let mut params = Vec::with_capacity(signature.params.len());
                for &ty in &signature.params {
                    params.push(Param { name: None, ty: self.told(ty)? });
                }
                Some(Shape::Subroutine(Sig {
                    returns,
                    params,
                    variadic: signature.variadic,
                    prototyped: signature.prototyped,
                }))
            }
            TypeKind::Enum(which) => {
                let info = self.types.enum_info(which);
                let name = info.tag.map(|tag| self.spelled(tag));
                let underlying = info.underlying?;
                let listed = info.enumerators.clone();
                let size = self.size(underlying)?;
                let of = self.told(underlying)?;
                let values = listed
                    .iter()
                    .map(|one| Constant { name: self.spelled(one.name), value: one.value })
                    .collect();
                Some(Shape::Enumeration { name, of, size, values })
            }
            TypeKind::Typedef { name, underlying, .. } => {
                let of = self.told_or_void(underlying)?;
                Some(Shape::Alias { name: self.spelled(name), of })
            }
            // Handled by the caller, where the entry exists before the members are walked.
            TypeKind::Record(_) => None,
        }
    }

    /// A `struct` or a `union`, whose entry is written before its members are looked at.
    fn record(&mut self, id: TypeId, record: RecordId) -> Option<usize> {
        if let Some(&at) = self.tags.get(&record) {
            return Some(at);
        }
        let info = self.types.record_info(record);
        let union = info.kind == RecordKind::Union;
        let name = info.tag.map(|tag| self.spelled(tag));
        // A record with a layout is one whose members are at offsets that are numbers, which is
        // every record but the variably modified one. Both of the others say nothing about what is
        // inside them, and `DW_AT_declaration` is what DWARF has for that.
        let placed = info.layout.is_some();
        let size = self.size(id);
        let at = self.out.len();
        self.out.push(Shape::Record { union, name, size, members: None });
        self.tags.insert(record, at);
        if !placed {
            return Some(at);
        }
        // Cloned because walking a member's type borrows the table this came out of, and a record
        // in a real header has a handful of members rather than a page of them.
        let fields = self.types.record_info(record).fields.clone();
        let mut members = Vec::with_capacity(fields.len());
        for field in &fields {
            // A member whose type has no entry is left out and the rest of the record stands. See
            // the module documentation: the alternative loses every function that mentions the
            // record, which in a real program is a far larger hole than one field.
            let Some(ty) = self.told(field.ty) else { continue };
            let bits = match field.bits {
                Some(width) => match u64::try_from(field.bit_offset()) {
                    Ok(start) => Some(Bits { at: start, width: u64::from(width) }),
                    Err(_) => continue,
                },
                None => None,
            };
            let name = field.name.map(|name| self.spelled(name));
            members.push(Member { name, ty, at: field.offset, bits });
        }
        if let Shape::Record { members: held, .. } = &mut self.out[at] {
            *held = Some(members);
        }
        Some(at)
    }

    /// How many bytes a type is, and nothing for one that has no size.
    fn size(&self, id: TypeId) -> Option<u64> {
        rucc_types::layout(self.types, id, self.target).ok().map(|laid_out| laid_out.size)
    }

    /// A name, as the program wrote it.
    fn spelled(&self, name: Symbol) -> String {
        self.names.resolve(name).to_owned()
    }
}

/// How DWARF reads the bits of an integer type.
///
/// The character types are kept apart from the rest because a debugger prints one as a character
/// and the other as a number, and plain `char` goes wherever the target put it.
fn reading(int: IntKind, target: &TargetInfo) -> Encoding {
    match int {
        IntKind::Char if target.char_is_signed => Encoding::SignedChar,
        IntKind::Char | IntKind::UChar => Encoding::UnsignedChar,
        IntKind::SChar => Encoding::SignedChar,
        _ if int.is_signed(target.char_is_signed) => Encoding::Signed,
        _ => Encoding::Unsigned,
    }
}
