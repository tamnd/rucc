//! Typed 32-bit indices.
//!
//! The compiler stores its trees, its IR and its machine code in flat vectors and refers to
//! elements by index rather than by pointer. A `u32` index is half the size of a pointer,
//! it is stable across a reallocation of the backing vector, and it serialises without
//! fixups, all of which matter at the sizes a translation unit reaches.
//!
//! The cost of a bare `u32` is that every index in the program has the same type, so a block
//! number can be passed where an instruction number was wanted and the compiler will not
//! object. `Idx<T>` is the fix: it is still a `u32` at runtime and it is a distinct type at
//! compile time.

use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU32;

/// A 32-bit index into a flat table of `T`.
///
/// `Idx<T>` is `Copy`, is exactly four bytes, and has a niche, so `Option<Idx<T>>` is also
/// four bytes. Optional indices are everywhere in a compiler (no successor, no parent, no
/// spill slot) and paying eight bytes for each of them adds up, so the value is stored
/// biased by one over a `NonZeroU32`. That is where the limit of one below `u32::MAX` on the
/// largest representable index comes from.
pub struct Idx<T> {
    raw: NonZeroU32,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Idx<T> {
    /// The largest index that can be represented.
    pub const MAX: u32 = u32::MAX - 1;

    /// Wraps a raw index.
    ///
    /// # Panics
    ///
    /// Panics if `raw` exceeds [`Idx::MAX`]. A translation unit with four billion of
    /// anything is not a translation unit we intend to compile, and the alternative to
    /// panicking is silently truncating, which is worse.
    #[inline]
    pub const fn new(raw: u32) -> Self {
        assert!(raw <= Self::MAX, "index out of range");
        match NonZeroU32::new(raw + 1) {
            Some(raw) => Self { raw, _marker: PhantomData },
            None => unreachable!(),
        }
    }

    /// Wraps a `usize`, which is what indexing a `Vec` gives back.
    ///
    /// # Panics
    ///
    /// Panics if the value exceeds [`Idx::MAX`].
    #[inline]
    pub fn from_usize(raw: usize) -> Self {
        Self::new(u32::try_from(raw).expect("index out of range"))
    }

    /// The underlying `u32`.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.raw.get() - 1
    }

    /// The index as a `usize`, for slicing.
    #[inline]
    pub const fn index(self) -> usize {
        self.raw() as usize
    }
}

// The derives would all demand `T: Trait`, which is wrong here: an index is four bytes of
// integer no matter what it points at, and requiring `T: Clone` to copy an index is a papercut
// that shows up in every signature. So they are written out.
impl<T> Clone for Idx<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Idx<T> {}

impl<T> PartialEq for Idx<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T> Eq for Idx<T> {}

impl<T> PartialOrd for Idx<T> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Idx<T> {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl<T> std::hash::Hash for Idx<T> {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<T> fmt::Debug for Idx<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The type name is worth the width: a dump full of bare integers is unreadable, and
        // dumps are the primary debugging tool for a compiler.
        let name = std::any::type_name::<T>();
        let short = name.rsplit("::").next().unwrap_or(name);
        write!(f, "{short}#{}", self.raw())
    }
}

/// A contiguous half-open run of indices, `start .. end`.
///
/// Children of an AST node, arguments of a call and parameters of a block are all stored as
/// runs in one flat vector, so the parent holds eight bytes instead of a `Vec`.
pub struct IdxRange<T> {
    start: u32,
    end: u32,
    _marker: PhantomData<fn() -> T>,
}

// Written out for the same reason as the ones on `Idx`: a range of indices is eight bytes of
// integer whatever it points at, and a derive would demand `T: Clone` to copy one.
impl<T> Clone for IdxRange<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for IdxRange<T> {}

impl<T> PartialEq for IdxRange<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.start == other.start && self.end == other.end
    }
}

impl<T> Eq for IdxRange<T> {}

impl<T> fmt::Debug for IdxRange<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = std::any::type_name::<T>();
        let short = name.rsplit("::").next().unwrap_or(name);
        write!(f, "{short}#{}..{}", self.start, self.end)
    }
}

impl<T> IdxRange<T> {
    /// The empty range at the start of the table.
    ///
    /// Every table has a start, so this is valid whatever the table holds and whether or not
    /// anything has been put in it yet. It is what a node holds when the thing it points at is
    /// a list that happens to have nothing in it: a declarator with no derivations, a call with
    /// no arguments, a declaration with no attributes.
    pub const EMPTY: Self = Self { start: 0, end: 0, _marker: PhantomData };

    /// Builds a range.
    ///
    /// # Panics
    ///
    /// Panics if `end` is before `start`.
    #[inline]
    pub fn new(start: Idx<T>, end: Idx<T>) -> Self {
        assert!(start.raw() <= end.raw(), "reversed index range");
        Self { start: start.raw(), end: end.raw(), _marker: PhantomData }
    }

    /// The empty range at `at`.
    #[inline]
    pub fn empty_at(at: Idx<T>) -> Self {
        Self { start: at.raw(), end: at.raw(), _marker: PhantomData }
    }

    /// How many indices the range covers.
    #[inline]
    pub const fn len(self) -> usize {
        (self.end - self.start) as usize
    }

    /// Whether the range covers nothing.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The indices in the range, in order.
    pub fn iter(self) -> impl Iterator<Item = Idx<T>> {
        (self.start..self.end).map(Idx::new)
    }

    /// The range as a `usize` range, for slicing the backing vector.
    #[inline]
    pub const fn as_usize_range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Block;
    struct Inst;

    #[test]
    fn an_index_is_four_bytes_and_so_is_an_optional_one() {
        assert_eq!(size_of::<Idx<Block>>(), 4);
        assert_eq!(size_of::<Option<Idx<Block>>>(), 4);
    }

    #[test]
    fn round_trips_through_usize() {
        let i = Idx::<Inst>::from_usize(7);
        assert_eq!(i.index(), 7);
        assert_eq!(i.raw(), 7);
    }

    #[test]
    fn debug_names_the_table() {
        assert_eq!(format!("{:?}", Idx::<Block>::new(3)), "Block#3");
    }

    #[test]
    fn a_range_iterates_half_open() {
        let r = IdxRange::new(Idx::<Inst>::new(2), Idx::<Inst>::new(5));
        let got: Vec<u32> = r.iter().map(Idx::raw).collect();
        assert_eq!(got, vec![2, 3, 4]);
        assert_eq!(r.len(), 3);
        assert_eq!(r.as_usize_range(), 2..5);
    }

    #[test]
    fn an_empty_range_is_empty() {
        let r = IdxRange::empty_at(Idx::<Inst>::new(9));
        assert!(r.is_empty());
        assert_eq!(r.iter().count(), 0);
    }

    #[test]
    fn the_empty_range_slices_an_empty_table() {
        let r = IdxRange::<Inst>::EMPTY;
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        let table: Vec<u8> = Vec::new();
        assert!(table[r.as_usize_range()].is_empty());
    }

    #[test]
    #[should_panic(expected = "reversed index range")]
    fn a_reversed_range_is_rejected() {
        let _ = IdxRange::new(Idx::<Inst>::new(5), Idx::<Inst>::new(2));
    }

    #[test]
    #[should_panic(expected = "index out of range")]
    fn the_niche_value_is_rejected() {
        let _ = Idx::<Inst>::new(u32::MAX);
    }
}
