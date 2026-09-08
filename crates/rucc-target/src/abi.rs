//! How an argument travels and how a return value comes back, which is the target's answer
//! and never C's.
//!
//! Design: `spec/12-abi-and-runtime.md` sections 12.1 to 12.5, and
//! `spec/cross-compile/06-abis.md` sections 6.2 and 6.7.
//!
//! The same declaration passes a pair of registers on one target and a hidden pointer on
//! another, so this is the one question about a C function that cannot be answered by reading
//! the C. It used to be answered here, by four hand written classifiers covering the four ABIs
//! this compiler had backends for. It is answered by [`rucc_abi`] now, where the same five ABIs
//! are data rather than code and are checked against a reference compiler, and this module is
//! the door between the compiler and that crate.
//!
//! # What is asked and what is answered
//!
//! A caller flattens a C type into a [`Shape`], which is a size, an alignment and the scalars
//! inside it with the offsets the layout gave them. That is everything every psABI reads: the
//! classification rules are all written over where the scalars are and whether they are integers
//! or floating point. Flattening is the caller's job because it is where the C type system
//! lives, and every rule after it is the target's.
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

use rucc_abi::abis;

#[doc(inline)]
pub use rucc_abi::{Arg, Call, Kind, Pass, Piece, Scalar, Shape, Slot};

use crate::TargetInfo;

impl TargetInfo {
    /// The start of one call, with every argument register still to spend, and [`None`] on a
    /// target whose ABI is not described.
    ///
    /// The [`None`] is AArch64 on Windows and nothing else today. That ABI is AAPCS64 with a
    /// different variadic rule and x18 reserved, per `spec/cross-compile/06-abis.md` section 6.1,
    /// and it is not written down yet. This used to answer AAPCS64 for it, which is the almost
    /// right answer, and an almost right ABI is the failure mode `spec/cross-compile/02-the-goal.md`
    /// exists to rule out: everything builds, everything links, and a structure crosses a
    /// library boundary with its members in the wrong registers. A caller that cannot compile
    /// the call says so instead.
    #[must_use]
    pub fn call(&self) -> Option<Call> {
        abis::for_target(self.triple.tuple()).map(rucc_abi::AbiDescription::call)
    }
}

#[cfg(test)]
mod tests {
    use rucc_abi::Format;

    use super::*;
    use crate::Triple;

    /// The target with this triple.
    fn target(triple: &str) -> TargetInfo {
        TargetInfo::new(triple.parse::<Triple>().expect("a triple the compiler supports"))
    }

    /// The pieces of a record whose members are these, each at the next offset it fits.
    fn packed(scalars: &[Scalar]) -> Vec<Piece> {
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
    fn record<'a>(pieces: &'a [Piece]) -> Shape<'a> {
        let align = pieces.iter().map(|piece| piece.scalar.align).max().unwrap_or(1);
        let size = pieces.iter().map(Piece::end).max().unwrap_or(0).next_multiple_of(align);
        Shape { size, align, pieces, complex: false }
    }

    #[test]
    fn a_triple_picks_the_abi_and_not_the_architecture_alone() {
        let mut linux = target("x86_64-unknown-linux-gnu").call().expect("a described ABI");
        let mut windows = target("x86_64-pc-windows-msvc").call().expect("a described ABI");
        let pieces = packed(&[Scalar::integer(8), Scalar::integer(8)]);
        let shape = Arg::Aggregate(record(&pieces));
        // Sixteen bytes is two registers on SysV and a hidden pointer on Windows, which is the
        // whole reason this is data about the target rather than a rule about C.
        assert_eq!(
            linux.argument(&shape),
            Pass::Pieces(vec![
                Slot::Integer { offset: 0, size: 8 },
                Slot::Integer { offset: 8, size: 8 },
            ])
        );
        assert_eq!(windows.argument(&shape), Pass::Reference);
    }

    #[test]
    fn a_homogeneous_floating_point_aggregate_travels_in_vector_registers() {
        let mut call = target("aarch64-unknown-linux-gnu").call().expect("a described ABI");
        let pieces = packed(&[Scalar::float(Format::Single, 4); 3]);
        let shape = Arg::Aggregate(record(&pieces));
        // Three `float`s are three vector registers on AAPCS64, and adding anything that is not
        // a `float` makes the whole thing an eightbyte pair in general purpose registers.
        assert_eq!(
            call.argument(&shape),
            Pass::Pieces(vec![
                Slot::Float { offset: 0, format: Format::Single },
                Slot::Float { offset: 4, format: Format::Single },
                Slot::Float { offset: 8, format: Format::Single },
            ])
        );
    }

    #[test]
    fn the_two_darwin_targets_follow_different_abis() {
        // Apple's arm64 is its own description rather than AAPCS64 with a note, because its
        // variadic rule and its `long double` both differ. x86-64 Darwin is plain SysV.
        let arm = target("aarch64-apple-darwin").call().expect("a described ABI");
        let intel = target("x86_64-apple-darwin").call().expect("a described ABI");
        assert_eq!(arm.abi().name, "Darwin arm64");
        assert_eq!(intel.abi().name, "SysV AMD64");
    }

    #[test]
    fn aarch64_on_windows_has_no_answer_rather_than_an_almost_right_one() {
        // The one target the compiler can name and cannot classify a call for. It is AAPCS64
        // with a different variadic rule and x18 reserved, and until that is written down
        // saying so is the only honest answer.
        assert!(target("aarch64-pc-windows-msvc").call().is_none());
        assert!(target("aarch64-unknown-linux-gnu").call().is_some());
        assert!(target("x86_64-pc-windows-msvc").call().is_some());
    }

    #[test]
    fn every_other_triple_the_compiler_accepts_can_classify_a_call() {
        use crate::{Arch, Env, Os};

        let mut described = 0;
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64] {
            for os in [Os::Linux, Os::Darwin, Os::Windows, Os::None] {
                for env in [Env::None, Env::Gnu, Env::Musl, Env::Msvc] {
                    let target = TargetInfo::new(Triple { arch, os, env });
                    // AArch64 on Windows is the only gap, and it is one gap rather than a family
                    // of them: a target with no operating system still has a calling convention,
                    // because a freestanding program calls functions.
                    let expected = !(arch == Arch::Aarch64 && os == Os::Windows);
                    assert_eq!(
                        target.call().is_some(),
                        expected,
                        "{arch:?} {os:?} {env:?} disagrees about whether its ABI is described"
                    );
                    described += usize::from(target.call().is_some());
                }
            }
        }
        // Forty eight combinations the triple can hold, less the four spellings of AArch64 on
        // Windows, which are one gap rather than four because the environment does not reach the
        // choice of ABI on that pair.
        assert_eq!(described, 44);
    }
}
