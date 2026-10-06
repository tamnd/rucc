//! The arenas of the typed tree, and everything that hangs off them.
//!
//! Design: `spec/03-architecture.md` section 3.3 and `spec/07-types-and-semantics.md` section
//! 7.14.
//!
//! The same shape as the untyped tree and for the same reasons: flat vectors, four-byte
//! indices, spans out of line, one owner per translation unit and one drop at the end of it.
//! What is different is that a type is in the node rather than beside it, because every walk
//! over this tree reads the type of every node it touches, which is exactly not true of spans.
//!
//! One [`Tast`] does not own the [`Types`](rucc_types::Types) its nodes point into. A type
//! outlives the tree that mentions it, the two are built together and handed on together, and
//! putting the table inside the tree would mean a pass that only wants to ask what a type is
//! has to borrow the tree to do it.

use std::fmt;
use std::ops::Index;

use rucc_base::float::Float;
use rucc_base::hash::{Map, Set};
use rucc_base::{Idx, IdxRange, Symbol};
use rucc_diag::Span;
use rucc_lex::StringLiteral;
use rucc_target::Isa;
use rucc_types::{TypeId, VlaId};

use crate::asm::{Asm, AsmId, AsmOperand, AsmOperandList, FileAsm, LabelList, StrList};
use crate::decl::{Decl, DeclId, DeclList, InitEntry};
use crate::expr::{CpuObject, Expr, ExprId, ExprList};
use crate::stmt::{Case, CaseId, Stmt, StmtId, StmtList};

/// A folded constant, in the value table.
pub type ConstId = Idx<Const>;

/// A string literal, in the literal table.
pub type StrId = Idx<StringLiteral>;

/// The messages `__attribute__((error("...")))` and `__attribute__((warning("...")))` put on a
/// function, for a call to it that survives optimization. A function may carry both, and gcc then
/// reports both at each such call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Notices {
    /// The message of an `error` attribute.
    pub error: Option<StrId>,
    /// The message of a `warning` attribute.
    pub warning: Option<StrId>,
}

/// The arguments `__attribute__((alloc_size(...)))` names, counted from one as the attribute
/// counts them: the size, and the count it is multiplied by in `calloc`'s shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocSize {
    /// The argument that is the size in bytes, or of one element when there is a count.
    pub size: u8,
    /// The argument that is how many elements there are, if one is.
    pub count: Option<u8>,
}

/// The names that clang's `export_name`, `import_module` and `import_name` attributes give a
/// function on a wasm row, in place of the names a wasm object gives it by default.
///
/// A definition with an export name is exported from the module under that name. A declaration
/// with an import module or an import name is imported from that module or under that field, and
/// the other part stays `env` or the symbol name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WasmNames {
    pub export: Option<StrId>,
    pub module: Option<StrId>,
    pub field: Option<StrId>,
}

/// A label, in the label table.
pub type LabelId = Idx<Label>;

/// The value of a constant expression, after folding.
///
/// Integers are held in a hundred and twenty eight bits whatever their type, which covers every
/// integer type this compiler has including `__int128`. A `_BitInt(N)` wider than that is not
/// representable here and is refused where it is written rather than silently truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Const {
    /// An integer, sign extended into the whole width from the type it has.
    Int(i128),
    /// A floating value, in the target's format rather than the host's.
    Float(Float),
    /// A complex value, which is two floating ones in the format the real half has.
    ///
    /// This is what an imaginary constant folds to and what `1.0 + 2.0i` in a static initializer
    /// folds to, and it is a variant of its own rather than a pair of entries because a constant
    /// is one value and the object it initializes is one object.
    Complex {
        /// The real half.
        real: Float,
        /// The imaginary half.
        imag: Float,
    },
    /// A complex value whose halves are integers, which is `_Complex int` and the rest of gcc's
    /// complex integer types.
    ///
    /// A variant of its own rather than a pair of [`Const::Int`] for the same reason the
    /// floating one is, and separate from it because the two halves are held the way a half of
    /// that type is held: sign extended into a hundred and twenty eight bits, not in a floating
    /// format that would round every value wider than a `double` can hold exactly.
    ComplexInt {
        /// The real half, sign extended into the whole width from the type it has.
        real: i128,
        /// The imaginary half, held the same way.
        imag: i128,
    },
    /// The address of an object, which is a number nobody knows until the link.
    Address(Address),
    /// How far one label of a function is from another, `&&to - &&from` in GNU C.
    ///
    /// Neither address is known until the link, and yet the difference is known as soon as the
    /// function is laid out, since both ends are in its code and the code moves as one piece.
    /// That makes it a number the assembler writes rather than one a relocation asks the linker
    /// for, and it is how a table of places to jump to says where each one is in four bytes and
    /// without anything to relocate when the program is loaded.
    Apart {
        /// The label the distance is measured to.
        to: LabelId,
        /// The label it is measured from.
        from: LabelId,
    },
}

/// An address constant: some object, and how far into it.
///
/// This is what `&x`, `a + 1` and `&s.field` fold to, and it is the reason folding hands back
/// something richer than a number. The value is not known here and will not be known until the
/// linker places the object, so what a static initializer needs is not the value but the pair
/// that names it, which is what an object file's relocation records.
///
/// A pointer with no object behind it is not one of these. `(int *)4` folds to [`Const::Int`],
/// because four is the whole answer and nothing has to be relocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    /// The object the address is into.
    pub base: Base,
    /// How many bytes into it, which a member or a subscript adds to and which may be outside
    /// the object: `&a[10]` on an `int a[10]` is a valid address constant and is one past it.
    pub offset: i128,
}

/// What an address constant is an address of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    /// A declared object or function, which the linker knows by name.
    Decl(DeclId),
    /// A string literal, which has static storage duration and no name of its own.
    Str(StrId),
    /// A label of the function this was written in, whose address GNU C lets a program take.
    ///
    /// The one base that is a place inside a function rather than an object. What it names still
    /// has an address nobody knows until the link, so it belongs here for the same reason the two
    /// above do, and the only thing it cannot have is an offset that means anything: adding to the
    /// address of a label is arithmetic on a `void *`, which has no element to scale by, and
    /// landing in the middle of an instruction is not somewhere a jump may go.
    Label(LabelId),
    /// No object at all, which is an address the program wrote as a number.
    ///
    /// `*(int *)4` is a place, so `&*(int *)4` is an address, and the whole of it is in the
    /// offset because there is nothing for the linker to fill in. The one that matters is the
    /// offset from nothing: `((size_t) &((struct S *)0)->field)` is how every program that
    /// predates `__builtin_offsetof` spells `offsetof`, and tcc's own headers still spell it that
    /// way. Since there is no symbol, this is the one base whose address is a number this
    /// compiler knows, which is why converting one to an integer gives an integer rather than a
    /// relocation that happens to be written in an integer's place.
    Absolute,
}

/// A label, and the statement it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label {
    /// The name it was written with.
    pub name: Symbol,
    /// The statement it labels, absent for a label that was used and never defined, which is a
    /// diagnostic rather than a reason to lose the reference.
    pub stmt: Option<StmtId>,
}

/// One version of a function that `__attribute__((target_clones(...)))` asked to have built, which
/// is one of the names in the attribute's strings.
///
/// gcc builds the body once for each name, as a local function called the function's name, a dot
/// and the suffix, and makes the function's own name an indirect function whose resolver asks
/// libgcc what the processor has and picks the first version, in gcc's order, that it can run. The
/// versions are kept in that order with `default` last, so the resolver is written by walking the
/// list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// What goes after the dot in the version's name, which is the name as written with every
    /// `=`, `-` and `.` made an underscore: `avx2`, `sse4_2`, `arch_x86_64_v3` and `default`.
    pub suffix: String,
    /// The extensions the version is built for, which is the unit's set with what the name adds,
    /// and nothing for `default`, which is built for the unit.
    pub isa: Option<Isa>,
    /// Where libgcc keeps the bit saying the processor can run the version, as the object, the word
    /// of it and the bit in the word, which is what `__builtin_cpu_supports` of the same name
    /// reads. Nothing for `default`, which is what the resolver falls back to.
    pub test: Option<(CpuObject, u8, u8)>,
}

/// One typed translation unit.
#[derive(Default)]
pub struct Tast {
    exprs: Vec<Expr>,
    expr_spans: Vec<Span>,
    stmts: Vec<Stmt>,
    stmt_spans: Vec<Span>,
    decls: Vec<Decl>,
    decl_spans: Vec<Span>,

    consts: Vec<Const>,
    /// Where each integer constant already is, so that writing the same one again is one entry.
    /// A table of a few hundred thousand bytes is a few hundred thousand constants and a few
    /// hundred values.
    ints: Map<i128, ConstId>,
    strings: Vec<StringLiteral>,
    labels: Vec<Label>,
    vlas: Vec<ExprId>,
    adjusted: Vec<(DeclId, TypeId)>,
    spellings: Vec<(DeclId, Symbol, TypeId)>,
    targets: Vec<(DeclId, Isa)>,
    versions: Vec<(DeclId, Vec<Version>)>,
    sections: Map<DeclId, StrId>,
    function_names: Set<StrId>,
    alloc_sizes: Map<DeclId, AllocSize>,
    patchable: Map<DeclId, (u32, u32)>,
    wasm_names: Map<DeclId, WasmNames>,
    ifuncs: Set<DeclId>,
    fentry_names: Map<DeclId, StrId>,
    fentry_sections: Map<DeclId, StrId>,
    symvers: Map<DeclId, Vec<(String, Span)>>,
    defined_at: Map<DeclId, Span>,
    notices: Map<DeclId, Notices>,
    asms: Vec<Asm>,
    file_asms: Vec<FileAsm>,

    expr_refs: Vec<ExprId>,
    stmt_refs: Vec<StmtId>,
    decl_refs: Vec<DeclId>,
    str_refs: Vec<StrId>,
    label_refs: Vec<LabelId>,
    cases: Vec<Case>,
    init_entries: Vec<InitEntry>,
    asm_operands: Vec<AsmOperand>,

    top_level: Vec<DeclId>,
}

impl Tast {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Tast {
        Tast::default()
    }

    /// An empty tree with room for this many expressions, for the same reason as the untyped
    /// tree's `with_capacity`.
    #[must_use]
    pub fn with_capacity(exprs: usize) -> Tast {
        Tast {
            exprs: Vec::with_capacity(exprs),
            expr_spans: Vec::with_capacity(exprs),
            ..Tast::default()
        }
    }

    /// The objects and functions of the translation unit, in the order they were declared.
    #[must_use]
    pub fn top_level(&self) -> &[DeclId] {
        &self.top_level
    }

    /// Adds a declaration at file scope.
    pub fn add_top_level(&mut self, decl: DeclId) {
        self.top_level.push(decl);
    }

    /// The `asm` written at file scope, in the order they were written.
    ///
    /// Beside [`Tast::top_level`] rather than in it, because one of these declares no object and
    /// no function and so is not a [`Decl`]. What it is instead is a contribution to the object
    /// file, which is a thing only the walk to the IR has anywhere to put.
    #[must_use]
    pub fn file_asms(&self) -> &[FileAsm] {
        &self.file_asms
    }

    /// Adds an `asm` written at file scope.
    pub fn add_file_asm(&mut self, asm: FileAsm) {
        self.file_asms.push(asm);
    }

    /// Adds an expression, with the source it came from.
    ///
    /// # Panics
    ///
    /// Panics if the arena would exceed four billion nodes, which is not a translation unit
    /// this compiler intends to accept.
    pub fn expr(&mut self, expr: Expr, span: Span) -> ExprId {
        let id = Idx::from_usize(self.exprs.len());
        self.exprs.push(expr);
        self.expr_spans.push(span);
        id
    }

    /// Adds a statement, with the source it came from.
    ///
    /// # Panics
    ///
    /// Panics if the arena would exceed four billion nodes.
    pub fn stmt(&mut self, stmt: Stmt, span: Span) -> StmtId {
        let id = Idx::from_usize(self.stmts.len());
        self.stmts.push(stmt);
        self.stmt_spans.push(span);
        id
    }

    /// Adds a declaration, with the source it came from.
    ///
    /// # Panics
    ///
    /// Panics if the arena would exceed four billion nodes.
    pub fn decl(&mut self, decl: Decl, span: Span) -> DeclId {
        let id = Idx::from_usize(self.decls.len());
        self.decls.push(decl);
        self.decl_spans.push(span);
        id
    }

    /// Replaces a declaration, which is what a definition of something already declared does.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not a declaration of this tree.
    pub fn set_decl(&mut self, id: DeclId, decl: Decl) {
        self.decls[id.index()] = decl;
    }

    /// Replaces a statement, which is what a `switch` does to the cases in its body.
    ///
    /// A `case` is checked before the table it is an entry of exists, since the table is a run
    /// and the run is not known until the whole body has been walked. So the statement is written
    /// with a placeholder entry and given its real one here.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not a statement of this tree.
    pub fn set_stmt(&mut self, id: StmtId, stmt: Stmt) {
        self.stmts[id.index()] = stmt;
    }

    /// The source an expression came from.
    #[must_use]
    pub fn expr_span(&self, id: ExprId) -> Span {
        self.expr_spans[id.index()]
    }

    /// The source a statement came from.
    #[must_use]
    pub fn stmt_span(&self, id: StmtId) -> Span {
        self.stmt_spans[id.index()]
    }

    /// The source a declaration came from.
    #[must_use]
    pub fn decl_span(&self, id: DeclId) -> Span {
        self.decl_spans[id.index()]
    }

    /// Records the size of one variable length array, and gives back its identity.
    ///
    /// The type table keeps a [`VlaId`] and nothing else, because two variable length arrays
    /// written with the same element type are still distinct types and interning them together
    /// would say they are not. The expression itself lives here, since it is evaluated once
    /// where the declaration is reached and its value is what every `sizeof` of that type
    /// afterwards answers with.
    ///
    /// # Panics
    ///
    /// Panics if the table would exceed four billion entries.
    pub fn add_vla(&mut self, size: ExprId) -> VlaId {
        let id = u32::try_from(self.vlas.len()).expect("too many variable length arrays");
        self.vlas.push(size);
        VlaId(id)
    }

    /// The size expression of one variable length array.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not one of this tree's.
    #[must_use]
    pub fn vla_size(&self, id: VlaId) -> ExprId {
        self.vlas[id.0 as usize]
    }

    /// Records the type a parameter was written as, where adjusting it to a pointer dropped a
    /// length the program still has to evaluate.
    ///
    /// `int f(int a[i++])` declares a pointer, since C11 6.7.6.3p7 adjusts an array parameter to
    /// one, and the adjustment takes the type away and not the expression: the size is evaluated
    /// once on entry to the function, in the order the parameters were written, so `i++` happens
    /// and the function sees the incremented value. Nothing needs the size for anything, because
    /// the parameter is a pointer, so what is kept here is the type it was written as and the
    /// walk over that type is what evaluates every length in it.
    ///
    /// Only the outermost length is ever lost this way. `int a[][n]` adjusts to `int (*)[n]` and
    /// the `n` is still in the type the parameter has, which is why this is a handful of entries
    /// in the whole tree and not one per parameter.
    pub fn record_adjustment(&mut self, decl: DeclId, written: TypeId) {
        self.adjusted.push((decl, written));
    }

    /// The type a parameter was written as, for the few that have one.
    #[must_use]
    pub fn adjusted_from(&self, decl: DeclId) -> Option<TypeId> {
        self.adjusted.iter().find(|&&(at, _)| at == decl).map(|&(_, written)| written)
    }

    /// Records the typedef name a declaration named its type with.
    ///
    /// A typedef binds its name to the very type it stands for, so `size_type n;` declares an
    /// object whose type is `unsigned long` and nothing in the type says which name it was reached
    /// through. The debug information wants that name, because a debugger printing `n` in the
    /// program's own words says `size_type`, and this is the one place it is kept.
    ///
    /// `of` is the type the name stood for, and the declaration's type is built on top of it:
    /// `const size_type n;` and `size_type *p;` have the name as a part of their type rather than
    /// the whole of it, and the debug information finds the part by looking for `of` inside. A
    /// declaration written again, which a merge makes the same declaration, may come in twice,
    /// and the first is the one that counts.
    pub fn record_spelling(&mut self, decl: DeclId, name: Symbol, of: TypeId) {
        self.spellings.push((decl, name, of));
    }

    /// Every declaration that named its type with a typedef, the name it used and the type the
    /// name stood for.
    #[must_use]
    pub fn spellings(&self) -> &[(DeclId, Symbol, TypeId)] {
        &self.spellings
    }

    /// Records where the name is written in a function's definition, when the function was
    /// declared somewhere else first.
    ///
    /// [`Tast::decl_span`] is where the name was first declared, which is what a diagnostic about
    /// a conflicting declaration points back at. A report about the function as a whole points at
    /// the definition instead, the way gcc's does, and in a file that declares its `static`
    /// functions at the top and defines them further down the two are far apart.
    pub fn record_definition(&mut self, decl: DeclId, span: Span) {
        if span != self.decl_span(decl) {
            self.defined_at.insert(decl, span);
        }
    }

    /// Where the name is written in a function's definition, which is [`Tast::decl_span`] unless
    /// [`Tast::record_definition`] said otherwise.
    #[must_use]
    pub fn definition_span(&self, decl: DeclId) -> Span {
        self.defined_at.get(&decl).copied().unwrap_or_else(|| self.decl_span(decl))
    }

    /// Records the extensions `__attribute__((target(...)))` said a function is built for, which is
    /// the unit's set with what the attribute added.
    ///
    /// A handful of functions in a unit carry one, which is why this is a list beside the tree
    /// rather than a field on every declaration, where a set of a hundred and twenty eight bits
    /// would double the size of each. It is a fact about the name, and the first declaration to
    /// say something stands, the way [`Decl::visibility`] does: gcc refuses a later one that
    /// disagrees, and the usual program writes it once, on the definition.
    pub fn record_target(&mut self, decl: DeclId, isa: Isa) {
        if self.target(decl).is_none() {
            self.targets.push((decl, isa));
        }
    }

    /// Records the versions `__attribute__((target_clones(...)))` asked a function to be built in,
    /// in the order the resolver tries them, with `default` last.
    ///
    /// A list beside the tree for the reason [`Tast::record_target`] is one. Unlike the `target`
    /// attribute, the last declaration to say something stands, even over the definition's list,
    /// because that is what gcc 16 builds.
    pub fn record_versions(&mut self, decl: DeclId, versions: Vec<Version>) {
        match self.versions.iter_mut().find(|(at, _)| *at == decl) {
            Some(slot) => slot.1 = versions,
            None => self.versions.push((decl, versions)),
        }
    }

    /// The versions a `target_clones` attribute asked this function to be built in, and nothing
    /// for a function built once, which is almost every function.
    #[must_use]
    pub fn versions(&self, decl: DeclId) -> Option<&[Version]> {
        self.versions.iter().find(|(at, _)| *at == decl).map(|(_, versions)| &versions[..])
    }

    /// Records the section `__attribute__((section(...)))` put a function or an object in.
    ///
    /// A list beside the tree for the reason [`Tast::record_target`] is one: most declarations
    /// name no section, and the ones that do are a kernel's `__init` functions and its tables. The
    /// first declaration to name one stands, which is what gcc does with a later one that names
    /// another, and the caller is what warns about that.
    pub fn record_section(&mut self, decl: DeclId, section: StrId) {
        self.sections.entry(decl).or_insert(section);
    }

    /// Records that a string is what `__func__` stands for in some function rather than a literal
    /// the program wrote.
    ///
    /// The standard declares it as `static const char __func__[]`, an object of its own, and gcc
    /// gives it one: a local `__func__.N` in `.rodata`, never merged with a literal that has the
    /// same characters. The kernel's section lists see the difference.
    pub fn record_function_name(&mut self, id: StrId) {
        self.function_names.insert(id);
    }

    /// Whether a string is what `__func__` stands for. See [`Tast::record_function_name`].
    #[must_use]
    pub fn is_function_name(&self, id: StrId) -> bool {
        self.function_names.contains(&id)
    }

    /// Records which arguments `__attribute__((alloc_size(...)))` said are the size of what the
    /// function returns.
    ///
    /// A map beside the tree for the reason [`Tast::record_section`] is one: the allocators of a
    /// program and of its libraries carry it and nothing else does. A later declaration replaces
    /// what an earlier one said, which is what gcc does, since the attribute is read off the type
    /// of the declaration in sight at the call.
    pub fn record_alloc_size(&mut self, decl: DeclId, alloc: AllocSize) {
        self.alloc_sizes.insert(decl, alloc);
    }

    /// Records the room `__attribute__((patchable_function_entry(N, M)))` asked a function to open
    /// with, as the total and the part of it in front of the label, the way the attribute and
    /// `-fpatchable-function-entry=` write them.
    ///
    /// A map beside the tree for the reason [`Tast::record_alloc_size`] is one: the kernel's
    /// `notrace` is the usual writer. A later declaration replaces what an earlier one said, and
    /// the second of two on one declaration the first, which is what gcc does.
    pub fn record_patchable(&mut self, decl: DeclId, total: u32, before: u32) {
        self.patchable.insert(decl, (total, before));
    }

    /// The total and the part in front of the label a `patchable_function_entry` attribute asked
    /// this function for, and nothing when the command line decides.
    #[must_use]
    pub fn patchable(&self, decl: DeclId) -> Option<(u32, u32)> {
        self.patchable.get(&decl).copied()
    }

    /// Records the names a wasm row gives a function, from clang's `export_name`,
    /// `import_module` and `import_name`. A later declaration adds to what an earlier one said,
    /// and the second of two names of one kind replaces the first, which is what clang does.
    pub fn record_wasm_names(&mut self, decl: DeclId, said: WasmNames) {
        let names = self.wasm_names.entry(decl).or_default();
        names.export = said.export.or(names.export);
        names.module = said.module.or(names.module);
        names.field = said.field.or(names.field);
    }

    /// The names a wasm row gives this function in place of its own, which are all `None` when
    /// no attribute said otherwise.
    #[must_use]
    pub fn wasm_names(&self, decl: DeclId) -> WasmNames {
        self.wasm_names.get(&decl).copied().unwrap_or_default()
    }

    /// Records the profiler's hook `__attribute__((fentry_name("hook")))` asked `-pg` to call on
    /// the way into this function, in place of `__fentry__` or `mcount`. A later declaration
    /// replaces what an earlier one said, the way gcc reads the attributes once the file is done.
    pub fn record_fentry_name(&mut self, decl: DeclId, name: StrId) {
        self.fentry_names.insert(decl, name);
    }

    /// The hook this function asked the profiler's call to go to, and nothing when the command
    /// line decides.
    #[must_use]
    pub fn fentry_name(&self, decl: DeclId) -> Option<StrId> {
        self.fentry_names.get(&decl).copied()
    }

    /// Records the section `__attribute__((fentry_section("name")))` asked the address of the
    /// profiler's call to be listed in, the same way [`Tast::record_fentry_name`] records the hook.
    pub fn record_fentry_section(&mut self, decl: DeclId, section: StrId) {
        self.fentry_sections.insert(decl, section);
    }

    /// The section this function asked the profiler's call to be listed in, and nothing when the
    /// command line decides.
    #[must_use]
    pub fn fentry_section(&self, decl: DeclId) -> Option<StrId> {
        self.fentry_sections.get(&decl).copied()
    }

    /// Records the versioned names `__attribute__((symver("name@node")))` asked for on one
    /// declaration of this function or object, with where that declaration is. A name an earlier
    /// declaration already asked for is the same request again and is not recorded twice, which
    /// is how gcc merges the attributes of two declarations. Two of them on one declaration are
    /// both kept, so that the second is reported as the duplicate gcc reports.
    pub fn record_symvers(&mut self, decl: DeclId, names: Vec<String>, span: Span) {
        let recorded = self.symvers.entry(decl).or_default();
        let earlier = recorded.len();
        for name in names {
            if !recorded[..earlier].iter().any(|(already, _)| *already == name) {
                recorded.push((name, span));
            }
        }
    }

    /// Every declaration that asked for a versioned name, in the order they were declared, with
    /// the names and where each was asked for.
    #[must_use]
    pub fn symvers(&self) -> Vec<(DeclId, Vec<(String, Span)>)> {
        let mut symvers: Vec<_> =
            self.symvers.iter().map(|(&decl, names)| (decl, names.clone())).collect();
        symvers.sort_by_key(|(decl, _)| decl.index());
        symvers
    }

    /// Records that the second name this function's `alias` is for is its resolver, which
    /// `__attribute__((ifunc("resolver")))` says: the symbol is an indirect function, and what
    /// the dynamic linker binds a call to is whatever the resolver hands back.
    ///
    /// A set beside the tree for the reason [`Tast::record_alloc_size`] is a map: a few libraries'
    /// string functions are the writers, and the declaration's own `alias` already holds the name.
    pub fn record_ifunc(&mut self, decl: DeclId) {
        self.ifuncs.insert(decl);
    }

    /// Whether this declaration's `alias` names a resolver rather than a definition.
    #[must_use]
    pub fn is_ifunc(&self, decl: DeclId) -> bool {
        self.ifuncs.contains(&decl)
    }

    /// Every declaration whose `alias` names a resolver, in no particular order.
    #[must_use]
    pub fn ifuncs(&self) -> Vec<DeclId> {
        self.ifuncs.iter().copied().collect()
    }

    /// The arguments an `alloc_size` attribute named on this function, and nothing for almost
    /// every function.
    #[must_use]
    pub fn alloc_size(&self, decl: DeclId) -> Option<AllocSize> {
        self.alloc_sizes.get(&decl).copied()
    }

    /// The section a `section` attribute put this declaration in, and nothing when no declaration
    /// of it named one.
    #[must_use]
    pub fn section(&self, decl: DeclId) -> Option<StrId> {
        self.sections.get(&decl).copied()
    }

    /// Records the message an `error` or a `warning` attribute asks to have said about a call to a
    /// function that is still there once the optimizer is done with it.
    ///
    /// A list beside the tree for the reason [`Tast::record_target`] is one: a handful of functions
    /// in a unit carry one, and they are the kernel's `__compiletime_assert_N` and the fortify
    /// checks. Unlike a target, a later declaration replaces what an earlier one said, which is
    /// what gcc does: the message a call is reported with is the last one written above it.
    pub fn record_notice(&mut self, decl: DeclId, error: bool, message: StrId) {
        let notices = self.notices.entry(decl).or_default();
        if error {
            notices.error = Some(message);
        } else {
            notices.warning = Some(message);
        }
    }

    /// What an `error` or a `warning` attribute asks to have said about a call to this function,
    /// which is nothing for almost every function.
    #[must_use]
    pub fn notices(&self, decl: DeclId) -> Notices {
        self.notices.get(&decl).copied().unwrap_or_default()
    }

    /// The extensions a function was built for, when a `target` attribute said, and nothing when
    /// it is built for what the unit is.
    #[must_use]
    pub fn target(&self, decl: DeclId) -> Option<Isa> {
        self.targets.iter().find(|&&(at, _)| at == decl).map(|&(_, isa)| isa)
    }

    /// Records that a label names a statement, which is not known when the label is created
    /// because a `goto` may come first.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not a label of this tree.
    pub fn define_label(&mut self, id: LabelId, stmt: StmtId) {
        self.labels[id.index()].stmt = Some(stmt);
    }

    /// How many expressions, statements and declarations the tree holds.
    #[must_use]
    pub fn counts(&self) -> Counts {
        Counts { exprs: self.exprs.len(), stmts: self.stmts.len(), decls: self.decls.len() }
    }

    /// Whether nothing has been checked into this tree.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exprs.is_empty() && self.stmts.is_empty() && self.decls.is_empty()
    }
}

/// How many nodes of each kind a typed tree holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Expressions.
    pub exprs: usize,
    /// Statements.
    pub stmts: usize,
    /// Declarations.
    pub decls: usize,
}

impl fmt::Debug for Tast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The same reasoning as the untyped tree: nobody wants a translation unit as a `{:?}`,
        // and the thing they did want has a printer.
        let counts = self.counts();
        f.debug_struct("Tast")
            .field("exprs", &counts.exprs)
            .field("stmts", &counts.stmts)
            .field("decls", &counts.decls)
            .field("top_level", &self.top_level.len())
            .finish()
    }
}

/// Generates the read side of a table that holds one item per index.
macro_rules! node_table {
    ($id:ty => $item:ty, $field:ident) => {
        impl Index<$id> for Tast {
            type Output = $item;

            #[inline]
            fn index(&self, id: $id) -> &$item {
                &self.$field[id.index()]
            }
        }
    };
}

/// Generates both sides of a side table whose items are added one at a time.
macro_rules! side_table {
    (
        $(#[$doc:meta])*
        $add:ident, $id:ty => $item:ty, $field:ident
    ) => {
        impl Tast {
            $(#[$doc])*
            ///
            /// # Panics
            ///
            /// Panics if the table would exceed four billion entries.
            pub fn $add(&mut self, item: $item) -> $id {
                let id = Idx::from_usize(self.$field.len());
                self.$field.push(item);
                id
            }
        }

        node_table!($id => $item, $field);
    };
}

/// Generates both sides of a table that is read in runs.
macro_rules! list_table {
    (
        $(#[$doc:meta])*
        $add:ident, $list:ty => $item:ty, $field:ident
    ) => {
        impl Tast {
            $(#[$doc])*
            ///
            /// # Panics
            ///
            /// Panics if the table would exceed four billion entries.
            pub fn $add(&mut self, items: &[$item]) -> $list {
                let start = Idx::from_usize(self.$field.len());
                self.$field.extend_from_slice(items);
                let end = Idx::from_usize(self.$field.len());
                IdxRange::new(start, end)
            }
        }

        impl Index<$list> for Tast {
            type Output = [$item];

            #[inline]
            fn index(&self, list: $list) -> &[$item] {
                &self.$field[list.as_usize_range()]
            }
        }
    };
}

node_table!(ExprId => Expr, exprs);
node_table!(StmtId => Stmt, stmts);
node_table!(DeclId => Decl, decls);
node_table!(CaseId => Case, cases);

impl Tast {
    /// Adds a folded constant.
    ///
    /// An integer is entered once and shared after that, since nothing writes to a constant
    /// once it is in and two readers of the same value cannot tell they share it.
    ///
    /// # Panics
    ///
    /// Panics if the table would exceed four billion entries.
    pub fn add_const(&mut self, item: Const) -> ConstId {
        if let Const::Int(value) = item {
            if let Some(&id) = self.ints.get(&value) {
                return id;
            }
            let id = Idx::from_usize(self.consts.len());
            self.consts.push(item);
            self.ints.insert(value, id);
            return id;
        }
        let id = Idx::from_usize(self.consts.len());
        self.consts.push(item);
        id
    }
}

node_table!(ConstId => Const, consts);
side_table! {
    /// Adds a string literal.
    add_string, StrId => StringLiteral, strings
}
side_table! {
    /// Adds a label, which is not defined until the statement it names has been seen.
    add_label, LabelId => Label, labels
}
side_table! {
    /// Adds an assembly statement.
    add_asm, AsmId => Asm, asms
}

list_table! {
    /// Adds a run of expression references, which is what a call's arguments are.
    add_expr_refs, ExprList => ExprId, expr_refs
}
list_table! {
    /// Adds a run of statement references, which is what a block is.
    add_stmt_refs, StmtList => StmtId, stmt_refs
}
list_table! {
    /// Adds a run of declaration references, which is what a declaration statement is.
    add_decl_refs, DeclList => DeclId, decl_refs
}
list_table! {
    /// Adds a run of string literal references, which is what an `asm` clobber list is.
    add_str_refs, StrList => StrId, str_refs
}
list_table! {
    /// Adds a run of label references, which is what the labels of an `asm goto` are.
    add_label_refs, LabelList => LabelId, label_refs
}
list_table! {
    /// Adds the operands of one section of an `asm` statement.
    add_asm_operands, AsmOperandList => AsmOperand, asm_operands
}
list_table! {
    /// Adds the cases of one `switch`, in the order a jump table wants them.
    add_cases, crate::stmt::CaseList => Case, cases
}
list_table! {
    /// Adds the values one initializer stores.
    add_init_entries, crate::decl::InitList => InitEntry, init_entries
}

#[cfg(test)]
mod tests {
    use rucc_ast::BinaryOp;
    use rucc_types::{IntKind, Types};

    use super::*;
    use crate::decl::{
        DeclFlags, DeclKind, Definition, Effects, Emission, Linkage, Startup, StorageDuration,
    };
    use crate::expr::{Category, Conversion, ExprKind};

    /// The sizes are asserted rather than left to whoever adds the next variant.
    ///
    /// A node that grows costs the whole arena, and the day one does is a day somebody should
    /// have to say so out loud rather than a day the walk over a large translation unit gets
    /// slower for no reason anybody can point at.
    ///
    /// A case is the outlier at forty eight bytes, because two `i128` bounds want sixteen byte
    /// alignment and nothing smaller holds a `switch` over `__int128`. It buys its size back by
    /// being rare: one entry per `case` rather than one per node.
    ///
    /// A declaration went from thirty six bytes to forty four when it was given the parameter
    /// list of a function definition, which is a field only a definition fills in and every
    /// declaration pays for. The alternative was a side table keyed by declaration, and it was
    /// not taken: a lookup per function in a table that is empty for almost every entry is
    /// worse than eight bytes on a node there are far fewer of than there are expressions.
    ///
    /// It went from forty four to forty eight when `constexpr` made a declaration a named
    /// constant. The four bytes are padding rather than the flag: the four one byte fields
    /// already filled a word exactly, so the first bit added costs the whole next one. The same
    /// reasoning as above applies, with the numbers even further apart, since a translation
    /// unit has a handful of named constants and hundreds of thousands of expressions.
    ///
    /// It went from forty eight to fifty two when a declaration was given the assembler name it
    /// renames the symbol to. That one is a whole four byte index rather than a bit, and it goes
    /// on the node for the reason the parameter list does: the name a symbol is emitted under is
    /// asked for once per definition and once per reference to one, and a side table would be a
    /// lookup on every one of those to find nothing almost every time.
    ///
    /// Fifty two to fifty six for the symbol an `alias` makes the name a second spelling of, which
    /// is the same kind of index and is here for a weaker reason: it is asked for once per
    /// declaration and almost none of them have one. It sits beside the assembler name because the
    /// two are the same question asked from opposite ends, and a side table for one of them would
    /// be a table nothing else in the tree has a use for.
    ///
    /// Fifty six to sixty for whether control comes back from a call to the function. It is one
    /// bit and it costs four bytes for the reason `constexpr` cost four: the one byte fields
    /// filled two words exactly, so the first bit past them takes the whole of the next one. The
    /// alternative here is not a side table, it is folding the five booleans on this node into a
    /// bitset, which would give back these four bytes and the four `constexpr` took. That is worth
    /// doing when there is a sixth, and it is not worth doing for the fifth: each of the five says
    /// a different thing about a declaration and each carries a paragraph saying which, and a
    /// bitset takes the paragraphs off the fields and puts them on a table of constants.
    ///
    /// Sixty to sixty eight for where a function goes in the two orders `constructor` and
    /// `destructor` ask for. Eight bytes and none of them padding: each order is a priority that
    /// may be absent, may be written bare, or may be a number up to sixty five thousand five
    /// hundred and thirty five, which is three kinds of answer and does not fit in the two bytes
    /// the number itself takes. A function may be in both orders and the two numbers have nothing
    /// to do with each other, so it is two of those and not one. This is the field with the
    /// weakest claim to a place on the node, since hardly any declaration in any program carries
    /// either attribute, and it is here because it is merged the way the visibility and the
    /// assembler name above it are merged: a header writes the attribute and the definition below
    /// it writes nothing, so the answer has to travel with the declarations of a name rather than
    /// with the one that was written.
    ///
    /// Sixty eight to seventy two for the handler a `cleanup` attribute names, which is four
    /// bytes and none of them padding: it is one index and it lands where the fields of that
    /// width already are. Unlike every field above it this one is a fact about the declaration
    /// rather than about the name, since the attribute is only allowed on an object inside a
    /// block and such an object is declared once, so there is nothing for the merge to carry. A
    /// side table was the alternative and was not taken for the reason the assembler name above
    /// did not take one: it is asked about at every declaration the lowering walks past, which
    /// is every local in the program, and a table that is empty for all but a handful of them is
    /// a lookup per local to find nothing.
    ///
    /// Seventy two back down to sixty eight when `naked` was the sixth boolean and the six of them
    /// became one byte of [`DeclFlags`](crate::decl::DeclFlags). That is the fold the paragraph
    /// about `noreturn` above said was worth doing once there was a sixth, and doing it took a
    /// declaration below the size it was before rather than four bytes above it. What is left is
    /// eight one byte fields filling two words exactly, so the seventh yes or no question about a
    /// declaration and several after it now cost nothing at all, which is most of what an attribute
    /// is and is the reason the fold was worth more than the four bytes it gave back.
    ///
    /// Sixty eight to seventy two again for the machine register a local is kept in, which is four
    /// bytes and none of them padding for the reason the handler above is: it is one index and it
    /// lands where the fields of that width already are. It is a fact about the declaration rather
    /// than about the name, the same as the handler, and for the same reason: the string after
    /// `asm` is read as a register only on an object with automatic storage and such an object is
    /// declared once. A side table was not taken either, and this time the argument is the
    /// stronger of the two: the lowering asks about it at every local in the program.
    ///
    /// Seventy two to seventy six when [`DeclFlags`](crate::decl::DeclFlags) went from eight bits
    /// to sixteen for `optimize ("no-strict-aliasing")`, the ninth yes or no question. The one
    /// byte fields filled their four bytes exactly, so the ninth byte costs four. A side table was
    /// the alternative, and it was not taken because the answer has to follow the name through
    /// the merge of its declarations the way `always_inline` does, which is what the bits already
    /// do. The seven bits past it are free again.
    ///
    /// Still seventy six when they went from sixteen bits to thirty two for `no_stack_protector`,
    /// the seventeenth, because the two bytes the wider field took were padding already.
    ///
    /// Eighty when they went from thirty two bits to sixty four for `optimize ("wrapv")`, the
    /// thirty third. The field wants eight byte alignment now, so the node does too, and four
    /// bytes of the growth are padding at the end. The thirty bits past the two it took are free.
    #[test]
    fn the_nodes_are_the_size_they_are_meant_to_be() {
        assert_eq!(size_of::<Expr>(), 24);
        assert_eq!(size_of::<Stmt>(), 24);
        assert_eq!(size_of::<Decl>(), 80);
        assert_eq!(size_of::<Case>(), 48);
    }

    #[test]
    fn a_tree_hands_back_what_was_put_into_it() {
        let types = Types::new();
        let int = types.int(IntKind::Int);
        let mut tast = Tast::new();

        let one = tast.add_const(Const::Int(1));
        let left = tast.expr(Expr::new(ExprKind::Const(one), int, Category::Rvalue), Span::DUMMY);
        let right = tast.expr(Expr::new(ExprKind::Const(one), int, Category::Rvalue), Span::DUMMY);
        let sum = Expr::new(
            ExprKind::Binary { op: BinaryOp::Add, lhs: left, rhs: right },
            int,
            Category::Rvalue,
        );
        let sum = tast.expr(sum, Span::new(0, 5));

        assert_eq!(tast[left].ty, int);
        assert_eq!(tast[sum].category, Category::Rvalue);
        assert_eq!(tast.expr_span(sum), Span::new(0, 5));
        assert_eq!(tast.counts().exprs, 3);
        assert_eq!(tast[one], Const::Int(1));
    }

    #[test]
    fn a_conversion_is_a_node_and_not_a_difference_between_two_types() {
        let types = Types::new();
        let char_type = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let mut tast = Tast::new();

        let object = tast.decl(
            Decl {
                name: None,
                ty: char_type,
                kind: DeclKind::Object,
                linkage: Linkage::None,
                duration: StorageDuration::Automatic,
                state: Definition::Defined,
                alignment: None,
                flags: DeclFlags::NONE,
                asm_label: None,
                register: None,
                alias: None,
                inline: Emission::Silent,
                effects: Effects::Any,
                visibility: None,
                startup: Startup::default(),
                init: None,
                cleanup: None,
                params: DeclList::EMPTY,
                body: None,
            },
            Span::DUMMY,
        );
        let name =
            tast.expr(Expr::new(ExprKind::Decl(object), char_type, Category::Lvalue), Span::DUMMY);
        let read = tast.expr(
            Expr::new(
                ExprKind::Convert { kind: Conversion::Lvalue, operand: name },
                char_type,
                Category::Rvalue,
            ),
            Span::DUMMY,
        );
        let promoted = tast.expr(
            Expr::new(
                ExprKind::Convert { kind: Conversion::Arithmetic, operand: read },
                int,
                Category::Rvalue,
            ),
            Span::DUMMY,
        );

        // Nothing downstream has to work out that a `char` met an `int` somewhere: the two
        // steps that got it there are in the tree, in the order they happened.
        assert_eq!(tast[promoted].ty, int);
        let ExprKind::Convert { kind, operand } = tast[promoted].kind else { panic!("a convert") };
        assert_eq!(kind, Conversion::Arithmetic);
        assert_eq!(tast[operand].ty, char_type);
    }

    #[test]
    fn a_run_comes_back_as_a_slice() {
        let types = Types::new();
        let int = types.int(IntKind::Int);
        let mut tast = Tast::new();

        let zero = tast.add_const(Const::Int(0));
        let args: Vec<ExprId> = (0..3)
            .map(|_| {
                tast.expr(Expr::new(ExprKind::Const(zero), int, Category::Rvalue), Span::DUMMY)
            })
            .collect();
        let list = tast.add_expr_refs(&args);

        assert_eq!(&tast[list], args.as_slice());
    }

    #[test]
    fn a_label_is_made_before_it_is_defined_because_a_goto_may_come_first() {
        let mut tast = Tast::new();
        let mut names = rucc_base::Interner::new();
        let name = names.intern("done");

        let label = tast.add_label(Label { name, stmt: None });
        let jump = tast.stmt(Stmt::Goto(label), Span::DUMMY);
        let target = tast.stmt(Stmt::Empty, Span::DUMMY);
        tast.define_label(label, target);

        assert_eq!(tast[jump], Stmt::Goto(label));
        assert_eq!(tast[label].stmt, Some(target));
    }
}
