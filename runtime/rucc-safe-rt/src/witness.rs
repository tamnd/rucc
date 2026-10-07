//! Whether the frame a local was declared in has returned, which is row T4 of document 03.
//!
//! Design: `spec/safe-memory/04-judgements.md` section 4.2, judgement J1 over automatic storage.
//!
//! The heap has a plane that says which instance owns a granule, and a stack has nothing like it.
//! A frame is made and given back by moving a register, so there is no call to hook and nothing
//! that could write the plane as cheaply as the frame itself is made. What this does instead is
//! give each frame that hands out a pointer to one of its locals a witness: one word of that frame,
//! written when the function is entered and cleared on every path that returns from it.
//!
//! The capability of the local then says which word that is and what it held when the capability
//! was made, and asking whether the local is still there is reading the word and comparing. A
//! frame that has returned cleared it, and a frame made later in the same place writes a different
//! serial or something that is not a witness at all, so a pointer that outlived its frame finds a
//! word that does not match and is refused.
//!
//! # Block scope
//!
//! A local declared in a block whose address is taken and that the compiler can see go out of
//! scope gets a witness of its own rather than the frame's. It is opened with the frame and shut
//! by [`shut`] where control leaves the block, which writes [`SHUT`] over the mark and keeps the
//! serial, so the same check refuses a pointer to it from then on. Reaching the declaration again
//! puts the mark back with [`reopen`], with the same serial, because a capability made once at the
//! top of the function for that local is still the one every later pass through the block uses.
//!
//! # What it gets wrong, and which way
//!
//! Every way it can be wrong is quiet. A frame left by `longjmp` never clears its witness, so a
//! pointer into it is believed until something else is written over that word. The serial is
//! sixteen bits, so a frame at the same depth sixty five thousand frames later can look like the
//! one the pointer was made in. And a frame on a stack the scheduler switched away from, which is
//! what a coroutine is, is still live and still says so, which is right.
//!
//! The one thing that is not quiet is a pointer to a local of a thread that has exited and whose
//! stack was unmapped, where reading the witness faults. That pointer was already dangling and the
//! program was about to read through it anyway, so the fault is on the same access a few
//! instructions sooner.
//!
//! # The packing
//!
//! The witness word holds [`MARK`] in its high bits and the serial in its low sixteen. The version
//! of the capability holds the serial in its high sixteen bits, the address of the witness in the
//! low forty eight, and the low bit set. The low bit is what keeps it out of everything else that
//! reads a version: [`crate::plane::owned`] is false for an odd one, so a capability that stops
//! being [`crate::layout::Meta::NAMED`] by leaving its object is never compared against a plane.

use core::ffi::c_void;

use crate::layout::{Cap, Class};
use crate::tls::Slot;

/// The last serial this thread handed out, as an integer stored where a pointer goes.
static SERIAL: Slot = Slot::new(crate::tls::SERIAL);

/// What the high bits of a live witness hold, so that a word some other frame wrote is very
/// unlikely to pass for one.
pub const MARK: u64 = 0x7275_6363_7769_0000;

/// What the high bits of a witness hold while the block its local was declared in is not running,
/// which is not [`MARK`] and so is gone to every capability made while it was.
pub const SHUT: u64 = 0x7275_6363_7368_0000;

/// How many bits of a version are the witness's address.
const SHIFT: u32 = 48;

/// The bits of a version that are the witness's address.
const ADDRESS: u64 = (1 << SHIFT) - 1;

/// The bits of a witness that are the serial.
const SERIALS: u64 = 0xffff;

/// Writes a fresh witness into `witness`, which is the first thing a frame with one does.
///
/// # Safety
///
/// `witness` is a writable, aligned word of the calling frame.
pub unsafe fn open(witness: *mut u64) {
    let serial = (SERIAL.get() as usize).wrapping_add(1);
    // SAFETY: an integer stored where a pointer goes, which is only ever read back as one.
    unsafe { SERIAL.set(serial as *mut c_void) };
    // SAFETY: the caller's word.
    unsafe { witness.write_volatile(MARK | (serial as u64 & SERIALS)) };
}

/// Clears `witness`, which is the last thing a frame with one does on every path out.
///
/// # Safety
///
/// As [`open`].
pub unsafe fn close(witness: *mut u64) {
    // SAFETY: the caller's word. Volatile so that a store to a frame about to go away is not one
    // anything decides nobody reads.
    unsafe { witness.write_volatile(0) };
}

/// Shuts `witness` when it is open, which is where control leaves the block of the local it
/// belongs to.
///
/// # Safety
///
/// As [`open`].
pub unsafe fn shut(witness: *mut u64) {
    // SAFETY: the caller's word.
    let held = unsafe { witness.read_volatile() };
    if held & !SERIALS == MARK {
        // SAFETY: as above.
        unsafe { witness.write_volatile(SHUT | (held & SERIALS)) };
    }
}

/// Opens `witness` again with the serial it had when it is shut, which is where control reaches
/// the declaration of the local it belongs to.
///
/// # Safety
///
/// As [`open`].
pub unsafe fn reopen(witness: *mut u64) {
    // SAFETY: the caller's word.
    let held = unsafe { witness.read_volatile() };
    if held & !SERIALS == SHUT {
        // SAFETY: as above.
        unsafe { witness.write_volatile(MARK | (held & SERIALS)) };
    }
}

/// The version a capability for a local of the frame `witness` belongs to carries.
///
/// [`crate::plane::FOREIGN`] when the word is not a witness, which is a local the compiler made a
/// capability for in a frame it gave no witness, and which is then never asked about.
///
/// # Safety
///
/// `witness` is null or a readable, aligned word.
#[must_use]
pub unsafe fn version(witness: *const u64) -> u64 {
    if witness.is_null() {
        return crate::plane::FOREIGN;
    }
    // SAFETY: the caller's word.
    let held = unsafe { witness.read_volatile() };
    if held & !SERIALS != MARK {
        return crate::plane::FOREIGN;
    }
    (held & SERIALS) << SHIFT | (witness as u64 & ADDRESS) | 1
}

/// Whether `cap` is the capability of a local whose frame has returned.
///
/// False for every other capability, which includes a local of a frame with no witness.
///
/// # Safety
///
/// When `cap` is a named local's, the witness its version names is readable. That holds for a
/// frame still on a live thread's stack whether it has returned or not, which is the point.
#[must_use]
pub unsafe fn gone(cap: &Cap) -> bool {
    if !cap.is_named()
        || cap.meta.class() != Class::Automatic as u8
        || cap.ver == crate::plane::FOREIGN
        || cap.ver & 1 == 0
    {
        return false;
    }
    let witness = (cap.ver & ADDRESS & !1) as *const u64;
    // SAFETY: the caller's contract.
    let held = unsafe { witness.read_volatile() };
    held != MARK | cap.ver >> SHIFT
}

/// The names generated code is compiled against.
pub mod exports {
    /// As [`super::open`].
    ///
    /// # Safety
    ///
    /// As [`super::open`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_frame_open(witness: *mut u64) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::open(witness) };
    }

    /// As [`super::close`].
    ///
    /// # Safety
    ///
    /// As [`super::close`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_frame_close(witness: *mut u64) {
        // SAFETY: as above.
        unsafe { super::close(witness) };
    }

    /// As [`super::shut`].
    ///
    /// # Safety
    ///
    /// As [`super::shut`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_scope_shut(witness: *mut u64) {
        // SAFETY: as above.
        unsafe { super::shut(witness) };
    }

    /// As [`super::reopen`].
    ///
    /// # Safety
    ///
    /// As [`super::reopen`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_scope_open(witness: *mut u64) {
        // SAFETY: as above.
        unsafe { super::reopen(witness) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recover;

    #[test]
    fn a_local_is_live_until_its_frame_closes() {
        let mut witness = 0_u64;
        let local = [0_u32; 4];
        // SAFETY: both are locals of this test that outlive every use below.
        unsafe {
            open(&raw mut witness);
            let cap = recover::local(local.as_ptr().cast(), 16, &raw const witness);
            assert_eq!(cap.ver & 1, 1);
            assert!(!crate::plane::owned(cap.ver));
            assert!(!gone(&cap));
            close(&raw mut witness);
            assert!(gone(&cap));
        }
    }

    #[test]
    fn a_frame_made_again_in_the_same_place_is_not_the_one_the_pointer_was_made_in() {
        let mut witness = 0_u64;
        let local = 0_u32;
        // SAFETY: as above.
        unsafe {
            open(&raw mut witness);
            let cap = recover::local((&raw const local).cast(), 4, &raw const witness);
            close(&raw mut witness);
            open(&raw mut witness);
            assert!(gone(&cap), "the serial moved on");
            let again = recover::local((&raw const local).cast(), 4, &raw const witness);
            assert!(!gone(&again));
            close(&raw mut witness);
        }
    }

    #[test]
    fn a_local_is_gone_while_its_block_is_shut_and_back_when_it_opens_again() {
        let mut witness = 0_u64;
        let local = 0_u32;
        // SAFETY: as above.
        unsafe {
            open(&raw mut witness);
            let cap = recover::local((&raw const local).cast(), 4, &raw const witness);
            reopen(&raw mut witness);
            assert!(!gone(&cap), "opening an open witness leaves it alone");
            shut(&raw mut witness);
            assert!(gone(&cap));
            shut(&raw mut witness);
            assert!(gone(&cap));
            let late = recover::local((&raw const local).cast(), 4, &raw const witness);
            assert_eq!(late.ver, crate::plane::FOREIGN, "made while shut, so never asked");
            reopen(&raw mut witness);
            assert!(!gone(&cap), "the same serial");
            close(&raw mut witness);
            reopen(&raw mut witness);
            assert!(gone(&cap), "a closed frame is not opened by a block");
            assert_eq!(witness, 0);
        }
    }

    #[test]
    fn a_local_with_no_witness_and_a_variable_are_never_gone() {
        let local = 0_u32;
        let stray = 0_u64;
        // SAFETY: as above, and a word that is not a witness is only read.
        unsafe {
            let bare = recover::local((&raw const local).cast(), 4, core::ptr::null());
            assert_eq!(bare.ver, crate::plane::FOREIGN);
            assert!(!gone(&bare));
            let unmarked = recover::local((&raw const local).cast(), 4, &raw const stray);
            assert_eq!(unmarked.ver, crate::plane::FOREIGN);
            let global = recover::object((&raw const local).cast(), 4, Class::Static);
            assert!(!gone(&global));
        }
    }
}
