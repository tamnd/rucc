//! One pointer per thread, for the two side channels that need one.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.3, which is where the first of them is
//! specified and which costs it at one thread local access.
//!
//! That is what generated code will eventually do, and it is not what this does. The attribute that
//! gives a `#![no_std]` Rust crate a thread local of its own is not stable, so what is here is a
//! `pthread` key and a call to reach it. The shape of the thing stored is the part that has to be
//! right now, because it is an ABI and the two halves have to agree about it; how the runtime finds
//! it is this crate's business and can get faster without anybody else noticing.
//!
//! There are two of these, [`crate::frame`]'s and [`crate::restrict`]'s, and they are separate keys
//! rather than two fields of one structure because they have different lifetimes. A frame is
//! published for one call and consumed by it, and a restrict scope lives for as long as the block
//! that declared the pointers. Putting them together would mean a call publishing a frame had to
//! carry the enclosing scope along with it.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// What the platform calls a `pthread_key_t`.
///
/// The one place this module is not the same everywhere. Apple's is an `unsigned long` and
/// everybody else's is an `unsigned int`, and getting it wrong means `pthread_key_create` writing
/// eight bytes into four.
#[cfg(target_vendor = "apple")]
type Key = core::ffi::c_ulong;
/// See the Apple arm above.
#[cfg(not(target_vendor = "apple"))]
type Key = core::ffi::c_uint;

/// A pointer each thread has its own copy of, made on first use.
///
/// Declared as a `static` by whoever wants one. The key behind it is created once for the process,
/// by whichever thread asks first, and never deleted.
#[derive(Debug)]
pub struct Slot {
    /// Where [`Slot::key`] is in making the key, as one of the four constants in that function.
    state: AtomicU32,
    /// The key itself, meaningful once `state` says it was made.
    key: AtomicUsize,
}

impl Slot {
    /// A slot whose key has not been made yet, which is what a `static` starts as.
    #[must_use]
    pub const fn new() -> Self {
        Self { state: AtomicU32::new(0), key: AtomicUsize::new(0) }
    }

    /// What this thread last stored here, or null.
    ///
    /// Null for a thread that has stored nothing as well as for one that stored null, and every
    /// caller treats the two the same way: there is nothing here, so fall back to whatever the
    /// weaker answer is.
    #[must_use]
    pub fn get(&self) -> *mut c_void {
        let Some(key) = self.key() else { return core::ptr::null_mut() };
        // SAFETY: the key was made by `pthread_key_create` and is never deleted, and the only
        // values ever stored under it are the ones `set` was handed.
        unsafe { pthread_getspecific(key) }
    }

    /// Stores `value` for this thread only.
    ///
    /// # Safety
    ///
    /// Whatever `value` points at stays valid for as long as something might read it back. In
    /// practice it is a local of the storing function and the read happens before that function
    /// returns, which is what makes the storage free.
    pub unsafe fn set(&self, value: *mut c_void) {
        let Some(key) = self.key() else { return };
        // SAFETY: as in `get`, and the caller says the pointer outlives the reads of it.
        unsafe { pthread_setspecific(key, value) };
    }

    /// The key this slot lives under, made once for the program.
    ///
    /// `None` when the key could not be made, which is a process that has run out of them.
    /// Everything above degrades to an empty slot, so the program keeps running with whatever the
    /// weaker answer is, which is the wrong amount of information rather than the wrong
    /// information.
    fn key(&self) -> Option<Key> {
        /// Not made yet.
        const COLD: u32 = 0;
        /// Being made by another thread right now.
        const MAKING: u32 = 1;
        /// Made, and `key` holds it.
        const MADE: u32 = 2;
        /// Could not be made, and asking again would not help.
        const FAILED: u32 = 3;

        loop {
            match self.state.load(Ordering::Acquire) {
                MADE => return Some(self.key.load(Ordering::Relaxed) as Key),
                FAILED => return None,
                MAKING => core::hint::spin_loop(),
                _ => {
                    if self
                        .state
                        .compare_exchange(COLD, MAKING, Ordering::Acquire, Ordering::Relaxed)
                        .is_err()
                    {
                        continue;
                    }
                    let mut made: Key = 0;
                    // SAFETY: the pointer is to a local this call fills in, and no destructor is
                    // wanted: what is stored points into a stack that is going away with the
                    // thread.
                    let failed =
                        unsafe { pthread_key_create(&raw mut made, core::ptr::null_mut()) };
                    if failed == 0 {
                        self.key.store(made as usize, Ordering::Relaxed);
                        self.state.store(MADE, Ordering::Release);
                    } else {
                        self.state.store(FAILED, Ordering::Release);
                    }
                }
            }
        }
    }
}

impl Default for Slot {
    fn default() -> Self {
        Self::new()
    }
}

unsafe extern "C" {
    fn pthread_key_create(key: *mut Key, dtor: *mut c_void) -> i32;
    fn pthread_getspecific(key: Key) -> *mut c_void;
    fn pthread_setspecific(key: Key, value: *const c_void) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slot nothing else in the crate uses, so what it holds is this test's business alone.
    static MINE: Slot = Slot::new();

    #[test]
    fn what_a_thread_stores_is_what_it_reads_back() {
        let mut value = 7_u32;
        // SAFETY: the local outlives the read below.
        unsafe { MINE.set((&raw mut value).cast()) };
        assert_eq!(MINE.get(), (&raw mut value).cast());
        // SAFETY: null is always a valid thing to store.
        unsafe { MINE.set(core::ptr::null_mut()) };
        assert!(MINE.get().is_null());
    }

    #[test]
    fn one_threads_slot_is_not_anothers() {
        // The whole reason the side channels are thread local. Two threads calling at the same
        // time would otherwise read each other's bookkeeping, which is a monitor reporting on the
        // wrong memory rather than a monitor being slow.
        let mut mine = 7_u32;
        // SAFETY: the local outlives the thread joined below.
        unsafe { MINE.set((&raw mut mine).cast()) };

        let theirs = std::thread::spawn(|| {
            let empty = MINE.get().is_null();
            let mut theirs = 9_u32;
            // SAFETY: the local outlives the read on the line after it.
            unsafe { MINE.set((&raw mut theirs).cast()) };
            (empty, MINE.get().is_null())
        })
        .join()
        .expect("the thread ran");

        assert!(theirs.0, "nothing of ours was visible over there");
        assert!(!theirs.1, "and what they stored was visible to them");
        assert_eq!(MINE.get(), (&raw mut mine).cast(), "and ours is still ours");
        // SAFETY: null is always a valid thing to store.
        unsafe { MINE.set(core::ptr::null_mut()) };
    }
}
