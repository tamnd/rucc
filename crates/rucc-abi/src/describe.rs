//! The language an ABI is described in.
//!
//! Design: `spec/cross-compile/06-abis.md` section 6.7.
//!
//! # The argument, restated
//!
//! `spec/cross-compile/06-abis.md` section 6.1 lists fifteen psABIs and the compiler has four of them today,
//! hand written, at about a thousand lines. Fifteen at that rate is six to ten thousand lines of
//! the most bug prone code in a compiler, and `spec/cross-compile/02-the-goal.md` claim 3 says the per target
//! line count outside the target crate and the rule set has to be zero.
//!
//! # Where the line is drawn, and why here
//!
//! The tempting version of this idea is to make everything data, and it does not work. The SysV
//! eightbyte merge is a real algorithm with a real fixed point, the homogeneous aggregate scan
//! walks a list and compares members, and writing either as a table produces an interpreter that
//! is longer than the four functions it replaced and slower than all of them.
//!
//! So the split is between mechanism and policy. The mechanisms are code, in [`crate::classify`],
//! and there are four of them across the five ABIs described here: cut into eightbytes and merge,
//! look for a homogeneous run of floating point members, look for a one or two member aggregate
//! with a floating point member in it, and check the size against a list. The policies are data,
//! and a policy is which mechanisms an ABI applies, in what order, with what limits, and what
//! happens when the registers a mechanism wanted are not there.
//!
//! That split is what makes the count work. The fifth ABI reuses a mechanism and costs a
//! description. The eleventh probably does too. A new mechanism is a real cost and it is paid
//! once per idea rather than once per target, and there are far fewer ideas than targets.
//!
//! # The performance objection
//!
//! Section 6.7 raises it against itself: a compile time decision becoming a run time table walk,
//! on the hot path. Two answers. The classifier runs once per call site and once per function
//! signature rather than once per instruction, so the exposure is bounded, and the descriptions
//! are `const` data reached through a `&'static`, so the branch predictor sees the same rule list
//! for every call in a translation unit.
//!
//! Bounded is a prediction rather than a measurement, and the measurement is `spec/cross-compile/02-the-goal.md`
//! claim 2's benchmark at the migration point. The fallback if it fails is written down in
//! section 6.7: the descriptions stay as the source of truth for the tests and the documentation
//! and the classifiers go back to being hand written, which loses claim 3 and keeps claim 2.
//! Claim 2 outranks claim 3.

use crate::shape::Format;

/// One psABI, completely.
///
/// Everything an ABI decides about how a value travels is in here. What is deliberately not in
/// here is in [`AbiDescription::stack_args`]'s note: prologue emission, register allocation
/// constraints and unwind emission are per architecture code with per ABI parameters, and
/// section 6.7 is explicit that turning those into tables costs more than the duplication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiDescription {
    /// What the ABI is called, which is the name that goes in a diagnostic and in the report.
    pub name: &'static str,
    /// The registers a call starts with, and how a scalar spends them.
    pub banks: Banks,
    /// How a scalar spends registers, which differs between ABIs more than it looks like it
    /// should.
    pub scalars: Scalars,
    /// The rules for a return value, tried in order.
    pub returns: &'static [Rule],
    /// The rules for an argument, tried in order.
    pub arguments: &'static [Rule],
    /// Where the address of a return value that comes back in memory travels.
    pub return_pointer: ReturnPointer,
    /// What a variadic argument does differently.
    pub variadic: Variadic,
    /// How arguments that did not get a register sit in the argument area.
    ///
    /// Nothing in this crate reads it. It is here because it is a fact about the ABI and section
    /// 6.7 wants the description to be the source of truth for the whole ABI rather than for the
    /// half of it that happens to be classification, and because the backend that does read it
    /// should be reading it from the same place the tests are generated from.
    pub stack_args: StackArgs,
}

/// The registers a call starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Banks {
    /// General purpose argument registers.
    pub integer: u32,
    /// Floating point argument registers.
    pub float: u32,
    /// Whether the two banks share argument positions.
    ///
    /// True on Windows x64, where rcx, rdx, r8 and r9 and xmm0 to xmm3 are the same four
    /// positions, so a call taking an `int` and then a `double` uses rcx and xmm1 and never
    /// xmm0. When this is set the floating point bank is not counted separately and every spend
    /// comes out of the integer one, which is why [`Banks::float`] is zero on such a target.
    pub shared: bool,
    /// The width of a general purpose register in bytes, which is how wide one integer slot is.
    pub integer_width: u64,
    /// The widest floating point value a vector register holds, in bytes.
    ///
    /// Eight on RISC-V LP64D, where a sixteen byte `long double` therefore travels in integer
    /// registers, and sixteen on AAPCS64, where it does not. This is the field that makes the
    /// difference between those two ABIs' otherwise identical treatment of a wide float.
    pub float_width: u64,
}

/// How a scalar spends registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scalars {
    /// A floating point value in this format travels in the argument area and spends nothing.
    ///
    /// `Some(Format::X87Extended)` on SysV AMD64, where a `long double` argument is on the stack
    /// and there is no register file it could have gone in. `None` everywhere else.
    pub in_memory: Option<Format>,
    /// Whether an integer wider than one register takes every register it needs or none of them.
    ///
    /// True on SysV AMD64, where an `__int128` takes two consecutive general purpose registers,
    /// and taking one of them would spend a register on half a value and deny it to an argument
    /// after it that could have used the whole thing.
    pub wide_integer_is_all_or_nothing: bool,
}

/// Where the address of a return value that comes back in memory travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnPointer {
    /// A hidden first argument, which spends an argument register.
    ///
    /// SysV AMD64, Windows x64 and RISC-V. This is why the return value is classified before the
    /// arguments: on these three, a function returning a large structure has one argument
    /// register fewer than the same function returning `int`, and classifying the arguments
    /// first gives the wrong answer for the last one of them.
    FirstArgument,
    /// A register outside the argument bank, which spends nothing.
    ///
    /// AAPCS64's x8. A function returning a large structure still has all eight argument
    /// registers for what it was called with.
    Dedicated,
}

/// What a variadic argument does differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variadic {
    /// Nothing. A variadic argument is classified the same way a fixed one is.
    SameAsFixed,
    /// Every variadic argument is in the argument area, whatever registers are left.
    ///
    /// Darwin arm64, and the divergence that makes it a separate ABI rather than AAPCS64 with
    /// notes, per `spec/cross-compile/06-abis.md` section 6.3. It is also the reason a variadic call there is
    /// ABI-incompatible with a non-variadic one, so calling an unprototyped function works until
    /// the day it does not.
    AlwaysMemory,
    /// A floating point argument travels in both its vector register and the corresponding
    /// general purpose one.
    ///
    /// Windows x64, because the callee of a variadic function does not know which bank to read.
    BothBanks,
}

/// How arguments that did not get a register sit in the argument area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackArgs {
    /// Each argument occupies a whole number of registers' worth of the argument area, so a
    /// `char` takes eight bytes. Every ELF ABI here.
    RegisterSized,
    /// Each argument occupies its natural size and alignment, so a `char` takes one byte.
    ///
    /// Darwin arm64. Getting this wrong produces functions whose ninth argument onward is
    /// garbage, on Darwin only, which is `spec/cross-compile/06-abis.md` section 6.3's first row.
    Packed,
}

/// One rule: what an aggregate has to look like, how it travels if it does, and what happens
/// when the registers it wanted are not there.
///
/// The rules are tried in order and the first one whose test matches wins, so a rule list reads
/// the way the psABI document it came from is written: the special cases first, the general size
/// rule after them, and the catch-all last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    /// What the aggregate has to look like.
    pub when: Test,
    /// How it travels if it does.
    pub then: Travel,
    /// What happens if the registers it wanted are not there.
    pub short: Short,
}

impl Rule {
    /// A rule that cannot run short of registers, which is every rule whose result does not
    /// depend on how many are left.
    #[must_use]
    pub const fn new(when: Test, then: Travel) -> Self {
        Self { when, then, short: Short::Unchanged }
    }

    /// The same rule, with what happens when the registers are gone.
    #[must_use]
    pub const fn short(self, short: Short) -> Self {
        Self { short, ..self }
    }
}

/// What an aggregate has to look like for a rule to apply.
///
/// Four of these look inside the aggregate and the rest read its size. The four are the
/// mechanisms of this crate, and the claim in section 6.7 is that the number of them grows much
/// more slowly than the number of ABIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Test {
    /// Anything, which is what the last rule in a list is.
    Anything,
    /// An aggregate of no size, which is a GNU empty struct and travels nowhere.
    Empty,
    /// A size that is exactly one of these.
    ///
    /// Windows x64's rule, and the sharpest one on the list: anything not exactly one, two, four
    /// or eight bytes travels as an address, so a three byte structure and a three hundred byte
    /// structure are passed the same way. Also s390x's, with the same list.
    SizeOneOf(&'static [u64]),
    /// A size at most this many bytes.
    SizeAtMost(u64),
    /// A homogeneous floating point aggregate of at most this many members.
    ///
    /// AAPCS64's HFA, and the same idea with a different limit on AAPCS32 hard float and on
    /// ELFv2. Homogeneous means every scalar in it is the same floating point type once arrays
    /// and nested records are flattened, and that they fill the aggregate with no padding left
    /// over. The second half is what rules out `struct { float a; char pad[8]; }` and anything a
    /// zero width bit-field has stretched.
    Homogeneous {
        /// The most members it can have and still travel in vector registers.
        limit: usize,
    },
    /// One or two members with at least one floating point member between them, each fitting one
    /// register.
    ///
    /// The RISC-V rule, and LoongArch's. `struct { double re, im; }` is two floating point
    /// registers and `struct { double value; int tag; }` is one of each, which no other ABI on
    /// the list does. A member wider than a floating point register is not a floating point
    /// member for this purpose, which is what makes a `long double` here behave like an integer
    /// pair.
    FloatPair,
    /// Every scalar is an x87 `long double`, and there is one of them, or two if it is a
    /// `_Complex`.
    ///
    /// The SysV return path, where a `long double` comes back in st(0) and a `_Complex long
    /// double` in st(0) and st(1). A record holding two of them is the same thirty two bytes and
    /// comes back in memory, which is the only thing [`crate::Shape::complex`] is for.
    X87Stack,
    /// The SysV eightbyte classification succeeds, and no eightbyte came out x87.
    ///
    /// The intricate one. The aggregate is cut into eight byte chunks, each chunk gets a class
    /// from merging the classes of every scalar reaching into it, and any chunk that comes out
    /// MEMORY takes the whole argument to memory with it. The cases that catch people are all in
    /// the merge: an eightbyte holding an `int` and a `float` together is INTEGER, so the float
    /// travels in a general purpose register, and a member away from its natural alignment sends
    /// the whole thing to memory.
    Eightbytes {
        /// The largest aggregate that can be classified at all, sixteen bytes on SysV.
        ///
        /// It is a consequence of the eight eightbyte limit rather than an independent rule: an
        /// aggregate over two eightbytes travels in registers only when every eightbyte after
        /// the first is SSEUP, and only a vector produces those.
        limit: u64,
    },
}

/// How a value travels when a rule's test matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Travel {
    /// Nothing travels.
    Ignore,
    /// In the slots the test found, which is only meaningful after a test that finds some.
    AsFound,
    /// As a run of integer registers covering the object, one per register width, the last one
    /// holding only what is left.
    AsIntegers,
    /// As one integer register of the object's exact size, whatever is in it.
    ///
    /// Windows x64, where a `struct { float x, y; }` arrives in rcx rather than in xmm0.
    AsOneInteger,
    /// As the address of a copy.
    ByReference,
    /// As the object's own bytes in the argument area.
    InMemory,
}

/// What happens when the registers a rule wanted are not there.
///
/// This is the part of a psABI that is easiest to get wrong and hardest to notice, because every
/// test anybody writes by hand passes few enough arguments that it never comes up. The ninth
/// argument of a call is not classified the way the first one is on three of the five ABIs here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Short {
    /// Running out changes nothing. The value goes in the argument area in the same form it
    /// would have had in a register, and the spend saturates.
    ///
    /// Every scalar, and every aggregate on Windows x64, where an argument past the fourth
    /// travels the way the first one does.
    Unchanged,
    /// The argument goes in the argument area, and the registers that are left stay available
    /// for the arguments after it.
    ///
    /// SysV AMD64. An aggregate that did not fit does not stop a later scalar from getting a
    /// register, which is the opposite of what AAPCS64 does with the same situation.
    Memory,
    /// The argument goes in the argument area, and every remaining register of that bank goes
    /// with it.
    ///
    /// AAPCS64 and RISC-V. The draining is the surprising half: once one aggregate has been put
    /// on the stack for want of registers, a later argument that would have fitted goes on the
    /// stack too, because the ABI will not leave a hole in the register sequence.
    MemoryAndDrain,
    /// The rule does not apply after all, and the rules after it are tried.
    ///
    /// The RISC-V floating point pair, which is a bonus rather than a requirement: an aggregate
    /// the rule reached but the registers did not is classified by the ordinary size rules and
    /// still travels in registers if those find any.
    TryNextRule,
}
