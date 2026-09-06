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
//! Three rows, which are the three shapes the vocabulary has: a function with two ranges whose
//! extent is another argument, a function with one, and a function whose extent is discovered by
//! looking. Milestone S2 in `spec/safe-memory/16-milestones.md` asks for the table and the
//! generator first and the movement and string group second, and these three are what make the
//! generator something that has been run rather than something that has been written.
//!
//! The rest of section 10.3's movement group is the next box: `memmove`, `memcmp`, `strcpy`,
//! `strncpy`, `strcat`, `strncat`, `strcmp`, `strchr`, `strstr` and the `printf` family that writes
//! into a buffer. So is redirecting a call site to a wrapper, which is the compiler's half and is
//! why nothing calls these yet.
//!
//! # Why the symbol is not the name
//!
//! A wrapper is exported as `__rucc_wrap_memcpy` rather than as `memcpy`. Defining `memcpy` here
//! would take the name for the whole program including the C library's own internals, which is a
//! much larger decision than interposing the calls a program wrote, and it would be a recursion
//! rather than an interposition, because the wrapper's own body calls `memcpy` to do the work.
//! Section 10.3's rule is that an interposed function is one whose effects are written down as
//! judgements and not one that was replaced, and calling straight through is what it describes.

use core::ffi::{c_char, c_void};

use crate::interpose;

/// The C library's own, called once the judgements have been made.
///
/// Declared rather than written. A wrapper's job is the judgement, and a hand written `memcpy` that
/// is subtly slower or subtly wrong is a cost the boundary should not carry.
mod real {
    use core::ffi::{c_char, c_void};

    unsafe extern "C" {
        pub(super) fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
        pub(super) fn memset(dst: *mut c_void, byte: i32, n: usize) -> *mut c_void;
        pub(super) fn strlen(s: *const c_char) -> usize;
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

    /// `memset`, which is the same with one range instead of two.
    ///
    /// The row worth having early because it is the one every zeroing helper in every program goes
    /// through, and because a `memset` with a length taken from the wrong structure is a very
    /// ordinary way to write over a neighbour.
    fn memset(dst: *mut c_void, byte: i32, n: usize) -> *mut c_void
        where writes(dst, n)
    {
        // SAFETY: the range has been judged, as in `memcpy`.
        unsafe { real::memset(dst, byte, n) }
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

    #[test]
    fn every_row_describes_itself_the_way_it_was_written() {
        // The table and the wrappers come out of the same rows, so this is not checking that they
        // agree. It is checking that the generator read the clause the way the row meant it, which
        // is the thing a person writing the next hundred rows is relying on.
        assert_eq!(TABLE.len(), 3);

        let copy = TABLE[0];
        assert_eq!(copy.name, "memcpy");
        assert_eq!(copy.wrapper, "__rucc_wrap_memcpy");
        assert_eq!(copy.group, Group::Movement);
        assert_eq!(copy.effects.len(), 2);
        assert_eq!(copy.effects[0].arg, "dst");
        assert_eq!(copy.effects[0].kind, Kind::Writes);
        assert_eq!(copy.effects[0].extent, Extent::SizedBy("n"));
        assert_eq!(copy.effects[1].arg, "src");
        assert_eq!(copy.effects[1].kind, Kind::Reads);

        let scan = TABLE[2];
        assert_eq!(scan.name, "strlen");
        assert_eq!(scan.effects[0].extent, Extent::Nul);
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
}
