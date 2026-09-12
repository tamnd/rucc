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

use core::ffi::c_void;

use crate::alloc;
use crate::aux_slot::{self, Read};
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

/// The names generated code is compiled against.
///
/// Separate from the functions above for the reason every other module's exports are: these are an
/// ABI and those are Rust.
pub mod exports {
    use core::ffi::c_void;

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
