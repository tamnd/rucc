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
    /// What an integer narrower than an `int` has above it in the register it travels in.
    pub narrow: Narrow,
    /// What a `_BitInt` of a width no register has holds above it in the register it travels in.
    pub bit_ints: BitInts,
    /// Who takes the arguments in the argument area off the stack once the call is over.
    pub cleanup: Cleanup,
}

impl AbiDescription {
    /// Whether a scalar of this size travels as the address of a copy the caller made.
    ///
    /// The size rule of the one ABI that does this is written over the size of the object and says
    /// nothing about what is in it, which is why this takes a number rather than a [`Scalar`]: a
    /// pass writing a call to a runtime routine has a width in hand and no C type behind it, and the
    /// answer is the same for both askers because there is only the one rule.
    ///
    /// [`Scalar`]: crate::shape::Scalar
    #[must_use]
    pub const fn scalar_is_by_reference(&self, size: u64) -> bool {
        self.scalars.wide_is_by_reference && !matches!(size, 1 | 2 | 4 | 8)
    }

    /// The format an integer of this size comes back in, where the ABI brings one back whole in a
    /// vector register rather than through the address the caller passed.
    ///
    /// The companion to [`AbiDescription::scalar_is_by_reference`] and asked by the same kind of
    /// caller for the same reason, a pass writing a call to a runtime routine with a width in hand
    /// and no C type behind it. It is a separate question rather than the same one answered the
    /// other way because the two disagree on the one ABI that says yes to either: Windows x64
    /// passes a sixteen byte integer as an address and brings one back in xmm0, so `__fixtfti`
    /// there takes an address and answers in a vector register.
    ///
    /// [`None`] where the size is one a register holds, since then nothing about it is wide, and
    /// [`None`] on every ABI that does not do this.
    #[must_use]
    pub const fn wide_integer_returns_in(&self, size: u64) -> Option<Format> {
        match self.scalars.wide_integer_returns_in {
            Some(format) if self.scalar_is_by_reference(size) => Some(format),
            _ => None,
        }
    }
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
    /// Whether an integer wider than one register that goes in registers starts at an even one.
    ///
    /// True on AAPCS64, whose rule C.9 rounds the next general purpose register up to an even
    /// number for a value aligned to sixteen, so an `__int128` after one `int` is in x2 and x3 and
    /// x1 is left empty. False on Darwin arm64, which gives it x1 and x2, and everywhere else.
    pub wide_integer_starts_even: bool,
    /// Whether an integer wider than one register that finds too few left spends the rest of them.
    ///
    /// True on AAPCS64, Darwin's included, where such a value goes in the argument area and the
    /// registers it could not use are not given to the arguments after it. False on SysV AMD64,
    /// where they are.
    pub wide_integer_drains: bool,
    /// Whether a scalar of a size no register holds travels as the address of a copy the caller
    /// made, the way an aggregate of that size does.
    ///
    /// True on Windows x64, whose one rule is about the size of the object and not about what is
    /// inside it: anything that is not one, two, four or eight bytes is an address, and a
    /// `long double`, a `_Float128` and an `__int128` are all sixteen bytes there. False on the
    /// other four, where a wide scalar has registers to travel in or a place in the argument area
    /// of its own, which is what [`Scalars::in_memory`] says for the one that puts it there.
    pub wide_is_by_reference: bool,
    /// The format a wide integer comes back in, where the ABI brings one back in a vector
    /// register rather than through the address the caller passed.
    ///
    /// `Some(Format::Quad)` on Windows x64, and for an integer only: gcc returns an `__int128`
    /// in xmm0 there, which is its own answer to a convention that has no 128-bit integer in it,
    /// and brings the two floating point types of the same size back through the address like
    /// everything else that size. `None` everywhere else, including on the ABIs where a wide
    /// integer is not by reference to begin with.
    pub wide_integer_returns_in: Option<Format>,
    /// Whether an integer wider than one register goes in the argument area whatever registers
    /// are left, and spends the ones that are.
    ///
    /// True for i386 `fastcall`, whose two registers hold an integer of four bytes or fewer and
    /// nothing wider: a `long long` is on the stack, and an `int` after it is on the stack too
    /// rather than in ecx. gcc and Microsoft's compiler agree on both halves. False everywhere
    /// else, where a wide integer takes registers when there are enough of them.
    pub wide_integer_in_memory: bool,
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
    /// A hidden first argument that the callee takes off the stack as it returns, with a `ret $4`,
    /// so the caller finds the stack one word higher than it left it.
    ///
    /// i386 SysV. It spends an argument slot the way [`ReturnPointer::FirstArgument`] does.
    FirstArgumentPopped,
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
    /// Every argument of a variadic function travels in general purpose registers and the argument
    /// area, the named ones as well as the rest, and nothing is looked inside for floating point
    /// members.
    ///
    /// Windows on AArch64. The callee homes x0 to x7 directly below the arguments the caller left
    /// in memory and walks the lot with a `char *`, so there is only one bank it could read. A
    /// `double` travels as its bits in the next x register, a structure of four `float`s is the
    /// sixteen bytes of two x registers rather than four s registers, and one of four `double`s is
    /// over sixteen bytes and so travels as the address of a copy. It reaches the named arguments
    /// too, which is the difference from [`Variadic::BothBanks`]: `void f(double, ...)` takes its
    /// `double` in x0, and a call through a prototype without the `...` puts it in d0. The value
    /// that comes back is not an argument and comes back where it always does.
    IntegersOnly,
}

/// What the bits above an integer narrower than an `int` are when it travels in a register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Narrow {
    /// Anything. The side that receives the value reads only its own bits, which is every ELF ABI
    /// here as their documents are written, whatever a particular compiler happens to leave.
    Unspecified,
    /// Extended to 32 bits by the type's own sign, by the caller for an argument and by the
    /// callee for a return value.
    ///
    /// Darwin arm64, where clang's callee of `f(unsigned char)` returns its argument with a bare
    /// `ret`, trusting the caller to have cleared bits 8 to 31, and its caller of a function
    /// returning `unsigned char` compares all of `w0` without clearing them itself.
    ToInt,
}

/// What the bits above a `_BitInt` narrower than its register are when it travels in one.
///
/// A separate question from [`Narrow`], because the documents answer it separately. RISC-V
/// leaves the bits above a `char` alone and extends a `_BitInt(N)` to the whole register, and
/// AAPCS64 has a `_BitInt` section of its own that is not the one for `char`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitInts {
    /// Anything, both ways. The side that receives the value reads only its own bits, and the
    /// side that sends it owes nothing above them. The x86-64 psABI, and gcc 16.2.0 does what it
    /// says: its callee of `f(unsigned _BitInt(37))` masks the argument before it divides, and its
    /// caller of a function returning `_BitInt(7)` reads only the low seven bits of `al`.
    SpareBits,
    /// The rule has not been taught here, so a function with one at its boundary is refused by
    /// name rather than compiled to an agreement nobody checked. tamnd/rucc#425 is the issue.
    Untaught,
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

/// Who takes the arguments in the argument area off the stack once the call is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    /// The caller, which is every convention a target calls its own. The callee returns with a
    /// plain `ret` and whatever the caller put in the argument area is still there after it.
    Caller,
    /// The callee, which returns with `ret $n` and leaves the stack pointer `n` bytes higher than
    /// the caller had it at the call.
    ///
    /// i386 `stdcall` and `fastcall`, which is what most of the Windows API is declared with, and
    /// the reason a variadic function cannot be either: only the caller knows how many bytes it
    /// pushed. `n` counts the argument area and nothing in a register, so it is the address of a
    /// returned structure as well when that is on the stack and not when `fastcall` has it in ecx.
    Callee,
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
    /// One `float` or one `double` and nothing else, however deeply it is wrapped.
    ///
    /// mingw's i386 return rule, which gcc and clang both follow: `struct { double d; }` comes
    /// back in st(0) the way a bare `double` does, where MSVC's rule for the same structure is
    /// edx:eax. A second member of any kind, even another `float`, turns it back into an
    /// ordinary eight byte structure.
    LoneFloat,
    /// A `_Complex float` and nothing else, which is not the same as a structure of two floats.
    ///
    /// The i386 SysV return rule gcc and libgcc follow. Every structure comes back in memory there,
    /// and a `_Complex float` comes back in edx:eax as the eight bytes it is, which is also how
    /// `__mulsc3` and `__divsc3` give their answer back.
    ComplexFloat,
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
        /// the first is SSEUP. A vector produces a run of those, and a `_Float128` produces one,
        /// and sixteen bytes of `_Float128` is inside this limit rather than over it.
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
    /// As the object's own bytes in the argument area, with every register left spent, so an
    /// argument after it that would have fit in one is in the argument area as well.
    ///
    /// i386 `fastcall`, where a structure of any size is on the stack and the `int` after it does
    /// not get the ecx the structure did not use.
    InMemoryAndDrain,
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

#[cfg(test)]
mod tests {
    use crate::abis::{AAPCS64, SYSV_AMD64, WIN64};
    use crate::shape::Format;

    /// The two questions about a wide scalar are asked separately because the one ABI that says
    /// yes to either gives different answers to them.
    #[test]
    fn windows_passes_a_wide_scalar_as_an_address_and_brings_an_integer_back_in_a_register() {
        assert!(WIN64.scalar_is_by_reference(16));
        assert_eq!(WIN64.wide_integer_returns_in(16), Some(Format::Quad));
    }

    /// A size a register holds is not wide, whatever the ABI says about the ones that are.
    #[test]
    fn a_size_a_register_holds_is_neither() {
        for size in [1, 2, 4, 8] {
            assert!(!WIN64.scalar_is_by_reference(size), "{size} bytes fits a register");
            assert_eq!(WIN64.wide_integer_returns_in(size), None, "{size} bytes fits a register");
        }
    }

    /// Everywhere else a wide scalar has registers to travel in, so neither question applies.
    #[test]
    fn the_conventions_with_registers_for_one_say_no_to_both() {
        for abi in [&SYSV_AMD64, &AAPCS64] {
            assert!(!abi.scalar_is_by_reference(16), "{}", abi.name);
            assert_eq!(abi.wide_integer_returns_in(16), None, "{}", abi.name);
        }
    }
}
