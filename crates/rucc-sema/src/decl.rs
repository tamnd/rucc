//! Declared objects and functions, with their linkage and their storage duration resolved.
//!
//! Design: `spec/07-types-and-semantics.md` sections 7.4 and 7.14.
//!
//! Only the things that exist at run time are here. A `typedef` is a name for a type and lives
//! in the type table as sugar, an enumerator is a constant and has been folded into the
//! expressions that used it, and a tag is a type. What is left is objects and functions, which
//! are what the walk to the IR needs a list of.
//!
//! An initializer is flattened. Brace elision, designators and the order the program wrote
//! things in are all resolved here into a list of values and the byte offsets they go at, so
//! that nothing downstream walks a nest of braces against a nest of types a second time. The
//! contract is that the object starts as zero and the entries are applied in order, which is
//! also what makes partial initialization and an overwriting designator fall out rather than
//! need rules of their own.

use rucc_base::{Idx, IdxRange, Symbol};
use rucc_types::TypeId;

use crate::expr::ExprId;
use crate::stmt::StmtId;

/// One declared object or function in the arena.
pub type DeclId = Idx<Decl>;

/// The table of references to declarations, which is what a declaration statement is a run of.
#[derive(Debug)]
pub struct DeclRef;

/// A run of declarations.
pub type DeclList = IdxRange<DeclRef>;

/// A run of the values one initializer stores.
pub type InitList = IdxRange<InitEntry>;

/// An object or a function, as it was declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl {
    /// The name, absent for a compound literal and for a parameter that was not given one.
    pub name: Option<Symbol>,
    /// The type, after the adjustments a declaration performs: an array parameter has already
    /// become a pointer, and a function parameter a function pointer.
    pub ty: TypeId,
    /// Whether it is an object or a function.
    pub kind: DeclKind,
    /// Whether the name is shared with other translation units, and how.
    pub linkage: Linkage,
    /// How long the object lives.
    pub duration: StorageDuration,
    /// How much of a definition this declaration is.
    pub state: Definition,
    /// The alignment `alignas` asked for, absent when the type's own alignment stands.
    pub alignment: Option<u32>,
    /// Whether `constexpr` was written, which makes the object a named constant.
    ///
    /// C23 6.6p8 puts a named constant of an integer type among the things an integer constant
    /// expression may be built out of, and a member of one of a structure or union type with
    /// it. That is the whole reason the keyword exists and it is why this is a fact about the
    /// declaration rather than something a reader could work out: a `const` object with a
    /// constant initializer is not one of them, so `const int n = 1; int a[n];` is a variable
    /// length array and the same two lines with `constexpr` are an array of one.
    pub constant: bool,
    /// Whether an attribute asks for this to exist where nothing in the file refers to it.
    ///
    /// `used`, `retain`, `constructor`, `destructor` and `alias` each say that something reaches
    /// the definition from where the compiler cannot see it, which is the only reason a program
    /// ever writes one of them. Nothing else in the tree says that, and a `static` function
    /// nothing refers to is not emitted, so this is how a program keeps one that has to be.
    pub retained: bool,
    /// The initializer, flattened, absent when there was none. An empty list is `= {}`, which
    /// C23 added and which zero-initializes, and is not the same as no initializer at all.
    pub init: Option<InitList>,
    /// The parameters of a function definition, in order, and empty for everything else.
    ///
    /// A parameter is an object with automatic storage like any other, and the body refers to
    /// one the same way it refers to a local. What is different is that nothing in the body
    /// declares it, so without this there is no way to ask which objects a definition takes and
    /// in what order, which is the first question the walk to the IR has: the entry block's
    /// parameters are these, in this order.
    ///
    /// A declaration that is not a definition has none of these even when it was written with a
    /// prototype, because `int f(int a);` declares no object called `a`. The types are in the
    /// function type, which is where a call reads them.
    pub params: DeclList,
    /// The body of a function definition.
    pub body: Option<StmtId>,
}

/// Whether a declaration declares an object or a function.
///
/// A `typedef` and an enumerator are neither: one is a name for a type and the other is a
/// constant, and both have been resolved by the time anything reads this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    /// An object, which includes parameters, block-scope variables and compound literals.
    Object,
    /// A function.
    Function,
}

/// Whether a name is shared with other translation units, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linkage {
    /// The name is not shared. Block-scope objects without `extern`, parameters, and anything
    /// declared in a function's body except a function or an `extern` object.
    None,
    /// The name is shared within the translation unit and not outside it, which is what
    /// `static` at file scope means.
    Internal,
    /// The name is shared with every translation unit that declares it.
    External,
}

/// How long an object lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageDuration {
    /// From the start of the program to the end of it.
    Static,
    /// From the start of the thread to the end of it, which is `_Thread_local`.
    Thread,
    /// From the point the declaration is reached to the end of the block, which is where a
    /// variable length array's deallocation and a compound literal's lifetime both come from.
    Automatic,
}

/// How much of a definition a declaration is.
///
/// The three states are what the one-definition rules are written in terms of, and keeping
/// them apart is what makes a tentative definition become a definition at the end of the
/// translation unit rather than at the point it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Definition {
    /// A declaration and nothing more, which is what `extern int x;` is and what every
    /// function declaration without a body is.
    Declared,
    /// A file-scope object with no initializer and no `extern`, which is a definition only if
    /// nothing else in the translation unit defines it. C calls this a tentative definition and
    /// it is the reason `int x; int x;` is one object and not an error.
    Tentative,
    /// A definition: an object with an initializer, a block-scope object with automatic
    /// storage, or a function with a body.
    Defined,
}

/// One value an initializer stores, and where it goes.
///
/// The offsets are from the start of the object being initialized, so a nested aggregate has
/// already been walked and there is nothing left to elide or designate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitEntry {
    /// The byte offset from the start of the object.
    pub offset: u64,
    /// The value, already converted to the type of what is at that offset.
    pub value: ExprId,
    /// The bit offset within the byte at `offset`, for a bit-field.
    pub bit_offset: u32,
    /// The width in bits, for a bit-field, and zero for everything else. A bit-field of width
    /// zero has no name and cannot be initialized, so zero is free to mean this instead.
    pub bit_width: u32,
}

impl InitEntry {
    /// A value at a byte offset, which is what everything that is not a bit-field is.
    #[must_use]
    pub const fn at(offset: u64, value: ExprId) -> InitEntry {
        InitEntry { offset, value, bit_offset: 0, bit_width: 0 }
    }

    /// Whether this entry writes part of a byte rather than whole bytes.
    #[must_use]
    pub const fn is_bit_field(&self) -> bool {
        self.bit_width != 0
    }
}
