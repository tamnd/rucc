//! How an argument travels and how a return value comes back, which is the target's answer
//! and never C's.
//!
//! Design: `spec/12-abi-and-runtime.md` sections 12.1 to 12.5.
//!
//! The same declaration passes a pair of registers on one target and a hidden pointer on
//! another, so this is the one question about a C function that cannot be answered by reading
//! the C. It is answered here, as data and an algorithm over data, because
//! `spec/18-package-layout.md` section 18.2 says there is no target-specific code outside this
//! crate.
//!
//! # What is asked and what is answered
//!
//! A caller flattens a C type into a [`Shape`], which is a size, an alignment and the scalars
//! inside it with the offsets the layout gave them. That is everything every psABI here reads:
//! the classification rules are all written over where the scalars are and whether they are
//! integers or floating point. Flattening is the caller's job because it is where the C type
//! system lives, and every rule after it is the target's.
//!
//! The answer is a [`Pass`], which is one of five things: nothing travels, the value travels as
//! itself, the object travels as a list of [`Slot`]s that each hold a register's worth of it,
//! the address of a copy travels in its place, or the object's own bytes go in the argument
//! area. A scalar is always [`Pass::Direct`]: whether it ends up in a register or on the stack
//! is the backend's arithmetic and not a change of form, and the only reason this cares about
//! scalars at all is that they spend the registers an aggregate after them was hoping for.
//!
//! # Why one call at a time
//!
//! Three of these ABIs put an aggregate in memory when the registers it wanted are gone, so the
//! answer for one argument depends on every argument before it and on whether the return value
//! took a register on its way past. That is what [`Call`] is: the registers a call has left.
//! Ask it about the return value first, then about the arguments in order, which is the order
//! the ABI documents themselves are written in.

use rucc_base::float::Format;

use crate::{Arch, Os, TargetInfo};

mod aapcs;
mod riscv;
mod sysv;
mod win64;

/// What a scalar is, once the ABI is the one asking.
///
/// Signedness is not here. Every ABI on this list passes a value of a given width the same way
/// whichever end of the range it is at, and the widening a small argument gets is a property of
/// the call and not of the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An integer, an enumeration, a `bool` or a pointer.
    Integer,
    /// A floating point value in this format.
    Float(Format),
}

/// One scalar, with the two facts about it a psABI reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scalar {
    /// Whether it is an integer or a floating point value.
    pub kind: Kind,
    /// How many bytes it takes in memory, which is the target's answer rather than the
    /// format's: an x87 `long double` is eighty bits of value in sixteen bytes of storage.
    pub size: u64,
    /// What it is aligned to, in bytes.
    ///
    /// One for a bit-field, which is allowed to start anywhere and which is an integer wherever
    /// it starts. That matters because an ordinary member that is not aligned puts the whole
    /// aggregate in memory on SysV, and a bit-field that straddles an eightbyte does not.
    pub align: u64,
}

/// One scalar inside an aggregate, at the offset the layout put it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Where it starts, in bytes from the start of the aggregate.
    pub offset: u64,
    /// What it is.
    pub scalar: Scalar,
}

impl Piece {
    /// One past the last byte it covers.
    fn end(&self) -> u64 {
        self.offset + self.scalar.size.max(1)
    }
}

/// An aggregate, as much of it as an ABI cares about.
///
/// The pieces are every scalar in it, arrays and nested records flattened out, in offset order.
/// Padding is not a piece: a hole is described by the offsets around it, which is what the
/// classification rules are written over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape<'a> {
    /// The size of the whole thing in bytes, padding included.
    pub size: u64,
    /// What it is aligned to, in bytes.
    pub align: u64,
    /// The scalars in it.
    pub pieces: &'a [Piece],
    /// Whether it is a `_Complex` rather than a `struct` or a `union` of the same shape.
    ///
    /// One rule reads this and it is on SysV AMD64, where `_Complex long double` comes back on
    /// the x87 stack and `struct { long double a, b; }`, which is the same thirty two bytes
    /// with the same two members in the same places, comes back in memory.
    pub complex: bool,
}

/// One argument, or one return value, as much of it as an ABI cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg<'a> {
    /// `void`, which is a return type and never an argument.
    Void,
    /// A scalar.
    Scalar(Scalar),
    /// A `struct`, a `union`, an array or a `_Complex`.
    Aggregate(Shape<'a>),
}

/// One register's worth of an aggregate that travels in registers, and which of the object's
/// bytes go in it.
///
/// A slot is what the object's bytes are read as rather than what the program wrote. An
/// eightbyte holding two `float`s is [`Slot::Float`] of [`Format::Double`], because eight bytes
/// of floating point data go in one vector register whichever way they are divided up and the
/// bits that arrive are the same either way.
///
/// The offset is here because it cannot be worked out from the run of slots. Two eightbytes are
/// at zero and eight and four `float`s of a homogeneous aggregate are four bytes apart, but
/// `struct { int a; double b; }` on RISC-V travels as an integer and a floating point register
/// whose bytes are at zero and eight, and the second is not where the first one ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// An integer this many bytes wide, which is one general purpose register.
    ///
    /// The last slot of an aggregate is as wide as what is left of it, so a twelve byte
    /// structure is eight bytes and then four, and nothing reads a byte past the object.
    Integer {
        /// Where its bytes start in the object.
        offset: u64,
        /// How many of them there are.
        size: u32,
    },
    /// A floating point value in this format, which is one vector register.
    Float {
        /// Where its bytes start in the object.
        offset: u64,
        /// What is read out of them.
        format: Format,
    },
}

impl Slot {
    /// Where its bytes start in the object.
    #[must_use]
    pub const fn offset(self) -> u64 {
        match self {
            Self::Integer { offset, .. } | Self::Float { offset, .. } => offset,
        }
    }
}

/// How one value travels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pass {
    /// Nothing travels, which is `void` and an aggregate of no size.
    Ignore,
    /// The value itself. Every scalar is this.
    Direct,
    /// The object's bytes, in these slots, which is what passing an aggregate in registers
    /// means once the object has been taken apart.
    Pieces(Vec<Slot>),
    /// The address of a copy, in the place the value would have gone.
    ///
    /// For an argument the caller makes the copy, and for a return value the caller passes the
    /// address of somewhere to put it, which is what the hidden first argument is.
    Reference,
    /// The object's own bytes, in the argument area, with no address anywhere.
    ///
    /// This is SysV's MEMORY class and AAPCS's aggregate that ran out of registers. It is never
    /// a return value: a return value that does not fit in registers is [`Pass::Reference`].
    Memory,
}

/// Which psABI a target follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Convention {
    /// SysV AMD64, which is x86-64 everywhere but Windows.
    Sysv,
    /// AAPCS64, and Apple's variant of it.
    Aapcs,
    /// Windows x64.
    Win64,
    /// The RISC-V LP64D psABI.
    Riscv,
}

/// The registers one call has left.
///
/// Made by [`TargetInfo::call`], asked about the return value first and then about each
/// argument in order. Asking out of order gives an answer for a different program.
#[derive(Debug)]
pub struct Call {
    /// Which psABI to follow.
    convention: Convention,
    /// General purpose argument registers left. On Windows x64 this is the argument positions
    /// left, since there the two kinds of register share them.
    gp: u32,
    /// Floating point argument registers left.
    fp: u32,
}

impl TargetInfo {
    /// The start of one call, with every argument register still to spend.
    #[must_use]
    pub fn call(&self) -> Call {
        let convention = match (self.triple.arch, self.triple.os) {
            (Arch::X86_64, Os::Windows) => Convention::Win64,
            (Arch::X86_64, _) => Convention::Sysv,
            // AArch64 on Windows is not one of the five ABIs `spec/12-abi-and-runtime.md`
            // implements for 1.0. It follows AAPCS64 here, which is most of what it does.
            (Arch::Aarch64, _) => Convention::Aapcs,
            (Arch::Riscv64, _) => Convention::Riscv,
        };
        let (gp, fp) = match convention {
            Convention::Sysv => (6, 8),
            Convention::Aapcs | Convention::Riscv => (8, 8),
            // rcx, rdx, r8 and r9, which an integer and a floating point argument share: the
            // fourth argument is in r9 or in xmm3 and never in both.
            Convention::Win64 => (4, 0),
        };
        Call { convention, gp, fp }
    }
}

impl Call {
    /// How the return value comes back, which is asked before anything else.
    ///
    /// A return value that comes back in memory takes an argument register with it on three of
    /// these four ABIs, so asking about the arguments first gives the wrong answer for the last
    /// one of them.
    #[must_use]
    pub fn returns(&mut self, arg: &Arg<'_>) -> Pass {
        match self.convention {
            Convention::Sysv => sysv::returns(self, arg),
            Convention::Aapcs => aapcs::returns(self, arg),
            Convention::Win64 => win64::returns(self, arg),
            Convention::Riscv => riscv::returns(self, arg),
        }
    }

    /// How the next argument travels, which spends whatever registers it takes.
    #[must_use]
    pub fn argument(&mut self, arg: &Arg<'_>) -> Pass {
        match self.convention {
            Convention::Sysv => sysv::argument(self, arg),
            Convention::Aapcs => aapcs::argument(self, arg),
            Convention::Win64 => win64::argument(self, arg),
            Convention::Riscv => riscv::argument(self, arg),
        }
    }
}

/// An aggregate of this size as a run of integer registers, the last one holding what is left.
fn integer_slots(size: u64) -> Vec<Slot> {
    (0..size.div_ceil(8))
        .map(|index| Slot::Integer {
            offset: index * 8,
            size: u32::try_from((size - index * 8).min(8)).unwrap_or(8),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Triple;

    /// The target with this triple.
    pub(super) fn target(triple: &str) -> TargetInfo {
        TargetInfo::new(triple.parse::<Triple>().expect("a triple the compiler supports"))
    }

    /// An integer scalar of this size, aligned to itself.
    pub(super) fn int(size: u64) -> Scalar {
        Scalar { kind: Kind::Integer, size, align: size }
    }

    /// A floating point scalar in this format, aligned to itself.
    pub(super) fn float(format: Format, size: u64) -> Scalar {
        Scalar { kind: Kind::Float(format), size, align: size }
    }

    /// An integer register holding this many bytes from this offset.
    pub(super) const fn gpr(offset: u64, size: u32) -> Slot {
        Slot::Integer { offset, size }
    }

    /// A vector register holding this format from this offset.
    pub(super) const fn fpr(offset: u64, format: Format) -> Slot {
        Slot::Float { offset, format }
    }

    /// The pieces of a record whose members are these, each at the next offset it fits.
    pub(super) fn packed(scalars: &[Scalar]) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let mut at: u64 = 0;
        for &scalar in scalars {
            at = at.next_multiple_of(scalar.align.max(1));
            pieces.push(Piece { offset: at, scalar });
            at += scalar.size;
        }
        pieces
    }

    /// The shape of a record whose members are these, sized and aligned the way C would.
    pub(super) fn record<'a>(pieces: &'a [Piece]) -> Shape<'a> {
        let align = pieces.iter().map(|piece| piece.scalar.align).max().unwrap_or(1);
        let size = pieces.iter().map(Piece::end).max().unwrap_or(0).next_multiple_of(align);
        Shape { size, align, pieces, complex: false }
    }

    #[test]
    fn the_last_register_of_an_aggregate_holds_only_what_is_left_of_it() {
        assert_eq!(integer_slots(4), vec![gpr(0, 4)]);
        assert_eq!(integer_slots(8), vec![gpr(0, 8)]);
        assert_eq!(integer_slots(12), vec![gpr(0, 8), gpr(8, 4)]);
        assert_eq!(integer_slots(16), vec![gpr(0, 8), gpr(8, 8)]);
    }

    #[test]
    fn a_triple_picks_the_abi_and_not_the_architecture_alone() {
        let mut linux = target("x86_64-unknown-linux-gnu").call();
        let mut windows = target("x86_64-pc-windows-msvc").call();
        let pieces = packed(&[int(8), int(8)]);
        let shape = Arg::Aggregate(record(&pieces));
        // Sixteen bytes is two registers on SysV and a hidden pointer on Windows, which is the
        // whole reason this is data about the target rather than a rule about C.
        assert_eq!(linux.argument(&shape), Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)]));
        assert_eq!(windows.argument(&shape), Pass::Reference);
    }
}
