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
use crate::tast::StrId;

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
    /// The yes or no answers about this declaration, one bit each. See [`DeclFlags`].
    pub flags: DeclFlags,
    /// The symbol this name stands for in the object file, when a declaration of it wrote an
    /// assembler name of its own.
    ///
    /// `extern int f (int) __asm__ ("g");` says that `f` here is the symbol `g`, which is how
    /// the C library redirects a name: `open` under `_FILE_OFFSET_BITS=64` is declared this way
    /// and reaches `open64`, and every `_FORTIFY_SOURCE` wrapper is the same trick. It is a fact
    /// about the name rather than about one declaration of it, so it is kept where the
    /// declarations of a name are merged, and the first one written is the one that stands.
    pub asm_label: Option<StrId>,
    /// The machine register this object lives in, when `register long x asm ("rbx");` said so.
    ///
    /// The other reading of the syntax above, and a separate field because it is a separate
    /// thing: an object of automatic storage has no symbol, so the string after `asm` on one
    /// cannot be a name the linker sees, and GNU C reads it as the name of a register instead.
    /// The object then has no slot in the frame and what it holds to begin with is whatever the
    /// register holds where the declaration stands.
    ///
    /// A garbage collector written in C is what writes one. A root that lives only in a callee
    /// saved register is a root no walk of the stack finds, so micropython declares six of these
    /// and copies them into a buffer it can walk.
    ///
    /// The name as the program wrote it, with the `%` gcc allows in front of it left on, because
    /// which register a name means is the target's question and this crate has no targets in it.
    /// It is a fact about this declaration rather than about the name, like
    /// [`Self::cleanup`] and for the same reason: the syntax is only read this way on an object
    /// with automatic storage, and such an object is declared once.
    pub register: Option<StrId>,
    /// The symbol this name is a second spelling of, when `__attribute__((alias("target")))` was
    /// written on a declaration of it.
    ///
    /// A declaration with one of these defines the name rather than declaring it: nothing is
    /// emitted for the declaration itself and the object file gets a second symbol pointing at
    /// whatever the string names. `extern int b __attribute__((alias("a")));` is how a program
    /// gives `a` the name `b`, and `weak, alias` beside it is the form glibc writes so that a
    /// program may define the name itself instead.
    ///
    /// The string is the symbol the linker sees rather than an identifier this resolves, which
    /// is why it is a [`StrId`] and not a [`Symbol`](rucc_base::Symbol). Whether anything
    /// defines it is settled where the whole translation unit is known.
    pub alias: Option<StrId>,
    /// Whether a definition of this name here is emitted, which `inline` is the only thing that
    /// changes.
    ///
    /// C 6.7.4p7: where every file-scope declaration of a function writes `inline` and none of
    /// them writes `extern`, the definition in this unit is an inline definition, no external
    /// definition is emitted for it, and a call goes to the definition some other unit holds.
    /// One declaration without `inline`, or one with `extern`, makes the whole thing an external
    /// definition again, which is why this is a fact about the name and is settled where the
    /// declarations of a name are merged.
    ///
    /// The two readings of `inline` swap over under [`DeclFlags::GNU_INLINE`], where it is the
    /// definition alone that decides and `extern inline` is the one that is not emitted.
    pub inline: Emission,
    /// The initializer, flattened, absent when there was none. An empty list is `= {}`, which
    /// C23 added and which zero-initializes, and is not the same as no initializer at all.
    pub init: Option<InitList>,
    /// What a declaration of this name promised a call to it does, and [`Effects::Any`] where
    /// none of them said.
    ///
    /// `__attribute__((const))` and `__attribute__((pure))`. A fact about the name rather than
    /// about one declaration of it, merged the way [`DeclFlags::NORETURN`] is and for the same
    /// reason: the place either attribute is written is a header, and the file that defines the
    /// function writes an ordinary definition.
    pub effects: Effects,
    /// Whether the function runs without anything calling it, which `constructor` and `destructor`
    /// ask for.
    ///
    /// A fact about the name rather than about one declaration of it, like [`Self::visibility`],
    /// and merged the way that one is: the usual place to write either attribute is the
    /// declaration in a header and the definition below it writes nothing, so the first
    /// declaration to ask for a place in the order is the one that gets it.
    pub startup: Startup,
    /// How far outside a shared library the name reaches, when a declaration of it said, and
    /// nothing when none did.
    ///
    /// `__attribute__((visibility("hidden")))` and the other three strings it takes. What is kept
    /// here is only what was written, because the other way a name gets a visibility is
    /// `-fvisibility=` and that is a fact about the compilation rather than about the declaration.
    /// The two meet where the IR is built, which is also the only place that has both.
    ///
    /// A fact about the name rather than about one declaration of it, like [`Self::asm_label`],
    /// and merged the way that one is: the first declaration to say something stands. gcc warns
    /// and keeps the first when a later one disagrees, since the calls above it have already been
    /// compiled against the answer it gave.
    pub visibility: Option<Visibility>,
    /// The function `__attribute__((cleanup(f)))` named, which runs on every way out of the block
    /// the object was declared in, and nothing when the attribute was not written.
    ///
    /// The handler takes a pointer to the object and is called with its address, in reverse order
    /// of declaration among the objects of one block, at the closing brace and at every `return`,
    /// `break`, `continue` and `goto` that leaves the block. It is a fact about this declaration
    /// rather than about the name, unlike most of the fields above, because the attribute is only
    /// allowed on an object with automatic storage and such an object is declared once.
    ///
    /// This is what glib spells `g_autoptr` and systemd spells `_cleanup_free_`, and what
    /// jansson spells `json_auto_t`. A compiler that reads past it leaks whatever the handler
    /// would have given back, which is the shape of wrongness hardest to notice.
    pub cleanup: Option<DeclId>,
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

/// The yes or no answers about a declaration, one bit each.
///
/// These were six `bool` fields until the sixth arrived. Each one byte field on a declaration is
/// nearly free until the byte that spills past a word, and then it costs four, which happened once
/// for `constexpr` and again for `noreturn` and would have happened a third time for `naked`. A
/// byte with six bits in it takes the node below the size it was before all three, and leaves
/// enough room that the next several questions of this kind cost nothing at all. The ninth took
/// it to sixteen bits, which is the story in `tast.rs` beside the size of a declaration.
///
/// What that trades away is where the paragraphs live. A field carries its explanation on the
/// field, where a reader of the struct meets it; a bit carries it on a constant here, one step
/// away. The constants below are written the way the fields were so the step is the only
/// difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeclFlags(u16);

impl DeclFlags {
    /// Nothing written.
    pub const NONE: Self = Self(0);

    /// `constexpr` was written, which makes the object a named constant.
    ///
    /// C23 6.6p8 puts a named constant of an integer type among the things an integer constant
    /// expression may be built out of, and a member of one of a structure or union type with
    /// it. That is the whole reason the keyword exists and it is why this is a fact about the
    /// declaration rather than something a reader could work out: a `const` object with a
    /// constant initializer is not one of them, so `const int n = 1; int a[n];` is a variable
    /// length array and the same two lines with `constexpr` are an array of one.
    pub const CONSTANT: Self = Self(1 << 0);

    /// An attribute asks for this to exist where nothing in the file refers to it.
    ///
    /// `used`, `retain`, `constructor`, `destructor` and `alias` each say that something reaches
    /// the definition from where the compiler cannot see it, which is the only reason a program
    /// ever writes one of them. Nothing else in the tree says that, and a `static` function
    /// nothing refers to is not emitted, so this is how a program keeps one that has to be.
    pub const RETAINED: Self = Self(1 << 1);

    /// This name is under GNU's reading of `inline` rather than C's.
    ///
    /// `__attribute__((__gnu_inline__))` asks for it by name, and the C89 dialects are under it
    /// throughout, which is what `__GNUC_GNU_INLINE__` tells a header. It is kept because the two
    /// readings fold differently over the declarations of a name, and because gcc refuses a name
    /// whose declarations disagree about which one they are under.
    pub const GNU_INLINE: Self = Self(1 << 2);

    /// Control does not come back from a call to this function.
    ///
    /// `_Noreturn`, `__attribute__((noreturn))` and `[[noreturn]]` all say it and all land here.
    /// What a caller does with it is put an `unreachable` after the call, so a program that tests
    /// its allocation with `if (!p) abort();` stops having a path where the block after the test is
    /// reached carrying a null pointer. Nothing else in the compiler can work that out, because
    /// what `abort` does belongs to `abort`.
    ///
    /// A fact about the name rather than about one declaration of it, so one declaration saying it
    /// is enough and the merge keeps it. That is the same rule [`Self::RETAINED`] is under and it
    /// is there for the same reason: the usual place to write it is a header, and the definition in
    /// the file below writes nothing.
    pub const NORETURN: Self = Self(1 << 3);

    /// The function is written without a prologue, an epilogue or a return.
    ///
    /// `__attribute__((naked))`, which says the body is the whole of the function and the compiler
    /// is to write nothing around it. What a program does with it is save and restore machine state
    /// by hand, which is what micropython's non local return is and what an interrupt handler is,
    /// and neither of those survives a prologue being written in front of it: the first instruction
    /// of micropython's `nlr_push` reads the return address out of `(%rsp)`, and a push in front of
    /// it moves the address somewhere else.
    ///
    /// A fact about the name rather than about one declaration of it, merged the way
    /// [`Self::NORETURN`] is, because it is written in the same places for the same reason.
    pub const NAKED: Self = Self(1 << 4);

    /// `__attribute__((weak))` was written on a declaration of this name.
    ///
    /// It asks for two different things depending on whether this file defines the name. On a
    /// definition it says that another object's definition of the same name wins over this one,
    /// which is how a library ships a default somebody may replace. On a declaration of something
    /// this file does not define it says that the link may leave the name undefined rather than
    /// fail, and the address a reference gets is then zero, which is how a library offers a hook a
    /// profiler may fill in: the calls are written under `if (hook)` and the test is false when
    /// nobody filled it in. zstd's four tracing hooks are the second of those and are what made
    /// this bit, since without it the link of thirty of its files fails.
    ///
    /// A fact about the name rather than about one declaration of it, like [`Self::RETAINED`], and
    /// merged the way that one is: one declaration saying it is enough, so a header may say it and
    /// the definition below may be written as an ordinary definition.
    ///
    /// It is refused on a name with internal linkage, since what it asks for is that the linker
    /// let somebody else win and a `static` name is one the linker never sees.
    pub const WEAK: Self = Self(1 << 5);

    /// `__attribute__((always_inline))` was written on a declaration of this name.
    ///
    /// Not a hint. gcc inlines every direct call to one of these at every level including `-O0`,
    /// and a header that defines one and nothing else is relying on that: an inline definition
    /// under it may have no out of line copy anywhere, and glibc's fortified wrappers forward
    /// their arguments with `__builtin_va_arg_pack`, which only means something once the body is
    /// where the call was. `rucc_opt::inline` is what keeps the promise.
    ///
    /// A fact about the name rather than about one declaration of it, merged the way
    /// [`Self::NORETURN`] is.
    pub const ALWAYS_INLINE: Self = Self(1 << 6);

    /// `__attribute__((noinline))` was written on a declaration of this name.
    ///
    /// Merged the way [`Self::ALWAYS_INLINE`] is. A name that ends up with both is under
    /// `always_inline`, which is what gcc does after warning that it ignored the other one.
    pub const NOINLINE: Self = Self(1 << 7);

    /// `__attribute__((optimize ("no-strict-aliasing")))` was written on a declaration of this
    /// name, the one part of `optimize` that is honoured.
    ///
    /// It matters once a body can be inlined: without it an access in the body carries the node
    /// for its type, and in the caller that lets a load of an `int` move past a store through a
    /// `float *` that the function was written to be allowed to make. Merged the way
    /// [`Self::NORETURN`] is.
    pub const NO_STRICT_ALIASING: Self = Self(1 << 8);

    /// Whether every bit of `other` is set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The same set with `flag` set when `on` and cleared when not, which is what a caller with a
    /// `bool` in hand wants.
    #[must_use]
    pub const fn with(self, flag: Self, on: bool) -> Self {
        if on { Self(self.0 | flag.0) } else { Self(self.0 & !flag.0) }
    }
}

impl std::ops::BitOr for DeclFlags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for DeclFlags {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// Whether a declaration declares an object, a function or a name for a type.
///
/// An enumerator is none of them, since what the program can do with one is what it can do with
/// the number it stands for, and it has been resolved by the time anything reads this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    /// An object, which includes parameters, block-scope variables and compound literals.
    Object,
    /// A function.
    Function,
    /// A name for a type, which is a `typedef` and declares nothing that exists at run time.
    ///
    /// Nearly every typedef is resolved where it is written and is not in the tree at all. The
    /// one that is here is a block-scope typedef of a variably modified type, `typedef char
    /// T[n]`, which is in the tree because there is something to do where it stands: 6.7.7.3p12
    /// says the size is evaluated when the declaration is reached, so the walk has to reach it.
    /// Nothing is emitted for one and nothing can name it as an expression, since the name is
    /// bound as a typedef rather than as a declaration.
    Type,
}

/// What a declaration promised a call to a function does, where nothing said is the default.
///
/// `__attribute__((const))` and `__attribute__((pure))` are the two claims. They are kept here
/// rather than worked out from a body because the body is usually in some other file: a unit that
/// only declares `strtol` has nothing to look at, so the promise travels on the declaration or it
/// does not travel at all. That is the rule [`DeclFlags::NORETURN`] is under and it is there for the
/// same reason.
///
/// The optimizer works out its own answer for the functions it can see, and this is not that
/// answer. What a person wrote is a promise the program made, and gcc believes the promise, so the
/// two are kept apart and met at the point they are read.
///
/// The order the variants are in is the order of strength, which is what the derived [`Ord`] is
/// for: merging two declarations of one name is taking the stronger promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Effects {
    /// No declaration of the name said anything, so a call may do anything a call may do.
    #[default]
    Any,
    /// `pure`. The result depends on the arguments and on memory, and the call writes nothing the
    /// caller can see, so two calls with the same arguments and no write between them are one
    /// call.
    Pure,
    /// `const`. The result depends on the arguments alone, so the call does not even read memory.
    /// Stronger than [`Self::Pure`], and a call to one is deletable when nothing reads it.
    Const,
}

impl Effects {
    /// What two declarations of one name promised between them, which is the stronger promise.
    ///
    /// A later declaration that says nothing does not take an earlier promise back, for the reason
    /// one declaration writing `noreturn` is enough: the place people write these is a header and
    /// the definition in the file underneath writes an ordinary definition.
    #[must_use]
    pub fn and(self, other: Effects) -> Effects {
        self.max(other)
    }

    /// Whether anything was promised at all.
    #[must_use]
    pub const fn promised(self) -> bool {
        !matches!(self, Effects::Any)
    }
}

/// Whether the definition of a name is emitted, which is what `inline` decides.
///
/// Two of the three mean that it is emitted, and they are apart because they behave differently
/// when one more declaration of the name arrives: a name nothing has said anything about takes
/// whatever the next declaration says, and one that is already an external definition stays one
/// however the rest of the file is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emission {
    /// Nothing has been said about it. Every object is this, and so is every function that is not
    /// declared at file scope with external linkage, since the rule is written about those alone.
    Silent,
    /// The definition here is an inline definition and nothing is emitted for it.
    Inline,
    /// The definition here is an external definition and is emitted.
    External,
}

impl Emission {
    /// Whether a definition of the name is emitted.
    #[must_use]
    pub const fn emits(self) -> bool {
        !matches!(self, Emission::Inline)
    }
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

/// How far outside a shared library a name reaches.
///
/// A different question from [`Linkage`] and asked of a different linker. The linkage is what the
/// static linker does with a name while it is building the output, and this is what the dynamic
/// linker may do with it once that output is a shared library and is being loaded. A hidden name
/// is still external as far as the static link is concerned, so two files in the same library
/// reach each other by it; it is simply not in the dynamic symbol table afterwards.
///
/// Four strings are written and there are three answers, because `internal` is `hidden` plus a
/// promise the program makes about never taking the address across a component boundary. Reading
/// it as hidden gives less than was asked for, which is safe in the way `-fstrict-aliasing` is
/// safe: every program correct under the stronger assumption is correct under the weaker one and
/// nothing here derives anything from the difference. It is written down in
/// `spec/13-gnu-compat.md` section 13.4 rather than left for someone to find in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// In the dynamic symbol table and interposable, which is what a name gets when nothing said
    /// otherwise and what `visibility("default")` puts back after `-fvisibility=hidden`.
    Default,
    /// Not in the dynamic symbol table, so nothing outside the library can name it.
    Hidden,
    /// In the dynamic symbol table, and a reference from inside the library binds to the
    /// definition inside it.
    Protected,
}

/// Whether a function runs without anything calling it, and where in the order it goes.
///
/// Both halves of one question, because a function may carry both attributes and they ask for
/// different ends of the program: `constructor` puts it in the run-up to `main` and `destructor`
/// in the run-down after `main` returns. Nothing in the tree says either without one of those
/// attributes being written, and nothing a translation unit contains calls such a function, which
/// is why this is the only record of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Startup {
    /// Where it goes in the run-up to `main`, from `constructor`, and nothing where that attribute
    /// was not written.
    pub before: Option<Priority>,
    /// Where it goes in the run-down after `main` returns, from `destructor`.
    pub after: Option<Priority>,
}

impl Startup {
    /// What two declarations of one name asked for between them, where the first one to say
    /// something stands.
    #[must_use]
    pub fn or(self, other: Startup) -> Startup {
        Startup { before: self.before.or(other.before), after: self.after.or(other.after) }
    }

    /// Whether either attribute was written, which is what makes the declaration one the object
    /// file has something to say about beyond the definition itself.
    #[must_use]
    pub const fn asked(&self) -> bool {
        self.before.is_some() || self.after.is_some()
    }
}

/// Where in the order one constructor or destructor goes.
///
/// The two are not one number with a default, because the unnumbered one is not at any number.
/// GCC puts a numbered entry in a section whose name carries the number and an unnumbered one in
/// the plain section, and the linker sorts the numbered sections in front of the plain one, so an
/// unnumbered constructor runs after every numbered one however high the number was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Written bare, as `__attribute__((constructor))`, which is almost every one of them.
    Unnumbered,
    /// Written with a number, as `__attribute__((constructor(101)))`, where a lower number runs
    /// earlier and two at the same number run in the order the file defined them.
    Numbered(u16),
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
    /// Whether the record this value lands in stores its scalars in the reverse byte order.
    ///
    /// The offsets here are from the start of the whole object, so the record a value belongs to
    /// is no longer in sight by the time the entry is written out. This is the one thing about
    /// that record the writing needs, and it is answered where the walk still knows which member
    /// it is on.
    pub reverse: bool,
}

impl InitEntry {
    /// A value at a byte offset, which is what everything that is not a bit-field is.
    #[must_use]
    pub const fn at(offset: u64, value: ExprId) -> InitEntry {
        InitEntry { offset, value, bit_offset: 0, bit_width: 0, reverse: false }
    }

    /// Whether this entry writes part of a byte rather than whole bytes.
    #[must_use]
    pub const fn is_bit_field(&self) -> bool {
        self.bit_width != 0
    }
}
