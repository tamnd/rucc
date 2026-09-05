//! The type table: interning, canonicalisation, and the nominal declarations.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.1.
//!
//! There is one [`Types`] per translation unit and every [`TypeId`] belongs to it. Interning
//! is what makes type identity an integer comparison, which is the single most frequent
//! question the compiler asks, and it is also what makes the canonical form free to look up:
//! each entry stores the id of its own canonical type, so stripping a stack of typedefs is one
//! array read rather than a walk.

use std::collections::HashMap;
use std::num::NonZeroU32;

use rucc_base::{Idx, Symbol};

use crate::kind::{
    ArrayLen, EnumId, FloatKind, FunctionId, FunctionType, IntKind, Qualifiers, RecordId,
    RecordKind, Type, TypeKind,
};
use crate::layout::Layout;
use crate::record::{Field, RecordLayout};

/// The identity of a type.
///
/// Four bytes, `Copy`, and equal exactly when the two types are the same type. Ids from two
/// different [`Types`] tables are not comparable, which is not a restriction in practice
/// because there is one table per translation unit.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(Idx<Entry>);

impl std::fmt::Debug for TypeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TypeId#{}", self.0.raw())
    }
}

/// One row of the table.
///
/// The canonical id is stored rather than computed because almost every read of a type wants
/// it, and computing it means walking a chain whose length is however many typedefs the header
/// author felt like writing.
#[derive(Debug, Clone, Copy)]
struct Entry {
    ty: Type,
    canonical: TypeId,
}

/// What is known about one `struct` or `union` declaration.
#[derive(Debug, Clone)]
pub struct RecordInfo {
    /// Whether it is a `struct` or a `union`.
    pub kind: RecordKind,
    /// The tag, absent for an anonymous one.
    pub tag: Option<Symbol>,
    /// The layout, absent until the members have been seen and laid out.
    ///
    /// This is also what says whether the type is complete. A record is incomplete from the
    /// point its tag is first mentioned until its closing brace, and code in between may
    /// declare pointers to it and nothing else.
    pub layout: Option<Layout>,
    /// The members, placed, and empty until the record is complete.
    ///
    /// One entry per member the program wrote, in that order, so a caller that kept the
    /// declarations can index the two together.
    pub fields: Vec<Field>,
}

/// What is known about one `enum` declaration.
#[derive(Debug, Clone)]
pub struct EnumInfo {
    /// The tag, absent for an anonymous one.
    pub tag: Option<Symbol>,
    /// The type the enumerators are represented in, absent until it is decided.
    ///
    /// C23 lets the program write it, and before that it is chosen once every enumerator has
    /// been seen. Either way it is a fact about the declaration rather than about the type
    /// system, so it is recorded here and not derived twice.
    pub underlying: Option<TypeId>,
    /// Whether the underlying type was written by the program rather than chosen.
    ///
    /// It changes the answer to what an enumerator's own type is, and it decides whether an
    /// enumerator that does not fit is an error or a reason to widen.
    pub fixed: bool,
}

/// Every type in one translation unit.
#[derive(Debug)]
pub struct Types {
    entries: Vec<Entry>,
    map: HashMap<Type, TypeId>,
    functions: Vec<FunctionType>,
    function_map: HashMap<FunctionType, FunctionId>,
    records: Vec<RecordInfo>,
    enums: Vec<EnumInfo>,
    void: TypeId,
    boolean: TypeId,
    ints: [TypeId; 13],
    floats: [TypeId; 9],
}

impl Default for Types {
    fn default() -> Types {
        Types::new()
    }
}

impl Types {
    /// A table holding the basic types and nothing else.
    ///
    /// The basic types are interned here rather than on first use so that asking for `int` is
    /// an array read. They are the ones asked for by far the most often, because every
    /// integer promotion produces one.
    #[must_use]
    pub fn new() -> Types {
        let mut types = Types {
            entries: Vec::new(),
            map: HashMap::new(),
            functions: Vec::new(),
            function_map: HashMap::new(),
            records: Vec::new(),
            enums: Vec::new(),
            // Fixed up immediately below. There is no id to put here before the table exists,
            // and an `Option` on each of them would be paid for on every read for the sake of
            // four lines of construction.
            void: TypeId(Idx::new(0)),
            boolean: TypeId(Idx::new(0)),
            ints: [TypeId(Idx::new(0)); 13],
            floats: [TypeId(Idx::new(0)); 9],
        };
        types.void = types.intern(Type::new(TypeKind::Void));
        types.boolean = types.intern(Type::new(TypeKind::Bool));
        for kind in IntKind::ALL {
            types.ints[kind.index()] = types.intern(Type::new(TypeKind::Int(kind)));
        }
        for kind in FloatKind::ALL {
            types.floats[kind.index()] = types.intern(Type::new(TypeKind::Float(kind)));
        }
        types
    }

    /// How many distinct types there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty, which it never is once [`Types::new`] has run.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The type `id` stands for, with its qualifiers.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn get(&self, id: TypeId) -> Type {
        self.entries[id.0.index()].ty
    }

    /// What `id` is, ignoring its qualifiers.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn kind(&self, id: TypeId) -> TypeKind {
        self.get(id).kind
    }

    /// What `id` is qualified with.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn quals(&self, id: TypeId) -> Qualifiers {
        self.get(id).quals
    }

    /// The canonical form of `id`, with every typedef resolved at every depth.
    ///
    /// This is what every semantic rule reads. `id` itself is what every diagnostic prints.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn canonical(&self, id: TypeId) -> TypeId {
        self.entries[id.0.index()].canonical
    }

    /// Whether `id` is written with a typedef name somewhere inside it.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn is_sugar(&self, id: TypeId) -> bool {
        self.canonical(id) != id
    }

    /// `void`.
    #[must_use]
    pub fn void(&self) -> TypeId {
        self.void
    }

    /// `bool`, which is `_Bool` in the older spellings.
    ///
    /// Named this way because `bool` is a Rust keyword and `r#bool` at every call site would
    /// be a worse trade than one unusual name here.
    #[must_use]
    pub fn boolean(&self) -> TypeId {
        self.boolean
    }

    /// One of the standard integer types.
    #[must_use]
    pub fn int(&self, kind: IntKind) -> TypeId {
        self.ints[kind.index()]
    }

    /// One of the real floating types.
    #[must_use]
    pub fn float(&self, kind: FloatKind) -> TypeId {
        self.floats[kind.index()]
    }

    /// `_Complex T`.
    pub fn complex(&mut self, kind: FloatKind) -> TypeId {
        self.intern(Type::new(TypeKind::Complex(kind)))
    }

    /// `_BitInt(width)`, signed or not.
    ///
    /// The width is not checked against the target's maximum here. That check belongs where
    /// there is a span to point at, and building the type anyway means the rest of the
    /// declaration still gets checked instead of collapsing into a cascade.
    pub fn bit_int(&mut self, signed: bool, width: u32) -> TypeId {
        self.intern(Type::new(TypeKind::BitInt { signed, width }))
    }

    /// A pointer to `pointee`.
    pub fn pointer(&mut self, pointee: TypeId) -> TypeId {
        self.intern(Type::new(TypeKind::Pointer(pointee)))
    }

    /// `_Atomic(inner)`.
    pub fn atomic(&mut self, inner: TypeId) -> TypeId {
        self.intern(Type::new(TypeKind::Atomic(inner)))
    }

    /// An array of `elem`.
    pub fn array(&mut self, elem: TypeId, len: ArrayLen) -> TypeId {
        self.intern(Type::new(TypeKind::Array { elem, len }))
    }

    /// A GNU vector of `len` elements of `elem`.
    pub fn vector(&mut self, elem: TypeId, len: u32) -> TypeId {
        self.intern(Type::new(TypeKind::Vector { elem, len }))
    }

    /// A function type, deduplicated by content.
    ///
    /// # Panics
    ///
    /// Panics past four billion distinct function types in one translation unit. The
    /// alternative to panicking is handing back an id that means a different type, so the
    /// limit is stated rather than worked around.
    pub fn function(&mut self, signature: FunctionType) -> TypeId {
        let id = match self.function_map.get(&signature) {
            Some(&id) => id,
            None => {
                let id = FunctionId(u32::try_from(self.functions.len()).expect("too many types"));
                self.functions.push(signature.clone());
                self.function_map.insert(signature, id);
                id
            }
        };
        self.intern(Type::new(TypeKind::Function(id)))
    }

    /// The signature behind a function type.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn signature(&self, id: FunctionId) -> &FunctionType {
        &self.functions[id.0 as usize]
    }

    /// Declares a `struct` or `union` that has been named but not yet laid out.
    ///
    /// Each call makes a new type even for the same tag, because a record type in C is its
    /// declaration. Redeclaring a tag in an inner scope makes a different type, and the two
    /// being distinct is what the scope rules mean.
    ///
    /// # Panics
    ///
    /// Panics past four billion record declarations in one translation unit.
    pub fn declare_record(&mut self, kind: RecordKind, tag: Option<Symbol>) -> RecordId {
        let id = RecordId(u32::try_from(self.records.len()).expect("too many types"));
        self.records.push(RecordInfo { kind, tag, layout: None, fields: Vec::new() });
        id
    }

    /// The type of a declared record.
    pub fn record(&mut self, id: RecordId) -> TypeId {
        self.intern(Type::new(TypeKind::Record(id)))
    }

    /// What is known about a declared record.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn record_info(&self, id: RecordId) -> &RecordInfo {
        &self.records[id.0 as usize]
    }

    /// Completes a record by recording what [`layout_record`](crate::layout_record) produced.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    pub fn complete_record(&mut self, id: RecordId, laid_out: RecordLayout) {
        let info = &mut self.records[id.0 as usize];
        info.layout = Some(laid_out.layout);
        info.fields = laid_out.fields;
    }

    /// The member of a record with the given name.
    ///
    /// Direct members only. Reaching into an anonymous member is a name lookup with a path to
    /// build rather than a search, so it belongs to whoever is resolving the expression.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn field(&self, id: RecordId, name: Symbol) -> Option<&Field> {
        self.records[id.0 as usize].fields.iter().find(|field| field.name == Some(name))
    }

    /// Declares an `enum` whose underlying type is not decided yet.
    ///
    /// # Panics
    ///
    /// Panics past four billion enumeration declarations in one translation unit.
    pub fn declare_enum(&mut self, tag: Option<Symbol>) -> EnumId {
        let id = EnumId(u32::try_from(self.enums.len()).expect("too many types"));
        self.enums.push(EnumInfo { tag, underlying: None, fixed: false });
        id
    }

    /// The type of a declared enumeration.
    pub fn enumeration(&mut self, id: EnumId) -> TypeId {
        self.intern(Type::new(TypeKind::Enum(id)))
    }

    /// What is known about a declared enumeration.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn enum_info(&self, id: EnumId) -> &EnumInfo {
        &self.enums[id.0 as usize]
    }

    /// Records what an enumeration is represented in, and whether the program said so.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    pub fn complete_enum(&mut self, id: EnumId, underlying: TypeId, fixed: bool) {
        let info = &mut self.enums[id.0 as usize];
        info.underlying = Some(underlying);
        info.fixed = fixed;
    }

    /// A typedef name standing for `underlying`.
    pub fn typedef(&mut self, name: Symbol, underlying: TypeId) -> TypeId {
        self.intern(Type::new(TypeKind::Typedef { name, underlying, align: None }))
    }

    /// The same, for a typedef that said what an object of it is aligned to.
    ///
    /// `align` is in bytes and is what the type is aligned to rather than a floor on it, which
    /// is what `__attribute__((aligned(n)))` means in this one position. See
    /// [`TypeKind::Typedef`].
    pub fn aligned_typedef(
        &mut self,
        name: Symbol,
        underlying: TypeId,
        align: NonZeroU32,
    ) -> TypeId {
        self.intern(Type::new(TypeKind::Typedef { name, underlying, align: Some(align) }))
    }

    /// What a typedef in `id`'s sugar asked an object of it to be aligned to, and [`None`] when
    /// none of them asked for anything.
    ///
    /// The nearest one wins, because `typedef L M __attribute__((aligned(8)))` over an `L` that
    /// asked for two is an eight and not a two: the outer typedef is the one the declaration was
    /// written with. Below the sugar there is nothing to find, since only a typedef can carry one
    /// of these, so the walk stops at the first node that is not one.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different table.
    #[must_use]
    pub fn align_override(&self, id: TypeId) -> Option<NonZeroU32> {
        let mut id = id;
        loop {
            let TypeKind::Typedef { underlying, align, .. } = self.kind(id) else { return None };
            if align.is_some() {
                return align;
            }
            id = underlying;
        }
    }

    /// `id` with `quals` added to whatever it already carries.
    ///
    /// Qualifying an array qualifies its element type and leaves the array itself unqualified,
    /// which is 6.7.3p10 and is not a shortcut. An array type has no qualifiers of its own,
    /// and if it did then `const` on an array parameter would mean nothing at all.
    pub fn qualified(&mut self, id: TypeId, quals: Qualifiers) -> TypeId {
        if quals.is_none() {
            return id;
        }
        let ty = self.get(id);
        if let TypeKind::Array { elem, len } = ty.kind {
            let elem = self.qualified(elem, quals);
            return self.intern(Type { kind: TypeKind::Array { elem, len }, quals: ty.quals });
        }
        self.intern(Type { kind: ty.kind, quals: ty.quals.with(quals) })
    }

    /// `id` with every qualifier removed from its outermost node.
    ///
    /// Only the outermost, because that is what the standard means by the unqualified version
    /// of a type. The pointee of a `const char *` stays `const`.
    pub fn unqualified(&mut self, id: TypeId) -> TypeId {
        let ty = self.get(id);
        if ty.quals.is_none() {
            return id;
        }
        self.intern(Type::new(ty.kind))
    }

    /// The id for `ty`, making one if this is the first time it has been asked for.
    fn intern(&mut self, ty: Type) -> TypeId {
        if let Some(&id) = self.map.get(&ty) {
            return id;
        }
        // Canonicalising can intern other types, which means `self.entries` may have grown by
        // the time this returns and the id below has to be taken afterwards. It cannot have
        // interned `ty` itself, because a canonical type differs from the sugar it came from,
        // but the second lookup is one hash of a cold path against a duplicate entry that
        // would quietly break the promise that equal ids mean equal types.
        let canonical = self.canonicalise(&ty);
        if let Some(&id) = self.map.get(&ty) {
            return id;
        }
        let id = TypeId(Idx::from_usize(self.entries.len()));
        self.entries.push(Entry { ty, canonical: canonical.unwrap_or(id) });
        self.map.insert(ty, id);
        id
    }

    /// The canonical form of `ty`, or `None` when `ty` is already canonical.
    ///
    /// A typedef is not the only place sugar hides. `T *` is sugar when `T` is, and so is an
    /// array of one, and so is a function that returns one, so this rebuilds the type around
    /// whatever its parts canonicalise to rather than only looking at the outermost node.
    fn canonicalise(&mut self, ty: &Type) -> Option<TypeId> {
        match ty.kind {
            TypeKind::Typedef { underlying, .. } => {
                let base = self.canonical(underlying);
                Some(self.qualified(base, ty.quals))
            }
            TypeKind::Pointer(inner) => self.rebuild(ty, inner, TypeKind::Pointer),
            TypeKind::Atomic(inner) => self.rebuild(ty, inner, TypeKind::Atomic),
            TypeKind::Array { elem, len } => {
                self.rebuild(ty, elem, |elem| TypeKind::Array { elem, len })
            }
            TypeKind::Vector { elem, len } => {
                self.rebuild(ty, elem, |elem| TypeKind::Vector { elem, len })
            }
            TypeKind::Function(id) => self.canonicalise_function(ty, id),
            TypeKind::Void
            | TypeKind::Bool
            | TypeKind::Int(_)
            | TypeKind::Float(_)
            | TypeKind::Complex(_)
            | TypeKind::BitInt { .. }
            | TypeKind::Record(_)
            | TypeKind::Enum(_) => None,
        }
    }

    /// The canonical form of a type built out of one other type.
    fn rebuild(
        &mut self,
        ty: &Type,
        inner: TypeId,
        make: impl FnOnce(TypeId) -> TypeKind,
    ) -> Option<TypeId> {
        let canonical = self.canonical(inner);
        if canonical == inner {
            return None;
        }
        Some(self.intern(Type { kind: make(canonical), quals: ty.quals }))
    }

    /// The canonical form of a function type, which is sugar when any part of its signature is.
    fn canonicalise_function(&mut self, ty: &Type, id: FunctionId) -> Option<TypeId> {
        let signature = self.signature(id).clone();
        let ret = self.canonical(signature.ret);
        let params: Vec<TypeId> =
            signature.params.iter().map(|&param| self.canonical(param)).collect();
        if ret == signature.ret && params == signature.params {
            return None;
        }
        let canonical = FunctionType { ret, params, ..signature };
        let id = self.function(canonical);
        Some(self.qualified(id, ty.quals))
    }
}
