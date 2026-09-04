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
use rucc_base::{Idx, IdxRange, Symbol};
use rucc_diag::Span;
use rucc_lex::StringLiteral;
use rucc_types::VlaId;

use crate::asm::{Asm, AsmId, AsmOperand, AsmOperandList, LabelList, StrList};
use crate::decl::{Decl, DeclId, DeclList, InitEntry};
use crate::expr::{Expr, ExprId, ExprList};
use crate::stmt::{Case, CaseId, Stmt, StmtId, StmtList};

/// A folded constant, in the value table.
pub type ConstId = Idx<Const>;

/// A string literal, in the literal table.
pub type StrId = Idx<StringLiteral>;

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
    /// The address of an object, which is a number nobody knows until the link.
    Address(Address),
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
    strings: Vec<StringLiteral>,
    labels: Vec<Label>,
    vlas: Vec<ExprId>,
    asms: Vec<Asm>,

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

    /// The objects and functions of the translation unit, in the order they were declared.
    #[must_use]
    pub fn top_level(&self) -> &[DeclId] {
        &self.top_level
    }

    /// Adds a declaration at file scope.
    pub fn add_top_level(&mut self, decl: DeclId) {
        self.top_level.push(decl);
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

side_table! {
    /// Adds a folded constant.
    add_const, ConstId => Const, consts
}
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
    use crate::decl::{DeclKind, Definition, Linkage, StorageDuration};
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
    #[test]
    fn the_nodes_are_the_size_they_are_meant_to_be() {
        assert_eq!(size_of::<Expr>(), 24);
        assert_eq!(size_of::<Stmt>(), 24);
        assert_eq!(size_of::<Decl>(), 48);
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
                constant: false,
                retained: false,
                init: None,
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
