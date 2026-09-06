//! Initialization: what an initializer stores, and where.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.4.
//!
//! The job is to turn what was written into a list of values with an offset each, so that
//! nothing downstream has to know what a brace meant. The semantics are a cursor walking a
//! nested object while a list of elements is consumed, and the parts that are hard are the ones
//! that have historically been wrong in every C compiler: brace elision, where
//! `struct { int a[2]; int b; } x = { 1, 2, 3 };` is legal and the boundaries between the
//! sub-objects are worked out rather than written; designation, after which the walk carries on
//! from the new position rather than from where it was; a designation that lands twice on one
//! place, where the later value wins and the earlier one is still evaluated; and a string
//! literal filling a character array, terminator and all.
//!
//! # The shape
//!
//! One level of the object is one call of [`Checker::fill`], and the levels below it are the
//! calls it makes. A level opened by a brace owns the elements of that brace pair, and a level
//! the walk descended into on its own reads the elements of the nearest brace pair above it,
//! which is what makes brace elision a recursion rather than a special case. That is also what
//! makes the designation rule fall out: a designation names a place in the innermost brace
//! pair, so a level that was not opened by a brace stops when it sees one and hands the element
//! back to the level that was.
//!
//! An element that lands on a sub-object of unknown extent moves a high-water mark, and the
//! entry point hands back the type the object ended up with, since `int a[] = { 1, 2, 3 }`
//! declares an `int[3]` and nobody wrote the three.
//!
//! # What an entry is
//!
//! Whatever is at one offset, converted to the type of what is there. A value of array type in
//! an entry is a block copy of that array, which is how a string literal costs one entry rather
//! than one per character and why a one megabyte `#embed` stays cheap. Everything not covered
//! by an entry is zero, which is the convention `= {}` already relies on.
//!
//! # What has to be a constant
//!
//! An object that exists before the program runs is written by the object file rather than by
//! any instruction, so every element of its initializer has to be a constant expression, which
//! for a pointer means an address rather than a number. An automatic object is asked nothing,
//! since it is written where the program reaches it. A `constexpr` object is stricter than a
//! static one rather than looser: C23 gives it a value and not a relocation, so the only
//! pointer it may hold is a null one.
//!
//! Asking is not folding. The entries keep the expressions as they were written, and the walk
//! to the IR folds them where it needs numbers, which is what keeps one answer to `1 << 31`.
//!
//! # The objects with no name
//!
//! A compound literal is here rather than with the other expressions, because what it is is an
//! object with an initializer and the only part of it that is an expression is that it has a
//! place in one. It builds a declaration like any other object, so the walk to the IR lays it
//! out and zero-fills it by the same rules, and it is an lvalue, which is what makes
//! `&(int){ 1 }` and `(struct S){ .a = 1 }.a` things to write. One written in a block lives as
//! long as the block and one written at file scope lives as long as the program.
//!
//! GNU's cast to a union builds the same kind of object from a value rather than from braces,
//! so it is here too, and it is an rvalue because there is no object in the source for it to
//! be a second name for.

use rucc_ast::{self as ast, Designator};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_lex::Encoding;
use rucc_types::{
    ArrayLen, IntKind, RecordId, RecordKind, TypeId, TypeKind, compatible, is_complete,
    is_function, is_void, layout,
};

use crate::check::Checker;
use crate::check::expr::Target;
use crate::decl::{
    Decl, DeclId, DeclKind, DeclList, Definition, InitEntry, InitList, Linkage, StorageDuration,
};
use crate::expr::{Category, Conversion, Expr, ExprId, ExprKind};
use crate::tast::Const;

/// Where one sub-object of the object being initialized sits, and what it is.
#[derive(Debug, Clone, Copy)]
struct Place {
    /// The type of the sub-object.
    ty: TypeId,
    /// Its byte offset from the start of the whole object.
    offset: u64,
    /// The bit offset within the byte at `offset`, for a bit-field.
    bit_offset: u32,
    /// The width in bits, for a bit-field, and zero for everything else.
    bit_width: u32,
    /// How it was reached from the level above, for the note that names it.
    part: Part,
}

/// The step from one level of the object to the sub-object below it, as a diagnostic spells it.
#[derive(Debug, Clone, Copy)]
enum Part {
    /// The whole object, which the note names by the declaration's own name.
    Root,
    /// An element of an array.
    Index(u64),
    /// A member of a record, unnamed when the member is an anonymous `struct` or `union`.
    Field(Option<Symbol>),
}

/// What one level of the object takes.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// An array, with the element type, how many there are when anybody has said, and how big
    /// one is.
    Array {
        /// The element type.
        elem: TypeId,
        /// How many elements, absent for an array whose length the initializer decides.
        len: Option<u64>,
        /// The size of one element in bytes.
        size: u64,
    },
    /// A `struct` or a `union`.
    Record {
        /// Which record.
        record: RecordId,
        /// Whether it is a `union`, which takes one member and not a list of them.
        union: bool,
    },
    /// Anything else, which takes one value.
    Scalar,
}

/// One step of a designation, resolved against the level it was written at.
#[derive(Debug, Clone, Copy)]
struct Step {
    /// Which element or member.
    index: u64,
    /// How many of them the step covers, which is one for everything but a GNU range.
    repeat: u64,
}

/// The elements of one brace pair, and how far into them the walk is.
struct Cursor<'a> {
    /// The elements, which outlive the walk because the untyped tree does.
    items: &'a [ast::InitItem],
    /// Where the run starts in the tree's own table, so that a position names an element
    /// whichever level is looking at it.
    base: usize,
    /// How many have been taken.
    taken: usize,
    /// Whether the designation on the element the cursor is on has already been resolved, which
    /// is what tells a level the walk descended into that the element is now its business.
    resolved: bool,
}

impl<'a> Cursor<'a> {
    fn new(ast: &'a ast::Ast, list: ast::InitItemList) -> Cursor<'a> {
        Cursor { items: &ast[list], base: list.as_usize_range().start, taken: 0, resolved: false }
    }

    /// The element the cursor is on.
    fn peek(&self) -> Option<ast::InitItem> {
        self.items.get(self.taken).copied()
    }

    /// Where that element is in the tree, which is what identifies it across the levels that
    /// look at it before one of them takes it.
    fn at(&self) -> usize {
        self.base + self.taken
    }

    /// Takes it.
    fn bump(&mut self) {
        self.taken += 1;
        self.resolved = false;
    }
}

/// The state of one initializer, which is everything the levels share.
struct Walk {
    /// The values, in the order they were written, so that a designation that lands twice on
    /// one place leaves the later value last.
    entries: Vec<InitEntry>,
    /// The levels from the whole object down to the one being filled, for the note that names
    /// the sub-object an element belongs to.
    stack: Vec<Place>,
    /// The name of the object, for the same note.
    name: Option<Symbol>,
    /// Whether the object lives for the whole program, which is what decides whether a flexible
    /// array member may be initialized.
    is_static: bool,
    /// Whether `constexpr` was written, which is what asks the folding for a value.
    constant: bool,
    /// Whether something was reported that leaves the initializer not worth keeping.
    poisoned: bool,
    /// The value of the element the cursor is on, checked once however many levels look at its
    /// type on the way down.
    cached: Option<(usize, ExprId)>,
}

impl Walk {
    fn new(name: Option<Symbol>, is_static: bool, constant: bool) -> Walk {
        Walk {
            entries: Vec::new(),
            stack: Vec::new(),
            name,
            is_static,
            constant,
            poisoned: false,
            cached: None,
        }
    }

    /// Writes a value at a place.
    fn store(&mut self, place: Place, value: ExprId) {
        self.entries.push(InitEntry {
            offset: place.offset,
            value,
            bit_offset: place.bit_offset,
            bit_width: place.bit_width,
        });
    }
}

impl<'a> Checker<'a> {
    /// The values an initializer stores, and the type the object ended up with.
    ///
    /// The type comes back because `int a[] = { 1, 2, 3 }` declares an `int[3]` and the three is
    /// not written anywhere but here.
    pub(in crate::check) fn init_object(
        &mut self,
        ty: TypeId,
        name: Option<Symbol>,
        is_static: bool,
        init: ast::InitId,
        constant: bool,
        span: Span,
    ) -> Option<(InitList, TypeId)> {
        if self.is_variable_length(ty) {
            // The size is not known until the declaration is reached, so there is nothing for a
            // value to be placed in relative to. C23 lets `= {}` through because zeroing an
            // object of any size needs no offsets.
            let empty = match self.ast[init] {
                ast::Init::List(list) => list.is_empty(),
                ast::Init::Expr(_) => false,
            };
            if !empty {
                self.report(
                    Diagnostic::error(
                        "variable-sized object may not be initialized except with an empty \
                         initializer",
                        span,
                    )
                    .with_code("E0645"),
                );
                return None;
            }
        }
        let mut w = Walk::new(name, is_static, constant);
        let place = Place { ty, offset: 0, bit_offset: 0, bit_width: 0, part: Part::Root };
        let reached = match self.ast[init] {
            ast::Init::List(list) => self.braced(&mut w, place, list, span, true),
            ast::Init::Expr(expr) => self.whole(&mut w, place, expr, span),
        };
        if w.poisoned {
            return None;
        }
        let ty = self.complete(ty, reached);
        Some((self.tast.add_init_entries(&w.entries), ty))
    }

    /// The value an initializer stores and the type it deduced for the object it stores it in.
    ///
    /// The type is the one a use of the initializer would have: an array becomes a pointer to
    /// its first element, a function becomes a pointer to itself, and `const`, `volatile` and
    /// `_Atomic` come off, because what is put into the new object is a value and a value has
    /// none of those. The qualifiers written on the declaration itself go back on afterwards,
    /// so `const auto x = c;` is a `const int` whatever `c` was qualified with.
    pub(in crate::check) fn init_deduced(
        &mut self,
        name: Option<Symbol>,
        is_static: bool,
        init: ast::InitId,
        constant: bool,
        quals: ast::Quals,
        span: Span,
    ) -> Option<(InitList, TypeId)> {
        let ast::Init::Expr(expr) = self.ast[init] else {
            // The parser reads an expression rather than an initializer where a type is being
            // deduced, so a list here is a parse that did not work out and has been reported.
            return None;
        };
        let value = self.expr(expr);
        let value = self.value(value);
        if self.is_poisoned(value) {
            return None;
        }
        let deduced = self.tast[value].ty;
        let ty = self.qualify(deduced, quals, span);
        let mut w = Walk::new(name, is_static, constant);
        let place = Place { ty, offset: 0, bit_offset: 0, bit_width: 0, part: Part::Root };
        self.store_scalar(&mut w, place, value, span);
        if w.poisoned {
            return None;
        }
        Some((self.tast.add_init_entries(&w.entries), ty))
    }

    /// `(T){ ... }`, which builds an unnamed object and is that object rather than its value.
    ///
    /// It is an lvalue, so `&(int){ 1 }` and `(struct S){ .a = 1 }.a` are both things to write,
    /// and one written twice in one function is two objects however alike they look.
    pub(in crate::check) fn compound_literal(
        &mut self,
        ty: ast::TypeNameId,
        init: ast::InitId,
        span: Span,
    ) -> ExprId {
        let ty = self.type_name(ty);
        if is_function(&self.types, ty) {
            self.report(
                Diagnostic::error("compound literal has function type", span).with_code("E0648"),
            );
            return self.poison(span);
        }
        if is_void(&self.types, ty) {
            self.report(
                Diagnostic::error("invalid use of void expression", span).with_code("E0649"),
            );
            return self.poison(span);
        }
        // An array of no length is the one incomplete type allowed here, since what is written
        // between the braces gives it one, which is the same rule a declaration goes by.
        if !is_complete(&self.types, ty) && !self.is_unsized_array(ty) {
            let spelled = self.spell(ty);
            self.report(
                Diagnostic::error(format!("invalid use of undefined type '{spelled}'"), span)
                    .with_code("E0503"),
            );
            return self.poison(span);
        }
        let is_static = self.scopes.at_file_scope();
        let Some((entries, ty)) = self.init_object(ty, None, is_static, init, false, span) else {
            return self.poison(span);
        };
        let decl = self.literal_decl(ty, entries, span);
        self.tast.expr(Expr::new(ExprKind::CompoundLiteral(decl), ty, Category::Lvalue), span)
    }

    /// The unnamed object that a compound literal and a cast to a union each build.
    ///
    /// One written in a block lives as long as the block and one written at file scope lives as
    /// long as the program, which is the difference that decides both whether its address is a
    /// constant and whether what goes into it has to be one.
    pub(in crate::check) fn literal_decl(
        &mut self,
        ty: TypeId,
        entries: InitList,
        span: Span,
    ) -> DeclId {
        let duration = if self.scopes.at_file_scope() {
            StorageDuration::Static
        } else {
            StorageDuration::Automatic
        };
        self.tast.decl(
            Decl {
                name: None,
                ty,
                kind: DeclKind::Object,
                linkage: Linkage::None,
                duration,
                state: Definition::Defined,
                alignment: None,
                constant: false,
                retained: false,
                asm_label: None,
                init: Some(entries),
                params: DeclList::EMPTY,
                body: None,
            },
            span,
        )
    }

    /// An initializer with no braces around it, which is a value and nothing else.
    fn whole(&mut self, w: &mut Walk, place: Place, expr: ast::ExprId, span: Span) -> u64 {
        w.stack.push(place);
        let reached = match self.kind_of(place.ty) {
            Kind::Array { .. } if self.is_string(expr) => self.string_init(w, place, expr, span),
            // A vector is filled like an array of its lanes when braces are written, and it is
            // also a value, which an array is not. So `v4si a = b;` is a copy of one and the
            // assignment rules are what say whether it is allowed, the same as for a scalar.
            Kind::Array { .. } if rucc_types::is_vector(&self.types, place.ty) => {
                let value = self.expr(expr);
                let value = self.value(value);
                self.store_scalar(w, place, value, span);
                1
            }
            Kind::Array { .. } => {
                // There is no value of array type, so a string literal is the only thing that
                // can be written here without braces.
                self.report(Diagnostic::error("invalid initializer", span).with_code("E0616"));
                w.poisoned = true;
                0
            }
            _ => {
                let value = self.expr(expr);
                let value = self.value(value);
                self.store_scalar(w, place, value, span);
                1
            }
        };
        w.stack.pop();
        reached
    }

    /// A brace pair, filling the object at `place` from what is between it.
    ///
    /// `outermost` is the brace the declaration itself was written with, which is the one pair
    /// the standard allows around a scalar.
    fn braced(
        &mut self,
        w: &mut Walk,
        place: Place,
        list: ast::InitItemList,
        span: Span,
        outermost: bool,
    ) -> u64 {
        let kind = self.kind_of(place.ty);
        let mut items = Cursor::new(self.ast, list);
        let Kind::Scalar = kind else {
            return self.fill(w, place, kind, &mut items, true, None);
        };
        w.stack.push(place);
        if !outermost {
            let near = self.near(w);
            self.report(
                Diagnostic::warning("braces around scalar initializer", span)
                    .with_code("E0636")
                    .note(near, span),
            );
        }
        self.scalar_braces(w, place, &mut items);
        w.stack.pop();
        1
    }

    /// What is between a brace pair that turned out to hold a scalar.
    fn scalar_braces(&mut self, w: &mut Walk, place: Place, items: &mut Cursor<'a>) {
        if let Some(item) = items.peek() {
            if !item.designators.is_empty() && !items.resolved {
                // Neither a member name nor an index says anything about a scalar, and which of
                // the two complaints is right depends on which was written.
                self.designate(w, place, item.designators, item.span);
                w.poisoned = true;
            } else if let ast::Init::List(_) = self.ast[item.init] {
                // One pair of braces around a scalar is what 6.7.9 allows. A second says the
                // value is an aggregate, and it is not.
                let near = self.near(w);
                self.report(
                    Diagnostic::error("braces around scalar initializer", item.span)
                        .with_code("E0636")
                        .note(near, item.span),
                );
                w.poisoned = true;
            } else if let ast::Init::Expr(expr) = self.ast[item.init] {
                let value = self.expr(expr);
                let value = self.value(value);
                self.store_scalar(w, place, value, item.span);
            }
            items.bump();
        }
        while let Some(item) = items.peek() {
            self.excess(w, Kind::Scalar, item.span);
            items.bump();
        }
    }

    /// One level of the object, taking elements until it is full or they run out.
    ///
    /// `braced` says a brace pair opened this level, which is what decides whether an element
    /// too many is this level's to complain about and whether a designation is this level's to
    /// resolve. `start` positions the first sub-object, for the rest of a designation that
    /// reached past the level it was written at.
    ///
    /// Gives back one past the highest element reached, which is the length of an array whose
    /// length the initializer decides.
    fn fill(
        &mut self,
        w: &mut Walk,
        place: Place,
        kind: Kind,
        items: &mut Cursor<'a>,
        braced: bool,
        start: Option<Vec<Step>>,
    ) -> u64 {
        w.stack.push(place);
        let mut pending = start;
        let mut next = 0;
        let mut high = 0;
        while let Some(item) = items.peek() {
            let (index, deeper, repeat) = match pending.take() {
                Some(steps) => split(steps),
                None if !item.designators.is_empty() && !items.resolved => {
                    if !braced {
                        // The designation names a place in the innermost brace pair, which is
                        // not this one, so this level is done and the one that owns the brace
                        // takes the element.
                        break;
                    }
                    let Some(steps) = self.designate(w, place, item.designators, item.span) else {
                        items.bump();
                        w.poisoned = true;
                        continue;
                    };
                    items.resolved = true;
                    split(steps)
                }
                None => match self.advance(kind, next) {
                    Some(index) => (index, Vec::new(), 1),
                    None => {
                        if !braced {
                            break;
                        }
                        self.excess(w, kind, item.span);
                        items.bump();
                        continue;
                    }
                },
            };
            let sub = self.sub(place, kind, index).expect("a sub-object of this level");
            let written = w.entries.len();
            if deeper.is_empty() {
                self.element(w, sub, items);
            } else {
                let inner = self.kind_of(sub.ty);
                self.fill(w, sub, inner, items, false, Some(deeper));
            }
            // A GNU range writes one value into a run of elements. What it writes is whatever
            // the one element above produced, moved along by an element each time, which is
            // what makes `[0 ... 1] = { 1, 2 }` cost the same as writing it twice.
            if repeat > 1 {
                let copied: Vec<InitEntry> = w.entries[written..].to_vec();
                let Kind::Array { size, .. } = kind else { unreachable!("a range on an array") };
                for step in 1..repeat {
                    for entry in &copied {
                        let mut moved = *entry;
                        moved.offset += step * size;
                        w.entries.push(moved);
                    }
                }
            }
            next = index + repeat;
            high = high.max(next);
            if let Kind::Record { record, union: true } = kind {
                // A union holds one member at a time, so the one that was just written is the
                // whole of it and anything after it is one too many.
                next = self.types.record_info(record).fields.len() as u64;
            }
        }
        w.stack.pop();
        high
    }

    /// One sub-object, from the element the cursor is on.
    ///
    /// Takes the element when the sub-object is what it initializes, and descends without
    /// taking it when what was written is the start of a list the sub-object holds, which is
    /// brace elision.
    fn element(&mut self, w: &mut Walk, sub: Place, items: &mut Cursor<'a>) {
        let item = items.peek().expect("an element");
        let kind = self.kind_of(sub.ty);
        if let (Kind::Array { len: None, .. }, Part::Field(_)) = (kind, sub.part) {
            // A flexible array member has no size of its own, so what is written into it grows
            // the object. That works where the object is laid out once, and a local is laid out
            // by the stack frame it is in. Braces around what goes in it change nothing, so this
            // is decided before them.
            if !w.is_static {
                let near = self.near(w);
                self.report(
                    Diagnostic::error(
                        "non-static initialization of a flexible array member",
                        item.span,
                    )
                    .with_code("E0644")
                    .note(near, item.span),
                );
                w.poisoned = true;
                items.bump();
                return;
            }
        }
        if let ast::Init::List(list) = self.ast[item.init] {
            items.bump();
            self.braced(w, sub, list, item.span, false);
            return;
        }
        let ast::Init::Expr(expr) = self.ast[item.init] else { return };
        match kind {
            Kind::Scalar => {
                let value = self.item_value(w, items, expr);
                items.bump();
                self.store_scalar(w, sub, value, item.span);
            }
            Kind::Array { .. } if self.is_string(expr) => {
                items.bump();
                self.string_init(w, sub, expr, item.span);
            }
            // A vector is filled like an array of its lanes and is also a value, which an array
            // is not, so a whole one written here takes the whole sub-object rather than
            // starting its lanes. The value's type is what says which of the two was meant, the
            // same way it does for a record: anything that is not a vector is the first lane.
            Kind::Array { .. } if rucc_types::is_vector(&self.types, sub.ty) => {
                let value = self.item_value(w, items, expr);
                let source = self.tast[value].ty;
                if self.is_poisoned(value) || rucc_types::is_vector(&self.types, source) {
                    items.bump();
                    self.store_scalar(w, sub, value, item.span);
                } else {
                    self.elide(w, sub, kind, items, item.span);
                }
            }
            Kind::Array { .. } => {
                self.elide(w, sub, kind, items, item.span);
            }
            Kind::Record { .. } => {
                // A value of a compatible record type initializes the whole of the member. Its
                // type is what says so, which is why it is checked here rather than after the
                // walk has descended into something it does not belong in.
                let value = self.item_value(w, items, expr);
                let (target, source) = (sub.ty, self.tast[value].ty);
                let (bare_target, bare_source) =
                    (self.types.unqualified(target), self.types.unqualified(source));
                if self.is_poisoned(value) || compatible(&self.types, bare_target, bare_source) {
                    items.bump();
                    self.store_scalar(w, sub, value, item.span);
                } else {
                    self.elide(w, sub, kind, items, item.span);
                }
            }
        }
    }

    /// Descends into a sub-object whose braces were left out.
    fn elide(&mut self, w: &mut Walk, sub: Place, kind: Kind, items: &mut Cursor<'a>, span: Span) {
        let before = items.at();
        self.fill(w, sub, kind, items, false, None);
        if items.at() == before {
            // The sub-object has nowhere to put anything, which an empty `struct` and a zero
            // length array both are. Saying the element is one too many is what gcc says and
            // what keeps the walk moving.
            w.stack.push(sub);
            self.excess(w, kind, span);
            w.stack.pop();
            items.bump();
        }
    }

    /// A value at a place, converted to what is there.
    fn store_scalar(&mut self, w: &mut Walk, place: Place, value: ExprId, span: Span) {
        let value = self.assign_to(place.ty, value, span, Target::Initialization);
        if self.is_poisoned(value) {
            w.poisoned = true;
            return;
        }
        if w.constant || w.is_static {
            self.constancy(w, place.ty, value, span);
        }
        w.store(place, value);
    }

    /// What an expression names with the lvalue conversion taken off, if it has one on.
    ///
    /// Reading an object is a node of its own, so a compound literal that is read comes through
    /// as a read of a literal rather than as a literal. Everything else is left alone.
    fn read_through(&self, expr: ExprId) -> ExprId {
        match self.tast[expr].kind {
            ExprKind::Convert { kind: Conversion::Lvalue, operand } => operand,
            _ => expr,
        }
    }

    /// Whether a value is allowed where an object that exists before the program runs is being
    /// initialized, which is where an address constant is a constant and a read of one is not.
    ///
    /// A `constexpr` object is stricter than a static one rather than looser. C23 gives it a
    /// value and not a relocation, so its initializer has to be a number, and the one pointer it
    /// may hold is a null one. gcc words that case separately and so does this.
    fn constancy(&mut self, w: &mut Walk, ty: TypeId, value: ExprId, span: Span) {
        // A complex value has no constant to fold to yet, so asking would refuse an initializer
        // the language allows. That is a gap in the folding rather than a rule about this.
        if matches!(self.types.kind(self.types.canonical(ty)), TypeKind::Complex(_)) {
            return;
        }
        // A whole object built by a compound literal or a cast to a union is a constant when it
        // lives as long as the program does, because what went into it was required to be one
        // where it was written. There is no constant for an object of several members, so this
        // is answered here rather than by the folding.
        // Through the lvalue conversion, because a compound literal is an object and what is
        // written here is a read of it: `struct T t = { (struct S){ 1 }, 2 };` puts the literal
        // where a value goes, and stopping at the conversion made that whole shape a
        // non-constant while `&(struct S){ 1 }`, which is not converted, was fine.
        if let ExprKind::CompoundLiteral(decl) = self.tast[self.read_through(value)].kind {
            if self.tast[decl].duration != StorageDuration::Automatic {
                return;
            }
        }
        let folded = self.eval_constant(value);
        if w.constant && matches!(folded, Ok(Const::Address(_))) {
            self.report(
                Diagnostic::error("'constexpr' pointer initializer is not null", span)
                    .with_code("E0646"),
            );
            w.poisoned = true;
            return;
        }
        if folded.is_ok() {
            return;
        }
        self.report(
            Diagnostic::error("initializer element is not constant", span).with_code("E0618"),
        );
        w.poisoned = true;
    }

    /// A string literal filling a character array, which is one entry and not one per character.
    ///
    /// Gives back how many elements it covers, terminator included, which is the length of an
    /// array whose length it decides.
    fn string_init(&mut self, w: &mut Walk, place: Place, expr: ast::ExprId, span: Span) -> u64 {
        let ast::Expr::Str(id) = self.ast[expr] else { return 0 };
        let (written, encoding) = {
            let literal = &self.ast[id];
            (literal.elements.len() as u64, literal.encoding)
        };
        let Kind::Array { elem, len, size } = self.kind_of(place.ty) else { return 0 };
        let source = self.string_element(encoding);
        if !self.takes_string(elem, encoding, source) {
            let (target, source) = (self.spell(elem), self.spell(source));
            self.report(
                Diagnostic::error(
                    format!(
                        "cannot initialize array of '{target}' from a string literal with type \
                         array of '{source}'"
                    ),
                    span,
                )
                .with_code("E0638"),
            );
            w.poisoned = true;
            return 0;
        }
        // An array with exactly room for the characters and not for the terminator is what
        // `char a[3] = "abc"` is, and it is allowed. The counts gcc prints are in bytes, which
        // is why a `u` literal that does not fit says twelve into four rather than six into two.
        if let Some(len) = len {
            let available = len * size;
            if written * size > available {
                let spelled = self.spell(elem);
                let chars = (written + 1) * size;
                self.report(
                    Diagnostic::warning(
                        format!(
                            "initializer-string for array of '{spelled}' is too long ({chars} \
                             chars into {available} available)"
                        ),
                        span,
                    )
                    .with_code("E0637"),
                );
            }
        }
        // Checked and not converted to a value, because an entry whose value has array type is
        // a block copy and the conversion to a pointer is exactly what would spoil that.
        let value = self.expr(expr);
        let value = self.cut_to_fit(value, len, span);
        w.store(place, value);
        written + 1
    }

    /// The same literal with the type of the array it is filling, when the array is the smaller
    /// of the two.
    ///
    /// C 6.7.10p14 says the terminator goes in only if there is room for it, so `char a[3] =
    /// "abc"` is three characters and no terminator, and the excess of a literal longer still is
    /// discarded rather than written. Both are the array's size deciding how many of the
    /// literal's elements are part of the value, and the type is where that is said: an entry
    /// whose value has array type is a block copy of what the type says the value is. Saying it
    /// here rather than where the image is built is what keeps `const char a[2][3] = { "1234",
    /// "xyz" }` from laying five bytes into the first row of three and then finding the second
    /// row written over.
    ///
    /// The array whose length the initializer decides has no size to cut to and keeps the
    /// literal's own type, which is the type that decides the length.
    fn cut_to_fit(&mut self, value: ExprId, len: Option<u64>, span: Span) -> ExprId {
        let Some(len) = len else { return value };
        let ExprKind::Str(id) = self.tast[value].kind else { return value };
        let Kind::Array { elem, len: Some(had), .. } = self.kind_of(self.tast[value].ty) else {
            return value;
        };
        if had <= len {
            return value;
        }
        let ty = self.types.array(elem, ArrayLen::Fixed(len));
        self.tast.expr(Expr::new(ExprKind::Str(id), ty, Category::Lvalue), span)
    }

    /// Where a designation points, as steps from the level it was written at.
    ///
    /// More than one step when it names a member of an anonymous `struct` or `union`, or when
    /// more than one designator was written, since each of those is a level of its own.
    fn designate(
        &mut self,
        w: &mut Walk,
        place: Place,
        list: ast::DesignatorList,
        span: Span,
    ) -> Option<Vec<Step>> {
        let ast = self.ast;
        let mut steps: Vec<Step> = Vec::new();
        let mut ty = place.ty;
        for &designator in &ast[list] {
            let kind = self.kind_of(ty);
            match designator {
                Designator::Field(name) | Designator::ObsoleteField(name) => {
                    let Kind::Record { record, .. } = kind else {
                        let near = self.near(w);
                        self.report(
                            Diagnostic::error(
                                "field name not in record or union initializer",
                                span,
                            )
                            .with_code("E0639")
                            .note(near, span),
                        );
                        return None;
                    };
                    let Some(path) = self.find_field(record, name) else {
                        let (spelled, name) = (self.spell(ty), self.text(name).to_owned());
                        self.report(
                            Diagnostic::error(
                                format!("'{spelled}' has no member named '{name}'"),
                                span,
                            )
                            .with_code("E0502"),
                        );
                        return None;
                    };
                    // A member of an anonymous member is reached through the one that holds it,
                    // so the designation is as many steps as the path has and not one.
                    let mut at = record;
                    for index in path {
                        let field = self.types.record_info(at).fields[index as usize];
                        steps.push(Step { index: u64::from(index), repeat: 1 });
                        ty = field.ty;
                        if let TypeKind::Record(inner) = self.types.kind(self.types.canonical(ty)) {
                            at = inner;
                        }
                    }
                }
                Designator::Index(index) => {
                    let Kind::Array { elem, len, .. } = kind else {
                        self.not_an_array(w, span);
                        return None;
                    };
                    let at = self.index(w, index, len, span)?;
                    steps.push(Step { index: at, repeat: 1 });
                    ty = elem;
                }
                Designator::Range { lo, hi } => {
                    let Kind::Array { elem, len, .. } = kind else {
                        self.not_an_array(w, span);
                        return None;
                    };
                    let first = self.index(w, lo, len, span)?;
                    let last = self.index(w, hi, len, span)?;
                    if last < first {
                        let near = self.near(w);
                        self.report(
                            Diagnostic::error("empty index range in initializer", span)
                                .with_code("E0643")
                                .note(near, span),
                        );
                        return None;
                    }
                    steps.push(Step { index: first, repeat: last - first + 1 });
                    ty = elem;
                }
            }
        }
        if steps.is_empty() { None } else { Some(steps) }
    }

    /// One index of a designation, folded and checked against the array it names into.
    fn index(
        &mut self,
        w: &mut Walk,
        expr: ast::ExprId,
        len: Option<u64>,
        span: Span,
    ) -> Option<u64> {
        let value = self.expr(expr);
        let value = self.value(value);
        if self.is_poisoned(value) {
            return None;
        }
        let Ok(at) = self.eval_integer(value) else {
            let near = self.near(w);
            self.report(
                Diagnostic::error("nonconstant array index in initializer", span)
                    .with_code("E0642")
                    .note(near, span),
            );
            return None;
        };
        let out_of_bounds = at < 0 || len.is_some_and(|len| at as u128 >= u128::from(len));
        if out_of_bounds {
            let near = self.near(w);
            self.report(
                Diagnostic::error("array index in initializer exceeds array bounds", span)
                    .with_code("E0641")
                    .note(near, span),
            );
            return None;
        }
        u64::try_from(at).ok()
    }

    /// An index written where the thing being initialized is not an array.
    fn not_an_array(&mut self, w: &mut Walk, span: Span) {
        let near = self.near(w);
        self.report(
            Diagnostic::error("array index in non-array initializer", span)
                .with_code("E0640")
                .note(near, span),
        );
    }

    /// An element the level it was written in has no room for.
    fn excess(&mut self, w: &mut Walk, kind: Kind, span: Span) {
        let what = match kind {
            Kind::Array { .. } => "array",
            Kind::Record { union: true, .. } => "union",
            Kind::Record { .. } => "struct",
            Kind::Scalar => "scalar",
        };
        let near = self.near(w);
        self.report(
            Diagnostic::warning(format!("excess elements in {what} initializer"), span)
                .with_code("E0635")
                .note(near, span),
        );
    }

    /// How a note names the sub-object the walk is in.
    fn near(&self, w: &Walk) -> String {
        // A compound literal has no name, and gcc calls the object it builds this rather than
        // leaving the note with nothing where the name goes.
        let mut path = match w.name {
            Some(name) => self.text(name).to_owned(),
            None => "(anonymous)".to_owned(),
        };
        for place in &w.stack {
            match place.part {
                Part::Root | Part::Field(None) => {}
                Part::Index(index) => path.push_str(&format!("[{index}]")),
                Part::Field(Some(name)) => {
                    path.push('.');
                    path.push_str(self.text(name));
                }
            }
        }
        format!("(near initialization for '{path}')")
    }

    /// The value of the element the cursor is on, checked once.
    ///
    /// Brace elision decides where an element goes by looking at its type, and the level that
    /// looks is not always the level that takes it, so without this the same expression would
    /// be walked once per level it was offered to.
    fn item_value(&mut self, w: &mut Walk, items: &Cursor<'a>, expr: ast::ExprId) -> ExprId {
        let at = items.at();
        if let Some((cached, value)) = w.cached {
            if cached == at {
                return value;
            }
        }
        let value = self.expr(expr);
        let value = self.value(value);
        w.cached = Some((at, value));
        value
    }

    /// What one level of the object takes.
    fn kind_of(&self, ty: TypeId) -> Kind {
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Array { elem, len } => {
                let size = layout(&self.types, elem, self.cx.target).map_or(0, |l| l.size);
                let len = match len {
                    ArrayLen::Fixed(len) => Some(len),
                    _ => None,
                };
                Kind::Array { elem, len, size }
            }
            // A vector takes a braced list the way an array of its lanes does, which is what
            // `v4si v = { 1, 2, 3, 4 }` means and the only spelling that gives one a value. It
            // is a fixed length always, since the attribute that built it gave a size.
            TypeKind::Vector { elem, len } => {
                let size = layout(&self.types, elem, self.cx.target).map_or(0, |l| l.size);
                Kind::Array { elem, len: Some(u64::from(len)), size }
            }
            TypeKind::Record(record) => {
                let union = self.types.record_info(record).kind == RecordKind::Union;
                Kind::Record { record, union }
            }
            _ => Kind::Scalar,
        }
    }

    /// The next sub-object at or after `from`, absent when the level is full.
    ///
    /// An unnamed bit-field is skipped, since it has no name to designate and no place in the
    /// order either. An anonymous `struct` or `union` is not skipped: it is a level of its own
    /// and the walk descends into it.
    fn advance(&self, kind: Kind, from: u64) -> Option<u64> {
        match kind {
            Kind::Array { len: Some(len), .. } if from >= len => None,
            Kind::Array { .. } => Some(from),
            Kind::Record { record, .. } => {
                let fields = &self.types.record_info(record).fields;
                let mut at = usize::try_from(from).ok()?;
                while let Some(field) = fields.get(at) {
                    if field.name.is_none() && field.is_bit_field() {
                        at += 1;
                        continue;
                    }
                    return u64::try_from(at).ok();
                }
                None
            }
            Kind::Scalar => (from == 0).then_some(0),
        }
    }

    /// Where one sub-object of a level sits.
    fn sub(&self, place: Place, kind: Kind, index: u64) -> Option<Place> {
        match kind {
            Kind::Array { elem, size, .. } => Some(Place {
                ty: elem,
                offset: place.offset + index * size,
                bit_offset: 0,
                bit_width: 0,
                part: Part::Index(index),
            }),
            Kind::Record { record, .. } => {
                let field =
                    *self.types.record_info(record).fields.get(usize::try_from(index).ok()?)?;
                Some(Place {
                    ty: field.ty,
                    offset: place.offset + field.offset,
                    bit_offset: field.bit,
                    bit_width: field.bits.unwrap_or(0),
                    part: Part::Field(field.name),
                })
            }
            Kind::Scalar => None,
        }
    }

    /// The type an array whose length the initializer decided ended up with.
    fn complete(&mut self, ty: TypeId, reached: u64) -> TypeId {
        let canonical = self.types.canonical(ty);
        let TypeKind::Array { elem, len: ArrayLen::Unknown } = self.types.kind(canonical) else {
            return ty;
        };
        self.types.array(elem, ArrayLen::Fixed(reached))
    }

    /// Whether what was written is a string literal, which an array takes whole.
    fn is_string(&self, expr: ast::ExprId) -> bool {
        matches!(self.ast[expr], ast::Expr::Str(_))
    }

    /// The element type of a string literal, which is the type its array is an array of.
    fn string_element(&self, encoding: Encoding) -> TypeId {
        match encoding {
            Encoding::Plain => self.types.int(IntKind::Char),
            Encoding::Utf8 => self.types.int(self.utf8_char()),
            Encoding::Utf16 => self.types.int(IntKind::UShort),
            Encoding::Utf32 => self.types.int(IntKind::UInt),
            Encoding::Wide => self.wide_char(),
        }
    }

    /// Whether an array of `elem` takes a string literal with that encoding.
    ///
    /// A narrow literal goes into any of the three character types, which is why
    /// `unsigned char a[] = "x"` is fine. A wide one goes into the type it is an array of and
    /// nothing else, which is why `int a[] = U"x"` is not, on a target where `char32_t` is
    /// `unsigned int`.
    fn takes_string(&self, elem: TypeId, encoding: Encoding, source: TypeId) -> bool {
        let bare = self.types.canonical(elem);
        match encoding {
            Encoding::Plain | Encoding::Utf8 => matches!(
                self.types.kind(bare),
                TypeKind::Int(IntKind::Char | IntKind::SChar | IntKind::UChar)
            ),
            _ => self.types.kind(bare) == self.types.kind(self.types.canonical(source)),
        }
    }
}

/// The first step of a designation, what is left of it, and how wide the first step is.
fn split(mut steps: Vec<Step>) -> (u64, Vec<Step>, u64) {
    let first = steps.remove(0);
    (first.index, steps, first.repeat)
}
#[cfg(test)]
mod tests {
    use rucc_ast::{
        ArraySize, AttrList, Builtin, BuiltinSet, DeclSpecs, DeclSpecsId, Declarator, DeclaratorId,
        Derived, Field, Member, Quals, RecordKind, StorageClass, TypeSpec, UnaryOp,
    };
    use rucc_base::Interner;
    use rucc_diag::Span;
    use rucc_lex::{IntConstant, IntConstantType, Remarks, StringLiteral};
    use rucc_session::Std;
    use rucc_target::{TargetInfo, Triple};

    use super::*;
    use crate::check::Context;
    use crate::decl::DeclId;
    use crate::print::Printer;

    /// The untyped tree a test checks, built by hand.
    ///
    /// Everything is built before the checker exists, because the checker borrows the interner
    /// for as long as it lives and a test that has started cannot invent another name.
    struct Fixture {
        ast: ast::Ast,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        fn new() -> Fixture {
            let target =
                TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
            Fixture { ast: ast::Ast::new(), names: Interner::new(), target }
        }

        fn name(&mut self, text: &str) -> Symbol {
            self.names.intern(text)
        }

        /// `int`, as a specifier list the test can add words to before it is added.
        fn int_specs(&self) -> DeclSpecs {
            self.builtin(BuiltinSet::INT)
        }

        fn builtin(&self, keyword: BuiltinSet) -> DeclSpecs {
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            let builtin = Builtin::NONE.add(keyword).expect("a keyword written once");
            specs.ty = TypeSpec::Builtin(builtin);
            specs
        }

        fn declarator(&mut self, name: Option<&str>, derived: &[Derived]) -> DeclaratorId {
            let name = name.map(|name| self.name(name));
            let derived = self.ast.add_derived_list(derived);
            self.ast.add_declarator(Declarator {
                name,
                name_span: Span::DUMMY,
                derived,
                span: Span::DUMMY,
            })
        }

        fn int(&mut self, value: u128) -> ast::ExprId {
            let ty = IntConstantType::Standard(IntKind::Int);
            let id = self.ast.add_int(IntConstant { value, ty, remarks: Remarks::default() });
            self.ast.expr(ast::Expr::Int(id), Span::DUMMY)
        }

        fn use_name(&mut self, text: &str) -> ast::ExprId {
            let name = self.name(text);
            self.ast.expr(ast::Expr::Name(name), Span::DUMMY)
        }

        /// `&x`, which is the one thing a static initializer may hold that a number is not.
        fn address_of(&mut self, text: &str) -> ast::ExprId {
            let operand = self.use_name(text);
            self.ast.expr(ast::Expr::Unary { op: UnaryOp::AddrOf, operand }, Span::DUMMY)
        }

        fn text(&mut self, text: &str, encoding: Encoding) -> ast::ExprId {
            let elements = text.chars().map(|c| c as u32).collect();
            let id = self.ast.add_string(StringLiteral {
                elements,
                encoding,
                remarks: Remarks::default(),
            });
            self.ast.expr(ast::Expr::Str(id), Span::DUMMY)
        }

        /// One member of a record, from its specifiers and its declarator.
        fn field(&mut self, specs: DeclSpecs, name: &str, derived: &[Derived]) -> Member {
            let declarator = Some(self.declarator(Some(name), derived));
            let specs = self.specs(specs);
            Member::Field(Field {
                specs,
                declarator,
                bits: None,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            })
        }

        /// `int x : n;`, and an unnamed one where no name is given.
        fn bit_field(&mut self, specs: DeclSpecs, name: Option<&str>, bits: u128) -> Member {
            let declarator = name.map(|name| self.declarator(Some(name), &[]));
            let bits = Some(self.int(bits));
            let specs = self.specs(specs);
            Member::Field(Field {
                specs,
                declarator,
                bits,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            })
        }

        /// An anonymous `struct` or `union` among the members of another.
        fn anonymous(&mut self, kind: RecordKind, members: &[Member]) -> Member {
            let specs = self.record(kind, None, members);
            let specs = self.specs(specs);
            Member::Field(Field {
                specs,
                declarator: None,
                bits: None,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            })
        }

        /// `struct S`, as a mention of a tag some other declaration defined.
        fn tag(&mut self, kind: RecordKind, tag: &str) -> DeclSpecs {
            let tag = Some(self.name(tag));
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            specs.ty =
                TypeSpec::Record { kind, tag, fields: None, attrs: AttrList::EMPTY, pack: None };
            specs
        }

        /// `struct S { ... }`, as a specifier list.
        fn record(&mut self, kind: RecordKind, tag: Option<&str>, members: &[Member]) -> DeclSpecs {
            let tag = tag.map(|tag| self.name(tag));
            let fields = Some(self.ast.add_member_list(members));
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            specs.ty = TypeSpec::Record { kind, tag, fields, attrs: AttrList::EMPTY, pack: None };
            specs
        }

        /// `= expr`.
        fn value(&mut self, expr: ast::ExprId) -> ast::InitId {
            self.ast.add_init(ast::Init::Expr(expr))
        }

        /// `= { ... }`.
        fn list(&mut self, items: &[ast::InitItem]) -> ast::InitId {
            let items = self.ast.add_init_item_list(items);
            self.ast.add_init(ast::Init::List(items))
        }

        /// One element of a braced list, with the designation that was written before it.
        fn item(&mut self, designators: &[Designator], init: ast::InitId) -> ast::InitItem {
            let designators = self.ast.add_designator_list(designators);
            ast::InitItem { designators, init, span: Span::DUMMY }
        }

        /// An element that is one value, which is what most of them are.
        fn plain(&mut self, expr: ast::ExprId) -> ast::InitItem {
            let init = self.value(expr);
            self.item(&[], init)
        }

        /// An element that is a braced list of its own.
        fn nested(&mut self, items: &[ast::InitItem]) -> ast::InitItem {
            let init = self.list(items);
            self.item(&[], init)
        }

        /// A declaration of one name, from the specifiers, the derivations and the initializer.
        fn var(
            &mut self,
            specs: DeclSpecs,
            name: &str,
            derived: &[Derived],
            init: Option<ast::InitId>,
        ) -> ast::DeclId {
            let declarator = self.declarator(Some(name), derived);
            let item = ast::InitDeclarator {
                declarator,
                init,
                asm_label: None,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            };
            let declarators = self.ast.add_init_declarator_list(&[item]);
            let specs = self.specs(specs);
            self.ast.decl(ast::Decl::Var { specs, declarators }, Span::DUMMY)
        }

        /// `(T)`, as a type name, which is what a compound literal is written with.
        fn type_name(&mut self, specs: DeclSpecs, derived: &[Derived]) -> ast::TypeNameId {
            let declarator = self.declarator(None, derived);
            let specs = self.specs(specs);
            self.ast.add_type_name(ast::TypeName { specs, declarator, span: Span::DUMMY })
        }

        /// `(T){ ... }`.
        fn literal(&mut self, ty: ast::TypeNameId, init: ast::InitId) -> ast::ExprId {
            self.ast.expr(ast::Expr::CompoundLiteral { ty, init }, Span::DUMMY)
        }

        fn specs(&mut self, specs: DeclSpecs) -> DeclSpecsId {
            self.ast.add_specs(specs)
        }

        fn checker(&self) -> Checker<'_> {
            Checker::new(&self.ast, Context::new(&self.names, &self.target, Std::C23))
        }
    }

    /// `[n]`, from whatever expression was written between the brackets.
    fn array(size: ast::ExprId) -> Derived {
        Derived::Array { size: ArraySize::Expr(size), quals: Quals::NONE, has_static: false }
    }

    /// `[]`, whose length the initializer decides.
    fn unsized_array() -> Derived {
        Derived::Array { size: ArraySize::Unspecified, quals: Quals::NONE, has_static: false }
    }

    /// `*`.
    fn pointer() -> Derived {
        Derived::Pointer { quals: Quals::NONE, attrs: AttrList::EMPTY }
    }

    /// Checks one declaration and gives back what it declared.
    fn check(checker: &mut Checker<'_>, decl: ast::DeclId) -> DeclId {
        let declared = checker.check_decl(decl);
        let declared = &checker.tast[declared];
        assert_eq!(declared.len(), 1, "expected exactly one declaration, got {declared:?}");
        declared[0]
    }

    /// One declaration and whatever hangs under it.
    fn dump(checker: &Checker<'_>, id: DeclId) -> String {
        let mut printer = Printer::new(&checker.tast, &checker.types, checker.cx.names);
        printer.decl(id);
        printer.finish()
    }

    /// One expression and whatever hangs under it.
    fn dump_expr(checker: &Checker<'_>, id: ExprId) -> String {
        let mut printer = Printer::new(&checker.tast, &checker.types, checker.cx.names);
        printer.expr(id);
        printer.finish()
    }

    /// What was reported, as the messages alone, notes included.
    fn messages(checker: &Checker<'_>) -> Vec<String> {
        checker
            .errors
            .diagnostics()
            .iter()
            .flat_map(|d| {
                std::iter::once(d.message.clone())
                    .chain(d.children.iter().map(|n| n.message.clone()))
            })
            .collect()
    }

    /// The same, with the severity in front, for the tests where which one it is is the point.
    fn reported(checker: &Checker<'_>) -> Vec<String> {
        checker
            .errors
            .diagnostics()
            .iter()
            .map(|d| format!("{}: {}", d.severity, d.message))
            .collect()
    }

    #[test]
    fn a_scalar_takes_one_value_and_one_pair_of_braces_around_it_is_allowed() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let bare = f.value(one);
        let bare = f.var(f.int_specs(), "a", &[], Some(bare));
        let one = f.int(1);
        let item = f.plain(one);
        let braced = f.list(&[item]);
        let braced = f.var(f.int_specs(), "b", &[], Some(braced));

        let mut c = f.checker();
        c.scopes.push();
        let bare = check(&mut c, bare);
        let braced = check(&mut c, braced);

        assert_eq!(
            dump(&c, bare),
            "decl #0 a : int object automatic defined\n  init\n    +0\n      const 1 : int\n"
        );
        assert_eq!(
            dump(&c, braced),
            "decl #1 b : int object automatic defined\n  init\n    +0\n      const 1 : int\n"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_second_pair_of_braces_around_a_scalar_is_an_error() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let inner = f.plain(one);
        let outer = f.nested(&[inner]);
        let init = f.list(&[outer]);
        let decl = f.var(f.int_specs(), "a", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(decl);

        assert_eq!(
            messages(&c),
            ["braces around scalar initializer", "(near initialization for 'a')"]
        );
    }

    #[test]
    fn a_brace_around_a_scalar_the_walk_descended_into_is_only_a_warning() {
        let mut f = Fixture::new();
        let two = f.int(2);
        let one = f.int(1);
        let inner = f.plain(one);
        let outer = f.nested(&[inner]);
        let init = f.list(&[outer]);
        let decl = f.var(f.int_specs(), "a", &[array(two)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            reported(&c),
            ["warning: braces around scalar initializer"],
            "one brace around a sub-object is what a great deal of code writes"
        );
        assert_eq!(
            messages(&c)[1],
            "(near initialization for 'a[0]')",
            "the note names the scalar and not what holds it"
        );
        assert_eq!(
            dump(&c, id),
            "decl #0 a : int[2] object automatic defined\n  init\n    +0\n      const 1 : int\n"
        );
    }

    #[test]
    fn an_array_takes_its_elements_in_order() {
        let mut f = Fixture::new();
        let three = f.int(3);
        let items: Vec<_> = [1, 2, 3]
            .into_iter()
            .map(|value| {
                let value = f.int(value);
                f.plain(value)
            })
            .collect();
        let init = f.list(&items);
        let decl = f.var(f.int_specs(), "a", &[array(three)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 a : int[3] object automatic defined
  init
    +0
      const 1 : int
    +4
      const 2 : int
    +8
      const 3 : int
"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn an_array_whose_length_nobody_wrote_takes_it_from_what_was_written() {
        let mut f = Fixture::new();
        let items: Vec<_> = [1, 2, 3]
            .into_iter()
            .map(|value| {
                let value = f.int(value);
                f.plain(value)
            })
            .collect();
        let init = f.list(&items);
        let decl = f.var(f.int_specs(), "a", &[unsized_array()], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert!(dump(&c, id).starts_with("decl #0 a : int[3] object"), "{}", dump(&c, id));
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_designation_past_the_end_is_what_decides_the_length_of_such_an_array() {
        let mut f = Fixture::new();
        let three = f.int(3);
        let one = f.int(1);
        let init = f.value(one);
        let item = f.item(&[Designator::Index(three)], init);
        let init = f.list(&[item]);
        let decl = f.var(f.int_specs(), "a", &[unsized_array()], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "decl #0 a : int[4] object automatic defined\n  init\n    +12\n      const 1 : int\n"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn braces_left_out_of_a_nested_array_are_worked_out() {
        let mut f = Fixture::new();
        let two = f.int(2);
        let other = f.int(2);
        let items: Vec<_> = [1, 2, 3, 4]
            .into_iter()
            .map(|value| {
                let value = f.int(value);
                f.plain(value)
            })
            .collect();
        let init = f.list(&items);
        let decl = f.var(f.int_specs(), "a", &[array(two), array(other)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 a : int[2][2] object automatic defined
  init
    +0
      const 1 : int
    +4
      const 2 : int
    +8
      const 3 : int
    +12
      const 4 : int
"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_struct_takes_its_members_in_order_and_what_is_left_over_is_zero() {
        let mut f = Fixture::new();
        let x = f.field(f.int_specs(), "x", &[]);
        let y = f.field(f.int_specs(), "y", &[]);
        let specs = f.record(RecordKind::Struct, Some("S"), &[x, y]);
        let one = f.int(1);
        let item = f.plain(one);
        let init = f.list(&[item]);
        let decl = f.var(specs, "s", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 s : struct S object automatic defined
  init
    +0
      const 1 : int
",
            "the member nobody wrote is not an entry, since what has no entry is zero"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_union_takes_one_member_and_says_so_when_more_is_written() {
        let mut f = Fixture::new();
        let x = f.field(f.int_specs(), "x", &[]);
        let y = f.field(f.builtin(BuiltinSet::CHAR), "y", &[]);
        let specs = f.record(RecordKind::Union, Some("U"), &[x, y]);
        let one = f.int(1);
        let first = f.plain(one);
        let two = f.int(2);
        let second = f.plain(two);
        let init = f.list(&[first, second]);
        let decl = f.var(specs, "u", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            messages(&c),
            ["excess elements in union initializer", "(near initialization for 'u')"]
        );
        assert_eq!(
            dump(&c, id),
            "decl #0 u : union U object automatic defined\n  init\n    +0\n      const 1 : int\n"
        );
    }

    #[test]
    fn a_designation_moves_the_cursor_and_the_walk_carries_on_from_there() {
        let mut f = Fixture::new();
        let four = f.int(4);
        let one = f.int(1);
        let at = f.int(1);
        let init = f.value(one);
        let designated = f.item(&[Designator::Index(at)], init);
        let two = f.int(2);
        let following = f.plain(two);
        let init = f.list(&[designated, following]);
        let decl = f.var(f.int_specs(), "a", &[array(four)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 a : int[4] object automatic defined
  init
    +4
      const 1 : int
    +8
      const 2 : int
",
            "the element after a designation goes after where the designation pointed"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_designation_that_lands_where_another_did_leaves_the_later_value_last() {
        let mut f = Fixture::new();
        let x = f.field(f.int_specs(), "x", &[]);
        let specs = f.record(RecordKind::Struct, Some("S"), &[x]);
        let name = f.name("x");
        let one = f.int(1);
        let init = f.value(one);
        let first = f.item(&[Designator::Field(name)], init);
        let two = f.int(2);
        let init = f.value(two);
        let second = f.item(&[Designator::Field(name)], init);
        let init = f.list(&[first, second]);
        let decl = f.var(specs, "s", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 s : struct S object automatic defined
  init
    +0
      const 1 : int
    +0
      const 2 : int
",
            "both are kept and in the order written, since the first one may have a side effect"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_member_of_an_anonymous_member_is_reached_through_the_one_that_holds_it() {
        let mut f = Fixture::new();
        let q = f.field(f.int_specs(), "q", &[]);
        let held = f.anonymous(RecordKind::Struct, &[q]);
        let r = f.field(f.int_specs(), "r", &[]);
        let specs = f.record(RecordKind::Struct, Some("T"), &[held, r]);
        let name = f.name("q");
        let one = f.int(1);
        let init = f.value(one);
        let designated = f.item(&[Designator::Field(name)], init);
        let two = f.int(2);
        let following = f.plain(two);
        let init = f.list(&[designated, following]);
        let decl = f.var(specs, "t", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 t : struct T object automatic defined
  init
    +0
      const 1 : int
    +4
      const 2 : int
",
            "the anonymous member fills up and the element after it goes to the one beside it"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_gnu_range_writes_one_value_into_a_run_of_elements() {
        let mut f = Fixture::new();
        let four = f.int(4);
        let lo = f.int(1);
        let hi = f.int(3);
        let seven = f.int(7);
        let init = f.value(seven);
        let item = f.item(&[Designator::Range { lo, hi }], init);
        let init = f.list(&[item]);
        let decl = f.var(f.int_specs(), "a", &[array(four)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 a : int[4] object automatic defined
  init
    +4
      const 7 : int
    +8
      const 7 : int
    +12
      const 7 : int
"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_string_literal_fills_a_character_array_and_is_one_entry() {
        let mut f = Fixture::new();
        let literal = f.text("hi", Encoding::Plain);
        let init = f.value(literal);
        let decl = f.var(f.builtin(BuiltinSet::CHAR), "a", &[unsized_array()], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 a : char[3] object automatic defined
  init
    +0
      string \"hi\" : char[3] lvalue
",
            "one entry of array type, which is a block copy and not three stores"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_string_literal_that_only_loses_its_terminator_fits_and_a_longer_one_does_not() {
        let mut f = Fixture::new();
        let three = f.int(3);
        let exact = f.text("abc", Encoding::Plain);
        let init = f.value(exact);
        let exact = f.var(f.builtin(BuiltinSet::CHAR), "a", &[array(three)], Some(init));
        let other = f.int(3);
        let long = f.text("hello", Encoding::Plain);
        let init = f.value(long);
        let long = f.var(f.builtin(BuiltinSet::CHAR), "b", &[array(other)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(exact);
        c.check_decl(long);

        assert_eq!(
            reported(&c),
            ["warning: initializer-string for array of 'char' is too long (6 chars into 3 \
                 available)"]
        );
    }

    #[test]
    fn a_string_literal_takes_the_type_of_the_array_it_has_to_fit_in() {
        let mut f = Fixture::new();
        let three = f.int(3);
        let exact = f.text("abc", Encoding::Plain);
        let init = f.value(exact);
        let decl = f.var(f.builtin(BuiltinSet::CHAR), "a", &[array(three)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        // The literal is a `char[4]` everywhere else it appears. Here the array has room for
        // three of its elements, so three of them are the value, and the type is where an entry
        // of array type says how many bytes it copies.
        assert_eq!(
            dump(&c, id),
            "\
decl #0 a : char[3] object automatic defined
  init
    +0
      string \"abc\" : char[3] lvalue
",
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn an_array_of_the_wrong_element_type_refuses_a_string_literal() {
        let mut f = Fixture::new();
        let two = f.int(2);
        let literal = f.text("hi", Encoding::Plain);
        let init = f.value(literal);
        let decl = f.var(f.int_specs(), "a", &[array(two)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(decl);

        assert_eq!(
            messages(&c),
            ["cannot initialize array of 'int' from a string literal with type array of 'char'"]
        );
    }

    #[test]
    fn a_wide_literal_goes_into_the_type_it_is_an_array_of_and_a_narrow_one_into_any_character() {
        let mut f = Fixture::new();
        let narrow = f.text("x", Encoding::Plain);
        let init = f.value(narrow);
        let mut specs = f.builtin(BuiltinSet::CHAR);
        specs.ty = TypeSpec::Builtin(
            Builtin::NONE
                .add(BuiltinSet::UNSIGNED)
                .and_then(|b| b.add(BuiltinSet::CHAR))
                .expect("unsigned char"),
        );
        let narrow = f.var(specs, "a", &[unsized_array()], Some(init));
        let wide = f.text("x", Encoding::Utf32);
        let init = f.value(wide);
        let wide = f.var(f.int_specs(), "b", &[unsized_array()], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(narrow);
        c.check_decl(wide);

        assert_eq!(
            messages(&c),
            ["cannot initialize array of 'int' from a string literal with type array of \
                 'unsigned int'"],
            "the narrow one is fine and the wide one is not, since char32_t is unsigned here"
        );
    }

    #[test]
    fn elements_the_object_has_no_room_for_name_the_sub_object_they_were_written_in() {
        let mut f = Fixture::new();
        let two = f.int(2);
        let other = f.int(2);
        let items: Vec<_> = [1, 2, 3]
            .into_iter()
            .map(|value| {
                let value = f.int(value);
                f.plain(value)
            })
            .collect();
        let inner = f.nested(&items);
        let init = f.list(&[inner]);
        let decl = f.var(f.int_specs(), "a", &[array(two), array(other)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(decl);

        assert_eq!(
            messages(&c),
            ["excess elements in array initializer", "(near initialization for 'a[0]')"],
            "the note names the row and not the whole of it"
        );
    }

    #[test]
    fn a_designation_that_names_something_the_object_does_not_have_is_refused() {
        let mut f = Fixture::new();
        let x = f.field(f.int_specs(), "x", &[]);
        let specs = f.record(RecordKind::Struct, Some("S"), &[x]);
        let missing = f.name("z");
        let one = f.int(1);
        let init = f.value(one);
        let item = f.item(&[Designator::Field(missing)], init);
        let init = f.list(&[item]);
        let no_member = f.var(specs, "s", &[], Some(init));

        let three = f.int(3);
        let past = f.int(5);
        let one = f.int(1);
        let init = f.value(one);
        let item = f.item(&[Designator::Index(past)], init);
        let init = f.list(&[item]);
        let out_of_bounds = f.var(f.int_specs(), "a", &[array(three)], Some(init));

        let three = f.int(3);
        let name = f.name("x");
        let one = f.int(1);
        let init = f.value(one);
        let item = f.item(&[Designator::Field(name)], init);
        let init = f.list(&[item]);
        let not_a_record = f.var(f.int_specs(), "b", &[array(three)], Some(init));

        let x = f.field(f.int_specs(), "x", &[]);
        let specs = f.record(RecordKind::Struct, Some("T"), &[x]);
        let zero = f.int(0);
        let one = f.int(1);
        let init = f.value(one);
        let item = f.item(&[Designator::Index(zero)], init);
        let init = f.list(&[item]);
        let not_an_array = f.var(specs, "t", &[], Some(init));

        let three = f.int(3);
        let lo = f.int(2);
        let hi = f.int(0);
        let one = f.int(1);
        let init = f.value(one);
        let item = f.item(&[Designator::Range { lo, hi }], init);
        let init = f.list(&[item]);
        let empty_range = f.var(f.int_specs(), "c", &[array(three)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        for decl in [no_member, out_of_bounds, not_a_record, not_an_array, empty_range] {
            c.check_decl(decl);
        }

        assert_eq!(
            reported(&c),
            [
                "error: 'struct S' has no member named 'z'",
                "error: array index in initializer exceeds array bounds",
                "error: field name not in record or union initializer",
                "error: array index in non-array initializer",
                "error: empty index range in initializer",
            ]
        );
    }

    #[test]
    fn an_index_that_is_not_a_constant_is_refused() {
        let mut f = Fixture::new();
        let counter = f.var(f.int_specs(), "n", &[], None);
        let three = f.int(3);
        let n = f.use_name("n");
        let one = f.int(1);
        let init = f.value(one);
        let item = f.item(&[Designator::Index(n)], init);
        let init = f.list(&[item]);
        let decl = f.var(f.int_specs(), "a", &[array(three)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(counter);
        c.check_decl(decl);

        assert_eq!(
            messages(&c),
            ["nonconstant array index in initializer", "(near initialization for 'a')"]
        );
    }

    #[test]
    fn a_bit_field_is_written_where_its_bits_start_and_not_where_its_byte_does() {
        let mut f = Fixture::new();
        let first = f.bit_field(f.int_specs(), Some("x"), 3);
        let unnamed = f.bit_field(f.int_specs(), None, 2);
        let second = f.bit_field(f.int_specs(), Some("y"), 4);
        let specs = f.record(RecordKind::Struct, Some("S"), &[first, unnamed, second]);
        let one = f.int(1);
        let first = f.plain(one);
        let two = f.int(2);
        let second = f.plain(two);
        let init = f.list(&[first, second]);
        let decl = f.var(specs, "s", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 s : struct S object automatic defined
  init
    +0 bit 0 width 3
      const 1 : int
    +0 bit 5 width 4
      const 2 : int
",
            "the unnamed one is skipped and the one after it keeps the bits it was given"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_flexible_array_member_may_be_initialized_where_the_object_is_laid_out_once() {
        let mut f = Fixture::new();
        let n = f.field(f.int_specs(), "n", &[]);
        let rest = f.field(f.int_specs(), "a", &[unsized_array()]);
        let specs = f.record(RecordKind::Struct, Some("F"), &[n, rest]);
        let one = f.int(1);
        let count = f.plain(one);
        let two = f.int(2);
        let element = f.plain(two);
        let tail = f.nested(&[element]);
        let init = f.list(&[count, tail]);
        let local = f.var(specs, "l", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(local);

        assert_eq!(
            messages(&c),
            [
                "non-static initialization of a flexible array member",
                "(near initialization for 'l')"
            ]
        );
    }

    #[test]
    fn a_variable_length_object_takes_nothing_but_an_empty_initializer() {
        let mut f = Fixture::new();
        let length = f.var(f.int_specs(), "n", &[], None);
        let n = f.use_name("n");
        let one = f.int(1);
        let item = f.plain(one);
        let init = f.list(&[item]);
        let written = f.var(f.int_specs(), "a", &[array(n)], Some(init));
        let n = f.use_name("n");
        let init = f.list(&[]);
        let empty = f.var(f.int_specs(), "b", &[array(n)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(length);
        c.check_decl(written);
        c.check_decl(empty);

        assert_eq!(
            messages(&c),
            ["variable-sized object may not be initialized except with an empty initializer"]
        );
    }

    #[test]
    fn a_struct_takes_a_value_of_its_own_type_whole_and_refuses_anything_else() {
        let mut f = Fixture::new();
        let x = f.field(f.int_specs(), "x", &[]);
        let specs = f.record(RecordKind::Struct, Some("S"), &[x]);
        let source = f.var(specs, "s", &[], None);
        let other = f.use_name("s");
        let init = f.value(other);
        let specs = f.tag(RecordKind::Struct, "S");
        let copied = f.var(specs, "t", &[], Some(init));
        let one = f.int(1);
        let init = f.value(one);
        let specs = f.tag(RecordKind::Struct, "S");
        let refused = f.var(specs, "u", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(source);
        let id = check(&mut c, copied);
        c.check_decl(refused);

        assert_eq!(
            dump(&c, id),
            "\
decl #1 t : struct S object automatic defined
  init
    +0
      convert lvalue : struct S
        decl #0 s : struct S lvalue
",
            "a value of the object's own type is stored whole and is not walked into"
        );
        assert_eq!(messages(&c), ["invalid initializer"]);
    }

    #[test]
    fn an_array_takes_no_value_at_all_without_braces() {
        let mut f = Fixture::new();
        let two = f.int(2);
        let one = f.int(1);
        let init = f.value(one);
        let decl = f.var(f.int_specs(), "a", &[array(two)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(decl);

        assert_eq!(messages(&c), ["invalid initializer"]);
    }

    #[test]
    fn a_constexpr_object_asks_the_folding_for_its_value() {
        let mut f = Fixture::new();
        let source = f.var(f.int_specs(), "n", &[], None);
        let mut specs = f.int_specs();
        specs.constexpr = true;
        let n = f.use_name("n");
        let init = f.value(n);
        let decl = f.var(specs, "a", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(source);
        c.check_decl(decl);

        assert_eq!(messages(&c), ["initializer element is not constant"]);
    }

    #[test]
    fn a_static_object_may_hold_an_address_and_may_not_hold_a_read_of_one() {
        let mut f = Fixture::new();
        let source = f.var(f.int_specs(), "a", &[], None);
        let taken = f.address_of("a");
        let init = f.value(taken);
        let held = f.var(f.int_specs(), "p", &[pointer()], Some(init));
        let read = f.use_name("a");
        let init = f.value(read);
        let copied = f.var(f.int_specs(), "q", &[], Some(init));

        let mut c = f.checker();
        c.check_decl(source);
        let held = check(&mut c, held);
        c.check_decl(copied);

        assert_eq!(
            dump(&c, held),
            "\
decl #1 p : int * object external static defined
  init
    +0
      unary & : int *
        decl #0 a : int lvalue
",
            "the value is kept as written, since asking whether it folds is not folding it"
        );
        assert_eq!(
            messages(&c),
            ["initializer element is not constant"],
            "reading the object is not, since nothing has put a value in it yet"
        );
    }

    #[test]
    fn an_automatic_object_asks_nothing_about_what_goes_in_it() {
        let mut f = Fixture::new();
        let source = f.var(f.int_specs(), "a", &[], None);
        let read = f.use_name("a");
        let init = f.value(read);
        let copied = f.var(f.int_specs(), "b", &[], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(source);
        c.check_decl(copied);

        assert!(c.errors.is_empty(), "a local is written to when the program reaches it");
    }

    #[test]
    fn a_constexpr_pointer_has_to_be_null() {
        let mut f = Fixture::new();
        let source = f.var(f.int_specs(), "a", &[], None);
        let mut specs = f.int_specs();
        specs.constexpr = true;
        let taken = f.address_of("a");
        let init = f.value(taken);
        let decl = f.var(specs, "p", &[pointer()], Some(init));

        let mut c = f.checker();
        c.check_decl(source);
        c.check_decl(decl);

        assert_eq!(
            messages(&c),
            ["'constexpr' pointer initializer is not null"],
            "a constexpr object holds a value and an address is not one until the link"
        );
    }

    #[test]
    fn an_empty_initializer_is_still_an_initializer() {
        let mut f = Fixture::new();
        let two = f.int(2);
        let init = f.list(&[]);
        let decl = f.var(f.int_specs(), "a", &[array(two)], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "decl #0 a : int[2] object automatic defined\n  init\n",
            "an initializer that is present and empty zeroes the object"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_pointer_takes_a_string_literal_as_a_pointer_and_an_array_takes_it_whole() {
        let mut f = Fixture::new();
        let two = f.int(2);
        let first = f.text("a", Encoding::Plain);
        let first = f.plain(first);
        let second = f.text("b", Encoding::Plain);
        let second = f.plain(second);
        let init = f.list(&[first, second]);
        let mut specs = f.builtin(BuiltinSet::CHAR);
        specs.ty = TypeSpec::Builtin(Builtin::NONE.add(BuiltinSet::CHAR).expect("char"));
        let decl = f.var(specs, "a", &[array(two), pointer()], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "\
decl #0 a : char *[2] object automatic defined
  init
    +0
      convert array-decay : char *
        string \"a\" : char[2] lvalue
    +8
      convert array-decay : char *
        string \"b\" : char[2] lvalue
",
            "the array of pointers decays each literal, which the array of characters does not"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_compound_literal_in_a_block_is_an_object_of_its_own_and_an_lvalue() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let item = f.plain(one);
        let init = f.list(&[item]);
        let ty = f.type_name(f.int_specs(), &[]);
        let literal = f.literal(ty, init);

        let mut c = f.checker();
        c.scopes.push();
        let id = c.check_expr(literal);

        assert_eq!(c.tast[id].category, Category::Lvalue);
        assert_eq!(
            dump_expr(&c, id),
            "compound-literal #0 : int lvalue\n  decl #0 : int object automatic defined\n    \
             init\n      +0\n        const 1 : int\n"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_compound_literal_at_file_scope_lives_as_long_as_the_program_and_its_address_is_a_constant()
    {
        let mut f = Fixture::new();
        let items: Vec<_> = [1, 2, 3]
            .into_iter()
            .map(|value| {
                let value = f.int(value);
                f.plain(value)
            })
            .collect();
        let init = f.list(&items);
        let ty = f.type_name(f.int_specs(), &[unsized_array()]);
        let literal = f.literal(ty, init);
        let init = f.value(literal);
        let decl = f.var(f.int_specs(), "p", &[pointer()], Some(init));

        let mut c = f.checker();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "decl #0 p : int * object external static defined\n  init\n    +0\n      convert \
             array-decay : int *\n        compound-literal #1 : int[3] lvalue\n          decl \
             #1 : int[3] object static defined\n            init\n              \
             +0\n                const 1 : int\n              +4\n                const 2 : \
             int\n              +8\n                const 3 : int\n"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_compound_literal_of_a_type_no_object_can_have_says_which_one_it_was() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let item = f.plain(one);
        let init = f.list(&[item]);
        let params = f.ast.add_param_list(&[]);
        let call = Derived::Function { params, variadic: false, kind: ast::ParamKind::Void };
        let function = f.type_name(f.int_specs(), &[call]);
        let function = f.literal(function, init);
        let void = f.type_name(f.builtin(BuiltinSet::VOID), &[]);
        let void = f.literal(void, init);
        let tag = f.tag(RecordKind::Struct, "S");
        let incomplete = f.type_name(tag, &[]);
        let incomplete = f.literal(incomplete, init);

        let mut c = f.checker();
        c.scopes.push();
        c.check_expr(function);
        c.check_expr(void);
        c.check_expr(incomplete);

        assert_eq!(
            messages(&c),
            [
                "compound literal has function type",
                "invalid use of void expression",
                "invalid use of undefined type 'struct S'",
            ]
        );
    }

    #[test]
    fn two_compound_literals_written_alike_are_two_objects() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let item = f.plain(one);
        let init = f.list(&[item]);
        let ty = f.type_name(f.int_specs(), &[]);
        let first = f.literal(ty, init);
        let second = f.literal(ty, init);

        let mut c = f.checker();
        c.scopes.push();
        let first = c.check_expr(first);
        let second = c.check_expr(second);

        let ExprKind::CompoundLiteral(first) = c.tast[first].kind else { panic!("a literal") };
        let ExprKind::CompoundLiteral(second) = c.tast[second].kind else { panic!("a literal") };
        assert_ne!(first, second);
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_static_object_is_not_initialized_by_a_literal_that_lives_in_a_block() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let item = f.plain(one);
        let init = f.list(&[item]);
        let ty = f.type_name(f.int_specs(), &[unsized_array()]);
        let literal = f.literal(ty, init);
        let init = f.value(literal);
        let mut specs = f.int_specs();
        specs.storage = Some(StorageClass::Static);
        let decl = f.var(specs, "p", &[pointer()], Some(init));

        let mut c = f.checker();
        c.scopes.push();
        c.check_decl(decl);

        assert_eq!(messages(&c), ["initializer element is not constant"]);
    }

    #[test]
    fn a_note_about_a_compound_literal_calls_it_anonymous_since_it_has_no_name() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let first = f.plain(one);
        let two = f.int(2);
        let second = f.plain(two);
        let init = f.list(&[first, second]);
        let one = f.int(1);
        let ty = f.type_name(f.int_specs(), &[array(one)]);
        let literal = f.literal(ty, init);

        let mut c = f.checker();
        c.scopes.push();
        c.check_expr(literal);

        assert_eq!(
            messages(&c),
            ["excess elements in array initializer", "(near initialization for '(anonymous)')",]
        );
    }

    #[test]
    fn a_cast_to_a_union_is_a_constant_where_what_went_into_it_was_one() {
        let mut f = Fixture::new();
        let int = f.int_specs();
        let member = f.field(int, "i", &[]);
        let definition = f.record(RecordKind::Union, Some("U"), &[member]);
        let mention = f.tag(RecordKind::Union, "U");
        let ty = f.type_name(mention, &[]);
        let one = f.int(1);
        let cast = f.ast.expr(ast::Expr::Cast { ty, operand: one }, Span::DUMMY);
        let init = f.value(cast);
        let decl = f.var(definition, "u", &[], Some(init));

        let mut c = f.checker();
        let id = check(&mut c, decl);

        assert_eq!(
            dump(&c, id),
            "decl #0 u : union U object external static defined\n  init\n    +0\n      \
             compound-literal #1 : union U\n        decl #1 : union U object static \
             defined\n          init\n            +0\n              const 1 : int\n"
        );
        assert!(c.errors.is_empty());
    }
}
