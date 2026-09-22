//! What the program means: the types a unit describes and the functions it defines.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! The line table answers where an address came from. This answers what the thing at that address
//! is: which function a program counter is inside, what that function takes and gives back, and
//! what the types in its signature are made of. A backtrace needs the first, and printing anything
//! at all needs the rest.
//!
//! # A table of shapes rather than the compiler's own types
//!
//! Nothing here is a C type. A [`Shape`] is a DWARF entry with its attributes already decided, so
//! the questions C answers and DWARF does not, which is most of them, are settled before anything
//! reaches this crate. Whoever builds the table decides what `long` is on this target, which of two
//! spellings of one width to write, whether a record is complete and where a bit-field's bits sit.
//! What is left here is writing entries down, which is the part that has to be right about DWARF
//! and has no opinion about C.
//!
//! It costs one thing, which is that a table can say something C cannot mean, and the answer to
//! that is the same as for the machine IR: the writer is not a checker and the thing that would
//! catch it is a reader. `readelf --debug-dump=info` is that reader and is what the differential
//! runs.
//!
//! Every reference between entries is an index into the unit's [`types`] table, because a table of
//! indices can be built in one pass over a recursive type without the borrow checker having
//! anything to say about it, and because a cycle is ordinary rather than special: `struct node {
//! struct node *next; }` is a pointer whose target is the record that holds it, and an index says
//! that with nothing added.
//!
//! # What is described and what is skipped
//!
//! An [`Option<usize>`] target is DWARF's own convention, where the absence of a `DW_AT_type` means
//! `void`. A type this compiler cannot yet describe is not that: a function that mentions one gets
//! no [`Sig`], and a function with no `Sig` gets no `DW_TAG_subprogram` at all, so a debugger falls
//! back to the symbol table for it the way it does for every function today. The alternative is an
//! entry that says `void` or `void *` where the program said something else, and a debugger showing
//! a wrong type is worse than one showing none.
//!
//! [`types`]: crate::Unit::types

/// How the bits of a base type are read, which is DWARF's `DW_AT_encoding`.
///
/// The set C needs and no more. An enumeration rather than the `DW_ATE_` constants themselves so
/// that whoever builds a table does not have to depend on `gimli` to name one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// `bool`, whose one byte holds zero or one.
    Boolean,
    /// A signed integer in two's complement.
    Signed,
    /// An unsigned integer.
    Unsigned,
    /// `char` where it is signed, which DWARF keeps apart from a signed integer because a debugger
    /// prints one as a character and the other as a number.
    SignedChar,
    /// `char` where it is unsigned, and `unsigned char`.
    UnsignedChar,
    /// One of the real floating types.
    Float,
    /// `_Complex T`, whose bytes are the real half and then the imaginary one.
    Complex,
}

/// A qualifier, which DWARF writes as an entry wrapping the type it qualifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qualifier {
    /// `const`.
    Const,
    /// `volatile`.
    Volatile,
    /// `restrict`.
    Restrict,
    /// `_Atomic`, which C calls a type rather than a qualifier and DWARF writes like one.
    Atomic,
}

/// Which bits of a record a bit-field member is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bits {
    /// How far into the record the first bit is, in bits from the start of it.
    ///
    /// Bits from the start of the whole record and not from the start of some storage unit, which
    /// is what `DW_AT_data_bit_offset` means and is the one of DWARF's two spellings that does not
    /// depend on the reader working out which unit was meant or which way round the target is.
    pub at: u64,
    /// How many bits it is, which is the width the program wrote.
    pub width: u64,
}

/// One member of a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Its name, absent for an anonymous `struct` or `union` member and for an unnamed bit-field.
    pub name: Option<String>,
    /// Which of the unit's [`types`](crate::Unit::types) it is.
    pub ty: usize,
    /// How far into the record it starts, in bytes. Meaningless for a bit-field, which says where
    /// it is in [`Member::bits`] instead.
    pub at: u64,
    /// Which bits it is, for a bit-field, and [`None`] for an ordinary member.
    pub bits: Option<Bits>,
}

/// One type, in the terms DWARF describes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// A type whose bytes are read directly, which is every arithmetic type C has.
    Base {
        /// What it is called, which is the spelling the program would have written.
        name: String,
        /// How its bits are read.
        encoding: Encoding,
        /// How many bytes it is.
        size: u64,
    },
    /// A pointer, whose target is [`None`] for `void *`.
    Pointer {
        /// Which of the unit's [`types`](crate::Unit::types) it points at.
        to: Option<usize>,
        /// How many bytes the pointer itself is.
        size: u64,
    },
    /// An array, whose length is [`None`] when it has none a number can say.
    ///
    /// That covers three C spellings at once: the flexible array member, the array of unknown
    /// length, and the array whose length is an expression. DWARF can describe the last of those
    /// with an expression of its own, which is worth doing and is not done here, and leaving the
    /// bound out is the answer a debugger already has to handle for the other two.
    Array {
        /// Which of the unit's [`types`](crate::Unit::types) the elements are.
        of: usize,
        /// How many of them there are.
        count: Option<u64>,
    },
    /// A `struct` or a `union`, whose members are [`None`] when it is incomplete.
    Record {
        /// Whether it is a `union` rather than a `struct`.
        union: bool,
        /// Its tag, absent for one the program left anonymous.
        name: Option<String>,
        /// How many bytes it is, and nothing for an incomplete one.
        size: Option<u64>,
        /// The members in the order they were written, and [`None`] for an incomplete record,
        /// which is a different thing from a complete record with no members.
        members: Option<Vec<Member>>,
    },
    /// An `enum`.
    Enumeration {
        /// Its tag, absent for one the program left anonymous.
        name: Option<String>,
        /// Which of the unit's [`types`](crate::Unit::types) the enumerators are held in.
        of: usize,
        /// How many bytes it is.
        size: u64,
        /// The enumerators in the order the program wrote them.
        ///
        /// Empty for an enumeration that has not been completed, which is a thing a C program can
        /// mention but cannot have an object of.
        values: Vec<Constant>,
    },
    /// A `typedef` name for another type.
    Alias {
        /// The name.
        name: String,
        /// Which of the unit's [`types`](crate::Unit::types) it stands for, and [`None`] for a
        /// name for `void`.
        of: Option<usize>,
    },
    /// A qualified version of another type.
    Qualified {
        /// Which qualifier.
        which: Qualifier,
        /// Which of the unit's [`types`](crate::Unit::types) it qualifies, and [`None`] for
        /// qualified `void`.
        of: Option<usize>,
    },
    /// A function type, which is what a pointer to a function points at.
    Subroutine(Sig),
}

/// One enumerator of an enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constant {
    /// The name the program wrote, which is what a debugger prints in place of the number.
    pub name: String,
    /// Its value.
    ///
    /// Wider than any enumeration a target has, so that the writer decides which DWARF form the
    /// number goes in rather than the caller having to know. A value wider than 64 bits has no
    /// form that holds it and is left out, the same way a record drops a member it cannot
    /// describe and keeps the rest.
    pub value: i128,
}

/// What a function takes and gives back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sig {
    /// Which of the unit's [`types`](crate::Unit::types) it returns, and [`None`] for `void`.
    pub returns: Option<usize>,
    /// The parameters in order.
    pub params: Vec<Param>,
    /// Whether it ends in `...`.
    pub variadic: bool,
    /// Whether the program wrote a parameter list rather than leaving it empty.
    ///
    /// `DW_AT_prototyped`, and it is not the same question as whether the list is empty: `int f()`
    /// says nothing about what `f` takes and `int f(void)` says it takes nothing, and a debugger
    /// that could not tell them apart would offer to call the first with no arguments.
    pub prototyped: bool,
}

/// One parameter of a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Its name, absent in a prototype that gave none and in a function type.
    pub name: Option<String>,
    /// Which of the unit's [`types`](crate::Unit::types) it is.
    pub ty: usize,
}

/// One variable the unit defines at file scope.
///
/// Not the same problem as a local, and that is the whole reason this is here and a local is not.
/// A file-scope variable is at one address for the whole of the program, so its location is the
/// address of its own symbol and the linker fills it in, the same way it fills in a function's. A
/// local's location is wherever the code happens to be keeping it at the program counter the
/// debugger stopped at, which is a list rather than an expression, and that is the rest of
/// tamnd/rucc#9.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Global {
    /// Its name, as the C program spelled it, which is what the relocation asks the linker for.
    pub name: String,
    /// Which of the unit's [`types`](crate::Unit::types) it is, and [`None`] when this compiler
    /// cannot yet say.
    ///
    /// A variable with nothing here still gets an entry, which is the one place the rule for a
    /// function is turned around. A `DW_TAG_variable` with no `DW_AT_type` does not say `void`,
    /// because nothing in C is a variable of type `void`, so a reader takes it as a variable whose
    /// type was not recorded. The name and the address are worth having on their own: they are
    /// what lets a debugger resolve the name at all, and a program that knows what it is looking
    /// at can cast.
    pub ty: Option<usize>,
    /// Where it was declared, and nothing when that is not known.
    pub decl: Option<Place>,
    /// Whether anything outside this unit can see it, which is the opposite of `static`.
    pub external: bool,
}

/// Where in the source something was declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    /// Which of the unit's [`files`](crate::Unit::files) it is in.
    pub file: usize,
    /// Which line of that file, counting from one.
    pub line: u32,
}
