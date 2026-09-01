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
use std::collections::{HashMap, HashSet};

use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_ir::{
    DataList, Datum, Func, Global, Imm, Linkage as IrLinkage, Module, Reloc, Signature, TlsModel,
};
use rucc_sema::{
    Base, Const, DeclId, DeclKind, Definition, Eval, ExprId, ExprKind, InitEntry, InitList,
    Linkage, StorageDuration, StrId, Tast,
};
use rucc_target::TargetInfo;
use rucc_types::{TypeId, TypeKind, Types};

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
            Definition::Defined => Some(self.image(init, size, span)),
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
        let Some(signature) = self.signature(ty, span) else { return };

        let mut func = Func::new(name, signature);
        func.linkage = match linkage {
            Linkage::Internal | Linkage::None => IrLinkage::Internal,
            Linkage::External => IrLinkage::External,
        };
        if body.is_some() {
            body::lower(self, decl, &mut func);
        }
        self.module.add_func(func);
    }

    /// The IR signature of a C function type, and [`None`] for one the walk cannot make.
    ///
    /// A structure passed or returned by value is the whole of what is missing, and it is
    /// missing because the answer is the target's rather than C's: the same declaration passes
    /// a pair of registers on one target and a hidden pointer on another.
    pub(crate) fn signature(&mut self, ty: TypeId, span: Span) -> Option<Signature> {
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
        let (ret, variadic) = (signature.ret, signature.variadic);
        let prototyped = signature.prototyped;
        let params = signature.params.clone();

        let mut lowered = Signature::new();
        // A function declared without a prototype takes what it is given, which is what a
        // signature with no parameters and no end to them says. C23 removed these and this is
        // what `int f();` means in every dialect before it.
        lowered.variadic = variadic || !prototyped;
        for param in params {
            match repr::value_type(self.types, self.target, param) {
                Some(ty) => lowered.params.push(ty),
                None => {
                    self.unsupported("passing a structure or a union by value", span);
                    return None;
                }
            }
        }
        if !matches!(self.types.kind(self.types.canonical(ret)), TypeKind::Void) {
            match repr::value_type(self.types, self.target, ret) {
                Some(ty) => lowered.returns.push(ty),
                None => {
                    self.unsupported("returning a structure or a union by value", span);
                    return None;
                }
            }
        }
        Some(lowered)
    }

    /// The image of an initializer: the entries in order, with the gaps between them zeroed.
    pub(crate) fn image(&mut self, init: Option<InitList>, size: u64, span: Span) -> DataList {
        let Some(init) = init else { return self.zeros(size) };
        let entries: Vec<InitEntry> = self.tast[init].to_vec();
        let mut data: Vec<Datum> = Vec::with_capacity(entries.len());
        let mut at = 0;
        for entry in entries {
            if entry.bit_width != 0 {
                self.unsupported("a bit-field with a static storage duration", span);
                continue;
            }
            let room = size.saturating_sub(entry.offset);
            let Some(datum) = self.datum(entry.value, room) else { continue };
            match entry.offset.cmp(&at) {
                Ordering::Greater => data.push(Datum::Zero(entry.offset - at)),
                // Two entries at one offset is one designator writing over another, which is
                // legal and which the image cannot express: the earlier bytes are already in
                // the list. Nothing here is wrong enough to drop the rest of the image.
                Ordering::Less => {
                    self.unsupported("an initializer that writes over an earlier one", span);
                    continue;
                }
                Ordering::Equal => {}
            }
            at = entry.offset + datum.size(&self.module);
            data.push(datum);
        }
        if at < size {
            // The tail of a partly initialized object, which C says is zero. So is the tail of
            // an array the initializer did not fill, and so is every byte of padding.
            data.push(Datum::Zero(size - at));
        }
        self.module.push_data(&data)
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
                    Base::Decl(decl) => self.symbol_of(decl),
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
