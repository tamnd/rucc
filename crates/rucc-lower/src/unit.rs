//! The module level of the walk: what a translation unit's declarations become.
//!
//! Design: `spec/08-ir.md` section 8.9.
//!
//! One typed tree becomes one [`Module`]. A file-scope object becomes a global with an image
//! built from its initializer, a function becomes a [`Func`] whose body is built by
//! [`body`](mod@crate::body), and a string literal becomes an unnamed constant global that
//! whatever mentioned it points at.
//!
//! # What an image is
//!
//! An initializer arrives here already flattened: one entry per scalar that is stored, each
//! with the byte offset it goes at, with every designator and every nested brace already
//! resolved. So building the image is a walk over the entries in offset order, filling the gaps
//! between them with zeros, and the only thing that has to be worked out per entry is whether
//! the value is a number, a run of bytes from a string literal, or the address of something the
//! linker has to place.
//!
//! # Names
//!
//! An object with linkage is known by the name it was written with, and there is nothing to
//! invent. A `static` inside a function has no linkage and still needs a name in the object
//! file, so it gets `name.N`, which is what gcc does and is why two functions may each have a
//! `static int count;` without colliding. A string literal has no name at all and gets
//! `.Lstr.N`, whose leading dot keeps it out of the symbol table on every target that has the
//! convention.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_ir::{
    DataList, Datum, Func, Global, Imm, Linkage as IrLinkage, Module, Reloc, TlsModel, Type,
};
use rucc_sema::{
    Base, Const, Conversion, DeclId, DeclKind, Definition, Eval, ExprId, ExprKind, InitEntry,
    InitList, Linkage, StorageDuration, StrId, Tast,
};
use rucc_target::TargetInfo;
use rucc_types::{TypeId, TypeKind, Types, compatible};

use crate::abi::{self, Plan};
use crate::body;
use crate::repr;

/// Everything the walk reads, which is a checked translation unit and the target it is for.
///
/// The interner is mutable because the walk invents names the program never wrote: the label a
/// string literal is emitted under, and the mangled name of a function-scope `static`.
#[derive(Debug)]
pub struct Context<'a> {
    /// The typed tree.
    pub tast: &'a Tast,
    /// The types it points into.
    pub types: &'a Types,
    /// What is being compiled for, which is where every width and every alignment comes from.
    pub target: &'a TargetInfo,
    /// The name table.
    pub names: &'a mut Interner,
}

/// What the walk produced.
#[derive(Debug)]
pub struct Lowered {
    /// The module, which is complete even when something was reported: a construct that is not
    /// supported yet leaves the rest of the function around it intact.
    pub module: Module,
    /// What was reported, in the order it was found.
    pub diagnostics: Vec<Diagnostic>,
}

/// Walks a checked translation unit and builds the IR for it.
///
/// `name` is the module's name, which is the file the tree came from.
#[must_use]
pub fn lower(name: &str, cx: Context<'_>) -> Lowered {
    let Context { tast, types, target, names } = cx;
    let module = Module::new(names.intern(name), target);
    let mut unit = Unit {
        tast,
        types,
        target,
        names,
        module,
        diagnostics: Vec::new(),
        strings: HashMap::new(),
        statics: HashMap::new(),
        done: HashSet::new(),
    };
    unit.run();
    Lowered { module: unit.module, diagnostics: unit.diagnostics }
}

/// The walk over one translation unit, and everything it has built so far.
pub(crate) struct Unit<'a> {
    pub(crate) tast: &'a Tast,
    pub(crate) types: &'a Types,
    pub(crate) target: &'a TargetInfo,
    pub(crate) names: &'a mut Interner,
    pub(crate) module: Module,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// The global each string literal was emitted as, so that two mentions of one literal are
    /// one object.
    strings: HashMap<StrId, Symbol>,
    /// The name each object with no linkage was given.
    statics: HashMap<DeclId, Symbol>,
    /// What has been emitted, because a redeclaration is the same declaration seen twice.
    done: HashSet<DeclId>,
}

// The debug is by hand and short: a translation unit is not something anybody wants printed as
// a `{:?}`, and the module has a printer of its own for when they do.
impl std::fmt::Debug for Unit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Unit")
            .field("module", &self.module.counts())
            .field("diagnostics", &self.diagnostics.len())
            .finish()
    }
}

impl Unit<'_> {
    /// Every declaration the file made, in the order it made them.
    fn run(&mut self) {
        for index in 0..self.tast.top_level().len() {
            let decl = self.tast.top_level()[index];
            if !self.done.insert(decl) {
                continue;
            }
            match self.tast[decl].kind {
                DeclKind::Function => self.function(decl),
                DeclKind::Object => self.object(decl),
            }
        }
    }

    /// One object with static storage duration.
    fn object(&mut self, decl: DeclId) {
        let tast = self.tast;
        let node = &tast[decl];
        let (ty, state, init) = (node.ty, node.state, node.init);
        let (linkage, duration, alignment) = (node.linkage, node.duration, node.alignment);
        let span = tast.decl_span(decl);
        if duration == StorageDuration::Automatic {
            // A block-scope object with automatic storage is a slot or a value in the function
            // that declares it, and the body is what makes it. Nothing is emitted here.
            return;
        }

        let symbol = self.symbol_of(decl);
        let size = repr::size_of(self.types, self.target, ty);
        let align = alignment.unwrap_or_else(|| repr::align_of(self.types, self.target, ty));
        let mut global = Global::new(symbol, size, align);
        global.linkage = match linkage {
            Linkage::External => IrLinkage::External,
            Linkage::Internal | Linkage::None => IrLinkage::Internal,
        };
        global.tls = (duration == StorageDuration::Thread).then_some(TlsModel::GlobalDynamic);
        global.constant = repr::is_read_only(self.types, ty);
        global.init = match state {
            // `extern int x;` and nothing else names an object another translation unit
            // defines. The global is here so that a reference to it has something to resolve
            // against, and it has no image, which is what makes it a declaration.
            Definition::Declared => None,
            Definition::Tentative => Some(self.zeros(size)),
            Definition::Defined => {
                let (data, covered) = self.image(init, size, span);
                // The object is as large as its image when the image is the larger of the two.
                // A structure whose last member is a flexible array is the only way that
                // happens: `sizeof` answers without the array and an initializer that fills it
                // makes an object big enough to hold what was written. C 6.7.2.1p18 leaves the
                // size to the implementation, gcc grows the object, and this does the same
                // rather than hand the linker a size the image does not fit in.
                global.size = size.max(covered);
                Some(data)
            }
        };
        self.module.add_global(global);
    }

    /// One function, with its body when it has one.
    fn function(&mut self, decl: DeclId) {
        let tast = self.tast;
        let node = &tast[decl];
        let (ty, linkage, body) = (node.ty, node.linkage, node.body);
        let span = tast.decl_span(decl);
        let Some(name) = node.name else { return };
        let Some(plan) = self.plan(ty, &[], span) else { return };

        let mut func = Func::new(name, plan.signature.clone());
        func.linkage = match linkage {
            Linkage::Internal | Linkage::None => IrLinkage::Internal,
            Linkage::External => IrLinkage::External,
        };
        if body.is_some() {
            body::lower(self, decl, &mut func, &plan);
        }
        self.module.add_func(func);
    }

    /// How everything a call to this function type hands over travels, and [`None`] for one the
    /// walk cannot make.
    ///
    /// `actual` is the types of the arguments at a call site, which matter only past the end of
    /// the prototype: what a variadic argument does is decided from what was written there, and
    /// there is no parameter to decide it from. A definition passes nothing for it.
    pub(crate) fn plan(&mut self, ty: TypeId, actual: &[TypeId], span: Span) -> Option<Plan> {
        self.plan_with(ty, actual, false, span)
    }

    /// The same, as the call site sees it rather than as the function does.
    ///
    /// The two differ for a type that is not a prototype. An old style definition is the one of
    /// those that knows what its parameters are, and 6.5.2.2p6 checks a call against a prototype
    /// and against nothing at all otherwise, so a parameter it disagrees with does not make the
    /// call wrong and cannot be what the argument travels as either: the value at the call is
    /// the argument's own type and nothing converted it. So a parameter the argument facing it
    /// is compatible with is used, which is the usual case and is what makes the call go to the
    /// name, and one it is not compatible with gives way to what was actually written. A call
    /// like that is undefined behaviour if control reaches it and the file still has to
    /// translate, which is the same position [`Body::direct`](crate::body) already takes.
    pub(crate) fn call_plan(&mut self, ty: TypeId, actual: &[TypeId], span: Span) -> Option<Plan> {
        self.plan_with(ty, actual, true, span)
    }

    fn plan_with(
        &mut self,
        ty: TypeId,
        actual: &[TypeId],
        at_call: bool,
        span: Span,
    ) -> Option<Plan> {
        let canonical = self.types.canonical(ty);
        let canonical = match self.types.kind(canonical) {
            // A call goes through a pointer to a function, and the type in hand may be either.
            TypeKind::Pointer(pointee) => self.types.canonical(pointee),
            _ => canonical,
        };
        let TypeKind::Function(id) = self.types.kind(canonical) else {
            self.unsupported("a call through something that is not a function", span);
            return None;
        };
        let signature = self.types.signature(id);
        let ret = signature.ret;
        // A function declared without a prototype takes what it is given, which is what a
        // signature with no parameters and no end to them says. C23 removed these and this is
        // what `int f();` means in every dialect before it.
        let variadic = signature.variadic || !signature.prototyped;
        let params = if at_call && !signature.prototyped {
            // An argument past the end of the list has no parameter to travel as, which is what
            // a call to an unprototyped function with more arguments than the definition takes
            // is, so the list ends where the arguments do.
            signature
                .params
                .iter()
                .zip(actual)
                .map(|(&param, &arg)| if compatible(self.types, param, arg) { param } else { arg })
                .collect()
        } else {
            signature.params.clone()
        };

        match abi::plan(self.types, self.target, ret, &params, actual, variadic) {
            Ok(plan) => Some(plan),
            Err(what) => {
                self.unsupported(what, span);
                None
            }
        }
    }

    /// The image of an initializer: the entries in ascending order, with the gaps zeroed, and
    /// how many bytes it covers.
    ///
    /// The count is the size that was asked for except when a flexible array member was given
    /// something to hold, which is the one case where an image is larger than the type it is an
    /// image of.
    pub(crate) fn image(
        &mut self,
        init: Option<InitList>,
        size: u64,
        span: Span,
    ) -> (DataList, u64) {
        let Some(init) = init else { return (self.zeros(size), size) };
        let (data, at) = self.pieces(init, size, span);
        (self.module.push_data(&data), at)
    }

    /// The data an image is made of, before it becomes a [`DataList`].
    ///
    /// This is apart from [`Self::image`] so that an image can be built inside another one,
    /// which is what a compound literal used as a value in an initializer needs.
    fn pieces(&mut self, init: InitList, size: u64, span: Span) -> (Vec<Datum>, u64) {
        let entries = self.in_image_order(&self.tast[init]);
        let mut packed = self.packed(&entries, size);
        let mut data: Vec<Datum> = Vec::with_capacity(entries.len());
        let mut at = 0;
        for entry in entries {
            let piece = self.entry(entry, &mut packed, size);
            if piece.is_empty() {
                continue;
            }
            let covered: u64 = piece.iter().map(|datum| datum.size(&self.module)).sum();
            match entry.offset.cmp(&at) {
                Ordering::Greater => data.push(Datum::Zero(entry.offset - at)),
                // An entry that begins inside the one before it, which is neither the same
                // place nor a later one. A union whose members are initialized through two
                // designators is the way to write it. The earlier bytes are already in the
                // list and the image cannot take them out again, so this is refused, and
                // nothing here is wrong enough to drop the rest of the image.
                Ordering::Less => {
                    self.unsupported("an initializer that writes over an earlier one", span);
                    continue;
                }
                Ordering::Equal => {}
            }
            at = entry.offset + covered;
            data.extend(piece);
        }
        if at < size {
            // The tail of a partly initialized object, which C says is zero. So is the tail of
            // an array the initializer did not fill, and so is every byte of padding.
            data.push(Datum::Zero(size - at));
            at = size;
        }
        (data, at)
    }

    /// The entries an image is written from, which is not the order they were written in.
    ///
    /// A designator names a place, and the places may be named in any order at all:
    /// `{ .b = 2, .a = 1 }` is the same object as `{ .a = 1, .b = 2 }` and C says so in as many
    /// words. An image is bytes in ascending order, so the entries are put in that order here.
    /// The sort is stable, which is what makes the rest of the rule work: naming one place
    /// twice is legal and the last of them is the one that stands, so among the entries at one
    /// offset the written order is kept and all but the last are dropped.
    ///
    /// A bit-field is never dropped, because several of them share one offset without writing
    /// over anything. Which bytes they came to is settled by [`Self::packed`] before this runs
    /// and the whole run goes in under the first entry that has a bit in it.
    fn in_image_order(&self, entries: &[InitEntry]) -> Vec<InitEntry> {
        let mut sorted = entries.to_vec();
        sorted.sort_by_key(|entry| entry.offset);
        let mut kept: Vec<InitEntry> = Vec::with_capacity(sorted.len());
        for entry in sorted {
            if !entry.is_bit_field() {
                let over = |last: &InitEntry| last.offset == entry.offset && !last.is_bit_field();
                while kept.last().is_some_and(over) {
                    kept.pop();
                }
            }
            kept.push(entry);
        }
        kept
    }

    /// What one entry of an initializer puts in the image.
    ///
    /// A bit-field is not a datum of its own, because two of them can live in one byte and an
    /// image is written in bytes. They were put together into their bytes by [`Self::packed`]
    /// before this ran, and the whole run of bytes goes in under the first entry that has a
    /// bit in it, which is why a later one in the same run answers with nothing.
    ///
    /// An entry is usually one datum and a compound literal read is the reason the answer is a
    /// list: that entry is a whole object and puts as many data in as the object it is.
    fn entry(&mut self, entry: InitEntry, packed: &mut BTreeMap<u64, u8>, size: u64) -> Vec<Datum> {
        if entry.is_bit_field() {
            let Some(bytes) = take_run(packed, entry.offset) else { return Vec::new() };
            return vec![Datum::Bytes(self.module.push_bytes(&bytes))];
        }
        if let Some(literal) = self.literal_read(entry.value) {
            return self.literal_image(literal, self.tast.expr_span(entry.value));
        }
        // How much room is left in the object, which is what a string literal longer than the
        // array it initializes is cut down to. An entry that begins where the object ends is the
        // initializer of a flexible array member, and there the object grows to hold what was
        // written rather than the value being cut to fit, so nothing is taken off it.
        let room = if entry.offset < size { size - entry.offset } else { u64::MAX };
        self.datum(entry.value, room).into_iter().collect()
    }

    /// The compound literal an entry reads, if that is what the entry is.
    ///
    /// Reading an object is a node of its own, so a literal used as a value comes through as a
    /// read of a literal. A literal whose address is taken is not a read and is not this: that
    /// one folds to an address and goes in as a relocation, with the object it points at emitted
    /// on its own.
    fn literal_read(&self, value: ExprId) -> Option<DeclId> {
        let ExprKind::Convert { kind: Conversion::Lvalue, operand } = self.tast[value].kind else {
            return None;
        };
        match self.tast[operand].kind {
            ExprKind::CompoundLiteral(decl) => Some(decl),
            _ => None,
        }
    }

    /// The bytes a compound literal contributes where it is read, which are its own image.
    ///
    /// The literal has static storage duration here, since a file-scope initializer is the only
    /// place this is reached from, and C 6.7.11p4 is what lets it stand as a constant element.
    /// Its own initializer is built at the offset the entry is at, so the parent image ends up
    /// with the literal's bytes laid into it rather than a name pointing at a second object.
    fn literal_image(&mut self, literal: DeclId, span: Span) -> Vec<Datum> {
        let size = repr::size_of(self.types, self.target, self.tast[literal].ty);
        let Some(init) = self.tast[literal].init else {
            return if size == 0 { Vec::new() } else { vec![Datum::Zero(size)] };
        };
        self.pieces(init, size, span).0
    }

    /// The bit-fields of an initializer, put together into the bytes they lie in.
    ///
    /// Only the bytes something was stored in are in the map. A field whose value is zero
    /// leaves nothing behind, which is right: what an image does not say is zero anyway. A
    /// field named twice takes only the bits of the field, so the last of them stands and does
    /// not read as the two values together.
    fn packed(&mut self, entries: &[InitEntry], size: u64) -> BTreeMap<u64, u8> {
        let mut bytes = BTreeMap::new();
        for entry in entries.iter().filter(|entry| entry.is_bit_field()) {
            let Some(folded) = self.fold(entry.value) else { continue };
            let Const::Int(number) = folded else {
                let span = self.tast.expr_span(entry.value);
                let what = "a bit-field initialized by something that is not an integer";
                self.unsupported(what, span);
                continue;
            };
            let width = entry.bit_width;
            let ones = if width >= 128 { u128::MAX } else { (1u128 << width) - 1 };
            let mut mask = ones << entry.bit_offset;
            let mut placed = ((number as u128) & ones) << entry.bit_offset;
            let mut at = entry.offset;
            while mask != 0 && at < size {
                let (bits, keep) = ((placed & 0xff) as u8, !((mask & 0xff) as u8));
                if bits != 0 || bytes.contains_key(&at) {
                    let byte = bytes.entry(at).or_insert(0);
                    *byte = (*byte & keep) | bits;
                }
                mask >>= 8;
                placed >>= 8;
                at += 1;
            }
        }
        bytes
    }

    /// One entry of an image, given how many bytes are left in the object it goes in.
    fn datum(&mut self, value: ExprId, room: u64) -> Option<Datum> {
        let tast = self.tast;
        let ty = tast[value].ty;
        let span = tast.expr_span(value);
        if let TypeKind::Array { .. } = self.types.kind(self.types.canonical(ty)) {
            // An array in an initializer is a string literal initializing it, because that is
            // the only way an array is ever a value. `char s[2] = "hi";` drops the terminator,
            // which is the one case where the literal is longer than what it initializes.
            let ExprKind::Str(id) = tast[value].kind else {
                self.unsupported("this initializer", span);
                return None;
            };
            let bytes = tast[id].bytes(self.target);
            let take = bytes.len().min(usize::try_from(room).unwrap_or(usize::MAX));
            return Some(Datum::Bytes(self.module.push_bytes(&bytes[..take])));
        }

        let size = repr::size_of(self.types, self.target, ty);
        match self.fold(value)? {
            Const::Int(number) => {
                let ty = repr::value_type(self.types, self.target, ty)?;
                // An integer constant of pointer type is a null pointer constant, which is what
                // `NULL` is, or an address the program wrote as a number. An image is bytes and
                // `ptr` says nothing about how many, so it goes in as the integer it is at the
                // width the target's addresses have. An address the linker has to fill in is
                // the arm below, and is the only one that stays a pointer.
                let ty = if ty.is_ptr() { Type::int(self.target.pointer_width) } else { ty };
                let imm = self.module.add_imm(Imm::int(number, ty));
                Some(Datum::Scalar { ty, value: imm })
            }
            Const::Float(number) => {
                let ty = repr::value_type(self.types, self.target, ty)?;
                let imm = self.module.add_imm(Imm::from_bits(number.to_bits()));
                Some(Datum::Scalar { ty, value: imm })
            }
            Const::Address(address) => {
                let symbol = match address.base {
                    Base::Decl(decl) => {
                        // A compound literal is an object nothing declares, so the address of
                        // one is also the only thing that asks for it to be emitted. Without
                        // this the image names a symbol the module never defines and the link
                        // is what finds out. Anything with a name of its own is left alone,
                        // since the walk over the unit reaches those on its own.
                        if self.tast[decl].name.is_none() {
                            self.local_static(decl);
                        }
                        self.symbol_of(decl)
                    }
                    Base::Str(id) => self.string(id),
                };
                let addend = i64::try_from(address.offset).unwrap_or(0);
                let size = u32::try_from(size).unwrap_or(0);
                Some(Datum::Addr(self.module.add_reloc(Reloc { symbol, addend, size })))
            }
        }
    }

    /// An image of nothing but zeros, which is what a tentative definition has.
    fn zeros(&mut self, size: u64) -> DataList {
        if size == 0 {
            return DataList::EMPTY;
        }
        self.module.push_data(&[Datum::Zero(size)])
    }

    /// The global a string literal is emitted as, making it the first time it is asked for.
    pub(crate) fn string(&mut self, id: StrId) -> Symbol {
        if let Some(&symbol) = self.strings.get(&id) {
            return symbol;
        }
        let literal = &self.tast[id];
        let bytes = literal.bytes(self.target);
        let align = literal.encoding.element_width(self.target) / 8;
        let symbol = self.names.intern(&format!(".Lstr.{}", self.strings.len()));

        let mut global = Global::new(symbol, bytes.len() as u64, align.max(1));
        global.linkage = IrLinkage::Internal;
        // Not because the type says so, since a literal is an array of `char` and not of
        // `const char`, but because writing to one is undefined and every target puts them
        // somewhere read-only.
        global.constant = true;
        let range = self.module.push_bytes(&bytes);
        global.init = Some(self.module.push_data(&[Datum::Bytes(range)]));
        self.module.add_global(global);
        self.strings.insert(id, symbol);
        symbol
    }

    /// The name an object or a function is known by in the object file.
    pub(crate) fn symbol_of(&mut self, decl: DeclId) -> Symbol {
        let tast = self.tast;
        let node = &tast[decl];
        if node.linkage != Linkage::None {
            return node.name.unwrap_or_else(|| self.names.intern(".Lanon"));
        }
        if let Some(&symbol) = self.statics.get(&decl) {
            return symbol;
        }
        // A `static` in a function, or a compound literal with static storage duration. The
        // number is what makes two of them in two functions two objects.
        let base = match node.name {
            Some(name) => self.names.resolve(name).to_string(),
            None => ".Lanon".to_string(),
        };
        let symbol = self.names.intern(&format!("{base}.{}", self.statics.len()));
        self.statics.insert(decl, symbol);
        symbol
    }

    /// Emits the global for an object with static storage duration declared inside a function.
    pub(crate) fn local_static(&mut self, decl: DeclId) {
        if !self.done.insert(decl) {
            return;
        }
        match self.tast[decl].kind {
            // A function declared inside a body is a declaration of the function, not an
            // object with static storage that happens to be one.
            DeclKind::Function => self.function(decl),
            DeclKind::Object => self.object(decl),
        }
    }

    /// The value of a constant expression, reporting what folding it reported.
    fn fold(&mut self, expr: ExprId) -> Option<Const> {
        let mut eval = Eval::new(self.tast, self.types, self.target, self.names);
        let folded = eval.constant(expr);
        let reported = eval.finish();
        self.diagnostics.extend(reported);
        match folded {
            Ok(value) => Some(value),
            Err(stop) => {
                if !stop.poisoned {
                    let span = self.tast.expr_span(stop.at);
                    self.unsupported("an initializer this compiler cannot fold", span);
                }
                None
            }
        }
    }

    /// Reports a construct the walk does not build IR for yet.
    pub(crate) fn unsupported(&mut self, what: &str, span: Span) {
        self.diagnostics.push(
            Diagnostic::error(format!("{what} is not supported yet"), span).with_code("E0519"),
        );
    }
}

/// The run of bytes a bit-field entry starts, taken out of the map.
///
/// [`None`] when there is no byte at that offset, which means either that every bit-field in
/// it was initialized to zero or that an earlier entry in the same run already took it.
fn take_run(bytes: &mut BTreeMap<u64, u8>, start: u64) -> Option<Vec<u8>> {
    let mut run = vec![bytes.remove(&start)?];
    let mut at = start + 1;
    while let Some(byte) = bytes.remove(&at) {
        run.push(byte);
        at += 1;
    }
    Some(run)
}
