//! The psABIs, as descriptions read by one classifier rather than as one function per ABI.
//!
//! Design: `spec/cross-compile/06-abis.md`, and section 6.7 for the argument this crate exists to settle.
//!
//! # The problem
//!
//! An ABI is the only part of a target where being subtly wrong produces a program that works
//! for months and then does not. Everything else fails loudly: a bad encoding traps, a missing
//! relocation is a link error. A classification that is right for the first eight arguments and
//! wrong for the ninth produces garbage, on one target, in a function nobody changed.
//!
//! `spec/cross-compile/06-abis.md` section 6.1 lists fifteen of them. The compiler implements four by hand at
//! about a thousand lines, and fifteen at that rate is six to ten thousand lines of exactly that
//! kind of code, which is what `spec/cross-compile/02-the-goal.md` claim 3 is written against.
//!
//! # The shape of the answer
//!
//! An ABI is a [`AbiDescription`]: register banks, how a scalar spends them, and two ordered
//! lists of [`Rule`]s saying what an aggregate has to look like and how it travels if it does.
//! [`Call`] reads a description and answers, once per return value and once per argument in
//! order.
//!
//! The rules are not an interpreter for the psABI. The parts that are real algorithms stay as
//! code in [`classify`], and there are four of them across the five ABIs described here. What is
//! data is which of those an ABI uses, in what order, with what limits, and what happens when the
//! registers run out. [`describe`] argues for the split at length, including the case against it.
//!
//! # What is in here and what is not
//!
//! Argument classification and type layout. Not prologue emission, not register allocation
//! constraints, not unwind information, because section 6.7 keeps those as per architecture code
//! with per ABI parameters and says why: an abstraction over them costs more than the
//! duplication it removes.
//!
//! ```
//! use rucc_abi::{Arg, Pass, Scalar, Shape, Slot, abis};
//! use rucc_tuple::TargetTuple;
//!
//! let linux: TargetTuple = "x86_64-linux-gnu".parse().unwrap();
//! let windows: TargetTuple = "x86_64-pc-windows-msvc".parse().unwrap();
//!
//! // Two eight byte integers: two registers on SysV, a hidden pointer on Windows. Same C, same
//! // architecture, different answer, which is the whole reason this is a property of the target
//! // rather than a rule about C.
//! let pieces = rucc_abi::pieces(&[Scalar::integer(8), Scalar::integer(8)]);
//! let shape = Arg::Aggregate(Shape { size: 16, align: 8, pieces: &pieces, complex: false });
//!
//! let mut call = abis::for_target(linux).unwrap().call();
//! assert_eq!(
//!     call.argument(&shape),
//!     Pass::Pieces(vec![
//!         Slot::Integer { offset: 0, size: 8 },
//!         Slot::Integer { offset: 8, size: 8 },
//!     ])
//! );
//!
//! let mut call = abis::for_target(windows).unwrap().call();
//! assert_eq!(call.argument(&shape), Pass::Reference);
//! ```

#![doc(html_root_url = "https://docs.rs/rucc-abi/0.10.14")]
// Every public item here is read by somebody bringing up a target who has the psABI document
// open beside it, so an undocumented one is a question they have to answer by reading the body.
#![deny(missing_docs)]

pub mod abis;
pub mod classify;
pub mod describe;
pub mod layout;
pub mod shape;

pub use classify::Call;
pub use describe::{
    AbiDescription, Banks, ReturnPointer, Rule, Scalars, Short, StackArgs, Test, Travel, Variadic,
};
pub use layout::{BitfieldOrder, DataLayout, FloatType};
pub use shape::{Arg, Format, Kind, Pass, Piece, Scalar, Shape, Slot};

/// The pieces of a record whose members are these, each at the next offset it fits.
///
/// A convenience for building a shape in a test or a tool, and it is here rather than in a test
/// module because the command line tool needs the same thing and two copies of a layout rule is
/// two chances to write one of them differently.
#[must_use]
pub fn pieces(scalars: &[Scalar]) -> Vec<Piece> {
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
#[must_use]
pub fn record(pieces: &[Piece]) -> Shape<'_> {
    let align = pieces.iter().map(|piece| piece.scalar.align).max().unwrap_or(1);
    let size = pieces.iter().map(Piece::end).max().unwrap_or(0).next_multiple_of(align);
    Shape { size, align, pieces, complex: false }
}
