//! The interposition table, as rows.
//!
//! Design: `spec/safe-memory/10-boundaries.md` section 10.3.
//!
//! This file is data. Everything that decides what a row means is in [`crate::effects`], and
//! everything a row says is on the line that spells it, so getting one wrong is a data fix. That
//! is the whole reason the generator exists: section 10.3 wants several hundred of these
//! eventually, and several hundred hand written wrappers is several hundred chances to describe a
//! `memmove` slightly wrong and weaken the monitor in a way nobody notices.
//!
//! # What is here, and what is not
//!
//! Section 10.3's movement group: the `mem` functions and the `b` functions, whose extent is
//! another argument; the string functions that only look, whose extent is a terminator or a count;
//! and the string functions that copy, whose destination is written for as far as a different
//! argument turns out to reach.
//!
//! That last group is the one the whole design was for. `strcpy` has no length anywhere in its
//! signature, so the judgement is the walk: the source is read to its terminator while the
//! destination is judged at every granule the write reaches, and a destination too small is refused
//! at the byte that leaves it. That is document 03's S1 in the form it usually arrives in.
//!
//! The `printf` family is not here either, and it is further off. A wrapper for a variadic C
//! function has to be a variadic Rust function, and defining one is unstable, so `snprintf` has to
//! be reached through `vsnprintf` and a `va_list`, which is unstable as well. Reaching it from the
//! compiler side instead, by judging the destination at the call site where the format string is
//! often a literal anyway, is the option worth costing before either of those lands.
//!
//! What reaches these is `rucc_safety::wrap`, which points a call the program wrote to `memcpy` at
//! `__rucc_wrap_memcpy` instead. Without that half the wrappers are code nothing calls.
//!
//! # Why the symbol is not the name
//!
//! A wrapper is exported as `__rucc_wrap_memcpy` rather than as `memcpy`. Defining `memcpy` here
//! would take the name for the whole program including the C library's own internals, which is a
//! much larger decision than interposing the calls a program wrote, and it would be a recursion
//! rather than an interposition, because the wrapper's own body calls `memcpy` to do the work.
//! Section 10.3's rule is that an interposed function is one whose effects are written down as
//! judgements and not one that was replaced, and calling straight through is what it describes.

use core::ffi::{c_char, c_int, c_void};

use crate::interpose;

/// The C library's own, called once the judgements have been made.
///
/// Declared rather than written. A wrapper's job is the judgement, and a hand written `memcpy` that
/// is subtly slower or subtly wrong is a cost the boundary should not carry.
mod real {
    use core::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        pub(super) fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
        pub(super) fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
        pub(super) fn memset(dst: *mut c_void, byte: c_int, n: usize) -> *mut c_void;
        pub(super) fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int;
        pub(super) fn memchr(s: *const c_void, byte: c_int, n: usize) -> *mut c_void;
        pub(super) fn bcopy(src: *const c_void, dst: *mut c_void, n: usize);
        pub(super) fn bzero(dst: *mut c_void, n: usize);
        pub(super) fn strlen(s: *const c_char) -> usize;
        pub(super) fn strnlen(s: *const c_char, n: usize) -> usize;
        pub(super) fn strcmp(a: *const c_char, b: *const c_char) -> c_int;
        pub(super) fn strncmp(a: *const c_char, b: *const c_char, n: usize) -> c_int;
        pub(super) fn strchr(s: *const c_char, byte: c_int) -> *mut c_char;
        pub(super) fn strrchr(s: *const c_char, byte: c_int) -> *mut c_char;
        pub(super) fn strstr(haystack: *const c_char, needle: *const c_char) -> *mut c_char;
        pub(super) fn strcpy(dst: *mut c_char, src: *const c_char) -> *mut c_char;
        pub(super) fn stpcpy(dst: *mut c_char, src: *const c_char) -> *mut c_char;
        pub(super) fn strncpy(dst: *mut c_char, src: *const c_char, n: usize) -> *mut c_char;
        pub(super) fn strcat(dst: *mut c_char, src: *const c_char) -> *mut c_char;
        pub(super) fn strncat(dst: *mut c_char, src: *const c_char, n: usize) -> *mut c_char;
    }
}

interpose! {
    group: Movement;

    /// `memcpy`, which section 10.1 calls the archetype of modelling a boundary.
    ///
    /// Two ranges of the same length, one read and one written, and one judgement each. This is
    /// the row that says why interposing beats instrumenting: the copy itself is one call into the
    /// C library's own, and the checking is two comparisons rather than two per byte.
    fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void
        where writes(dst, n), reads(src, n)
    {
        // SAFETY: both ranges have been judged, so each is inside one live instance of this
        // monitor's heap or outside its heap entirely, and the caller's contract is what says the
        // second is as good as the C library would have found it.
        unsafe { real::memcpy(dst, src, n) }
    }

    /// `memmove`, which is `memcpy` with the overlap allowed.
    ///
    /// The same two judgements. Whether the ranges overlap is the C library's problem and not the
    /// monitor's: an overlap is defined behaviour here, and each range still has to be inside one
    /// live instance for the call to be one the program is allowed to make.
    fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void
        where writes(dst, n), reads(src, n)
    {
        // SAFETY: both ranges have been judged, as in `memcpy`.
        unsafe { real::memmove(dst, src, n) }
    }

    /// `memset`, which is the same with one range instead of two.
    ///
    /// The row worth having early because it is the one every zeroing helper in every program goes
    /// through, and because a `memset` with a length taken from the wrong structure is a very
    /// ordinary way to write over a neighbour.
    fn memset(dst: *mut c_void, byte: c_int, n: usize) -> *mut c_void
        where writes(dst, n)
    {
        // SAFETY: the range has been judged, as in `memcpy`.
        unsafe { real::memset(dst, byte, n) }
    }

    /// `memcmp`, which reads two ranges and writes neither.
    ///
    /// Worth having for the reason a read is worth judging at all: the answer leaves the function
    /// as one integer, and a comparison that ran off the end of one of its buffers is a comparison
    /// whose answer came partly from whatever was next to it.
    fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int
        where reads(a, n), reads(b, n)
    {
        // SAFETY: both ranges have been judged, as in `memcpy`.
        unsafe { real::memcmp(a, b, n) }
    }

    /// `memchr`, which reads until it finds the byte and is judged for the whole range anyway.
    ///
    /// The judgement is the range the call is allowed to read rather than the part of it the call
    /// turns out to read, which is stricter than the letter of what happens and is the right rule:
    /// a `memchr` handed a length longer than its buffer is a bug whether or not the byte it was
    /// looking for happened to turn up early enough to hide it.
    fn memchr(s: *const c_void, byte: c_int, n: usize) -> *mut c_void
        where reads(s, n)
    {
        // SAFETY: the range has been judged, as in `memcpy`.
        unsafe { real::memchr(s, byte, n) }
    }

    /// `bcopy`, which is `memmove` with its arguments the other way round.
    ///
    /// In the table because it is still called, and because the argument order is exactly the sort
    /// of thing a hand written wrapper gets backwards. Here the clause names the arguments, so the
    /// row cannot disagree with the signature above it.
    fn bcopy(src: *const c_void, dst: *mut c_void, n: usize) -> ()
        where reads(src, n), writes(dst, n)
    {
        // SAFETY: both ranges have been judged, as in `memcpy`.
        unsafe { real::bcopy(src, dst, n) }
    }

    /// `bzero`, which is `memset` with zero.
    fn bzero(dst: *mut c_void, n: usize) -> ()
        where writes(dst, n)
    {
        // SAFETY: the range has been judged, as in `memcpy`.
        unsafe { real::bzero(dst, n) }
    }

    /// `strlen`, which is the archetype of a discovered extent.
    ///
    /// There is no length argument to compare against, so the walk is the check: the string has to
    /// reach its NUL without leaving the object it started in. That is document 03's S8 and it is
    /// the shape of nearly every buffer overflow that has a CVE number.
    ///
    /// The string is walked twice, once by the judgement and once by the C library. That is a real
    /// cost and it is the honest version: the length the judgement found cannot be handed to the
    /// body without the effects clause binding a name, which is a change to the vocabulary rather
    /// than to this row, and it is worth making when the group it would speed up exists.
    fn strlen(s: *const c_char) -> usize
        where reads(s, nul)
    {
        // SAFETY: the string has been judged, so it reaches a NUL inside the instance it started
        // in, or it is not in this monitor's heap and the caller's contract is what covers it.
        unsafe { real::strlen(s) }
    }

    /// `strnlen`, which is the same walk with somewhere to stop.
    ///
    /// The row that shows why the bounded extent has to be in the vocabulary rather than judged as
    /// an unbounded one. `strnlen(field, 8)` over an eight byte field with eight characters in it
    /// is the whole reason `strnlen` exists, and a monitor that refused it would be telling the
    /// program to go back to `strlen`.
    fn strnlen(s: *const c_char, n: usize) -> usize
        where reads(s, nul, n)
    {
        // SAFETY: the walk has been judged as far as it goes, which is the terminator or `n`.
        unsafe { real::strnlen(s, n) }
    }

    /// `strcmp`, which walks two strings and is judged over both.
    ///
    /// It stops at the first byte that differs, so it usually reads far less than it is judged for.
    /// The judgement is still the whole walk, for the same reason `memchr`'s is: a string with no
    /// terminator inside its object is a bug that happens to have been survivable this time.
    fn strcmp(a: *const c_char, b: *const c_char) -> c_int
        where reads(a, nul), reads(b, nul)
    {
        // SAFETY: both strings have been judged, as in `strlen`.
        unsafe { real::strcmp(a, b) }
    }

    /// `strncmp`, which is the same with somewhere to stop.
    fn strncmp(a: *const c_char, b: *const c_char, n: usize) -> c_int
        where reads(a, nul, n), reads(b, nul, n)
    {
        // SAFETY: both walks have been judged as far as they go, as in `strnlen`.
        unsafe { real::strncmp(a, b, n) }
    }

    /// `strchr`, which is a walk that reports where it stopped.
    fn strchr(s: *const c_char, byte: c_int) -> *mut c_char
        where reads(s, nul)
    {
        // SAFETY: the string has been judged, as in `strlen`.
        unsafe { real::strchr(s, byte) }
    }

    /// `strrchr`, which is the same walk and cannot stop early.
    fn strrchr(s: *const c_char, byte: c_int) -> *mut c_char
        where reads(s, nul)
    {
        // SAFETY: the string has been judged, as in `strlen`.
        unsafe { real::strrchr(s, byte) }
    }

    /// `strstr`, which walks one string many times and the other once.
    ///
    /// Both are judged once, which is all that is needed: the search reads no byte of either that
    /// the single walk did not already reach.
    fn strstr(haystack: *const c_char, needle: *const c_char) -> *mut c_char
        where reads(haystack, nul), reads(needle, nul)
    {
        // SAFETY: both strings have been judged, as in `strlen`.
        unsafe { real::strstr(haystack, needle) }
    }

    /// `strcpy`, which is the row section 10.3 is really about.
    ///
    /// Nothing in the signature says how many bytes move. The source's terminator says, and it is
    /// found while the copy is happening, so a check made before the call would have nothing to
    /// compare. The clause is one judgement over the pair: the source is walked and the destination
    /// is judged for every granule the write is going to reach, so a destination one byte short is
    /// refused at that byte and the report names it.
    ///
    /// This is the function behind a large fraction of the buffer overflows that have a CVE number,
    /// which is why it is worth a judgement that reports well rather than one that merely refuses.
    fn strcpy(dst: *mut c_char, src: *const c_char) -> *mut c_char
        where copies(dst, src)
    {
        // SAFETY: the source reaches a terminator inside its own instance and the destination has
        // room for every byte of it, which is what the judgement establishes, or neither is in this
        // monitor's heap and the caller's contract is what covers them.
        unsafe { real::strcpy(dst, src) }
    }

    /// `stpcpy`, which is `strcpy` returning where it stopped.
    ///
    /// The same judgement. What the call does with the pointer it hands back is the program's own
    /// business, and the usual thing is another `stpcpy` from it, which is judged when it happens.
    fn stpcpy(dst: *mut c_char, src: *const c_char) -> *mut c_char
        where copies(dst, src)
    {
        // SAFETY: both have been judged, as in `strcpy`.
        unsafe { real::stpcpy(dst, src) }
    }

    /// `strncpy`, which has a length after all.
    ///
    /// The one row of the copying group the older vocabulary already reached, because `strncpy`
    /// writes exactly `n` bytes whatever the source holds. It pads with terminators rather than
    /// stopping, which is the part people forget, and it means the destination is judged for the
    /// whole of `n` rather than for the length of the source.
    ///
    /// The source is a bounded walk rather than an unbounded one, because a `strncpy` out of a
    /// fixed width field with no terminator in it is correct code and refusing it would be wrong.
    fn strncpy(dst: *mut c_char, src: *const c_char, n: usize) -> *mut c_char
        where writes(dst, n), reads(src, nul, n)
    {
        // SAFETY: the destination has room for `n` bytes and the source has been walked as far as
        // the call will read it, which is the terminator or `n`.
        unsafe { real::strncpy(dst, src, n) }
    }

    /// `strcat`, which discovers where to start as well as how far to go.
    ///
    /// Two discovered numbers rather than one. The destination's own terminator says where the
    /// write begins, so a destination with no terminator in it runs off the end before a byte of
    /// the source has been looked at, and that walk is judged first and reported on its own terms.
    ///
    /// Then the same pair walk as `strcpy`, from the end of what is already there. A buffer that
    /// had room for the first string and not for the second is the classic `strcat`, and the byte
    /// it is refused at is the byte where the total ran out.
    fn strcat(dst: *mut c_char, src: *const c_char) -> *mut c_char
        where appends(dst, src)
    {
        // SAFETY: the destination is terminated inside its own instance and has room for the source
        // after it, which is what the judgement establishes.
        unsafe { real::strcat(dst, src) }
    }

    /// `strncat`, which stops after `n` bytes of the source and terminates anyway.
    ///
    /// Not `strncpy`'s shape. The count bounds the source rather than the destination, and the
    /// terminator is written after those bytes, so what the destination has to hold is `n` plus one
    /// past where its own string ends. Writing the row as `writes(dst, n)` would be the commonest
    /// misreading of this function, and the clause is what keeps it out.
    fn strncat(dst: *mut c_char, src: *const c_char, n: usize) -> *mut c_char
        where appends(dst, src, n)
    {
        // SAFETY: as `strcat`, reading no more than `n` bytes of the source.
        unsafe { real::strncat(dst, src, n) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::effects::{Extent, Group, Kind};
    use crate::turnstile::turn;

    /// Runs a wrapper and says whether it refused, without the panic reaching the harness.
    fn refused(call: impl FnOnce()) -> bool {
        let hook = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
        std::panic::set_hook(hook);
        out.is_err()
    }

    /// The row a name was written as.
    fn row(name: &str) -> &'static crate::effects::Row {
        TABLE.iter().find(|row| row.name == name).expect("the row is in the table")
    }

    #[test]
    fn every_row_describes_itself_the_way_it_was_written() {
        // The table and the wrappers come out of the same rows, so this is not checking that they
        // agree. It is checking that the generator read the clause the way the row meant it, which
        // is the thing a person writing the next hundred rows is relying on.
        let copy = row("memcpy");
        assert_eq!(copy.wrapper, "__rucc_wrap_memcpy");
        assert_eq!(copy.group, Group::Movement);
        assert_eq!(copy.effects.len(), 2);
        assert_eq!(copy.effects[0].arg, "dst");
        assert_eq!(copy.effects[0].kind, Kind::Writes);
        assert_eq!(copy.effects[0].extent, Extent::SizedBy("n"));
        assert_eq!(copy.effects[1].arg, "src");
        assert_eq!(copy.effects[1].kind, Kind::Reads);

        assert_eq!(row("strlen").effects[0].extent, Extent::Nul);
        assert_eq!(row("strnlen").effects[0].extent, Extent::NulWithin("n"));

        // One clause, two effects, and the written one says whose length it borrowed.
        let copy = row("strcpy");
        assert_eq!(copy.effects.len(), 2);
        assert_eq!(copy.effects[0].kind, Kind::Writes);
        assert_eq!(copy.effects[0].extent, Extent::NulOf("src"));
        assert_eq!(copy.effects[1].arg, "src");
        assert_eq!(copy.effects[1].extent, Extent::Nul);

        // Three, because the destination is read to find where the write starts.
        let cat = row("strncat");
        assert_eq!(cat.effects.len(), 3);
        assert_eq!(cat.effects[0].kind, Kind::Reads);
        assert_eq!(cat.effects[0].extent, Extent::Nul);
        assert_eq!(cat.effects[1].extent, Extent::NulOfWithin("src", "n"));
        assert_eq!(cat.effects[2].extent, Extent::NulWithin("n"));

        // `strncat`'s count bounds its source and `strncpy`'s bounds its destination, which is the
        // difference between the two functions and the easiest thing in the group to write down
        // backwards.
        assert_eq!(row("strncpy").effects[0].extent, Extent::SizedBy("n"));

        // `bcopy` takes its source first, which is exactly the kind of thing a hand written wrapper
        // gets backwards, so the row is checked to have read the signature and not the habit.
        let legacy = row("bcopy");
        assert_eq!(legacy.effects[0].arg, "src");
        assert_eq!(legacy.effects[0].kind, Kind::Reads);
        assert_eq!(legacy.effects[1].arg, "dst");
        assert_eq!(legacy.effects[1].kind, Kind::Writes);
    }

    #[test]
    fn every_row_is_written_once_and_has_its_own_symbol() {
        // The table is going to be several hundred rows and two rows for one name would mean two
        // definitions of one symbol, which the linker would decide between rather than report.
        for (at, row) in TABLE.iter().enumerate() {
            assert!(
                !TABLE[..at].iter().any(|earlier| earlier.name == row.name),
                "{} is in the table twice",
                row.name
            );
            assert!(row.wrapper.starts_with("__rucc_wrap_"));
            assert!(row.wrapper.ends_with(row.name));
            assert!(!row.effects.is_empty(), "{} has no effects clause", row.name);
        }
    }

    #[test]
    fn a_copy_that_fits_moves_the_bytes_and_says_nothing() {
        let _turn = turn();
        // A wrapper that refused a correct call would be worse than no wrapper, and a wrapper that
        // checked correctly and then failed to do the work would be worse than that.
        let from = alloc(64);
        let to = alloc(64);
        // SAFETY: `from` is a live instance of sixty four bytes.
        unsafe { real::memset(from, 0x5A, 64) };
        // SAFETY: both are live instances of sixty four bytes.
        let out = unsafe { memcpy(to, from, 64) };
        assert_eq!(out, to);
        for offset in 0..64 {
            // SAFETY: inside a live instance.
            assert_eq!(unsafe { to.cast::<u8>().add(offset).read() }, 0x5A);
        }
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_copy_longer_than_its_destination_is_refused_before_it_writes_anything() {
        let _turn = turn();
        // The classic, and the reason the judgement comes first: the neighbour is checked rather
        // than repaired, so the bytes it holds are still its own when the report is printed.
        let from = alloc(128);
        let to = alloc(64);
        let after = alloc(64);
        // SAFETY: `after` is a live instance of sixty four bytes.
        unsafe { real::memset(after, 0x11, 64) };

        assert!(refused(|| {
            // SAFETY: the destination is judged before anything is written, which is what this is
            // for.
            let _ = unsafe { memcpy(to, from, 128) };
        }));
        for offset in 0..64 {
            // SAFETY: inside a live instance.
            assert_eq!(unsafe { after.cast::<u8>().add(offset).read() }, 0x11);
        }
        // SAFETY: all three are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
            dealloc(after);
        }
    }

    #[test]
    fn a_copy_reading_further_than_its_source_is_refused_as_well() {
        let _turn = turn();
        // The other half, which is the one that leaks rather than corrupts: a length taken from
        // the destination and applied to a shorter source is how a heap gets read into a response.
        let from = alloc(64);
        let to = alloc(128);
        assert!(refused(|| {
            // SAFETY: reading past the source is what is being judged.
            let _ = unsafe { memcpy(to, from, 128) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_set_longer_than_its_destination_is_refused() {
        let _turn = turn();
        let ptr = alloc(64);
        // SAFETY: a set of exactly the instance is what a correct program writes.
        assert_eq!(unsafe { memset(ptr, 0, 64) }, ptr);
        assert!(refused(|| {
            // SAFETY: one byte more is the bug.
            let _ = unsafe { memset(ptr, 0, 128) };
        }));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_length_of_a_string_that_is_terminated_is_the_length_and_nothing_else_happens() {
        let _turn = turn();
        let ptr = alloc(64);
        // SAFETY: a live instance with room for the text and its terminator.
        unsafe { real::memcpy(ptr, c"hello".as_ptr().cast(), 6) };
        // SAFETY: the string is terminated inside its instance.
        assert_eq!(unsafe { strlen(ptr.cast()) }, 5);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_bounded_walk_over_a_field_that_fills_its_buffer_is_allowed() {
        let _turn = turn();
        // The reason the bounded extent is in the vocabulary. A fixed width field with no room for
        // a terminator is the commonest thing `strnlen` and `strncmp` are called on, and refusing
        // it would be telling the program its correct code is a bug.
        let ptr = alloc(8);
        // SAFETY: eight bytes of a live instance, filled to the end.
        unsafe { real::memset(ptr, b'a'.into(), 8) };
        // SAFETY: the walk stops at eight, which is inside the instance.
        assert_eq!(unsafe { strnlen(ptr.cast(), 8) }, 8);
        // SAFETY: as above, over the same field.
        assert_eq!(unsafe { strncmp(ptr.cast(), ptr.cast(), 8) }, 0);
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_bounded_walk_that_is_given_more_than_its_buffer_holds_is_still_refused() {
        let _turn = turn();
        // The other half of it. The count says where the call stops and not where the object ends,
        // so a count longer than the object is the same bug it always was.
        //
        // A whole number of granules, because the plane holds one version per granule and an object
        // that ends in the middle of one shares it with whatever the allocator put after it. That
        // is the resolution document 04 section 4.3 chose and not something this test can work
        // around, so it asks about a walk that leaves the granule rather than one that leaves the
        // object.
        let ptr = alloc(64);
        // SAFETY: sixty four bytes of a live instance, filled to the end.
        unsafe { real::memset(ptr, b'a'.into(), 64) };
        assert!(refused(|| {
            // SAFETY: the walk is what is being judged, and it runs off the instance.
            let _ = unsafe { strnlen(ptr.cast(), 256) };
        }));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    #[test]
    fn a_comparison_that_runs_off_one_of_its_buffers_is_refused() {
        let _turn = turn();
        // A read rather than a write, which is the half that leaks. The answer comes back as one
        // integer, and a comparison that read past the end answered partly about its neighbour.
        let a = alloc(64);
        let b = alloc(64);
        assert!(refused(|| {
            // SAFETY: the length is longer than either instance, which is the bug.
            let _ = unsafe { memcmp(a, b, 128) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(a);
            dealloc(b);
        }
    }

    #[test]
    fn the_legacy_functions_are_judged_over_the_arguments_they_actually_name() {
        let _turn = turn();
        // `bcopy` takes its source first. A wrapper that judged the first argument as the
        // destination would let every overflowing `bcopy` through and refuse correct ones, which
        // is the failure mode the generated table exists to make impossible.
        let from = alloc(64);
        let to = alloc(128);
        // SAFETY: both are live instances and the copy fits in both.
        unsafe { bcopy(from, to, 64) };
        assert!(refused(|| {
            // SAFETY: the source is what runs out here, not the destination.
            unsafe { bcopy(from, to, 128) };
        }));
        // SAFETY: `to` is a live instance of a hundred and twenty eight bytes.
        unsafe { bzero(to, 128) };
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_length_of_a_string_that_runs_out_of_its_object_is_refused() {
        let _turn = turn();
        // The whole reason `strlen` is in the table. Without the judgement this walks into the
        // next block's header and returns a number that has nothing to do with the program.
        let ptr = alloc(64);
        // SAFETY: sixty four bytes of a live instance, filled without a terminator.
        unsafe { real::memset(ptr, b'a'.into(), 64) };
        assert!(refused(|| {
            // SAFETY: the walk is what is being judged.
            let _ = unsafe { strlen(ptr.cast()) };
        }));
        // SAFETY: `ptr` is a live instance.
        unsafe { dealloc(ptr) };
    }

    /// Puts a NUL terminated string at the start of an instance.
    fn put(ptr: *mut c_void, text: &core::ffi::CStr) {
        let bytes = text.to_bytes_with_nul();
        // SAFETY: every caller allocates room for the text and its terminator first.
        unsafe { real::memcpy(ptr, bytes.as_ptr().cast(), bytes.len()) };
    }

    #[test]
    fn a_copy_of_a_string_that_fits_moves_it_and_says_nothing() {
        let _turn = turn();
        // The silent case. A wrapper that refused this would be refusing the commonest line of C
        // there is.
        let from = alloc(16);
        let to = alloc(16);
        put(from, c"hello");
        // SAFETY: the destination has room for the string and its terminator.
        assert_eq!(unsafe { strcpy(to.cast(), from.cast()) }, to.cast());
        // SAFETY: the copy is terminated inside a live instance.
        assert_eq!(unsafe { strlen(to.cast()) }, 5);
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_copy_into_a_destination_with_no_room_for_it_is_refused() {
        let _turn = turn();
        // Document 03's S1 in the form it usually arrives in, and the case the whole discovered
        // extent exists for: there is no length in the signature to compare against, so the walk
        // has to be the check.
        let from = alloc(64);
        let to = alloc(16);
        put(from, c"a string that is longer than sixteen bytes");
        assert!(refused(|| {
            // SAFETY: the destination is judged before anything is written, which is the point.
            let _ = unsafe { strcpy(to.cast(), from.cast()) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_copy_of_a_string_that_has_no_terminator_is_refused_over_its_source() {
        let _turn = turn();
        // The other direction. Here the destination is enormous and the source is the problem, and
        // the report has to say so rather than blame the buffer that was big enough.
        let from = alloc(16);
        let to = alloc(1024);
        // SAFETY: sixteen bytes of a live instance, filled without a terminator.
        unsafe { real::memset(from, b'a'.into(), 16) };
        assert!(refused(|| {
            // SAFETY: the source walk is what is being judged.
            let _ = unsafe { strcpy(to.cast(), from.cast()) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_copy_that_fills_its_destination_exactly_leaves_no_room_for_the_terminator() {
        let _turn = turn();
        // The one byte overflow, which is the one that gets argued about. Sixteen characters into
        // sixteen bytes is not a copy that fits: `strcpy` writes a seventeenth byte, and the
        // seventeenth byte belongs to somebody else.
        let from = alloc(32);
        let to = alloc(16);
        put(from, c"sixteen bytes!!!");
        assert!(refused(|| {
            // SAFETY: the terminator is judged along with the bytes before it.
            let _ = unsafe { strcpy(to.cast(), from.cast()) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_bounded_copy_is_judged_over_the_bytes_it_pads_as_well() {
        let _turn = turn();
        // `strncpy` writes `n` bytes whatever the source holds, which is the part people forget. A
        // short string and a count longer than the destination is still an overflow, and it is
        // written entirely by the padding.
        let from = alloc(64);
        let to = alloc(16);
        put(from, c"hi");
        // SAFETY: sixteen into sixteen is the whole destination and no more.
        assert_eq!(unsafe { strncpy(to.cast(), from.cast(), 16) }, to.cast());
        assert!(refused(|| {
            // SAFETY: the destination is judged for the whole count.
            let _ = unsafe { strncpy(to.cast(), from.cast(), 64) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn an_append_that_runs_the_total_past_the_destination_is_refused() {
        let _turn = turn();
        // The classic `strcat`: each string fits, and the two of them together do not. Nothing at
        // the call site knows the total, which is why the judgement is made where the bytes are.
        let from = alloc(32);
        let to = alloc(16);
        put(from, c"world");
        put(to, c"hello ");
        // SAFETY: six and five and a terminator is twelve, which is inside sixteen.
        assert_eq!(unsafe { strcat(to.cast(), from.cast()) }, to.cast());
        // SAFETY: the result is terminated inside a live instance.
        assert_eq!(unsafe { strlen(to.cast()) }, 11);
        assert!(refused(|| {
            // SAFETY: appending it again is what runs past the end.
            let _ = unsafe { strcat(to.cast(), from.cast()) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn an_append_onto_a_destination_with_no_terminator_is_refused_before_the_source_is_read() {
        let _turn = turn();
        // The walk that finds where to start is a walk like any other. Without judging it, a
        // `strcat` onto an unterminated buffer would start writing at whatever address the search
        // for the terminator wandered to.
        let from = alloc(16);
        let to = alloc(16);
        put(from, c"x");
        // SAFETY: sixteen bytes of a live instance, filled without a terminator.
        unsafe { real::memset(to, b'a'.into(), 16) };
        assert!(refused(|| {
            // SAFETY: the destination's own walk is what is being judged.
            let _ = unsafe { strcat(to.cast(), from.cast()) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_bounded_append_stops_where_the_count_says_and_terminates_after_it() {
        let _turn = turn();
        // `strncat`'s count bounds the source, not the destination, and the terminator goes after
        // those bytes. Eleven characters plus five plus one is seventeen, which is one more than
        // the destination holds, and it is refused for that one byte.
        let from = alloc(64);
        let to = alloc(16);
        put(from, c"a long source string");
        put(to, c"hello ");
        // SAFETY: six and five and a terminator is inside sixteen.
        assert_eq!(unsafe { strncat(to.cast(), from.cast(), 5) }, to.cast());
        // SAFETY: the result is terminated inside a live instance.
        assert_eq!(unsafe { strlen(to.cast()) }, 11);
        assert!(refused(|| {
            // SAFETY: five more bytes and a terminator is what runs past the end.
            let _ = unsafe { strncat(to.cast(), from.cast(), 5) };
        }));
        // SAFETY: both are live instances.
        unsafe {
            dealloc(from);
            dealloc(to);
        }
    }

    #[test]
    fn a_string_that_is_not_the_heaps_is_copied_without_a_word() {
        let _turn = turn();
        // A literal into a local, which is a program doing nothing this monitor watches. Reporting
        // on one of these would be a false positive against correct code.
        let mut local = [0_u8; 32];
        // SAFETY: the destination is a live local with room, and the source is a literal.
        let out = unsafe { strcpy(local.as_mut_ptr().cast(), c"hello".as_ptr()) };
        assert_eq!(out, local.as_mut_ptr().cast());
        assert_eq!(&local[..5], b"hello");
    }
}
