//! A call through a pointer, which is where rows Y4 and Y5 of document 03 happen.
//!
//! Design: `spec/safe-memory/04-judgements.md` section 4.4, judgement J1 over a function.
//!
//! A pointer that is called is an access like any other, and what it is checked against is the
//! function it names rather than an extent. Two things can be wrong with it. It can name storage
//! that holds data, which is Y5, and it can name a function whose parameters are not the ones the
//! call passes, which is Y4. Both are judgement J1, because both are a use of a pointer the object
//! it names does not permit.
//!
//! # Data called as a function
//!
//! [`through`] is in front of every call through a pointer in instrumented code, and the first thing
//! it asks is whether the address is inside the heap. Nothing in the heap is code: the allocator
//! hands out storage for data and nothing else, and the page being executable or not is a fact about
//! the machine rather than about the program, which is why the rule is about where the pointer came
//! from. Storage outside the heap is left alone, because a mapping a program made executable on
//! purpose is a thing a JIT does and this has no way to tell that from a mistake.
//!
//! # A function called with the wrong number of arguments
//!
//! The caller knows how many arguments it passes and the callee knows how many it takes, and the two
//! are never in the same place. So the caller writes down what it is about to do, the target and the
//! count, in a word of thread local storage, and the callee reads it back as the first thing it does
//! and compares. [`entered`] is that half, and it is only in a function that can be reached through
//! a pointer, which is one the unit takes the address of or one with a name another unit can use.
//!
//! The target is what makes the record safe to leave lying around. A call through a pointer may
//! reach a function nobody instrumented, which never reads the word, and that function may then call
//! back into one that does. The record still names the first function, so the callback finds an
//! address that is not its own and takes nothing from it. A record is believed only by the function
//! it names, and it is cleared by whichever instrumented function reads it first, so a stale one
//! lasts no further than the next instrumented entry.
//!
//! The count is the C level one, before the ABI splits a structure into registers or turns a return
//! into a pointer, and the compiler counts both ends the same way. A variadic signature at either end
//! is not compared, because the count is the point of one.
//!
//! # The packing
//!
//! One word holds both halves: the low 48 bits of the target and the count above them, plus one so
//! that a record of nothing is a zero word. Every address a function can have in a user space
//! program on the targets this crate builds for fits in 48 bits, and comparing only those bits at
//! both ends means a tag in the top byte, which AArch64 ignores, does not make a function a stranger
//! to itself.

use core::ffi::c_void;

use crate::fail::Descriptor;
use crate::tls::Slot;

/// What the next instrumented function this thread enters should find, as described above.
static CALLED: Slot = Slot::new(crate::tls::CALLED);

/// How many bits of a record are the target's.
const SHIFT: u32 = 48;

/// The bits of a record that are the target's.
const ADDRESS: usize = (1 << SHIFT) - 1;

/// The count a call through a variadic signature says it passes, which no callee compares against.
///
/// One below the largest sixteen bit number rather than it, because the count is stored plus one
/// and the top of the range is the last value that still fits above the address.
pub const ANY: usize = 0xfffe;

/// The word a call to `target` passing `passed` arguments leaves for the callee.
const fn packed(target: usize, passed: usize) -> usize {
    let passed = if passed > ANY { ANY } else { passed };
    (target & ADDRESS) | (passed + 1) << SHIFT
}

/// Judges a call to `target` that passes `passed` arguments, and leaves the count for the callee.
///
/// Refused here when the target is inside the heap. Otherwise the record is written, and whether
/// the count was right is the callee's question, which [`entered`] asks.
///
/// # Safety
///
/// `descriptor` is null or one the same build wrote, as [`crate::fail::report`] takes. `target` is
/// only ever compared, never read through or called.
pub unsafe fn through(target: *const c_void, passed: usize, descriptor: *const Descriptor) {
    let target = target as usize;
    if crate::alloc::covering(target).is_some() {
        // SAFETY: this function's contract about the descriptor, passed on.
        unsafe { crate::fail::report(descriptor, Some(target)) };
    }
    // SAFETY: the word is a number rather than a pointer to anything, so nothing has to outlive it.
    unsafe { CALLED.set(packed(target, passed) as *mut c_void) };
}

/// Judges the call that reached `own`, a function that takes `params` arguments.
///
/// The record is cleared whatever it said, so that it is believed once. It is about this function
/// only when the target it names is this function's address, and then a count other than `params`
/// is refused.
///
/// # Safety
///
/// As [`through`]. `own` is the address of the function this is called from, and is never read.
pub unsafe fn entered(own: *const c_void, params: usize, descriptor: *const Descriptor) {
    let record = CALLED.get() as usize;
    if record == 0 {
        return;
    }
    // SAFETY: null is always a valid thing to store.
    unsafe { CALLED.set(core::ptr::null_mut()) };
    if record & ADDRESS != own as usize & ADDRESS {
        return;
    }
    let passed = (record >> SHIFT) - 1;
    if passed != ANY && passed != params {
        // SAFETY: as in `through`. The address is the function's, which is what was called.
        unsafe { crate::fail::report(descriptor, Some(own as usize)) };
    }
}

/// The names generated code is compiled against.
pub mod exports {
    use core::ffi::c_void;

    use crate::fail::Descriptor;

    /// As [`super::through`].
    ///
    /// # Safety
    ///
    /// As [`super::through`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_call_through(
        target: *const c_void,
        passed: usize,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: this wrapper's contract is the one it calls, passed straight on.
        unsafe { super::through(target, passed, descriptor) };
    }

    /// As [`super::entered`].
    ///
    /// # Safety
    ///
    /// As [`super::entered`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_call_entered(
        own: *const c_void,
        params: usize,
        descriptor: *const Descriptor,
    ) {
        // SAFETY: as above.
        unsafe { super::entered(own, params, descriptor) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::{alloc, dealloc};
    use crate::turnstile::turn;

    /// The descriptor every refusal here is reported against.
    static ROW: Descriptor = Descriptor { judgement: 1, class: 0, size: 0, pc: 0 };

    /// Runs one judgement and says whether it refused, without the panic reaching the harness.
    fn refused(check: impl FnOnce()) -> bool {
        let hook = std::panic::take_hook();
        std::panic::set_hook(std::boxed::Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
        std::panic::set_hook(hook);
        out.is_err()
    }

    /// Two places that stand in for two functions, since only their addresses are ever used.
    static ONE: u8 = 0;
    static TWO: u8 = 0;

    fn one() -> *const c_void {
        (&raw const ONE).cast()
    }

    fn two() -> *const c_void {
        (&raw const TWO).cast()
    }

    #[test]
    fn a_record_is_the_target_and_the_count_and_never_zero() {
        assert_eq!(packed(0x1234, 0), 0x1234 | 1 << SHIFT);
        assert_eq!(packed(0x1234, 3) >> SHIFT, 4);
        assert_ne!(packed(0, 0), 0);
        // A tag in the top byte is not part of the address that is compared.
        assert_eq!(packed(0x0f00_0000_0000_1234, 1) & ADDRESS, 0x1234);
        assert_eq!(packed(0x1234, 1 << 20) >> SHIFT, ANY + 1);
    }

    #[test]
    fn a_callee_takes_the_record_that_names_it_and_clears_it() {
        // SAFETY: a null descriptor is allowed, and nothing is refused on this path.
        unsafe {
            through(one(), 2, core::ptr::null());
            entered(one(), 2, core::ptr::null());
        }
        assert!(CALLED.get().is_null());
    }

    #[test]
    fn a_record_about_another_function_is_cleared_and_not_believed() {
        // The callback shape: the record names the function the pointer reached, which never read
        // it, and the function that reads it is a different one with a different count.
        // SAFETY: as above. A count that differs would refuse, and it is not compared here.
        unsafe {
            through(one(), 5, core::ptr::null());
            entered(two(), 1, core::ptr::null());
        }
        assert!(CALLED.get().is_null());
    }

    #[test]
    fn a_count_other_than_the_one_the_callee_takes_is_refused() {
        let _turn = turn();
        // Row Y4: a function of one parameter called through a pointer to one of two.
        // SAFETY: the descriptor is a real one, and the addresses are only compared.
        assert!(refused(|| unsafe {
            through(one(), 2, &raw const ROW);
            entered(one(), 1, &raw const ROW);
        }));
        assert!(CALLED.get().is_null());
        // SAFETY: as above.
        assert!(refused(|| unsafe {
            through(one(), 0, &raw const ROW);
            entered(one(), 1, &raw const ROW);
        }));
    }

    #[test]
    fn a_variadic_call_is_not_counted() {
        let _turn = turn();
        // SAFETY: as above.
        assert!(!refused(|| unsafe {
            through(one(), ANY, &raw const ROW);
            entered(one(), 3, &raw const ROW);
        }));
    }

    #[test]
    fn a_heap_address_called_is_refused_and_a_function_is_not() {
        let _turn = turn();
        // Row Y5. The block is never called, only judged.
        let data = alloc(64);
        // SAFETY: as above.
        assert!(refused(|| unsafe { through(data, 0, &raw const ROW) }));
        // SAFETY: as above.
        assert!(!refused(|| unsafe { through(one(), 0, &raw const ROW) }));
        // SAFETY: the block came from `alloc` above and is freed once.
        unsafe { dealloc(data) };
        // SAFETY: null is always a valid thing to store.
        unsafe { CALLED.set(core::ptr::null_mut()) };
    }

    #[test]
    fn a_function_entered_with_nothing_recorded_does_nothing() {
        // SAFETY: null is always a valid thing to store, and a null descriptor is allowed.
        unsafe {
            CALLED.set(core::ptr::null_mut());
            entered(one(), 3, core::ptr::null());
        }
        assert!(CALLED.get().is_null());
    }
}
