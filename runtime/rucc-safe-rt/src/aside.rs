//! The capabilities of pointers stored into the stack and into static storage, which have no aux.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.2.2, and tamnd/rucc#3269.
//!
//! [`crate::aux_slot`] only has room for a capability beside a word the allocator laid out, so a
//! pointer stored into a local or a global used to arrive at its next load with nothing written
//! down, and the load recovered it from the address. For a pointer into the heap that is exact,
//! because the heap's planes find an object from any address inside it. For a pointer to a local or
//! a global it is not, since no plane covers either, and the recovery answers with bounds over
//! everything. That is the case of a buffer on the stack whose address is put in a pointer
//! variable, and the variable's address handed to a function in another file, which reads the
//! pointer back out and writes past the end of the buffer.
//!
//! This is the place those capabilities go instead: a table per thread, kept beside the thread's
//! stack plane in [`crate::stack`]'s mapping, with one entry per word picked by the word's address.
//! An entry says which word it is about, which pointer was stored there and what the capability of
//! that pointer was. A store that has no slot writes one and a load that has no slot reads one.
//!
//! # Why a table and not the frame region the spec describes
//!
//! Section 5.2.2 puts the aux of a frame inside the frame, at a fixed offset from the frame pointer.
//! That is the right answer for the function that owns the frame and no answer at all for anybody
//! else: the function in the other file has a pointer to the word and no idea which frame it is
//! in. A table indexed by the word's address answers the same question from either side, and it
//! covers static storage too, which the section gives a linker section of its own that does not
//! exist.
//!
//! # Why an entry can be believed
//!
//! An entry is believed only when the word it names is the one being loaded from and the pointer it
//! names is the one that was loaded. Anything that wrote the word without this crate seeing it, a
//! store of an integer, a copy, code built without the flag, either left a different value in it or
//! left the same pointer, so the first test throws out everything that changed the word to
//! something else.
//!
//! What is left is the same pointer written again, and that is only safe to believe if the pointer
//! still means the same object. For static storage it always does, since a global's object is the
//! global for the life of the program. For a local it does while the frame is running, and the
//! capability already says how to ask that: its version names the frame's witness, and
//! [`crate::witness::gone`] is the question. An entry for a local whose frame has returned is thrown
//! out at the load rather than handed on, because the pointer in the word may be a new pointer with
//! the same value into a later frame, and a capability that called it dangling would be a refusal of
//! a correct program. An entry for a local whose block was left while its frame runs on is handed
//! on, because the witness still holds the serial it was made with and so the pointer can only be
//! that same local after its lifetime, which `check_live` then refuses as row T4. Throwing that one
//! out too had the load recover bounds over everything, and a read through a union or a pointer to
//! a pointer of a local from a closed block was let through (tamnd/rucc#3269).
//!
//! Only capabilities that can be believed this way are kept. A recovered one or a wide one says
//! nothing the recovery at the load would not say again, a heap one is better answered by the heap's
//! planes, which find the object from the address exactly, and a local's is kept only when its
//! witness is on this thread's stack. Reading a witness on another thread's stack could be reading
//! a stack that was unmapped when the thread exited, and that is a fault on a load the program made
//! correctly.
//!
//! # What it gets wrong, and which way
//!
//! Every way is a capability that is not there, which is the recovery this replaced. The table has
//! a fixed number of entries and two words with the same index push each other out. A pointer
//! stored on one thread and loaded on another finds nothing, since each thread has its own table.
//! And a copy that is too long to walk word by word clears what it covers rather than moving it.
//!
//! The one way it could say too much is a word a copy wrote, which is why `moved` is called for
//! every copy into storage with no aux. Without it a structure copied over a word that still had an
//! entry would leave the entry in place, and the same pointer value copied in would be believed
//! with whatever the entry said about it.

use crate::layout::{Cap, Class, Meta};

/// How many entries a thread keeps, a power of two so that picking one is a mask.
const ENTRIES: usize = 4096;

/// The bytes of a word, which is what an entry is about and the grid the index is taken on.
const WORD: u64 = 8;

/// One word with no aux, the pointer stored into it and that pointer's capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct Entry {
    /// The address of the word, or zero for an entry that says nothing.
    at: u64,
    /// The pointer the word was given.
    value: u64,
    /// Its capability.
    cap: Cap,
}

impl Entry {
    /// What a slot nothing wrote reads as, which is what a fresh mapping is.
    const EMPTY: Self = Self { at: 0, value: 0, cap: Cap::BOTTOM };
}

/// How many bytes [`crate::stack`] sets aside for the table, which it does only where it maps a
/// plane at all.
#[cfg(target_os = "linux")]
pub(crate) const BYTES: usize = ENTRIES * size_of::<Entry>();

/// The entry for the word at `at`.
const fn index(at: u64) -> usize {
    (at / WORD) as usize & (ENTRIES - 1)
}

/// This thread's table, made now when `make` says so and it is not there yet.
fn table(make: bool) -> Option<(*mut Entry, usize, usize)> {
    let (at, lo, hi) = crate::stack::aside(make)?;
    Some((at.cast::<Entry>(), lo, hi))
}

/// Whether `cap` is one an entry can carry, for the reasons the module comment gives.
///
/// `lo` and `hi` are this thread's stack, which a local's witness has to be on.
fn worth(cap: &Cap, lo: usize, hi: usize) -> bool {
    if !cap.is_named() || cap.meta.flags() & (Meta::RECOVERED | Meta::WIDE) != 0 {
        return false;
    }
    match cap.meta.class() {
        class if class == Class::Static as u8 || class == Class::Literal as u8 => true,
        class if class == Class::Automatic as u8 => {
            crate::witness::word(cap.ver).is_some_and(|word| lo <= word && word < hi)
        }
        _ => false,
    }
}

/// Writes down that the word at `at` was given the pointer `value`, whose capability is `cap`.
///
/// For a word [`crate::aux_slot`] has no slot for. A capability this cannot carry still empties the
/// word's entry if it has one, because the word now holds a pointer that entry is not about, and an
/// entry left behind would be believed the moment the old value came back.
pub(crate) fn keep(at: u64, value: u64, cap: Cap) {
    let make = at != 0 && value != 0;
    let Some((entries, lo, hi)) = table(make) else { return };
    // SAFETY: the index is inside the table, which is `ENTRIES` entries this thread owns.
    let entry = unsafe { entries.add(index(at)) };
    let wanted = make && worth(&cap, lo, hi);
    // SAFETY: as above.
    let held = unsafe { entry.read() };
    if wanted {
        // SAFETY: as above.
        unsafe { entry.write(Entry { at, value, cap }) };
    } else if held.at == at {
        // SAFETY: as above.
        unsafe { entry.write(Entry::EMPTY) };
    }
}

/// The capability of the pointer `value`, loaded from the word at `at`, if one was written down.
pub(crate) fn found(at: u64, value: u64) -> Option<Cap> {
    if at == 0 || value == 0 {
        return None;
    }
    let (entries, _, _) = table(false)?;
    // SAFETY: the index is inside the table, which is `ENTRIES` entries this thread owns.
    let entry = unsafe { entries.add(index(at)).read() };
    if entry.at != at || entry.value != value {
        return None;
    }
    // SAFETY: a local's capability is only kept when its witness is on this thread's stack, which
    // stays mapped for as long as the thread that is asking runs.
    if unsafe { crate::witness::gone(&entry.cap) && !crate::witness::left(&entry.cap) } {
        return None;
    }
    Some(entry.cap.beside(value))
}

/// A copy of `len` bytes from `src` to `dst` has happened, into storage with no aux.
///
/// Every whole destination word gets the entry its source word had, moved over to the
/// destination's address, and every other destination word that had one loses it. The source's
/// entries say which pointer they were about, so one that moves with a word the copy did not
/// really carry is still thrown out at the load.
///
/// A copy longer than the table is cleared rather than moved, by a walk over the table instead of
/// over the copy, so that a megabyte copied on the stack does not cost a lookup per word.
pub(crate) fn moved(dst: u64, src: u64, len: u64) {
    if len == 0 {
        return;
    }
    let Some((entries, _, _)) = table(false) else { return };
    let first = dst & !(WORD - 1);
    let end = dst.saturating_add(len);
    let words = end.saturating_sub(first).div_ceil(WORD);
    if words > ENTRIES as u64 {
        for at in 0..ENTRIES {
            // SAFETY: the index is inside the table, which is `ENTRIES` entries this thread owns.
            let entry = unsafe { entries.add(at) };
            // SAFETY: as above.
            let held = unsafe { entry.read() };
            if held.at >= first && held.at < end {
                // SAFETY: as above.
                unsafe { entry.write(Entry::EMPTY) };
            }
        }
        return;
    }
    // Walked from the end when the destination is above an overlapping source, the way `memmove`
    // walks, so that no source entry is overwritten before it is read.
    let backwards = dst > src;
    for step in 0..words {
        let word = if backwards { words - 1 - step } else { step };
        let at = first + word * WORD;
        let whole = at >= dst && at + WORD <= end;
        let from = src.wrapping_add(at - dst);
        // SAFETY: both indices are inside the table, which is `ENTRIES` entries this thread owns.
        let (to, source) = unsafe { (entries.add(index(at)), entries.add(index(from)).read()) };
        // SAFETY: as above.
        let held = unsafe { to.read() };
        if whole && source.at == from && from != 0 {
            // SAFETY: as above.
            unsafe { to.write(Entry { at, ..source }) };
        } else if held.at == at {
            // SAFETY: as above.
            unsafe { to.write(Entry::EMPTY) };
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::layout::perm;

    /// The capability `__rucc_cap_static` makes for a global of `size` bytes at `lo`.
    fn global(lo: u64, size: u64) -> Cap {
        crate::recover::object(lo as *const core::ffi::c_void, size as usize, Class::Static)
    }

    /// Two words of a local, whose addresses the table is indexed by.
    fn words() -> (u64, u64) {
        let local = [0_u64; 2];
        let at = core::hint::black_box(local.as_ptr()) as u64;
        (at, at + WORD)
    }

    #[test]
    fn a_pointer_to_a_global_stored_on_the_stack_comes_back_with_its_bounds() {
        let (word, _) = words();
        let cap = global(0x1000_0000, 40);
        keep(word, 0x1000_0008, cap);
        let back = found(word, 0x1000_0008).expect("the entry was kept");
        assert_eq!((back.lo, back.ext), (0x1000_0000, 40));
        assert!(back.is_named());
    }

    #[test]
    fn an_entry_is_not_believed_for_another_word_or_another_pointer() {
        let (word, next) = words();
        keep(word, 0x1000_0008, global(0x1000_0000, 40));
        assert_eq!(found(word, 0x1000_0010), None);
        assert_eq!(found(next, 0x1000_0008), None);
    }

    #[test]
    fn a_capability_with_nothing_to_say_empties_the_entry_rather_than_leaving_it() {
        let (word, _) = words();
        keep(word, 0x1000_0008, global(0x1000_0000, 40));
        let meta = Meta::new(Class::Static, perm::READ, 0).with_flags(Meta::NAMED | Meta::WIDE);
        let wide = Cap::new(0, u64::MAX, 7, meta);
        keep(word, 0x1000_0008, wide);
        assert_eq!(found(word, 0x1000_0008), None);
    }

    #[test]
    fn a_heap_capability_is_left_to_the_heap() {
        let (word, _) = words();
        let meta = Meta::new(Class::Allocated, perm::READ, 0).with_flags(Meta::NAMED);
        keep(word, 0x1000_0008, Cap::new(0x1000_0000, 40, 6, meta));
        assert_eq!(found(word, 0x1000_0008), None);
    }

    #[test]
    fn a_local_is_believed_while_its_frame_runs_and_not_after() {
        let (word, _) = words();
        let mut witness = 0_u64;
        let buffer = [0_u8; 10];
        let base = core::hint::black_box(buffer.as_ptr());
        // SAFETY: the witness is a word of this test's frame, and so is the buffer.
        let cap = unsafe {
            crate::witness::open(&raw mut witness);
            crate::recover::local(base.cast(), 10, &raw const witness)
        };
        keep(word, base as u64, cap);
        let back = found(word, base as u64).expect("the frame is still running");
        assert_eq!((back.lo, back.ext), (base as u64, 10));
        // SAFETY: as above.
        unsafe { crate::witness::close(&raw mut witness) };
        assert_eq!(found(word, base as u64), None);
    }

    #[test]
    fn a_local_of_a_block_left_while_its_frame_runs_comes_back_to_be_refused() {
        let (word, _) = words();
        let mut witness = 0_u64;
        let local = 0_u8;
        let base = core::hint::black_box(&raw const local);
        // SAFETY: the witness is a word of this test's frame, and so is the local.
        let cap = unsafe {
            crate::witness::open(&raw mut witness);
            crate::recover::local(base.cast(), 1, &raw const witness)
        };
        keep(word, base as u64, cap);
        // SAFETY: as above.
        unsafe { crate::witness::shut(&raw mut witness) };
        let back = found(word, base as u64).expect("the frame still runs, so it is that local");
        assert_eq!((back.lo, back.ext), (base as u64, 1));
        // SAFETY: as above.
        assert!(unsafe { crate::witness::gone(&back) });
        // SAFETY: as above.
        unsafe { crate::witness::close(&raw mut witness) };
        assert_eq!(found(word, base as u64), None);
    }

    #[test]
    fn a_copy_moves_an_entry_with_its_word_and_clears_what_it_wrote_over() {
        let words = [0_u64; 4];
        let at = core::hint::black_box(words.as_ptr()) as u64;
        let cap = global(0x1000_0000, 40);
        keep(at, 0x1000_0008, cap);
        keep(at + 3 * WORD, 0x1000_0010, cap);
        moved(at + 2 * WORD, at, 2 * WORD);
        let back = found(at + 2 * WORD, 0x1000_0008).expect("moved with the word");
        assert_eq!((back.lo, back.ext), (0x1000_0000, 40));
        assert_eq!(found(at + 3 * WORD, 0x1000_0010), None);
        assert!(found(at, 0x1000_0008).is_some());
    }

    #[test]
    fn a_copy_too_long_to_walk_clears_what_it_covers() {
        let words = [0_u64; 2];
        let at = core::hint::black_box(words.as_ptr()) as u64;
        keep(at, 0x1000_0008, global(0x1000_0000, 40));
        moved(at, 0x2000_0000, (ENTRIES as u64 + 1) * WORD);
        assert_eq!(found(at, 0x1000_0008), None);
    }
}
