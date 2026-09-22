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
    /// Where the parameter is, and [`None`] where this compiler cannot yet say and for every
    /// function type.
    ///
    /// A parameter is a local that happened to arrive in a register, so where it ends up is decided
    /// the same way every other local's place is. See [`Local::spot`]. A function type never has
    /// one, since a type is not a piece of code and has no frame or registers to be in.
    pub spot: Option<Spot>,
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

/// One local the program declared.
///
/// A parameter is not here even when it has a place. It is already a child of the subprogram, from
/// the signature, and a second entry for it would be a second variable of the same name. What it
/// gets instead is [`Param::spot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    /// Its name, as the C program spelled it.
    pub name: String,
    /// Which of the unit's [`types`](crate::Unit::types) it is, and [`None`] when this compiler
    /// cannot yet say, which is the case [`Global::ty`] explains.
    pub ty: Option<usize>,
    /// Where it was declared, and nothing when that is not known.
    pub decl: Option<Place>,
    /// Where in the machine it is, over the addresses of the function it is in.
    pub spot: Spot,
    /// Which of its function's [`scopes`](crate::Function::scopes) it was declared in, and [`None`]
    /// for one written directly in the body of the function.
    pub scope: Option<usize>,
}

/// One inner scope of a function, which is a `{ ... }` the program declared something in.
///
/// A function's own body is not one of these. It is the subprogram, and a local written straight
/// into it is a child of the subprogram, so the scopes are the ones written inside that.
///
/// What they are for is the question of which `i` a debugger means. A function with two blocks that
/// each declare one has two variables of that name, and with every local a child of the subprogram
/// a reader has no way to tell which of them is in scope at the address it stopped at. Nesting the
/// entries and saying which addresses each nest covers is the answer DWARF has for that, and it is
/// the only thing here that is about a scope rather than about a variable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// Which scope this one is written inside, and [`None`] for one written directly in the body of
    /// the function.
    ///
    /// Always an earlier entry than this one, since a scope is opened before anything written in it
    /// is reached, which is what lets a reader build the tree in one pass.
    pub parent: Option<usize>,
    /// The stretches of the function's addresses the code written inside it ended up at.
    ///
    /// Empty for a scope whose code all went away, which is a scope that still says which names it
    /// held and says nothing about where they were.
    pub over: Vec<Reach>,
}

/// One stretch of a function's addresses, with nothing said about what is at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reach {
    /// How far into the function the stretch starts, in bytes.
    pub from: u64,
    /// How long it is, in bytes. A stretch of no length covers nothing and is refused.
    pub len: u64,
}

/// Where a local is over the addresses of the function it is in.
///
/// Which of the two a local gets is decided by what lowering did with it rather than by the
/// optimization level. A local with a frame slot is in that slot from the first instruction of the
/// function to the last, because the frame layout hands the slot out once and nothing moves it
/// afterwards, and one expression says so. A scalar whose address is never taken is put in an SSA
/// value instead, at every optimization level including `-O0`, and where the register allocator put
/// that value changes from one program counter to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spot {
    /// In one place at every address in the function.
    Always(Held),
    /// In a place that depends on where the program counter is, said stretch by stretch.
    ///
    /// The stretches do not have to cover the function, and an address none of them covers is an
    /// address the local is nowhere. That is the honest answer rather than a gap: a value that has
    /// not been computed yet, or whose last reader is already behind, is somewhere for part of a
    /// function and nowhere for the rest, and a debugger stopped in the rest should say the
    /// variable is not available rather than print whatever is in the register now.
    Over(Vec<Span>),
}

/// One stretch of a function's addresses and where a local is over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// How far into the function the stretch starts, in bytes.
    pub from: u64,
    /// How long it is, in bytes. A stretch of no length covers nothing and is refused.
    pub len: u64,
    /// Where the local is over it.
    pub held: Held,
}

/// A place in the machine something can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// This far from the function's frame base, which is a negative number for anything in the
    /// frame, since the frame base is the stack pointer the caller had and a frame is below that.
    Frame(i64),
    /// In this register, by the number this target's DWARF register numbering gives it.
    ///
    /// Not the register number the back end uses. The two agree on some targets and not on others,
    /// and the translation is the caller's, because the caller is what knows which target this is.
    Reg(u16),
}

/// Where in the source something was declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    /// Which of the unit's [`files`](crate::Unit::files) it is in.
    pub file: usize,
    /// Which line of that file, counting from one.
    pub line: u32,
}
