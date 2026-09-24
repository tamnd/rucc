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
//!
//! A declaration points at the name it was written with. `types.kind` for a parameter declared
//! `size_type` answers `unsigned long`, since the name is not in the type, so which name a
//! declaration used is a fact about the declaration and the checker keeps it beside the tree. A
//! parameter, a local or a file-scope object whose whole type was a file-scope typedef name gets
//! that name's `DW_TAG_typedef` as its `DW_AT_type`, and a debugger printing it says `size_type`,
//! the way it does for gcc. One written `const size_type` or `size_type *` still points at the
//! type it has, because the name is only part of it and the entry for the qualified or pointer
//! type is made from the type table, which does not have the name either. That half is open on
//! tamnd/rucc#1817.

use std::collections::{HashMap, HashSet};

use rucc_base::{Interner, Symbol};
use rucc_debug::{Bits, Constant, Encoding, Member, Param, Qualifier, Shape, Sig};
use rucc_diag::{SourceMap, Span};
use rucc_sema::{DeclId, DeclKind, Linkage, Stmt, StmtId, StorageDuration, Tast};
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
    /// What is known about each local the program declared, by the number the declaration has.
    ///
    /// Keyed by the number rather than by a name, because a name is not unique in a unit and is not
    /// unique in a function either: two blocks may each declare an `i` and they are two variables.
    /// The number is the one the lowering wrote onto the memory the local's slot came from, so the
    /// join with what the back end hands back is exact rather than a guess.
    ///
    /// Every declaration in the unit, whether or not the local it names ended up with a slot and
    /// whether or not the function it is in was emitted. Which of them get an entry is decided by
    /// the back end handing over the ones the frame placed, and the rest are never asked for.
    pub locals: HashMap<u32, Named>,
    /// Every scope any local in the unit was declared in, each after the scope it is written inside.
    ///
    /// One table over the whole unit rather than one per function, because a local is looked up by
    /// its declaration number and the number is a fact about the unit. Which function a scope
    /// belongs to is never asked: what reads this reads it through the locals of one function, so
    /// the scopes it reaches are that function's by construction.
    pub scopes: Vec<Scope>,
}

/// One `{ ... }` a local was declared in, which is a `DW_TAG_lexical_block` where anything comes of
/// it.
///
/// A function's own body is not one. A local written straight into it belongs to the function, and
/// the scopes are the ones written inside that.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    /// Which scope this one is written inside, and [`None`] for one written directly in a body.
    ///
    /// Always an earlier entry than this one, since a scope is opened before anything written in it
    /// is reached. What reads this leans on that to build the tree in one pass.
    pub parent: Option<usize>,
    /// The source bytes the compound statement covers, brace to brace.
    ///
    /// This is how a scope turns into addresses, and it is the one thing here that is not simply
    /// carried through. Every machine instruction already knows which source bytes it was built for,
    /// because the line table is written from exactly that, so the instructions of a scope are the
    /// ones whose bytes are inside these and the addresses of a scope are the addresses those
    /// instructions got. Nothing new has to be carried down the compiler for it.
    pub span: Span,
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
    /// Which declaration each of [`Sig::params`] is, in the same order, and [`None`] for a position
    /// the definition's own parameter list did not reach.
    ///
    /// Carried so that a parameter the back end says has a frame slot can be matched to the entry
    /// the signature already wrote for it, rather than given a second entry of its own. The list is
    /// empty for a function whose signature could not be described, since there are no parameters
    /// to line it up against.
    pub params: Vec<Option<u32>>,
    /// Whether anything outside the unit can see it.
    pub external: bool,
}

/// What is known about one local the program declared.
#[derive(Debug, Clone)]
pub(crate) struct Named {
    /// The name it was declared with, as the program spelled it.
    pub name: String,
    /// The file it was declared in, as the source map spells it, for the reason [`Known::file`] is
    /// a name.
    pub file: String,
    /// The line it is declared on, counting from one.
    pub line: u32,
    /// Which entry in [`Meaning::types`] it is, and [`None`] when it cannot be described.
    ///
    /// A local with nothing here still gets an entry, for the reason [`Held::ty`] gives.
    pub ty: Option<usize>,
    /// Which entry in [`Meaning::scopes`] it was declared in, and [`None`] for one written straight
    /// into the body of its function or into no function at all.
    pub scope: Option<usize>,
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
    let mut walk = Walk {
        types,
        target,
        names,
        out: Vec::new(),
        memo: HashMap::new(),
        tags: HashMap::new(),
        spellings: tast.spellings().iter().copied().collect(),
        written: types.aliases().iter().map(|alias| (alias.name, alias.of)).collect(),
        aliases: HashMap::new(),
    };
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
                let described = walk.signature(tast, id);
                let known = Known {
                    file: at.name.to_owned(),
                    line: at.line,
                    sig: described.as_ref().map(|(sig, _)| sig.clone()),
                    params: described.map(|(_, params)| params).unwrap_or_default(),
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
                    ty: walk.declared(id, decl.ty),
                    external,
                };
                objects.insert(symbol, held);
            }
            _ => {}
        }
    }
    // The locals, which are every declaration in the tree with automatic storage duration and a
    // name. The top level above is no use for them: a local is declared inside a body and the top
    // level holds the function. So this is a walk over the declarations rather than over the tree,
    // which reaches one in a nested block the same way it reaches one at the top of a body, and
    // asks nothing about where it was written, since where it was written is a question the tree
    // answers and the number is what the join needs.
    //
    // Parameters are among them, because a parameter is an object of automatic duration and a
    // parameter that has a slot wants an offset like any other local. Which of the two an entry
    // turns out to be is decided where the back end's list is read, against the parameter numbers
    // the function carries.
    //
    // Every declaration, including ones in a function the back end never emitted. A type walked
    // for one of those is an entry in the table nothing points at, which costs its bytes and is
    // read by nothing, and the alternative is to know here which functions survive, which is not
    // decided until code generation has run.
    //
    // Which scope each of them was declared in is the one thing the flat walk cannot answer, since
    // a declaration says nothing about the block it was written in, so the bodies are walked first
    // and what comes back is a lookup from a declaration to a scope.
    let nests = nesting(tast);
    let mut locals = HashMap::new();
    for raw in 0..u32::try_from(tast.counts().decls).unwrap_or(u32::MAX) {
        let id = DeclId::new(raw);
        let decl = &tast[id];
        if decl.kind != DeclKind::Object || decl.duration != StorageDuration::Automatic {
            continue;
        }
        let Some(name) = decl.name else { continue };
        let Some(at) = sources.presumed(tast.decl_span(id).lo) else { continue };
        let ty = walk.declared(id, decl.ty);
        let name = walk.spelled(name);
        let scope = nests.which.get(&raw).copied();
        locals.insert(raw, Named { name, file: at.name.to_owned(), line: at.line, ty, scope });
    }
    // The rest of the typedef names last, which are the ones no declaration above was written
    // with, so that nothing else waits behind a name that may turn out to stand for a type nothing
    // else mentions.
    for &alias in types.aliases() {
        walk.alias(alias.name, alias.of);
    }
    Meaning { types: walk.out, funcs, objects, locals, scopes: nests.out }
}

/// Which `{ ... }` each local in the unit was declared in.
///
/// A walk over the statements of every body rather than over the declarations, because a block is a
/// statement and a declaration carries no note of the one it was written in. Only the blocks matter:
/// everything else is walked through so that a block nested in an `if` or a loop is reached, and
/// nothing else opens a scope.
///
/// Two things that are not blocks and are scopes anyway. A function's own body is a block and is not
/// one of these, because a local written straight into it belongs to the function itself. And
/// `for (int i = 0; ...)` declares `i` in a scope that is the whole `for` statement rather than its
/// body, which is what C 6.8.5p5 says and is what stops the `i` of two loops in a row from being one
/// name declared twice in the same place.
fn nesting(tast: &Tast) -> Nests<'_> {
    let mut nests = Nests { tast, out: Vec::new(), which: HashMap::new() };
    for &id in tast.top_level() {
        let decl = &tast[id];
        if decl.kind != DeclKind::Function {
            continue;
        }
        let Some(body) = decl.body else { continue };
        // The body itself rather than the block it is, so that the top of a function is the function
        // and not a scope inside it.
        if let Stmt::Block(list) = tast[body] {
            for &stmt in &tast[list] {
                nests.walk(stmt, None);
            }
        }
    }
    nests
}

/// The walk that finds the scopes, and what it has found so far.
struct Nests<'a> {
    tast: &'a Tast,
    out: Vec<Scope>,
    /// Which scope each declaration is in, by the number the declaration has.
    which: HashMap<u32, usize>,
}

impl Nests<'_> {
    /// One statement, and every statement inside it, with the scope they are written in.
    fn walk(&mut self, at: StmtId, inside: Option<usize>) {
        match self.tast[at] {
            Stmt::Block(list) => {
                let scope = self.open(self.tast.stmt_span(at), inside);
                for &stmt in &self.tast[list] {
                    self.walk(stmt, Some(scope));
                }
            }
            Stmt::Decls(list) => {
                let Some(scope) = inside else { return };
                for &decl in &self.tast[list] {
                    self.which.insert(decl.raw(), scope);
                }
            }
            Stmt::If { then, otherwise, .. } => {
                self.walk(then, inside);
                if let Some(otherwise) = otherwise {
                    self.walk(otherwise, inside);
                }
            }
            Stmt::While { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::Switch { body, .. }
            | Stmt::Case { body, .. }
            | Stmt::Default { body }
            | Stmt::Label { body, .. } => self.walk(body, inside),
            Stmt::For { init, body, .. } => {
                // A `for` whose first clause declares something is a scope covering the whole
                // statement, and one whose first clause is an expression or nothing is not a scope
                // at all. Opening one either way would cost an entry per loop in the program and
                // say nothing, since a scope with no declaration in it holds no name to tell apart.
                let declares = init.is_some_and(|init| matches!(self.tast[init], Stmt::Decls(_)));
                let inside = match declares {
                    true => Some(self.open(self.tast.stmt_span(at), inside)),
                    false => inside,
                };
                if let Some(init) = init {
                    self.walk(init, inside);
                }
                self.walk(body, inside);
            }
            _ => {}
        }
    }

    /// A new scope inside the one given, and which it is.
    fn open(&mut self, span: Span, inside: Option<usize>) -> usize {
        self.out.push(Scope { parent: inside, span });
        self.out.len() - 1
    }
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
    /// The typedef name each declaration named its type with, for the ones that did.
    spellings: HashMap<DeclId, Symbol>,
    /// Every typedef name written at file scope and the type it stands for, which are the only
    /// names a declaration can point at, since those are the only ones that get an entry.
    written: HashSet<(Symbol, TypeId)>,
    /// Which entry each typedef name went in, once it has one.
    aliases: HashMap<(Symbol, TypeId), Option<usize>>,
}

impl Walk<'_> {
    /// What one function definition takes and gives back, and which declaration each parameter is.
    ///
    /// The numbers come back beside the signature rather than inside it because the signature is
    /// what the DWARF writer is handed and a declaration's number means nothing there. What wants
    /// them is the join with the locals the back end placed, which happens in this crate.
    fn signature(&mut self, tast: &Tast, id: DeclId) -> Option<(Sig, Vec<Option<u32>>)> {
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
        let mut declared = Vec::with_capacity(signature.params.len());
        for (index, &ty) in signature.params.iter().enumerate() {
            let ty = match written.get(index) {
                Some(&param) if tast[param].ty == ty => self.declared(param, ty)?,
                _ => self.told(ty)?,
            };
            let name = written.get(index).and_then(|&param| tast[param].name);
            params.push(Param { name: name.map(|name| self.spelled(name)), ty, spot: None });
            declared.push(written.get(index).map(|param| param.raw()));
        }
        let sig =
            Sig { returns, params, variadic: signature.variadic, prototyped: signature.prototyped };
        Some((sig, declared))
    }

    /// Which entry a declaration's type is, going through the typedef name it was written with.
    ///
    /// The name is used only when the type it stands for is still the declaration's type. The
    /// checker records the name before anything after it has had its say, and `name a[] = {...}`
    /// with `name` an array of unknown length, or an attribute that changes the type, leaves a
    /// declaration whose type the name no longer stands for. Pointing at the name there would
    /// describe an object of the wrong size, so it falls back to the type itself.
    fn declared(&mut self, id: DeclId, ty: TypeId) -> Option<usize> {
        match self.spellings.get(&id) {
            Some(&name) if self.written.contains(&(name, ty)) => self.alias(name, ty),
            _ => self.told(ty),
        }
    }

    /// Which entry a typedef name is, adding it the first time it is asked for.
    ///
    /// A name whose type cannot be described is left out rather than written with no
    /// `DW_AT_type`, since that is how DWARF spells a name for `void`, and a declaration written
    /// with it gets nothing, the same as one written with the type itself would.
    fn alias(&mut self, name: Symbol, of: TypeId) -> Option<usize> {
        if let Some(&at) = self.aliases.get(&(name, of)) {
            return at;
        }
        let at = self.told_or_void(of).map(|of| {
            let name = self.spelled(name);
            self.out.push(Shape::Alias { name, of });
            self.out.len() - 1
        });
        self.aliases.insert((name, of), at);
        at
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
                    // No place, because this is a function type rather than a function: nothing
                    // here is code and there is no frame for a parameter of it to be in.
                    params.push(Param { name: None, ty: self.told(ty)?, spot: None });
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
