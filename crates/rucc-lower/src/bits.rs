//! Where a bit-field is, and which accesses reach it.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.6.
//!
//! The record layout in `rucc_types` places a member at a bit offset from the start of its
//! record, so a bit-field is a run of bits that may start partway through a byte and end
//! partway through another. Nothing loads or stores bits, so a read of one is a load of the
//! bytes it lies in followed by a shift and a mask, and a write is that load with the run's
//! bits replaced and the bytes put back.
//!
//! Which bytes is the whole question. The rule the spec takes from C11's memory model is that
//! a store must not write a byte the run has no bit in, because the member next to it may be
//! an ordinary one and a program is allowed to write the two from two threads. So the access
//! is cut into pieces that between them cover the run's bytes and no other byte, which is what
//! [`Run::pieces`] does. A load may read a byte the run has no bit in, since reading one nobody
//! asked for is harmless, but it goes through the same pieces all the same: one rule is easier
//! to be sure of than two, and widening a load back out is work the optimizer is better at.
//!
//! The bit numbering is the layout's: bit zero is the low bit of the byte at the lowest
//! address. That is the little-endian order, and it is why a big-endian target is reported
//! rather than lowered.
//!
//! A record that carries `scalar_storage_order` naming the order the target does not have is the
//! one place that numbering is turned around, and [`Run::reverse`] is where it is turned. The
//! layout is untouched: the run lies in the same bytes and is the same width. What changes is that
//! the bits are counted from the top bit of the first of those bytes rather than from the bottom
//! bit, which is one statement covering both halves of what the attribute does. Every byte of the
//! record reads in the order it lies in memory, so a scalar member comes out byte swapped and a
//! bit-field comes out allocated from the other end of its storage unit. Measured against gcc
//! 16.2.0 on x86-64 over five shapes, which is where the one statement came from.

/// What is left of an alignment `offset` bytes further on.
///
/// The alignment of an address inside an object is what the offset leaves of the object's: a four
/// byte aligned record has a byte at offset six that is aligned to two, and one at offset zero that
/// is as aligned as the record is. Never less than one, because every address is aligned to one
/// byte and an alignment of zero is not a thing an access can be asked about.
pub(crate) fn shifted(base: u32, offset: u64) -> u32 {
    let align = if offset == 0 {
        base
    } else {
        // Capped at a shift the type can take, which is far above any alignment.
        base.min(1 << offset.trailing_zeros().min(16))
    };
    align.max(1)
}

/// A run of bits at an address, which is what a bit-field is once its record is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
    /// Which bit of the byte at the address the run starts at, from zero to seven.
    pub start: u32,
    /// How many bits the run is, which is the width the program declared.
    pub width: u32,
    /// What the address is known to be aligned to, in bytes.
    pub align: u32,
    /// Whether the bits are counted from the top of the run's bytes rather than from the bottom.
    ///
    /// What `scalar_storage_order` asks for, and false for every other run. It changes nothing
    /// about which bytes the run lies in or how wide it is, only where in them the run's bits are
    /// and which way round each piece of the access reads.
    pub reverse: bool,
}

impl Run {
    /// A run `offset` bytes after a base address that is aligned to `base`.
    ///
    /// The alignment of the run's own address is what the offset leaves of the base's: a
    /// four byte aligned record has a byte at offset six that is aligned to two.
    pub(crate) fn at(base: u32, offset: u64, start: u32, width: u32) -> Run {
        Run { start, width, align: shifted(base, offset), reverse: false }
    }

    /// The same run in a record whose scalars are stored in the reverse byte order.
    pub(crate) fn reversed(self, reverse: bool) -> Run {
        Run { reverse, ..self }
    }

    /// Where the run's bits sit in the integer its pieces are assembled in, from the low bit.
    ///
    /// The bit the layout wrote down for an ordinary run, since there the integer is assembled in
    /// the order the bytes lie in and the two numberings are the same one. Counted from the other
    /// end for a reversed run: the bits are the first `start` of the run's bytes in from the top,
    /// so what is under them is everything the run does not reach at the bottom.
    pub(crate) fn field(self) -> u32 {
        if self.reverse { self.bytes() * 8 - self.start - self.width } else { self.start }
    }

    /// How many bytes the run has a bit in.
    pub(crate) fn bytes(self) -> u32 {
        (self.start + self.width).div_ceil(8)
    }

    /// The width of the integer the pieces are put together in, in bits.
    ///
    /// Wide enough to hold every byte the run lies in, and a width the IR has, which is why a
    /// run over three bytes is assembled in thirty two bits rather than in twenty four. The
    /// pieces themselves stay eight bytes or under, since a run of nine bytes is split into an
    /// eight and a one, so nothing is loaded in a width a machine does not have.
    pub(crate) fn unit(self) -> u32 {
        (self.bytes() * 8).next_power_of_two()
    }

    /// Whether an access to the run can be built.
    ///
    /// A run of no bits is the zero width bit-field, which has no name and which nothing can
    /// read or write. Sixteen bytes is the widest integer the pieces are assembled in, which a
    /// run reaches only by being a bit-field of a hundred and twenty one bits or more that
    /// packing has pushed off a byte boundary, and the widest bit-field any type here has room
    /// for is the hundred and twenty eight of an `__int128`.
    pub(crate) fn accessible(self) -> bool {
        self.width > 0 && self.bytes() <= 16
    }

    /// The accesses that between them cover the run's bytes and no other byte.
    ///
    /// Each piece is a power of two bytes wide and starts at a multiple of its own width, so
    /// each is an access a machine has, and the widest comes first. A run over three bytes is
    /// two bytes and then one, and one over eight bytes is a single eight byte access.
    pub(crate) fn pieces(self) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let (mut at, mut left) = (0, self.bytes());
        let (start, end) = (self.field(), self.field() + self.width);
        while left > 0 {
            let size = if left.is_power_of_two() { left } else { left.next_power_of_two() / 2 };
            // Where this piece's bytes land in the assembled integer. The bytes of a reversed run
            // read from the top of it, so the piece nearest the run's address is the highest one
            // and the piece after it is the one below.
            let shift = if self.reverse { (self.bytes() - at - size) * 8 } else { at * 8 };
            pieces.push(Piece {
                offset: u64::from(at),
                size,
                align: self.align.min(size),
                shift,
                from: start.max(shift),
                to: end.min(shift + size * 8),
            });
            at += size;
            left -= size;
        }
        pieces
    }
}

/// One load or store of the bytes a run lies in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Piece {
    /// Where it starts, in bytes from the run's address.
    pub offset: u64,
    /// How wide it is, in bytes.
    pub size: u32,
    /// What its address is aligned to, in bytes.
    pub align: u32,
    /// Where its bytes sit in the integer the run is assembled in, as a shift from the low bit.
    ///
    /// The offset in bits for an ordinary run, and the offset counted from the other end of the
    /// run's bytes for a reversed one.
    pub shift: u32,
    /// The first of the run's bits it holds, counted in the assembled integer.
    pub from: u32,
    /// One past the last of the run's bits it holds, counted the same way.
    pub to: u32,
}

impl Piece {
    /// The bits of the run this piece holds, as a mask of the piece's own width.
    ///
    /// Of the piece as the assembled integer holds it, which for a reversed run is the piece with
    /// its bytes the other way round from how they lie in memory. That is the same way round as
    /// the value a load of it is turned into and a store of it is built from, so the mask lines up
    /// with what it is applied to.
    pub(crate) fn mask(self) -> u128 {
        let width = self.to - self.from;
        let ones = if width >= 128 { u128::MAX } else { (1u128 << width) - 1 };
        ones << (self.from - self.shift)
    }

    /// Whether the piece is the run's bits and nothing else, so a store into it needs no load
    /// of what was there first.
    pub(crate) fn whole(self) -> bool {
        self.from == self.shift && self.to == self.shift + self.size * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_inside_one_byte_is_one_byte_wide_access() {
        let run = Run { start: 3, width: 4, align: 4, reverse: false };
        assert_eq!(run.bytes(), 1);
        assert_eq!(run.unit(), 8);
        let pieces = run.pieces();
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0], Piece { offset: 0, size: 1, align: 1, shift: 0, from: 3, to: 7 });
        assert_eq!(pieces[0].mask(), 0b0111_1000);
        assert!(!pieces[0].whole());
    }

    #[test]
    fn a_run_that_fills_its_bytes_needs_no_load_before_a_store() {
        let run = Run { start: 0, width: 32, align: 4, reverse: false };
        let pieces = run.pieces();
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0], Piece { offset: 0, size: 4, align: 4, shift: 0, from: 0, to: 32 });
        assert!(pieces[0].whole());
        assert_eq!(pieces[0].mask(), 0xffff_ffff);
    }

    #[test]
    fn a_run_over_three_bytes_is_two_accesses_and_neither_reaches_the_fourth() {
        // `struct { int a : 24; char c; }`, where a store to `a` that took four bytes would
        // write over `c` and the memory model says it may not.
        let run = Run { start: 0, width: 24, align: 4, reverse: false };
        assert_eq!(run.bytes(), 3);
        assert_eq!(run.unit(), 32);
        let pieces = run.pieces();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0], Piece { offset: 0, size: 2, align: 2, shift: 0, from: 0, to: 16 });
        assert_eq!(pieces[1], Piece { offset: 2, size: 1, align: 1, shift: 16, from: 16, to: 24 });
        assert!(pieces.iter().all(|piece| piece.whole()));
    }

    #[test]
    fn a_packed_run_is_covered_by_pieces_that_start_where_they_can_be_addressed() {
        // A field of thirty two bits at bit one, which `packed` can arrange.
        let run = Run { start: 1, width: 32, align: 1, reverse: false };
        assert_eq!(run.bytes(), 5);
        assert_eq!(run.unit(), 64);
        let pieces = run.pieces();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0], Piece { offset: 0, size: 4, align: 1, shift: 0, from: 1, to: 32 });
        assert_eq!(pieces[1], Piece { offset: 4, size: 1, align: 1, shift: 32, from: 32, to: 33 });
        assert_eq!(pieces[1].mask(), 0b1);
        assert!(!pieces[1].whole());
    }

    #[test]
    fn the_pieces_cover_every_bit_of_the_run_and_nothing_outside_its_bytes() {
        for start in 0..8 {
            for width in 1..=128 {
                for reverse in [false, true] {
                    let run = Run { start, width, align: 8, reverse };
                    if !run.accessible() {
                        continue;
                    }
                    let pieces = run.pieces();
                    let covered: u32 = pieces.iter().map(|piece| piece.to - piece.from).sum();
                    assert_eq!(covered, width, "{run:?}");
                    let bytes: u32 = pieces.iter().map(|piece| piece.size).sum();
                    assert_eq!(bytes, run.bytes(), "{run:?}");
                    for piece in pieces {
                        assert!(piece.size.is_power_of_two(), "{piece:?}");
                        assert_eq!(piece.offset % u64::from(piece.size), 0, "{piece:?}");
                        assert!(piece.from < piece.to, "{piece:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_reversed_run_lies_in_the_same_bytes_and_is_read_from_the_other_end_of_them() {
        // `struct { unsigned f0 : 3; unsigned f1 : 5; unsigned f2 : 24; }` with the attribute on
        // it, where gcc 16.2.0 puts `{ 1, 2, 3 }` in as the bytes 22 00 00 03. `f0` is the top
        // three bits of the first byte, which is the run being read from the other end.
        let run = Run { start: 0, width: 3, align: 4, reverse: true };
        assert_eq!(run.bytes(), 1);
        assert_eq!(run.field(), 5);
        let pieces = run.pieces();
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0], Piece { offset: 0, size: 1, align: 1, shift: 0, from: 5, to: 8 });
        assert_eq!(pieces[0].mask(), 0b1110_0000);
    }

    #[test]
    fn the_bytes_of_a_reversed_run_are_assembled_from_the_top_down() {
        // The first of `unsigned long long f0 : 29, f1 : 4, f2 : 31` with the attribute on it,
        // which is `execute/pr86659-1.c`. The run is four bytes and the field is the top
        // twenty nine bits of them, so the byte at the run's address is the highest of the four.
        let run = Run { start: 0, width: 29, align: 8, reverse: true };
        assert_eq!(run.bytes(), 4);
        assert_eq!(run.field(), 3);
        let pieces = run.pieces();
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0], Piece { offset: 0, size: 4, align: 4, shift: 0, from: 3, to: 32 });
        assert!(!pieces[0].whole());

        // The same run cut into pieces, which `#pragma pack` is the way to ask for: three bytes
        // is a two byte access and then a one byte access, and the two byte one holds the top of
        // the field because it is the one at the run's address.
        let run = Run { start: 0, width: 24, align: 1, reverse: true };
        let pieces = run.pieces();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0], Piece { offset: 0, size: 2, align: 1, shift: 8, from: 8, to: 24 });
        assert_eq!(pieces[1], Piece { offset: 2, size: 1, align: 1, shift: 0, from: 0, to: 8 });
        assert!(pieces.iter().all(|piece| piece.whole()));
    }

    #[test]
    fn a_run_wider_than_the_widest_access_is_not_one_this_builds() {
        assert!(Run { start: 0, width: 64, align: 8, reverse: false }.accessible());
        // `#pragma pack(1)` over a `long long z : 63` after eighteen bits of other fields,
        // which is tcc's `95_bitfields.c` and which lies in eleven bytes.
        assert!(Run { start: 2, width: 63, align: 1, reverse: false }.accessible());
        assert!(!Run { start: 1, width: 128, align: 1, reverse: false }.accessible());
        assert!(!Run { start: 0, width: 0, align: 4, reverse: false }.accessible());
    }

    #[test]
    fn a_run_over_more_than_eight_bytes_is_assembled_wide_and_read_in_pieces_that_are_not() {
        let run = Run { start: 2, width: 63, align: 1, reverse: false };
        assert_eq!(run.bytes(), 9);
        assert_eq!(run.unit(), 128);
        let pieces = run.pieces();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0], Piece { offset: 0, size: 8, align: 1, shift: 0, from: 2, to: 64 });
        assert_eq!(pieces[1], Piece { offset: 8, size: 1, align: 1, shift: 64, from: 64, to: 65 });
        assert!(pieces.iter().all(|piece| piece.size <= 8));
    }

    #[test]
    fn an_offset_leaves_a_base_alignment_with_what_it_divides_by() {
        assert_eq!(Run::at(4, 0, 0, 3).align, 4);
        assert_eq!(Run::at(4, 2, 0, 3).align, 2);
        assert_eq!(Run::at(4, 3, 0, 3).align, 1);
        assert_eq!(Run::at(16, 8, 0, 3).align, 8);
        assert_eq!(Run::at(1, 8, 0, 3).align, 1);
    }
}
