//! The two ends of a capability in memory, which document 06 section 6.2.2 calls `cap_store` and
//! `cap_load`.
//!
//! [`crate::aux_slot`] is the format and the arithmetic and knows nothing about the rest of the
//! runtime. This is the policy over it: what a store does when there is nowhere to write the
//! capability down, and what a load answers when the slot cannot say the whole thing by itself.
//! Both of those are questions about the planes and the allocator, so they are here and not there.
//!
//! # What a load answers
//!
//! Four cases and they are all different.
//!
//! The slot says the whole capability, which is every object shorter than two megabytes, and the
//! answer is what it says. No version check happens here. Document 08 section 8.3 asks for one,
//! against the recycled aux slot of a freed object, and `check_live` is where it lands: generated
//! code checks the capability's version against the plane before any access through it, so a slot
//! that outlived its object produces a capability that is refused at its first use. Doing the same
//! compare again inside the load would be a second region lookup on the hot path for a hole that is
//! already closed.
//!
//! The slot says to ask the header, which is an object of two megabytes or more. The version and
//! the meta bits are in the slot, the bounds come out of the planes, and the plane's version has to
//! be the one the slot holds or the answer is bottom. That last part is the check the case above
//! does not need: here the bounds are being taken from whatever instance owns the address now, so
//! believing them without checking would be believing a different object's extent.
//!
//! The slot says the word holds no pointer. That is section 5.2.2's class Y1 and the answer is
//! bottom, which refuses the first access through it. It is also what a word written by code this
//! build did not compile looks like, and those two want opposite answers, which is tamnd/rucc#1081.
//!
//! The word has no slot at all, which today is every local and every global, since only the
//! allocator lays out an aux. The answer is a recovery from the address, the same one a pointer
//! that crossed the boundary gets, and it is counted with those: a capability that was never
//! written down is exactly the situation document 10 section 10.1 is about, whether it got that way
//! by crossing a boundary or by being stored somewhere with no aux to store it in.
//!
//! # What a store does with nowhere to write
//!
//! Nothing, and it says so. A word with no slot is the same three cases
//! [`crate::aux_slot::address_of`] reports, and the capability is dropped. The load beside it
//! recovers, so the pair stays honest, and no judgement is made at the store: a store of a pointer
//! into a local is not a fault.
//!
//! # The torn store
//!
//! [`pair`] and [`torn`] are document 03's C1, which section 9.5 of
//! `spec/safe-memory/09-type-init-and-races.md` specifies. A pointer in memory is two things
//! written by two stores, the word and the capability in the slot in front of it, and two threads
//! storing pointers into one word can leave one thread's pointer beside the other thread's
//! capability. Fil-C accepts that pairing and is memory safe under it, because the capability is a
//! real one with real bounds. It is still the program following a pointer to an object it never had
//! a pointer to.
//!
//! Both halves are stamped in the epoch plane rather than in the slot, at the word's address and at
//! the slot's own address, which the plane already covers because it is reserved over a whole
//! watched region and a block is the aux, then the header, then the payload. [`crate::epoch::torn`]
//! is the comparison and it is an equality: one store writes both stamps with one clock value, so
//! any difference at all is two stores.
//!
//! Both are calls of their own beside a `cap_store` and a `cap_load` rather than something folded
//! into [`store()`] and [`load()`], and the reason is that they are two flags. The aux plane is what
//! makes a capability survive a trip through memory and `-fsafety-races` is what asks about threads,
//! and a program that wanted the first should not pay a plane write per pointer store for the
//! second. Section 9.5's cost is written against a program that asked for race detection, which is
//! what keeps that honest.

use core::ffi::c_void;

use crate::alloc;
use crate::aux_slot::{self, Read};
use crate::epoch;
use crate::fail::Descriptor;
use crate::layout::{Cap, Meta};
use crate::plane::Version;
use crate::recover;

/// Writes the capability of the pointer `value` into the slot beside the word at `at`.
///
/// `dest` is the capability of the object the word is in, which is the one generated code's own
/// bounds check for this store used. False when the word has no slot and the capability went
/// nowhere.
///
/// # Safety
///
/// `dest` describes a live instance this runtime's allocator laid out, which is what makes the aux
/// in front of it the runtime's storage rather than the program's. `at` and `value` are never read
/// through, so either may be any value at all.
pub unsafe fn store(dest: Cap, at: *const c_void, value: *const c_void, cap: Cap) -> bool {
    // SAFETY: the caller's contract is the one `aux_slot::store` asks for, passed straight on.
    unsafe { aux_slot::store(dest, at as u64, value as u64, cap) }
}

/// The capability of the pointer `value`, which was loaded from the word at `at`.
///
/// `dest` is the capability of the object the word is in, as in [`store()`]. The four answers are
/// the four cases in the module comment.
///
/// # Safety
///
/// As [`store()`].
#[must_use]
pub unsafe fn load(dest: Cap, at: *const c_void, value: *const c_void) -> Cap {
    // SAFETY: as `store`.
    let Some(read) = (unsafe { aux_slot::load(dest, at as u64, value as u64) }) else {
        return recover::recover(value);
    };
    match read {
        Read::Nothing => Cap::BOTTOM,
        Read::Whole(cap) => cap,
        Read::Header { ver, meta } => header(ver, meta, value),
    }
}

/// The capability of an object too long for its slot to describe.
///
/// The slot kept the version and the meta bits, so what is missing is where the object starts and
/// how far it runs, and the planes have both. Bottom when the instance the address is in now is not
/// the instance the slot was written about, which is the one thing this case has to rule out.
fn header(ver: Version, meta: Meta, value: *const c_void) -> Cap {
    let addr = value as usize;
    let Some(region) = alloc::covering(addr) else {
        return Cap::BOTTOM;
    };
    // SAFETY: the region is the one covering this address, so its plane is built over it.
    let version = unsafe { region.plane.version(addr) };
    if version != ver {
        return Cap::BOTTOM;
    }
    match recover::extent(&region, addr) {
        Some((lo, ext)) => Cap::new(lo as u64, ext as u64, ver, meta),
        None => Cap::BOTTOM,
    }
}

/// The recording half of judgement C1: this thread wrote the word at `at` and the capability beside
/// it, both of them in one store.
///
/// One stamp written twice, which is the whole of the mechanism. Two halves that came from one
/// store carry the same stamp because there was one call of [`crate::epoch::tick`] between them,
/// and two halves that came from two stores cannot, because a thread's clock only goes up and two
/// threads never share a number.
///
/// It is what a pointer store emits in place of `__rucc_meta_epoch`, not as well as it. A second
/// call would take a second stamp and overwrite the word's half of the pair with a clock the slot's
/// half does not have, which reads as a tear on the very store that wrote both.
///
/// A word with no slot is stamped anyway and nothing else happens. That is [`crate::aux_slot`]'s
/// three cases, none of them is a fault, and the word is still a pointer word another thread can
/// race on, so leaving it unstamped would give up C2 and C3 to buy nothing.
///
/// A thread with nowhere to keep a clock stamps nothing, for the reason `crate::check::stamped`
/// gives: writing [`crate::epoch::NONE`] would erase what another thread had honestly recorded.
///
/// # Safety
///
/// As [`store()`].
pub unsafe fn pair(dest: Cap, at: *const c_void) {
    let word = at as usize;
    let Some(region) = alloc::covering(word) else { return };
    let stamp = epoch::tick();
    if stamp == epoch::NONE {
        return;
    }
    // SAFETY: the address is inside the region, whose epoch plane covers every byte of it.
    unsafe { region.epochs.write(word, stamp) };
    let Some(slot) = aux_slot::address_of(dest, word as u64) else { return };
    // SAFETY: the slot is in the aux of the block the word is in, the block is inside the region
    // the word is in, and the plane is reserved over the whole of that region rather than over its
    // payloads. `crate::alloc` has a test on that geometry, since this is what depends on it.
    unsafe { region.epochs.write(slot as usize, stamp) };
}

/// Judgement C1: the pointer word at `at` and the capability beside it came from one store.
///
/// The reading half, and it is an equality rather than an ordering. There is nothing for an
/// ordering to say here: both stores were correct on their own thread, both halves are real, and
/// what is wrong is that the reader has one of each. So the report names the two stores rather than
/// a writer and a reader, which is what `crate::report::Witness::Tear` is for.
///
/// A word with no slot passes, which is [`crate::aux_slot`]'s three cases and is the second half of
/// what [`pair`] does nothing about. So does a half the plane never watched, since
/// [`crate::epoch::torn`] takes [`crate::epoch::NONE`] on either side as the plane having nothing to
/// say rather than as a disagreement.
///
/// The version half of section 9.5's C1 bullet is not here. A slot that outlived its object is
/// refused by `crate::check::live` against the lifetime plane before any access through the
/// capability, and for an object too long for its slot [`load()`] compares the version itself. This
/// is the half that needs the epoch plane.
///
/// # Panics
///
/// Under the abort posture, as every judgement does.
///
/// # Safety
///
/// As [`store()`], and `descriptor` is one this build emitted or null.
pub unsafe fn torn(dest: Cap, at: *const c_void, descriptor: *const Descriptor) {
    let word = at as usize;
    let Some(region) = alloc::covering(word) else { return };
    let Some(slot) = aux_slot::address_of(dest, word as u64) else { return };
    // SAFETY: both addresses are inside the region, for the reason `pair` gives about the second.
    let (written, paired) =
        unsafe { (region.epochs.read(word), region.epochs.read(slot as usize)) };
    if epoch::torn(written, paired) {
        // SAFETY: the caller's descriptor, and neither stamp is an address.
        unsafe {
            crate::fail::report_witness(
                descriptor,
                word,
                crate::report::Witness::Tear(written, paired),
            );
        }
    }
}

/// The names generated code is compiled against.
///
/// Separate from the functions above for the reason every other module's exports are: these are an
/// ABI and those are Rust.
pub mod exports {
    use core::ffi::c_void;

    use crate::fail::Descriptor;
    use crate::layout::Cap;

    /// Document 06 section 6.2.2's `cap_store`.
    ///
    /// Both capabilities come in through pointers rather than by value, for the reason
    /// `__rucc_cap_recover` writes its answer through one: a capability is four words, the call
    /// convention for a structure that size is a hidden pointer anyway, and saying so in the
    /// signature lets the backend hand over the slot it already has the capability in.
    ///
    /// # Safety
    ///
    /// `dest` and `cap` are readable, aligned [`Cap`] sized slots. `dest` describes a live instance
    /// this runtime's allocator laid out. `at` and `value` are never read through.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_cap_store(
        dest: *const Cap,
        at: *const c_void,
        value: *const c_void,
        cap: *const Cap,
    ) {
        // SAFETY: the caller's slots, which the contract above says are readable and aligned.
        let (dest, cap) = unsafe { (dest.read(), cap.read()) };
        // SAFETY: as above, and the rest of the contract is the one `store` asks for.
        let _ = unsafe { super::store(dest, at, value, cap) };
    }

    /// Document 06 section 6.2.2's `cap_load`, writing its answer through `out`.
    ///
    /// # Safety
    ///
    /// As [`__rucc_cap_store`], and `out` is a writable, aligned [`Cap`] sized slot.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_cap_load(
        out: *mut Cap,
        dest: *const Cap,
        at: *const c_void,
        value: *const c_void,
    ) {
        // SAFETY: the caller's slot, which the contract above says is readable and aligned.
        let dest = unsafe { dest.read() };
        // SAFETY: as above, and the rest of the contract is the one `load` asks for.
        let cap = unsafe { super::load(dest, at, value) };
        // SAFETY: the caller's slot, which the contract above says is writable and aligned.
        unsafe { out.write(cap) }
    }

    /// The recording half of document 03's C1, which a pointer store emits in place of
    /// `__rucc_meta_epoch`.
    ///
    /// No size, unlike `__rucc_meta_epoch`, because the thing being stamped is a pointer word and a
    /// pointer word is exactly one granule of the plane.
    ///
    /// # Safety
    ///
    /// As [`__rucc_cap_store`], and `at` is the word the capability was written beside.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_epoch_pair(dest: *const Cap, at: *const c_void) {
        // SAFETY: the caller's slot, which the contract above says is readable and aligned.
        let dest = unsafe { dest.read() };
        // SAFETY: as above, and the rest of the contract is the one `pair` asks for.
        unsafe { super::pair(dest, at) }
    }

    /// Document 03's C1, the reading half.
    ///
    /// Document 06 section 6.2.2 writes this as `check_race %c, %p` together with C3, and here it
    /// is a symbol of its own beside `__rucc_check_race`. The two ask different questions of the
    /// same plane and one of them needs the container's capability, which is a value the backend
    /// does not yet understand, so they are fused once tamnd/rucc#1085 is done rather than now.
    ///
    /// # Safety
    ///
    /// As [`__rucc_cap_store`], and `descriptor` is one this build emitted or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_check_torn(
        dest: *const Cap,
        at: *const c_void,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: the caller's slot, which the contract above says is readable and aligned.
        let dest = unsafe { dest.read() };
        // SAFETY: as above, and the rest of the contract is the one `torn` asks for.
        unsafe { super::torn(dest, at, descriptor) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::layout::{Class, perm};
    use crate::turnstile::turn;

    /// The capability of a whole instance the allocator just handed back.
    ///
    /// Read out of the header in front of the payload, which is what `cap_of` does and what
    /// generated code would have in hand at a store into this object.
    fn of(ptr: *mut c_void) -> Cap {
        let addr = ptr as usize;
        let region = alloc::covering(addr).expect("the allocator's own storage is watched");
        let (lo, ext) = recover::extent(&region, addr).expect("a live instance owns it");
        // SAFETY: the region is the one covering the address.
        let ver = unsafe { region.plane.version(addr) };
        Cap::new(
            lo as u64,
            ext as u64,
            ver,
            Meta::new(Class::Allocated, perm::READ | perm::WRITE, 0),
        )
    }

    /// The address of the word `offset` bytes into an instance.
    fn at(ptr: *mut c_void, offset: usize) -> *const c_void {
        ptr.cast::<u8>().wrapping_add(offset).cast()
    }

    /// A real descriptor, because that is what generated code passes and the reporter reads it.
    static ROW: Descriptor = Descriptor { judgement: 9, class: 0, size: 8, pc: 0 };

    /// The C1 check, with the descriptor argument filled in.
    fn tear(dest: Cap, word: *const c_void) {
        // SAFETY: the address of a `static`, which is what a descriptor is at run time too, and the
        // capability of an instance the caller owns.
        unsafe { torn(dest, word, &raw const ROW) }
    }

    /// What the epoch plane holds for the granule at `addr`.
    fn stamp_at(addr: *const c_void) -> epoch::Stamp {
        let addr = addr as usize;
        let region = alloc::covering(addr).expect("the instance is in a watched region");
        // SAFETY: the address is one an instance in the test owns, so the plane covers it.
        unsafe { region.epochs.read(addr) }
    }

    /// Puts a stamp in the plane as though some other store had written the granule at `addr`.
    ///
    /// Really running two threads would test less rather than more. What C1 compares is two stamps,
    /// and a thread this one spawns and joins is one the join has ordered, so the interesting stamp
    /// is one that has to be placed rather than earned.
    fn written_by(addr: *const c_void, stamp: epoch::Stamp) {
        let addr = addr as usize;
        let region = alloc::covering(addr).expect("the instance is in a watched region");
        // SAFETY: as above.
        unsafe { region.epochs.write(addr, stamp) }
    }

    /// Runs one check and says whether it refused, without the panic reaching the harness.
    fn refused(check: impl FnOnce()) -> bool {
        let hook = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
        std::panic::set_hook(hook);
        out.is_err()
    }

    #[test]
    fn one_store_leaves_one_stamp_on_both_halves_of_the_pointer() {
        let _turn = turn();
        // The whole of the mechanism. The word and the slot in front of it are stamped by one call,
        // so they hold the same stamp, and that is what makes a difference mean two stores.
        let holder = alloc(64);
        let pointee = alloc(128);
        let word = at(holder, 16);
        let slot = aux_slot::address_of(of(holder), word as u64).expect("the word has a slot");

        // SAFETY: both are instances this test owns, and neither address is read through.
        unsafe {
            assert!(store(of(holder), word, pointee, of(pointee)));
            pair(of(holder), word);
        }
        let written = stamp_at(word);
        assert_ne!(written, epoch::NONE, "this thread wrote the word");
        assert_eq!(stamp_at(slot as *const c_void), written, "and the capability beside it");
        assert!(!refused(|| tear(of(holder), word)));

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(holder);
            dealloc(pointee);
        }
    }

    #[test]
    fn a_pointer_from_one_store_beside_a_capability_from_another_is_reported() {
        let _turn = turn();
        // Document 03's C1. Both halves are real and each was written by a thread that did nothing
        // wrong, and what is wrong is that the reader has one of each.
        let holder = alloc(64);
        let pointee = alloc(128);
        let word = at(holder, 16);

        // SAFETY: instances this test owns, and neither address is read through.
        unsafe {
            assert!(store(of(holder), word, pointee, of(pointee)));
            pair(of(holder), word);
        }
        // The word rewritten by somebody else, which is the pointer half arriving from a store the
        // capability half did not come from.
        written_by(word, epoch::stamp(epoch::thread(stamp_at(word)) + 1, 1));
        assert!(refused(|| tear(of(holder), word)));

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(holder);
            dealloc(pointee);
        }
    }

    #[test]
    fn a_second_stamping_of_one_word_is_not_a_tear() {
        let _turn = turn();
        // A thread storing twice through the same word writes both halves both times, so the pair
        // is whatever the later store left. This is the case a `__rucc_meta_epoch` beside the pair
        // would break, and it is why the pair replaces that call rather than joining it.
        let holder = alloc(64);
        let word = at(holder, 16);

        // SAFETY: an instance this test owns, and the address is never read through.
        unsafe { pair(of(holder), word) };
        let first = stamp_at(word);
        // SAFETY: as above.
        unsafe { pair(of(holder), word) };
        assert_ne!(stamp_at(word), first, "the clock moved");
        assert!(!refused(|| tear(of(holder), word)));

        // SAFETY: the address `alloc` handed back.
        unsafe { dealloc(holder) };
    }

    #[test]
    fn a_half_the_plane_never_watched_is_not_a_disagreement() {
        let _turn = turn();
        // The slot is stamped and the word is not, which is what a program that stored a pointer
        // before the plane was watching looks like. Reporting on that would be reporting on a
        // correct program.
        let holder = alloc(64);
        let word = at(holder, 16);
        let slot = aux_slot::address_of(of(holder), word as u64).expect("the word has a slot");
        written_by(slot as *const c_void, epoch::stamp(3, 4));
        assert_eq!(stamp_at(word), epoch::NONE, "nobody wrote the word");
        assert!(!refused(|| tear(of(holder), word)));

        // SAFETY: the address `alloc` handed back.
        unsafe { dealloc(holder) };
    }

    #[test]
    fn a_word_with_no_slot_is_stamped_and_asked_nothing() {
        let _turn = turn();
        // An odd offset has no slot, which is section 5.2.2's third case and not a fault. The word
        // is still a pointer word another thread can race on, so it is stamped for C2 and C3, and
        // C1 has nothing to compare it against.
        let holder = alloc(64);
        let word = at(holder, 4);
        assert!(aux_slot::address_of(of(holder), word as u64).is_none(), "not pointer aligned");

        // SAFETY: an instance this test owns, and the address is never read through.
        unsafe { pair(of(holder), word) };
        assert_ne!(stamp_at(word), epoch::NONE, "the word itself is watched");
        assert!(!refused(|| tear(of(holder), word)));

        // SAFETY: the address `alloc` handed back.
        unsafe { dealloc(holder) };
    }

    #[test]
    fn storage_the_monitor_does_not_watch_is_neither_stamped_nor_asked() {
        let _turn = turn();
        // A local, which no region covers. Both halves of C1 step aside there for the reason every
        // other judgement does: there is no plane to read and nothing to say.
        let mut local: u64 = 0;
        let word: *const c_void = (&raw mut local).cast();
        let outside =
            Cap::new(word as u64, 8, 1, Meta::new(Class::Automatic, perm::READ | perm::WRITE, 0));

        // SAFETY: the word is this test's own local and is never read through by either call.
        unsafe { pair(outside, word) };
        assert!(!refused(|| tear(outside, word)));
    }

    #[test]
    fn the_exported_pair_and_check_are_the_functions_beside_them() {
        let _turn = turn();
        // The ABI rather than the Rust, since generated code reaches these and not the pair above.
        let holder = alloc(64);
        let word = at(holder, 8);
        let dest = of(holder);
        let slot = aux_slot::address_of(dest, word as u64).expect("the word has a slot");

        // Only the pair that agrees, because a refusal through this boundary aborts rather than
        // unwinding, for the reason `crate::fail` gives, and a test cannot catch it. Which pairs
        // are refused is the tests above and what these two add is that the ABI reaches them.
        // SAFETY: a real capability in this test's own storage, and a descriptor this build wrote.
        unsafe { exports::__rucc_meta_epoch_pair(&raw const dest, word) };
        assert_eq!(stamp_at(slot as *const c_void), stamp_at(word));
        // SAFETY: as above.
        unsafe { exports::__rucc_check_torn(&raw const dest, word, &raw const ROW) };

        // SAFETY: the address `alloc` handed back.
        unsafe { dealloc(holder) };
    }

    #[test]
    fn a_pointer_stored_into_an_object_comes_back_with_its_capability() {
        let _turn = turn();
        let holder = alloc(64);
        let pointee = alloc(128);
        let word = at(holder, 16);

        // SAFETY: both are instances this test owns, and neither address is read through.
        let back = unsafe {
            assert!(store(of(holder), word, pointee, of(pointee)));
            load(of(holder), word, pointee)
        };
        assert_eq!(back.lo, pointee as u64);
        assert_eq!(back.ext, of(pointee).ext);
        assert_eq!(back.ver, of(pointee).ver);

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(holder);
            dealloc(pointee);
        }
    }

    #[test]
    fn a_word_nobody_stored_a_pointer_into_says_nothing_and_that_refuses() {
        let _turn = turn();
        // Section 5.2.2's class Y1: an integer read as a pointer arrives with no capability. It is
        // also tamnd/rucc#1081, since a word a foreign writer filled looks exactly like this.
        let holder = alloc(64);
        // SAFETY: an instance this test owns, and the address is never read through.
        let back = unsafe { load(of(holder), at(holder, 0), at(holder, 32)) };
        assert!(back.is_bottom());
        // SAFETY: the address `alloc` handed back.
        unsafe { dealloc(holder) };
    }

    #[test]
    fn a_pointer_out_of_storage_with_no_aux_is_recovered_rather_than_refused() {
        let _turn = turn();
        // A local has no aux, so the capability was never written down. Refusing would report a
        // correct program, and recovery is the answer the boundary already gives for a pointer
        // whose capability nobody wrote down.
        let pointee = alloc(128);
        let mut local: u64 = 0;
        let word: *const c_void = (&raw mut local).cast();
        let outside =
            Cap::new(word as u64, 8, 1, Meta::new(Class::Automatic, perm::READ | perm::WRITE, 0));

        // SAFETY: the word is this test's own local and is never read through by either call.
        let back = unsafe {
            assert!(!store(outside, word, pointee, of(pointee)));
            load(outside, word, pointee)
        };
        assert!(!back.is_bottom());
        assert_eq!(back.lo, pointee as u64);
        assert_eq!(back.ext, of(pointee).ext);

        // SAFETY: the address `alloc` handed back.
        unsafe { dealloc(pointee) };
    }

    #[test]
    fn an_object_too_long_for_its_slot_has_its_bounds_read_back_out_of_the_planes() {
        let _turn = turn();
        // Two megabytes and one granule, which is a byte over what twenty one bits of extent can
        // say, so the slot keeps the version and the meta and the bounds come from the planes.
        let holder = alloc(64);
        let long = alloc(aux_slot::EXACT as usize + 1);
        assert!(!long.is_null(), "the arena has room for one");
        let word = at(holder, 8);
        let inside = at(long, 4096);

        // SAFETY: both are instances this test owns, and neither address is read through.
        let back = unsafe {
            assert!(store(of(holder), word, inside, of(long)));
            load(of(holder), word, inside)
        };
        assert_eq!(back.lo, long as u64);
        assert_eq!(back.ext, of(long).ext);
        assert_eq!(back.ver, of(long).ver);
        assert_eq!(back.meta.class(), Class::Allocated as u8);

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(holder);
            dealloc(long);
        }
    }

    #[test]
    fn a_long_object_that_has_been_given_back_does_not_answer_with_its_successor() {
        let _turn = turn();
        // The one thing the header case has to rule out. The bounds are taken from whatever owns
        // the address now, so a slot written about an instance that is gone must not be believed
        // about the instance that took its place.
        let holder = alloc(64);
        let long = alloc(aux_slot::EXACT as usize + 1);
        let word = at(holder, 8);
        let inside = at(long, 4096);
        // SAFETY: instances this test owns, neither address read through.
        unsafe { assert!(store(of(holder), word, inside, of(long))) };

        // SAFETY: the address `alloc` handed back.
        unsafe { dealloc(long) };
        // SAFETY: the word is still this test's own instance and is never read through.
        let back = unsafe { load(of(holder), word, inside) };
        assert!(back.is_bottom());

        // SAFETY: as above.
        unsafe { dealloc(holder) };
    }

    #[test]
    fn the_exported_names_are_the_functions_beside_them() {
        let _turn = turn();
        // The ABI rather than the Rust, since generated code reaches these and not the pair above.
        let holder = alloc(64);
        let pointee = alloc(128);
        let word = at(holder, 24);
        let (dest, cap) = (of(holder), of(pointee));
        let mut out = Cap::BOTTOM;

        // SAFETY: real capabilities in this test's own storage, and a writable slot for the answer.
        unsafe {
            exports::__rucc_cap_store(&raw const dest, word, pointee, &raw const cap);
            exports::__rucc_cap_load(&raw mut out, &raw const dest, word, pointee);
        }
        assert_eq!(out, cap);

        // SAFETY: the addresses `alloc` handed back.
        unsafe {
            dealloc(holder);
            dealloc(pointee);
        }
    }
}
